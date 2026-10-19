//! The help sheet: F1, or `? HELP` at the strip's right end, opens it over
//! the panes, a read-only table of fwterm's keys. `KEYS` is the one list
//! the sheet and `fwterm --help` are both printed from, so they cannot
//! drift. Esc or Enter closes it; Up/Down, PageUp/PageDown and the wheel
//! scroll it.

use crate::settings_ui::{self, Cell, Key, Paint};
use std::cell::Cell as Slot;

/// Every key fwterm takes, and what it does, in the order shown.
pub const KEYS: &[(&str, &str)] = &[
    (
        "Alt+Shift+Plus",
        "split the focused pane, the new one to the right",
    ),
    ("Alt+Shift+Minus", "split it, the new one below"),
    ("Alt+Shift+D", "split it along its longer side"),
    (
        "Alt+Arrow, click",
        "focus the pane in that direction / under the pointer",
    ),
    (
        "Ctrl+Comma",
        "settings: theme, font, size, the paste question, the instrument strip, \
         logs, shell (saved to fwterm.conf)",
    ),
    ("F1", "this help"),
    (
        "Ctrl+Shift+W",
        "close the focused pane (the last one closes the window)",
    ),
    (
        "Ctrl+Shift+Z",
        "zoom the focused pane to the whole window, or back",
    ),
    (
        "Ctrl+Shift+J",
        "show the pane of an agent waiting on a decision (atrium's bus; the \
         instrument strip counts them), then the next",
    ),
    (
        "Ctrl+Up/Down",
        "jump to the previous / next command's prompt",
    ),
    (
        "Ctrl+Shift+O",
        "select a command's output (then Ctrl+C copies it)",
    ),
    (
        "Ctrl+Shift+[ / ]",
        "fold that command's output to one line / unfold",
    ),
    (
        "Ctrl+Shift+F",
        "search every pane's log, and earlier sessions' in the same log folder: \
         type, Enter searches, Enter / Shift+Enter step through hits (newest \
         first), Esc closes; /pattern/ is a regular expression (. [a-z] \\d \\w \
         \\s * + ? {m,n} | ( ) ^ $), case ignored like text",
    ),
    (
        "drag, double-click",
        "select text in a pane / select a word (a program that asked for the \
         mouse gets clicks, drags and the wheel instead; Shift keeps them \
         foldwave's)",
    ),
    (
        "Ctrl+Shift+C",
        "copy the selection (Ctrl+C too while there is one; otherwise Ctrl+C \
         goes to the program)",
    ),
    (
        "Ctrl+V, Ctrl+Shift+V",
        "paste; several lines into a program that did not ask for bracketed \
         paste (a shell runs each) ask first: Enter pastes, Esc cancels \
         (paste_warning = off in the settings file pastes at once)",
    ),
    (
        "Shift+PgUp/PgDn, wheel",
        "scroll the focused pane back / forward",
    ),
];

/// The line under the table: the strip's buttons do the same.
pub const NOTE: &str = "the strip's SETTINGS and HELP buttons open these too";

/// `text` broken into lines of at most `width` characters at spaces; a word
/// longer than that stands alone on its line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let fits = line.chars().count() + 1 + word.chars().count() <= width;
        if line.is_empty() {
            line.push_str(word);
        } else if fits {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// The `keys:` section of `fwterm --help`: each key padded to a column of
/// 18, or followed by three spaces when longer, its description wrapped to
/// 80 columns and continued under itself.
pub fn usage_keys() -> String {
    let mut out = String::new();
    for (key, what) in KEYS {
        let head = if key.len() < 18 {
            format!("  {key:<18}")
        } else {
            format!("  {key}   ")
        };
        let room = 80usize.saturating_sub(head.chars().count()).max(20);
        for (i, line) in wrap(what, room).into_iter().enumerate() {
            if i == 0 {
                out.push_str(&head);
            } else {
                out.push_str(&" ".repeat(20));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// The help sheet: what is scrolled past.
#[derive(Debug)]
pub struct Sheet {
    scroll: usize,
    /// The furthest the last `render` could scroll, so a key does not run
    /// past the end; `render` takes `&self`, hence the slot.
    max: Slot<usize>,
}

impl Default for Sheet {
    fn default() -> Self {
        Self::new()
    }
}

impl Sheet {
    pub fn new() -> Self {
        Sheet {
            scroll: 0,
            max: Slot::new(usize::MAX),
        }
    }

    /// Take a key; `true` means the sheet closes.
    pub fn key(&mut self, k: Key) -> bool {
        let max = self.max.get();
        match k {
            Key::Esc | Key::Enter => return true,
            Key::Up => self.scroll = self.scroll.saturating_sub(1),
            Key::Down => self.scroll = (self.scroll + 1).min(max),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(10),
            Key::PageDown => self.scroll = (self.scroll + 10).min(max),
            _ => {}
        }
        false
    }

    /// The table's rows for a sheet `w` wide: the key (on an entry's first
    /// line) and a line of its description, then a blank and the note.
    fn rows(w: usize) -> Vec<(Option<&'static str>, String, Paint)> {
        // The frame's two columns, the three before the key column, and
        // one kept clear before the right edge.
        let room = w.saturating_sub(5 + KEY_COL).max(8);
        let mut rows = Vec::new();
        for (key, what) in KEYS {
            for (i, line) in wrap(what, room).into_iter().enumerate() {
                rows.push(((i == 0).then_some(*key), line, Paint::Value));
            }
        }
        rows.push((None, String::new(), Paint::Note));
        for line in wrap(NOTE, w.saturating_sub(6).max(8)) {
            rows.push((None, line, Paint::Note));
        }
        rows
    }

    /// The sheet drawn `w x h` cells: a frame with accent corners, the
    /// keys down the left and what each does beside them, a line of keys
    /// at the bottom.
    pub fn render(&self, w: usize, h: usize) -> Vec<Vec<Cell>> {
        let (w, h) = (w.max(40), h.max(8));
        let mut g = settings_ui::panel(w, h, " [ HELP ] ");
        let rows = Self::rows(w);
        let shown = h - 5;
        let max = rows.len().saturating_sub(shown);
        self.max.set(max);
        let scroll = self.scroll.min(max);
        for (r, (key, line, paint)) in rows.iter().skip(scroll).take(shown).enumerate() {
            if let Some(key) = key {
                settings_ui::put(&mut g, 2 + r, 3, key, Paint::Button, false);
            }
            let left = if key.is_some() || *paint == Paint::Value {
                3 + KEY_COL
            } else {
                3
            };
            settings_ui::put(&mut g, 2 + r, left, line, *paint, false);
        }
        let mut hint = "Esc closes \u{b7} \u{2191}\u{2193} scroll".to_string();
        if scroll + shown < rows.len() {
            hint.push_str(&format!(
                " \u{b7} {} more below",
                rows.len() - scroll - shown
            ));
        }
        settings_ui::put(&mut g, h - 2, 3, &hint, Paint::Hint, false);
        g
    }
}

/// The description column's offset from the key column: the widest key and
/// two spaces.
const KEY_COL: usize = 24;

#[cfg(test)]
mod tests {
    use super::*;

    fn text(g: &[Vec<Cell>], r: usize) -> String {
        g[r].iter().map(|c| c.0).collect::<String>()
    }

    #[test]
    fn every_key_in_the_list_is_in_the_printed_usage() {
        let usage = usage_keys();
        for (key, what) in KEYS {
            let first = what.split(' ').next().unwrap();
            let line = usage
                .lines()
                .find(|l| l.starts_with(&format!("  {key}")))
                .unwrap_or_else(|| panic!("{key} missing from the usage"));
            assert!(line.contains(first), "{key}: {line}");
            assert!(
                key.len() + 2 >= 20 || line.find(first) == Some(20),
                "{line:?}"
            );
        }
        assert!(
            usage.lines().all(|l| l.chars().count() <= 80),
            "wrapped at 80"
        );
        assert!(usage.contains("  F1                this help\n"));
        assert!(
            usage.contains("  Shift+PgUp/PgDn, wheel   scroll the focused pane back / forward\n")
        );
    }

    #[test]
    fn the_sheet_shows_the_title_and_the_first_keys() {
        let s = Sheet::new();
        let g = s.render(60, 12);
        assert_eq!(g.len(), 12);
        assert!(text(&g, 0).starts_with("┌── [ HELP ] ─"), "{}", text(&g, 0));
        assert!(
            text(&g, 2).starts_with("│  Alt+Shift+Plus"),
            "{}",
            text(&g, 2)
        );
        assert!(text(&g, 2).contains("split the focused pane"));
        assert_eq!(g[2][3].1, Paint::Button, "keys in the accent");
        // At this width the first description wraps under itself.
        assert!(
            text(&g, 3).starts_with("│                          one to the right"),
            "{}",
            text(&g, 3)
        );
        assert!(
            text(&g, 4).starts_with("│  Alt+Shift+Minus"),
            "{}",
            text(&g, 4)
        );
        assert!(
            text(&g, 10).contains("Esc closes · ↑↓ scroll · "),
            "{}",
            text(&g, 10)
        );
        assert!(text(&g, 10).contains("more below"));
        // Wide enough, the long descriptions still fit the frame.
        let g = s.render(96, 22);
        assert!(g.iter().all(|row| row.len() == 96));
        assert!(text(&g, 2).ends_with("│"));
    }

    #[test]
    fn scrolling_past_the_end_clamps_and_shows_the_note_last() {
        let mut s = Sheet::new();
        s.render(60, 12);
        for _ in 0..500 {
            assert!(!s.key(Key::Down));
        }
        let g = s.render(60, 12);
        let max = Sheet::rows(60).len() - 7;
        assert_eq!(s.scroll, max);
        assert!(text(&g, 8).contains("open these too"), "{}", text(&g, 8));
        assert!(!text(&g, 10).contains("more below"), "{}", text(&g, 10));
        assert!(!s.key(Key::PageDown));
        assert_eq!(s.scroll, max, "a page past the end clamps too");
        for _ in 0..500 {
            s.key(Key::Up);
        }
        assert_eq!(s.scroll, 0);
        assert!(!s.key(Key::PageUp));
        assert_eq!(s.scroll, 0);
        assert!(s.key(Key::Esc), "Esc closes");
        assert!(s.key(Key::Enter), "Enter closes");
        assert!(!s.key(Key::Char('x')), "other keys do nothing");
    }

    #[test]
    fn wrapping_breaks_at_spaces_and_keeps_long_words_whole() {
        assert_eq!(wrap("a bb ccc", 5), ["a bb", "ccc"]);
        assert_eq!(wrap("abcdefgh ij", 4), ["abcdefgh", "ij"]);
        assert_eq!(wrap("", 4), Vec::<String>::new());
    }
}
