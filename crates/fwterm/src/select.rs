//! Selecting text in a pane, and turning a selection into text.
//!
//! A selection is pinned to lines of output, not to rows of the window, so
//! it stays on the same text while output scrolls in or the view scrolls
//! back. Lines are counted from the start of the pane's output: the screen's
//! top row is line `scrolled` (the number of lines that have scrolled off the
//! top so far), the newest history line is `scrolled - 1`, and a view scrolled
//! back `back` lines has line `scrolled - back` in its top row.

/// A cell by output line and column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: u64,
    pub col: usize,
}

/// A selection: where it started and where it extends to (either order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub head: Point,
    /// Made in the view rebuilt from a pane's log: which rebuilt screen (the
    /// log line it ends before), whose rows its `line`s then are. `None` in
    /// the live view, where lines are output lines.
    pub log: Option<u64>,
}

impl Selection {
    /// First and last selected cells, in reading order.
    pub fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The selected cells as view rows of a `rows x cols` view whose top row
    /// is output line `top`: `(start, end)` as `(row, col)`, clipped to the
    /// view, or `None` if none are in view.
    pub fn in_view(
        &self,
        top: u64,
        rows: usize,
        cols: usize,
    ) -> Option<((usize, usize), (usize, usize))> {
        let (a, b) = self.ordered();
        let bottom = top + rows as u64; // one past the last row
        if rows == 0 || cols == 0 || b.line < top || a.line >= bottom {
            return None;
        }
        let start = if a.line < top {
            (0, 0)
        } else {
            ((a.line - top) as usize, a.col.min(cols - 1))
        };
        let end = if b.line >= bottom {
            (rows - 1, cols - 1)
        } else {
            ((b.line - top) as usize, b.col.min(cols - 1))
        };
        Some((start, end))
    }
}

/// The text from `start` to `end` inclusive, `cols` wide. `line(n)` gives
/// line `n`'s characters (a wide character once, its second half left out)
/// and whether the line wrapped onto the next (the emulator's autowrap), or
/// `None` if the line is no longer held. A line that ends in a line break
/// loses its trailing spaces and is joined to the next with CR LF, as the
/// Windows clipboard expects; a wrapped one runs straight on, spaces and all,
/// so a long line copies as the one line it was.
pub fn text(
    start: Point,
    end: Point,
    cols: usize,
    mut line: impl FnMut(u64, usize, usize) -> Option<(String, bool)>,
) -> String {
    let mut out = String::new();
    // Whether the last line taken ran on into this one.
    let mut run_on: Option<bool> = None;
    for n in start.line..=end.line {
        let from = if n == start.line { start.col } else { 0 };
        let to = if n == end.line { end.col + 1 } else { cols };
        let Some((text, wrapped)) = line(n, from, to.min(cols)) else {
            run_on = None;
            continue;
        };
        // A break after a line that ended in one, or where a lost line came
        // between (the two did not meet); the line before loses its spaces.
        let brk = match run_on {
            Some(ran_on) => !ran_on,
            None => !out.is_empty(),
        };
        if brk {
            let kept = out.trim_end_matches(' ').len();
            out.truncate(kept);
            out.push_str("\r\n");
        }
        out.push_str(&text);
        run_on = Some(wrapped && n != end.line);
    }
    if run_on != Some(true) {
        let kept = out.trim_end_matches(' ').len();
        out.truncate(kept);
    }
    out
}

/// Characters that end a word for a double-click (Windows Terminal's
/// default word delimiters).
const DELIMITERS: &str = " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}?\u{2502}";

fn class(c: char) -> u8 {
    if c == ' ' {
        0
    } else if DELIMITERS.contains(c) {
        1
    } else {
        2
    }
}

/// While a selection is dragged past a pane's top or bottom, how many lines
/// to scroll each tick, given the pointer's row relative to the pane
/// (negative above it) and the pane's `rows`: back into history (negative)
/// above, toward live (positive) below, faster the farther out, at most half
/// the pane; 0 inside.
pub fn edge_scroll(row: i32, rows: usize) -> i32 {
    let cap = (rows as i32 / 2).max(1);
    if row < 0 {
        -(-row).min(cap)
    } else if row >= rows as i32 {
        (row - rows as i32 + 1).min(cap)
    } else {
        0
    }
}

/// The columns of the word under column `col` of a line, `(first, last)`:
/// the run of characters of the same kind (word characters, delimiters or
/// spaces) around it. `chars` has one entry per column.
pub fn word_at(chars: &[char], col: usize) -> (usize, usize) {
    if chars.is_empty() {
        return (0, 0);
    }
    let col = col.min(chars.len() - 1);
    let kind = class(chars[col]);
    let mut first = col;
    while first > 0 && class(chars[first - 1]) == kind {
        first -= 1;
    }
    let mut last = col;
    while last + 1 < chars.len() && class(chars[last + 1]) == kind {
        last += 1;
    }
    (first, last)
}

/// Whether pasting `text` should be confirmed first, and if so how many
/// lines it holds: it has a line break, and the program neither asked for
/// bracketed paste nor is full-screen, so a shell would run each line as it
/// arrives (design §11 row 14). PSReadLine never asks for bracketed paste.
pub fn paste_asks(text: &str, bracketed: bool, full_screen: bool) -> Option<usize> {
    let breaks = text.contains(['\r', '\n']);
    (breaks && !bracketed && !full_screen).then(|| text.lines().count().max(1))
}

/// Text pasted into a terminal: line breaks become carriage returns, which
/// is what the Enter key sends. When the program asked for bracketed paste
/// (`?2004h`), it comes wrapped in `ESC [200~` and `ESC [201~`, and any of
/// those markers in the text itself are dropped, so pasted text cannot end
/// the bracket early and pass the rest off as typed.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if !bracketed {
        return text.into_bytes();
    }
    let inner = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
    format!("\x1b[200~{inner}\x1b[201~").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(line: u64, col: usize) -> Point {
        Point { line, col }
    }

    #[test]
    fn a_drag_past_the_edge_scrolls_faster_the_farther_out() {
        assert_eq!(edge_scroll(0, 20), 0);
        assert_eq!(edge_scroll(19, 20), 0);
        assert_eq!(edge_scroll(-1, 20), -1, "just above: back one line");
        assert_eq!(edge_scroll(-4, 20), -4);
        assert_eq!(edge_scroll(-50, 20), -10, "at most half the pane");
        assert_eq!(edge_scroll(20, 20), 1, "just below: toward live");
        assert_eq!(edge_scroll(22, 20), 3);
        assert_eq!(edge_scroll(0, 1), 0);
        assert_eq!(edge_scroll(5, 1), 1, "a one-row pane still scrolls");
        assert_eq!(edge_scroll(-3, 1), -1);
    }

    #[test]
    fn a_selection_orders_its_ends_and_clips_to_the_view() {
        let s = Selection {
            anchor: pt(12, 5),
            head: pt(10, 2),
            log: None,
        };
        assert_eq!(s.ordered(), (pt(10, 2), pt(12, 5)));
        // View of 4 rows x 8 cols starting at line 10.
        assert_eq!(s.in_view(10, 4, 8), Some(((0, 2), (2, 5))));
        // Scrolled so line 11 is on top: the start is above the view.
        assert_eq!(s.in_view(11, 4, 8), Some(((0, 0), (1, 5))));
        // Starting at line 9, line 12 is the view's last row.
        assert_eq!(s.in_view(9, 4, 8), Some(((1, 2), (3, 5))));
        assert_eq!(s.in_view(4, 4, 8), None, "all below the view");
        assert_eq!(s.in_view(13, 4, 8), None, "all above the view");
        // Runs past the bottom: ends at the last cell.
        assert_eq!(s.in_view(8, 3, 8), Some(((2, 2), (2, 7))));
        // A column past a narrower view is clipped.
        assert_eq!(s.in_view(10, 4, 4), Some(((0, 2), (2, 3))));
    }

    #[test]
    fn text_joins_lines_trims_trailing_spaces_and_skips_lost_lines() {
        let lines = ["hello world   ", "second line   ", "gone", "last one  "];
        let get = |n: u64, from: usize, to: usize| -> Option<(String, bool)> {
            if n == 2 {
                return None;
            }
            let l: Vec<char> = lines[n as usize].chars().collect();
            Some((
                l[from.min(l.len())..to.min(l.len())].iter().collect(),
                false,
            ))
        };
        assert_eq!(text(pt(0, 6), pt(0, 10), 14, get), "world");
        assert_eq!(
            text(pt(0, 6), pt(3, 3), 14, get),
            "world\r\nsecond line\r\nlast"
        );
    }

    #[test]
    fn a_wrapped_line_copies_as_one_line_with_its_spaces() {
        // "echo one two three" wrapped on a 10-column screen, then "done".
        let rows = [
            ("echo one t", true),
            ("wo three  ", false),
            ("done      ", false),
        ];
        let get = |n: u64, from: usize, to: usize| -> Option<(String, bool)> {
            let (l, w) = rows[n as usize];
            let l: Vec<char> = l.chars().collect();
            Some((l[from.min(l.len())..to.min(l.len())].iter().collect(), w))
        };
        assert_eq!(
            text(pt(0, 0), pt(2, 9), 10, get),
            "echo one two three\r\ndone"
        );
        // A wrap at a space keeps it: "ab " + "cd".
        let spaced = [("ab ", true), ("cd", false)];
        let get = |n: u64, from: usize, to: usize| -> Option<(String, bool)> {
            let (l, w) = spaced[n as usize];
            let l: Vec<char> = l.chars().collect();
            Some((l[from.min(l.len())..to.min(l.len())].iter().collect(), w))
        };
        assert_eq!(text(pt(0, 0), pt(1, 2), 3, get), "ab cd");
        // A selection ending on a wrapped row still loses its trailing spaces.
        assert_eq!(text(pt(0, 0), pt(0, 2), 3, get), "ab");
    }

    #[test]
    fn a_double_click_takes_the_run_of_the_same_kind() {
        let line: Vec<char> = "cd C:\\src\\fold-wave  now".chars().collect();
        assert_eq!(word_at(&line, 5), (4, 5), "a run of delimiters");
        assert_eq!(word_at(&line, 7), (6, 8), "src");
        assert_eq!(word_at(&line, 12), (10, 13), "fold stops at '-'");
        assert_eq!(word_at(&line, 20), (19, 20), "a run of spaces");
        assert_eq!(word_at(&line, 99), (21, 23), "clamped to the end");
        assert_eq!(word_at(&[], 3), (0, 0));
    }

    #[test]
    fn a_paste_that_would_run_lines_asks_first() {
        // Two lines into a shell that did not ask for bracketed paste: ask.
        assert_eq!(paste_asks("echo a\r\necho b", false, false), Some(2));
        assert_eq!(
            paste_asks("echo a\n", false, false),
            Some(1),
            "a trailing Enter runs it"
        );
        assert_eq!(paste_asks("one line", false, false), None);
        // Bracketed, the program takes the text as pasted: no question.
        assert_eq!(paste_asks("a\nb", true, false), None);
        // A full-screen program (an editor) takes lines as text.
        assert_eq!(paste_asks("a\nb", false, true), None);
    }

    #[test]
    fn paste_turns_line_breaks_into_enter() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("é", false), "é".as_bytes());
    }

    #[test]
    fn a_bracketed_paste_is_wrapped_and_cannot_close_itself() {
        assert_eq!(paste_bytes("ls\nrm x", true), b"\x1b[200~ls\rrm x\x1b[201~");
        // The end marker inside the text is dropped, not passed through.
        assert_eq!(
            paste_bytes("a\x1b[201~evil\n", true),
            b"\x1b[200~aevil\r\x1b[201~"
        );
    }
}
