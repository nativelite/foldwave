//! Rendering an [`Attribution`] as text.
//!
//! Pure formatting, so every line of the report is testable without sampling
//! anything.

use crate::delta::{Attribution, GroupUsage};

/// Format a byte count with a binary unit, at three significant figures.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Format a byte rate.
pub fn rate(bytes_per_sec: f64) -> String {
    let n = if bytes_per_sec.is_finite() && bytes_per_sec > 0.0 {
        bytes_per_sec as u64
    } else {
        0
    };
    format!("{}/s", bytes(n))
}

fn row(g: &GroupUsage, elapsed: f64) -> String {
    format!(
        "{:<10} {:>7.2} {:>7.1}% {:>10} {:>11} {:>11} {:>10}",
        g.label,
        g.cpu_seconds,
        g.cpu_pct_machine,
        bytes(g.peak_working_set),
        rate(g.read_bytes_per_sec(elapsed)),
        rate(g.write_bytes_per_sec(elapsed)),
        bytes(g.vram_bytes),
    )
}

const HEADER: &str =
    "class         cpu-s    %machine        peak-rss    disk-read   disk-write       vram";

/// Render the full report: a header, the per-class table, the busiest trees,
/// the GPU reading and the caveats.
pub fn render(at: &Attribution, top_trees: usize) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "resource attribution over {:.2} s | {} logical CPUs | RAM {} used of {}\n\n",
        at.elapsed_secs,
        at.logical_cpus,
        bytes(at.mem_total.saturating_sub(at.mem_available)),
        bytes(at.mem_total),
    ));

    out.push_str(HEADER);
    out.push('\n');
    for g in &at.by_class {
        out.push_str(&row(g, at.elapsed_secs));
        out.push('\n');
    }
    out.push_str(&format!(
        "{:<10} {:>7.2} {:>7.1}%\n",
        "TOTAL",
        at.total_cpu_seconds(),
        at.by_class.iter().map(|g| g.cpu_pct_machine).sum::<f64>(),
    ));

    if top_trees > 0 && !at.by_tree.is_empty() {
        out.push_str(&format!("\ntop {top_trees} process trees by CPU\n"));
        for g in at.by_tree.iter().take(top_trees) {
            out.push_str(&format!(
                "{:<38} {:>7.2} cpu-s {:>6.1}% {:>10} rss {:>3} proc\n",
                truncate(&g.label, 38),
                g.cpu_seconds,
                g.cpu_pct_machine,
                bytes(g.peak_working_set),
                g.pids.len(),
            ));
        }
    }

    out.push_str("\ngpu: ");
    match &at.gpu {
        Some(g) if !g.is_unavailable() => {
            out.push_str(&format!("{} backend\n", g.backend));
            for d in &g.devices {
                out.push_str(&format!(
                    "  [{}] {:<28} {:>3}% util   {} of {} vram\n",
                    d.index,
                    truncate(&d.name, 28),
                    d.utilization_pct,
                    bytes(d.memory_used),
                    bytes(d.memory_total),
                ));
            }
            if g.devices.is_empty() {
                out.push_str("  no devices reported\n");
            }
        }
        _ => out.push_str("no backend available (no reading taken)\n"),
    }

    if at.denied > 0 {
        out.push_str(&format!(
            "\nnote: {} process(es) could not be opened; their counters read zero because they\n      are unknown, not because they are idle.\n",
            at.denied
        ));
    }
    out.push_str(
        "note: disk-read/disk-write count every read and write a process makes, including\n      pipes, sockets and console handles, not only disk.\n",
    );
    out.push_str(
        "note: a process that started AND exited inside the window is not in the closing\n      snapshot, so its cost is not counted here. Short-lived work (compiler\n      invocations, one-shot commands, flood panes) is therefore undercounted.\n",
    );
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).collect::<String>() + "\u{2026}"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::{attribute, Snapshot, SystemInfo};
    use crate::proc::RawProc;

    fn attribution() -> Attribution {
        let mk = |cpu: u64, ws: u64| RawProc {
            pid: 10,
            parent_pid: 1,
            name: "claude.exe".into(),
            start_time: 5,
            cpu_user_ns: cpu,
            working_set: ws,
            ..Default::default()
        };
        let sys = SystemInfo {
            logical_cpus: 4,
            mem_total: 16 << 30,
            mem_available: 8 << 30,
        };
        let a = Snapshot {
            procs: vec![mk(0, 1 << 20)],
            system: sys.clone(),
            gpu: None,
        };
        let b = Snapshot {
            procs: vec![mk(1_000_000_000, 2 << 20)],
            system: sys,
            gpu: None,
        };
        attribute(&a, &b, 1.0)
    }

    #[test]
    fn bytes_uses_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.00 KiB");
        assert_eq!(bytes(1536), "1.50 KiB");
        assert_eq!(bytes(10 * 1024), "10.0 KiB");
        assert_eq!(bytes(100 * 1024), "100 KiB");
        assert_eq!(bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(bytes(8 * 1024 * 1024 * 1024), "8.00 GiB");
    }

    #[test]
    fn bytes_stops_at_the_largest_unit_it_knows() {
        assert!(bytes(u64::MAX).ends_with("PiB"));
    }

    #[test]
    fn rate_appends_per_second() {
        assert_eq!(rate(1024.0), "1.00 KiB/s");
        assert_eq!(rate(0.0), "0 B/s");
    }

    #[test]
    fn rate_of_a_non_finite_value_is_zero_not_a_crash() {
        assert_eq!(rate(f64::INFINITY), "0 B/s");
        assert_eq!(rate(f64::NAN), "0 B/s");
        assert_eq!(rate(-5.0), "0 B/s");
    }

    #[test]
    fn truncate_leaves_short_strings_alone() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_marks_what_it_cut() {
        assert_eq!(truncate("abcdefghij", 5), "abcd\u{2026}");
    }

    #[test]
    fn truncate_handles_multibyte_characters_by_character_not_byte() {
        // Slicing this by bytes would panic mid-character.
        assert_eq!(
            truncate("\u{e9}\u{e9}\u{e9}\u{e9}", 3),
            "\u{e9}\u{e9}\u{2026}"
        );
    }

    #[test]
    fn report_lists_every_class_and_a_total() {
        let text = render(&attribution(), 3);
        for class in crate::proc::Class::all() {
            assert!(
                text.contains(class.label()),
                "missing class {}",
                class.label()
            );
        }
        assert!(text.contains("TOTAL"));
    }

    #[test]
    fn report_states_the_window_and_the_machine() {
        let text = render(&attribution(), 3);
        assert!(text.contains("over 1.00 s"));
        assert!(text.contains("4 logical CPUs"));
        assert!(text.contains("RAM 8.00 GiB used of 16.0 GiB"));
    }

    #[test]
    fn report_says_plainly_when_no_gpu_backend_answered() {
        let text = render(&attribution(), 0);
        assert!(text.contains("no backend available"));
    }

    #[test]
    fn report_always_carries_the_io_caveat() {
        let text = render(&attribution(), 0);
        assert!(text.contains("not only disk"));
    }

    #[test]
    fn report_always_admits_it_misses_processes_that_exited() {
        let text = render(&attribution(), 0);
        assert!(text.contains("started AND exited inside the window"));
    }

    #[test]
    fn report_omits_the_tree_section_when_asked_for_none() {
        assert!(!render(&attribution(), 0).contains("process trees"));
        assert!(render(&attribution(), 2).contains("process trees"));
    }

    #[test]
    fn report_shows_gpu_devices_when_a_backend_answered() {
        let mut at = attribution();
        at.gpu = Some(crate::gpu::GpuSample {
            backend: "nvidia-smi",
            devices: vec![crate::gpu::GpuDevice {
                index: 0,
                name: "NVIDIA GeForce RTX 3070".into(),
                utilization_pct: 42,
                memory_used: 1975 * 1024 * 1024,
                memory_total: 8192 * 1024 * 1024,
            }],
            process_vram: vec![],
        });
        let text = render(&at, 0);
        assert!(text.contains("nvidia-smi backend"));
        assert!(text.contains("RTX 3070"));
        assert!(text.contains("42% util"));
    }

    #[test]
    fn report_warns_when_processes_could_not_be_opened() {
        let mut at = attribution();
        at.denied = 3;
        assert!(render(&at, 0).contains("3 process(es) could not be opened"));
    }
}
