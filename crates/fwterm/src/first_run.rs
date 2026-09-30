//! First run (design M5, the Mirai-06 board): before any pane starts, three
//! steps in the window. Shell: the program each pane runs, from those found
//! on the machine. Logs: every fixed drive measured for durable writes in
//! the background, the fastest with room for the cap picked unless one is
//! chosen. Look: the theme, previewed live, and the instrument strip. Finish
//! writes them to `fwterm.conf`; Skip writes nothing, so it shows again.
//!
//! Keys: Up/Down (or Tab) move, Space chooses, Left/Right change a choice,
//! a switch or the cap (digits type it), Enter chooses and goes on, Esc goes
//! back (on the first step it skips).

use crate::logstore;
use crate::settings_ui::{panel, put, Cell, Paint};
use std::path::{Path, PathBuf};

/// A shell found on this machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Shell {
    pub name: &'static str,
    /// The command line saved as `shell =`.
    pub command: String,
    /// fwterm marks its prompts itself, so blocks and exit codes work with
    /// no setup (cmd.exe; others need OSC 133 in their prompt).
    pub marks: bool,
}

/// The shells on this machine, the likeliest first: PowerShell 7, Windows
/// PowerShell, Command Prompt, Git Bash, WSL. `var` reads an environment
/// variable and `exists` says whether a file is there. Never empty: with
/// nothing found, Command Prompt by name.
pub fn find_shells(
    var: &dyn Fn(&str) -> Option<String>,
    exists: &dyn Fn(&Path) -> bool,
) -> Vec<Shell> {
    let system = var("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let programs = var("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
    // Found on PATH: saved by name, so PATH finds it again at launch (a
    // Store install's folder carries its version and moves on update).
    let on_path = |exe: &str| {
        var("PATH")
            .unwrap_or_default()
            .split(';')
            .filter(|d| !d.trim().is_empty())
            .any(|d| exists(&Path::new(d.trim()).join(exe)))
            .then(|| PathBuf::from(exe))
    };
    let at = |p: PathBuf| exists(&p).then_some(p);
    let mut out = Vec::new();
    let mut add = |name, path: Option<PathBuf>, args: &str, marks| {
        if let Some(p) = path {
            let exe = quote(&p.display().to_string());
            let command = if args.is_empty() {
                exe
            } else {
                format!("{exe} {args}")
            };
            out.push(Shell {
                name,
                command,
                marks,
            });
        }
    };
    add(
        "PowerShell 7",
        on_path("pwsh.exe").or_else(|| at(Path::new(&programs).join("PowerShell\\7\\pwsh.exe"))),
        "-NoLogo",
        false,
    );
    add(
        "Windows PowerShell",
        at(Path::new(&system).join("System32\\WindowsPowerShell\\v1.0\\powershell.exe")),
        "-NoLogo",
        false,
    );
    add(
        "Command Prompt",
        at(Path::new(&system).join("System32\\cmd.exe")),
        "",
        true,
    );
    add(
        "Git Bash",
        at(Path::new(&programs).join("Git\\bin\\bash.exe")),
        "--login -i",
        false,
    );
    add(
        "WSL",
        at(Path::new(&system).join("System32\\wsl.exe")),
        "",
        false,
    );
    if out.is_empty() {
        out.push(Shell {
            name: "Command Prompt",
            command: "cmd.exe".into(),
            marks: true,
        });
    }
    out
}

/// `s` in double quotes when it holds a space.
fn quote(s: &str) -> String {
    if s.contains(' ') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

/// A saved command line as a program and its arguments: split at spaces,
/// except inside double quotes, which are dropped.
pub fn split_command(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut any) = (false, false);
    for ch in s.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(word);
    }
    out
}

/// How far a drive's measurement has got.
#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    Waiting,
    /// Durable writes done so far, of [`PROBE_WRITES`].
    Measuring(u32),
    /// MiB/s, and the p99 of one write in ms.
    Done(f64, f64),
    Failed(String),
}

/// Writes of 1 MiB in one drive's measurement.
pub const PROBE_WRITES: u32 = 128;

/// A fixed drive, its space and its measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    pub root: PathBuf,
    pub free: u64,
    pub total: u64,
    pub probe: Probe,
}

/// The three steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Shell,
    Logs,
    Look,
}

/// What a key did.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    /// Leave without saving.
    Skip,
    /// Save these `(key, value)` pairs (`None`: remove the key).
    Finish(Vec<(&'static str, Option<String>)>),
}

/// A key as the first run sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    Char(char),
}

/// The first run's state.
#[derive(Debug, Clone, PartialEq)]
pub struct FirstRun {
    pub step: Step,
    /// The focused row of this step: an option, a field, or one of the two
    /// buttons (the last two rows).
    pub at: usize,
    pub shells: Vec<Shell>,
    pub shell: usize,
    pub drives: Vec<Drive>,
    /// The drive chosen by hand; otherwise the fastest with room.
    pub picked: Option<usize>,
    pub cap_gib: u32,
    /// The cap's digits are being typed (the first digit starts afresh).
    typing: bool,
    pub themes: Vec<&'static str>,
    pub theme: usize,
    pub instruments: bool,
}

impl FirstRun {
    /// A first run over `shells` and `drives` (measurements to come), with
    /// what `s` (the settings file, if any) already says chosen.
    pub fn new(shells: Vec<Shell>, drives: Vec<Drive>, s: &logstore::Settings) -> FirstRun {
        let themes = crate::theme::NAMES.to_vec();
        let shell = s
            .shell
            .as_ref()
            .and_then(|c| shells.iter().position(|sh| &sh.command == c))
            .unwrap_or(0);
        let theme = s
            .theme
            .as_ref()
            .and_then(|t| themes.iter().position(|n| n.eq_ignore_ascii_case(t)))
            .unwrap_or(0);
        FirstRun {
            step: Step::Shell,
            at: shell,
            shells,
            shell,
            drives,
            picked: None,
            cap_gib: (s.log_cap.unwrap_or(logstore::DEFAULT_CAP) >> 30).max(1) as u32,
            typing: false,
            themes,
            theme,
            instruments: s.instruments.unwrap_or(true),
        }
    }

    /// The theme to draw with now: the one chosen, previewed live.
    pub fn theme_name(&self) -> &'static str {
        self.themes[self.theme]
    }

    fn cap(&self) -> u64 {
        u64::from(self.cap_gib) << 30
    }

    /// `d` holds the cap above its reserve.
    pub fn has_room(&self, d: &Drive) -> bool {
        !logstore::budget(self.cap(), 0, d.free, d.total).squeezed
    }

    /// The fastest drive measured so far with room for the cap.
    pub fn fastest(&self) -> Option<usize> {
        self.drives
            .iter()
            .enumerate()
            .filter(|(_, d)| self.has_room(d))
            .filter_map(|(i, d)| match d.probe {
                Probe::Done(rate, _) => Some((i, rate)),
                _ => None,
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The drive the logs go to: the one picked, unless it cannot be
    /// written, else the fastest with room.
    pub fn chosen(&self) -> Option<usize> {
        self.picked
            .filter(|&i| !matches!(self.drives[i].probe, Probe::Failed(_)))
            .or_else(|| self.fastest())
    }

    /// The log folder on the chosen drive; `None` leaves the OS's place.
    pub fn log_dir(&self) -> Option<PathBuf> {
        self.chosen()
            .map(|i| self.drives[i].root.join("foldwave").join("logs"))
    }

    /// Every drive measured, or given up on.
    pub fn measured(&self) -> bool {
        self.drives
            .iter()
            .all(|d| matches!(d.probe, Probe::Done(..) | Probe::Failed(_)))
    }

    /// A measurement's progress or result for drive `i`.
    pub fn update(&mut self, i: usize, probe: Probe) {
        if let Some(d) = self.drives.get_mut(i) {
            d.probe = probe;
        }
    }

    /// The step's rows before its two buttons.
    fn items(&self) -> usize {
        match self.step {
            Step::Shell => self.shells.len(),
            // The drives, then the cap.
            Step::Logs => self.drives.len() + 1,
            // The theme, then the strip.
            Step::Look => 2,
        }
    }

    fn back_button(&self) -> usize {
        self.items()
    }

    fn next_button(&self) -> usize {
        self.items() + 1
    }

    fn on_cap(&self) -> bool {
        self.step == Step::Logs && self.at == self.drives.len()
    }

    /// Choose the focused option (a shell or a drive), if it is one.
    fn choose(&mut self) {
        match self.step {
            Step::Shell if self.at < self.shells.len() => self.shell = self.at,
            Step::Logs if self.at < self.drives.len() => self.picked = Some(self.at),
            _ => {}
        }
    }

    fn change(&mut self, step: i64) {
        match (self.step, self.at) {
            (Step::Look, 0) => {
                let n = self.themes.len() as i64;
                self.theme = (self.theme as i64 + step).rem_euclid(n) as usize;
            }
            (Step::Look, 1) => self.instruments = !self.instruments,
            _ if self.on_cap() => {
                self.cap_gib = (self.cap_gib as i64 + step).clamp(1, 100_000) as u32;
                self.typing = false;
            }
            _ => {}
        }
    }

    fn go(&mut self, step: Step) {
        self.step = step;
        self.typing = false;
        self.at = match step {
            Step::Shell => self.shell,
            // A drive picked by hand; else Continue, so Enter keeps the
            // fastest, which may change while the rest are measured.
            Step::Logs => self.picked.unwrap_or(self.next_button()),
            Step::Look => 0,
        };
    }

    fn back(&mut self) -> Action {
        match self.step {
            Step::Shell => return Action::Skip,
            Step::Logs => self.go(Step::Shell),
            Step::Look => self.go(Step::Logs),
        }
        Action::None
    }

    fn next(&mut self) -> Action {
        match self.step {
            Step::Shell => self.go(Step::Logs),
            Step::Logs => self.go(Step::Look),
            Step::Look => return Action::Finish(self.values()),
        }
        Action::None
    }

    /// Act on a key.
    pub fn key(&mut self, k: Key) -> Action {
        let n = self.items() + 2;
        if self.on_cap() {
            match k {
                Key::Char(c) if c.is_ascii_digit() => {
                    let d = c as u32 - '0' as u32;
                    let base = if self.typing { self.cap_gib } else { 0 };
                    self.cap_gib = (base.saturating_mul(10) + d).clamp(1, 100_000);
                    self.typing = true;
                    return Action::None;
                }
                Key::Backspace => {
                    self.cap_gib = (self.cap_gib / 10).max(1);
                    self.typing = true;
                    return Action::None;
                }
                _ => {}
            }
        }
        match k {
            Key::Up => {
                self.at = (self.at + n - 1) % n;
                self.typing = false;
            }
            Key::Down | Key::Char('\t') => {
                self.at = (self.at + 1) % n;
                self.typing = false;
            }
            Key::Left | Key::Right => {
                let step = if k == Key::Left { -1 } else { 1 };
                if self.at >= self.back_button() {
                    self.at = if self.at == self.back_button() {
                        self.next_button()
                    } else {
                        self.back_button()
                    };
                } else {
                    self.change(step);
                }
            }
            Key::Char(' ') if self.at == self.back_button() => return self.back(),
            Key::Char(' ') if self.at == self.next_button() => return self.next(),
            Key::Char(' ') => {
                self.choose();
                self.change(1);
            }
            Key::Enter if self.at == self.back_button() => return self.back(),
            Key::Enter => {
                self.choose();
                return self.next();
            }
            Key::Esc => return self.back(),
            _ => {}
        }
        Action::None
    }

    /// Everything the first run sets, as the settings file writes it.
    pub fn values(&self) -> Vec<(&'static str, Option<String>)> {
        vec![
            ("shell", Some(self.shells[self.shell].command.clone())),
            ("log_dir", self.log_dir().map(|d| d.display().to_string())),
            ("log_cap_gib", Some(self.cap_gib.to_string())),
            ("theme", Some(self.theme_name().to_string())),
            (
                "instruments",
                Some(if self.instruments { "on" } else { "off" }.to_string()),
            ),
        ]
    }
}

/// `text` in lines of at most `width` characters, broken at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// `s` cut to `room` characters, with `…` where it was cut.
fn fit(s: &str, room: usize) -> String {
    if s.chars().count() <= room {
        s.to_string()
    } else {
        s.chars()
            .take(room.saturating_sub(1))
            .chain(['…'])
            .collect()
    }
}

/// The first run drawn `w x h` cells: the step's question and the steps
/// down the left, its choices on the right, the buttons, a line of keys.
pub fn render(fr: &FirstRun, w: usize, h: usize) -> Vec<Vec<Cell>> {
    let (w, h) = (w.max(60), h.max(20));
    let mut g = panel(w, h, " [ FOLDWAVE ] · FIRST RUN ");
    let left = if w >= 90 { 30 } else { 24 };
    for r in 2..h - 4 {
        put(&mut g, r, left + 2, "│", Paint::Rule, false);
    }
    // The question and why it is asked.
    let (heading, why) = match fr.step {
        Step::Shell => (
            "Which shell opens in each pane?",
            "Each new pane runs it. Change it later with Ctrl+Comma, or run any \
             program with fwterm -- <program>.",
        ),
        Step::Logs => (
            "Where should pane logs live?",
            "Every pane keeps its whole output on disk, so scrollback and \
             search reach back hours. foldwave measures how fast each drive \
             takes durable writes and picks the fastest with room.",
        ),
        Step::Look => (
            "How should it look?",
            "Mirai is foldwave's own look, shown here as you choose. Any \
             colour can be changed later: color.<name> = #rrggbb in \
             fwterm.conf.",
        ),
    };
    let mut r = 2;
    put(&mut g, r, 3, "[ FOLDWAVE ]", Paint::Title, false);
    r += 2;
    for line in wrap(heading, left - 2) {
        put(&mut g, r, 3, &line, Paint::Label, false);
        r += 1;
    }
    r += 1;
    for line in wrap(why, left - 2) {
        if r + 9 >= h {
            break;
        }
        put(&mut g, r, 3, &line, Paint::Hint, false);
        r += 1;
    }
    // The steps, this one lit.
    let here = fr.step as usize;
    for (i, name) in ["SHELL", "LOGS", "LOOK"].iter().enumerate() {
        let paint = match i.cmp(&here) {
            std::cmp::Ordering::Less => Paint::Label,
            std::cmp::Ordering::Equal => Paint::NavOn,
            std::cmp::Ordering::Greater => Paint::Nav,
        };
        put(
            &mut g,
            h - 8 + i,
            3,
            &format!("[{}] {name}", i + 1),
            paint,
            false,
        );
    }
    // The choices.
    let x = left + 5;
    let right = w - 3;
    let width = right - x;
    let mut r = 2;
    let row = |g: &mut Vec<Vec<Cell>>, r: usize, sel: bool| {
        if sel {
            for c in x - 1..right + 1 {
                put(g, r, c, " ", Paint::Label, true);
            }
        }
    };
    // A label, and a tag flush right on the same row.
    let line = |g: &mut Vec<Vec<Cell>>, r: usize, text: &str, tag: (&str, Paint), sel: bool| {
        let room = width.saturating_sub(tag.0.chars().count() + 1);
        put(g, r, x, &fit(text, room), Paint::Value, sel);
        put(g, r, right - tag.0.chars().count(), tag.0, tag.1, sel);
    };
    match fr.step {
        Step::Shell => {
            put(&mut g, r, x, "SHELLS FOUND", Paint::Hint, false);
            r += 2;
            for (i, sh) in fr.shells.iter().enumerate() {
                if r + 5 >= h {
                    break;
                }
                let sel = fr.at == i;
                row(&mut g, r, sel);
                row(&mut g, r + 1, sel);
                let mark = if fr.shell == i { "(•)" } else { "( )" };
                let tag = if sh.marks { "BLOCKS BUILT IN" } else { "" };
                line(
                    &mut g,
                    r,
                    &format!("{mark} {}", sh.name),
                    (tag, Paint::Title),
                    sel,
                );
                put(
                    &mut g,
                    r + 1,
                    x + 4,
                    &fit(&sh.command, width - 4),
                    Paint::Hint,
                    sel,
                );
                r += 3;
            }
        }
        Step::Logs => {
            put(&mut g, r, x, "FIXED DRIVES", Paint::Hint, false);
            r += 2;
            let fastest = fr.fastest();
            let chosen = fr.chosen();
            for (i, d) in fr.drives.iter().enumerate() {
                if r + 8 >= h {
                    break;
                }
                let sel = fr.at == i;
                row(&mut g, r, sel);
                row(&mut g, r + 1, sel);
                let mark = if chosen == Some(i) { "(•)" } else { "( )" };
                let room = fr.has_room(d);
                let tag = match &d.probe {
                    Probe::Failed(_) => ("CAN'T WRITE", Paint::Warn),
                    _ if !room => ("TOO FULL", Paint::Warn),
                    Probe::Done(..) if fastest == Some(i) => ("FASTEST", Paint::Title),
                    Probe::Done(..) => ("", Paint::Title),
                    Probe::Measuring(_) => ("MEASURING", Paint::Nav),
                    Probe::Waiting => ("WAITING", Paint::Nav),
                };
                let head = format!(
                    "{mark} {}   {} free",
                    d.root.display(),
                    logstore::human(d.free)
                );
                line(&mut g, r, &head, tag, sel);
                let (y, x2) = (r + 1, x + 4);
                match &d.probe {
                    Probe::Done(rate, p99) => put(
                        &mut g,
                        y,
                        x2,
                        &format!("{rate:.0} MiB/s durable · p99 {p99:.1} ms"),
                        Paint::Hint,
                        sel,
                    ),
                    Probe::Measuring(n) => {
                        let bar = (width - 4).min(32);
                        let filled = bar * *n as usize / PROBE_WRITES as usize;
                        for c in 0..bar {
                            let p = if c < filled {
                                Paint::Meter
                            } else {
                                Paint::Track
                            };
                            put(&mut g, y, x2 + c, "━", p, sel);
                        }
                    }
                    Probe::Waiting => put(
                        &mut g,
                        y,
                        x2,
                        "measured after the ones above",
                        Paint::Hint,
                        sel,
                    ),
                    Probe::Failed(e) => put(&mut g, y, x2, &fit(e, width - 4), Paint::Warn, sel),
                }
                r += 3;
            }
            if fr.drives.is_empty() {
                put(&mut g, r, x, "No fixed drives found.", Paint::Hint, false);
                r += 2;
            }
            // The cap.
            let sel = fr.on_cap();
            row(&mut g, r, sel);
            let cap = if sel && fr.typing {
                format!("{}_", fr.cap_gib)
            } else {
                format!("‹ {} ›", fr.cap_gib)
            };
            put(&mut g, r, x, "Keep up to", Paint::Label, sel);
            put(
                &mut g,
                r,
                x + 12,
                &format!("{cap} GB of logs"),
                Paint::Value,
                sel,
            );
            r += 2;
            let whither = match fr.log_dir() {
                Some(d) if fr.picked.is_none() && !fr.measured() => {
                    format!("Logs go to {} (fastest so far)", d.display())
                }
                Some(d) => format!("Logs go to {}", d.display()),
                None => {
                    "Logs go to %LOCALAPPDATA%\\foldwave\\logs until a drive is measured".into()
                }
            };
            for l in wrap(&whither, width).into_iter().take(2) {
                put(&mut g, r, x, &l, Paint::Hint, false);
                r += 1;
            }
        }
        Step::Look => {
            put(&mut g, r, x, "LOOK", Paint::Hint, false);
            r += 2;
            let sel = fr.at == 0;
            row(&mut g, r, sel);
            put(&mut g, r, x, "Theme", Paint::Label, sel);
            put(
                &mut g,
                r,
                x + 20,
                &format!("‹ {} ›", fr.theme_name()),
                Paint::Value,
                sel,
            );
            r += 2;
            // The chosen theme's sixteen colours, as drawn.
            for i in 0..16u8 {
                let (rr, c) = (r + usize::from(i / 8), x + 20 + usize::from(i % 8) * 3);
                put(&mut g, rr, c, "██", Paint::Swatch(i), false);
            }
            r += 3;
            let sel = fr.at == 1;
            row(&mut g, r, sel);
            put(&mut g, r, x, "Instrument strip", Paint::Label, sel);
            let on = if fr.instruments {
                "[■] on"
            } else {
                "[ ] off"
            };
            put(&mut g, r, x + 20, on, Paint::Value, sel);
            put(
                &mut g,
                r + 1,
                x + 20,
                &fit("CPU, index and log drive under the title bar", width - 20),
                Paint::Hint,
                false,
            );
        }
    }
    // The buttons.
    let back = if fr.step == Step::Shell {
        "[ SKIP ]"
    } else {
        "[ BACK ]"
    };
    let next = if fr.step == Step::Look {
        "[ FINISH ]"
    } else {
        "[ CONTINUE ]"
    };
    let br = h - 4;
    put(
        &mut g,
        br,
        x,
        back,
        Paint::Button,
        fr.at == fr.back_button(),
    );
    put(
        &mut g,
        br,
        right - next.chars().count(),
        next,
        Paint::Button,
        fr.at == fr.next_button(),
    );
    let esc = if fr.step == Step::Shell {
        "Esc skip"
    } else {
        "Esc back"
    };
    put(
        &mut g,
        h - 2,
        3,
        &format!("↑↓ move · Space choose · ←→ change · Enter continue · {esc}"),
        Paint::Hint,
        false,
    );
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const GB: u64 = 1 << 30;

    fn shells() -> Vec<Shell> {
        let files: HashSet<PathBuf> = [
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
            "C:\\Program Files\\Git\\bin\\bash.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();
        let var = |k: &str| match k {
            "SystemRoot" => Some("C:\\Windows".to_string()),
            "ProgramFiles" => Some("C:\\Program Files".to_string()),
            "PATH" => Some("C:\\Windows\\System32;C:\\Program Files\\PowerShell\\7".to_string()),
            _ => None,
        };
        find_shells(&var, &|p: &Path| files.contains(p))
    }

    fn drives() -> Vec<Drive> {
        let d = |root: &str, free| Drive {
            root: PathBuf::from(root),
            free,
            total: 1000 * GB,
            probe: Probe::Waiting,
        };
        vec![d("C:\\", 50 * GB), d("D:\\", 400 * GB), d("E:\\", 364 * GB)]
    }

    fn first_run() -> FirstRun {
        FirstRun::new(shells(), drives(), &logstore::Settings::default())
    }

    #[test]
    fn shells_are_found_likeliest_first_and_their_command_lines_split_back() {
        let s = shells();
        let names: Vec<_> = s.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "PowerShell 7",
                "Windows PowerShell",
                "Command Prompt",
                "Git Bash"
            ]
        );
        assert_eq!(s[0].command, "pwsh.exe -NoLogo", "on PATH: by name");
        assert_eq!(
            s[3].command,
            "\"C:\\Program Files\\Git\\bin\\bash.exe\" --login -i"
        );
        assert_eq!(
            split_command(&s[3].command),
            ["C:\\Program Files\\Git\\bin\\bash.exe", "--login", "-i"]
        );
        // Not on PATH: its usual folder, in full.
        let bare = |k: &str| (k != "PATH").then(|| "C:\\Program Files".to_string());
        let files = [PathBuf::from("C:\\Program Files\\PowerShell\\7\\pwsh.exe")];
        let s2 = find_shells(&bare, &|p: &Path| files.iter().any(|f| f == p));
        assert_eq!(
            s2[0].command,
            "\"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -NoLogo"
        );
        assert!(s[2].marks && !s[0].marks, "only cmd marks its own prompts");
        assert_eq!(split_command("  cmd.exe  "), ["cmd.exe"]);
        assert_eq!(split_command("a \"\" b"), ["a", "", "b"]);
        let none = find_shells(&|_| None, &|_| false);
        assert_eq!(none[0].command, "cmd.exe", "never empty");
    }

    #[test]
    fn the_fastest_drive_with_room_is_chosen_until_one_is_picked() {
        let mut f = first_run();
        assert_eq!(f.log_dir(), None, "nothing measured: the OS's place");
        f.update(1, Probe::Measuring(64));
        f.update(0, Probe::Done(900.0, 1.0));
        // C: has 50 GB free of 1000: its reserve (20 GB) leaves 30, so a
        // 10 GB cap fits.
        assert_eq!(f.chosen(), Some(0));
        f.update(1, Probe::Done(134.0, 3.0));
        f.update(2, Probe::Done(451.0, 2.8));
        assert!(f.measured());
        assert_eq!(f.chosen(), Some(0));
        f.cap_gib = 40;
        assert!(!f.has_room(&f.drives[0].clone()));
        assert_eq!(f.chosen(), Some(2), "C: is too full for 40 GB");
        assert_eq!(f.log_dir(), Some(PathBuf::from("E:\\foldwave\\logs")));
        f.picked = Some(1);
        assert_eq!(f.chosen(), Some(1), "a pick wins");
        f.update(1, Probe::Failed("access denied".into()));
        assert_eq!(f.chosen(), Some(2), "but not one that cannot be written");
    }

    #[test]
    fn keys_walk_the_three_steps_and_finish_with_every_value() {
        let mut f = first_run();
        assert_eq!(f.at, 0, "the first shell is focused");
        f.key(Key::Down);
        f.key(Key::Down);
        assert_eq!(
            f.key(Key::Enter),
            Action::None,
            "Enter chooses cmd and goes on"
        );
        assert_eq!(f.shell, 2);
        assert_eq!(f.step, Step::Logs);
        assert_eq!(f.at, 5, "none picked: Continue is focused");
        f.key(Key::Up);
        f.key(Key::Up);
        assert_eq!(f.at, 3, "up past Back to the cap");
        f.key(Key::Char('2'));
        f.key(Key::Char('5'));
        assert_eq!(f.cap_gib, 25, "digits type the cap");
        f.key(Key::Backspace);
        assert_eq!(f.cap_gib, 2);
        f.key(Key::Right);
        assert_eq!(f.cap_gib, 3, "arrows step it");
        f.key(Key::Up);
        f.key(Key::Char(' '));
        assert_eq!(f.picked, Some(2), "Space picks the focused drive");
        f.key(Key::Esc);
        assert_eq!(f.step, Step::Shell, "Esc goes back");
        f.key(Key::Enter);
        assert_eq!((f.step, f.at), (Step::Logs, 2), "back on the picked drive");
        f.key(Key::Enter);
        assert_eq!(f.step, Step::Look);
        f.key(Key::Right);
        assert_eq!(f.theme_name(), "classic", "the theme cycles");
        f.key(Key::Down);
        f.key(Key::Char(' '));
        assert!(!f.instruments, "Space flips the strip");
        let Action::Finish(v) = f.key(Key::Enter) else {
            panic!("Enter on the last step finishes");
        };
        assert_eq!(
            v,
            [
                ("shell", Some("C:\\Windows\\System32\\cmd.exe".into())),
                ("log_dir", Some("E:\\foldwave\\logs".into())),
                ("log_cap_gib", Some("3".into())),
                ("theme", Some("classic".into())),
                ("instruments", Some("off".into())),
            ]
        );
    }

    #[test]
    fn the_buttons_skip_go_back_and_on() {
        let mut f = first_run();
        f.key(Key::Up);
        assert_eq!(f.at, f.next_button(), "up from the top wraps to Continue");
        f.key(Key::Left);
        assert_eq!(f.at, f.back_button(), "left moves to Skip");
        assert_eq!(f.key(Key::Enter), Action::Skip);
        let mut g = first_run();
        assert_eq!(g.key(Key::Esc), Action::Skip, "Esc on the first step skips");
        let mut h = first_run();
        h.at = h.next_button();
        h.key(Key::Char(' '));
        assert_eq!(h.step, Step::Logs, "Space presses Continue");
        assert_eq!(h.values()[1], ("log_dir", None), "no drive: the OS's place");
    }

    #[test]
    fn enter_keeps_the_fastest_even_when_a_faster_drive_finishes_later() {
        let mut f = first_run();
        f.update(0, Probe::Done(219.0, 12.0));
        f.key(Key::Enter);
        assert_eq!(f.at, f.next_button(), "C: is fastest so far, not focused");
        f.update(2, Probe::Done(293.0, 5.8));
        f.key(Key::Enter);
        let Action::Finish(v) = f.key(Key::Enter) else {
            panic!("finishes");
        };
        assert_eq!(v[1], ("log_dir", Some("E:\\foldwave\\logs".into())));
    }

    #[test]
    fn it_starts_from_what_the_settings_file_says() {
        let mut s = logstore::Settings::default();
        s.shell = Some("C:\\Windows\\System32\\cmd.exe".into());
        s.theme = Some("classic".into());
        s.instruments = Some(false);
        s.log_cap = Some(4 * GB);
        let f = FirstRun::new(shells(), drives(), &s);
        assert_eq!((f.shell, f.at), (2, 2));
        assert_eq!(f.theme_name(), "classic");
        assert!(!f.instruments);
        assert_eq!(f.cap_gib, 4);
    }

    fn text(g: &[Vec<Cell>], r: usize) -> String {
        g[r].iter().map(|c| c.0).collect()
    }

    fn all(g: &[Vec<Cell>]) -> String {
        (0..g.len()).map(|r| text(g, r) + "\n").collect()
    }

    #[test]
    fn each_step_draws_its_question_choices_and_buttons() {
        let mut f = first_run();
        let g = render(&f, 100, 26);
        assert_eq!(g.len(), 26);
        assert!(text(&g, 0).starts_with("┌── [ FOLDWAVE ] · FIRST RUN ─"));
        let s = all(&g);
        assert!(s.contains("Which shell opens in each"), "{s}");
        assert!(s.contains("(•) PowerShell 7"), "{s}");
        assert!(
            s.contains("( ) Command Prompt") && s.contains("BLOCKS BUILT IN"),
            "{s}"
        );
        assert!(s.contains("[ SKIP ]") && s.contains("[ CONTINUE ]"), "{s}");
        assert!(s.contains("Esc skip"));
        assert!(g[4].iter().any(|c| c.2), "the focused shell is marked");
        f.key(Key::Enter);
        f.update(0, Probe::Measuring(64));
        f.update(2, Probe::Done(451.0, 2.8));
        let g = render(&f, 100, 26);
        let s = all(&g);
        assert!(s.contains("Where should pane logs live?"), "{s}");
        assert!(s.contains("MEASURING") && s.contains("WAITING"), "{s}");
        assert!(s.contains("(•) E:\\") && s.contains("FASTEST"), "{s}");
        assert!(s.contains("451 MiB/s durable · p99 2.8 ms"), "{s}");
        assert!(
            g.iter().flatten().any(|c| c.1 == Paint::Meter)
                && g.iter().flatten().any(|c| c.1 == Paint::Track),
            "a drive being measured shows a meter"
        );
        assert!(s.contains("‹ 10 › GB of logs"), "{s}");
        assert!(
            s.contains("Logs go to E:\\foldwave\\logs (fastest so far)"),
            "{s}"
        );
        f.key(Key::Enter);
        let g = render(&f, 100, 26);
        let s = all(&g);
        assert!(s.contains("‹ mirai ›") && s.contains("[■] on"), "{s}");
        assert!(s.contains("[ BACK ]") && s.contains("[ FINISH ]"), "{s}");
        let swatches: HashSet<u8> = g
            .iter()
            .flatten()
            .filter_map(|c| match c.1 {
                Paint::Swatch(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(swatches.len(), 16, "all sixteen colours shown");
        // Small windows still draw, cut rather than broken.
        let g = render(&f, 10, 5);
        assert_eq!((g.len(), g[0].len()), (20, 60));
    }
}
