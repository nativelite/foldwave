//! Experiment E4, the parse budget (design 8.0), plus the GPU fit.
//!
//! Four questions, each answered on the same agent-like workload as the pane
//! floods ([`crate::emit`]):
//!
//! 1. **The split.** How much of full emulation (`vterm::Term`) is the
//!    tokenizer (`ansi::Parser`), how much is the bare state machine
//!    ([`crate::fsm`]), and how much is applying tokens to the screen? Only
//!    the tokenizer half could move to a GPU; the screen half is sequential.
//! 2. **Scaling.** Full emulation on 1..N threads, one independent pane per
//!    thread, aggregate MiB/s. Core placement (P, P+HT, +E) is set from
//!    outside with a process affinity mask, so this stays free of `unsafe`.
//! 3. **The hidden-pane path.** The structural scan ([`crate::scan`]) per core.
//! 4. **The GPU fit.** How fast do the tokenizer's start states converge, and
//!    where does a chunk boundary actually land? Parallel chunked tokenizing
//!    is cheap only if both answers are favourable.

use crate::fsm::{self, State, Table, ALL, STATES};
use crate::{emit, scan, stats::Summary};
use std::time::{Duration, Instant};

const MIB: f64 = 1024.0 * 1024.0;
/// Bytes handed to the parser per `feed`, as a pty read would.
pub const FEED: usize = 64 * 1024;

/// What to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Workload bytes per measurement (per thread for scaling).
    pub bytes: usize,
    /// Repetitions of each timed measurement.
    pub reps: usize,
    /// Thread counts for the scaling curve; empty skips it.
    pub threads: Vec<usize>,
    /// Chunk boundaries sampled for the convergence study.
    pub offsets: usize,
}

/// The workload for pane `i`: same generator, a different seed per pane.
pub fn workload(i: usize, bytes: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(bytes + 512);
    emit::Generator::new(0xF01D_2A7E ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        .fill(&mut v, bytes);
    v
}

/// One timed stage: per-rep durations over `bytes`.
#[derive(Debug, Clone)]
pub struct Rate {
    /// Stage name.
    pub name: &'static str,
    /// Bytes processed per rep.
    pub bytes: usize,
    /// Duration of each rep.
    pub reps: Vec<Duration>,
}

impl Rate {
    /// MiB/s at a duration.
    fn at(&self, d: Duration) -> f64 {
        self.bytes as f64 / MIB / d.as_secs_f64().max(1e-12)
    }
    /// Median rate.
    pub fn median(&self) -> f64 {
        self.at(Summary::of(&self.reps).p50)
    }
    /// The slow tail: the rate at the p90 duration.
    pub fn p90_slow(&self) -> f64 {
        self.at(Summary::of(&self.reps).p90)
    }
}

fn time<F: FnMut()>(name: &'static str, bytes: usize, reps: usize, mut f: F) -> Rate {
    f(); // warm-up, not recorded
    let reps = (0..reps)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    Rate { name, bytes, reps }
}

/// Stage 1: the split, single thread.
pub fn split(data: &[u8], reps: usize) -> Vec<Rate> {
    let table = Table::new();
    let n = data.len();
    let mut sink = 0usize;
    let mut rates = vec![
        time("vterm::Term::feed (tokenize + screen)", n, reps, || {
            let mut term = vterm::Term::new(24, 80);
            for c in data.chunks(FEED) {
                term.feed(c);
            }
            sink ^= term.screen().cursor().row;
        }),
        time(
            "ansi::Parser::feed (tokenize, allocating tokens)",
            n,
            reps,
            || {
                let mut p = ansi::Parser::new();
                for c in data.chunks(FEED) {
                    sink ^= p.feed(c).len();
                }
            },
        ),
        time(
            "fsm table, 1 trajectory (state machine only)",
            n,
            reps,
            || {
                sink ^= table.run(State::Ground as u8, data) as usize;
            },
        ),
        time(
            "fsm table, all 11 start states in lockstep",
            n,
            reps,
            || {
                sink ^= table.run_all(data)[0] as usize;
            },
        ),
    ];
    rates.push(time("structural scan (lines + OSC 133)", n, reps, || {
        let mut s = scan::Structure::default();
        let mut base = 0u64;
        for c in data.chunks(FEED) {
            scan::scan(c, base, &mut s);
            base += c.len() as u64;
        }
        sink ^= s.line_starts.len();
    }));
    std::hint::black_box(sink);
    rates
}

/// Stage 2: full emulation on `threads` threads, one pane each. Returns the
/// per-rep wall time over `threads * bytes` total.
pub fn scaling(threads: usize, bytes: usize, reps: usize) -> Rate {
    let panes: Vec<Vec<u8>> = (0..threads).map(|i| workload(i, bytes)).collect();
    let total = panes.iter().map(Vec::len).sum();
    time("vterm, one pane per thread", total, reps, || {
        std::thread::scope(|s| {
            for p in &panes {
                s.spawn(move || {
                    let mut term = vterm::Term::new(24, 80);
                    for c in p.chunks(FEED) {
                        term.feed(c);
                    }
                    std::hint::black_box(term.screen().cursor().row);
                });
            }
        });
    })
}

/// Stage 4: convergence of each start state with the ground trajectory.
#[derive(Debug, Clone)]
pub struct Convergence {
    /// Chunk boundaries sampled.
    pub offsets: usize,
    /// Bytes each state ran before it would give up (the chunk length).
    pub horizon: usize,
    /// Per start state: convergence distance in bytes for each offset that
    /// converged, and how many did not within `horizon`.
    pub per_state: Vec<(State, Vec<u32>, usize)>,
    /// Where a boundary truly lands: count of offsets in each state.
    pub landing: [u64; STATES],
}

/// Sample `offsets` boundaries evenly through `data` and measure, from each
/// start state, how many bytes until it meets the ground trajectory.
pub fn convergence(data: &[u8], offsets: usize, horizon: usize) -> Convergence {
    let table = Table::new();
    let stride = (data.len().saturating_sub(horizon) / offsets.max(1)).max(1);
    let mut per_state: Vec<(State, Vec<u32>, usize)> =
        ALL.iter().map(|&s| (s, Vec::new(), 0)).collect();
    for k in 0..offsets {
        let at = (k * stride + k % 97) % data.len().saturating_sub(horizon).max(1);
        let window = &data[at..(at + horizon).min(data.len())];
        for (s, hits, misses) in per_state.iter_mut() {
            match fsm::converge_with_ground(&table, *s, window) {
                Some(d) => hits.push(d as u32),
                None => *misses += 1,
            }
        }
    }
    Convergence {
        offsets,
        horizon,
        per_state,
        landing: fsm::state_histogram(&table, data),
    }
}

fn pct(sorted: &[u32], q: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((q / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Render the whole E4 result as markdown.
pub fn render(
    cfg: &Config,
    split: &[Rate],
    scaling: &[(usize, Rate)],
    conv: &Convergence,
) -> String {
    let mut o = String::new();
    o += &format!(
        "E4 parse budget: {:.0} MiB agent-like workload, {} reps per cell (after 1 warm-up), feed {} KiB\n\n",
        cfg.bytes as f64 / MIB,
        cfg.reps,
        FEED / 1024
    );
    o += "| stage (1 thread) | median MiB/s | p90-slow MiB/s | share of vterm time |\n| --- | --- | --- | --- |\n";
    let full = split.first().map(|r| r.median()).unwrap_or(0.0);
    for r in split {
        o += &format!(
            "| {} | {:.1} | {:.1} | {:.0} % |\n",
            r.name,
            r.median(),
            r.p90_slow(),
            100.0 * full / r.median()
        );
    }
    if !scaling.is_empty() {
        o += "\n| threads | aggregate median MiB/s | p90-slow | per thread |\n| --- | --- | --- | --- |\n";
        for (t, r) in scaling {
            o += &format!(
                "| {} | {:.1} | {:.1} | {:.1} |\n",
                t,
                r.median(),
                r.p90_slow(),
                r.median() / *t as f64
            );
        }
    }
    o += &format!(
        "\nGPU fit: {} chunk boundaries, horizon {} KiB\n\n| start state | converged | p50 bytes | p99 bytes | max bytes | never (within horizon) |\n| --- | --- | --- | --- | --- | --- |\n",
        conv.offsets,
        conv.horizon / 1024
    );
    for (s, hits, misses) in &conv.per_state {
        let mut h = hits.clone();
        h.sort_unstable();
        o += &format!(
            "| {:?} | {} | {} | {} | {} | {} |\n",
            s,
            h.len(),
            pct(&h, 50.0),
            pct(&h, 99.0),
            h.last().copied().unwrap_or(0),
            misses
        );
    }
    let total: u64 = conv.landing.iter().sum();
    o += "\n| where a boundary truly lands | share of byte offsets |\n| --- | --- |\n";
    for s in ALL {
        let c = conv.landing[s as usize];
        if c > 0 {
            o += &format!("| {:?} | {:.4} % |\n", s, 100.0 * c as f64 / total as f64);
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workloads_differ_per_pane_and_repeat_per_seed() {
        assert_eq!(workload(3, 4096), workload(3, 4096));
        assert_ne!(workload(0, 4096), workload(1, 4096));
        assert!(workload(0, 4096).len() >= 4096);
    }

    #[test]
    fn split_and_scaling_run_small() {
        let data = workload(0, 64 * 1024);
        let s = split(&data, 2);
        assert_eq!(s.len(), 5);
        assert!(s.iter().all(|r| r.reps.len() == 2 && r.median() > 0.0));
        let r = scaling(2, 16 * 1024, 1);
        assert!(r.bytes >= 32 * 1024);
    }

    #[test]
    fn convergence_on_agent_output() {
        let data = workload(0, 256 * 1024);
        let c = convergence(&data, 50, 4096);
        // Ground converges with itself immediately.
        let (_, g, miss) = &c.per_state[State::Ground as usize];
        assert_eq!(*miss, 0);
        assert!(g.iter().all(|&d| d == 0));
        // The workload has no OSC strings, so a string start state absorbs
        // it and never meets ground.
        let (_, _, osc_miss) = &c.per_state[State::Osc as usize];
        assert_eq!(*osc_miss, 50);
        assert_eq!(c.landing.iter().sum::<u64>(), data.len() as u64);
    }

    #[test]
    fn render_has_every_section() {
        let data = workload(0, 32 * 1024);
        let cfg = Config {
            bytes: data.len(),
            reps: 1,
            threads: vec![1],
            offsets: 10,
        };
        let out = render(
            &cfg,
            &split(&data, 1),
            &[(1, scaling(1, 8 * 1024, 1))],
            &convergence(&data, 10, 1024),
        );
        for needle in [
            "stage (1 thread)",
            "threads",
            "GPU fit",
            "truly lands",
            "Csi",
        ] {
            assert!(out.contains(needle), "missing {needle}");
        }
    }
}
