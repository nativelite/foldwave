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
pub use windows::{fixed_volumes, processes, snapshot, system_info, volume};

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::{fixed_volumes, processes, snapshot, system_info, volume};
