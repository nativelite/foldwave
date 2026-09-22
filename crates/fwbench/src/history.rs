//! The §4 history target, measured through `panelog`: write a large pane log,
//! then time opening it and jumping to lines anywhere in it.
//!
//! §4: "100 GB per session on disk; open, jump to any line, and search in
//! < 1 s". This covers write, open and jump; search is a later M1 slice.

use crate::emit::Rng;
use crate::parse;
use crate::stats::Summary;
use panelog::{Config, Reader, Writer};
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
}

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
        let mut written = 0u64;
        let mut at = 0usize;
        const CHUNK: usize = 64 * 1024;
        while written < plan.bytes {
            if at + CHUNK > source.len() {
                at = 0;
            }
            let take = CHUNK.min((plan.bytes - written) as usize);
            w.append(&source[at..at + take], epoch.elapsed().as_nanos() as u64)?;
            at += take;
            written += take as u64;
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
    )
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
        };
        let h = run(&plan).expect("history");
        assert_eq!(h.bytes, plan.bytes);
        assert!(h.lines > 1000);
        assert_eq!(h.jumps.count, 50);
        assert!(render(&plan, &h).contains("jump to a random line"));
        assert_eq!(
            std::fs::read_dir(&parent).unwrap().count(),
            0,
            "log removed"
        );
        let _ = std::fs::remove_dir_all(&parent);
    }
}
