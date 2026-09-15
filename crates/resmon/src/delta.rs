//! Turning two snapshots into attributed usage over a window.
//!
//! Pure arithmetic over [`RawProc`] values: no syscalls, no clock. The elapsed
//! time is passed in so every result here is deterministic and testable.

use crate::gpu::GpuSample;
use crate::proc::{classify, Class, RawProc, Tree};
use std::collections::BTreeMap;

/// Machine-wide facts that do not come from any one process.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SystemInfo {
    /// Logical processors the scheduler can run threads on.
    pub logical_cpus: u32,
    /// Total physical memory in bytes.
    pub mem_total: u64,
    /// Physical memory currently available in bytes.
    pub mem_available: u64,
}

/// One instant: every visible process plus machine-wide facts.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Every process visible to the caller at this instant.
    pub procs: Vec<RawProc>,
    /// Machine-wide facts at this instant.
    pub system: SystemInfo,
    /// The GPU reading, or `None` when no sampler ran.
    pub gpu: Option<GpuSample>,
}

/// What one group of processes used over the window.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupUsage {
    /// Human-readable name of the group: a class label or a tree root.
    pub label: String,
    /// Processes that contributed, at their last-seen pid.
    pub pids: Vec<u32>,
    /// CPU seconds consumed during the window.
    pub cpu_seconds: f64,
    /// CPU seconds as a percentage of the whole machine, so 100% means every
    /// logical core was busy with this group for the entire window.
    pub cpu_pct_machine: f64,
    /// Largest working set observed at either end of the window.
    pub peak_working_set: u64,
    /// Largest private committed bytes observed at either end.
    pub peak_private_bytes: u64,
    /// Bytes read during the window. See [`Attribution`] for the caveat.
    pub read_bytes: u64,
    /// Bytes written during the window. See [`Attribution`] for the caveat.
    pub write_bytes: u64,
    /// Read operations during the window.
    pub read_ops: u64,
    /// Write operations during the window.
    pub write_ops: u64,
    /// Video memory attributed to this group at the end of the window.
    pub vram_bytes: u64,
    /// Processes in this group the sampler could not open.
    pub denied: usize,
}

impl GroupUsage {
    fn new(label: String) -> GroupUsage {
        GroupUsage {
            label,
            pids: Vec::new(),
            cpu_seconds: 0.0,
            cpu_pct_machine: 0.0,
            peak_working_set: 0,
            peak_private_bytes: 0,
            read_bytes: 0,
            write_bytes: 0,
            read_ops: 0,
            write_ops: 0,
            vram_bytes: 0,
            denied: 0,
        }
    }

    /// Bytes read per second over the window.
    pub fn read_bytes_per_sec(&self, elapsed_secs: f64) -> f64 {
        per_sec(self.read_bytes, elapsed_secs)
    }

    /// Bytes written per second over the window.
    pub fn write_bytes_per_sec(&self, elapsed_secs: f64) -> f64 {
        per_sec(self.write_bytes, elapsed_secs)
    }
}

fn per_sec(bytes: u64, elapsed_secs: f64) -> f64 {
    if elapsed_secs > 0.0 {
        bytes as f64 / elapsed_secs
    } else {
        0.0
    }
}

/// The result of differencing two snapshots.
///
/// **Caveat on the I/O figures:** the OS counts every kind of read and write a
/// process performs, including pipes, sockets and console handles, not only
/// disk. They attribute I/O *pressure* to a process; they are not a measurement
/// of SSD traffic on their own.
#[derive(Debug, Clone)]
pub struct Attribution {
    /// Wall-clock length of the window, as measured rather than requested.
    pub elapsed_secs: f64,
    /// Logical processors the percentages are normalised against.
    pub logical_cpus: u32,
    /// Total physical memory in bytes at the end of the window.
    pub mem_total: u64,
    /// Physical memory available in bytes at the end of the window.
    pub mem_available: u64,
    /// Per-class usage, in [`Class::all`] order, including empty classes.
    pub by_class: Vec<GroupUsage>,
    /// Per-tree usage, sorted by CPU seconds descending.
    pub by_tree: Vec<GroupUsage>,
    /// The GPU reading taken at the end of the window.
    pub gpu: Option<GpuSample>,
    /// Processes present at the end of the window that could not be opened.
    pub denied: usize,
}

impl Attribution {
    /// Total CPU seconds across every class.
    pub fn total_cpu_seconds(&self) -> f64 {
        self.by_class.iter().map(|g| g.cpu_seconds).sum()
    }

    /// Look up one class group.
    pub fn class(&self, class: Class) -> Option<&GroupUsage> {
        self.by_class.iter().find(|g| g.label == class.label())
    }
}

/// True when two samples are the same running process rather than a pid reuse.
fn same_process(prev: &RawProc, cur: &RawProc) -> bool {
    prev.pid == cur.pid && prev.start_time == cur.start_time
}

/// Counter difference that copes with a restarted process.
///
/// If the process was replaced, or the counter went backwards (which it must
/// not, but a driver bug or pid reuse we failed to spot could do it), the
/// current value is taken as the whole window's usage.
fn counter_delta(prev: Option<u64>, cur: u64) -> u64 {
    match prev {
        Some(p) if cur >= p => cur - p,
        _ => cur,
    }
}

/// Difference two snapshots into per-class and per-tree usage.
///
/// `elapsed_secs` is the wall time between them, measured by the caller.
pub fn attribute(prev: &Snapshot, cur: &Snapshot, elapsed_secs: f64) -> Attribution {
    let before: BTreeMap<u32, &RawProc> = prev.procs.iter().map(|p| (p.pid, p)).collect();
    let tree = Tree::build(&cur.procs);

    let vram: BTreeMap<u32, u64> = cur
        .gpu
        .as_ref()
        .map(|g| g.process_vram.iter().copied().collect())
        .unwrap_or_default();

    let cpus = cur.system.logical_cpus.max(1) as f64;
    let machine_cpu_seconds = elapsed_secs * cpus;

    let mut by_class: BTreeMap<Class, GroupUsage> = BTreeMap::new();
    let mut by_tree: BTreeMap<u32, GroupUsage> = BTreeMap::new();
    let mut denied = 0usize;

    for p in &cur.procs {
        if p.denied {
            denied += 1;
        }
        let old = before.get(&p.pid).copied().filter(|o| same_process(o, p));

        let cpu_ns = counter_delta(old.map(|o| o.cpu_ns()), p.cpu_ns());
        let read_bytes = counter_delta(old.map(|o| o.io_read_bytes), p.io_read_bytes);
        let write_bytes = counter_delta(old.map(|o| o.io_write_bytes), p.io_write_bytes);
        let read_ops = counter_delta(old.map(|o| o.io_read_ops), p.io_read_ops);
        let write_ops = counter_delta(old.map(|o| o.io_write_ops), p.io_write_ops);
        let working_set = p.working_set.max(old.map(|o| o.working_set).unwrap_or(0));
        let private_bytes = p
            .private_bytes
            .max(old.map(|o| o.private_bytes).unwrap_or(0));
        let vram_bytes = vram.get(&p.pid).copied().unwrap_or(0);

        let class = classify(&p.name);
        let root = tree.root_of(p.pid);
        let root_name = cur
            .procs
            .iter()
            .find(|q| q.pid == root)
            .map(|q| q.name.clone())
            .unwrap_or_else(|| format!("pid {root}"));

        for group in [
            by_class
                .entry(class)
                .or_insert_with(|| GroupUsage::new(class.label().to_string())),
            by_tree
                .entry(root)
                .or_insert_with(|| GroupUsage::new(format!("{root_name} ({root})"))),
        ] {
            group.pids.push(p.pid);
            group.cpu_seconds += cpu_ns as f64 / 1e9;
            group.peak_working_set = group.peak_working_set.max(working_set);
            group.peak_private_bytes = group.peak_private_bytes.max(private_bytes);
            group.read_bytes += read_bytes;
            group.write_bytes += write_bytes;
            group.read_ops += read_ops;
            group.write_ops += write_ops;
            group.vram_bytes += vram_bytes;
            if p.denied {
                group.denied += 1;
            }
        }
    }

    let finish = |g: &mut GroupUsage| {
        g.cpu_pct_machine = if machine_cpu_seconds > 0.0 {
            g.cpu_seconds / machine_cpu_seconds * 100.0
        } else {
            0.0
        };
        g.pids.sort_unstable();
        g.pids.dedup();
    };

    // Every class appears, so a zero row is visibly zero rather than missing.
    let mut classes: Vec<GroupUsage> = Class::all()
        .into_iter()
        .map(|c| {
            by_class
                .remove(&c)
                .unwrap_or_else(|| GroupUsage::new(c.label().to_string()))
        })
        .collect();
    classes.iter_mut().for_each(&finish);

    let mut trees: Vec<GroupUsage> = by_tree.into_values().collect();
    trees.iter_mut().for_each(&finish);
    trees.sort_by(|a, b| {
        b.cpu_seconds
            .partial_cmp(&a.cpu_seconds)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });

    Attribution {
        elapsed_secs,
        logical_cpus: cur.system.logical_cpus,
        mem_total: cur.system.mem_total,
        mem_available: cur.system.mem_available,
        by_class: classes,
        by_tree: trees,
        gpu: cur.gpu.clone(),
        denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: u32, name: &str, cpu_ns: u64) -> RawProc {
        RawProc {
            pid,
            parent_pid: parent,
            name: name.to_string(),
            start_time: 1000,
            cpu_user_ns: cpu_ns,
            ..Default::default()
        }
    }

    fn snap(procs: Vec<RawProc>) -> Snapshot {
        Snapshot {
            procs,
            system: SystemInfo {
                logical_cpus: 4,
                mem_total: 16 << 30,
                mem_available: 8 << 30,
            },
            gpu: None,
        }
    }

    #[test]
    fn counter_delta_subtracts_when_the_process_continued() {
        assert_eq!(counter_delta(Some(100), 250), 150);
    }

    #[test]
    fn counter_delta_takes_the_whole_value_for_a_new_process() {
        assert_eq!(counter_delta(None, 250), 250);
    }

    #[test]
    fn counter_delta_takes_the_whole_value_when_a_counter_goes_backwards() {
        assert_eq!(counter_delta(Some(400), 250), 250);
    }

    #[test]
    fn cpu_seconds_are_the_difference_over_the_window() {
        let a = snap(vec![proc(10, 1, "rustc.exe", 1_000_000_000)]);
        let b = snap(vec![proc(10, 1, "rustc.exe", 3_000_000_000)]);
        let at = attribute(&a, &b, 2.0);
        let compiler = at.class(Class::Compiler).unwrap();
        assert_eq!(compiler.cpu_seconds, 2.0);
    }

    #[test]
    fn cpu_percent_is_relative_to_every_core() {
        // 2 CPU-seconds over 2 s on 4 cores is one core busy: 25% of the machine.
        let a = snap(vec![proc(10, 1, "rustc.exe", 0)]);
        let b = snap(vec![proc(10, 1, "rustc.exe", 2_000_000_000)]);
        let at = attribute(&a, &b, 2.0);
        assert_eq!(at.class(Class::Compiler).unwrap().cpu_pct_machine, 25.0);
    }

    #[test]
    fn a_restarted_pid_is_not_credited_with_the_old_process_time() {
        let a = snap(vec![proc(10, 1, "claude.exe", 5_000_000_000)]);
        let mut restarted = proc(10, 1, "claude.exe", 1_000_000_000);
        restarted.start_time = 9999; // different process, same pid
        let b = snap(vec![restarted]);
        let at = attribute(&a, &b, 1.0);
        // The whole 1 s counts, and the old 5 s does not come back as negative.
        assert_eq!(at.class(Class::Agent).unwrap().cpu_seconds, 1.0);
    }

    #[test]
    fn a_process_that_appeared_mid_window_counts_from_zero() {
        let a = snap(vec![]);
        let b = snap(vec![proc(10, 1, "rustc.exe", 1_500_000_000)]);
        let at = attribute(&a, &b, 3.0);
        assert_eq!(at.class(Class::Compiler).unwrap().cpu_seconds, 1.5);
    }

    #[test]
    fn a_process_that_exited_mid_window_is_not_reported() {
        let a = snap(vec![proc(10, 1, "rustc.exe", 1_000_000_000)]);
        let b = snap(vec![]);
        let at = attribute(&a, &b, 1.0);
        assert_eq!(at.total_cpu_seconds(), 0.0);
    }

    #[test]
    fn classes_aggregate_across_processes() {
        let a = snap(vec![
            proc(10, 1, "rustc.exe", 0),
            proc(11, 1, "cargo.exe", 0),
            proc(12, 1, "claude.exe", 0),
        ]);
        let b = snap(vec![
            proc(10, 1, "rustc.exe", 1_000_000_000),
            proc(11, 1, "cargo.exe", 2_000_000_000),
            proc(12, 1, "claude.exe", 4_000_000_000),
        ]);
        let at = attribute(&a, &b, 1.0);
        assert_eq!(at.class(Class::Compiler).unwrap().cpu_seconds, 3.0);
        assert_eq!(at.class(Class::Agent).unwrap().cpu_seconds, 4.0);
        assert_eq!(at.class(Class::Compiler).unwrap().pids, vec![10, 11]);
    }

    #[test]
    fn every_class_is_present_even_when_idle() {
        let at = attribute(&snap(vec![]), &snap(vec![]), 1.0);
        assert_eq!(at.by_class.len(), 6);
        assert!(at.by_class.iter().all(|g| g.cpu_seconds == 0.0));
    }

    #[test]
    fn a_tree_gathers_children_under_its_root() {
        let procs_a = vec![
            proc(1, 0, "shell.exe", 0),
            proc(2, 1, "cargo.exe", 0),
            proc(3, 2, "rustc.exe", 0),
        ];
        let procs_b = vec![
            proc(1, 0, "shell.exe", 1_000_000_000),
            proc(2, 1, "cargo.exe", 1_000_000_000),
            proc(3, 2, "rustc.exe", 1_000_000_000),
        ];
        let at = attribute(&snap(procs_a), &snap(procs_b), 1.0);
        assert_eq!(at.by_tree.len(), 1);
        assert_eq!(at.by_tree[0].cpu_seconds, 3.0);
        assert_eq!(at.by_tree[0].pids, vec![1, 2, 3]);
    }

    #[test]
    fn trees_are_sorted_by_cpu_descending() {
        let a = snap(vec![proc(1, 0, "quiet.exe", 0), proc(2, 0, "busy.exe", 0)]);
        let b = snap(vec![
            proc(1, 0, "quiet.exe", 1_000_000_000),
            proc(2, 0, "busy.exe", 8_000_000_000),
        ]);
        let at = attribute(&a, &b, 1.0);
        assert!(at.by_tree[0].label.starts_with("busy.exe"));
        assert!(at.by_tree[1].label.starts_with("quiet.exe"));
    }

    #[test]
    fn memory_takes_the_peak_across_both_ends_of_the_window() {
        let mut before = proc(10, 1, "claude.exe", 0);
        before.working_set = 900;
        let mut after = proc(10, 1, "claude.exe", 0);
        after.working_set = 400;
        let at = attribute(&snap(vec![before]), &snap(vec![after]), 1.0);
        assert_eq!(at.class(Class::Agent).unwrap().peak_working_set, 900);
    }

    #[test]
    fn io_bytes_and_rates_are_differenced() {
        let mut before = proc(10, 1, "claude.exe", 0);
        before.io_write_bytes = 1_000;
        before.io_write_ops = 2;
        let mut after = proc(10, 1, "claude.exe", 0);
        after.io_write_bytes = 5_000;
        after.io_write_ops = 12;
        let at = attribute(&snap(vec![before]), &snap(vec![after]), 2.0);
        let g = at.class(Class::Agent).unwrap();
        assert_eq!(g.write_bytes, 4_000);
        assert_eq!(g.write_ops, 10);
        assert_eq!(g.write_bytes_per_sec(at.elapsed_secs), 2_000.0);
    }

    #[test]
    fn rates_are_zero_rather_than_infinite_for_a_zero_window() {
        let g = GroupUsage::new("x".into());
        assert_eq!(g.write_bytes_per_sec(0.0), 0.0);
    }

    #[test]
    fn a_zero_length_window_reports_zero_percent_not_a_division_by_zero() {
        let a = snap(vec![proc(10, 1, "rustc.exe", 0)]);
        let b = snap(vec![proc(10, 1, "rustc.exe", 1_000_000_000)]);
        let at = attribute(&a, &b, 0.0);
        assert_eq!(at.class(Class::Compiler).unwrap().cpu_pct_machine, 0.0);
    }

    #[test]
    fn vram_is_attributed_to_the_owning_process() {
        let mut b = snap(vec![proc(10, 1, "claude.exe", 0)]);
        b.gpu = Some(GpuSample {
            backend: "test",
            devices: Vec::new(),
            process_vram: vec![(10, 2 << 20)],
        });
        let at = attribute(&snap(vec![proc(10, 1, "claude.exe", 0)]), &b, 1.0);
        assert_eq!(at.class(Class::Agent).unwrap().vram_bytes, 2 << 20);
    }

    #[test]
    fn denied_processes_are_counted_not_hidden() {
        let mut p = proc(10, 1, "system.exe", 0);
        p.denied = true;
        let at = attribute(&snap(vec![]), &snap(vec![p]), 1.0);
        assert_eq!(at.denied, 1);
        assert_eq!(at.class(Class::Other).unwrap().denied, 1);
    }
}
