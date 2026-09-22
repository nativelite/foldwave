//! Crash and replay: does the log survive a kill, and how fast does a screen
//! come back from it?
//!
//! §4 promises that "after a crash or kill, every pane's history up to the last
//! flush replays". Nothing tested it. This does, with a real kill rather than a
//! simulated one:
//!
//! 1. Spawn a child that appends agent-like bytes to a log on the target drive,
//!    unbuffered write-through, exactly as §8.4's writer will.
//! 2. Kill it mid-write ([`std::process::Child::kill`], which is
//!    `TerminateProcess` on Windows: no unwinding, no flush, no destructors).
//! 3. Read what is on disk and check it byte for byte against the stream the
//!    child was producing. The workload is generated from a seed, so the whole
//!    log must be an exact *prefix* of it: any torn, reordered or partially
//!    written record shows up as a mismatch.
//! 4. Replay that prefix through `vterm` and time it, which is also the
//!    pane-switch cost §8.0 budgets a checkpoint interval against.
//!
//! The property being tested is what write-through buys: a write is durable
//! when it returns, so a killed writer leaves whole records or nothing, never
//! half of one.

use crate::disk::{aligned, open_writethrough, SECTOR};
use crate::parse;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Seed for the crash workload: the child writes it, the parent regenerates it
/// to check what survived.
pub const SEED_PANE: usize = 0;

/// How long to wait for the child to create its log before giving up.
const STARTUP_LIMIT: Duration = Duration::from_secs(5);

/// What to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory for the log; it is removed afterwards.
    pub dir: PathBuf,
    /// Bytes the child is told to write. It is killed long before this.
    pub bytes: u64,
    /// Bytes per append; a multiple of [`SECTOR`].
    pub io: usize,
    /// How long the child writes before the kill.
    pub run_for: Duration,
    /// Bytes of log to replay when timing a pane rebuild.
    pub rebuild_bytes: u64,
}

/// What survived, and what it cost to come back.
#[derive(Debug, Clone)]
pub struct Recovery {
    /// Bytes found in the log after the kill.
    pub durable: u64,
    /// Whether every byte matches the stream the child was writing.
    pub intact: bool,
    /// Offset of the first mismatching byte, if any.
    pub first_mismatch: Option<u64>,
    /// Bytes past the last whole `io`-sized append: a torn tail, which
    /// write-through should make impossible.
    pub torn_tail: u64,
    /// Time to replay the whole log through `vterm`.
    pub replay: Duration,
    /// Bytes replayed.
    pub replayed: u64,
    /// Time to rebuild a screen from `rebuild_bytes` of log.
    pub rebuild: Duration,
    /// Bytes the rebuild replayed.
    pub rebuild_bytes: u64,
}

impl Recovery {
    /// Replay rate over the whole log.
    pub fn replay_mib_per_sec(&self) -> f64 {
        self.replayed as f64 / (1024.0 * 1024.0) / self.replay.as_secs_f64().max(1e-12)
    }
}

/// Write the crash workload to `path` until `bytes`, or until killed.
///
/// This is the child half; it never returns in a normal run.
pub fn crash_writer(path: &Path, bytes: u64, io: usize) -> io::Result<()> {
    let source = parse::workload(SEED_PANE, io.max(4 * 1024 * 1024));
    let mut file = open_writethrough(path, true)?;
    let mut staging = aligned(io);
    let mut written = 0u64;
    let mut at = 0usize;
    while written < bytes {
        if at + io > source.len() {
            at = 0;
        }
        staging.aligned_mut().copy_from_slice(&source[at..at + io]);
        at += io;
        file.write_all(staging.aligned())?;
        written += io as u64;
    }
    Ok(())
}

/// The bytes the child would have written, as one contiguous stream.
fn expected_stream(len: u64, io: usize) -> Vec<u8> {
    let source = parse::workload(SEED_PANE, io.max(4 * 1024 * 1024));
    let mut out = Vec::with_capacity(len as usize);
    let mut at = 0usize;
    while (out.len() as u64) < len {
        if at + io > source.len() {
            at = 0;
        }
        let take = io.min((len - out.len() as u64) as usize);
        out.extend_from_slice(&source[at..at + take]);
        at += io;
    }
    out
}

/// Spawn the writer, kill it mid-write, then check and replay what survived.
pub fn run(exe: &str, config: &Config) -> io::Result<Recovery> {
    if config.io == 0 || config.io % SECTOR != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("append size must be a non-zero multiple of {SECTOR}"),
        ));
    }
    fs::create_dir_all(&config.dir)?;
    let path = config
        .dir
        .join(format!("fwbench-crash-{}.log", std::process::id()));

    let mut child = Command::new(exe)
        .arg("--crash-writer")
        .arg("--path")
        .arg(&path)
        .arg("--mib")
        .arg((config.bytes / (1024 * 1024)).max(1).to_string())
        .arg("--io-kib")
        .arg((config.io / 1024).to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Wait for the writer to actually start before timing it: process startup
    // is tens of milliseconds, and `run_for` is meant to be writing time.
    let started = Instant::now();
    while !path.exists() {
        if started.elapsed() > STARTUP_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the writer produced no log within {:.1} s",
                    STARTUP_LIMIT.as_secs_f64()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(config.run_for);
    // TerminateProcess: no unwinding, no flush, no destructors. As close to a
    // power cut as a test can get without one.
    let killed = child.kill();
    let _ = child.wait();
    killed?;

    let result = inspect(&path, config);
    let _ = fs::remove_file(&path);
    result
}

/// Check a log against the stream that produced it, then replay it.
pub fn inspect(path: &Path, config: &Config) -> io::Result<Recovery> {
    let mut log = Vec::new();
    fs::File::open(path)?.read_to_end(&mut log)?;
    let durable = log.len() as u64;
    let expected = expected_stream(durable, config.io);

    let first_mismatch = log
        .iter()
        .zip(expected.iter())
        .position(|(a, b)| a != b)
        .map(|p| p as u64);
    let intact = first_mismatch.is_none() && durable == expected.len() as u64;
    let torn_tail = durable % config.io as u64;

    let t = Instant::now();
    let mut term = vterm::Term::new(24, 80);
    for chunk in log.chunks(parse::FEED) {
        term.feed(chunk);
    }
    let replay = t.elapsed();
    std::hint::black_box(term.screen().cursor().row);

    let rebuild_bytes = config.rebuild_bytes.min(durable);
    let t = Instant::now();
    let mut term = vterm::Term::new(24, 80);
    for chunk in log[..rebuild_bytes as usize].chunks(parse::FEED) {
        term.feed(chunk);
    }
    let rebuild = t.elapsed();
    std::hint::black_box(term.screen().cursor().row);

    Ok(Recovery {
        durable,
        intact,
        first_mismatch,
        torn_tail,
        replay,
        replayed: durable,
        rebuild,
        rebuild_bytes,
    })
}

/// Render the result as markdown.
pub fn render(config: &Config, r: &Recovery) -> String {
    let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
    let mut o = format!(
        "replay: writer killed after {:.1} s, {} KiB appends, log on {}\n\n",
        config.run_for.as_secs_f64(),
        config.io / 1024,
        config.dir.display()
    );
    o += "| check | result |\n| --- | --- |\n";
    o += &format!("| durable after the kill | {:.1} MiB |\n", mib(r.durable));
    o += &format!(
        "| log matches the stream byte for byte | {} |\n",
        if r.intact {
            "yes".to_string()
        } else {
            format!("NO, first mismatch at {:?}", r.first_mismatch)
        }
    );
    o += &format!(
        "| torn tail (partial append) | {} |\n",
        if r.torn_tail == 0 {
            "none".to_string()
        } else {
            format!("{} bytes", r.torn_tail)
        }
    );
    o += &format!(
        "| replay of the whole log | {:.0} ms, {:.1} MiB/s |\n",
        r.replay.as_secs_f64() * 1000.0,
        r.replay_mib_per_sec()
    );
    o += &format!(
        "| rebuild a screen from {:.1} MiB | {:.1} ms (a 60 Hz frame is 16.7 ms) |\n",
        mib(r.rebuild_bytes),
        r.rebuild.as_secs_f64() * 1000.0
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fwbench-replay-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn cfg(dir: PathBuf) -> Config {
        Config {
            dir,
            bytes: 4 * 1024 * 1024,
            io: 64 * 1024,
            run_for: Duration::from_millis(50),
            rebuild_bytes: 256 * 1024,
        }
    }

    #[test]
    fn a_whole_log_inspects_as_intact() {
        let dir = tmp("intact");
        let c = cfg(dir.clone());
        let path = dir.join("log");
        crash_writer(&path, 512 * 1024, c.io).expect("write");
        let r = inspect(&path, &c).expect("inspect");
        assert_eq!(r.durable, 512 * 1024);
        assert!(r.intact, "first mismatch {:?}", r.first_mismatch);
        assert_eq!(r.torn_tail, 0);
        assert_eq!(r.rebuild_bytes, c.rebuild_bytes);
        assert!(r.replay_mib_per_sec() > 0.0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupted_byte_is_caught() {
        let dir = tmp("corrupt");
        let c = cfg(dir.clone());
        let path = dir.join("log");
        crash_writer(&path, 256 * 1024, c.io).expect("write");
        let mut bytes = fs::read(&path).unwrap();
        bytes[100_000] ^= 0xFF;
        fs::write(&path, &bytes).unwrap();
        let r = inspect(&path, &c).expect("inspect");
        assert!(!r.intact);
        assert_eq!(r.first_mismatch, Some(100_000));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_torn_tail_is_reported() {
        let dir = tmp("torn");
        let c = cfg(dir.clone());
        let path = dir.join("log");
        crash_writer(&path, 128 * 1024, c.io).expect("write");
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 777); // half an append
        fs::write(&path, &bytes).unwrap();
        let r = inspect(&path, &c).expect("inspect");
        assert_eq!(r.torn_tail, c.io as u64 - 777);
        // The surviving bytes still match the stream: a short tail is not a
        // corrupt one.
        assert!(r.first_mismatch.is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_expected_stream_repeats_the_source_the_same_way() {
        let io = 64 * 1024;
        let a = expected_stream(300 * 1024, io);
        let b = expected_stream(300 * 1024, io);
        assert_eq!(a, b);
        assert_eq!(a.len(), 300 * 1024);
        // The first append equals the head of the pane's workload.
        let src = parse::workload(SEED_PANE, 4 * 1024 * 1024);
        assert_eq!(&a[..io], &src[..io]);
    }

    #[test]
    fn unaligned_appends_are_rejected() {
        let mut c = cfg(tmp("bad"));
        c.io = SECTOR + 1;
        assert!(run("fwbench", &c).is_err());
        let _ = fs::remove_dir_all(&c.dir);
    }
}
