//! Attribute CPU, memory, disk I/O and GPU use to process trees over a window.
//!
//! Built for foldwave's M0 harness, experiment E3 (`docs/design.md` section
//! 8.7b): *where does the machine actually go during an agentic session, and
//! what share of it is the terminal itself?* Answering that is what decides
//! whether moving work to the GPU is worth building.
//!
//! Zero third-party dependencies: Win32 is reached through this crate's own
//! extern blocks.
//!
//! # Example
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! use std::time::Duration;
//!
//! let window = resmon::measure(Duration::from_secs(5))?;
//! for group in &window.by_class {
//!     println!("{:10} {:6.2}% of machine", group.label, group.cpu_pct_machine);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What the numbers mean
//!
//! - **CPU** is real consumed time, so `cpu_pct_machine` of 100 means every
//!   logical core was busy with that group for the whole window.
//! - **Memory** is a peak gauge across the window's two ends, not an average.
//! - **I/O** counts every read and write a process makes, including pipes,
//!   sockets and console handles, not only disk. It attributes I/O *pressure*;
//!   it is not a measurement of SSD traffic on its own.
//! - **GPU** comes from whichever backend answered, named in
//!   [`GpuSample::backend`]. Today that is `nvidia-smi` or `none`.

#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod delta;
pub mod gpu;
pub mod proc;
pub mod report;
pub mod sys;

pub use delta::{attribute, Attribution, GroupUsage, Snapshot, SystemInfo};
pub use gpu::{GpuDevice, GpuSample};
pub use proc::{Class, RawProc};

use std::io;
use std::time::{Duration, Instant};

/// Sample, wait, sample again, and attribute the difference.
///
/// Blocks for `window`. The elapsed time is measured around the sampling, not
/// assumed from `window`, so a slow sample cannot inflate the percentages.
pub fn measure(window: Duration) -> io::Result<Attribution> {
    let before = sys::snapshot()?;
    let started = Instant::now();
    std::thread::sleep(window);
    let after = sys::snapshot()?;
    Ok(attribute(&before, &after, started.elapsed().as_secs_f64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn measure_over_a_short_window_attributes_this_process() {
        let at = measure(Duration::from_millis(120)).expect("measure a window");
        assert!(at.elapsed_secs >= 0.1, "elapsed was {}", at.elapsed_secs);
        assert!(at.logical_cpus >= 1);
        assert!(at.mem_total > 0);
        assert_eq!(at.by_class.len(), 6, "every class is always reported");
        assert!(!at.by_tree.is_empty(), "the machine has at least one tree");
    }

    #[test]
    #[cfg(windows)]
    fn percentages_never_exceed_what_the_machine_can_do() {
        let at = measure(Duration::from_millis(120)).expect("measure a window");
        let total: f64 = at.by_class.iter().map(|g| g.cpu_pct_machine).sum();
        // Some headroom for counter granularity, but a result far above 100%
        // would mean the per-core normalisation is wrong.
        assert!(total < 200.0, "total CPU share implausible: {total}%");
    }
}
