//! Platform sampling.
//!
//! Each backend produces the same [`Snapshot`](crate::delta::Snapshot), so
//! everything above this module is platform-independent and testable anywhere.
//!
//! Windows is implemented, per `docs/design.md` section 11, decision 5
//! (Windows first). Other platforms compile and report plainly that they are
//! not implemented rather than returning zeros that would read as "idle".

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    cpu_sets, cpu_times, fixed_volumes, job_assign, job_new, place_current_thread, proc_clock,
    proc_clock_read, process_links, processes, snapshot, system_info, volume, Job, ProcClock,
};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::{
    cpu_sets, cpu_times, fixed_volumes, job_assign, job_new, place_current_thread, proc_clock,
    proc_clock_read, process_links, processes, snapshot, system_info, volume, Job, ProcClock,
};
