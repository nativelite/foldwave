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
