//! Sampling stub for platforms with no backend yet.
//!
//! Windows is the implemented target (`docs/design.md` section 11, decision 5).
//! A Linux `/proc` backend is the obvious next one; until it exists these
//! functions fail loudly. Reporting an error rather than empty data matters:
//! zeros would read as "nothing is using this machine", which is exactly the
//! wrong answer for an attribution tool.

use crate::delta::{Snapshot, SystemInfo};
use crate::proc::RawProc;
use std::io;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "resmon has no sampling backend for this platform yet (Windows only)",
    ))
}

/// Always fails on an unsupported platform.
pub fn processes() -> io::Result<Vec<RawProc>> {
    unsupported()
}

/// Always fails on an unsupported platform.
pub fn system_info() -> io::Result<SystemInfo> {
    unsupported()
}

/// Always fails on an unsupported platform.
pub fn snapshot() -> io::Result<Snapshot> {
    unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_point_fails_as_unsupported() {
        for kind in [
            processes().map(|_| ()).unwrap_err().kind(),
            system_info().map(|_| ()).unwrap_err().kind(),
            snapshot().map(|_| ()).unwrap_err().kind(),
        ] {
            assert_eq!(kind, io::ErrorKind::Unsupported);
        }
    }
}
