//! Agent events in foldwave (design §8.7): the agents atrium runs in fwterm's
//! panes, each with a status, and what needs the human: an agent waiting on
//! a decision, one that finished, one that failed. They show in the
//! instrument strip wherever their pane is, hidden or not.
//!
//! atrium already keeps each running session's bus beside its session
//! snapshot, `<state>\<project>\<pid>-<started ms>.bus.json`, rewritten at
//! most every 5 s, and the snapshot itself every 15 s as a heartbeat. So
//! nothing in atrium changes and no channel is opened: a thread here finds the
//! sessions with a fresh heartbeat, finds which pane each atrium runs under
//! from the process tree, reads the agents' roles from the snapshot
//! (`panes[].role`) and the open decisions and each agent's latest
//! `status=` with atrium's own bus code (`abus`). A decision leaves when
//! atrium marks it answered; `status=done` and `status=failed` are what
//! atrium's fleet rules have agents post, so they are read as finished and
//! failed.

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

/// A running atrium: its process, its snapshot and the bus file beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub pid: u32,
    pub started_ms: u64,
    pub snapshot: PathBuf,
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
                snapshot: f.path(),
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

/// What an ask is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AskKind {
    /// An agent waits on a decision (open until atrium marks it answered).
    Decision,
    /// An agent said it finished (`status=done`).
    Finished,
    /// An agent said it failed (`status=failed`, `blocked`, `error`).
    Failed,
}

/// Something that needs the human, as fwterm shows it: a decision an agent
/// is waiting on, or an agent that finished or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub pane: PaneId,
    /// Unique across sessions: the atrium's pid and start, and the event's
    /// sequence number on its bus.
    pub key: (u32, u64, u64),
    pub kind: AskKind,
    /// Who raised it (the agent's role or id), if the bus says.
    pub from: Option<String>,
    pub text: String,
    pub ts_ms: u64,
}

impl Ask {
    /// One line: who, what became of them, and the event's text.
    pub fn label(&self) -> String {
        let who = self.from.as_deref().unwrap_or("an agent");
        match self.kind {
            AskKind::Decision => format!("{who}: {}", self.text),
            AskKind::Finished => format!("{who} finished: {}", self.text),
            AskKind::Failed => format!("{who} failed: {}", self.text),
        }
    }
}

/// Where an agent stands, from the last it said on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Nothing said, or a status word that is not an end: working.
    Running,
    /// Waiting on a decision.
    Waiting,
    Finished,
    Failed,
}

/// The status a `status=` word means. atrium's fleet rules have agents post
/// `status=done`; its board uses `DONE`; `blocked` is what a stuck agent
/// posts. Anything else is taken as still working.
pub fn status_of(word: &str) -> Status {
    match word.trim().to_ascii_lowercase().as_str() {
        "done" | "finished" | "complete" | "completed" | "ok" | "success" | "succeeded" => {
            Status::Finished
        }
        "failed" | "fail" | "failure" | "error" | "blocked" | "crashed" | "aborted" => {
            Status::Failed
        }
        _ => Status::Running,
    }
}

/// An agent in one of the panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub pane: PaneId,
    pub role: String,
    pub status: Status,
}

/// What the watcher reports: everything that needs the human, oldest first,
/// and every agent, by pane then in the order atrium lists them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub asks: Vec<Ask>,
    pub agents: Vec<Agent>,
}

/// One bus file, read: `(seq, from, headline, ts_ms)` of each open decision,
/// oldest first, and the latest `status=` event of each agent that posted
/// one, in the order the agents first did: `(role, seq, status word,
/// headline without the status, ts_ms)`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BusRead {
    pub decisions: Vec<(u64, Option<String>, String, u64)>,
    pub statuses: Vec<(String, u64, String, String, u64)>,
}

/// Read a bus file. A missing or unreadable file has nothing in it.
pub fn read_bus(bus: &Path) -> BusRead {
    let b = abus::bus::Bus::with_file(bus.to_path_buf());
    let decisions = b
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
        .collect();
    let mut statuses: Vec<(String, u64, String, String, u64)> = Vec::new();
    for e in b.tail(usize::MAX) {
        let (Some(from), Some(status)) = (&e.from, e.fields.get("status")) else {
            continue;
        };
        let rest: BTreeMap<String, String> = e
            .fields
            .iter()
            .filter(|(k, _)| k.as_str() != "status")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let entry = (
            from.clone(),
            e.seq,
            status.clone(),
            headline(&e.topic, &rest),
            e.ts_ms,
        );
        match statuses.iter_mut().find(|s| &s.0 == from) {
            Some(slot) => *slot = entry,
            None => statuses.push(entry),
        }
    }
    BusRead {
        decisions,
        statuses,
    }
}

/// The agents' roles in a session snapshot (`panes[].role`), in atrium's
/// pane order; panes without a role are left out. Nothing from a file that
/// is missing or not a snapshot.
pub fn roles(snapshot: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(snapshot) else {
        return Vec::new();
    };
    let Ok(v) = json::parse(&text) else {
        return Vec::new();
    };
    v.get("panes")
        .and_then(|p| p.as_array())
        .map(|panes| {
            panes
                .iter()
                .filter_map(|p| p.get("role").and_then(|r| r.as_str()))
                .filter(|r| !r.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The agents of one session, under `pane`: those atrium lists, in its
/// order, then any that only the bus names. An open decision from an agent
/// makes it waiting, whatever it said last; else its last status word says.
pub fn agents_of(pane: PaneId, roles: &[String], read: &BusRead) -> Vec<Agent> {
    let mut names: Vec<&str> = roles.iter().map(String::as_str).collect();
    for (role, ..) in &read.statuses {
        if !names.contains(&role.as_str()) {
            names.push(role);
        }
    }
    names
        .into_iter()
        .map(|role| {
            let waiting = read
                .decisions
                .iter()
                .any(|(_, from, _, _)| from.as_deref() == Some(role));
            let status = if waiting {
                Status::Waiting
            } else {
                read.statuses
                    .iter()
                    .find(|s| s.0 == role)
                    .map_or(Status::Running, |s| status_of(&s.2))
            };
            Agent {
                pane,
                role: role.to_string(),
                status,
            }
        })
        .collect()
}

/// What needs the human in one session, under `pane`: its open decisions,
/// and each agent whose last word was an end (finished or failed).
pub fn asks_of(pane: PaneId, session: (u32, u64), read: &BusRead) -> Vec<Ask> {
    let mut out: Vec<Ask> = read
        .decisions
        .iter()
        .map(|(seq, from, text, ts_ms)| Ask {
            pane,
            key: (session.0, session.1, *seq),
            kind: AskKind::Decision,
            from: from.clone(),
            text: text.clone(),
            ts_ms: *ts_ms,
        })
        .collect();
    for (role, seq, word, text, ts_ms) in &read.statuses {
        let kind = match status_of(word) {
            Status::Finished => AskKind::Finished,
            Status::Failed => AskKind::Failed,
            _ => continue,
        };
        out.push(Ask {
            pane,
            key: (session.0, session.1, *seq),
            kind,
            from: Some(role.clone()),
            text: text.clone(),
            ts_ms: *ts_ms,
        });
    }
    out
}

/// The ask Ctrl+Shift+J shows: the newest not yet looked at; else, all
/// looked at, the first waiting in another pane than `focus`; else the
/// first.
pub fn next_to_show<'a>(
    asks: &'a [Ask],
    looked_at: &std::collections::HashSet<(u32, u64, u64)>,
    focus: PaneId,
) -> Option<&'a Ask> {
    asks.iter()
        .rev()
        .find(|d| !looked_at.contains(&d.key))
        .or_else(|| asks.iter().find(|d| d.pane != focus))
        .or(asks.first())
}

/// Watch the agents, every [`EVERY`], on a thread of its own. `roots` is
/// every pane's root process, kept current by the window; each change in
/// what is reported is sent, then `wake` is called. The thread ends when
/// the receiver is dropped.
pub fn watch(
    roots: Arc<Mutex<Vec<(PaneId, u32)>>>,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("fwterm agents".into())
        .spawn(move || {
            let mut last = Report::default();
            // Each bus and snapshot file's last read, by modification time.
            let mut cache: HashMap<PathBuf, (SystemTime, BusRead)> = HashMap::new();
            let mut role_cache: HashMap<PathBuf, (SystemTime, Vec<String>)> = HashMap::new();
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
                            Report::default()
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
                            let mut found = Report::default();
                            let modified = |p: &Path| {
                                std::fs::metadata(p)
                                    .and_then(|m| m.modified())
                                    .unwrap_or(SystemTime::UNIX_EPOCH)
                            };
                            for s in &live {
                                let Some(&Some(pane)) = placed.get(&(s.pid, s.started_ms)) else {
                                    continue;
                                };
                                let at = modified(&s.bus);
                                let read = match cache.get(&s.bus) {
                                    Some((t, read)) if *t == at => read.clone(),
                                    _ => {
                                        let read = read_bus(&s.bus);
                                        cache.insert(s.bus.clone(), (at, read.clone()));
                                        read
                                    }
                                };
                                let at = modified(&s.snapshot);
                                let roles = match role_cache.get(&s.snapshot) {
                                    Some((t, roles)) if *t == at => roles.clone(),
                                    _ => {
                                        let roles = roles(&s.snapshot);
                                        role_cache.insert(s.snapshot.clone(), (at, roles.clone()));
                                        roles
                                    }
                                };
                                found
                                    .asks
                                    .extend(asks_of(pane, (s.pid, s.started_ms), &read));
                                found.agents.extend(agents_of(pane, &roles, &read));
                            }
                            cache.retain(|p, _| live.iter().any(|s| &s.bus == p));
                            role_cache.retain(|p, _| live.iter().any(|s| &s.snapshot == p));
                            found.asks.sort_by_key(|d| (d.ts_ms, d.key));
                            found.agents.sort_by_key(|a| a.pane);
                            found
                        }
                    }
                    None => Report::default(),
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
                snapshot: p.join("100-5.json"),
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
        let d = |pane, seq, ts_ms| Ask {
            pane,
            key: (1, 1, seq),
            kind: AskKind::Decision,
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
    fn the_bus_gives_open_decisions_and_each_agents_last_status() {
        let root = scratch("bus");
        let path = root.join("1-2.bus.json");
        let mut bus = abus::bus::Bus::with_file(path.clone());
        let fields = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        };
        use abus::bus::Kind;
        bus.publish(
            "deploy",
            Kind::DecisionNeeded,
            Some("dev_1"),
            &fields(&[("msg", "merge to main?")]),
            1_000,
        )
        .unwrap();
        bus.publish(
            "status",
            Kind::Fyi,
            Some("dev_2"),
            &fields(&[("msg", "built")]),
            1_001,
        )
        .unwrap();
        let answered = bus
            .publish(
                "review",
                Kind::DecisionNeeded,
                Some("dev_2"),
                &fields(&[("msg", "ship it?")]),
                1_002,
            )
            .unwrap();
        assert!(bus.resolve(answered.seq));
        // dev_2 works, then is done; dev_3 is blocked.
        bus.publish(
            "work",
            Kind::Fyi,
            Some("dev_2"),
            &fields(&[("item", "A"), ("status", "started")]),
            1_003,
        )
        .unwrap();
        bus.publish(
            "work",
            Kind::Fyi,
            Some("dev_3"),
            &fields(&[("item", "B"), ("status", "blocked"), ("msg", "no key")]),
            1_004,
        )
        .unwrap();
        bus.publish(
            "work",
            Kind::Fyi,
            Some("dev_2"),
            &fields(&[("item", "A"), ("status", "done"), ("commit", "abc")]),
            1_005,
        )
        .unwrap();
        drop(bus);
        let read = read_bus(&path);
        assert_eq!(
            read.decisions.len(),
            1,
            "fyi and answered ones are not open: {read:?}"
        );
        assert_eq!(read.decisions[0].1.as_deref(), Some("dev_1"));
        assert_eq!(read.decisions[0].2, "deploy: merge to main?");
        assert_eq!(
            read.statuses,
            [
                (
                    "dev_2".to_string(),
                    6,
                    "done".to_string(),
                    "work: commit=abc · item=A".to_string(),
                    1_005
                ),
                (
                    "dev_3".to_string(),
                    5,
                    "blocked".to_string(),
                    "work: no key · item=B".to_string(),
                    1_004
                ),
            ],
            "the latest per agent, in the order they first posted"
        );
        assert_eq!(read_bus(&root.join("none.bus.json")), BusRead::default());

        // The snapshot names the agents; the bus says where they stand.
        let snap = root.join("1-2.json");
        std::fs::write(
            &snap,
            r#"{"version":2,"layout":{"ids":[0,1,2],"focus":0},"panes":[
                {"id":0,"role":"lead"},{"id":1,"role":"dev_1"},{"id":2,"role":"dev_2"},
                {"id":3,"role":null}],"meta":{"pid":1}}"#,
        )
        .unwrap();
        assert_eq!(roles(&snap), ["lead", "dev_1", "dev_2"]);
        assert!(roles(&root.join("none.json")).is_empty());
        let agents = agents_of(7, &roles(&snap), &read);
        let got: Vec<(&str, Status)> = agents.iter().map(|a| (a.role.as_str(), a.status)).collect();
        assert_eq!(
            got,
            [
                ("lead", Status::Running),
                ("dev_1", Status::Waiting),
                ("dev_2", Status::Finished),
                ("dev_3", Status::Failed),
            ],
            "atrium's order, then one only the bus names"
        );
        assert!(agents.iter().all(|a| a.pane == 7));
        let asks = asks_of(7, (1, 2), &read);
        let got: Vec<(AskKind, String)> = asks.iter().map(|a| (a.kind, a.label())).collect();
        assert_eq!(
            got,
            [
                (
                    AskKind::Decision,
                    "dev_1: deploy: merge to main?".to_string()
                ),
                (
                    AskKind::Finished,
                    "dev_2 finished: work: commit=abc · item=A".to_string()
                ),
                (
                    AskKind::Failed,
                    "dev_3 failed: work: no key · item=B".to_string()
                ),
            ]
        );
        assert_eq!(asks[1].key, (1, 2, 6), "the status event's own sequence");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn status_words_follow_atriums_fleet_rules_and_board() {
        assert_eq!(status_of("done"), Status::Finished);
        assert_eq!(status_of("DONE"), Status::Finished);
        assert_eq!(status_of(" completed "), Status::Finished);
        assert_eq!(status_of("blocked"), Status::Failed);
        assert_eq!(status_of("FAILED"), Status::Failed);
        assert_eq!(status_of("started"), Status::Running);
        assert_eq!(status_of("review=pending"), Status::Running);
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
