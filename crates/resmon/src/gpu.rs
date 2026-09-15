//! GPU sampling.
//!
//! The parsing here is pure and tested; the one impure function shells out to
//! `nvidia-smi`, which ships with the NVIDIA display driver.
//!
//! **This is a vendor-specific probe, and it is deliberate.** `resmon` is a
//! measurement tool for the M0 harness, not shipped terminal code, and reading
//! a driver utility over stdout costs no dependency and no linkage. A
//! vendor-neutral backend over the Windows `GPU Engine` performance counters is
//! the natural follow-up; [`GpuSample::backend`] names which one produced a
//! sample so a report can never silently imply coverage it does not have.

use std::process::Command;

/// One GPU device at an instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuDevice {
    /// Device index as the driver numbers it.
    pub index: u32,
    /// Marketing name of the device, e.g. `NVIDIA GeForce RTX 3070`.
    pub name: String,
    /// Percentage of the last sampling period during which work was executing.
    pub utilization_pct: u32,
    /// Video memory in use, in bytes.
    pub memory_used: u64,
    /// Video memory installed on the device, in bytes.
    pub memory_total: u64,
}

/// A GPU reading, plus which backend produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuSample {
    /// Backend name, for honest reporting: `nvidia-smi`, or `none`.
    pub backend: &'static str,
    /// Every device the backend reported.
    pub devices: Vec<GpuDevice>,
    /// Video memory per process, as `(pid, bytes)`.
    pub process_vram: Vec<(u32, u64)>,
}

impl GpuSample {
    /// The sample reported when no GPU backend is available.
    pub fn none() -> GpuSample {
        GpuSample {
            backend: "none",
            devices: Vec::new(),
            process_vram: Vec::new(),
        }
    }

    /// True when no backend could read the GPU.
    pub fn is_unavailable(&self) -> bool {
        self.backend == "none"
    }
}

const MIB: u64 = 1024 * 1024;

/// Parse `nvidia-smi --query-gpu=index,name,utilization.gpu,memory.used,memory.total
/// --format=csv,noheader,nounits`.
///
/// Memory columns are mebibytes. Unparsable or short rows are skipped rather
/// than failing the whole sample: a partial GPU reading still beats none.
pub fn parse_devices(csv: &str) -> Vec<GpuDevice> {
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 5 {
                return None;
            }
            Some(GpuDevice {
                index: f[0].parse().ok()?,
                name: f[1].to_string(),
                utilization_pct: f[2].parse().ok()?,
                memory_used: f[3].parse::<u64>().ok()? * MIB,
                memory_total: f[4].parse::<u64>().ok()? * MIB,
            })
        })
        .collect()
}

/// Parse `nvidia-smi --query-compute-apps=pid,used_memory --format=csv,noheader,nounits`.
///
/// Memory is mebibytes. Rows whose memory reads `[N/A]`, which the driver
/// returns for processes it cannot inspect, are skipped.
pub fn parse_process_vram(csv: &str) -> Vec<(u32, u64)> {
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 2 {
                return None;
            }
            Some((f[0].parse().ok()?, f[1].parse::<u64>().ok()? * MIB))
        })
        .collect()
}

fn nvidia_smi(args: &[&str]) -> Option<String> {
    let out = Command::new("nvidia-smi").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Sample the GPU, returning [`GpuSample::none`] when no backend answers.
///
/// Never fails: a machine with no NVIDIA driver, or none at all, reports an
/// unavailable sample so the rest of the attribution still runs.
pub fn sample() -> GpuSample {
    let devices = match nvidia_smi(&[
        "--query-gpu=index,name,utilization.gpu,memory.used,memory.total",
        "--format=csv,noheader,nounits",
    ]) {
        Some(csv) => parse_devices(&csv),
        None => return GpuSample::none(),
    };
    let process_vram = nvidia_smi(&[
        "--query-compute-apps=pid,used_memory",
        "--format=csv,noheader,nounits",
    ])
    .map(|csv| parse_process_vram(&csv))
    .unwrap_or_default();

    GpuSample {
        backend: "nvidia-smi",
        devices,
        process_vram,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_device_row() {
        let csv = "0, NVIDIA GeForce RTX 3070, 37, 1975, 8192";
        let d = parse_devices(csv);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].index, 0);
        assert_eq!(d[0].name, "NVIDIA GeForce RTX 3070");
        assert_eq!(d[0].utilization_pct, 37);
        assert_eq!(d[0].memory_used, 1975 * MIB);
        assert_eq!(d[0].memory_total, 8192 * MIB);
    }

    #[test]
    fn parses_several_devices() {
        let csv = "0, A, 10, 100, 8192\n1, B, 20, 200, 8192\n";
        assert_eq!(parse_devices(csv).len(), 2);
    }

    #[test]
    fn skips_short_or_unparsable_device_rows() {
        let csv = "0, OnlyThreeFields, 10\nnot-a-number, X, 1, 2, 3\n0, Good, 5, 10, 20";
        let d = parse_devices(csv);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "Good");
    }

    #[test]
    fn empty_device_output_gives_no_devices() {
        assert!(parse_devices("").is_empty());
        assert!(parse_devices("\n\n").is_empty());
    }

    #[test]
    fn parses_process_vram_rows() {
        let csv = "1234, 512\n5678, 1024\n";
        assert_eq!(
            parse_process_vram(csv),
            vec![(1234, 512 * MIB), (5678, 1024 * MIB)]
        );
    }

    #[test]
    fn skips_not_applicable_process_memory() {
        // The driver prints [N/A] for processes it cannot inspect.
        let csv = "1234, [N/A]\n5678, 64";
        assert_eq!(parse_process_vram(csv), vec![(5678, 64 * MIB)]);
    }

    #[test]
    fn unavailable_sample_is_labelled_and_empty() {
        let s = GpuSample::none();
        assert!(s.is_unavailable());
        assert_eq!(s.backend, "none");
        assert!(s.devices.is_empty());
        assert!(s.process_vram.is_empty());
    }

    #[test]
    fn sample_never_panics_whatever_the_machine_has() {
        // On a box with no NVIDIA driver this must still return, labelled.
        let s = sample();
        assert!(s.backend == "nvidia-smi" || s.backend == "none");
    }
}
