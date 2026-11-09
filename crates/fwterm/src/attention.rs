//! Attention: what a pane's program said rather than drew (`vterm::Notice`),
//! kept per pane and shown by the window (design §8.7c, slice 2). The title
//! goes to the strip and, for the focused pane, the window's title bar; a
//! bell colours the pane's brackets for a moment, flashes the taskbar button
//! when the window is in the background, and asks for Ctrl+Shift+J when the
//! pane is not the focused one; an `OSC 9` toast shows in the bar for a few
//! seconds (and as a Windows notification if the settings say so); `OSC 9;4`
//! progress is a badge on the pane and the taskbar button's state. atrium
//! 0.43.0 sends all four for a session that needs its human; any program's
//! `printf '\a'` is served the same way.
//!
//! Everything here is change-gated: nothing runs unless a notice arrived or
//! one is on show, so an idle pane costs what it did.

use std::time::{Duration, Instant};
use vterm::Notice;

/// How long a toast stays in the bar.
pub const TOAST_FOR: Duration = Duration::from_secs(6);
/// How long a bell colours the pane's brackets.
pub const FLASH_FOR: Duration = Duration::from_millis(400);
/// Most characters of a title or toast kept: the strip and the bar are one
/// row.
pub const TEXT_MAX: usize = 160;

/// A program's progress report (`OSC 9;4`), as the badge and taskbar show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Normal(u8),
    Error(u8),
    Indeterminate,
    Paused(u8),
}

impl From<Progress> for fwrender::win::TaskbarProgress {
    fn from(p: Progress) -> Self {
        use fwrender::win::TaskbarProgress as T;
        match p {
            Progress::Normal(n) => T::Normal(n),
            Progress::Error(n) => T::Error(n),
            Progress::Indeterminate => T::Indeterminate,
            Progress::Paused(n) => T::Paused(n),
        }
    }
}

/// What the window must do about a notice, besides redrawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// The bell rang.
    Rang,
    /// A toast with this text (cleaned) arrived.
    Toasted(String),
}

/// One pane's attention state, as shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attention {
    /// The title the program set, cleaned.
    pub title: Option<String>,
    /// When the bell last rang.
    pub rang: Option<Instant>,
    /// Bells heard since the pane started: each is an ask of its own.
    pub bells: u64,
    /// The latest toast and when it arrived.
    pub toast: Option<(String, Instant)>,
    pub progress: Option<Progress>,
    /// What arrived since the window last acted ([`Attention::take_acts`]).
    acts: Vec<Act>,
}

impl Attention {
    /// Take a notice in; what it changes is read back through the fields
    /// and the getters below, and what the window must do about it through
    /// [`Attention::take_acts`].
    pub fn apply(&mut self, n: Notice, now: Instant) {
        match n {
            Notice::Title(t) => self.title = Some(clean(&t)),
            Notice::Bell => {
                self.rang = Some(now);
                self.bells += 1;
                self.acts.push(Act::Rang);
            }
            Notice::Toast(t) => {
                let t = clean(&t);
                self.toast = Some((t.clone(), now));
                self.acts.push(Act::Toasted(t));
            }
            Notice::Progress { state, percent } => {
                self.progress = match state {
                    1 => Some(Progress::Normal(percent)),
                    2 => Some(Progress::Error(percent)),
                    3 => Some(Progress::Indeterminate),
                    4 => Some(Progress::Paused(percent)),
                    _ => None,
                };
            }
        }
    }

    /// What arrived since the last call, oldest first.
    pub fn take_acts(&mut self) -> Vec<Act> {
        std::mem::take(&mut self.acts)
    }

    /// Whether the bell's flash is still on.
    pub fn flashing(&self, now: Instant) -> bool {
        self.rang
            .is_some_and(|t| now.saturating_duration_since(t) < FLASH_FOR)
    }

    /// The toast on show, and when it arrived.
    pub fn toast_at(&self, now: Instant) -> Option<(&str, Instant)> {
        self.toast
            .as_ref()
            .filter(|(_, at)| now.saturating_duration_since(*at) < TOAST_FOR)
            .map(|(s, at)| (s.as_str(), *at))
    }

    /// When what is on show next changes by itself: the flash or the toast
    /// ending. `None` while nothing timed is on show.
    pub fn next_change(&self, now: Instant) -> Option<Instant> {
        let flash = self.rang.map(|t| t + FLASH_FOR).filter(|&t| t > now);
        let toast = self
            .toast
            .as_ref()
            .map(|(_, t)| *t + TOAST_FOR)
            .filter(|&t| t > now);
        match (flash, toast) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The progress badge drawn over the pane's top right corner, with a
    /// space either side; `None` without progress. An error or a pause at
    /// 0 % shows its sign alone: atrium marks "needs you" as `OSC 9;4;2;0`.
    /// Plain ASCII: a glyph outside the font comes through the fallback
    /// wider than a cell.
    pub fn badge(&self) -> Option<String> {
        Some(match self.progress? {
            Progress::Normal(p) => format!(" {p}% "),
            Progress::Error(0) => " ! ".to_string(),
            Progress::Error(p) => format!(" ! {p}% "),
            Progress::Indeterminate => " ... ".to_string(),
            Progress::Paused(0) => " || ".to_string(),
            Progress::Paused(p) => format!(" || {p}% "),
        })
    }
}

/// A title or toast as shown: control characters become spaces, the ends are
/// trimmed, and a long one is cut at [`TEXT_MAX`] characters.
pub fn clean(text: &str) -> String {
    let s: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let s = s.trim();
    match s.char_indices().nth(TEXT_MAX) {
        Some((i, _)) => s[..i].trim_end().to_string(),
        None => s.to_string(),
    }
}

/// The taskbar button's progress for the whole window, from each pane's
/// (`focused`, progress): an error anywhere shows as the error, else
/// indeterminate anywhere, else paused, else normal with the focused pane's
/// percent if it reports one, else the lowest percent reported (the window
/// is as far along as its slowest pane). `None` when no pane reports.
pub fn taskbar(panes: &[(bool, Option<Progress>)]) -> Option<Progress> {
    let all = || panes.iter().filter_map(|(_, p)| *p);
    if let Some(e) = all().find(|p| matches!(p, Progress::Error(_))) {
        return Some(e);
    }
    if all().any(|p| p == Progress::Indeterminate) {
        return Some(Progress::Indeterminate);
    }
    if let Some(p) = all().find(|p| matches!(p, Progress::Paused(_))) {
        return Some(p);
    }
    let focused = panes
        .iter()
        .find(|(f, p)| *f && matches!(p, Some(Progress::Normal(_))))
        .and_then(|(_, p)| *p);
    focused.or_else(|| {
        all()
            .filter_map(|p| match p {
                Progress::Normal(n) => Some(n),
                _ => None,
            })
            .min()
            .map(Progress::Normal)
    })
}

/// A bell's ask, as the strip and Ctrl+Shift+J read it: the pane's number
/// (from 1) and its title if it has one.
pub fn bell_text(pane: usize, title: Option<&str>) -> String {
    match title.filter(|t| !t.is_empty()) {
        Some(t) => format!("pane {pane} rang: {t}"),
        None => format!("pane {pane} rang"),
    }
}

/// The toast bar's text and how many of its leading characters are the
/// bar's name (drawn in the accent).
pub fn toast_line(pane: usize, text: &str) -> (String, usize) {
    let name = format!(" PANE {pane} ");
    let lead = name.chars().count();
    (format!("{name}› {text}"), lead)
}

/// What `notify = bar|windows|off` means: (a toast shows in the bar, a toast
/// also raises a Windows notification). Unset or unknown is `bar`.
pub fn notify_mode(setting: Option<&str>) -> (bool, bool) {
    match setting {
        Some("windows") => (true, true),
        Some("off") => (false, false),
        _ => (true, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        // A fixed origin keeps the arithmetic exact.
        thread_local!(static T0: Instant = Instant::now());
        T0.with(|t| *t + Duration::from_millis(ms))
    }

    #[test]
    fn a_title_is_kept_cleaned_and_is_no_act() {
        let mut a = Attention::default();
        a.apply(Notice::Title("  atrium \u{b7} dev\x07 \t".into()), at(0));
        assert_eq!(a.title.as_deref(), Some("atrium \u{b7} dev"));
        assert!(a.take_acts().is_empty());
        assert_eq!(a.next_change(at(0)), None);
    }

    #[test]
    fn a_bell_flashes_counts_and_acts() {
        let mut a = Attention::default();
        a.apply(Notice::Bell, at(100));
        assert_eq!(a.take_acts(), [Act::Rang]);
        assert_eq!(a.bells, 1);
        assert!(a.flashing(at(100)));
        assert!(a.flashing(at(499)));
        assert!(!a.flashing(at(500)));
        assert_eq!(a.next_change(at(100)), Some(at(500)));
        assert_eq!(
            a.next_change(at(500)),
            None,
            "over: nothing timed is on show"
        );
        a.apply(Notice::Bell, at(600));
        assert_eq!(a.bells, 2, "each bell is its own ask");
        assert_eq!(a.take_acts(), [Act::Rang]);
    }

    #[test]
    fn a_toast_shows_for_six_seconds_and_the_newest_wins() {
        let mut a = Attention::default();
        a.apply(Notice::Toast("build done\n".into()), at(0));
        assert_eq!(a.take_acts(), [Act::Toasted("build done".into())]);
        assert_eq!(a.toast_at(at(5_999)), Some(("build done", at(0))));
        assert_eq!(a.toast_at(at(6_000)), None);
        assert_eq!(a.next_change(at(1_000)), Some(at(6_000)));
        a.apply(Notice::Toast("tests done".into()), at(2_000));
        assert_eq!(a.toast_at(at(3_000)), Some(("tests done", at(2_000))));
        assert_eq!(a.next_change(at(3_000)), Some(at(8_000)));
    }

    #[test]
    fn the_next_change_is_the_earlier_of_flash_and_toast() {
        let mut a = Attention::default();
        a.apply(Notice::Toast("t".into()), at(0));
        a.apply(Notice::Bell, at(1_000));
        assert_eq!(a.next_change(at(1_000)), Some(at(1_400)));
        assert_eq!(a.next_change(at(1_400)), Some(at(6_000)));
    }

    #[test]
    fn progress_states_make_badges_and_zero_clears() {
        let mut a = Attention::default();
        let badge = |a: &mut Attention, state, percent| {
            a.apply(Notice::Progress { state, percent }, at(0));
            a.badge()
        };
        assert_eq!(badge(&mut a, 1, 40).as_deref(), Some(" 40% "));
        assert_eq!(a.progress, Some(Progress::Normal(40)));
        assert_eq!(badge(&mut a, 2, 7).as_deref(), Some(" ! 7% "));
        assert_eq!(
            badge(&mut a, 2, 0).as_deref(),
            Some(" ! "),
            "atrium's needs-you"
        );
        assert_eq!(badge(&mut a, 3, 0).as_deref(), Some(" ... "));
        assert_eq!(badge(&mut a, 4, 50).as_deref(), Some(" || 50% "));
        assert_eq!(badge(&mut a, 4, 0).as_deref(), Some(" || "));
        assert_eq!(badge(&mut a, 1, 0).as_deref(), Some(" 0% "));
        assert_eq!(badge(&mut a, 0, 0), None);
        assert_eq!(a.progress, None);
        assert!(
            a.take_acts().is_empty(),
            "progress needs no act: the badge is redrawn"
        );
    }

    #[test]
    fn clean_cuts_long_text_on_a_character_boundary() {
        let long: String = "\u{e9}".repeat(TEXT_MAX + 20);
        let cut = clean(&long);
        assert_eq!(cut.chars().count(), TEXT_MAX);
        assert_eq!(clean("a\x1b[31mb"), "a [31mb");
        assert_eq!(clean("   "), "");
    }

    #[test]
    fn the_taskbar_shows_the_worst_then_the_focused_then_the_slowest() {
        use Progress::*;
        assert_eq!(taskbar(&[]), None);
        assert_eq!(taskbar(&[(true, None), (false, None)]), None);
        assert_eq!(
            taskbar(&[(true, Some(Normal(90))), (false, Some(Error(3)))]),
            Some(Error(3))
        );
        assert_eq!(
            taskbar(&[(true, Some(Normal(90))), (false, Some(Indeterminate))]),
            Some(Indeterminate)
        );
        assert_eq!(
            taskbar(&[(true, Some(Normal(90))), (false, Some(Paused(10)))]),
            Some(Paused(10))
        );
        assert_eq!(
            taskbar(&[(true, Some(Normal(90))), (false, Some(Normal(10)))]),
            Some(Normal(90)),
            "the focused pane's percent"
        );
        assert_eq!(
            taskbar(&[
                (true, None),
                (false, Some(Normal(70))),
                (false, Some(Normal(10)))
            ]),
            Some(Normal(10)),
            "else the slowest"
        );
    }

    #[test]
    fn asks_bars_and_the_setting_read_as_designed() {
        assert_eq!(bell_text(2, None), "pane 2 rang");
        assert_eq!(bell_text(2, Some("")), "pane 2 rang");
        assert_eq!(
            bell_text(3, Some("atrium \u{b7} dev")),
            "pane 3 rang: atrium \u{b7} dev"
        );
        let (line, lead) = toast_line(12, "build done");
        assert_eq!(line, " PANE 12 › build done");
        assert_eq!(&line.chars().take(lead).collect::<String>(), " PANE 12 ");
        assert_eq!(notify_mode(None), (true, false));
        assert_eq!(notify_mode(Some("bar")), (true, false));
        assert_eq!(notify_mode(Some("windows")), (true, true));
        assert_eq!(notify_mode(Some("off")), (false, false));
        assert_eq!(notify_mode(Some("loud")), (true, false));
    }
}
