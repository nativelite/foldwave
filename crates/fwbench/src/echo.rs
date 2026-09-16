//! Key-echo latency: the time from writing a key to its echo arriving.
//!
//! This is the measurement `docs/design.md` section 4 sets a target for, and
//! section 5 principle 5 warns about getting wrong: *a harness that sleeps adds
//! its own latency*. atrium measured a "bare cmd" figure of 1.6 ms that was
//! really 0.1 ms, purely because the harness polled.
//!
//! So the timestamp is taken **on the reader thread, the instant a blocking
//! read returns** — never after a poll, a sleep or a queue hop. The reader
//! thread sends the already-taken `Instant` back; the channel's own latency
//! therefore cannot inflate the sample.
//!
//! No newline is ever sent, so the shell under test never executes anything:
//! the keys accumulate on an abandoned command line and the process is killed.

use crate::stats::Summary;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for a single echo before counting it lost.
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
/// Quiet period that marks the shell as settled after spawn.
const SETTLE_QUIET: Duration = Duration::from_millis(300);
/// Upper bound on waiting for the shell to settle.
const SETTLE_LIMIT: Duration = Duration::from_secs(10);
/// Keystroke spacing at or above which the Windows console stops adding a
/// coalescing wait, measured on the reference machine at ~15.2 ms
/// (`docs/design.md` section 8.9). Below this, a key waits for the next tick.
pub const COALESCE_FLOOR: Duration = Duration::from_millis(16);

/// The outcome of a key-echo run.
#[derive(Debug, Clone)]
pub struct EchoResult {
    /// Latency distribution over the samples that echoed.
    pub summary: Summary,
    /// Every sample, in order, for callers that want the raw series.
    pub samples: Vec<Duration>,
    /// Keys that never echoed within the timeout.
    pub lost: usize,
    /// The shell that was measured.
    pub shell: String,
    /// Gap requested between keystrokes. The figures are meaningless without
    /// it: below the console's coalescing period the latency is dominated by
    /// the wait for the next tick, not by the echo. See [`COALESCE_FLOOR`].
    pub gap: Duration,
}

impl EchoResult {
    /// True when nothing echoed, so the numbers mean nothing.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// True when keys were sent faster than the console coalesces, so the
    /// latency reported is dominated by the wait for the next console tick.
    pub fn below_coalesce_floor(&self) -> bool {
        self.gap < COALESCE_FLOOR
    }
}

/// The default interactive shell to measure on this platform.
pub fn default_shell() -> &'static str {
    if cfg!(windows) {
        "cmd"
    } else {
        "sh"
    }
}

/// Measure key-echo latency through a pty running `shell`.
///
/// Sends `samples` keystrokes, spaced by `gap`, and times each echo.
pub fn run(shell: &str, samples: usize, gap: Duration) -> io::Result<EchoResult> {
    let mut pty = pty::Pty::spawn(shell, &[], 24, 80)?;
    let mut reader = pty.reader()?;
    let (tx, rx) = mpsc::channel::<Instant>();

    // The reader thread times arrival the moment the blocking read returns.
    let reader_thread = std::thread::spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let arrived = Instant::now();
                    if tx.send(arrived).is_err() {
                        break; // the run finished
                    }
                }
                Err(_) => break,
            }
        }
    });

    settle(&rx);

    let mut latencies = Vec::with_capacity(samples);
    let mut lost = 0usize;
    for _ in 0..samples {
        // Discard anything still in flight so the next arrival is ours.
        while rx.try_recv().is_ok() {}

        let sent = Instant::now();
        if pty.write(b"x").is_err() {
            lost += 1;
            break;
        }
        match rx.recv_timeout(ECHO_TIMEOUT) {
            Ok(arrived) => latencies.push(arrived.saturating_duration_since(sent)),
            Err(_) => lost += 1,
        }
        std::thread::sleep(gap);
    }

    let _ = pty.kill();
    let _ = pty.wait();
    // On Windows the reader only reaches end-of-stream once the pseudoconsole
    // itself closes, which happens when the `Pty` is dropped. So the `Pty` must
    // be dropped *before* joining the reader: the other order deadlocks, with
    // the join waiting on a read that is waiting on the drop. pty-rs documents
    // this on `Pty::reader`.
    drop(pty);
    let _ = reader_thread.join();

    Ok(EchoResult {
        summary: Summary::of(&latencies),
        samples: latencies,
        lost,
        shell: shell.to_string(),
        gap,
    })
}

/// Wait until the shell stops producing output, so its banner and prompt are
/// not mistaken for an echo. Gives up after [`SETTLE_LIMIT`].
fn settle(rx: &mpsc::Receiver<Instant>) {
    let deadline = Instant::now() + SETTLE_LIMIT;
    while Instant::now() < deadline {
        match rx.recv_timeout(SETTLE_QUIET) {
            Ok(_) => continue, // still talking
            Err(_) => return,  // quiet for SETTLE_QUIET: settled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(samples: &[u64], lost: usize) -> EchoResult {
        let samples: Vec<Duration> = samples.iter().map(|&v| Duration::from_micros(v)).collect();
        EchoResult {
            summary: Summary::of(&samples),
            samples,
            lost,
            shell: "cmd".into(),
            gap: Duration::from_millis(25),
        }
    }

    #[test]
    fn default_shell_is_named_for_this_platform() {
        let shell = default_shell();
        assert!(!shell.is_empty());
        if cfg!(windows) {
            assert_eq!(shell, "cmd");
        }
    }

    #[test]
    fn a_result_with_no_samples_says_so() {
        assert!(result(&[], 5).is_empty());
    }

    #[test]
    fn a_result_with_samples_summarises_them() {
        let r = result(&[400, 500, 600], 0);
        assert!(!r.is_empty());
        assert_eq!(r.summary.count, 3);
        assert_eq!(r.summary.p50, Duration::from_micros(500));
    }

    #[test]
    fn lost_keys_are_carried_separately_from_the_distribution() {
        let r = result(&[400, 500], 3);
        assert_eq!(r.lost, 3);
        assert_eq!(r.summary.count, 2, "lost keys must not enter the summary");
    }

    #[test]
    #[cfg(windows)]
    fn measuring_a_real_shell_produces_plausible_latencies() {
        let r = run(default_shell(), 12, Duration::from_millis(5)).expect("spawn a shell");
        assert!(
            !r.is_empty(),
            "no echo observed from {}; the harness would be reporting nothing",
            r.shell
        );
        // A local pty echo is sub-second by any reasonable measure. This is a
        // sanity bound, not a performance assertion.
        assert!(
            r.summary.p50 < Duration::from_secs(1),
            "implausible p50 {:?}",
            r.summary.p50
        );
    }
}
