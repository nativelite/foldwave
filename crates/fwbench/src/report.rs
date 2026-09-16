//! Rendering an M0 [`Run`] as text. Pure formatting, so it is tested directly.

use crate::stats::ms;
use crate::Run;

/// Format a byte rate in MiB/s.
pub fn mib_per_sec(bytes_per_sec: f64) -> String {
    let v = if bytes_per_sec.is_finite() && bytes_per_sec > 0.0 {
        bytes_per_sec
    } else {
        0.0
    };
    format!("{:.1} MiB/s", v / (1024.0 * 1024.0))
}

/// Format a byte count in MiB.
pub fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Render the whole run.
pub fn render(run: &Run) -> String {
    let mut out = String::new();
    out.push_str("foldwave M0 harness\n\n");

    if let Some(f) = &run.flood {
        out.push_str(&format!(
            "throughput over {}: {} panes, {} requested each, over {:.2} s\n",
            f.transport.label(),
            f.panes.len(),
            mib(f.requested_per_pane),
            f.elapsed.as_secs_f64(),
        ));
        out.push_str(&format!(
            "  aggregate   {}  ({} read in total)\n",
            mib_per_sec(f.aggregate_bytes_per_sec()),
            mib(f.total_bytes()),
        ));
        let slowest = f
            .panes
            .iter()
            .map(|p| p.bytes_per_sec())
            .fold(f64::INFINITY, f64::min);
        let fastest = f
            .panes
            .iter()
            .map(|p| p.bytes_per_sec())
            .fold(0.0_f64, f64::max);
        if f.panes.len() > 1 {
            out.push_str(&format!(
                "  per pane    slowest {}, fastest {}\n",
                mib_per_sec(slowest),
                mib_per_sec(fastest),
            ));
        }
        out.push_str(&format!(
            "  observed/requested ratio {:.2}\n",
            f.observed_ratio()
        ));
        if f.observed_ratio() < 0.95 || f.observed_ratio() > 1.05 {
            out.push_str(
                "    note: the bytes we read differ from the bytes the child wrote. On Windows\n\
                 \x20         ConPTY re-renders output rather than passing it through, so this is\n\
                 \x20         terminal-observed throughput, not child output.\n",
            );
        }
        for failed in f.failures() {
            out.push_str(&format!(
                "  PANE {} FAILED: {}\n",
                failed.index,
                failed.error.as_deref().unwrap_or("unknown")
            ));
        }
        out.push('\n');
    }

    if let Some(e) = &run.echo {
        out.push_str(&format!(
            "key echo through `{}`, {:.0} ms between keys\n",
            e.shell,
            e.gap.as_secs_f64() * 1000.0
        ));
        if e.is_empty() {
            out.push_str("  NO ECHO OBSERVED - no latency can be reported\n");
        } else {
            out.push_str(&format!("  {}\n", e.summary.line()));
        }
        if e.lost > 0 {
            out.push_str(&format!("  {} key(s) never echoed\n", e.lost));
        }
        if e.below_coalesce_floor() && !e.is_empty() {
            out.push_str(
                "    note: keys were sent faster than the Windows console coalesces output\n\
                 \x20         (~15.2 ms on the reference machine), so this figure is dominated\n\
                 \x20         by the wait for the next console tick, not by echo latency. Use a\n\
                 \x20         gap at or above 16 ms to measure the echo itself.\n",
            );
        }
        out.push('\n');
    }

    if let Some(at) = &run.attribution {
        out.push_str(&resmon::report::render(at, 5));
    }
    out
}

/// A one-line verdict against the section 4 targets, for a quick read.
///
/// Reports only what was measured; anything absent is named as not measured
/// rather than silently passing.
pub fn verdict(run: &Run) -> String {
    let mut parts = Vec::new();
    match &run.echo {
        Some(e) if !e.is_empty() => {
            let p50_ok = e.summary.p50 < std::time::Duration::from_millis(1);
            let p99_ok = e.summary.p99 < std::time::Duration::from_millis(5);
            parts.push(format!(
                "input p50 {} ({}), p99 {} ({})",
                ms(e.summary.p50),
                if p50_ok { "within 1ms" } else { "OVER 1ms" },
                ms(e.summary.p99),
                if p99_ok { "within 5ms" } else { "OVER 5ms" },
            ));
        }
        _ => parts.push("input latency not measured".to_string()),
    }
    match &run.flood {
        Some(f) => parts.push(format!(
            "throughput {} aggregate",
            mib_per_sec(f.aggregate_bytes_per_sec())
        )),
        None => parts.push("throughput not measured".to_string()),
    }
    parts.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::echo::EchoResult;
    use crate::flood::{FloodResult, PaneResult};
    use crate::stats::Summary;
    use std::time::Duration;

    fn flood(bytes: u64, requested: u64, secs: f64, failed: bool) -> FloodResult {
        let mut pane = PaneResult {
            index: 0,
            bytes_read: bytes,
            reads: 4,
            elapsed: Duration::from_secs_f64(secs),
            error: None,
        };
        if failed {
            pane.error = Some("spawn failed".into());
        }
        FloodResult {
            panes: vec![pane],
            elapsed: Duration::from_secs_f64(secs),
            requested_per_pane: requested,
            read_sizes: Summary::of(&[]),
            transport: crate::flood::Transport::Pty,
        }
    }

    fn echo(micros: &[u64], lost: usize) -> EchoResult {
        let samples: Vec<Duration> = micros.iter().map(|&v| Duration::from_micros(v)).collect();
        EchoResult {
            summary: Summary::of(&samples),
            samples,
            lost,
            shell: "cmd".into(),
            gap: Duration::from_millis(25),
        }
    }

    #[test]
    fn mib_per_sec_formats_and_guards_bad_values() {
        assert_eq!(mib_per_sec(1024.0 * 1024.0), "1.0 MiB/s");
        assert_eq!(mib_per_sec(0.0), "0.0 MiB/s");
        assert_eq!(mib_per_sec(f64::NAN), "0.0 MiB/s");
        assert_eq!(mib_per_sec(-1.0), "0.0 MiB/s");
    }

    #[test]
    fn the_throughput_section_names_the_transport() {
        let run = Run {
            flood: Some(flood(1024, 1024, 1.0, false)),
            echo: None,
            attribution: None,
        };
        assert!(render(&run).contains("ConPTY"));
    }

    #[test]
    fn mib_formats_byte_counts() {
        assert_eq!(mib(1024 * 1024), "1.0 MiB");
        assert_eq!(mib(0), "0.0 MiB");
    }

    #[test]
    fn a_run_with_nothing_measured_still_renders() {
        let run = Run {
            flood: None,
            echo: None,
            attribution: None,
        };
        let text = render(&run);
        assert!(text.contains("foldwave M0 harness"));
    }

    #[test]
    fn throughput_is_reported_with_its_aggregate() {
        let run = Run {
            flood: Some(flood(4 * 1024 * 1024, 4 * 1024 * 1024, 2.0, false)),
            echo: None,
            attribution: None,
        };
        let text = render(&run);
        assert!(text.contains("throughput over"));
        assert!(text.contains("2.0 MiB/s"));
    }

    #[test]
    fn a_ratio_far_from_one_carries_the_conpty_explanation() {
        let run = Run {
            flood: Some(flood(8 * 1024 * 1024, 4 * 1024 * 1024, 1.0, false)),
            echo: None,
            attribution: None,
        };
        assert!(render(&run).contains("ConPTY re-renders"));
    }

    #[test]
    fn a_ratio_near_one_omits_the_explanation() {
        let run = Run {
            flood: Some(flood(4 * 1024 * 1024, 4 * 1024 * 1024, 1.0, false)),
            echo: None,
            attribution: None,
        };
        assert!(!render(&run).contains("ConPTY re-renders"));
    }

    #[test]
    fn a_failed_pane_is_shouted_about_not_hidden() {
        let run = Run {
            flood: Some(flood(0, 1024, 1.0, true)),
            echo: None,
            attribution: None,
        };
        assert!(render(&run).contains("PANE 0 FAILED"));
    }

    #[test]
    fn an_echo_run_that_saw_nothing_says_so_loudly() {
        let run = Run {
            flood: None,
            echo: Some(echo(&[], 10)),
            attribution: None,
        };
        let text = render(&run);
        assert!(text.contains("NO ECHO OBSERVED"));
        assert!(text.contains("10 key(s) never echoed"));
    }

    #[test]
    fn the_echo_section_states_the_keystroke_gap() {
        let run = Run {
            flood: None,
            echo: Some(echo(&[100], 0)),
            attribution: None,
        };
        assert!(render(&run).contains("25 ms between keys"));
    }

    #[test]
    fn a_gap_below_the_coalescing_floor_is_flagged() {
        let mut e = echo(&[9_000], 0);
        e.gap = Duration::from_millis(5);
        let run = Run {
            flood: None,
            echo: Some(e),
            attribution: None,
        };
        assert!(render(&run).contains("faster than the Windows console coalesces"));
    }

    #[test]
    fn a_gap_above_the_floor_carries_no_warning() {
        let run = Run {
            flood: None,
            echo: Some(echo(&[100], 0)),
            attribution: None,
        };
        assert!(!render(&run).contains("coalesces"));
    }

    #[test]
    fn verdict_names_the_targets_it_checked() {
        let run = Run {
            flood: Some(flood(1024 * 1024, 1024 * 1024, 1.0, false)),
            echo: Some(echo(&[400, 500, 600], 0)),
            attribution: None,
        };
        let v = verdict(&run);
        assert!(v.contains("within 1ms"), "{v}");
        assert!(v.contains("throughput"), "{v}");
    }

    #[test]
    fn verdict_calls_out_a_missed_target() {
        let run = Run {
            flood: None,
            echo: Some(echo(&[9_000], 0)),
            attribution: None,
        };
        let v = verdict(&run);
        assert!(v.contains("OVER 1ms"), "{v}");
        assert!(v.contains("OVER 5ms"), "{v}");
    }

    #[test]
    fn verdict_says_what_was_not_measured_rather_than_passing_it() {
        let run = Run {
            flood: None,
            echo: None,
            attribution: None,
        };
        let v = verdict(&run);
        assert!(v.contains("input latency not measured"));
        assert!(v.contains("throughput not measured"));
    }
}
