//! The settings sheet (design M5, the Mirai-05 board): Ctrl+, opens it over
//! the panes. It edits what `fwterm.conf` holds and saves it there, other
//! lines kept. Keys: Up/Down (or Tab) move, Left/Right change a choice, a
//! switch or a number, Enter edits text and numbers or presses a button,
//! Ctrl+S saves, Esc closes. Mouse: a click on a row selects it, on a value
//! does what Enter does there (the `‹` of a choice steps back), on a section
//! shows it, on a button presses it; the wheel moves.

use crate::logstore::Settings;

/// A field's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Choice(Vec<&'static str>, usize),
    Switch(bool),
    Text(String),
    Number(u32, u32, u32),
}

/// One row of the sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub section: &'static str,
    pub label: &'static str,
    /// The settings-file key it writes.
    pub key: &'static str,
    pub value: Value,
    /// It takes effect when saved; otherwise the next time fwterm starts.
    pub live: bool,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Close,
    /// Save these `(key, value)` pairs (`None`: remove the key), then close.
    Save(Vec<(&'static str, Option<String>)>),
}

/// A key as the sheet sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Up,
    Down,
    /// The help sheet's page keys; the settings sheet ignores them.
    PageUp,
    PageDown,
    Left,
    Right,
    Enter,
    Esc,
    Save,
    Backspace,
    Char(char),
}

pub const SECTIONS: [&str; 4] = ["Appearance", "Paste", "Logs", "Shell"];

/// The sheet: its fields, then the Save and Cancel buttons.
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub fields: Vec<Field>,
    /// The row with focus: a field, or `fields.len()` (Save) or one past
    /// (Cancel).
    pub at: usize,
    /// Text being typed into the focused field.
    pub editing: Option<String>,
}

impl Form {
    /// The sheet for the settings as the file has them now, `theme` the one
    /// in use.
    pub fn new(s: &Settings, theme: &str) -> Form {
        let themes: Vec<&'static str> = crate::theme::NAMES.to_vec();
        let t = themes
            .iter()
            .position(|n| n.eq_ignore_ascii_case(theme))
            .unwrap_or(0);
        let f = |section, label, key, value, live| Field {
            section,
            label,
            key,
            value,
            live,
        };
        Form {
            fields: vec![
                f(
                    "Appearance",
                    "Theme",
                    "theme",
                    Value::Choice(themes, t),
                    true,
                ),
                f(
                    "Appearance",
                    "Font",
                    "font",
                    Value::Text(s.font.clone().unwrap_or_default()),
                    false,
                ),
                f(
                    "Appearance",
                    "Size (px)",
                    "px",
                    Value::Number(s.px.unwrap_or(16), 6, 72),
                    false,
                ),
                f(
                    "Appearance",
                    "Instrument strip",
                    "instruments",
                    Value::Switch(s.instruments.unwrap_or(true)),
                    true,
                ),
                f(
                    "Appearance",
                    "Activity ribbon",
                    "ribbon",
                    Value::Switch(s.ribbon.unwrap_or(true)),
                    true,
                ),
                f(
                    "Paste",
                    "Ask before several lines",
                    "paste_warning",
                    Value::Switch(s.paste_warning.unwrap_or(true)),
                    true,
                ),
                f(
                    "Logs",
                    "Log folder",
                    "log_dir",
                    Value::Text(
                        s.log_dir
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    ),
                    false,
                ),
                f(
                    "Logs",
                    "Keep up to (GB)",
                    "log_cap_gib",
                    Value::Number(
                        (s.log_cap.unwrap_or(crate::logstore::DEFAULT_CAP) >> 30) as u32,
                        1,
                        100_000,
                    ),
                    false,
                ),
                f(
                    "Shell",
                    "Program in each pane",
                    "shell",
                    Value::Text(s.shell.clone().unwrap_or_default()),
                    false,
                ),
            ],
            at: 0,
            editing: None,
        }
    }

    fn rows(&self) -> usize {
        self.fields.len() + 2
    }

    /// The section the focused row belongs to (the buttons: the last one's).
    pub fn section(&self) -> &'static str {
        self.fields[self.at.min(self.fields.len() - 1)].section
    }

    /// Act on a key.
    pub fn key(&mut self, k: Key) -> Action {
        if let Some(text) = &mut self.editing {
            match k {
                Key::Char(c) if !c.is_control() => text.push(c),
                Key::Backspace => {
                    text.pop();
                }
                Key::Enter => {
                    let t = self.editing.take().unwrap_or_default();
                    let field = &mut self.fields[self.at];
                    match &mut field.value {
                        Value::Text(v) => *v = t.trim().to_string(),
                        Value::Number(v, lo, hi) => {
                            if let Ok(n) = t.trim().parse::<u32>() {
                                *v = n.clamp(*lo, *hi);
                            }
                        }
                        _ => {}
                    }
                }
                Key::Esc => self.editing = None,
                _ => {}
            }
            return Action::None;
        }
        let n = self.rows();
        match k {
            Key::Up => self.at = (self.at + n - 1) % n,
            Key::Down | Key::Char('\t') => self.at = (self.at + 1) % n,
            Key::Left | Key::Right => {
                let step = if k == Key::Left { -1i64 } else { 1 };
                if let Some(field) = self.fields.get_mut(self.at) {
                    match &mut field.value {
                        Value::Choice(opts, i) => {
                            *i = ((*i as i64 + step).rem_euclid(opts.len() as i64)) as usize
                        }
                        Value::Switch(on) => *on = !*on,
                        Value::Number(v, lo, hi) => {
                            *v = ((*v as i64 + step).clamp(*lo as i64, *hi as i64)) as u32
                        }
                        Value::Text(_) => {}
                    }
                } else {
                    // Between the buttons.
                    self.at = if self.at == self.fields.len() {
                        n - 1
                    } else {
                        n - 2
                    };
                }
            }
            Key::Char(' ') | Key::Enter if self.at == self.fields.len() => {
                return Action::Save(self.values())
            }
            Key::Char(' ') | Key::Enter if self.at > self.fields.len() => return Action::Close,
            Key::Char(' ') | Key::Enter => match &mut self.fields[self.at].value {
                Value::Switch(on) => *on = !*on,
                Value::Choice(opts, i) => *i = (*i + 1) % opts.len(),
                Value::Text(v) => self.editing = Some(v.clone()),
                Value::Number(v, _, _) => self.editing = Some(v.to_string()),
            },
            Key::Save => return Action::Save(self.values()),
            Key::Esc => return Action::Close,
            _ => {}
        }
        Action::None
    }

    /// Act on a click at cell `(row, col)` of the sheet drawn `w x h` (the
    /// same `w, h` as [`render`]; the cell relative to the sheet's top-left).
    /// A click while typing first takes the text, as Enter would. A click on
    /// the frame, the hints or empty space does nothing.
    pub fn click(&mut self, row: usize, col: usize, w: usize, h: usize) -> Action {
        if self.editing.is_some() {
            self.key(Key::Enter);
        }
        let l = Layout::of(w, h);
        if row == l.button_row {
            if (l.save_col..l.save_col + SAVE.len()).contains(&col) {
                return Action::Save(self.values());
            }
            if (l.cancel_col..l.cancel_col + CANCEL.len()).contains(&col) {
                return Action::Close;
            }
            return Action::None;
        }
        // A section, left of the rule: its first field gets the focus.
        if col < NAV {
            if let Some(s) = SECTIONS
                .iter()
                .enumerate()
                .find(|(i, _)| Layout::section_row(*i) == row)
                .map(|(_, s)| *s)
            {
                if let Some(i) = self.fields.iter().position(|f| f.section == s) {
                    self.at = i;
                }
            }
            return Action::None;
        }
        // A field of the section shown: selected, and its value acted on.
        if !(l.left - 1..l.w - 2).contains(&col) {
            return Action::None;
        }
        let on = self.section();
        let hit = self
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.section == on)
            .enumerate()
            .find(|(n, _)| Layout::field_row(*n) == row)
            .map(|(_, (i, _))| i);
        if let Some(i) = hit {
            self.at = i;
            let value_col = l.left + VALUE;
            if col == value_col && matches!(self.fields[i].value, Value::Choice(..)) {
                self.key(Key::Left);
            } else if col >= value_col {
                self.key(Key::Enter);
            }
        }
        Action::None
    }

    /// The wheel: a notch moves the focus as Up or Down would.
    pub fn scroll(&mut self, up: bool) {
        self.key(if up { Key::Up } else { Key::Down });
    }

    /// Every field as the settings file writes it.
    pub fn values(&self) -> Vec<(&'static str, Option<String>)> {
        self.fields
            .iter()
            .map(|f| {
                let v = match &f.value {
                    Value::Choice(opts, i) => Some(opts[*i].to_string()),
                    Value::Switch(on) => Some(if *on { "on" } else { "off" }.to_string()),
                    Value::Text(t) if t.is_empty() => None,
                    Value::Text(t) => Some(t.clone()),
                    Value::Number(n, _, _) => Some(n.to_string()),
                };
                (f.key, v)
            })
            .collect()
    }

    /// How a field's value reads.
    pub fn shown(&self, i: usize) -> String {
        let f = &self.fields[i];
        if i == self.at {
            if let Some(t) = &self.editing {
                return format!("{t}_");
            }
        }
        match &f.value {
            Value::Choice(opts, k) => format!("‹ {} ›", opts[*k]),
            Value::Switch(true) => "[■] on".into(),
            Value::Switch(false) => "[ ] off".into(),
            Value::Text(t) if t.is_empty() => match f.key {
                "font" => "JetBrains Mono (built in)".into(),
                "log_dir" => "%LOCALAPPDATA%\\foldwave\\logs".into(),
                "shell" => "cmd.exe".into(),
                _ => "—".into(),
            },
            Value::Text(t) => t.clone(),
            Value::Number(n, _, _) => n.to_string(),
        }
    }
}

/// `text` (a settings file) with each of `values` set: an existing line for
/// a key is replaced where it stands, a new key is added at the end, and
/// `None` removes the key. Every other line, comments too, is kept.
pub fn merged(text: &str, values: &[(&str, Option<String>)]) -> String {
    let key_of = |line: &str| {
        let t = line.trim();
        (!t.starts_with('#'))
            .then(|| t.split('=').next().map(str::trim))
            .flatten()
            .map(str::to_string)
    };
    let mut done = vec![false; values.len()];
    let mut out = String::new();
    for line in text.lines() {
        match key_of(line).and_then(|k| values.iter().position(|(v, _)| *v == k)) {
            Some(i) if done[i] => {}
            Some(i) => {
                done[i] = true;
                if let Some(v) = &values[i].1 {
                    out.push_str(&format!("{} = {v}\n", values[i].0));
                }
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for (i, (k, v)) in values.iter().enumerate() {
        if let (false, Some(v)) = (done[i], v) {
            out.push_str(&format!("{k} = {v}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form() -> Form {
        Form::new(&Settings::default(), "mirai")
    }

    #[test]
    fn keys_move_change_edit_and_save() {
        let mut f = form();
        assert_eq!(f.shown(0), "‹ mirai ›");
        assert_eq!(f.key(Key::Right), Action::None);
        assert_eq!(f.shown(0), "‹ classic ›", "a choice cycles");
        f.key(Key::Down);
        f.key(Key::Enter);
        for c in "Cascadia Mono".chars() {
            f.key(Key::Char(c));
        }
        assert_eq!(f.shown(1), "Cascadia Mono_", "typing shows a cursor");
        f.key(Key::Enter);
        f.key(Key::Down);
        f.key(Key::Right);
        f.key(Key::Right);
        assert_eq!(f.shown(2), "18", "a number steps");
        f.key(Key::Enter);
        f.key(Key::Backspace);
        f.key(Key::Backspace);
        f.key(Key::Char('9'));
        f.key(Key::Char('9'));
        f.key(Key::Enter);
        assert_eq!(f.shown(2), "72", "a typed number is clamped");
        f.key(Key::Down);
        f.key(Key::Char(' '));
        assert_eq!(f.shown(3), "[ ] off", "space flips a switch");
        let Action::Save(v) = f.key(Key::Save) else {
            panic!("Ctrl+S saves");
        };
        assert_eq!(
            v,
            [
                ("theme", Some("classic".into())),
                ("font", Some("Cascadia Mono".into())),
                ("px", Some("72".into())),
                ("instruments", Some("off".into())),
                ("ribbon", Some("on".into())),
                ("paste_warning", Some("on".into())),
                ("log_dir", None),
                ("log_cap_gib", Some("10".into())),
                ("shell", None),
            ]
        );
    }

    #[test]
    fn the_buttons_save_or_cancel_and_esc_backs_out_of_typing_first() {
        let mut f = form();
        f.key(Key::Up);
        assert_eq!(f.at, f.fields.len() + 1, "up from the top wraps to Cancel");
        assert_eq!(f.key(Key::Enter), Action::Close);
        f.key(Key::Left);
        assert_eq!(f.at, f.fields.len(), "left moves to Save");
        assert!(matches!(f.key(Key::Enter), Action::Save(_)));
        let mut g = form();
        g.key(Key::Down);
        g.key(Key::Enter);
        g.key(Key::Char('x'));
        assert_eq!(g.key(Key::Esc), Action::None, "Esc drops the typing");
        assert_eq!(g.editing, None);
        assert_eq!(g.key(Key::Esc), Action::Close, "then closes the sheet");
        assert_eq!(g.section(), "Appearance");
    }

    #[test]
    fn saving_keeps_the_rest_of_the_file() {
        let text = "# fwterm\nlog_dir = E:\\logs  # fast\ncolor.accent = #ff8800\ntheme = mirai\nfont = Consolas\n";
        let out = merged(
            text,
            &[
                ("theme", Some("classic".into())),
                ("font", None),
                ("px", Some("18".into())),
            ],
        );
        assert_eq!(
            out,
            "# fwterm\nlog_dir = E:\\logs  # fast\ncolor.accent = #ff8800\ntheme = classic\npx = 18\n"
        );
    }
}

/// How a cell of the sheet is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    Rule,
    Corner,
    Title,
    Nav,
    NavOn,
    Label,
    Value,
    Note,
    Hint,
    Button,
    /// A meter's filled part, and the rest of its track.
    Meter,
    Track,
    /// Something wrong: a drive too full, one that cannot be written.
    Warn,
    /// A block of terminal colour `n` (the theme's `ansi<n>`).
    Swatch(u8),
}

/// One cell: its character, paint, and whether it is on the focused row.
pub type Cell = (char, Paint, bool);

/// Write `text` from cell `(r, c)` on, cut at the grid's edge.
pub fn put(g: &mut [Vec<Cell>], r: usize, c: usize, text: &str, p: Paint, sel: bool) {
    for (i, ch) in text.chars().enumerate() {
        if let Some(cell) = g.get_mut(r).and_then(|row| row.get_mut(c + i)) {
            *cell = (ch, p, sel);
        }
    }
}

/// A `w x h` panel: hairline edges, accent corners, `title` in the top edge.
pub fn panel(w: usize, h: usize, title: &str) -> Vec<Vec<Cell>> {
    let mut g: Vec<Vec<Cell>> = vec![vec![(' ', Paint::Label, false); w]; h];
    for c in 1..w - 1 {
        put(&mut g, 0, c, "─", Paint::Rule, false);
        put(&mut g, h - 1, c, "─", Paint::Rule, false);
    }
    for r in 1..h - 1 {
        put(&mut g, r, 0, "│", Paint::Rule, false);
        put(&mut g, r, w - 1, "│", Paint::Rule, false);
    }
    put(&mut g, 0, 0, "┌──", Paint::Corner, false);
    put(&mut g, 0, w - 3, "──┐", Paint::Corner, false);
    put(&mut g, h - 1, 0, "└──", Paint::Corner, false);
    put(&mut g, h - 1, w - 3, "──┘", Paint::Corner, false);
    put(&mut g, 0, 3, title, Paint::Title, false);
    g
}

/// The sheet is never drawn smaller than this: `(w, h)` as [`render`] takes
/// them, as it draws them.
pub fn fitted(w: usize, h: usize) -> (usize, usize) {
    (w.max(40), h.max(12))
}

/// The column of the rule between the sections and the fields.
const NAV: usize = 20;
/// A value starts this many columns after its label.
const VALUE: usize = 26;
const SAVE: &str = "[ SAVE ]";
const CANCEL: &str = "[ CANCEL ]";

/// Where the sheet drawn `w x h` puts things: one place for [`render`] and
/// [`Form::click`], so a change in the drawing cannot leave a click behind.
struct Layout {
    w: usize,
    h: usize,
    /// Where a field's label starts; its value at `left + VALUE`.
    left: usize,
    /// The buttons' row, and each button's first column.
    button_row: usize,
    save_col: usize,
    cancel_col: usize,
}

impl Layout {
    fn of(w: usize, h: usize) -> Layout {
        let (w, h) = fitted(w, h);
        let cancel_col = w - 3 - CANCEL.len();
        Layout {
            w,
            h,
            left: NAV + 3,
            button_row: h - 3,
            save_col: cancel_col - 2 - SAVE.len(),
            cancel_col,
        }
    }

    /// The row section `i` is named on.
    fn section_row(i: usize) -> usize {
        2 + i * 2
    }

    /// The row the `n`th field of the section shown is on.
    fn field_row(n: usize) -> usize {
        2 + n * 2
    }
}

/// The sheet drawn `w x h` cells: a frame with accent corners, the sections
/// down the left, the focused section's fields on the right, the buttons
/// and a line of keys at the bottom.
pub fn render(form: &Form, w: usize, h: usize) -> Vec<Vec<Cell>> {
    let l = Layout::of(w, h);
    let (w, h) = (l.w, l.h);
    let mut g = panel(w, h, " [ SETTINGS ] ");
    // The sections, and the line between them and the fields.
    for r in 2..h - 4 {
        put(&mut g, r, NAV, "│", Paint::Rule, false);
    }
    let on = form.section();
    for (i, s) in SECTIONS.iter().enumerate() {
        let (mark, paint) = if *s == on {
            ("▸ ", Paint::NavOn)
        } else {
            ("  ", Paint::Nav)
        };
        put(
            &mut g,
            Layout::section_row(i),
            3,
            &format!("{mark}{s}"),
            paint,
            false,
        );
    }
    // The focused section's fields.
    let left = l.left;
    for (n, (i, f)) in form
        .fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.section == on)
        .enumerate()
    {
        let r = Layout::field_row(n);
        let sel = i == form.at;
        if sel {
            for c in left - 1..w - 2 {
                put(&mut g, r, c, " ", Paint::Label, true);
            }
        }
        put(&mut g, r, left, f.label, Paint::Label, sel);
        // The value, cut to what fits before its note.
        let note = if f.live { "" } else { "next start" };
        let room = (w - 3 - note.len()).saturating_sub(left + VALUE + 1);
        let shown = form.shown(i);
        let shown: String = if shown.chars().count() > room {
            shown
                .chars()
                .take(room.saturating_sub(1))
                .chain(['…'])
                .collect()
        } else {
            shown
        };
        put(&mut g, r, left + VALUE, &shown, Paint::Value, sel);
        if !note.is_empty() {
            put(&mut g, r, w - 3 - note.len(), note, Paint::Note, sel);
        }
    }
    // The buttons.
    let br = l.button_row;
    put(
        &mut g,
        br,
        l.save_col,
        SAVE,
        Paint::Button,
        form.at == form.fields.len(),
    );
    put(
        &mut g,
        br,
        l.cancel_col,
        CANCEL,
        Paint::Button,
        form.at == form.fields.len() + 1,
    );
    put(
        &mut g,
        br,
        3,
        "↑↓ move · ←→ change · Enter edit · Ctrl+S save · Esc close",
        Paint::Hint,
        false,
    );
    put(
        &mut g,
        h - 2,
        3,
        "Saved to fwterm.conf, where color.<name> = #rrggbb changes any colour",
        Paint::Hint,
        false,
    );
    g
}

#[cfg(test)]
mod render_tests {
    use super::*;

    fn text(g: &[Vec<Cell>], r: usize) -> String {
        g[r].iter().map(|c| c.0).collect()
    }

    #[test]
    fn the_sheet_shows_the_focused_section_and_marks_the_focused_row() {
        let mut f = Form::new(&Settings::default(), "mirai");
        let g = render(&f, 90, 20);
        assert_eq!(g.len(), 20);
        assert!(text(&g, 0).starts_with("┌── [ SETTINGS ] ─"));
        assert!(text(&g, 2).contains("▸ Appearance"));
        assert!(text(&g, 2).contains("Theme") && text(&g, 2).contains("‹ mirai ›"));
        assert!(g[2].iter().any(|c| c.2), "the focused row is marked");
        assert!(
            text(&g, 4).contains("JetBrains Mono (built in)") && text(&g, 4).contains("next start")
        );
        assert!(
            !text(&g, 2).contains("next start"),
            "the theme applies at once"
        );
        // Down to the paste switch: its section shows instead.
        for _ in 0..5 {
            f.key(Key::Down);
        }
        let g = render(&f, 90, 20);
        assert!(text(&g, 4).contains("▸ Paste"));
        assert!(text(&g, 2).contains("Ask before several lines") && text(&g, 2).contains("[■] on"));
        assert!(text(&g, 17).contains("[ SAVE ]") && text(&g, 17).contains("[ CANCEL ]"));
        // A long value is cut before its note, never over it.
        let mut s = Settings::default();
        s.log_dir = Some(std::path::PathBuf::from(format!(
            "E:\\{}",
            "deep\\".repeat(20)
        )));
        let mut f = Form::new(&s, "mirai");
        for _ in 0..6 {
            f.key(Key::Down);
        }
        let g = render(&f, 90, 20);
        let row = text(&g, 2);
        assert!(row.contains("… next start"), "{row}");
    }

    /// Where `s` is on the rendered sheet: `(row, col)` of its first cell.
    fn find(g: &[Vec<Cell>], s: &str) -> (usize, usize) {
        let want: Vec<char> = s.chars().collect();
        for (r, row) in g.iter().enumerate() {
            let chars: Vec<char> = row.iter().map(|c| c.0).collect();
            if let Some(c) = chars.windows(want.len()).position(|w| w == want) {
                return (r, c);
            }
        }
        panic!("{s:?} is not on the sheet");
    }

    #[test]
    fn clicks_on_the_drawn_sheet_select_flip_step_edit_and_press() {
        let (w, h) = (90, 20);
        let mut f = Form::new(&Settings::default(), "mirai");
        let g = render(&f, w, h);
        // A label selects its row; the value of a switch flips it.
        let (r, c) = find(&g, "Instrument strip");
        assert_eq!(f.click(r, c, w, h), Action::None);
        assert_eq!(f.at, 3);
        assert_eq!(f.shown(3), "[■] on");
        let (r, c) = find(&g, "[■] on");
        f.click(r, c + 1, w, h);
        assert_eq!(f.shown(3), "[ ] off", "a click on the switch flips it");
        // A choice steps forward from its name and back from its ‹.
        let (r, c) = find(&g, "‹ mirai ›");
        f.click(r, c + 4, w, h);
        assert_eq!((f.at, f.shown(0).as_str()), (0, "‹ classic ›"));
        f.click(r, c, w, h);
        assert_eq!(f.shown(0), "‹ mirai ›", "the ‹ steps back");
        // A text value opens for typing; a click elsewhere takes the text.
        let (r, c) = find(&g, "JetBrains Mono (built in)");
        f.click(r, c, w, h);
        assert_eq!((f.at, f.editing.as_deref()), (1, Some("")));
        f.key(Key::Char('C'));
        let (r, c) = find(&g, "Size (px)");
        f.click(r, c, w, h);
        assert_eq!((f.at, f.editing.as_deref()), (2, None));
        assert_eq!(f.shown(1), "C");
        // A section on the left shows it, its first field focused.
        let (r, c) = find(&g, "  Paste");
        f.click(r, c + 2, w, h);
        assert_eq!((f.at, f.section()), (5, "Paste"));
        assert!(text(&render(&f, w, h), 4).contains("▸ Paste"));
        // The buttons.
        let (r, c) = find(&g, "[ CANCEL ]");
        assert_eq!(f.click(r, c + 3, w, h), Action::Close);
        let (r, c) = find(&g, "[ SAVE ]");
        let Action::Save(v) = f.click(r, c + 7, w, h) else {
            panic!("the SAVE button saves");
        };
        assert_eq!(v[0], ("theme", Some("mirai".into())));
        assert_eq!(v[1], ("font", Some("C".into())));
        assert_eq!(v[3], ("instruments", Some("off".into())));
    }

    #[test]
    fn clicks_on_the_frame_the_hints_and_empty_space_do_nothing() {
        let (w, h) = (90, 20);
        let mut f = Form::new(&Settings::default(), "mirai");
        f.key(Key::Down);
        let before = f.clone();
        let g = render(&f, w, h);
        let (hr, hc) = find(&g, "↑↓ move");
        let (sr, sc) = find(&g, "Saved to fwterm.conf");
        for (r, c) in [
            (0, 0),
            (0, 5),
            (1, 30),
            (3, 40),
            (h - 1, 10),
            (hr, hc),
            (sr, sc),
            (2, 21),
            (2, 88),
            (12, 50),
        ] {
            assert_eq!(f.click(r, c, w, h), Action::None, "({r}, {c})");
            assert_eq!(f, before, "({r}, {c}) changed the sheet");
        }
        // The wheel moves the focus a row a notch, and wraps as Up does.
        f.scroll(false);
        assert_eq!(f.at, 2);
        f.scroll(true);
        f.scroll(true);
        f.scroll(true);
        assert_eq!(f.at, f.fields.len() + 1);
    }
}
