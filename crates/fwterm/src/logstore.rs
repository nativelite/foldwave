//! Where pane logs live, and how much of them is kept (design §8.4).
//!
//! Every pane logs by default. The log root comes from, in order: `--log DIR`,
//! the `FOLDWAVE_LOG_DIR` variable, `log_dir` in the settings file
//! (`%LOCALAPPDATA%\foldwave\fwterm.conf`, written by `--choose-log-drive`),
//! then `%LOCALAPPDATA%\foldwave\logs`.
//!
//! ```text
//! <root>/<stamp>-<pid>/            one fwterm run (a session)
//!          live.lock               held open, unshared, while it runs
//!          pane-<id>/
//!            gen-<stamp>-<n>/      a complete panelog, at most one budget share
//! ```
//!
//! A pane's log is a chain of *generations*. Each starts with a checkpoint at
//! its offset 0, so it rebuilds screens without the ones before it, and
//! retention deletes whole generations, oldest first, never a pane's current
//! one. The budget is the smaller of a size cap and what the volume can spare
//! above a reserve kept free for the system, so the logs never fill a drive.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Log bytes kept across every session, unless configured otherwise.
pub const DEFAULT_CAP: u64 = 10 * GIB;

/// Space the logs leave free on their volume: a tenth of it, at least 2 GiB
/// and at most 20 GiB.
pub fn reserve(total: u64) -> u64 {
    (total / 10).clamp(2 * GIB, 20 * GIB)
}

/// How many log bytes may be kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub bytes: u64,
    /// The volume, not the cap, sets the budget: the drive is nearly full and
    /// old logs go sooner than the cap alone would send them.
    pub squeezed: bool,
}

/// The budget on a volume with `free` of `total` bytes left, where the logs
/// already hold `logs` bytes (theirs to reuse).
pub fn budget(cap: u64, logs: u64, free: u64, total: u64) -> Budget {
    let room = (free + logs).saturating_sub(reserve(total));
    Budget {
        bytes: cap.min(room),
        squeezed: room < cap,
    }
}

/// When a pane's current generation rolls over: a thirty-second of the
/// budget, between 16 MiB and 256 MiB. The floor bounds how far a squeezed
/// volume is still written: one generation per live pane.
pub fn generation_bytes(budget: u64) -> u64 {
    (budget / 32).clamp(16 * MIB, 256 * MIB)
}

/// What the settings file says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub log_dir: Option<PathBuf>,
    pub log_cap: Option<u64>,
}

/// `key = value` lines; `#` starts a comment. Unknown keys are ignored.
pub fn parse_settings(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim();
        match k.trim() {
            "log_dir" if !v.is_empty() => s.log_dir = Some(PathBuf::from(v)),
            "log_cap_gib" => s.log_cap = v.parse::<u64>().ok().map(|g| g * GIB),
            _ => {}
        }
    }
    s
}

/// `settings` with `log_dir` set to `dir`, other lines kept.
pub fn with_log_dir(settings: &str, dir: &Path) -> String {
    let mut out: String = settings
        .lines()
        .filter(|l| l.split('=').next().map(str::trim) != Some("log_dir"))
        .map(|l| format!("{l}\n"))
        .collect();
    out.push_str(&format!("log_dir = {}\n", dir.display()));
    out
}

/// `%LOCALAPPDATA%\foldwave`, where settings and default logs live.
pub fn app_dir(local_app_data: Option<OsString>) -> Option<PathBuf> {
    local_app_data.map(|d| PathBuf::from(d).join("foldwave"))
}

pub fn settings_path(app: &Path) -> PathBuf {
    app.join("fwterm.conf")
}

/// Where the log root came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    Settings,
    Default,
}

/// The log root, by the order in the module docs.
pub fn resolve(
    flag: Option<&str>,
    env: Option<OsString>,
    settings: &Settings,
    app: Option<&Path>,
) -> Option<(PathBuf, Source)> {
    if let Some(f) = flag {
        return Some((PathBuf::from(f), Source::Flag));
    }
    if let Some(e) = env.filter(|e| !e.is_empty()) {
        return Some((PathBuf::from(e), Source::Env));
    }
    if let Some(d) = &settings.log_dir {
        return Some((d.clone(), Source::Settings));
    }
    app.map(|a| (a.join("logs"), Source::Default))
}

/// One generation found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gen {
    pub path: PathBuf,
    pub bytes: u64,
    /// Creation time from its name, in ms since the epoch: the age order.
    pub stamp: u64,
    /// The current generation of a pane that may still be writing it.
    pub protected: bool,
}

/// Which generations to delete, oldest first, to fit `budget`.
pub fn plan(gens: &[Gen], budget: u64) -> Vec<PathBuf> {
    let mut total: u64 = gens.iter().map(|g| g.bytes).sum();
    let mut order: Vec<&Gen> = gens.iter().filter(|g| !g.protected).collect();
    order.sort_by_key(|g| g.stamp);
    let mut out = Vec::new();
    for g in order {
        if total <= budget {
            break;
        }
        total -= g.bytes;
        out.push(g.path.clone());
    }
    out
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn gen_name(stamp: u64, n: u32) -> String {
    format!("gen-{stamp:013}-{n:05}")
}

fn gen_stamp(name: &str) -> Option<u64> {
    name.strip_prefix("gen-")?.split('-').next()?.parse().ok()
}

/// A pane's generations, oldest first.
pub fn generations(pane_dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<(String, PathBuf)> = fs::read_dir(pane_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        // A real folder: never a link or junction to somewhere else.
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            gen_stamp(&name)?;
            Some((name, e.path()))
        })
        .collect();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

fn dir_bytes(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_bytes(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

const LOCK: &str = "live.lock";
/// Written when fwterm makes a session folder and never removed: a folder
/// without it is not fwterm's, whatever its name.
const MARK: &str = "foldwave-session";

/// Whether another fwterm still runs `session`: its lock cannot be deleted
/// while it is held. A dead session's stale lock is deleted on the way.
fn session_live(session: &Path) -> bool {
    let lock = session.join(LOCK);
    match fs::remove_file(&lock) {
        Ok(()) => false,
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `<13-digit stamp>-<pid>`, as [`Session::create`] names them.
fn is_session_name(name: &str) -> bool {
    name.split_once('-')
        .is_some_and(|(stamp, pid)| stamp.len() == 13 && digits(stamp) && digits(pid))
}

/// Subfolders of `dir` whose names pass `ours`. Only folders fwterm named
/// are ever looked into or removed: the root may be any folder a person
/// chose, with other things in it.
fn ours(dir: &Path, ours: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()))
        .filter(|e| e.file_name().to_str().is_some_and(&ours))
        .map(|e| e.path())
        .collect()
}

/// Every generation under `root`, with the current generation of each pane
/// in a live session (this one included) protected. Emptied pane and
/// session folders of finished runs are removed on the way (never
/// recursively: anything else in them stays).
pub fn scan(root: &Path, own: &Path) -> Vec<Gen> {
    let mut out = Vec::new();
    for s in ours(root, is_session_name) {
        if !s.join(MARK).is_file() {
            continue;
        }
        let live = s == own || session_live(&s);
        let panes = ours(&s, |n| n.strip_prefix("pane-").is_some_and(digits));
        if panes.is_empty() && !live {
            // Only the mark left: the session is over and empty.
            if fs::read_dir(&s).map_or(0, |d| d.count()) == 1 {
                let _ = fs::remove_file(s.join(MARK));
                let _ = fs::remove_dir(&s);
            }
            continue;
        }
        for pane in panes {
            let gens = generations(&pane);
            if gens.is_empty() && !live {
                let _ = fs::remove_dir(&pane);
            }
            let last = gens.len().saturating_sub(1);
            for (i, g) in gens.into_iter().enumerate() {
                let name = g.file_name().and_then(|n| n.to_str()).unwrap_or("");
                out.push(Gen {
                    stamp: gen_stamp(name).unwrap_or(0),
                    bytes: dir_bytes(&g),
                    protected: live && i == last,
                    path: g,
                });
            }
        }
    }
    out
}

/// Delete a generation. Its `panelog.meta` goes first, so a deletion cut
/// short (a file still open elsewhere) leaves a log nothing will open; the
/// next pass finishes it.
pub fn delete(gen: &Path) {
    let _ = fs::remove_file(gen.join("panelog.meta"));
    let _ = fs::remove_dir_all(gen);
    if let Some(pane) = gen.parent() {
        if generations(pane).is_empty() {
            let _ = fs::remove_dir(pane);
        }
    }
}

/// This run's session folder, locked while it lives.
pub struct Session {
    pub dir: PathBuf,
    _lock: fs::File,
}

impl Session {
    pub fn create(root: &Path) -> io::Result<Session> {
        let dir = root.join(format!("{:013}-{}", now_ms(), std::process::id()));
        fs::create_dir_all(root)?;
        // Not `create_dir_all`: a folder already there is not ours to adopt.
        fs::create_dir(&dir)?;
        fs::write(dir.join(MARK), b"")?;
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut opts, 0);
        let lock = opts.open(dir.join(LOCK))?;
        Ok(Session { dir, _lock: lock })
    }

    pub fn pane_dir(&self, id: usize) -> PathBuf {
        self.dir.join(format!("pane-{id}"))
    }
}

/// Bytes shown to people: `8.7 GB`, `256 MB`.
pub fn human(bytes: u64) -> String {
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MB", bytes / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gen(name: &str, stamp: u64, mib: u64, protected: bool) -> Gen {
        Gen {
            path: PathBuf::from(name),
            bytes: mib * MIB,
            stamp,
            protected,
        }
    }

    #[test]
    fn the_reserve_is_a_tenth_of_the_volume_within_bounds() {
        assert_eq!(reserve(10 * GIB), 2 * GIB);
        assert_eq!(reserve(100 * GIB), 10 * GIB);
        assert_eq!(reserve(4000 * GIB), 20 * GIB);
    }

    #[test]
    fn a_roomy_volume_gives_the_cap() {
        let b = budget(DEFAULT_CAP, 0, 500 * GIB, 1000 * GIB);
        assert_eq!(
            b,
            Budget {
                bytes: DEFAULT_CAP,
                squeezed: false
            }
        );
    }

    #[test]
    fn a_nearly_full_volume_squeezes_the_budget_and_logs_count_as_room() {
        // 476 GB drive, 8.7 GB free: under the 20 GiB reserve, nothing to spend.
        let b = budget(DEFAULT_CAP, 0, 8 * GIB, 476 * GIB);
        assert_eq!(
            b,
            Budget {
                bytes: 0,
                squeezed: true
            }
        );
        // 3 GiB already in logs and 20 GiB free: 3 GiB may stay.
        let b = budget(DEFAULT_CAP, 3 * GIB, 20 * GIB, 476 * GIB);
        assert_eq!(
            b,
            Budget {
                bytes: 3 * GIB,
                squeezed: true
            }
        );
    }

    #[test]
    fn generations_are_a_share_of_the_budget_within_bounds() {
        assert_eq!(generation_bytes(DEFAULT_CAP), 256 * MIB);
        assert_eq!(generation_bytes(0), 16 * MIB);
        assert_eq!(generation_bytes(1 * GIB), 32 * MIB);
    }

    #[test]
    fn the_plan_deletes_oldest_first_and_never_a_protected_one() {
        let gens = [
            gen("c", 30, 100, false),
            gen("a", 10, 100, true),
            gen("b", 20, 100, false),
            gen("d", 40, 100, true),
        ];
        assert!(plan(&gens, 400 * MIB).is_empty());
        assert_eq!(plan(&gens, 300 * MIB), vec![PathBuf::from("b")]);
        assert_eq!(
            plan(&gens, 150 * MIB),
            vec![PathBuf::from("b"), PathBuf::from("c")]
        );
        // Nothing unprotected left: it stops over budget rather than touch "a"/"d".
        assert_eq!(plan(&gens, 0).len(), 2);
    }

    #[test]
    fn settings_parse_and_rewrite() {
        let s = parse_settings(
            "# fwterm\nlog_dir = E:\\foldwave\\logs  # fast drive\nlog_cap_gib=4\nother = 1\n",
        );
        assert_eq!(s.log_dir, Some(PathBuf::from("E:\\foldwave\\logs")));
        assert_eq!(s.log_cap, Some(4 * GIB));
        let again = with_log_dir("log_cap_gib=4\nlog_dir = C:\\old\n", Path::new("D:\\new"));
        assert_eq!(again, "log_cap_gib=4\nlog_dir = D:\\new\n");
        assert_eq!(parse_settings("log_dir =\n").log_dir, None);
    }

    #[test]
    fn the_root_comes_from_flag_then_env_then_settings_then_the_default() {
        let app = PathBuf::from("L\\foldwave");
        let set = Settings {
            log_dir: Some(PathBuf::from("S")),
            log_cap: None,
        };
        let none = Settings::default();
        let r = |f: Option<&str>, e: Option<&str>, s: &Settings| {
            resolve(f, e.map(OsString::from), s, Some(&app)).unwrap()
        };
        assert_eq!(
            r(Some("F"), Some("E"), &set),
            (PathBuf::from("F"), Source::Flag)
        );
        assert_eq!(r(None, Some("E"), &set), (PathBuf::from("E"), Source::Env));
        assert_eq!(
            r(None, Some(""), &set),
            (PathBuf::from("S"), Source::Settings)
        );
        assert_eq!(r(None, None, &none), (app.join("logs"), Source::Default));
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fwterm-logstore-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_gen(pane: &Path, stamp: u64, n: u32, bytes: usize) -> PathBuf {
        let g = pane.join(gen_name(stamp, n));
        fs::create_dir_all(&g).unwrap();
        fs::write(g.join("panelog.meta"), b"panelog 1\n").unwrap();
        fs::write(g.join("seg-00000001.log"), vec![0u8; bytes]).unwrap();
        g
    }

    #[test]
    fn scan_protects_live_panes_current_generation_and_clears_dead_sessions() {
        let root = scratch("scan");
        let own = Session::create(&root).unwrap();
        let a0 = fake_gen(&own.pane_dir(0), 100, 0, 1000);
        let a1 = fake_gen(&own.pane_dir(0), 200, 1, 1000);
        // A session from an fwterm that is gone: a stale lock, one generation.
        let dead = root.join("0000000000001-1");
        fs::create_dir_all(&dead).unwrap();
        fs::write(dead.join(MARK), b"").unwrap();
        fs::write(dead.join(LOCK), b"").unwrap();
        let d0 = fake_gen(&dead.join("pane-0"), 50, 0, 1000);
        // An empty dead session is removed outright.
        let empty = root.join("0000000000002-2");
        fs::create_dir_all(&empty).unwrap();
        fs::write(empty.join(MARK), b"").unwrap();
        // Named like a session, laid out like one, but not made by fwterm:
        // no mark, so nothing in it is counted or deleted.
        let lookalike = root.join("0000000000000-9");
        let alien = fake_gen(&lookalike.join("pane-0"), 1, 0, 1000);
        // Folders fwterm did not make are left alone, even empty ones.
        let foreign = root.join("photos");
        fs::create_dir_all(&foreign).unwrap();
        fs::write(foreign.join("a.jpg"), b"x").unwrap();
        let foreign_empty = root.join("0000000000003-3-notes");
        fs::create_dir_all(&foreign_empty).unwrap();

        let mut gens = scan(&root, &own.dir);
        gens.sort_by_key(|g| g.stamp);
        let got: Vec<(PathBuf, bool)> =
            gens.iter().map(|g| (g.path.clone(), g.protected)).collect();
        assert_eq!(
            got,
            vec![(d0.clone(), false), (a0.clone(), false), (a1.clone(), true)]
        );
        assert!(gens.iter().all(|g| g.bytes >= 1000));
        assert!(!empty.exists());
        assert!(foreign.join("a.jpg").exists() && foreign_empty.exists());
        assert!(alien.exists());

        for p in plan(&gens, 1500) {
            delete(&p);
        }
        assert!(!d0.exists() && !a0.exists() && a1.exists());
        assert!(
            !dead.join("pane-0").exists(),
            "an emptied pane folder goes too"
        );
        drop(own);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    fn a_running_sessions_lock_cannot_be_taken() {
        let root = scratch("lock");
        let s = Session::create(&root).unwrap();
        assert!(session_live(&s.dir));
        drop(s);
        let dir = fs::read_dir(&root).unwrap().next().unwrap().unwrap().path();
        assert!(!session_live(&dir));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn generation_names_sort_by_age() {
        let root = scratch("names");
        for (s, n) in [(900, 2), (100, 0), (500, 1)] {
            fs::create_dir_all(root.join(gen_name(s, n))).unwrap();
        }
        fs::create_dir_all(root.join("not-a-gen")).unwrap();
        let names: Vec<String> = generations(&root)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![gen_name(100, 0), gen_name(500, 1), gen_name(900, 2)]
        );
        let _ = fs::remove_dir_all(&root);
    }
}
