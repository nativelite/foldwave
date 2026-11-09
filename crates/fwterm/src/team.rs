//! Claude Code's agent teams in fwterm panes (design §8.7c, slice 4).
//!
//! Claude Code draws no panes of its own: its teams backend runs a program
//! named `tmux` (`split-window -d -t @0 -h -l 70% -P -F '#{pane_id}' --
//! claude --agent-name …`, then `send-keys`, `list-panes`, `kill-pane`) and
//! picks it whenever `TMUX` is set. So every fwterm pane is born with
//! `TMUX=<host channel>,<window pid>,0`, `TMUX_PANE=%<pane>`, a per-pane
//! `FOLDWAVE_TOKEN`, truecolor kept, and a `PATH` led by a directory holding
//! `tmux.exe`: fwterm's own binary under a second name. Started under that
//! name, fwterm serves the verbs with `nativelite-tmuxproto` (the parsing,
//! the `#{…}` formats, the replies; the same crate atrium's shim uses, so a
//! lead moves between hosts without surprises) over a [`Broker`] that talks
//! to the window: one JSON line each way per connection on the window's
//! `nativelite-ipc` endpoint, [`address`]. The window answers `list`,
//! `spawn` (a split beside the target pane, honouring `-l` and `-d`), `kill`
//! and `send`, each only for a caller whose token matches its pane and, for
//! `kill` and `send`, only inside that pane's subtree. `capture-pane` is
//! refused by the crate itself: a host never hands one pane another's text.
//!
//! Inside atrium's panes atrium's own shim wins (it prepends its directory
//! later, §11 row 19); fwterm's serves everywhere else.

use crate::layout::PaneId;
use json::{Number, Value};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tmuxproto::{Host, Node, Outcome};

/// The endpoint's application name: `\\.\pipe\foldwave-ctl-<window pid>`.
pub const APP: &str = "foldwave";

/// What `tmux -V` prints and `#{session_name}` is.
pub const HOST: Host = Host {
    session_name: "foldwave",
    version_suffix: "fwterm",
};

/// Claude Code's own flag for how teammates are shown; `auto` splits panes
/// when inside tmux, which an fwterm pane now is. Taken on the command line
/// since 2.1.50 (hidden from `--help`, so the version is the probe).
pub const TEAMMATE_MODE_FLAG: &str = "--teammate-mode";
pub const TEAMMATE_MODE_SINCE: (u64, u64, u64) = (2, 1, 50);

/// The window's endpoint.
pub fn address(window: u32) -> String {
    nlipc::address(APP, window)
}

// ----------------------------------------------------------------- protocol

/// What a shim asks the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Every pane, for the crate to narrow to the caller and its children.
    List,
    /// A new pane running `argv`, split from `beside` (a pane the caller may
    /// see) or the caller; `focus` false keeps the keyboard where it is;
    /// `size` is tmux's raw `-l`.
    Spawn {
        focus: bool,
        beside: Option<usize>,
        size: Option<String>,
        role: Option<String>,
        argv: Vec<String>,
    },
    Kill(usize),
    /// Type `text` into the pane, then Enter.
    Send(usize, String),
}

/// A request with who sends it: the caller's pane and its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub caller: usize,
    pub token: String,
    pub request: Request,
}

fn s(v: &str) -> Value {
    Value::String(v.to_string())
}

fn n(v: usize) -> Value {
    Value::Number(Number::Int(i64::try_from(v).unwrap_or(i64::MAX)))
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// The request as one line.
pub fn request_line(caller: usize, token: &str, request: &Request) -> String {
    let mut pairs = vec![("caller", n(caller)), ("token", s(token))];
    match request {
        Request::List => pairs.push(("cmd", s("list"))),
        Request::Spawn {
            focus,
            beside,
            size,
            role,
            argv,
        } => {
            pairs.push(("cmd", s("spawn")));
            pairs.push(("focus", Value::Bool(*focus)));
            pairs.push(("beside", beside.map_or(Value::Null, n)));
            pairs.push(("size", size.as_deref().map_or(Value::Null, s)));
            pairs.push(("role", role.as_deref().map_or(Value::Null, s)));
            pairs.push(("argv", Value::Array(argv.iter().map(|a| s(a)).collect())));
        }
        Request::Kill(id) => {
            pairs.push(("cmd", s("kill")));
            pairs.push(("pane", n(*id)));
        }
        Request::Send(id, text) => {
            pairs.push(("cmd", s("send")));
            pairs.push(("pane", n(*id)));
            pairs.push(("text", s(text)));
        }
    }
    obj(pairs).to_string()
}

fn uint(v: Option<&Value>) -> Option<usize> {
    v.and_then(Value::as_i64)
        .and_then(|i| usize::try_from(i).ok())
}

/// Parse a request line. Anything malformed is an error the window answers
/// with [`reply_err`].
pub fn parse_request(line: &str) -> Result<Envelope, String> {
    let v = json::parse(line).map_err(|e| format!("not a request: {e}"))?;
    let caller = uint(v.get("caller")).ok_or("a request names its caller pane")?;
    let token = v
        .get("token")
        .and_then(Value::as_str)
        .ok_or("a request carries its pane's token")?
        .to_string();
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    let request = match v.get("cmd").and_then(Value::as_str) {
        Some("list") => Request::List,
        Some("spawn") => Request::Spawn {
            focus: v.get("focus").and_then(Value::as_bool).unwrap_or(true),
            beside: uint(v.get("beside")),
            size: text("size"),
            role: text("role"),
            argv: v
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        Some("kill") => Request::Kill(uint(v.get("pane")).ok_or("kill names a pane")?),
        Some("send") => Request::Send(
            uint(v.get("pane")).ok_or("send names a pane")?,
            text("text").unwrap_or_default(),
        ),
        Some(other) => return Err(format!("unknown request {other:?}")),
        None => return Err("a request has a cmd".to_string()),
    };
    Ok(Envelope {
        caller,
        token,
        request,
    })
}

/// One pane as the window lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneInfo {
    pub id: PaneId,
    pub parent: Option<PaneId>,
    pub role: Option<String>,
    pub title: String,
}

pub fn reply_list(panes: &[PaneInfo]) -> String {
    obj(vec![
        ("ok", Value::Bool(true)),
        ("foldwave", s(env!("CARGO_PKG_VERSION"))),
        (
            "panes",
            Value::Array(
                panes
                    .iter()
                    .map(|p| {
                        obj(vec![
                            ("id", n(p.id)),
                            ("parent", p.parent.map_or(Value::Null, n)),
                            ("role", p.role.as_deref().map_or(Value::Null, s)),
                            ("title", s(&p.title)),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
    .to_string()
}

pub fn reply_spawned(id: PaneId) -> String {
    obj(vec![("ok", Value::Bool(true)), ("pane", n(id))]).to_string()
}

pub fn reply_ok() -> String {
    obj(vec![("ok", Value::Bool(true))]).to_string()
}

pub fn reply_err(err: &str) -> String {
    obj(vec![("ok", Value::Bool(false)), ("err", s(err))]).to_string()
}

/// A reply's body, or its `err`.
pub fn parse_reply(line: &str) -> Result<Value, String> {
    let v = json::parse(line).map_err(|e| format!("not a reply: {e}"))?;
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v),
        Some(false) => Err(v
            .get("err")
            .and_then(Value::as_str)
            .unwrap_or("refused")
            .to_string()),
        None => Err("not a reply: no ok".to_string()),
    }
}

/// Whether `id` is `root` or below it in the spawn tree `parents`
/// (`(pane, its parent)`); bounded, since a parent can be gone.
pub fn in_subtree(id: PaneId, root: PaneId, parents: &[(PaneId, Option<PaneId>)]) -> bool {
    let mut at = id;
    for _ in 0..64 {
        if at == root {
            return true;
        }
        match parents
            .iter()
            .find(|(p, _)| *p == at)
            .and_then(|(_, up)| *up)
        {
            Some(up) => at = up,
            None => return false,
        }
    }
    false
}

/// The existing pane's share of a split for tmux's `-l` (the NEW pane's
/// size): `70%` leaves the target 30 %, `20` cells leaves it the rest of
/// `room` (the cells the two share, divider included); unset or unreadable
/// is half. Clamped so both sides keep a cell.
pub fn share_for(size: Option<&str>, room: usize) -> f32 {
    let room = room.max(3) as f32;
    let new = match size.map(str::trim) {
        Some(p) if p.ends_with('%') => p[..p.len() - 1]
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| v / 100.0),
        Some(cells) => cells.parse::<f32>().ok().map(|c| c / (room - 1.0)),
        None => None,
    };
    let new = new.unwrap_or(0.5);
    (1.0 - new).clamp(1.0 / room, 1.0 - 1.0 / room)
}

// ------------------------------------------------------------------- broker

/// The shim's side of the channel: requests to the window, with the pane's
/// caller id and token from the environment the pane was born with.
pub struct Broker {
    pub address: String,
    pub caller: Option<usize>,
    pub token: String,
}

impl Broker {
    /// From the pane's environment: `TMUX`'s first field is the endpoint,
    /// `FOLDWAVE_PANE` the caller, `FOLDWAVE_TOKEN` its token.
    pub fn from_env() -> Result<Broker, String> {
        let tmux = std::env::var("TMUX")
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or("not inside an fwterm pane (TMUX unset)")?;
        let address = tmux.split(',').next().unwrap_or("").to_string();
        if address.is_empty() {
            return Err("TMUX names no endpoint".to_string());
        }
        let caller = std::env::var("FOLDWAVE_PANE")
            .ok()
            .and_then(|p| p.parse().ok());
        let token = std::env::var("FOLDWAVE_TOKEN").unwrap_or_default();
        Ok(Broker {
            address,
            caller,
            token,
        })
    }

    fn call(&self, request: &Request) -> Result<Value, String> {
        let caller = self
            .caller
            .ok_or("FOLDWAVE_PANE unset: not an fwterm pane")?;
        let line = request_line(caller, &self.token, request);
        let reply = nlipc::request(&self.address, &line).map_err(|e| e.to_string())?;
        parse_reply(&reply)
    }
}

impl tmuxproto::Broker for Broker {
    fn caller(&self) -> Option<usize> {
        self.caller
    }

    fn list(&mut self) -> Result<Vec<Node>, String> {
        let v = self.call(&Request::List)?;
        Ok(v.get("panes")
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| {
                Some((
                    uint(p.get("id"))?,
                    uint(p.get("parent")),
                    p.get("role").and_then(Value::as_str).map(str::to_string),
                    p.get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ))
            })
            .collect())
    }

    fn spawn(
        &mut self,
        _here: bool,
        focus: bool,
        beside: Option<usize>,
        size: Option<&str>,
        role: Option<&str>,
        argv: &[String],
    ) -> Result<usize, String> {
        // One window: a new window is a split too.
        let v = self.call(&Request::Spawn {
            focus,
            beside,
            size: size.map(str::to_string),
            role: role.map(str::to_string),
            argv: argv.to_vec(),
        })?;
        uint(v.get("pane")).ok_or_else(|| "spawn reply named no pane".to_string())
    }

    fn kill(&mut self, id: usize) -> Result<(), String> {
        self.call(&Request::Kill(id)).map(|_| ())
    }

    fn send(&mut self, id: usize, text: &str) -> Result<(), String> {
        self.call(&Request::Send(id, text.to_string())).map(|_| ())
    }
}

// --------------------------------------------------------------------- shim

/// True when this process was started under the name `tmux`: the copy of
/// fwterm that [`ensure_shim`] placed on the panes' `PATH`.
pub fn invoked_as_shim() -> bool {
    let is_tmux = |p: &Path| {
        p.file_stem()
            .is_some_and(|st| st.eq_ignore_ascii_case("tmux"))
    };
    std::env::current_exe().is_ok_and(|p| is_tmux(&p))
        || std::env::args_os()
            .next()
            .is_some_and(|a| is_tmux(Path::new(&a)))
}

/// Serve one tmux command line as the shim: parse, run, print, and the exit
/// code tmux would give.
pub fn main(args: &[String]) -> u8 {
    let verb = tmuxproto::parse(args);
    let outcome = if tmuxproto::needs_broker(&verb) {
        match Broker::from_env() {
            Ok(mut b) => tmuxproto::run(verb, &mut b, HOST),
            Err(e) => Outcome::err(format!("tmux (fwterm): {e}")),
        }
    } else {
        tmuxproto::run(verb, &mut tmuxproto::NoBroker, HOST)
    };
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    if !outcome.stderr.is_empty() {
        eprintln!("{}", outcome.stderr.trim_end());
    }
    outcome.code
}

/// Where the shim lives: `%LOCALAPPDATA%\foldwave\shim`, beside the settings.
pub fn shim_dir(app: &Path) -> PathBuf {
    app.join("shim")
}

/// Make `<app>\shim\tmux.exe` a current copy of `exe` (fwterm itself): a hard
/// link when the volume allows, a copy otherwise; refreshed when fwterm
/// changes (size or mtime), kept when current, and left alone when a running
/// shim holds it (Windows locks a running image).
pub fn ensure_shim(exe: &Path, app: &Path) -> Result<PathBuf, String> {
    let dir = shim_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let shim = dir.join("tmux.exe");
    let want = std::fs::metadata(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    if let Ok(have) = std::fs::metadata(&shim) {
        let same = have.len() == want.len() && have.modified().ok() == want.modified().ok();
        if same || std::fs::remove_file(&shim).is_err() {
            return Ok(shim);
        }
    }
    if std::fs::hard_link(exe, &shim).is_ok() {
        return Ok(shim);
    }
    std::fs::copy(exe, &shim)
        .map(|_| shim.clone())
        .map_err(|e| format!("{}: {e}", shim.display()))
}

/// Place the shim for this fwterm, or say why not (once, by the caller).
pub fn place_shim() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app =
        crate::logstore::app_dir(std::env::var_os("LOCALAPPDATA")).ok_or("LOCALAPPDATA unset")?;
    ensure_shim(&exe, &app)
}

/// What a pane gets so Claude Code's teams backend finds fwterm: `TMUX`
/// (the endpoint, the window's pid, 0), `TMUX_PANE`, the pane's token,
/// truecolor kept, and `PATH` led by the shim's directory. Nothing without a
/// shim: a `TMUX` that points at no `tmux` would send Claude Code looking
/// for one and dim its colours on the way.
pub fn pane_env(
    window: u32,
    pane: PaneId,
    token: &str,
    shim: Option<&Path>,
    path: Option<&OsStr>,
) -> Vec<(String, String)> {
    let Some(dir) = shim.and_then(Path::parent) else {
        return Vec::new();
    };
    let mut new_path = dir.to_string_lossy().into_owned();
    if let Some(old) = path.map(|p| p.to_string_lossy()) {
        if !old.is_empty() {
            new_path.push(';');
            new_path.push_str(&old);
        }
    }
    vec![
        ("TMUX".into(), format!("{},{window},0", address(window))),
        ("TMUX_PANE".into(), format!("%{pane}")),
        ("FOLDWAVE_TOKEN".into(), token.to_string()),
        ("CLAUDE_CODE_TMUX_TRUECOLOR".into(), "1".into()),
        ("PATH".into(), new_path),
    ]
}

/// A pane's token: 128 random bits as hex, from the standard library's
/// per-process random hasher keys.
pub fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(32);
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(crate::logstore::now_ms());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

// ----------------------------------------------------------- teammate mode

/// Is the first `major.minor.patch` in `version_output` at least `min`?
/// `claude --version` prints `2.1.289 (Claude Code)`.
pub fn version_at_least(version_output: &str, min: (u64, u64, u64)) -> bool {
    let mut nums = [0u64; 3];
    let mut i = 0;
    let mut cur: Option<u64> = None;
    for c in version_output.chars() {
        match c {
            '0'..='9' => cur = Some(cur.unwrap_or(0) * 10 + (c as u64 - '0' as u64)),
            '.' if cur.is_some() && i < 2 => {
                nums[i] = cur.take().unwrap_or(0);
                i += 1;
            }
            _ if cur.is_some() => break,
            _ => {}
        }
    }
    if let Some(v) = cur {
        if i < 3 {
            nums[i] = v;
            i += 1;
        }
    }
    i == 3 && (nums[0], nums[1], nums[2]) >= min
}

/// Whether `argv` starts Claude Code.
pub fn is_claude(argv: &[String]) -> bool {
    argv.first().is_some_and(|a| {
        Path::new(a)
            .file_stem()
            .is_some_and(|st| st.eq_ignore_ascii_case("claude"))
    })
}

/// `argv` with `--teammate-mode auto` added when it starts a Claude Code that
/// takes the flag (`takes_flag`, probed once by the caller) and the user
/// passed none: theirs wins. Anything else comes back as it was.
pub fn with_teammate_mode(argv: &[String], takes_flag: impl FnOnce() -> bool) -> Vec<String> {
    let has = argv
        .iter()
        .any(|a| a == TEAMMATE_MODE_FLAG || a.starts_with("--teammate-mode="));
    if !is_claude(argv) || has || !takes_flag() {
        return argv.to_vec();
    }
    let mut out = argv.to_vec();
    out.push(TEAMMATE_MODE_FLAG.to_string());
    out.push("auto".to_string());
    out
}

/// Probe the installed Claude Code: does `claude --version` say 2.1.50 or
/// later? False when it cannot be run.
pub fn claude_takes_teammate_mode(claude: &str) -> bool {
    std::process::Command::new(claude)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .is_some_and(|v| version_at_least(&v, TEAMMATE_MODE_SINCE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmuxproto::Broker as _;

    #[test]
    fn requests_round_trip_as_one_line_each() {
        let reqs = [
            Request::List,
            Request::Spawn {
                focus: false,
                beside: Some(4),
                size: Some("70%".into()),
                role: Some("researcher".into()),
                argv: vec!["claude".into(), "--agent-name".into(), "researcher".into()],
            },
            Request::Spawn {
                focus: true,
                beside: None,
                size: None,
                role: None,
                argv: vec!["cmd".into()],
            },
            Request::Kill(7),
            Request::Send(3, "say \"hi\" \\ there".into()),
        ];
        for r in reqs {
            let line = request_line(2, "t0k", &r);
            assert!(!line.contains('\n'));
            assert_eq!(
                parse_request(&line).unwrap(),
                Envelope {
                    caller: 2,
                    token: "t0k".into(),
                    request: r
                }
            );
        }
        assert_eq!(
            request_line(1, "t", &Request::List),
            r#"{"caller":1,"token":"t","cmd":"list"}"#
        );
        assert!(parse_request(r#"{"cmd":"list"}"#).is_err(), "no caller");
        assert!(parse_request(r#"{"caller":1,"token":"t","cmd":"dance"}"#).is_err());
        assert!(parse_request("nope").is_err());
    }

    #[test]
    fn replies_carry_ok_and_the_crate_reads_them_back() {
        let list = reply_list(&[
            PaneInfo {
                id: 0,
                parent: None,
                role: None,
                title: "cmd".into(),
            },
            PaneInfo {
                id: 3,
                parent: Some(0),
                role: Some("dev".into()),
                title: "claude".into(),
            },
        ]);
        let v = parse_reply(&list).unwrap();
        assert_eq!(
            v.get("foldwave").and_then(Value::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(
            v.get("panes").and_then(Value::as_array).map(<[_]>::len),
            Some(2)
        );
        assert_eq!(
            parse_reply(&reply_spawned(9))
                .unwrap()
                .get("pane")
                .and_then(Value::as_i64),
            Some(9)
        );
        assert!(parse_reply(&reply_ok()).is_ok());
        assert_eq!(
            parse_reply(&reply_err("no room")),
            Err("no room".to_string())
        );
        assert!(parse_reply(r#"{"pane":1}"#).is_err());
    }

    #[test]
    fn the_subtree_is_the_caller_and_what_it_spawned() {
        let parents = [
            (0, None),
            (1, Some(0)),
            (2, Some(1)),
            (3, None),
            (4, Some(9)),
        ];
        assert!(in_subtree(0, 0, &parents));
        assert!(in_subtree(2, 0, &parents));
        assert!(in_subtree(2, 1, &parents));
        assert!(!in_subtree(1, 2, &parents));
        assert!(!in_subtree(3, 0, &parents));
        assert!(
            !in_subtree(4, 0, &parents),
            "a parent that is gone ends the walk"
        );
        assert!(!in_subtree(5, 0, &parents));
    }

    #[test]
    fn the_split_share_follows_tmuxs_l() {
        // -l names the NEW pane's size; the share is the target's.
        assert!((share_for(Some("70%"), 100) - 0.3).abs() < 1e-6);
        assert!((share_for(Some("30%"), 100) - 0.7).abs() < 1e-6);
        assert!((share_for(None, 100) - 0.5).abs() < 1e-6);
        assert!(
            (share_for(Some("20"), 101) - 0.8).abs() < 1e-6,
            "20 of the 100 cells left"
        );
        assert!((share_for(Some("x"), 50) - 0.5).abs() < 1e-6);
        assert!(share_for(Some("100%"), 10) >= 0.1, "both sides keep a cell");
        assert!(share_for(Some("0%"), 10) <= 0.9);
    }

    #[test]
    fn the_pane_env_leads_path_with_the_shim_or_sets_nothing() {
        let shim = Path::new(r"C:\u\AppData\Local\foldwave\shim\tmux.exe");
        let env = pane_env(4242, 3, "abc", Some(shim), Some(OsStr::new(r"C:\bin")));
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("TMUX"), Some(r"\\.\pipe\foldwave-ctl-4242,4242,0"));
        assert_eq!(get("TMUX_PANE"), Some("%3"));
        assert_eq!(get("FOLDWAVE_TOKEN"), Some("abc"));
        assert_eq!(get("CLAUDE_CODE_TMUX_TRUECOLOR"), Some("1"));
        assert_eq!(
            get("PATH"),
            Some(r"C:\u\AppData\Local\foldwave\shim;C:\bin")
        );
        assert_eq!(env.len(), 5);
        let no_old = pane_env(1, 0, "t", Some(shim), None);
        assert_eq!(
            no_old
                .iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v.as_str()),
            Some(r"C:\u\AppData\Local\foldwave\shim")
        );
        assert!(
            pane_env(1, 0, "t", None, None).is_empty(),
            "no shim, no TMUX"
        );
    }

    #[test]
    fn the_shim_is_a_current_copy_of_the_binary() {
        let root = std::env::temp_dir().join(format!(
            "fwterm-team-{}-{}",
            std::process::id(),
            crate::logstore::now_ms()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let exe = root.join("fwterm.exe");
        std::fs::write(&exe, b"one").unwrap();
        let shim = ensure_shim(&exe, &root).unwrap();
        assert_eq!(shim, root.join("shim").join("tmux.exe"));
        assert_eq!(std::fs::read(&shim).unwrap(), b"one");
        // A changed binary is placed anew; an unchanged one is kept.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&exe, b"two!").unwrap();
        ensure_shim(&exe, &root).unwrap();
        assert_eq!(std::fs::read(&shim).unwrap(), b"two!");
        let before = std::fs::metadata(&shim).unwrap().modified().unwrap();
        ensure_shim(&exe, &root).unwrap();
        assert_eq!(
            std::fs::metadata(&shim).unwrap().modified().unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tokens_are_long_hex_and_differ() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn teammate_mode_is_added_for_a_new_enough_claude_only() {
        assert!(version_at_least(
            "2.1.289 (Claude Code)",
            TEAMMATE_MODE_SINCE
        ));
        assert!(version_at_least("2.1.50", TEAMMATE_MODE_SINCE));
        assert!(!version_at_least(
            "2.1.49 (Claude Code)",
            TEAMMATE_MODE_SINCE
        ));
        assert!(!version_at_least("1.0.0", TEAMMATE_MODE_SINCE));
        assert!(!version_at_least("", TEAMMATE_MODE_SINCE));
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            with_teammate_mode(&argv(&["claude", "--agent-name", "dev"]), || true),
            argv(&["claude", "--agent-name", "dev", "--teammate-mode", "auto"])
        );
        assert_eq!(
            with_teammate_mode(&argv(&[r"C:\x\claude.exe"]), || true),
            argv(&[r"C:\x\claude.exe", "--teammate-mode", "auto"])
        );
        assert_eq!(
            with_teammate_mode(&argv(&["claude", "--teammate-mode", "tmux"]), || true),
            argv(&["claude", "--teammate-mode", "tmux"]),
            "the user's flag wins"
        );
        assert_eq!(
            with_teammate_mode(&argv(&["claude", "--teammate-mode=in-process"]), || true),
            argv(&["claude", "--teammate-mode=in-process"])
        );
        assert_eq!(
            with_teammate_mode(&argv(&["claude"]), || false),
            argv(&["claude"])
        );
        assert_eq!(
            with_teammate_mode(&argv(&["cmd.exe"]), || true),
            argv(&["cmd.exe"])
        );
        assert_eq!(with_teammate_mode(&[], || true), Vec::<String>::new());
    }

    #[test]
    fn version_and_formats_need_no_window() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let out = tmuxproto::run(
            tmuxproto::parse(&args(&["-V"])),
            &mut tmuxproto::NoBroker,
            HOST,
        );
        assert_eq!(out.stdout.trim(), "tmux 3.5a-fwterm");
        assert_eq!(out.code, 0);
    }

    /// A window pid no fwterm has: the endpoint is ours to bind in a test.
    fn fake_window(salt: u32) -> u32 {
        0xFFE0_0000 | ((std::process::id() + salt) & 0xFFFF)
    }

    #[test]
    fn the_broker_speaks_to_a_window_over_the_channel() {
        let window = fake_window(0);
        let mut server = nlipc::Listener::bind(&address(window)).expect("bind");
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        // The window: pane 0 is the lead with token "t0"; it answers list,
        // spawn (pane 1), send and kill, and refuses a wrong token.
        let serving = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while stopped.try_recv().is_err() {
                match server.poll() {
                    Ok(Some(line)) => {
                        let reply = match parse_request(&line) {
                            Err(e) => reply_err(&e),
                            Ok(env) if env.token != "t0" || env.caller != 0 => {
                                reply_err("unauthenticated")
                            }
                            Ok(env) => {
                                seen.push(env.request.clone());
                                match env.request {
                                    Request::List => reply_list(&[
                                        PaneInfo {
                                            id: 0,
                                            parent: None,
                                            role: None,
                                            title: "cmd".into(),
                                        },
                                        PaneInfo {
                                            id: 1,
                                            parent: Some(0),
                                            role: Some("dev".into()),
                                            title: "claude".into(),
                                        },
                                    ]),
                                    Request::Spawn { .. } => reply_spawned(1),
                                    Request::Kill(_) | Request::Send(..) => reply_ok(),
                                }
                            }
                        };
                        server.respond(&reply).unwrap();
                    }
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
                    Err(e) => panic!("poll: {e}"),
                }
            }
            seen
        });
        let mut b = Broker {
            address: address(window),
            caller: Some(0),
            token: "t0".into(),
        };
        let argv: Vec<String> = ["claude", "--agent-name", "dev"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            b.spawn(true, false, None, Some("70%"), Some("dev"), &argv),
            Ok(1)
        );
        assert_eq!(
            b.list(),
            Ok(vec![
                (0, None, None, "cmd".to_string()),
                (1, Some(0), Some("dev".to_string()), "claude".to_string()),
            ])
        );
        assert_eq!(b.send(1, "hello"), Ok(()));
        assert_eq!(b.kill(1), Ok(()));
        // The whole way through the crate: Claude Code's own split line.
        let line: Vec<String> = [
            "split-window",
            "-d",
            "-t",
            "@0",
            "-h",
            "-l",
            "70%",
            "-P",
            "-F",
            "#{pane_id}",
            "--",
            "claude --agent-name dev",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = tmuxproto::run(tmuxproto::parse(&line), &mut b, HOST);
        assert_eq!(
            (out.code, out.stdout.as_str()),
            (0, "%1\n"),
            "{}",
            out.stderr
        );
        let out = tmuxproto::run(
            tmuxproto::parse(
                &[
                    "display-message",
                    "-p",
                    "#{session_name}:#{window_id}.#{pane_id}",
                ]
                .map(String::from),
            ),
            &mut b,
            HOST,
        );
        assert_eq!(out.stdout.trim(), "foldwave:@0.%0");
        // A wrong token is refused by the window, not the crate.
        let mut wrong = Broker {
            address: address(window),
            caller: Some(0),
            token: "nope".into(),
        };
        assert_eq!(wrong.list(), Err("unauthenticated".to_string()));
        stop.send(()).unwrap();
        let seen = serving.join().unwrap();
        assert!(matches!(
            &seen[0],
            Request::Spawn { focus: false, beside: None, size: Some(s), role: Some(r), argv: a }
                if s == "70%" && r == "dev" && a == &argv
        ));
        let spawns: Vec<&Request> = seen
            .iter()
            .filter(|r| matches!(r, Request::Spawn { .. }))
            .collect();
        assert_eq!(spawns.len(), 2, "the broker's and the crate's split");
        assert!(
            spawns
                .iter()
                .all(|r| matches!(r, Request::Spawn { focus: false, .. })),
            "-d: no focus"
        );
        assert!(
            matches!(seen.last(), Some(Request::List)),
            "display-message lists"
        );
    }
}
