//! A job that takes its processes down when it closes (Windows job objects,
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
//!
//! foldwave puts a pane's program in one, held by the process that started
//! it (the window, or a pane host with `--isolate`). If that process ends in
//! any way, a crash or a kill included, Windows closes the job and ends the
//! program with it: nothing outlives its pane by accident (design §8.8a).

use std::io;

/// A kill-on-close job. Dropping it ends every process in it.
pub struct KillOnClose(crate::sys::Job);

impl KillOnClose {
    /// A new, empty job.
    pub fn new() -> io::Result<KillOnClose> {
        crate::sys::job_new().map(KillOnClose)
    }

    /// Put `child` in the job.
    pub fn assign(&self, child: &std::process::Child) -> io::Result<()> {
        crate::sys::job_assign(&self.0, child)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn closing_the_job_ends_the_process_in_it() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let job = KillOnClose::new().unwrap();
        job.assign(&child).unwrap();
        assert!(child.try_wait().unwrap().is_none(), "running in the job");
        drop(job);
        let until = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "the job's close did not end it");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
