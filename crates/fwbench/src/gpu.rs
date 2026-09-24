//! Host <-> GPU copy cost: what it costs to hand parse work to the GPU.
//!
//! The GPU-fit half of E4 asks whether tokenizing can move to the GPU. Before
//! any kernel exists, the transfer has to be affordable: at the section 4
//! target the GPU would receive 273 MiB/s of pane output. This times
//! synchronous copies through the CUDA Driver API ([`fwcuda`], loaded from the
//! display driver at runtime, nothing redistributed), pinned and pageable, at
//! sizes from one pty read (64 KiB) to a large batch.

use crate::stats::Summary;
use std::time::{Duration, Instant};

/// Copy sizes measured, in bytes.
pub const SIZES: [usize; 4] = [64 << 10, 1 << 20, 16 << 20, 64 << 20];

/// One measured cell.
#[derive(Debug)]
pub struct Cell {
    /// Bytes per copy.
    pub bytes: usize,
    /// Page-locked host memory (true) or an ordinary allocation.
    pub pinned: bool,
    /// Host-to-device samples.
    pub h2d: Summary,
    /// Device-to-host samples.
    pub d2h: Summary,
}

/// Measure every size, pinned and pageable, `iterations` copies each way,
/// after `warm` of unrecorded copies to bring the GPU out of its idle clocks.
pub fn run(iterations: usize, warm: Duration) -> Result<(String, Vec<Cell>), fwcuda::Error> {
    let cuda = fwcuda::Cuda::load()?;
    let dev = cuda.device(0)?;
    let ctx = fwcuda::Context::new(&cuda, &dev)?;
    let label = format!(
        "{} ({:.1} GiB)",
        dev.name(),
        dev.total_mem() as f64 / (1u64 << 30) as f64
    );

    let t = Instant::now();
    while t.elapsed() < warm {
        fwcuda::copy_bench(&ctx, 16 << 20, 8, true)?;
    }

    let mut cells = Vec::new();
    for &bytes in &SIZES {
        for pinned in [true, false] {
            let s = fwcuda::copy_bench(&ctx, bytes, iterations, pinned)?;
            cells.push(Cell {
                bytes,
                pinned,
                h2d: Summary::of(&s.h2d),
                d2h: Summary::of(&s.d2h),
            });
        }
    }
    Ok((label, cells))
}

fn gib_s(bytes: usize, d: Duration) -> f64 {
    bytes as f64 / (1u64 << 30) as f64 / d.as_secs_f64().max(1e-12)
}

/// Render as markdown. The rate columns are at the median duration; the
/// latency columns show the per-copy tail.
pub fn render(label: &str, cells: &[Cell]) -> String {
    let mut o = format!(
        "gpu: {label}, synchronous copies, each sample = copy + cuCtxSynchronize\n\n\
         | size | host memory | H2D GiB/s (p50) | H2D p50 / p99 | D2H GiB/s (p50) | D2H p50 / p99 | n |\n\
         | --- | --- | --- | --- | --- | --- | --- |\n"
    );
    for c in cells {
        let size = if c.bytes >= 1 << 20 {
            format!("{} MiB", c.bytes >> 20)
        } else {
            format!("{} KiB", c.bytes >> 10)
        };
        o += &format!(
            "| {} | {} | {:.2} | {} / {} | {:.2} | {} / {} | {} |\n",
            size,
            if c.pinned { "pinned" } else { "pageable" },
            gib_s(c.bytes, c.h2d.p50),
            us(c.h2d.p50),
            us(c.h2d.p99),
            gib_s(c.bytes, c.d2h.p50),
            us(c.d2h.p50),
            us(c.d2h.p99),
            c.h2d.count,
        );
    }
    o
}

fn us(d: Duration) -> String {
    format!("{:.0} us", d.as_secs_f64() * 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_formats_sizes_and_rates() {
        let s = Summary::of(&[Duration::from_millis(1); 4]);
        let cells = [
            Cell {
                bytes: 64 << 10,
                pinned: true,
                h2d: s.clone(),
                d2h: s.clone(),
            },
            Cell {
                bytes: 1 << 30,
                pinned: false,
                h2d: s.clone(),
                d2h: s,
            },
        ];
        let out = render("GPU", &cells);
        assert!(out.contains("| 64 KiB | pinned | 0.06 |"));
        assert!(out.contains("| 1024 MiB | pageable | 1000.00 |"));
        assert!(out.contains("1000 us"));
    }

    #[test]
    fn runs_on_a_real_gpu_when_present() {
        match run(3, Duration::ZERO) {
            Ok((label, cells)) => {
                assert!(!label.is_empty());
                assert_eq!(cells.len(), SIZES.len() * 2);
            }
            Err(e) => eprintln!("skipping: no CUDA device ({e})"),
        }
    }
}

/// `gpuscan`: load for experiment E1. Counts newlines in `bytes` of text on
/// the GPU, launch after launch, for `run` (each launch timed, uploads
/// included when `upload`), and checks every result against the CPU's count.
pub fn scan(bytes: usize, run: Duration, upload: bool) -> Result<String, fwcuda::Error> {
    let cuda = fwcuda::Cuda::load()?;
    let dev = cuda.device(0)?;
    let ctx = fwcuda::Context::new(&cuda, &dev)?;
    let counter = fwcuda::ByteCounter::new(&ctx)?;
    // Text with a line break every 40-ish bytes, like agent output.
    let mut host = fwcuda::PinnedBuffer::new(&ctx, bytes)?;
    let mut x: u32 = 0x2545_F491;
    for (i, b) in host.as_mut_slice().iter_mut().enumerate() {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *b = if x % 41 == 0 || i % 160 == 159 {
            b'\n'
        } else {
            b' ' + (x % 94) as u8
        };
    }
    let want = host.as_slice().iter().filter(|&&b| b == b'\n').count() as u64;
    let data = fwcuda::DeviceBuffer::new(&ctx, bytes)?;
    ctx.copy_h2d(&data, host.as_slice())?;
    let mut times = Vec::new();
    let started = Instant::now();
    while started.elapsed() < run {
        let t = Instant::now();
        if upload {
            ctx.copy_h2d(&data, host.as_slice())?;
        }
        let got = counter.count(&data, b'\n')?;
        times.push(t.elapsed());
        if got != want {
            return Err(fwcuda::Error::Corrupted {
                offset: got as usize,
            });
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let s = Summary::of(&times);
    let gib = (bytes as f64 * times.len() as f64) / (1u64 << 30) as f64;
    Ok(format!(
        "gpuscan  {} | {} MiB per launch{} | {} launches in {:.1} s | {:.1} GiB/s scanned\n\
         launch   {}\n",
        dev.name(),
        bytes >> 20,
        if upload { " + upload" } else { "" },
        times.len(),
        secs,
        gib / secs,
        s.line(),
    ))
}

/// Bytes of log per search signature, and bytes per signature (panelog's).
const SIG_BLOCK: usize = fwcuda::SIG_BLOCK;
const SIG_BYTES: usize = fwcuda::SIG_BYTES;

/// panelog's search signatures on one CPU thread, the same way its writer
/// builds them: block `k` gets a bit for every case-folded trigram that ends
/// in it. Whole blocks only.
pub fn cpu_sigs(data: &[u8]) -> Vec<u8> {
    let blocks = data.len() / SIG_BLOCK;
    let mut out = vec![0u8; blocks * SIG_BYTES];
    sigs_into(data, 0, &mut out);
    out
}

/// Signatures of blocks `first..first + out.len() / SIG_BYTES` into `out`.
fn sigs_into(data: &[u8], first: usize, out: &mut [u8]) {
    for (k, sig) in out.chunks_mut(SIG_BYTES).enumerate() {
        let start = (first + k) * SIG_BLOCK;
        // The log's first two bytes end no trigram.
        let from = start.max(2);
        let (mut a, mut b) = (
            data[from - 2].to_ascii_lowercase(),
            data[from - 1].to_ascii_lowercase(),
        );
        for &raw in &data[from..start + SIG_BLOCK] {
            let c = raw.to_ascii_lowercase();
            let t = (a as u32) | (b as u32) << 8 | (c as u32) << 16;
            let i = (t.wrapping_mul(0x9E37_79B1) >> 16) as usize;
            sig[i >> 3] |= 1 << (i & 7);
            a = b;
            b = c;
        }
    }
}

/// The same on `threads` threads, blocks split evenly.
pub fn cpu_sigs_parallel(data: &[u8], threads: usize) -> Vec<u8> {
    let blocks = data.len() / SIG_BLOCK;
    let mut out = vec![0u8; blocks * SIG_BYTES];
    let per = blocks.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (t, part) in out.chunks_mut(per * SIG_BYTES).enumerate() {
            s.spawn(move || sigs_into(data, t * per, part));
        }
    });
    out
}

/// One way of building the signatures, measured.
#[derive(Debug)]
pub struct IndexRun {
    /// What was built, and how.
    pub label: String,
    /// Wall time.
    pub secs: f64,
    /// CPU time this process spent, all threads.
    pub cpu_secs: f64,
}

type Built = (IndexRun, Vec<u8>);

fn timed(
    label: String,
    f: impl FnOnce() -> Result<Vec<u8>, fwcuda::Error>,
) -> Result<Built, fwcuda::Error> {
    let cpu0 = resmon::cpu::times().map_or(0, |t| t.process);
    let t = Instant::now();
    let sigs = f()?;
    let secs = t.elapsed().as_secs_f64();
    let cpu = resmon::cpu::times().map_or(0, |t| t.process);
    Ok((
        IndexRun {
            label,
            secs,
            cpu_secs: cpu.saturating_sub(cpu0) as f64 / 1e9,
        },
        sigs,
    ))
}

fn same(got: &[u8], want: &[u8]) -> Result<(), fwcuda::Error> {
    match got.iter().zip(want).position(|(x, y)| x != y) {
        None if got.len() == want.len() => Ok(()),
        at => Err(fwcuda::Error::Corrupted {
            offset: at.unwrap_or(got.len().min(want.len())),
        }),
    }
}

/// `gpuindex`: build the signatures of `mib` MiB of agent-like output on one
/// CPU thread, on every core, and on the GPU (upload, kernel, download) at
/// each batch size in `batches_mib`. Every result must equal the one-thread
/// build.
pub fn index_trial(
    mib: usize,
    batches_mib: &[usize],
) -> Result<(String, Vec<IndexRun>), fwcuda::Error> {
    let cuda = fwcuda::Cuda::load()?;
    let dev = cuda.device(0)?;
    let ctx = fwcuda::Context::new(&cuda, &dev)?;
    let kernel = fwcuda::TrigramSigs::new(&ctx)?;
    let bytes = mib * SIG_BLOCK;
    let mut host = fwcuda::PinnedBuffer::new(&ctx, bytes)?;
    host.as_mut_slice()
        .copy_from_slice(&crate::parse::workload(7, bytes)[..bytes]);
    let data = host.as_slice();
    let mut runs = Vec::new();

    let (run, want) = timed("CPU, 1 thread".into(), || Ok(cpu_sigs(data)))?;
    runs.push(run);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let (run, got) = timed(format!("CPU, {threads} threads"), || {
        Ok(cpu_sigs_parallel(data, threads))
    })?;
    same(&got, &want)?;
    runs.push(run);

    for &batch in batches_mib {
        let first = fwcuda::DeviceBuffer::new(&ctx, batch * SIG_BLOCK)?;
        let rest = fwcuda::DeviceBuffer::new(&ctx, batch * SIG_BLOCK + 2)?;
        let out = fwcuda::DeviceBuffer::new(&ctx, batch * SIG_BYTES)?;
        // Warm the kernel and the copies once, unrecorded.
        ctx.copy_h2d(&first, &data[..batch * SIG_BLOCK])?;
        kernel.build(&first, 0, batch, &out)?;
        let (run, got) = timed(format!("GPU, {batch} MiB per launch"), || {
            let mut all = vec![0u8; mib * SIG_BYTES];
            let mut part = vec![0u8; batch * SIG_BYTES];
            let mut k = 0;
            while k < mib {
                let n = batch.min(mib - k);
                let start = k * SIG_BLOCK;
                if n < batch {
                    // A short last batch goes to the CPU, so every launch
                    // stays at the batch size (E1's rule).
                    sigs_into(data, k, &mut all[k * SIG_BYTES..(k + n) * SIG_BYTES]);
                } else {
                    if k == 0 {
                        ctx.copy_h2d(&first, &data[..n * SIG_BLOCK])?;
                        kernel.build(&first, 0, n, &out)?;
                    } else {
                        ctx.copy_h2d(&rest, &data[start - 2..start + n * SIG_BLOCK])?;
                        kernel.build(&rest, 2, n, &out)?;
                    }
                    ctx.copy_d2h(&mut part, &out)?;
                    all[k * SIG_BYTES..(k + n) * SIG_BYTES].copy_from_slice(&part);
                }
                k += n;
            }
            Ok(all)
        })?;
        same(&got, &want)?;
        runs.push(run);
    }
    let head = format!(
        "gpuindex  {} | {mib} MiB of agent-like output | every build equal to the one-thread CPU build\n",
        dev.name()
    );
    Ok((head, runs))
}

/// The table for [`index_trial`].
pub fn render_index(head: &str, mib: usize, runs: &[IndexRun]) -> String {
    let gib = mib as f64 / 1024.0;
    let mut s = String::from(head);
    s.push_str("| Build | GiB/s | CPU-s per GiB |\n| --- | --- | --- |\n");
    for r in runs {
        s.push_str(&format!(
            "| {} | {:.2} | {:.3} |\n",
            r.label,
            gib / r.secs,
            r.cpu_secs / gib
        ));
    }
    s
}

#[cfg(test)]
mod index_tests {
    use super::*;

    /// panelog's own signatures, read back from a log it wrote: the
    /// definition every other build must match.
    fn panelog_sigs(data: &[u8]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("fwbench-sigs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut w = panelog::Writer::create(&dir, panelog::Config::default()).unwrap();
            for chunk in data.chunks(70_001) {
                w.append(chunk, 0).unwrap();
            }
        }
        let raw = std::fs::read(dir.join("search.sig")).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        raw.chunks(SIG_BYTES + 4)
            .flat_map(|e| e[..SIG_BYTES].to_vec())
            .collect()
    }

    #[test]
    fn cpu_and_gpu_signatures_equal_panelogs() {
        let mut data = crate::parse::workload(3, 8 * SIG_BLOCK + 12345);
        // Upper case too, so the folding is exercised.
        for b in data.iter_mut().step_by(7) {
            *b = b.to_ascii_uppercase();
        }
        let want = panelog_sigs(&data);
        assert_eq!(want.len(), 8 * SIG_BYTES);
        assert_eq!(cpu_sigs(&data), want, "1 thread");
        assert_eq!(cpu_sigs_parallel(&data, 3), want, "3 threads");
        // The GPU, where there is one.
        let Ok(cuda) = fwcuda::Cuda::load() else {
            return;
        };
        let Ok(dev) = cuda.device(0) else { return };
        let ctx = fwcuda::Context::new(&cuda, &dev).unwrap();
        let k = fwcuda::TrigramSigs::new(&ctx).unwrap();
        let first = fwcuda::DeviceBuffer::new(&ctx, 4 * SIG_BLOCK).unwrap();
        let rest = fwcuda::DeviceBuffer::new(&ctx, 4 * SIG_BLOCK + 2).unwrap();
        let out = fwcuda::DeviceBuffer::new(&ctx, 4 * SIG_BYTES).unwrap();
        let mut got = vec![0u8; 8 * SIG_BYTES];
        ctx.copy_h2d(&first, &data[..4 * SIG_BLOCK]).unwrap();
        k.build(&first, 0, 4, &out).unwrap();
        ctx.copy_d2h(&mut got[..4 * SIG_BYTES], &out).unwrap();
        ctx.copy_h2d(&rest, &data[4 * SIG_BLOCK - 2..8 * SIG_BLOCK])
            .unwrap();
        k.build(&rest, 2, 4, &out).unwrap();
        ctx.copy_d2h(&mut got[4 * SIG_BYTES..], &out).unwrap();
        assert_eq!(got, want, "GPU");
    }
}

/// `gpuindex-load`: GPU load for the frame check of the indexer trial.
/// Uploads `batch_mib` MiB and builds its signatures, again and again, for
/// `run`; returns launches and GiB/s.
pub fn index_load(batch_mib: usize, run: Duration) -> Result<String, fwcuda::Error> {
    let cuda = fwcuda::Cuda::load()?;
    let dev = cuda.device(0)?;
    let ctx = fwcuda::Context::new(&cuda, &dev)?;
    let kernel = fwcuda::TrigramSigs::new(&ctx)?;
    let bytes = batch_mib * SIG_BLOCK + 2;
    let mut host = fwcuda::PinnedBuffer::new(&ctx, bytes)?;
    host.as_mut_slice()
        .copy_from_slice(&crate::parse::workload(5, bytes)[..bytes]);
    let data = fwcuda::DeviceBuffer::new(&ctx, bytes)?;
    let out = fwcuda::DeviceBuffer::new(&ctx, batch_mib * SIG_BYTES)?;
    let mut sigs = vec![0u8; batch_mib * SIG_BYTES];
    let (started, mut launches) = (Instant::now(), 0u64);
    while started.elapsed() < run {
        ctx.copy_h2d(&data, host.as_slice())?;
        kernel.build(&data, 2, batch_mib, &out)?;
        ctx.copy_d2h(&mut sigs, &out)?;
        launches += 1;
    }
    let secs = started.elapsed().as_secs_f64();
    Ok(format!(
        "gpuindex-load {batch_mib} MiB per launch: {launches} launches, {:.2} GiB/s\n",
        launches as f64 * batch_mib as f64 / 1024.0 / secs
    ))
}
