//! Space on a volume: what a writer of logs needs to know before the disk
//! fills.

use std::io;
use std::path::{Path, PathBuf};

/// A volume's root and its space, in bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// The volume's root, e.g. `C:\`, or its mount point, e.g. `/home`.
    pub root: PathBuf,
    /// The volume's size.
    pub total: u64,
    /// Space this process may still write (quotas applied).
    pub free: u64,
}

/// The volume holding `path`, which need not exist yet.
pub fn of(path: &Path) -> io::Result<Volume> {
    crate::sys::volume(path)
}

/// The roots of the machine's fixed drives, in order. Empty where the
/// platform has no backend.
pub fn fixed() -> Vec<PathBuf> {
    crate::sys::fixed_volumes()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn the_current_directory_is_on_a_fixed_volume_with_space() {
        let here = std::env::current_dir().unwrap();
        let v = of(&here).expect("volume of the current directory");
        assert!(v.total > 0 && v.free <= v.total, "{v:?}");
        assert!(here.starts_with(&v.root), "{here:?} not under {:?}", v.root);
        assert!(
            fixed().contains(&v.root),
            "{:?} not in {:?}",
            v.root,
            fixed()
        );
    }

    #[test]
    fn a_path_that_does_not_exist_yet_still_has_a_volume() {
        let here = std::env::current_dir().unwrap();
        let v = of(&here.join("no-such-folder").join("deeper")).expect("volume");
        assert!(here.starts_with(&v.root));
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    fn the_current_directory_is_on_a_volume_with_space() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let v = of(&here).expect("volume of the current directory");
        assert!(v.total > 0 && v.free <= v.total, "{v:?}");
        assert!(here.starts_with(&v.root), "{here:?} not under {:?}", v.root);
        // A container's root may be no disk at all, so `fixed` may be empty;
        // whatever it lists is a mount point.
        for root in fixed() {
            assert_eq!(of(&root).unwrap().root, root);
        }
    }

    #[test]
    fn a_path_that_does_not_exist_yet_still_has_a_volume() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let v = of(&here.join("no-such-folder").join("deeper")).expect("volume");
        assert!(here.starts_with(&v.root));
        assert_eq!(v, of(&here).unwrap());
    }
}
