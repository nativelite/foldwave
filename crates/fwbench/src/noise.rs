//! The noisy-run guard: how much CPU did *other* processes use during a run?
//!
//! A benchmark on a machine that is also running a browser, an indexer or an
//! agent measures the sum of both. `standards/performance.md` wants numbers
//! from a quiet machine, and "quiet" is a measurement, not a feeling. So every
//! run records the CPU time consumed outside fwbench's own process tree, via
//! two `resmon` snapshots, and says whether it exceeded a limit.
//!
//! Counting rules:
//! - Processes are matched across the snapshots by `(pid, start_time)`, so a
//!   reused pid is never mistaken for the same process.
//! - A process that started during the run has all its CPU counted. One that
//!   started *and* exited inside the run is invisible to both snapshots and
//!   undercounted; the report says so.
//! - fwbench itself and every descendant (flood panes, the echo shell) are
//!   excluded, as is the idle process (pid 0).
//! - Processes that could not be opened read zero; they are counted and
//!   reported, never silently treated as idle.

use resmon::{RawProc, Snapshot};
use std::collections::{HashMap, HashSet};

/// The default limit: 5 % of one logical core.
pub const DEFAULT_LIMIT_CORES: f64 = 0.05;

/// What the rest of the machine did during a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Noise {
    /// Wall time between the snapshots.
    pub elapsed_secs: f64,
    /// CPU seconds consumed by processes outside fwbench's tree.
    pub other_cpu_secs: f64,
    /// `other_cpu_secs / elapsed_secs`: the average number of busy cores.
    pub other_cores: f64,
    /// Logical CPUs on the machine.
    pub logical_cpus: u32,
    /// The heaviest programs, by name, in cores, heaviest first.
    pub top: Vec<(String, f64)>,
    /// Processes whose counters could not be read.
    pub unreadable: usize,
    /// The limit the verdict was judged against, in cores.
    pub limit_cores: f64,
}

impl Noise {
    /// True if other processes used more than the limit.
    pub fn noisy(&self) -> bool {
        self.other_cores > self.limit_cores
    }
}

/// Every pid in `root`'s tree (root included) in `procs`, following
/// `parent_pid` links. A child must not predate its parent, which rules out
/// links through a reused parent pid. This orders `start_time`, which on
/// Windows is the `GetProcessTimes` creation FILETIME, so ordering is
/// meaningful. An unreadable process has `start_time` 0 and so drops out of
/// the tree, but it contributes no CPU either way and is reported as
/// unreadable.
fn tree(procs: &[RawProc], root: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<&RawProc>> = HashMap::new();
    for p in procs {
        children.entry(p.parent_pid).or_default().push(p);
    }
    let started: HashMap<u32, u64> = procs.iter().map(|p| (p.pid, p.start_time)).collect();
    let mut out = HashSet::from([root]);
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        let parent_start = started.get(&pid).copied().unwrap_or(0);
        for c in children.get(&pid).into_iter().flatten() {
            if c.pid != pid && c.start_time >= parent_start && out.insert(c.pid) {
                stack.push(c.pid);
            }
        }
    }
    out
}

/// Attribute the CPU between `prev` and `cur` to everything except `self_pid`
/// and its descendants.
pub fn measure(
    prev: &Snapshot,
    cur: &Snapshot,
    self_pid: u32,
    elapsed_secs: f64,
    limit_cores: f64,
) -> Noise {
    let mut ours = tree(&prev.procs, self_pid);
    ours.extend(tree(&cur.procs, self_pid));

    let before: HashMap<(u32, u64), u64> = prev
        .procs
        .iter()
        .map(|p| ((p.pid, p.start_time), p.cpu_ns()))
        .collect();

    let mut by_name: HashMap<String, u64> = HashMap::new();
    let mut total_ns = 0u64;
    let mut unreadable = 0usize;
    for p in &cur.procs {
        if p.pid == 0 || ours.contains(&p.pid) {
            continue;
        }
        if p.denied {
            unreadable += 1;
            continue;
        }
        let was = before.get(&(p.pid, p.start_time)).copied().unwrap_or(0);
        let used = p.cpu_ns().saturating_sub(was);
        if used > 0 {
            total_ns += used;
            *by_name.entry(p.name.clone()).or_default() += used;
        }
    }

    let elapsed = elapsed_secs.max(1e-9);
    let mut top: Vec<(String, f64)> = by_name
        .into_iter()
        .map(|(n, ns)| (n, ns as f64 / 1e9 / elapsed))
        .collect();
    top.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top.truncate(5);

    let other_cpu_secs = total_ns as f64 / 1e9;
    Noise {
        elapsed_secs,
        other_cpu_secs,
        other_cores: other_cpu_secs / elapsed,
        logical_cpus: cur.system.logical_cpus,
        top,
        unreadable,
        limit_cores,
    }
}

/// One paragraph for the end of a report.
pub fn render(n: &Noise) -> String {
    let share = if n.logical_cpus > 0 {
        100.0 * n.other_cores / n.logical_cpus as f64
    } else {
        0.0
    };
    let top: Vec<String> = n
        .top
        .iter()
        .map(|(name, cores)| format!("{name} {cores:.2}"))
        .collect();
    let mut o = format!(
        "\nnoise: other processes used {:.2} cores ({:.1}% of {} logical CPUs) over {:.1} s: {} (limit {:.2} core)\n",
        n.other_cores,
        share,
        n.logical_cpus,
        n.elapsed_secs,
        if n.noisy() { "NOISY" } else { "quiet" },
        n.limit_cores,
    );
    if !top.is_empty() {
        o += &format!("noise: heaviest, in cores: {}\n", top.join(", "));
    }
    if n.unreadable > 0 {
        o += &format!(
            "noise: {} process(es) could not be read and count as zero; short-lived processes are undercounted\n",
            n.unreadable
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use resmon::SystemInfo;

    fn p(pid: u32, parent: u32, name: &str, start: u64, cpu_ms: u64) -> RawProc {
        RawProc {
            pid,
            parent_pid: parent,
            name: name.into(),
            start_time: start,
            cpu_kernel_ns: 0,
            cpu_user_ns: cpu_ms * 1_000_000,
            working_set: 0,
            private_bytes: 0,
            io_read_bytes: 0,
            io_write_bytes: 0,
            io_read_ops: 0,
            io_write_ops: 0,
            denied: false,
        }
    }

    fn snap(procs: Vec<RawProc>) -> Snapshot {
        Snapshot {
            procs,
            system: SystemInfo {
                logical_cpus: 16,
                mem_total: 1,
                mem_available: 1,
            },
            gpu: None,
        }
    }

    #[test]
    fn excludes_self_descendants_and_idle() {
        let prev = snap(vec![
            p(0, 0, "Idle", 0, 0),
            p(10, 1, "fwbench.exe", 100, 0),
            p(11, 10, "cmd.exe", 110, 0),
            p(12, 11, "fwbench.exe", 120, 0),
            p(20, 1, "browser.exe", 50, 0),
        ]);
        let cur = snap(vec![
            p(0, 0, "Idle", 0, 90_000),
            p(10, 1, "fwbench.exe", 100, 5_000),
            p(11, 10, "cmd.exe", 110, 1_000),
            p(12, 11, "fwbench.exe", 120, 4_000),
            p(20, 1, "browser.exe", 50, 2_000),
        ]);
        let n = measure(&prev, &cur, 10, 2.0, DEFAULT_LIMIT_CORES);
        assert_eq!(n.other_cpu_secs, 2.0);
        assert_eq!(n.other_cores, 1.0);
        assert_eq!(n.top, vec![("browser.exe".to_string(), 1.0)]);
        assert!(n.noisy());
    }

    #[test]
    fn groups_by_name_and_counts_new_processes_fully() {
        let prev = snap(vec![p(20, 1, "chrome.exe", 50, 1_000)]);
        let cur = snap(vec![
            p(20, 1, "chrome.exe", 50, 1_400),
            p(21, 20, "chrome.exe", 500, 600), // started during the run
            p(30, 1, "defender.exe", 40, 100), // absent from prev: counted fully
        ]);
        let n = measure(&prev, &cur, 999, 1.0, DEFAULT_LIMIT_CORES);
        assert!((n.other_cpu_secs - 1.1).abs() < 1e-9);
        assert_eq!(n.top[0].0, "chrome.exe");
        assert!((n.top[0].1 - 1.0).abs() < 1e-9);
        assert_eq!(n.top[1].0, "defender.exe");
    }

    #[test]
    fn reused_pid_is_a_new_process() {
        // pid 20 exited and was reused: its counter restarts from zero.
        let prev = snap(vec![p(20, 1, "old.exe", 50, 9_000)]);
        let cur = snap(vec![p(20, 1, "new.exe", 700, 300)]);
        let n = measure(&prev, &cur, 999, 1.0, DEFAULT_LIMIT_CORES);
        assert!((n.other_cpu_secs - 0.3).abs() < 1e-9);
    }

    #[test]
    fn child_older_than_parent_is_not_a_descendant() {
        // pid 10 is ours, but pid 11 names 10 as parent while predating it: the
        // parent pid was reused, so 11 is someone else's.
        let prev = snap(vec![
            p(10, 1, "fwbench.exe", 100, 0),
            p(11, 10, "svc.exe", 5, 0),
        ]);
        let cur = snap(vec![
            p(10, 1, "fwbench.exe", 100, 0),
            p(11, 10, "svc.exe", 5, 500),
        ]);
        let n = measure(&prev, &cur, 10, 1.0, DEFAULT_LIMIT_CORES);
        assert!((n.other_cpu_secs - 0.5).abs() < 1e-9);
    }

    #[test]
    fn unreadable_processes_are_reported_not_counted() {
        let mut locked = p(40, 1, "system.exe", 1, 0);
        locked.denied = true;
        let cur = snap(vec![locked, p(20, 1, "a.exe", 50, 10)]);
        let n = measure(&snap(vec![]), &cur, 999, 1.0, 0.5);
        assert_eq!(n.unreadable, 1);
        assert!(!n.noisy());
        let text = render(&n);
        assert!(text.contains("quiet"));
        assert!(text.contains("1 process(es) could not be read"));
    }

    #[test]
    fn verdict_is_strictly_above_the_limit() {
        let cur = snap(vec![p(20, 1, "a.exe", 50, 50)]);
        let n = measure(&snap(vec![p(20, 1, "a.exe", 50, 0)]), &cur, 999, 1.0, 0.05);
        assert!(!n.noisy(), "exactly at the limit is not noisy");
        let text = render(&n);
        assert!(text.contains("0.05 cores (0.3% of 16 logical CPUs)"));
        assert!(text.contains("heaviest, in cores: a.exe 0.05"));
    }
}
