//! The instrument strip (design §11 row 17): one row under the title bar
//! reading out what fwterm is doing, `PANES 04 · FOCUS 1 · CPU 0.1 % · LOG
//! E: 364.1 GB FREE · INDEX CPU · HOSTS 16/16`; the agents atrium runs in the
//! panes, one glyph each by pane (`AGENTS 1 ●●◆  3 ✓✗`: working, waiting on a
//! decision, finished, failed), so an agent behind a zoomed pane is still
//! present; and while any wait on a decision, how many and the newest thing
//! not yet looked at (`agents.rs`). `instruments = off` in the settings file
//! hides it.

use crate::agents::Status;

/// What the strip reads out.
#[derive(Debug, Clone, PartialEq)]
pub struct Readings {
    pub panes: usize,
    /// The focused pane's number, from 1.
    pub focus: usize,
    /// fwterm's CPU (the window and its pane hosts) as a share of the
    /// machine, as Task Manager shows it; `None` before the first reading.
    pub cpu: Option<f64>,
    /// The log drive and what it has free; `None` without logs.
    pub log: Option<(String, u64)>,
    /// Where search signatures are built: `GPU`, `CPU`, or `OFF`.
    pub index: &'static str,
    /// Pane hosts running of those started; `None` without any.
    pub hosts: Option<(usize, usize)>,
    /// The agents in each pane that runs an atrium: the pane's number, from
    /// 1, and each agent's status in atrium's order. Panes without agents
    /// are left out.
    pub agents: Vec<(usize, Vec<Status>)>,
    /// Decisions agents in the panes are waiting on.
    pub decisions: usize,
    /// The newest thing not yet looked at (a decision, an agent finished or
    /// failed), as one line.
    pub ask: Option<String>,
}

/// An agent's presence: one glyph.
pub fn glyph(s: Status) -> (char, Kind) {
    match s {
        Status::Running => ('\u{25CF}', Kind::Value), // ●
        Status::Waiting => ('\u{25C6}', Kind::Bad),   // ◆
        Status::Finished => ('\u{2713}', Kind::Good), // ✓
        Status::Failed => ('\u{2717}', Kind::Bad),    // ✗
    }
}

/// How a piece of the strip is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Label,
    Value,
    Accent,
    Good,
    Bad,
}

/// The strip's pieces, left to right.
pub fn segments(r: &Readings) -> Vec<(String, Kind)> {
    let mut out: Vec<(String, Kind)> = vec![(" ".into(), Kind::Label)];
    let mut item = |label: &str, value: String, kind: Kind| {
        if out.len() > 1 {
            out.push(("  ·  ".into(), Kind::Label));
        }
        out.push((format!("{label} "), Kind::Label));
        out.push((value, kind));
    };
    item("PANES", format!("{:02}", r.panes), Kind::Value);
    item("FOCUS", r.focus.to_string(), Kind::Value);
    let cpu = r.cpu.map_or("—".into(), |c| format!("{:.1} %", c * 100.0));
    item("CPU", cpu, Kind::Value);
    if let Some((drive, free)) = &r.log {
        item(
            &format!("LOG {drive}"),
            format!("{} FREE", crate::logstore::human(*free)),
            Kind::Value,
        );
    }
    item("INDEX", r.index.to_string(), Kind::Accent);
    if let Some((up, all)) = r.hosts {
        let kind = if up == all { Kind::Good } else { Kind::Bad };
        item("HOSTS", format!("{up}/{all}"), kind);
    }
    if r.decisions > 0 {
        out.push(("  ·  ".into(), Kind::Label));
        out.push(("DECISIONS ".into(), Kind::Label));
        out.push((r.decisions.to_string(), Kind::Bad));
    }
    // What needs the human before the roster: the strip is cut from the
    // right, so with many agents the key and the message still show, and a
    // long message cuts into the roster, not the other way round. The key
    // before the message for the same reason.
    if let Some(ask) = &r.ask {
        out.push(("  ·  ".into(), Kind::Label));
        out.push(("CTRL+SHIFT+J".into(), Kind::Accent));
        out.push(("  ›  ".into(), Kind::Accent));
        out.push((ask.clone(), Kind::Value));
    }
    if !r.agents.is_empty() {
        out.push(("  ·  ".into(), Kind::Label));
        out.push(("AGENTS ".into(), Kind::Label));
        for (i, (pane, agents)) in r.agents.iter().enumerate() {
            if i > 0 {
                out.push(("  ".into(), Kind::Label));
            }
            out.push((format!("{pane} "), Kind::Label));
            for &s in agents {
                let (ch, kind) = glyph(s);
                out.push((ch.to_string(), kind));
            }
        }
    }
    out
}

/// The strip as one line of text, with each character's kind.
pub fn line(r: &Readings) -> Vec<(char, Kind)> {
    segments(r)
        .into_iter()
        .flat_map(|(s, k)| s.chars().map(move |c| (c, k)).collect::<Vec<_>>())
        .collect()
}

/// The two buttons at the strip's right end, for whoever does not know the
/// keys: the settings sheet (Ctrl+,) and the help (F1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Settings,
    Help,
}

const SETTINGS: &str = "\u{2699} SETTINGS"; // ⚙ SETTINGS
const HELP: &str = "? HELP";

/// The buttons as drawn, left to right: two spaces between them and one
/// after, at the strip's right edge.
pub fn buttons() -> Vec<(String, Kind)> {
    vec![
        (SETTINGS.into(), Kind::Accent),
        ("  ".into(), Kind::Label),
        (HELP.into(), Kind::Accent),
        (" ".into(), Kind::Label),
    ]
}

/// How many columns the buttons take, their gap and trailing space included.
pub fn buttons_width() -> usize {
    buttons().iter().map(|(s, _)| s.chars().count()).sum()
}

/// The button a click at column `col` of a `cols`-wide strip hits, if any.
pub fn button_at(cols: usize, col: usize) -> Option<Button> {
    let start = cols.checked_sub(buttons_width())?;
    let settings = SETTINGS.chars().count();
    let help = HELP.chars().count();
    let at = col.checked_sub(start)?;
    if at < settings {
        Some(Button::Settings)
    } else if at >= settings + 2 && at < settings + 2 + help {
        Some(Button::Help)
    } else {
        None
    }
}

/// The strip's row as drawn `cols` wide: the readings cut so they end at
/// least two columns before the buttons (a long ask never runs into them),
/// then the buttons against the right edge. A strip too narrow for the
/// buttons shows the readings alone.
pub fn with_buttons(line: &[(char, Kind)], cols: usize) -> Vec<(char, Kind)> {
    let blank = (' ', Kind::Label);
    let Some(start) = cols.checked_sub(buttons_width()) else {
        return (0..cols)
            .map(|c| line.get(c).copied().unwrap_or(blank))
            .collect();
    };
    let cut = start.saturating_sub(2);
    let tail: Vec<(char, Kind)> = buttons()
        .into_iter()
        .flat_map(|(s, k)| s.chars().map(move |c| (c, k)).collect::<Vec<_>>())
        .collect();
    (0..cols)
        .map(|c| {
            if c < cut {
                line.get(c).copied().unwrap_or(blank)
            } else if c >= start {
                tail[c - start]
            } else {
                blank
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(r: &Readings) -> String {
        line(r).into_iter().map(|(c, _)| c).collect()
    }

    #[test]
    fn the_strip_reads_out_panes_cpu_logs_index_and_hosts() {
        let r = Readings {
            panes: 16,
            focus: 3,
            cpu: Some(0.0012),
            log: Some(("E:".into(), 364 * (1 << 30))),
            index: "CPU",
            hosts: Some((16, 16)),
            agents: Vec::new(),
            decisions: 0,
            ask: None,
        };
        assert_eq!(
            text(&r),
            " PANES 16  ·  FOCUS 3  ·  CPU 0.1 %  ·  LOG E: 364.0 GB FREE  ·  INDEX CPU  ·  HOSTS 16/16"
        );
        let hosts = segments(&r).last().cloned().unwrap();
        assert_eq!(hosts, ("16/16".into(), Kind::Good));
        let down = Readings {
            hosts: Some((15, 16)),
            ..r.clone()
        };
        assert_eq!(
            segments(&down).last().unwrap().1,
            Kind::Bad,
            "a host down shows"
        );
    }

    #[test]
    fn what_is_not_known_or_not_running_is_left_out() {
        let r = Readings {
            panes: 1,
            focus: 1,
            cpu: None,
            log: None,
            index: "OFF",
            hosts: None,
            agents: Vec::new(),
            decisions: 0,
            ask: None,
        };
        assert_eq!(text(&r), " PANES 01  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF");
    }

    #[test]
    fn agents_show_one_glyph_each_by_pane() {
        let r = Readings {
            panes: 4,
            focus: 1,
            cpu: None,
            log: None,
            index: "OFF",
            hosts: None,
            agents: vec![
                (1, vec![Status::Running, Status::Running, Status::Waiting]),
                (3, vec![Status::Finished, Status::Failed]),
            ],
            decisions: 1,
            ask: None,
        };
        assert_eq!(
            text(&r),
            " PANES 04  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF  ·  DECISIONS 1  ·  AGENTS 1 \u{25CF}\u{25CF}\u{25C6}  3 \u{2713}\u{2717}"
        );
        // With something to show, the key and the message come before the
        // roster: the strip is cut from the right, so the roster is what a
        // long message cuts into.
        let asked = Readings {
            ask: Some("dev_1: deploy: merge?".into()),
            ..r.clone()
        };
        assert_eq!(
            text(&asked),
            " PANES 04  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF  ·  DECISIONS 1  ·  CTRL+SHIFT+J  ›  dev_1: deploy: merge?  ·  AGENTS 1 \u{25CF}\u{25CF}\u{25C6}  3 \u{2713}\u{2717}"
        );
        let kinds: Vec<Kind> = segments(&r)
            .iter()
            .filter(|(s, _)| {
                s.chars()
                    .all(|c| "\u{25CF}\u{25C6}\u{2713}\u{2717}".contains(c))
            })
            .map(|(_, k)| *k)
            .collect();
        assert_eq!(
            kinds,
            [Kind::Value, Kind::Value, Kind::Bad, Kind::Good, Kind::Bad],
            "working plain, waiting and failed red, finished green"
        );
    }

    #[test]
    fn decisions_waiting_show_their_count_and_the_newest_unseen() {
        let r = Readings {
            panes: 2,
            focus: 1,
            cpu: None,
            log: None,
            index: "OFF",
            hosts: None,
            agents: Vec::new(),
            decisions: 2,
            ask: Some("dev_1: deploy: merge?".into()),
        };
        assert_eq!(
            text(&r),
            " PANES 02  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF  ·  DECISIONS 2  ·  \
             CTRL+SHIFT+J  ›  dev_1: deploy: merge?"
        );
        assert!(segments(&r).contains(&("2".into(), Kind::Bad)));
        let seen = Readings { ask: None, ..r };
        assert!(
            text(&seen).ends_with("DECISIONS 2"),
            "the count stays until answered"
        );
    }

    #[test]
    fn a_click_on_a_button_names_it_and_the_gap_and_the_readings_miss() {
        // 40 columns: the buttons take the last 19, from column 21.
        assert_eq!(buttons_width(), 19);
        let start = 40 - 19;
        assert_eq!(button_at(40, start), Some(Button::Settings));
        assert_eq!(button_at(40, start + 9), Some(Button::Settings));
        assert_eq!(button_at(40, start + 10), None, "the gap");
        assert_eq!(button_at(40, start + 11), None, "the gap");
        assert_eq!(button_at(40, start + 12), Some(Button::Help));
        assert_eq!(button_at(40, start + 17), Some(Button::Help));
        assert_eq!(button_at(40, start + 18), None, "the trailing space");
        assert_eq!(button_at(40, 0), None, "the readings");
        assert_eq!(button_at(40, start - 1), None, "just left of them");
        assert_eq!(button_at(10, 5), None, "too narrow for the buttons");
    }

    #[test]
    fn the_buttons_sit_at_the_right_edge_and_a_long_ask_stops_short_of_them() {
        let r = Readings {
            panes: 2,
            focus: 1,
            cpu: None,
            log: None,
            index: "OFF",
            hosts: None,
            agents: Vec::new(),
            decisions: 1,
            ask: Some("dev_1: deploy: merge this very long branch name now?".into()),
        };
        let row = with_buttons(&line(&r), 60);
        let s: String = row.iter().map(|(c, _)| *c).collect();
        assert_eq!(s.chars().count(), 60);
        assert!(s.ends_with("  \u{2699} SETTINGS  ? HELP "), "{s:?}");
        assert!(s.starts_with(" PANES 02  ·  FOCUS 1"), "{s:?}");
        // The readings end two blank columns before the buttons.
        let start = 60 - buttons_width();
        assert_eq!(&s[..].chars().nth(start - 1), &Some(' '));
        assert_eq!(&s[..].chars().nth(start - 2), &Some(' '));
        assert_ne!(row[start - 3].0, ' ', "the ask fills up to the cut");
        assert_eq!(row[start].1, Kind::Accent);
        assert_eq!(row[start + 12].1, Kind::Accent);
        // Short readings leave the middle blank.
        let short = Readings {
            ask: None,
            decisions: 0,
            ..r
        };
        let s: String = with_buttons(&line(&short), 80)
            .iter()
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(
            s,
            format!(
                "{:<61}\u{2699} SETTINGS  ? HELP ",
                " PANES 02  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF"
            )
        );
        // Too narrow for them: the readings alone.
        let s: String = with_buttons(&line(&short), 10)
            .iter()
            .map(|(c, _)| *c)
            .collect();
        assert_eq!(s, " PANES 02 ");
    }
}
