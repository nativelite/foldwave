//! Throughput: N pty panes all flooding at once.
//!
//! Each pane runs this same binary in `--emit` mode, so the workload needs no
//! fixture and is byte-identical between runs. Each pane gets its own blocking
//! reader thread, which is the shape `docs/design.md` section 8.1 specifies and
//! atrium already proved.
//!
//! What this measures is **terminal-observed throughput**: bytes that arrived
//! at our side of the pty. On Windows that is not the same as the bytes the
//! child wrote, because ConPTY re-renders output rather than passing it
//! through. The result reports both so the difference is visible instead of
//! assumed.

use crate::stats::Summary;
use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// What one pane achieved.
#[derive(Debug, Clone)]
pub struct PaneResult {
    /// Index of the pane, from zero.
    pub index: usize,
    /// Bytes read from the pty.
    pub bytes_read: u64,
    /// Reads that returned data.
    pub reads: u64,
    /// Wall time from spawn to the pane closing.
    pub elapsed: Duration,
    /// Any error that ended the pane early.
    pub error: Option<String>,
}

impl PaneResult {
    /// Bytes per second observed for this pane.
    pub fn bytes_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.bytes_read as f64 / secs
        } else {
            0.0
        }
    }
}

/// What the whole flood achieved.
#[derive(Debug, Clone)]
pub struct FloodResult {
    /// One entry per pane.
    pub panes: Vec<PaneResult>,
    /// Wall time for the whole run.
    pub elapsed: Duration,
    /// Bytes each pane was asked to emit.
    pub requested_per_pane: u64,
    /// Distribution of per-read sizes, as a proxy for how output arrives.
    pub read_sizes: Summary,
    /// Transport the panes used.
    pub transport: Transport,
}

impl FloodResult {
    /// Total bytes read across every pane.
    pub fn total_bytes(&self) -> u64 {
        self.panes.iter().map(|p| p.bytes_read).sum()
    }

    /// Aggregate throughput across every pane.
    pub fn aggregate_bytes_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.total_bytes() as f64 / secs
        } else {
            0.0
        }
    }

    /// Bytes read divided by bytes requested.
    ///
    /// On Windows this exposes ConPTY's re-rendering: a ratio far from 1.0
    /// means the console host, not the child, decided what we received.
    pub fn observed_ratio(&self) -> f64 {
        let requested = self.requested_per_pane * self.panes.len() as u64;
        if requested > 0 {
            self.total_bytes() as f64 / requested as f64
        } else {
            0.0
        }
    }

    /// Panes that ended with an error.
    pub fn failures(&self) -> Vec<&PaneResult> {
        self.panes.iter().filter(|p| p.error.is_some()).collect()
    }
}

/// Which transport a pane's output travels over.
///
/// The design's transport table (section 8.9) says agents should run headless
/// over plain pipes where they document it, and only interactive console apps
/// need ConPTY. Measuring both is the only way to tell whether a throughput
/// ceiling belongs to the console host or to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// A real pseudo-terminal. What an interactive console app requires.
    Pty,
    /// A plain pipe on stdout. No console host in the path.
    Pipe,
}

impl Transport {
    /// Stable name for reports.
    pub fn label(self) -> &'static str {
        match self {
            Transport::Pty => "pty (ConPTY)",
            Transport::Pipe => "pipe (headless)",
        }
    }
}

/// Drain one pane over a plain pipe, counting what arrives.
fn drain_pane_pipe(index: usize, exe: &str, bytes: u64) -> PaneResult {
    use std::process::{Command, Stdio};
    let started = Instant::now();

    let mut child = match Command::new(exe)
        .args(["--emit", "--bytes", &bytes.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return PaneResult {
                index,
                bytes_read: 0,
                reads: 0,
                elapsed: started.elapsed(),
                error: Some(format!("spawn failed: {e}")),
            }
        }
    };

    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            return PaneResult {
                index,
                bytes_read: 0,
                reads: 0,
                elapsed: started.elapsed(),
                error: Some("no stdout pipe".to_string()),
            };
        }
    };

    let mut buf = vec![0u8; 64 * 1024];
    let mut bytes_read = 0u64;
    let mut reads = 0u64;
    let mut error = None;
    loop {
        match std::io::Read::read(&mut stdout, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                bytes_read += n as u64;
                reads += 1;
            }
            Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
            Err(e) => {
                error = Some(format!("read failed: {e}"));
                break;
            }
        }
    }
    let _ = child.wait();

    PaneResult {
        index,
        bytes_read,
        reads,
        elapsed: started.elapsed(),
        error,
    }
}

/// Drain one pty pane to completion, counting what arrives.
///
/// Returns the byte count, the number of reads, and every read size, so the
/// caller can see whether output arrives in a few large chunks or many small
/// ones.
fn drain_pane(index: usize, exe: &str, bytes: u64, rows: u16, cols: u16) -> PaneResult {
    let started = Instant::now();
    let bytes_arg = bytes.to_string();
    let args = ["--emit", "--bytes", bytes_arg.as_str()];

    let mut pty = match pty::Pty::spawn(exe, &args, rows, cols) {
        Ok(p) => p,
        Err(e) => {
            return PaneResult {
                index,
                bytes_read: 0,
                reads: 0,
                elapsed: started.elapsed(),
                error: Some(format!("spawn failed: {e}")),
            }
        }
    };

    let reader = match pty.reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = pty.kill();
            return PaneResult {
                index,
                bytes_read: 0,
                reads: 0,
                elapsed: started.elapsed(),
                error: Some(format!("reader failed: {e}")),
            };
        }
    };

    // The reader must keep draining while the child runs, or the pipe fills and
    // the child stalls. It runs on its own thread for exactly that reason
    // (design section 8.1).
    let (replies_tx, replies_rx) = std::sync::mpsc::channel::<&'static [u8]>();
    let counter = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = vec![0u8; 64 * 1024];
        let mut bytes_read = 0u64;
        let mut reads = 0u64;
        let mut error = None;
        let mut watcher = crate::terminal::Watcher::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    bytes_read += n as u64;
                    reads += 1;
                    // A console host that asks a terminal query (OpenConsole
                    // opens with one) stalls until it is answered.
                    for reply in watcher.replies(&buf[..n]) {
                        let _ = replies_tx.send(reply);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break,
                Err(e) => {
                    error = Some(format!("read failed: {e}"));
                    break;
                }
            }
        }
        (bytes_read, reads, error)
    });

    // Wait for the child to finish emitting, answering terminal queries while
    // it runs, then close the pseudoconsole so the reader reaches
    // end-of-stream. Dropping the `Pty` before the join is required on Windows;
    // the other order deadlocks (see `echo.rs`).
    loop {
        match replies_rx.recv_timeout(std::time::Duration::from_millis(5)) {
            Ok(reply) => {
                let _ = pty.write(reply);
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            // The reader is gone; keep polling for exit without spinning.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
        }
        match pty.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) => {}
        }
    }
    drop(pty);

    let (bytes_read, reads, error) = match counter.join() {
        Ok(v) => v,
        Err(_) => (0, 0, Some("reader thread panicked".to_string())),
    };

    PaneResult {
        index,
        bytes_read,
        reads,
        elapsed: started.elapsed(),
        error,
    }
}

/// Run `panes` flooding panes of `bytes_per_pane` each, concurrently.
///
/// `exe` is the path to this binary, which each pane re-runs in `--emit` mode.
pub fn run(
    exe: &str,
    panes: usize,
    bytes_per_pane: u64,
    rows: u16,
    cols: u16,
    transport: Transport,
) -> FloodResult {
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::with_capacity(panes);

    for index in 0..panes {
        let exe = exe.to_string();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let result = match transport {
                Transport::Pty => drain_pane(index, &exe, bytes_per_pane, rows, cols),
                Transport::Pipe => drain_pane_pipe(index, &exe, bytes_per_pane),
            };
            // A closed receiver just means the run was abandoned.
            let _ = tx.send(result);
        }));
    }
    drop(tx);

    let mut results: Vec<PaneResult> = rx.iter().collect();
    for h in handles {
        let _ = h.join();
    }
    results.sort_by_key(|p| p.index);

    // Mean read size per pane stands in for the per-read distribution, which
    // would otherwise mean shipping every read size back across the channel.
    let read_sizes: Vec<Duration> = results
        .iter()
        .filter(|p| p.reads > 0)
        .map(|p| Duration::from_nanos(p.bytes_read / p.reads))
        .collect();

    FloodResult {
        panes: results,
        elapsed: started.elapsed(),
        requested_per_pane: bytes_per_pane,
        read_sizes: Summary::of(&read_sizes),
        transport,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(index: usize, bytes: u64, secs: f64) -> PaneResult {
        PaneResult {
            index,
            bytes_read: bytes,
            reads: 10,
            elapsed: Duration::from_secs_f64(secs),
            error: None,
        }
    }

    fn result(panes: Vec<PaneResult>, secs: f64, requested: u64) -> FloodResult {
        FloodResult {
            panes,
            elapsed: Duration::from_secs_f64(secs),
            requested_per_pane: requested,
            read_sizes: Summary::of(&[]),
            transport: Transport::Pty,
        }
    }

    #[test]
    fn transport_labels_are_distinct_and_named() {
        assert_ne!(Transport::Pty.label(), Transport::Pipe.label());
        assert!(Transport::Pty.label().contains("ConPTY"));
        assert!(Transport::Pipe.label().contains("headless"));
    }

    #[test]
    fn pane_throughput_is_bytes_over_its_own_elapsed_time() {
        assert_eq!(pane(0, 2_000, 2.0).bytes_per_sec(), 1_000.0);
    }

    #[test]
    fn a_zero_length_pane_reports_zero_not_infinity() {
        assert_eq!(pane(0, 500, 0.0).bytes_per_sec(), 0.0);
    }

    #[test]
    fn totals_sum_across_panes() {
        let r = result(vec![pane(0, 100, 1.0), pane(1, 400, 1.0)], 2.0, 250);
        assert_eq!(r.total_bytes(), 500);
        assert_eq!(r.aggregate_bytes_per_sec(), 250.0);
    }

    #[test]
    fn aggregate_of_a_zero_length_run_is_zero() {
        let r = result(vec![pane(0, 100, 0.0)], 0.0, 100);
        assert_eq!(r.aggregate_bytes_per_sec(), 0.0);
    }

    #[test]
    fn observed_ratio_compares_what_arrived_with_what_was_asked_for() {
        // Two panes asked for 100 each; 300 arrived, so the host added output.
        let r = result(vec![pane(0, 150, 1.0), pane(1, 150, 1.0)], 1.0, 100);
        assert_eq!(r.observed_ratio(), 1.5);
    }

    #[test]
    fn observed_ratio_of_a_zero_request_is_zero_not_a_division_by_zero() {
        assert_eq!(result(vec![pane(0, 10, 1.0)], 1.0, 0).observed_ratio(), 0.0);
    }

    #[test]
    fn failures_are_reported_rather_than_averaged_away() {
        let mut bad = pane(1, 0, 1.0);
        bad.error = Some("spawn failed".into());
        let r = result(vec![pane(0, 100, 1.0), bad], 1.0, 100);
        assert_eq!(r.failures().len(), 1);
        assert_eq!(r.failures()[0].index, 1);
    }

    #[test]
    fn a_run_with_no_failures_reports_none() {
        assert!(result(vec![pane(0, 100, 1.0)], 1.0, 100)
            .failures()
            .is_empty());
    }
}
