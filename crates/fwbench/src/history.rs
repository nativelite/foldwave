//! The §4 history target, measured through `panelog`: write a large pane log,
//! then time opening it and jumping to lines anywhere in it.
//!
//! §4: "100 GB per session on disk; open, jump to any line, and search in
//! < 1 s". This covers write, open and jump; search is a later M1 slice.

use crate::emit::Rng;
use crate::parse;
use crate::stats::Summary;
use panelog::{Config, Reader, SearchOptions, Writer};
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What to run.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Parent directory; the log is created in a fresh subdirectory and
    /// removed afterwards.
    pub dir: PathBuf,
    /// Logical bytes to write.
    pub bytes: u64,
    /// Random line jumps to time.
    pub jumps: usize,
    /// Store a terminal checkpoint every this many bytes (at the next point
    /// the emulator allows one) and time pane rebuilds; 0 skips both, and the
    /// emulator is not run during the write.
    pub checkpoint_every: u64,
}

/// What it measured.
#[derive(Debug, Clone)]
pub struct History {
    /// Logical bytes written.
    pub bytes: u64,
    /// Lines in the log.
    pub lines: u64,
    /// Wall time of the append phase, final flush included.
    pub write: Duration,
    /// Time for `Reader::open` on the finished log.
    pub open: Duration,
    /// Per-jump latency of `Reader::line(n)` for random `n`.
    pub jumps: Summary,
    /// Files in the log directory, and their total size on disk.
    pub files: usize,
    /// Bytes on disk, headers and padding included.
    pub disk_bytes: u64,
    /// Checkpoints stored.
    pub checkpoints: usize,
    /// Per-rebuild latency: nearest checkpoint, restore, replay to the line.
    pub rebuild: Option<Summary>,
    /// Largest replay a rebuild needed, in bytes.
    pub max_replay: u64,
    /// The search queries and what they found.
    pub searches: Vec<Query>,
}

/// One timed search.
#[derive(Debug, Clone)]
pub struct Query {
    /// What the query represents.
    pub label: &'static str,
    /// The needle.
    pub needle: String,
    /// Case-insensitive?
    pub ignore_case: bool,
    /// Result limit.
    pub limit: usize,
    /// Matches returned.
    pub matches: usize,
    /// First run, and a repeat (the repeat has the signatures in RAM).
    pub first: Duration,
    /// Second run.
    pub second: Duration,
}

/// Unique marker lines planted through the log, one of which the rare query
/// looks for.
const MARKERS: u64 = 8;

/// Write the log, open it, jump around it, remove it.
pub fn run(plan: &Plan) -> io::Result<History> {
    let dir = plan
        .dir
        .join(format!("fwbench-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let result = measure(plan, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn measure(plan: &Plan, dir: &std::path::Path) -> io::Result<History> {
    // Pre-generated agent-like output, replayed: the harness measures the log,
    // not the generator.
    let source = parse::workload(0, 8 * 1024 * 1024);
    let epoch = Instant::now();
    let started = Instant::now();
    {
        let mut w = Writer::create(dir, Config::default())?;
        let mut term = vterm::Term::new(24, 80);
        let mut since = 0u64;
        let mut written = 0u64;
        let mut at = 0usize;
        let marker_every = (plan.bytes / MARKERS).max(1);
        let (mut next_marker, mut planted) = (marker_every / 2, 0u64);
        const CHUNK: usize = 64 * 1024;
        while written < plan.bytes {
            if at + CHUNK > source.len() {
                at = 0;
            }
            let take = CHUNK.min((plan.bytes - written) as usize);
            let chunk = &source[at..at + take];
            w.append(chunk, epoch.elapsed().as_nanos() as u64)?;
            if plan.checkpoint_every > 0 {
                term.feed(chunk);
                since += take as u64;
                if since >= plan.checkpoint_every {
                    if let Some(ck) = term.checkpoint() {
                        w.checkpoint(&ck)?;
                        since = 0;
                    }
                }
            }
            at += take;
            written += take as u64;
            if planted < MARKERS && written >= next_marker {
                let line = format!("fwbench-marker-{planted:02} planted here\n");
                w.append(line.as_bytes(), epoch.elapsed().as_nanos() as u64)?;
                planted += 1;
                next_marker += marker_every;
            }
        }
        w.flush()?;
    }
    let write = started.elapsed();

    let t = Instant::now();
    let r = Reader::open(dir)?;
    let open = t.elapsed();
    let lines = r.line_count();

    let mut rng = Rng::new(0x4157_0DE5);
    let mut samples = Vec::with_capacity(plan.jumps);
    for _ in 0..plan.jumps {
        let n = rng.below(lines);
        let t = Instant::now();
        std::hint::black_box(r.line(n)?);
        samples.push(t.elapsed());
    }

    let (mut rebuild, mut max_replay) = (None, 0u64);
    if plan.checkpoint_every > 0 {
        let mut samples = Vec::with_capacity(plan.jumps);
        let mut buf = vec![0u8; 64 * 1024];
        for _ in 0..plan.jumps {
            let n = rng.below(lines);
            let t = Instant::now();
            let target = r.line_start(n)?;
            let (mut pos, mut term) = match r.checkpoint_before(target)? {
                Some((at, blob)) => (
                    at,
                    vterm::Term::restore(&blob)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                ),
                None => (0, vterm::Term::new(24, 80)),
            };
            max_replay = max_replay.max(target - pos);
            while pos < target {
                let want = buf.len().min((target - pos) as usize);
                let got = r.read_at(pos, &mut buf[..want])?;
                if got == 0 {
                    break;
                }
                term.feed(&buf[..got]);
                pos += got as u64;
            }
            std::hint::black_box(term.screen().cursor().row);
            samples.push(t.elapsed());
        }
        rebuild = Some(Summary::of(&samples));
    }

    let queries: [(&'static str, &str, bool, usize); 4] = [
        ("rare: one planted marker", "fwbench-marker-03", false, 1000),
        ("absent", "zq-not-in-this-log-7f3a", false, 1000),
        ("common word, first 1,000", "Compiling", false, 1000),
        ("common word, any case, first 100", "warn", true, 100),
    ];
    let mut searches = Vec::new();
    for (label, needle, ignore_case, limit) in queries {
        let opts = SearchOptions { ignore_case, limit };
        let t = Instant::now();
        let matches = r.search(needle.as_bytes(), &opts)?.len();
        let first = t.elapsed();
        let t = Instant::now();
        std::hint::black_box(r.search(needle.as_bytes(), &opts)?);
        let second = t.elapsed();
        searches.push(Query {
            label,
            needle: needle.to_string(),
            ignore_case,
            limit,
            matches,
            first,
            second,
        });
    }

    let (mut files, mut disk_bytes) = (0usize, 0u64);
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        files += 1;
        disk_bytes += e.metadata()?.len();
    }
    Ok(History {
        bytes: r.len(),
        lines,
        write,
        open,
        jumps: Summary::of(&samples),
        files,
        disk_bytes,
        checkpoints: r.checkpoint_count(),
        rebuild,
        max_replay,
        searches,
    })
}

/// Render as markdown.
pub fn render(plan: &Plan, h: &History) -> String {
    let gib = |b: u64| b as f64 / (1u64 << 30) as f64;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    format!(
        "history: {:.1} GiB through panelog under {}\n\n\
         | measure | result |\n| --- | --- |\n\
         | write (append + final flush) | {:.1} MiB/s over {:.1} s |\n\
         | lines | {} |\n\
         | on disk | {:.2} GiB in {} files ({:.2} % overhead) |\n\
         | open (`Reader::open`) | {:.1} ms |\n\
         | jump to a random line, p50 / p90 / p99 (n = {}) | {:.3} / {:.3} / {:.3} ms |\n\
         | slowest jump | {:.3} ms |\n",
        gib(h.bytes),
        plan.dir.display(),
        h.bytes as f64 / (1024.0 * 1024.0) / h.write.as_secs_f64().max(1e-9),
        h.write.as_secs_f64(),
        h.lines,
        gib(h.disk_bytes),
        h.files,
        100.0 * (h.disk_bytes as f64 - h.bytes as f64) / h.bytes.max(1) as f64,
        ms(h.open),
        h.jumps.count,
        ms(h.jumps.p50),
        ms(h.jumps.p90),
        ms(h.jumps.p99),
        ms(h.jumps.max),
    ) + &match &h.rebuild {
        Some(s) => format!(
            "| checkpoints stored (every {:.1} MiB) | {} |\n\
             | rebuild a 24x80 pane at a random line, p50 / p90 / p99 (n = {}) | {:.2} / {:.2} / {:.2} ms |\n\
             | slowest rebuild; largest replay | {:.2} ms; {:.2} MiB |\n",
            plan.checkpoint_every as f64 / (1024.0 * 1024.0),
            h.checkpoints,
            s.count,
            ms(s.p50),
            ms(s.p90),
            ms(s.p99),
            ms(s.max),
            h.max_replay as f64 / (1024.0 * 1024.0),
        ),
        None => String::new(),
    } + &{
        let mut o = String::new();
        for q in &h.searches {
            o += &format!(
                "| search, {}: `{}` | {} match(es); {:.1} ms, again {:.1} ms |\n",
                q.label,
                q.needle,
                q.matches,
                ms(q.first),
                ms(q.second)
            );
        }
        o
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_history_round_trips_and_cleans_up() {
        let parent = std::env::temp_dir().join("fwbench-history-test");
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).unwrap();
        let plan = Plan {
            dir: parent.clone(),
            bytes: 3 * 1024 * 1024,
            jumps: 50,
            checkpoint_every: 512 * 1024,
        };
        let h = run(&plan).expect("history");
        assert!(h.bytes >= plan.bytes, "markers add a few bytes");
        let rare = &h.searches[0];
        assert_eq!(rare.matches, 1, "exactly one planted marker matches");
        assert_eq!(h.searches[1].matches, 0, "the absent needle");
        assert!(h.lines > 1000);
        assert_eq!(h.jumps.count, 50);
        assert!(h.checkpoints >= 4, "{} checkpoints", h.checkpoints);
        assert_eq!(h.rebuild.as_ref().map(|s| s.count), Some(50));
        assert!(
            h.max_replay <= 512 * 1024 + 64 * 1024 + 4096,
            "{}",
            h.max_replay
        );
        assert!(render(&plan, &h).contains("jump to a random line"));
        assert_eq!(
            std::fs::read_dir(&parent).unwrap().count(),
            0,
            "log removed"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }
}
