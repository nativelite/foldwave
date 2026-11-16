//! Platform sampling.
//!
//! Each backend produces the same [`Snapshot`](crate::delta::Snapshot), so
//! everything above this module is platform-independent and testable anywhere.
//!
//! Windows came first (`docs/design.md` section 11, decision 5) and Linux is
//! the first Unix (decision 24). Other platforms compile and report plainly
//! that they are not implemented rather than returning zeros that would read
//! as "idle".

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    cpu_sets, cpu_times, fixed_volumes, job_assign, job_new, place_current_thread, proc_clock,
    proc_clock_read, process_links, processes, snapshot, system_info, volume, Job, ProcClock,
};

#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    target_pointer_width = "64",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{
    cpu_sets, cpu_times, fixed_volumes, job_assign, job_new, place_current_thread, proc_clock,
    proc_clock_read, process_links, processes, snapshot, system_info, volume, Job, ProcClock,
};

#[cfg(not(any(
    windows,
    all(
        target_os = "linux",
        target_pointer_width = "64",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod unsupported;
#[cfg(not(any(
    windows,
    all(
        target_os = "linux",
        target_pointer_width = "64",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
pub use unsupported::{
    cpu_sets, cpu_times, fixed_volumes, job_assign, job_new, place_current_thread, proc_clock,
    proc_clock_read, process_links, processes, snapshot, system_info, volume, Job, ProcClock,
};
