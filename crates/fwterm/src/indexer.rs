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
//!
//! A pane host's logger (design §8.8a) runs in another process, so its
//! blocks reach the indexer another way: the host announces each block once
//! it is durable, and a [`fetcher`](Indexer::fetcher) thread here opens the
//! log and queues the block; the indexer reads it straight into its launch
//! buffer.

use std::path::PathBuf;
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

/// What the indexer builds a signature from.
enum Work<R> {
    /// A block a logger here cut, bytes in hand.
    Staged(Job<R>),
    /// A block of a pane host's log, read straight into the launch's pinned
    /// buffer, lead included: no copy in between (design §8.8a).
    Log {
        reply: R,
        generation: u32,
        block: u64,
        log: Arc<panelog::ByteReader>,
    },
}

impl<R> Work<R> {
    fn target(&self) -> (&R, u32, u64) {
        match self {
            Work::Staged(j) => (&j.reply, j.generation, j.block),
            Work::Log {
                reply,
                generation,
                block,
                ..
            } => (reply, *generation, *block),
        }
    }
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
/// How long the fetcher gathers announced blocks before opening their logs:
/// an open reads every record header of the live segment (~0.65 ms for a
/// 256 MiB generation), about what signing a block on the CPU costs, so it
/// is paid once per gathering, not once per block.
const FETCH_GATHER: Duration = Duration::from_millis(100);

/// Counts of signatures built each way, for `--stats`.
#[derive(Debug, Default)]
pub struct Counts {
    pub gpu: AtomicU64,
    pub cpu: AtomicU64,
    pub launches: AtomicU64,
}

/// The loggers' side of the indexer.
pub struct Indexer<R> {
    tx: SyncSender<Work<R>>,
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
        let (tx, rx) = std::sync::mpsc::sync_channel::<Work<R>>(QUEUE);
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
        match self.try_send(Work::Staged(job)) {
            Ok(()) => Ok(()),
            Err(Work::Staged(j)) => Err(j),
            Err(Work::Log { .. }) => unreachable!("sent a staged block"),
        }
    }

    fn try_send(&self, work: Work<R>) -> Result<(), Work<R>> {
        match self.tx.try_send(work) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(w)) | Err(TrySendError::Disconnected(w)) => Err(w),
        }
    }
}

/// A block of a log another process is writing (a pane host's), announced
/// as durable, to read and sign here.
pub struct Remote<R> {
    pub reply: R,
    /// The generation's folder.
    pub dir: PathBuf,
    pub generation: u32,
    pub block: u64,
}

impl<R: Clone + Send + 'static> Indexer<R> {
    /// Start a thread that reads announced blocks from their logs and queues
    /// them for the GPU. It builds a signature itself, and hands it to
    /// `deliver`, for a generation's first block (no bytes before it) or when
    /// the GPU's queue is full. A block it cannot read is skipped: panelog
    /// rebuilds its signature when that log is opened.
    pub fn fetcher(
        &self,
        deliver: impl Fn(&R, u32, u64, Vec<u8>) + Send + 'static,
    ) -> SyncSender<Remote<R>> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Remote<R>>(QUEUE);
        let ix = self.clone();
        std::thread::spawn(move || {
            while let Ok(first) = rx.recv() {
                // Gather announcements for a while, then open each log once.
                let mut todo = vec![first];
                let until = Instant::now() + FETCH_GATHER;
                while todo.len() < MIN_BATCH {
                    let now = Instant::now();
                    if now >= until {
                        break;
                    }
                    match rx.recv_timeout(until - now) {
                        Ok(r) => todo.push(r),
                        Err(_) => break,
                    }
                }
                todo.extend(rx.try_iter());
                let mut logs: Vec<(PathBuf, Option<Arc<panelog::ByteReader>>)> = Vec::new();
                for r in todo {
                    let at = match logs.iter().position(|(d, _)| *d == r.dir) {
                        Some(k) => k,
                        None => {
                            let log = panelog::ByteReader::open(&r.dir).ok().map(Arc::new);
                            logs.push((r.dir.clone(), log));
                            logs.len() - 1
                        }
                    };
                    let Some(log) = &logs[at].1 else { continue };
                    if (r.block + 1) * BLOCK as u64 > log.len() {
                        continue; // not in the log: panelog rebuilds it
                    }
                    let local = if r.block == 0 {
                        // No bytes before it, so not in a GPU launch.
                        read_block(log, 0).map(|(lead, bytes)| panelog::signature(lead, &bytes))
                    } else {
                        let work = Work::Log {
                            reply: r.reply.clone(),
                            generation: r.generation,
                            block: r.block,
                            log: log.clone(),
                        };
                        match ix.try_send(work) {
                            Ok(()) => None,
                            // The GPU's queue is full: build it here.
                            Err(_) => read_block(log, r.block)
                                .map(|(lead, bytes)| panelog::signature(lead, &bytes)),
                        }
                    };
                    if let Some(sig) = local {
                        ix.counts.cpu.fetch_add(1, Ordering::Relaxed);
                        deliver(&r.reply, r.generation, r.block, sig);
                    }
                }
            }
        });
        tx
    }
}

/// Block `block` of `log` and the two bytes before it (none for block 0).
fn read_block(log: &panelog::ByteReader, block: u64) -> Option<(Option<[u8; 2]>, Vec<u8>)> {
    let start = block * BLOCK as u64;
    if block == 0 {
        let mut bytes = vec![0u8; BLOCK];
        log.read_exact_at(0, &mut bytes).ok()?;
        return Some((None, bytes));
    }
    let mut lead = [0u8; 2];
    log.read_exact_at(start - 2, &mut lead).ok()?;
    let mut bytes = vec![0u8; BLOCK];
    log.read_exact_at(start, &mut bytes).ok()?;
    Some((Some(lead), bytes))
}

fn run<R>(
    rx: Receiver<Work<R>>,
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
    let mut batch: Vec<Work<R>> = Vec::with_capacity(MAX_BATCH);
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
        {
            // Fill the pinned buffer; a host's block that cannot be read is
            // left out (panelog rebuilds its signature when the log opens).
            let h = host.as_mut_slice();
            let mut k = 0;
            batch.retain(|w| {
                let slot = &mut h[k * stride..(k + 1) * stride];
                let ok = match w {
                    Work::Staged(j) => {
                        slot[..2].copy_from_slice(&j.lead);
                        slot[2..].copy_from_slice(&j.bytes);
                        true
                    }
                    Work::Log { block, log, .. } => {
                        log.read_exact_at(block * BLOCK as u64 - 2, slot).is_ok()
                    }
                };
                k += ok as usize;
                ok
            });
        }
        let n = batch.len();
        if n == 0 {
            continue;
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
        for (k, w) in batch.drain(..).enumerate() {
            let sig = sigs[k * fwcuda::SIG_BYTES..(k + 1) * fwcuda::SIG_BYTES].to_vec();
            let (reply, generation, block) = w.target();
            deliver(reply, generation, block, sig);
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

    #[test]
    fn the_fetcher_signs_announced_blocks_of_a_log_written_elsewhere() {
        type Reply = std::sync::mpsc::Sender<(u32, u64, Vec<u8>)>;
        let deliver = |r: &Reply, g, b, sig| {
            let _ = r.send((g, b, sig));
        };
        let Some(ix) = Indexer::start(deliver) else {
            return; // no CUDA here
        };
        let fetch = ix.fetcher(deliver);
        let dir = std::env::temp_dir().join("fwterm-fetcher-test");
        let _ = std::fs::remove_dir_all(&dir);
        let data = text(5 * BLOCK + 1000);
        let mut w = panelog::Writer::create(&dir, panelog::Config::default()).unwrap();
        w.defer_signatures().unwrap();
        w.append(&data, 1).unwrap();
        w.flush().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // Every block announced, plus one that is not in the log: skipped.
        for block in 0..6 {
            let remote = Remote {
                reply: tx.clone(),
                dir: dir.clone(),
                generation: 4,
                block,
            };
            fetch.send(remote).unwrap();
        }
        let mut got = std::collections::BTreeMap::new();
        for _ in 0..5 {
            let (g, b, sig) = rx.recv_timeout(Duration::from_secs(20)).unwrap();
            assert_eq!(g, 4);
            got.insert(b, sig);
        }
        assert!(rx.recv_timeout(Duration::from_millis(800)).is_err());
        for (b, sig) in got {
            let k = b as usize;
            let lead = (k > 0).then(|| [data[k * BLOCK - 2], data[k * BLOCK - 1]]);
            let want = panelog::signature(lead, &data[k * BLOCK..(k + 1) * BLOCK]);
            assert_eq!(sig, want, "block {b}");
            w.add_signature(b, &sig).unwrap();
        }
        assert_eq!(w.signature_count(), 5);
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
