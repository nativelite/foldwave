//! Experiment E5: the legs running together (design §8.0).
//!
//! §4.1 measured each leg alone. §8.0 then composed them on paper. This runs
//! the composition: N panes, each pushing agent-like bytes through the whole
//! ingest path at once —
//!
//! 1. **source** a chunk (pre-generated, so the generator's own cost is not
//!    measured here — [`crate::flood`] covers the transport leg),
//! 2. **log** it durably (unbuffered write-through, the pattern `disk`
//!    measured fastest),
//! 3. **parse** it into a screen if the pane is visible, or **scan** it
//!    structurally if it is hidden (§8.0's central claim: hidden panes never
//!    run the state machine).
//!
//! Every stage is timed inside each pane's own thread, so the report says
//! which leg the time actually went to rather than inferring it.
//!
//! The log layout is the open question in §8.4: one file per pane, or one
//! device-level log with pane-tagged records. [`LogMode`] runs either, so the
//! two can be compared on the same load.

use crate::disk::{aligned, open_writethrough, SECTOR};
use crate::stats::Summary;
use crate::{parse, scan};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Where each pane's bytes are logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogMode {
    /// No log at all: the control, isolating parse and scan.
    None,
    /// One file per pane, as §8.4 specifies.
    PerPane,
    /// One device-level log, appends serialized across panes. Each write
    /// carries a [`SECTOR`]-sized header naming the pane, so records stay
    /// aligned for unbuffered I/O; that costs one sector per write.
    Single,
}

impl LogMode {
    /// Parse the CLI spelling.
    pub fn parse(s: &str) -> Option<LogMode> {
        match s {
            "none" => Some(LogMode::None),
            "per-pane" => Some(LogMode::PerPane),
            "single" => Some(LogMode::Single),
            _ => None,
        }
    }

    /// The CLI spelling.
    pub fn name(self) -> &'static str {
        match self {
            LogMode::None => "none",
            LogMode::PerPane => "per-pane",
            LogMode::Single => "single",
        }
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Panes running at once.
    pub panes: usize,
    /// How many of them are visible, and so fully parsed. The rest are hidden
    /// and only structurally scanned.
    pub visible: usize,
    /// Bytes each pane pushes through.
    pub bytes_per_pane: u64,
    /// Log layout.
    pub log: LogMode,
    /// Directory for the logs; files are removed afterwards.
    pub dir: PathBuf,
    /// Bytes per log write; a multiple of [`SECTOR`].
    pub io: usize,
    /// Aggregate MiB/s to pace at, or 0 to run flat out.
    pub rate_mib: f64,
}

/// One pane's result.
#[derive(Debug, Clone)]
pub struct PaneStat {
    /// Pane index.
    pub pane: usize,
    /// Parsed into a screen (visible) or only scanned (hidden).
    pub visible: bool,
    /// Bytes pushed through.
    pub bytes: u64,
    /// Wall time for this pane.
    pub elapsed: Duration,
    /// Time inside log writes.
    pub log_time: Duration,
    /// Time inside `vterm`, for a visible pane.
    pub parse_time: Duration,
    /// Time inside the structural scan, for a hidden pane.
    pub scan_time: Duration,
    /// Time asleep to hold the requested rate.
    pub paced_time: Duration,
    /// Latency of each log write.
    pub writes: Vec<Duration>,
}

impl PaneStat {
    /// Sustained MiB/s for this pane.
    pub fn mib_per_sec(&self) -> f64 {
        self.bytes as f64 / (1024.0 * 1024.0) / self.elapsed.as_secs_f64().max(1e-12)
    }
}

/// The whole run.
#[derive(Debug, Clone)]
pub struct Composed {
    /// The configuration that produced it.
    pub log: LogMode,
    /// Panes, visible first.
    pub panes: Vec<PaneStat>,
    /// Wall time of the timed region.
    pub wall: Duration,
    /// Bytes through every pane.
    pub bytes: u64,
}

impl Composed {
    /// Aggregate MiB/s across all panes.
    pub fn mib_per_sec(&self) -> f64 {
        self.bytes as f64 / (1024.0 * 1024.0) / self.wall.as_secs_f64().max(1e-12)
    }

    /// Total time spent in a stage across panes, as a share of all pane time.
    pub fn share(&self, stage: fn(&PaneStat) -> Duration) -> f64 {
        let total: f64 = self.panes.iter().map(|p| p.elapsed.as_secs_f64()).sum();
        let part: f64 = self.panes.iter().map(|p| stage(p).as_secs_f64()).sum();
        if total <= 0.0 {
            0.0
        } else {
            100.0 * part / total
        }
    }
}

/// The shared log, when [`LogMode::Single`] is in force.
struct SharedLog {
    file: Mutex<File>,
    header: Vec<u8>,
}

impl SharedLog {
    fn new(path: &Path) -> io::Result<SharedLog> {
        Ok(SharedLog {
            file: Mutex::new(open_writethrough(path, true)?),
            header: vec![0u8; SECTOR],
        })
    }

    /// Append one pane-tagged record. The header block keeps every payload
    /// sector-aligned, which unbuffered I/O requires.
    fn append(&self, pane: usize, payload: &[u8]) -> io::Result<Duration> {
        let mut header = self.header.clone();
        header[..8].copy_from_slice(&(pane as u64).to_le_bytes());
        header[8..16].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        let t = Instant::now();
        let mut file = self.file.lock().expect("log mutex");
        file.write_all(&header)?;
        file.write_all(payload)?;
        Ok(t.elapsed())
    }
}

/// Run the composed pipeline.
pub fn run(config: &Config) -> io::Result<Composed> {
    if config.io == 0 || config.io % SECTOR != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("log write size must be a non-zero multiple of {SECTOR}"),
        ));
    }
    if config.panes == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "need at least one pane",
        ));
    }
    fs::create_dir_all(&config.dir)?;

    // Sources are generated up front: E5 measures the pipeline, not the
    // generator, which `flood` already covers.
    let sources: Vec<Vec<u8>> = (0..config.panes)
        .map(|i| parse::workload(i, config.io.max(4 * 1024 * 1024)))
        .collect();

    let single = match config.log {
        LogMode::Single => Some(SharedLog::new(&log_path(&config.dir, None))?),
        _ => None,
    };

    let per_pane_rate = if config.rate_mib > 0.0 {
        config.rate_mib * 1024.0 * 1024.0 / config.panes as f64
    } else {
        0.0
    };

    let started = Instant::now();
    let stats: Vec<io::Result<PaneStat>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..config.panes)
            .map(|pane| {
                let src = &sources[pane];
                let single = single.as_ref();
                let cfg = config;
                scope.spawn(move || pane_worker(pane, src, cfg, single, per_pane_rate))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("pane"))
            .collect()
    });
    let wall = started.elapsed();

    drop(single);
    cleanup(&config.dir, config.panes, config.log);

    let mut panes = Vec::with_capacity(config.panes);
    for s in stats {
        panes.push(s?);
    }
    let bytes = panes.iter().map(|p| p.bytes).sum();
    Ok(Composed {
        log: config.log,
        panes,
        wall,
        bytes,
    })
}

fn log_path(dir: &Path, pane: Option<usize>) -> PathBuf {
    match pane {
        Some(p) => dir.join(format!(
            "fwbench-compose-{}-pane{p}.log",
            std::process::id()
        )),
        None => dir.join(format!("fwbench-compose-{}-device.log", std::process::id())),
    }
}

fn cleanup(dir: &Path, panes: usize, log: LogMode) {
    match log {
        LogMode::None => {}
        LogMode::Single => {
            let _ = fs::remove_file(log_path(dir, None));
        }
        LogMode::PerPane => {
            for p in 0..panes {
                let _ = fs::remove_file(log_path(dir, Some(p)));
            }
        }
    }
}

fn pane_worker(
    pane: usize,
    src: &[u8],
    config: &Config,
    single: Option<&SharedLog>,
    per_pane_rate: f64,
) -> io::Result<PaneStat> {
    let visible = pane < config.visible;
    let io = config.io;

    let mut own_file = match config.log {
        LogMode::PerPane => Some(open_writethrough(&log_path(&config.dir, Some(pane)), true)?),
        _ => None,
    };
    // Unbuffered writes need a sector-aligned buffer, so each chunk is copied
    // into one. At RAM speed that copy is noise next to the other stages.
    let mut staging = aligned(io);
    let mut term = visible.then(|| vterm::Term::new(24, 80));
    let mut structure = scan::Structure::default();

    let mut stat = PaneStat {
        pane,
        visible,
        bytes: 0,
        elapsed: Duration::ZERO,
        log_time: Duration::ZERO,
        parse_time: Duration::ZERO,
        scan_time: Duration::ZERO,
        paced_time: Duration::ZERO,
        writes: Vec::new(),
    };

    let started = Instant::now();
    let mut at = 0usize;
    while stat.bytes < config.bytes_per_pane {
        let want = io.min((config.bytes_per_pane - stat.bytes) as usize);
        // Replay the source cyclically; a chunk never straddles the end.
        if at + want > src.len() {
            at = 0;
        }
        let chunk = &src[at..at + want];
        at += want;

        if config.log != LogMode::None {
            let payload = &mut staging.aligned_mut()[..want];
            payload.copy_from_slice(chunk);
            let padded = want.next_multiple_of(SECTOR).min(io);
            let out = &staging.aligned()[..padded];
            let took = match (&mut own_file, single) {
                (Some(file), _) => {
                    let t = Instant::now();
                    file.write_all(out)?;
                    t.elapsed()
                }
                (None, Some(log)) => log.append(pane, out)?,
                (None, None) => Duration::ZERO,
            };
            stat.log_time += took;
            stat.writes.push(took);
        }

        if let Some(term) = term.as_mut() {
            let t = Instant::now();
            term.feed(chunk);
            stat.parse_time += t.elapsed();
        } else {
            let t = Instant::now();
            structure.line_starts.clear();
            structure.marks.clear();
            scan::scan(chunk, stat.bytes, &mut structure);
            stat.scan_time += t.elapsed();
        }

        stat.bytes += want as u64;

        if per_pane_rate > 0.0 {
            let due = Duration::from_secs_f64(stat.bytes as f64 / per_pane_rate);
            if let Some(sleep) = due.checked_sub(started.elapsed()) {
                let t = Instant::now();
                std::thread::sleep(sleep);
                stat.paced_time += t.elapsed();
            }
        }
    }
    stat.elapsed = started.elapsed();
    if let Some(term) = term.as_ref() {
        std::hint::black_box(term.screen().cursor().row);
    }
    std::hint::black_box(structure.line_starts.len());
    Ok(stat)
}

/// Render one run as markdown.
pub fn render(config: &Config, run: &Composed) -> String {
    let visible: Vec<&PaneStat> = run.panes.iter().filter(|p| p.visible).collect();
    let hidden: Vec<&PaneStat> = run.panes.iter().filter(|p| !p.visible).collect();
    let writes: Vec<Duration> = run.panes.iter().flat_map(|p| p.writes.clone()).collect();
    let w = Summary::of(&writes);

    let mut o = format!(
        "compose: {} panes ({} visible, {} hidden), {:.0} MiB each, log {}, {} KiB writes{}\n\n",
        config.panes,
        visible.len(),
        hidden.len(),
        config.bytes_per_pane as f64 / (1024.0 * 1024.0),
        run.log.name(),
        config.io / 1024,
        if config.rate_mib > 0.0 {
            format!(", paced at {:.0} MiB/s", config.rate_mib)
        } else {
            String::new()
        }
    );
    o += &format!(
        "| aggregate | vs 273 MiB/s target | wall | log write p50 | p90 | p99 | writes |\n| --- | --- | --- | --- | --- | --- | --- |\n| {:.1} MiB/s | {:.2}x | {:.2} s | {} | {} | {} | {} |\n\n",
        run.mib_per_sec(),
        run.mib_per_sec() / 273.0,
        run.wall.as_secs_f64(),
        crate::stats::ms(w.p50),
        crate::stats::ms(w.p90),
        crate::stats::ms(w.p99),
        w.count,
    );
    o += "| stage | share of pane time |\n| --- | --- |\n";
    o += &format!("| log write | {:.1} % |\n", run.share(|p| p.log_time));
    o += &format!(
        "| parse (visible) | {:.1} % |\n",
        run.share(|p| p.parse_time)
    );
    o += &format!("| scan (hidden) | {:.1} % |\n", run.share(|p| p.scan_time));
    o += &format!("| paced idle | {:.1} % |\n\n", run.share(|p| p.paced_time));

    let group = |name: &str, panes: &[&PaneStat]| -> String {
        if panes.is_empty() {
            return String::new();
        }
        let rates: Vec<f64> = panes.iter().map(|p| p.mib_per_sec()).collect();
        let lo = rates.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = rates.iter().cloned().fold(0.0, f64::max);
        format!(
            "| {} | {} | {:.1} | {:.1} | {:.1} |\n",
            name,
            panes.len(),
            rates.iter().sum::<f64>(),
            lo,
            hi
        )
    };
    o += "| panes | n | aggregate MiB/s | slowest | fastest |\n| --- | --- | --- | --- | --- |\n";
    o += &group("visible (parsed)", &visible);
    o += &group("hidden (scanned)", &hidden);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fwbench-compose-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn small(dir: PathBuf, log: LogMode) -> Config {
        Config {
            panes: 4,
            visible: 2,
            bytes_per_pane: 2 * 1024 * 1024,
            log,
            dir,
            io: 512 * 1024,
            rate_mib: 0.0,
        }
    }

    #[test]
    fn every_pane_pushes_its_bytes_through_each_log_mode() {
        for mode in [LogMode::None, LogMode::PerPane, LogMode::Single] {
            let dir = tmp(mode.name());
            let cfg = small(dir.clone(), mode);
            let run = run(&cfg).expect("compose");
            assert_eq!(run.panes.len(), 4, "{mode:?}");
            assert_eq!(run.bytes, 4 * cfg.bytes_per_pane, "{mode:?}");
            assert!(run.mib_per_sec() > 0.0);
            assert_eq!(run.panes.iter().filter(|p| p.visible).count(), 2);
            // Visible panes parse, hidden panes scan, and never the reverse.
            for p in &run.panes {
                if p.visible {
                    assert!(p.parse_time > Duration::ZERO && p.scan_time.is_zero());
                } else {
                    assert!(p.scan_time > Duration::ZERO && p.parse_time.is_zero());
                }
                assert_eq!(p.writes.is_empty(), mode == LogMode::None);
            }
            let left = fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
            assert_eq!(left, 0, "{mode:?} left log files behind");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn pacing_holds_the_requested_rate() {
        let mut cfg = small(tmp("paced"), LogMode::None);
        cfg.bytes_per_pane = 1024 * 1024;
        cfg.io = 64 * 1024;
        cfg.rate_mib = 8.0; // 4 MiB total at 8 MiB/s ~ 0.5 s
        let run = run(&cfg).expect("compose");
        assert!(run.mib_per_sec() <= 9.0, "{} MiB/s", run.mib_per_sec());
        assert!(run.panes.iter().all(|p| p.paced_time > Duration::ZERO));
        let _ = fs::remove_dir_all(&cfg.dir);
    }

    #[test]
    fn rejects_unaligned_writes_and_zero_panes() {
        let mut cfg = small(tmp("bad"), LogMode::PerPane);
        cfg.io = SECTOR + 1;
        assert!(run(&cfg).is_err());
        let mut cfg = small(tmp("bad2"), LogMode::None);
        cfg.panes = 0;
        assert!(run(&cfg).is_err());
    }

    #[test]
    fn render_reports_every_section() {
        let cfg = small(tmp("render"), LogMode::PerPane);
        let out = render(&cfg, &run(&cfg).expect("compose"));
        for needle in [
            "compose:",
            "aggregate",
            "log write",
            "hidden (scanned)",
            "per-pane",
        ] {
            assert!(out.contains(needle), "missing {needle}");
        }
        let _ = fs::remove_dir_all(&cfg.dir);
    }

    #[test]
    fn log_modes_parse_and_name_round_trip() {
        for m in [LogMode::None, LogMode::PerPane, LogMode::Single] {
            assert_eq!(LogMode::parse(m.name()), Some(m));
        }
        assert_eq!(LogMode::parse("nope"), None);
    }
}
