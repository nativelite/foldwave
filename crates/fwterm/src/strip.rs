//! The instrument strip (design §11 row 17): one row under the title bar
//! reading out what fwterm is doing, `PANES 04 · FOCUS 1 · CPU 0.1 % · LOG
//! E: 364.1 GB FREE · INDEX CPU · HOSTS 16/16`, and while agents in the panes
//! wait on a decision (atrium's bus, `agents.rs`), how many and the newest one
//! not yet looked at. `instruments = off` in the settings file hides it.

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
    /// Decisions agents in the panes are waiting on.
    pub decisions: usize,
    /// The newest of them not yet looked at, as one line.
    pub ask: Option<String>,
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
        item("DECISIONS", r.decisions.to_string(), Kind::Bad);
    }
    // The key before the message, so a long message is what gets cut.
    if let Some(ask) = &r.ask {
        out.push(("  ·  ".into(), Kind::Label));
        out.push(("CTRL+SHIFT+J".into(), Kind::Accent));
        out.push(("  ›  ".into(), Kind::Accent));
        out.push((ask.clone(), Kind::Value));
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
            decisions: 0,
            ask: None,
        };
        assert_eq!(text(&r), " PANES 01  ·  FOCUS 1  ·  CPU —  ·  INDEX OFF");
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
}
