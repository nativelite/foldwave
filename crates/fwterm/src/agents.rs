//! Agent events in foldwave (design §8.7, first slice): an agent run by atrium
//! in one of fwterm's panes that needs a decision shows in the instrument
//! strip, wherever that pane is, until it is answered.
//!
//! atrium already keeps each running session's bus beside its session
//! snapshot, `<state>\<project>\<pid>-<started ms>.bus.json`, rewritten at
//! most every 5 s, and the snapshot itself every 15 s as a heartbeat. So
//! nothing in atrium changes and no channel is opened: a thread here finds the
//! sessions with a fresh heartbeat, finds which pane each atrium runs under
//! from the process tree, and reads the open decisions with atrium's own bus
//! code (`abus`). A decision leaves when atrium marks it answered.

use crate::layout::PaneId;
use resmon::RawProc;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// atrium's `STALE_AFTER_MS`: a snapshot not rewritten for this long belongs
/// to an atrium that is no longer running.
pub const STALE: Duration = Duration::from_secs(60);

/// How often the watcher looks while an atrium is running.
pub const EVERY: Duration = Duration::from_secs(2);

/// How often it looks while none is: an atrium just started is found within
/// this, and an idle window pays for a folder listing only this often
/// (measured: every 2 s cost 1-1.5 ms of CPU a second, idle).
pub const EVERY_IDLE: Duration = Duration::from_secs(10);

/// The folder atrium keeps its sessions in: `ATRIUM_STATE_DIR`, else
/// `%LOCALAPPDATA%\atrium\sessions` (atrium's `session_store::root_from`).
pub fn sessions_root(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |k: &str| var(k).filter(|v| !v.is_empty());
    if let Some(dir) = set("ATRIUM_STATE_DIR") {
        return Some(PathBuf::from(dir));
    }
    set("LOCALAPPDATA").map(|d| PathBuf::from(d).join("atrium").join("sessions"))
}

/// A running atrium: its process and the bus file beside its snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub pid: u32,
    pub started_ms: u64,
    pub bus: PathBuf,
}

/// The sessions under `root` whose snapshot was rewritten within [`STALE`]
/// of `now` and that have a bus file; for a pid listed twice (a reused pid),
/// the one started last.
pub fn live_sessions(root: &Path, now: SystemTime) -> Vec<Session> {
    let mut by_pid: HashMap<u32, Session> = HashMap::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let dirs = projects
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()));
    for project in dirs {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".json") else {
                continue;
            };
            if stem.ends_with(".bus") || stem.ends_with(".board") {
                continue;
            }
            let Some((pid, started)) = stem.split_once('-') else {
                continue;
            };
            let (Ok(pid), Ok(started_ms)) = (pid.parse::<u32>(), started.parse::<u64>()) else {
                continue;
            };
            let fresh = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok().or(Some(Duration::ZERO)))
                .is_some_and(|age| age <= STALE);
            let bus = project.path().join(format!("{stem}.bus.json"));
            if !fresh || !bus.is_file() {
                continue;
            }
            let s = Session {
                pid,
                started_ms,
                bus,
            };
            match by_pid.get(&pid) {
                Some(old) if old.started_ms >= started_ms => {}
                _ => {
                    by_pid.insert(pid, s);
                }
            }
        }
    }
    let mut out: Vec<Session> = by_pid.into_values().collect();
    out.sort_by_key(|s| (s.pid, s.started_ms));
    out
}

/// The pane `pid` runs under: its own or an ancestor's pid is a pane's root
/// process (the pane host, or the pane's program). `None` when it runs under
/// no pane of this window.
pub fn pane_of(pid: u32, procs: &[RawProc], roots: &[(PaneId, u32)]) -> Option<PaneId> {
    let parent: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.parent_pid)).collect();
    let mut at = pid;
    // A parent pid may name a process that has exited and been replaced,
    // so the walk is bounded rather than trusted to end.
    for _ in 0..64 {
        if let Some(&(pane, _)) = roots.iter().find(|&&(_, root)| root == at) {
            return Some(pane);
        }
        match parent.get(&at) {
            Some(&p) if p != 0 && p != at => at = p,
            _ => return None,
        }
    }
    None
}

/// One line for an event: its topic, then `msg` (or `message`) if it has
/// one, then its other fields as `key=value`.
pub fn headline(topic: &str, fields: &BTreeMap<String, String>) -> String {
    let mut parts = Vec::new();
    let msg = ["msg", "message"].iter().find_map(|k| fields.get(*k));
    if let Some(m) = msg {
        parts.push(m.clone());
    }
    parts.extend(
        fields
            .iter()
            .filter(|(k, _)| !["msg", "message"].contains(&k.as_str()))
            .map(|(k, v)| format!("{k}={v}")),
    );
    let body = parts.join(" · ");
    let line = if body.is_empty() {
        topic.to_string()
    } else {
        format!("{topic}: {body}")
    };
    // One line, whatever the fields held.
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A decision an agent is waiting on, as fwterm shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub pane: PaneId,
    /// Unique across sessions: the atrium's pid and start, and the event's
    /// sequence number on its bus.
    pub key: (u32, u64, u64),
    /// Who raised it (the agent's role or id), if the bus says.
    pub from: Option<String>,
    pub text: String,
    pub ts_ms: u64,
}

/// The open decisions in a bus file, oldest first: `(seq, from, headline,
/// ts_ms)`. A missing or unreadable file has none.
pub fn open_decisions(bus: &Path) -> Vec<(u64, Option<String>, String, u64)> {
    abus::bus::Bus::with_file(bus.to_path_buf())
        .pending_decisions()
        .into_iter()
        .map(|e| {
            (
                e.seq,
                e.from.clone(),
                headline(&e.topic, &e.fields),
                e.ts_ms,
            )
        })
        .collect()
}

/// The decision Ctrl+Shift+J shows: the newest not yet looked at; else,
/// all looked at, the first waiting in another pane than `focus`; else the
/// first.
pub fn next_to_show<'a>(
    decisions: &'a [Decision],
    looked_at: &std::collections::HashSet<(u32, u64, u64)>,
    focus: PaneId,
) -> Option<&'a Decision> {
    decisions
        .iter()
        .rev()
        .find(|d| !looked_at.contains(&d.key))
        .or_else(|| decisions.iter().find(|d| d.pane != focus))
        .or(decisions.first())
}

/// Watch for decisions, every [`EVERY`], on a thread of its own. `roots` is
/// every pane's root process, kept current by the window; each change in
/// the open decisions is sent, then `wake` is called. The thread ends when
/// the receiver is dropped.
pub fn watch(
    roots: Arc<Mutex<Vec<(PaneId, u32)>>>,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Vec<Decision>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fwterm agents".into())
        .spawn(move || {
            let mut last: Vec<Decision> = Vec::new();
            // Each bus file's last read, by its modification time.
            #[allow(clippy::type_complexity)]
            let mut cache: HashMap<
                PathBuf,
                (SystemTime, Vec<(u64, Option<String>, String, u64)>),
            > = HashMap::new();
            // Which pane each session's atrium runs under (`None`: none of
            // this window's). A running process keeps its place, so the
            // process list (~5 ms) is read again only for a new session or
            // when the panes change.
            let mut placed: HashMap<(u32, u64), Option<PaneId>> = HashMap::new();
            let mut placed_for: Vec<(PaneId, u32)> = Vec::new();
            loop {
                let mut any_live = false;
                let found = match sessions_root(|k| std::env::var_os(k)) {
                    Some(root) => {
                        let live = live_sessions(&root, SystemTime::now());
                        any_live = !live.is_empty();
                        if live.is_empty() {
                            Vec::new()
                        } else {
                            let roots = roots.lock().map(|r| r.clone()).unwrap_or_default();
                            if roots != placed_for {
                                placed.clear();
                                placed_for = roots.clone();
                            }
                            if live
                                .iter()
                                .any(|s| !placed.contains_key(&(s.pid, s.started_ms)))
                            {
                                let procs = resmon::sys::process_links().unwrap_or_default();
                                for s in &live {
                                    placed
                                        .entry((s.pid, s.started_ms))
                                        .or_insert_with(|| pane_of(s.pid, &procs, &roots));
                                }
                            }
                            placed.retain(|k, _| live.iter().any(|s| (s.pid, s.started_ms) == *k));
                            let mut found = Vec::new();
                            for s in &live {
                                let Some(&Some(pane)) = placed.get(&(s.pid, s.started_ms)) else {
                                    continue;
                                };
                                let modified = std::fs::metadata(&s.bus)
                                    .and_then(|m| m.modified())
                                    .unwrap_or(SystemTime::UNIX_EPOCH);
                                let open = match cache.get(&s.bus) {
                                    Some((t, open)) if *t == modified => open.clone(),
                                    _ => {
                                        let open = open_decisions(&s.bus);
                                        cache.insert(s.bus.clone(), (modified, open.clone()));
                                        open
                                    }
                                };
                                found.extend(open.into_iter().map(|(seq, from, text, ts_ms)| {
                                    Decision {
                                        pane,
                                        key: (s.pid, s.started_ms, seq),
                                        from,
                                        text,
                                        ts_ms,
                                    }
                                }));
                            }
                            cache.retain(|p, _| live.iter().any(|s| &s.bus == p));
                            found.sort_by_key(|d| (d.ts_ms, d.key));
                            found
                        }
                    }
                    None => Vec::new(),
                };
                if found != last {
                    if tx.send(found.clone()).is_err() {
                        return;
                    }
                    wake();
                    last = found;
                }
                std::thread::sleep(if any_live { EVERY } else { EVERY_IDLE });
            }
        })
        .expect("start the agents thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fwterm-agents-{name}-{}-{}",
            std::process::id(),
            crate::logstore::now_ms()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn proc(pid: u32, parent_pid: u32, name: &str) -> RawProc {
        RawProc {
            pid,
            parent_pid,
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_sessions_root_follows_atrium() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| OsString::from(*v))
            }
        };
        assert_eq!(
            sessions_root(env(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")])),
            Some(PathBuf::from(r"C:\Users\u\AppData\Local\atrium\sessions"))
        );
        assert_eq!(
            sessions_root(env(&[
                ("ATRIUM_STATE_DIR", r"D:\s"),
                ("LOCALAPPDATA", r"C:\x")
            ])),
            Some(PathBuf::from(r"D:\s"))
        );
        assert_eq!(
            sessions_root(env(&[("ATRIUM_STATE_DIR", ""), ("LOCALAPPDATA", r"C:\x")])),
            Some(PathBuf::from(r"C:\x\atrium\sessions")),
            "an empty override is ignored, as atrium does"
        );
    }

    #[test]
    fn live_sessions_need_a_fresh_snapshot_and_a_bus() {
        let root = scratch("live");
        let p = root.join("proj-1a2b");
        std::fs::create_dir_all(&p).unwrap();
        for f in [
            "100-5.json",
            "100-5.bus.json",
            "100-5.board.json",
            "200-7.json", // no bus beside it
            "notes.json",
        ] {
            std::fs::write(p.join(f), "{}").unwrap();
        }
        // Pid 100 again, started earlier: the later start wins.
        std::fs::write(p.join("100-3.json"), "{}").unwrap();
        std::fs::write(p.join("100-3.bus.json"), "{}").unwrap();
        let now = SystemTime::now();
        let live = live_sessions(&root, now);
        assert_eq!(
            live,
            [Session {
                pid: 100,
                started_ms: 5,
                bus: p.join("100-5.bus.json")
            }]
        );
        // A minute and more later, nothing was rewritten: none are live.
        assert!(live_sessions(&root, now + STALE + Duration::from_secs(5)).is_empty());
        assert!(live_sessions(&root.join("missing"), now).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_process_belongs_to_the_pane_it_runs_under() {
        // fwterm (1) runs pane hosts 10 and 20; 10 runs pwsh (11), which runs
        // atrium (12); 20 runs cmd (21). Another atrium (30) runs elsewhere.
        let procs = [
            proc(1, 0, "fwterm.exe"),
            proc(10, 1, "fwterm.exe"),
            proc(11, 10, "pwsh.exe"),
            proc(12, 11, "atrium.exe"),
            proc(20, 1, "fwterm.exe"),
            proc(21, 20, "cmd.exe"),
            proc(30, 99, "atrium.exe"),
        ];
        let roots = [(0, 10), (1, 20)];
        assert_eq!(pane_of(12, &procs, &roots), Some(0));
        assert_eq!(pane_of(21, &procs, &roots), Some(1));
        assert_eq!(pane_of(10, &procs, &roots), Some(0), "the root itself");
        assert_eq!(pane_of(30, &procs, &roots), None);
        // A cycle of stale parent pids ends the walk.
        let looped = [proc(5, 6, "a"), proc(6, 5, "b")];
        assert_eq!(pane_of(5, &looped, &roots), None);
    }

    #[test]
    fn a_headline_puts_the_message_first_on_one_line() {
        let f = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            headline("deploy", &f(&[("item", "X"), ("msg", "merge to main?")])),
            "deploy: merge to main? · item=X"
        );
        assert_eq!(headline("review", &f(&[])), "review");
        assert_eq!(
            headline("q", &f(&[("message", "two\nlines")])),
            "q: two lines"
        );
    }

    #[test]
    fn ctrl_shift_j_shows_the_newest_unseen_then_cycles_through_the_rest() {
        let d = |pane, seq, ts_ms| Decision {
            pane,
            key: (1, 1, seq),
            from: None,
            text: String::new(),
            ts_ms,
        };
        let all = [d(0, 1, 10), d(1, 2, 20), d(2, 3, 30)];
        let mut seen = std::collections::HashSet::new();
        let pick = |seen: &std::collections::HashSet<_>, focus| {
            next_to_show(&all, seen, focus).map(|d| d.key.2)
        };
        assert_eq!(pick(&seen, 0), Some(3), "the newest unseen first");
        seen.insert((1, 1, 3));
        assert_eq!(pick(&seen, 2), Some(2));
        seen.insert((1, 1, 2));
        seen.insert((1, 1, 1));
        assert_eq!(pick(&seen, 0), Some(2), "all seen: one in another pane");
        assert_eq!(pick(&seen, 1), Some(1));
        assert_eq!(next_to_show(&[], &seen, 0), None);
        let one = [d(4, 9, 1)];
        assert_eq!(
            next_to_show(&one, &[(1, 1, 9)].into_iter().collect(), 4).map(|d| d.pane),
            Some(4),
            "the only one, even in the focused pane"
        );
    }

    #[test]
    fn open_decisions_come_from_atriums_own_bus_code() {
        let root = scratch("bus");
        let path = root.join("1-2.bus.json");
        let mut bus = abus::bus::Bus::with_file(path.clone());
        let fields = |msg: &str| vec![("msg".to_string(), msg.to_string())];
        use abus::bus::Kind;
        bus.publish(
            "deploy",
            Kind::DecisionNeeded,
            Some("dev_1"),
            &fields("merge to main?"),
            1_000,
        )
        .unwrap();
        bus.publish("status", Kind::Fyi, Some("dev_2"), &fields("built"), 1_001)
            .unwrap();
        let answered = bus
            .publish(
                "review",
                Kind::DecisionNeeded,
                Some("dev_2"),
                &fields("ship it?"),
                1_002,
            )
            .unwrap();
        assert!(bus.resolve(answered.seq));
        drop(bus);
        let open = open_decisions(&path);
        assert_eq!(
            open.len(),
            1,
            "fyi and answered ones are not open: {open:?}"
        );
        assert_eq!(open[0].1.as_deref(), Some("dev_1"));
        assert_eq!(open[0].2, "deploy: merge to main?");
        assert!(open_decisions(&root.join("none.bus.json")).is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[cfg(test)]
mod timing {
    /// What one look at atrium's real session folder costs. A measurement,
    /// run by hand: `cargo test --release --bin fwterm -- --ignored --nocapture agents_scan_cost`
    #[test]
    #[ignore = "timing on this machine's own atrium folder"]
    fn agents_scan_cost() {
        let root = super::sessions_root(|k| std::env::var_os(k)).expect("root");
        let n = 200;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(super::live_sessions(&root, std::time::SystemTime::now()));
        }
        let per = t.elapsed() / n;
        let t = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(resmon::sys::process_links().unwrap());
        }
        println!(
            "live_sessions: {per:?} per look; process_links: {:?} per snapshot",
            t.elapsed() / n
        );
    }
}
