//! atrium's control plane, read from the window (design §8.7c, slice 3).
//!
//! An atrium started with `--allow-ctl` answers one JSON line per connection
//! on `\\.\pipe\atrium-ctl-<pid>` (`nativelite-ipc`); the reads need no
//! token and are a documented, stable surface from 0.43.0 on
//! (`docs/control-plane.md`, "Reading the control plane from a host"): a
//! reply keeps every key it has, `ok` is always there and
//! `{"ok":false,"err":…}` is the only failure, pane ids are stable for the
//! session, and `list` names the atrium that answered.
//!
//! What the window takes from it: the live org chart (`list`: each pane's
//! id, parent, role, agsess status and idle time), which replaces the
//! snapshot file's roles (rewritten only every 15 s) and adds atrium's own
//! reading of each agent. Decisions and the agents' `status=` words still
//! come from the bus sidecar, unless the atrium is 0.45.1 or later: a host
//! has no pane, and an older server serves its `bus feed` as the operator,
//! who subscribes to nothing and gets nothing (found 2026-11-09; atrium
//! 0.45.1 serves a caller-less feed as the whole bus, so `list`'s `atrium`
//! key decides). An atrium that holds no endpoint (ctl off) costs one
//! system call to learn, and the sidecar carries on alone.

use crate::agents::{headline, BusRead, Status};
use std::collections::BTreeMap;

/// The `list` request: the org chart.
pub const LIST: &str = r#"{"cmd":"list"}"#;

/// The `status` request for one pane (its caption, `doing`, when the mod
/// reports one; not read yet).
pub fn status_request(id: u64) -> String {
    format!(r#"{{"cmd":"status","target":"{id}"}}"#)
}

/// The `bus feed` request a host sends: the whole bus, every look, since a
/// decision's `resolved` flips on an event already read.
pub const FEED: &str = r#"{"cmd":"bus","op":"feed","since":0}"#;

/// Whether this atrium (the `atrium` key of `list`) serves a host's `bus
/// feed` as the whole bus: 0.45.1 and later. Older ones answer it empty.
pub fn serves_host_feed(version: &str) -> bool {
    let mut parts = version.trim().split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .unwrap_or(0)
    });
    let v = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    v >= (0, 45, 1)
}

/// Parse a `bus feed` reply into what the sidecar read gives: the open
/// decisions (`decision_needed`, not `resolved`), oldest first, and each
/// agent's latest `status=` event. An event is `{"seq","topic","kind","from",
/// "ts","resolved","fields":{…}}` (abus's `event_to_value`).
pub fn parse_feed(reply: &str) -> Result<BusRead, String> {
    let v = json::parse(reply).map_err(|e| format!("not a ctl reply: {e:?}"))?;
    ok(&v)?;
    let u = |v: Option<&json::Value>| v.and_then(json::Value::as_i64).unwrap_or(0).max(0) as u64;
    let mut read = BusRead::default();
    for e in v.get("feed").and_then(json::Value::as_array).unwrap_or(&[]) {
        let seq = u(e.get("seq"));
        let topic = e.get("topic").and_then(json::Value::as_str).unwrap_or("");
        let from = e
            .get("from")
            .and_then(json::Value::as_str)
            .map(str::to_string);
        let ts = u(e.get("ts"));
        let fields: BTreeMap<String, String> = e
            .get("fields")
            .and_then(json::Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let open = e.get("kind").and_then(json::Value::as_str) == Some("decision_needed")
            && e.get("resolved").and_then(json::Value::as_bool) == Some(false);
        if open {
            read.decisions
                .push((seq, from.clone(), headline(topic, &fields), ts));
        }
        if let (Some(from), Some(status)) = (&from, fields.get("status")) {
            let rest: BTreeMap<String, String> = fields
                .iter()
                .filter(|(k, _)| k.as_str() != "status")
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let entry = (
                from.clone(),
                seq,
                status.clone(),
                headline(topic, &rest),
                ts,
            );
            match read.statuses.iter_mut().find(|s| &s.0 == from) {
                Some(slot) => *slot = entry,
                None => read.statuses.push(entry),
            }
        }
    }
    Ok(read)
}

/// Ask the atrium with this pid for its bus; call it only for an atrium
/// that [`serves_host_feed`].
pub fn feed(pid: u32) -> Result<BusRead, String> {
    let addr = address(pid);
    if !endpoint_exists(&addr) {
        return Err("no control channel".to_string());
    }
    let reply = nlipc::request(&addr, FEED).map_err(|e| e.to_string())?;
    parse_feed(&reply)
}

/// One pane in the org chart, as `list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The agent id: `ATRIUM_PANE` inside the pane, `%<id>` in the tmux shim.
    pub id: u64,
    /// `None` for a root pane.
    pub parent: Option<u64>,
    pub role: Option<String>,
    pub title: String,
    pub depth: u64,
    /// agsess's word (`working`, `waiting-approval`, `waiting-prompt`,
    /// `idle`, `errored`, `ended`), `None` for an unbound pane.
    pub status: Option<String>,
    pub idle_ms: u64,
}

/// A `list` reply: the atrium's version and its panes, in its order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub atrium: String,
    pub nodes: Vec<Node>,
}

impl Tree {
    /// The agents: every pane with a role, in atrium's order.
    pub fn roles(&self) -> Vec<String> {
        self.nodes.iter().filter_map(|n| n.role.clone()).collect()
    }

    /// atrium's own reading of each agent, where it says something the strip
    /// shows: `(role, status)` for the panes whose word is an end or a wait.
    pub fn statuses(&self) -> Vec<(String, Status)> {
        self.nodes
            .iter()
            .filter_map(|n| {
                let role = n.role.clone()?;
                let status = status_word(n.status.as_deref())?;
                Some((role, status))
            })
            .collect()
    }
}

/// What an agsess status word means to the strip: `waiting-approval` is
/// waiting on the human, `errored` failed, `ended` finished; `working`,
/// `waiting-prompt` (its turn is over, nothing is asked) and `idle` say
/// nothing the bus does not say better, and so does an unbound pane.
pub fn status_word(word: Option<&str>) -> Option<Status> {
    match word? {
        "waiting-approval" => Some(Status::Waiting),
        "errored" => Some(Status::Failed),
        "ended" => Some(Status::Finished),
        _ => None,
    }
}

/// The reply's `ok`, else its `err` as the error.
fn ok(v: &json::Value) -> Result<(), String> {
    match v.get("ok").and_then(json::Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => Err(v
            .get("err")
            .and_then(json::Value::as_str)
            .unwrap_or("refused")
            .to_string()),
        None => Err("not a ctl reply: no ok".to_string()),
    }
}

/// Parse a `list` reply. Keys that are missing read as empty, not as an
/// error, since a reply only ever gains keys.
pub fn parse_list(reply: &str) -> Result<Tree, String> {
    let v = json::parse(reply).map_err(|e| format!("not a ctl reply: {e:?}"))?;
    ok(&v)?;
    let u = |v: Option<&json::Value>| v.and_then(json::Value::as_i64).map(|n| n.max(0) as u64);
    let s = |v: Option<&json::Value>| v.and_then(json::Value::as_str).map(str::to_string);
    let nodes = v
        .get("tree")
        .and_then(json::Value::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|n| {
            Some(Node {
                id: u(n.get("id"))?,
                parent: u(n.get("parent")),
                role: s(n.get("role")).filter(|r| !r.is_empty()),
                title: s(n.get("title")).unwrap_or_default(),
                depth: u(n.get("depth")).unwrap_or(0),
                status: s(n.get("status")),
                idle_ms: u(n.get("idle_ms")).unwrap_or(0),
            })
        })
        .collect();
    Ok(Tree {
        atrium: s(v.get("atrium")).unwrap_or_default(),
        nodes,
    })
}

/// The endpoint of the atrium with this pid.
pub fn address(pid: u32) -> String {
    nlipc::address("atrium", pid)
}

/// Whether a server holds this endpoint's name, learnt without connecting
/// (`nativelite-ipc` 0.2.1, one system call). The crate's client retries a
/// name nobody holds for its whole deadline (30 s: an `atrium ctl` started
/// before its atrium waits for it), which a watcher looking at an atrium
/// without `--allow-ctl` cannot afford.
pub fn endpoint_exists(addr: &str) -> bool {
    nlipc::endpoint_exists(addr)
}

/// Ask the atrium with this pid for its org chart: one connection, one line
/// each way. An atrium without `--allow-ctl` holds no endpoint, which costs
/// one system call to learn.
pub fn list(pid: u32) -> Result<Tree, String> {
    let addr = address(pid);
    if !endpoint_exists(&addr) {
        return Err("no control channel".to_string());
    }
    let reply = nlipc::request(&addr, LIST).map_err(|e| e.to_string())?;
    parse_list(&reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// atrium's own golden `list` reply (`src/ctl/tests.rs`,
    /// `golden_list_reply_names_the_atrium_and_each_pane`), version filled in.
    const GOLDEN: &str = r#"{"ok":true,"atrium":"0.43.0","tree":[{"id":1,"parent":null,"role":null,"title":"cmd","depth":0,"status":null,"idle_ms":0},{"id":3,"parent":1,"role":"lead","title":"claude","depth":1,"status":"working","idle_ms":1200}]}"#;

    #[test]
    fn the_golden_list_reply_parses_to_the_tree() {
        let t = parse_list(GOLDEN).unwrap();
        assert_eq!(t.atrium, "0.43.0");
        assert_eq!(
            t.nodes,
            [
                Node {
                    id: 1,
                    parent: None,
                    role: None,
                    title: "cmd".into(),
                    depth: 0,
                    status: None,
                    idle_ms: 0,
                },
                Node {
                    id: 3,
                    parent: Some(1),
                    role: Some("lead".into()),
                    title: "claude".into(),
                    depth: 1,
                    status: Some("working".into()),
                    idle_ms: 1200,
                },
            ]
        );
        assert_eq!(t.roles(), ["lead"]);
        assert!(
            t.statuses().is_empty(),
            "working says nothing the strip shows"
        );
    }

    #[test]
    fn unknown_keys_are_ignored_and_missing_ones_read_as_empty() {
        let t = parse_list(r#"{"ok":true,"atrium":"9.9.9","teams":"show","tree":[{"id":7,"role":"dev_1","status":"errored","extra":[1,2]}]}"#).unwrap();
        assert_eq!(t.nodes.len(), 1);
        assert_eq!(t.nodes[0].parent, None);
        assert_eq!(t.nodes[0].title, "");
        assert_eq!(t.statuses(), [("dev_1".to_string(), Status::Failed)]);
        let empty = parse_list(r#"{"ok":true}"#).unwrap();
        assert_eq!(
            empty,
            Tree {
                atrium: String::new(),
                nodes: Vec::new()
            }
        );
    }

    #[test]
    fn a_refusal_and_a_non_reply_are_errors() {
        assert_eq!(
            parse_list(r#"{"ok":false,"err":"unauthenticated: no token"}"#),
            Err("unauthenticated: no token".to_string())
        );
        assert_eq!(parse_list(r#"{"ok":false}"#), Err("refused".to_string()));
        assert!(parse_list(r#"{"tree":[]}"#).unwrap_err().contains("no ok"));
        assert!(parse_list("not json").is_err());
    }

    #[test]
    fn agsess_words_map_to_what_the_strip_shows() {
        assert_eq!(status_word(None), None);
        assert_eq!(status_word(Some("working")), None);
        assert_eq!(status_word(Some("waiting-prompt")), None);
        assert_eq!(status_word(Some("idle")), None);
        assert_eq!(status_word(Some("waiting-approval")), Some(Status::Waiting));
        assert_eq!(status_word(Some("errored")), Some(Status::Failed));
        assert_eq!(status_word(Some("ended")), Some(Status::Finished));
        assert_eq!(
            status_word(Some("something-new")),
            None,
            "a word 0.44 may add"
        );
    }

    #[test]
    fn requests_are_what_atrium_ctl_sends() {
        assert_eq!(LIST, r#"{"cmd":"list"}"#);
        assert_eq!(status_request(3), r#"{"cmd":"status","target":"3"}"#);
        assert_eq!(FEED, r#"{"cmd":"bus","op":"feed","since":0}"#);
    }

    #[test]
    fn the_host_feed_needs_atrium_0_45_1() {
        assert!(!serves_host_feed("0.43.0"));
        assert!(!serves_host_feed("0.45.0"));
        assert!(serves_host_feed("0.45.1"));
        assert!(serves_host_feed("0.46.0"));
        assert!(serves_host_feed("1.0.0"));
        assert!(serves_host_feed("0.45.1-rc1"));
        assert!(!serves_host_feed(""));
        assert!(!serves_host_feed("garbage"));
    }

    #[test]
    fn a_feed_reply_reads_as_the_sidecar_does() {
        let reply = r#"{"ok":true,"feed":[
          {"seq":1,"topic":"deploy","kind":"decision_needed","from":"dev_1","ts":1000,"resolved":false,"fields":{"msg":"merge to main?"}},
          {"seq":2,"topic":"status","kind":"fyi","from":"dev_2","ts":1001,"resolved":false,"fields":{"msg":"built","status":"working"}},
          {"seq":3,"topic":"review","kind":"decision_needed","from":"dev_2","ts":1002,"resolved":true,"fields":{"msg":"ship it?"}},
          {"seq":4,"topic":"status","kind":"fyi","from":"dev_2","ts":1003,"resolved":false,"fields":{"status":"done","msg":"tests green"}},
          {"seq":5,"topic":"work","kind":"fyi","from":null,"ts":1004,"resolved":false,"fields":{"status":"done"}}
        ],"cursor":5}"#;
        let read = parse_feed(reply).unwrap();
        assert_eq!(
            read.decisions,
            [(
                1,
                Some("dev_1".into()),
                "deploy: merge to main?".into(),
                1000
            )],
            "open decisions only"
        );
        assert_eq!(
            read.statuses,
            [(
                "dev_2".into(),
                4,
                "done".into(),
                "status: tests green".into(),
                1003
            )],
            "the latest status per agent; an event from nobody is no status"
        );
        assert_eq!(
            parse_feed(r#"{"ok":true,"feed":[],"cursor":0}"#),
            Ok(BusRead::default())
        );
        assert!(parse_feed(r#"{"ok":false,"err":"no"}"#).is_err());
    }

    /// A pid no atrium has: the endpoint is ours to bind in a test.
    fn fake_pid(salt: u32) -> u32 {
        0xFFF0_0000 | ((std::process::id() + salt) & 0xFFFF)
    }

    #[test]
    fn list_reads_a_live_endpoint_and_a_missing_one_fails_fast() {
        let pid = fake_pid(0);
        let mut server = nlipc::Listener::bind(&address(pid)).expect("bind the test endpoint");
        // The server lives until the client side is done: a listener dropped
        // right after a reply disconnects the pipe before the client reads.
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let serving = std::thread::spawn(move || {
            let mut served = 0;
            while stopped.try_recv().is_err() {
                match server.poll() {
                    Ok(Some(req)) => {
                        assert_eq!(req.trim(), LIST);
                        server.respond(GOLDEN).unwrap();
                        served += 1;
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
                    Err(e) => panic!("poll: {e}"),
                }
            }
            served
        });
        let t = list(pid).expect("the endpoint answers");
        assert_eq!(t.atrium, "0.43.0");
        assert_eq!(t.roles(), ["lead"]);
        // The cost of one read, for the idle budget (design §8.7c, slice 3).
        let n = 50;
        let t0 = std::time::Instant::now();
        for i in 0..n {
            if let Err(e) = list(pid) {
                panic!("request {i} after {:?} failed: {e}", t0.elapsed());
            }
        }
        println!("ctl list: {:?} per request over a pipe", t0.elapsed() / n);
        assert!(endpoint_exists(&address(pid)), "a bound endpoint exists");
        stop.send(()).unwrap();
        assert_eq!(serving.join().unwrap(), 51);

        // No atrium at this pid: no endpoint, learnt without a connect, and
        // quickly (the ipc client alone would wait out its 30 s deadline).
        let t0 = std::time::Instant::now();
        assert!(!endpoint_exists(&address(fake_pid(1))));
        assert_eq!(list(fake_pid(1)), Err("no control channel".to_string()));
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "a missing endpoint must not stall the watcher: {:?}",
            t0.elapsed()
        );
    }
}
