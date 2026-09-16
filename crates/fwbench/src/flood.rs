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
    let counter = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = vec![0u8; 64 * 1024];
        let mut bytes_read = 0u64;
        let mut reads = 0u64;
        let mut error = None;
        loop {
            match reader.read(&mut buf) {
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
        (bytes_read, reads, error)
    });

    // Wait for the child to finish emitting, then close the pseudoconsole so
    // the reader reaches end-of-stream. Dropping the `Pty` before the join is
    // required on Windows; the other order deadlocks (see `echo.rs`).
    let _ = pty.wait();
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
pub fn run(exe: &str, panes: usize, bytes_per_pane: u64, rows: u16, cols: u16) -> FloodResult {
    let started = Instant::now();
    let (tx, rx) = mpsc::channel();
    let mut handles = Vec::with_capacity(panes);

    for index in 0..panes {
        let exe = exe.to_string();
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let result = drain_pane(index, &exe, bytes_per_pane, rows, cols);
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
        }
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
