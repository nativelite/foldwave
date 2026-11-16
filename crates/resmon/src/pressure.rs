//! Pressure: the share of time work stood waiting for a resource.
//!
//! Linux reports it directly (pressure-stall information, `/proc/pressure`),
//! for the CPU, for memory and for I/O. Utilization says how busy a resource
//! is; pressure says whether anything had to wait for it, which is what a
//! governor needs to know before it gives way (design §8.8). A machine at
//! 100 % CPU with nothing waiting is not under pressure.
//!
//! Windows has no counterpart; there [`read`] fails as unsupported and the
//! governor keeps using load ([`crate::cpu::load`]).

use std::io;

/// Which resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resource {
    /// Runnable tasks waiting for a CPU.
    Cpu,
    /// Tasks waiting on reclaim, swap-in or refaults.
    Memory,
    /// Tasks waiting on block I/O.
    Io,
}

/// One line of a pressure file.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stall {
    /// Percent of the last 10 seconds spent stalled.
    pub avg10: f64,
    /// Percent of the last 60 seconds.
    pub avg60: f64,
    /// Percent of the last 300 seconds.
    pub avg300: f64,
    /// Stalled time since boot, in microseconds. The difference between two
    /// readings is exact over any window, where the averages are smoothed.
    pub total_us: u64,
}

/// A resource's pressure.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Pressure {
    /// At least one task was stalled.
    pub some: Stall,
    /// Every task that could run was stalled at once: time lost outright.
    /// Absent for the CPU on older kernels, and then all zero.
    pub full: Stall,
}

/// Parse a pressure file:
///
/// ```text
/// some avg10=0.12 avg60=0.05 avg300=0.01 total=1479011
/// full avg10=0.00 avg60=0.00 avg300=0.00 total=0
/// ```
pub fn parse(text: &str) -> Pressure {
    let mut out = Pressure::default();
    for line in text.lines() {
        let mut words = line.split_ascii_whitespace();
        let stall = match words.next() {
            Some("some") => &mut out.some,
            Some("full") => &mut out.full,
            _ => continue,
        };
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            match key {
                "avg10" => stall.avg10 = value.parse().unwrap_or(0.0),
                "avg60" => stall.avg60 = value.parse().unwrap_or(0.0),
                "avg300" => stall.avg300 = value.parse().unwrap_or(0.0),
                "total" => stall.total_us = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    out
}

/// The share of the time between two readings that was stalled, from 0.0 to
/// 1.0, given the time between them in seconds.
pub fn stalled(before: &Stall, after: &Stall, elapsed_secs: f64) -> f64 {
    if elapsed_secs <= 0.0 {
        return 0.0;
    }
    let us = after.total_us.saturating_sub(before.total_us) as f64;
    (us / (elapsed_secs * 1e6)).clamp(0.0, 1.0)
}

/// The machine's pressure on `resource`, now. Needs no privilege. Fails as
/// unsupported off Linux, and as not found on a kernel built without it.
pub fn read(resource: Resource) -> io::Result<Pressure> {
    if !cfg!(target_os = "linux") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "pressure-stall information is Linux's",
        ));
    }
    let name = match resource {
        Resource::Cpu => "cpu",
        Resource::Memory => "memory",
        Resource::Io => "io",
    };
    std::fs::read_to_string(format!("/proc/pressure/{name}")).map(|t| parse(&t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_lines_parse_and_a_missing_one_is_zero() {
        let p = parse(
            "some avg10=1.50 avg60=0.25 avg300=0.01 total=1479011\n\
             full avg10=0.10 avg60=0.00 avg300=0.00 total=42\n",
        );
        assert_eq!(p.some.avg10, 1.5);
        assert_eq!(p.some.avg60, 0.25);
        assert_eq!(p.some.total_us, 1_479_011);
        assert_eq!(p.full.avg10, 0.1);
        assert_eq!(p.full.total_us, 42);
        assert_eq!(parse("some avg10=2.00 total=7\n").full, Stall::default());
    }

    #[test]
    fn stalled_time_is_a_share_of_the_window() {
        let at = |total_us| Stall {
            total_us,
            ..Default::default()
        };
        assert_eq!(stalled(&at(1_000_000), &at(1_250_000), 1.0), 0.25);
        assert_eq!(stalled(&at(0), &at(9_000_000), 1.0), 1.0, "clamped");
        assert_eq!(stalled(&at(5), &at(5), 0.0), 0.0);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn this_machine_reports_pressure_if_its_kernel_has_it() {
        match read(Resource::Cpu) {
            Ok(p) => assert!(p.some.avg10 >= 0.0 && p.some.avg10 <= 100.0),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::NotFound, "{e}"),
        }
    }
}
