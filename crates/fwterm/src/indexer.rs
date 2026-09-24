//! Search signatures built on the GPU (design §8.7a/b), off the loggers.
//!
//! panelog gives each 1 MiB block of a pane's log a search signature. Built
//! inline, that work runs on every pane's logger thread and costs ~1.3 CPU
//! seconds per GiB. Here each logger defers it: every block it completes goes,
//! without waiting, to one indexer thread shared by all panes. The indexer
//! gathers blocks from every pane into one launch of at least 4 MiB where it
//! can (E1: launches that size leave frames alone), builds them on the GPU,
//! and sends each signature back to its logger, which stores them in order.
//!
//! The GPU is never on the ingest path: when the queue is full, or for a
//! generation's first block (no bytes before it), the logger builds the
//! signature itself, as before. Signatures still in flight when a generation
//! rolls over are rebuilt by panelog when that log is next opened.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Bytes of log per signature.
pub const BLOCK: usize = panelog::SIG_BLOCK as usize;

/// A completed block with the two bytes before it (`lead`), or `None` for a
/// log's first block, which has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub block: u64,
    pub lead: Option<[u8; 2]>,
    pub bytes: Vec<u8>,
}

/// Cuts a log's bytes, as they are appended, into whole blocks.
#[derive(Debug, Default)]
pub struct BlockStager {
    block: u64,
    buf: Vec<u8>,
    /// The last two bytes before `buf`, once there are two.
    lead: Option<[u8; 2]>,
}

impl BlockStager {
    /// Add appended bytes; returns the blocks they complete, in order.
    pub fn push(&mut self, mut bytes: &[u8]) -> Vec<Staged> {
        let mut out = Vec::new();
        while !bytes.is_empty() {
            let take = (BLOCK - self.buf.len()).min(bytes.len());
            self.buf.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buf.len() == BLOCK {
                let full = std::mem::replace(&mut self.buf, Vec::with_capacity(BLOCK));
                let next_lead = [full[BLOCK - 2], full[BLOCK - 1]];
                out.push(Staged {
                    block: self.block,
                    lead: self.lead,
                    bytes: full,
                });
                self.lead = Some(next_lead);
                self.block += 1;
            }
        }
        out
    }
}

/// One block on its way to the GPU, and where its signature goes.
pub struct Job<R> {
    pub reply: R,
    pub generation: u32,
    pub block: u64,
    pub lead: [u8; 2],
    pub bytes: Vec<u8>,
}

/// Blocks the indexer may hold before loggers build signatures themselves:
/// 128 MiB of memory at most (a batch gathering, a batch in flight).
const QUEUE: usize = 128;
/// Blocks per launch at most, and the fewest it waits for.
const MAX_BATCH: usize = 64;
const MIN_BATCH: usize = 32;
/// How long it waits to reach `MIN_BATCH` before launching what it has.
/// Signatures are not urgent (panelog rebuilds any missing when a log is
/// opened), and bigger launches cost the CPU less: 0.31 CPU-s per GiB at
/// 4 MiB, 0.08 at 64 MiB (design §8.7a).
const GATHER: Duration = Duration::from_millis(400);

/// Counts of signatures built each way, for `--stats`.
#[derive(Debug, Default)]
pub struct Counts {
    pub gpu: AtomicU64,
    pub cpu: AtomicU64,
    pub launches: AtomicU64,
}

/// The loggers' side of the indexer.
pub struct Indexer<R> {
    tx: SyncSender<Job<R>>,
    pub counts: Arc<Counts>,
}

impl<R> Clone for Indexer<R> {
    fn clone(&self) -> Self {
        Indexer {
            tx: self.tx.clone(),
            counts: self.counts.clone(),
        }
    }
}

impl<R: Send + 'static> Indexer<R> {
    /// Start the indexer if the GPU can take it. `deliver` hands a finished
    /// signature to its logger. `None` when there is no CUDA device or the
    /// kernel will not load: every logger then builds its own.
    pub fn start(deliver: impl Fn(&R, u32, u64, Vec<u8>) + Send + 'static) -> Option<Indexer<R>> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job<R>>(QUEUE);
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let counts = Arc::new(Counts::default());
        let c = counts.clone();
        std::thread::spawn(move || run(rx, deliver, ready_tx, c));
        ready.recv().ok().filter(|&ok| ok)?;
        Some(Indexer { tx, counts })
    }

    /// Queue a block without waiting; gives it back when the queue is full
    /// (or the indexer has stopped), and the caller builds it itself.
    pub fn submit(&self, job: Job<R>) -> Result<(), Job<R>> {
        match self.tx.try_send(job) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(j)) | Err(TrySendError::Disconnected(j)) => Err(j),
        }
    }
}

fn run<R>(
    rx: Receiver<Job<R>>,
    deliver: impl Fn(&R, u32, u64, Vec<u8>),
    ready: std::sync::mpsc::Sender<bool>,
    counts: Arc<Counts>,
) {
    let stride = BLOCK + 2;
    let setup = || -> Result<_, fwcuda::Error> {
        let cuda = fwcuda::Cuda::load()?;
        let dev = cuda.device(0)?;
        Ok((cuda, dev))
    };
    let Ok((cuda, dev)) = setup() else {
        let _ = ready.send(false);
        return;
    };
    let Ok(ctx) = fwcuda::Context::new_blocking(&cuda, &dev) else {
        let _ = ready.send(false);
        return;
    };
    let parts = (|| -> Result<_, fwcuda::Error> {
        Ok((
            fwcuda::TrigramSigs::new(&ctx)?,
            fwcuda::PinnedBuffer::new(&ctx, MAX_BATCH * stride)?,
            fwcuda::DeviceBuffer::new(&ctx, MAX_BATCH * stride)?,
            fwcuda::DeviceBuffer::new(&ctx, MAX_BATCH * fwcuda::SIG_BYTES)?,
        ))
    })();
    let Ok((kernel, mut host, dev_in, dev_out)) = parts else {
        let _ = ready.send(false);
        return;
    };
    let _ = ready.send(true);
    let mut sigs = vec![0u8; MAX_BATCH * fwcuda::SIG_BYTES];
    let mut batch: Vec<Job<R>> = Vec::with_capacity(MAX_BATCH);
    loop {
        // Wait for work, then gather more for up to GATHER or a full batch.
        match rx.recv() {
            Ok(j) => batch.push(j),
            Err(_) => return,
        }
        let until = Instant::now() + GATHER;
        while batch.len() < MAX_BATCH {
            match rx.try_recv() {
                Ok(j) => batch.push(j),
                Err(_) if batch.len() >= MIN_BATCH => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(_) => {
                    let now = Instant::now();
                    if now >= until {
                        break;
                    }
                    match rx.recv_timeout(until - now) {
                        Ok(j) => batch.push(j),
                        Err(_) => break,
                    }
                }
            }
        }
        let n = batch.len();
        {
            let h = host.as_mut_slice();
            for (k, j) in batch.iter().enumerate() {
                h[k * stride..k * stride + 2].copy_from_slice(&j.lead);
                h[k * stride + 2..(k + 1) * stride].copy_from_slice(&j.bytes);
            }
        }
        let built = (|| -> Result<(), fwcuda::Error> {
            ctx.copy_h2d_prefix(&dev_in, &host.as_slice()[..n * stride])?;
            kernel.build(&dev_in, 2, stride, n, &dev_out)?;
            ctx.copy_d2h_prefix(&mut sigs[..n * fwcuda::SIG_BYTES], &dev_out)
        })();
        if built.is_err() {
            // The GPU failed: hand the blocks back as never built; panelog
            // rebuilds their signatures when the log is opened. Stop.
            return;
        }
        counts.launches.fetch_add(1, Ordering::Relaxed);
        counts.gpu.fetch_add(n as u64, Ordering::Relaxed);
        for (k, j) in batch.drain(..).enumerate() {
            let sig = sigs[k * fwcuda::SIG_BYTES..(k + 1) * fwcuda::SIG_BYTES].to_vec();
            deliver(&j.reply, j.generation, j.block, sig);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| b"Error: Build FAILED at step \n"[i % 29])
            .collect()
    }

    #[test]
    fn the_stager_cuts_whole_blocks_with_their_lead_across_any_chunking() {
        let data = text(3 * BLOCK + 777);
        let mut s = BlockStager::default();
        let mut got = Vec::new();
        for chunk in data.chunks(65_537) {
            got.extend(s.push(chunk));
        }
        assert_eq!(got.len(), 3);
        for (k, b) in got.iter().enumerate() {
            assert_eq!(b.block, k as u64);
            assert_eq!(b.bytes, data[k * BLOCK..(k + 1) * BLOCK]);
            let want = (k > 0).then(|| [data[k * BLOCK - 2], data[k * BLOCK - 1]]);
            assert_eq!(b.lead, want);
        }
    }

    #[test]
    fn gathered_gpu_signatures_equal_panelogs() {
        let (tx, rx) = std::sync::mpsc::channel::<(u32, u64, Vec<u8>)>();
        let Some(ix) = Indexer::start(move |r: &std::sync::mpsc::Sender<_>, g, b, sig| {
            let _ = r.send((g, b, sig));
        }) else {
            return; // no CUDA here
        };
        // Blocks from two "panes", interleaved, as the loggers would send them.
        let (a, b) = (text(4 * BLOCK), {
            let mut v = text(4 * BLOCK);
            v.reverse();
            v
        });
        let mut want = std::collections::HashMap::new();
        let mut sent = 0;
        for (pane, data) in [(0u32, &a), (1u32, &b)] {
            let mut s = BlockStager::default();
            for st in s.push(data) {
                let Some(lead) = st.lead else { continue };
                want.insert((pane, st.block), panelog::signature(Some(lead), &st.bytes));
                let job = Job {
                    reply: tx.clone(),
                    generation: pane,
                    block: st.block,
                    lead,
                    bytes: st.bytes,
                };
                assert!(ix.submit(job).is_ok());
                sent += 1;
            }
        }
        for _ in 0..sent {
            let (g, blk, sig) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            assert_eq!(Some(&sig), want.get(&(g, blk)), "pane {g} block {blk}");
        }
        assert_eq!(ix.counts.gpu.load(Ordering::Relaxed), sent);
    }
}
