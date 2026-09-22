//! Read rate of the log drive, and what NTFS compression costs to read back.
//!
//! The log is written once and read many times: replay, search, rebuilding a
//! pane's screen from a checkpoint. `disk` measures writing; this measures
//! reading the same file back two ways:
//!
//! - **unbuffered** (`FILE_FLAG_NO_BUFFERING`), which bypasses the Windows
//!   page cache, so the number is the drive plus the filesystem, not RAM;
//! - **buffered**, the ordinary path, which the page cache may serve entirely
//!   from RAM — reported as a ceiling, never as a drive figure.
//!
//! Each pass also records the CPU time this process itself burned, reported
//! per GiB. That is where transparent decompression shows up: a compressed
//! file is unpacked by the CPU on every read.
//!
//! Run it against a compressed and an uncompressed directory to price
//! compression. The report states which the directory is, read from the
//! file's own `FILE_ATTRIBUTE_COMPRESSED` bit.

use crate::disk::{aligned, SECTOR};
use crate::stats::Summary;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// What to read, and from where.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory to probe. The file is written there and removed afterwards.
    pub dir: PathBuf,
    /// Size of the probe file in bytes.
    pub bytes: u64,
    /// Bytes per read call; must be a multiple of [`SECTOR`].
    pub io: usize,
    /// Bytes of throwaway data written before reading, to push the probe out
    /// of the page cache. 0 skips it, and the report says the passes were warm.
    pub flush_bytes: u64,
    /// Where the flush file is written. Point it at an uncompressed directory:
    /// eviction is RAM-wide, so it need not share the probe's directory, and a
    /// compressed one only makes the flush slow.
    pub flush_dir: Option<PathBuf>,
}

/// One pass over the file.
#[derive(Debug, Clone)]
pub struct Pass {
    /// How the file was opened.
    pub label: &'static str,
    /// Bytes read.
    pub bytes: u64,
    /// Wall time of the whole pass.
    pub elapsed: Duration,
    /// Per-read durations.
    pub reads: Vec<Duration>,
    /// CPU seconds this process used during the pass, or `None` if unknown.
    pub cpu_secs: Option<f64>,
}

impl Pass {
    /// Sustained MiB/s over the pass.
    pub fn mib_per_sec(&self) -> f64 {
        self.bytes as f64 / (1024.0 * 1024.0) / self.elapsed.as_secs_f64().max(1e-12)
    }
    /// CPU seconds spent per GiB read.
    pub fn cpu_per_gib(&self) -> Option<f64> {
        let gib = self.bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        self.cpu_secs.map(|c| c / gib.max(1e-12))
    }
}

/// The whole measurement.
#[derive(Debug, Clone)]
pub struct Report {
    /// Directory measured.
    pub dir: PathBuf,
    /// Whether the probe file carried `FILE_ATTRIBUTE_COMPRESSED`.
    pub compressed: Option<bool>,
    /// Bytes written to evict the page cache before reading. 0 means the
    /// passes may have been served from RAM.
    pub flushed: u64,
    /// Bytes the file occupied on disk, when the platform reports it.
    pub passes: Vec<Pass>,
}

/// True when the file has NTFS's compressed attribute. `None` off Windows.
pub fn is_compressed(path: &Path) -> Option<bool> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
        fs::metadata(path)
            .ok()
            .map(|m| m.file_attributes() & FILE_ATTRIBUTE_COMPRESSED != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

fn own_cpu() -> Option<f64> {
    let snap = resmon::sys::snapshot().ok()?;
    crate::noise::own_cpu_secs(&snap, std::process::id())
}

/// Write the probe file, read it back both ways, then remove it.
pub fn run(config: &Config) -> io::Result<Report> {
    if config.io == 0 || config.io % SECTOR != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("read size must be a non-zero multiple of {SECTOR}"),
        ));
    }
    fs::create_dir_all(&config.dir)?;
    let path = config.dir.join("fwbench-read.probe");
    write_probe(&path, config.bytes, config.io)?;
    let compressed = is_compressed(&path);
    if config.flush_bytes > 0 {
        let dir = config
            .flush_dir
            .clone()
            .unwrap_or_else(|| config.dir.clone());
        flush_cache(&dir, config.flush_bytes, config.io)?;
    }

    let result = (|| -> io::Result<Vec<Pass>> {
        let unbuffered = pass("unbuffered (no page cache)", &path, config.io, true)?;
        let buffered = pass(
            "buffered (page cache may serve it)",
            &path,
            config.io,
            false,
        )?;
        Ok(vec![unbuffered, buffered])
    })();
    let _ = fs::remove_file(&path);

    Ok(Report {
        dir: config.dir.clone(),
        compressed,
        flushed: config.flush_bytes,
        passes: result?,
    })
}

/// Write and delete `bytes` of throwaway data so the page cache evicts what it
/// held before. Cheaper than any privileged cache-drop call, and it needs no
/// elevation.
fn flush_cache(dir: &Path, bytes: u64, io_size: usize) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join("fwbench-read.flush");
    let r = write_probe(&path, bytes, io_size);
    let _ = fs::remove_file(&path);
    r
}

fn write_probe(path: &Path, bytes: u64, io_size: usize) -> io::Result<()> {
    let buf = aligned(io_size);
    let mut f = File::create(path)?;
    let mut written = 0u64;
    while written < bytes {
        let want = io_size.min((bytes - written) as usize);
        f.write_all(&buf.aligned()[..want])?;
        written += want as u64;
    }
    f.sync_all()
}

fn pass(label: &'static str, path: &Path, io_size: usize, unbuffered: bool) -> io::Result<Pass> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(windows)]
    if unbuffered {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        opts.custom_flags(FILE_FLAG_NO_BUFFERING);
    }
    #[cfg(not(windows))]
    let _ = unbuffered;

    let mut file = opts.open(path)?;
    let mut buf = aligned(io_size);
    let mut reads = Vec::new();
    let mut bytes = 0u64;

    let cpu_before = own_cpu();
    let started = Instant::now();
    loop {
        let t = Instant::now();
        let n = file.read(buf.aligned_mut())?;
        let d = t.elapsed();
        if n == 0 {
            break;
        }
        reads.push(d);
        bytes += n as u64;
    }
    let elapsed = started.elapsed();
    let cpu_secs = match (cpu_before, own_cpu()) {
        (Some(a), Some(b)) => Some((b - a).max(0.0)),
        _ => None,
    };

    Ok(Pass {
        label,
        bytes,
        elapsed,
        reads,
        cpu_secs,
    })
}

/// Render the report as markdown.
pub fn render(report: &Report) -> String {
    let state = match report.compressed {
        Some(true) => "NTFS-compressed",
        Some(false) => "not compressed",
        None => "compression state unknown",
    };
    let cold = if report.flushed > 0 {
        format!(
            "{} MiB written first to evict the page cache",
            report.flushed / (1024 * 1024)
        )
    } else {
        "page cache NOT evicted: treat both passes as warm".to_string()
    };
    let mut o = format!(
        "read: {} ({state}; {cold})\n\n| pass | MiB/s | read p50 | p90 | p99 | CPU s/GiB | reads |\n| --- | --- | --- | --- | --- | --- | --- |\n",
        report.dir.display(),
    );
    for p in &report.passes {
        let s = Summary::of(&p.reads);
        o += &format!(
            "| {} | {:.1} | {} | {} | {} | {} | {} |\n",
            p.label,
            p.mib_per_sec(),
            crate::stats::ms(s.p50),
            crate::stats::ms(s.p90),
            crate::stats::ms(s.p99),
            match p.cpu_per_gib() {
                Some(c) => format!("{c:.2}"),
                None => "-".into(),
            },
            s.count,
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fwbench-read-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn reads_the_file_back_both_ways() {
        let dir = tmp("both");
        let cfg = Config {
            dir: dir.clone(),
            bytes: 4 * 1024 * 1024,
            io: 1024 * 1024,
            flush_bytes: 2 * 1024 * 1024,
            flush_dir: None,
        };
        let r = run(&cfg).expect("read probe");
        assert_eq!(r.passes.len(), 2);
        for p in &r.passes {
            assert_eq!(p.bytes, cfg.bytes, "{} read the whole file", p.label);
            assert!(p.mib_per_sec() > 0.0);
            assert_eq!(p.reads.len(), 4);
        }
        assert_eq!(r.flushed, 2 * 1024 * 1024);
        assert!(!dir.join("fwbench-read.probe").exists(), "probe removed");
        assert!(
            !dir.join("fwbench-read.flush").exists(),
            "flush file removed"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_unaligned_read_size() {
        let cfg = Config {
            dir: tmp("bad"),
            bytes: SECTOR as u64,
            io: SECTOR + 1,
            flush_bytes: 0,
            flush_dir: None,
        };
        assert!(run(&cfg).is_err());
    }

    #[test]
    fn compression_state_is_reported() {
        let dir = tmp("attr");
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plain");
        fs::write(&f, b"hello").unwrap();
        if cfg!(windows) {
            // A file just created in a temp directory is normally not
            // compressed; the call must at least answer.
            assert!(is_compressed(&f).is_some());
        }
        assert_eq!(is_compressed(&dir.join("missing")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rates_and_cpu_are_derived_per_pass() {
        let p = Pass {
            label: "x",
            bytes: 2 * 1024 * 1024 * 1024,
            elapsed: Duration::from_secs(2),
            reads: vec![Duration::from_millis(1); 4],
            cpu_secs: Some(3.0),
        };
        assert!((p.mib_per_sec() - 1024.0).abs() < 1e-6);
        assert!((p.cpu_per_gib().unwrap() - 1.5).abs() < 1e-6);
    }
}
