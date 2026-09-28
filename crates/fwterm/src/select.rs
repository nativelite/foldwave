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
/// line `n`'s characters (a wide character once, its second half left out),
/// or `None` if the line is no longer held. Each line loses its trailing
/// spaces; lines are joined with CR LF, as the Windows clipboard expects.
pub fn text(
    start: Point,
    end: Point,
    cols: usize,
    mut line: impl FnMut(u64, usize, usize) -> Option<String>,
) -> String {
    let mut out = String::new();
    let mut first = true;
    for n in start.line..=end.line {
        let from = if n == start.line { start.col } else { 0 };
        let to = if n == end.line { end.col + 1 } else { cols };
        let Some(text) = line(n, from, to.min(cols)) else {
            continue;
        };
        if !first {
            out.push_str("\r\n");
        }
        first = false;
        out.push_str(text.trim_end_matches(' '));
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

/// Text pasted into a terminal: line breaks become carriage returns, which
/// is what the Enter key sends.
pub fn paste_bytes(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
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
        let get = |n: u64, from: usize, to: usize| -> Option<String> {
            if n == 2 {
                return None;
            }
            let l: Vec<char> = lines[n as usize].chars().collect();
            Some(l[from.min(l.len())..to.min(l.len())].iter().collect())
        };
        assert_eq!(text(pt(0, 6), pt(0, 10), 14, get), "world");
        assert_eq!(
            text(pt(0, 6), pt(3, 3), 14, get),
            "world\r\nsecond line\r\nlast"
        );
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
    fn paste_turns_line_breaks_into_enter() {
        assert_eq!(paste_bytes("a\r\nb\nc"), b"a\rb\rc");
        assert_eq!(paste_bytes("é"), "é".as_bytes());
    }
}
