//! `fwbench disk` — how fast the log's drive really is, measured the way the
//! log intends to use it.
//!
//! The first E: figure in `docs/design.md` §4.1 (479 MiB/s) came from an
//! ad-hoc probe: buffered 256 KiB appends through the page cache with an
//! `fsync` every 16 MiB, one thread, queue depth 1. That measures the cost of
//! a flush round-trip, not the drive. This module runs that pattern as the
//! control and puts it next to the patterns a log writer would actually use:
//!
//! | Pattern | What it measures |
//! | --- | --- |
//! | `buffered+fsync` | the old probe, for continuity |
//! | `writethrough qd1` | unbuffered write-through appends, one in flight: durable on return, no page cache, no fsync |
//! | `writethrough qdN` | the same with N in flight (N threads, one handle, positioned writes): what the device does with a queue |
//!
//! Every write is timed individually so the report carries p50/p90/p99 and not
//! just a rate. Files are created under the directory being measured and
//! removed afterwards. All std, no unsafe: the flags go through
//! `OpenOptionsExt::custom_flags`, and the sector-aligned buffer is a slice
//! into an over-allocated `Vec`.

use crate::stats::Summary;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Alignment for unbuffered I/O: address, length and offset must all be
/// multiples of the physical sector size. 4 KiB covers every drive we target.
pub const SECTOR: usize = 4096;

/// One access pattern to measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    /// Buffered appends of `io` bytes with an `fsync` every `flush_every`
    /// bytes. The original probe.
    BufferedFsync {
        /// Bytes per `write` call.
        io: usize,
        /// Bytes between `fsync` calls.
        flush_every: u64,
    },
    /// Unbuffered write-through appends of `io` bytes, `qd` in flight.
    /// `qd == 1` appends on one thread; `qd > 1` preallocates the file and
    /// overwrites it in place from `qd` threads so NTFS never zero-fills a
    /// valid-data-length gap.
    WriteThrough {
        /// Bytes per `write` call; a multiple of [`SECTOR`].
        io: usize,
        /// Writes in flight.
        qd: usize,
    },
}

impl Pattern {
    /// Short name for the report.
    pub fn name(&self) -> String {
        match self {
            Pattern::BufferedFsync { io, flush_every } => format!(
                "buffered+fsync/{}MiB ({}KiB io)",
                flush_every / (1024 * 1024),
                io / 1024
            ),
            Pattern::WriteThrough { io, qd } => {
                format!("writethrough qd{qd} ({}KiB io)", io / 1024)
            }
        }
    }

    /// The matrix `fwbench disk` runs by default: the old probe as control,
    /// then write-through at queue depth 1 and `qd`.
    pub fn default_matrix(io: usize, qd: usize) -> Vec<Pattern> {
        vec![
            Pattern::BufferedFsync {
                io: 256 * 1024,
                flush_every: 16 * 1024 * 1024,
            },
            Pattern::WriteThrough { io, qd: 1 },
            Pattern::WriteThrough { io, qd },
        ]
    }
}

/// The result of one pattern.
#[derive(Debug, Clone)]
pub struct DiskResult {
    /// Which pattern.
    pub pattern: Pattern,
    /// Bytes written inside the timed region.
    pub bytes: u64,
    /// Wall time of the timed region.
    pub elapsed: Duration,
    /// Latency of each `write` call.
    pub write: Summary,
    /// Latency of each `fsync`, for patterns that issue one.
    pub fsync: Option<Summary>,
}

impl DiskResult {
    /// Sustained bytes per second.
    pub fn bytes_per_sec(&self) -> f64 {
        if self.elapsed.is_zero() {
            return 0.0;
        }
        self.bytes as f64 / self.elapsed.as_secs_f64()
    }
}

/// What to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Directory on the drive under test; the probe file is created here.
    pub dir: PathBuf,
    /// Bytes to write per pattern. Rounded down to a multiple of the IO size.
    pub bytes: u64,
    /// The patterns, in order.
    pub patterns: Vec<Pattern>,
}

/// Run every pattern in `config` and return one result per pattern.
pub fn run(config: &Config) -> io::Result<Vec<DiskResult>> {
    let mut results = Vec::with_capacity(config.patterns.len());
    for (n, pattern) in config.patterns.iter().enumerate() {
        let path = config
            .dir
            .join(format!("fwbench-disk-{}-{n}.tmp", std::process::id()));
        let result = run_one(&path, config.bytes, pattern);
        // Best effort: a failed unlink is not a measurement failure.
        let _ = std::fs::remove_file(&path);
        results.push(result?);
    }
    Ok(results)
}

fn run_one(path: &Path, bytes: u64, pattern: &Pattern) -> io::Result<DiskResult> {
    match *pattern {
        Pattern::BufferedFsync { io, flush_every } => buffered_fsync(path, bytes, io, flush_every),
        Pattern::WriteThrough { io, qd } => {
            if io % SECTOR != 0 || io == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("io size {io} is not a multiple of {SECTOR}"),
                ));
            }
            if qd <= 1 {
                writethrough_append(path, bytes, io)
            } else {
                writethrough_queued(path, bytes, io, qd)
            }
        }
    }
    .map(|mut r| {
        r.pattern = pattern.clone();
        r
    })
}

/// The original probe: buffered appends, periodic `fsync`.
fn buffered_fsync(path: &Path, bytes: u64, io: usize, flush_every: u64) -> io::Result<DiskResult> {
    let mut file = File::create(path)?;
    let buf = filler(io);
    let total = bytes - bytes % io as u64;
    let mut written = 0u64;
    let mut since_flush = 0u64;
    let mut writes = Vec::with_capacity((total / io as u64) as usize);
    let mut fsyncs = Vec::new();

    let started = Instant::now();
    while written < total {
        let t = Instant::now();
        file.write_all(&buf)?;
        writes.push(t.elapsed());
        written += io as u64;
        since_flush += io as u64;
        if since_flush >= flush_every {
            let t = Instant::now();
            file.sync_data()?;
            fsyncs.push(t.elapsed());
            since_flush = 0;
        }
    }
    if since_flush > 0 {
        let t = Instant::now();
        file.sync_data()?;
        fsyncs.push(t.elapsed());
    }
    let elapsed = started.elapsed();

    Ok(DiskResult {
        pattern: Pattern::BufferedFsync { io, flush_every },
        bytes: written,
        elapsed,
        write: Summary::of(&writes),
        fsync: Some(Summary::of(&fsyncs)),
    })
}

/// Open for unbuffered write-through: the write returns when the bytes are on
/// the device, and the page cache is never involved.
pub(crate) fn open_writethrough(path: &Path, create: bool) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true);
    if create {
        opts.create(true).truncate(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;
        const FILE_FLAG_WRITE_THROUGH: u32 = 0x8000_0000;
        opts.custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_DIRECT: i32 = 0x4000;
        const O_DSYNC: i32 = 0x1000;
        opts.custom_flags(O_DIRECT | O_DSYNC);
    }
    opts.open(path)
}

/// Write-through appends, one in flight: the log writer's own pattern.
fn writethrough_append(path: &Path, bytes: u64, io: usize) -> io::Result<DiskResult> {
    let mut file = open_writethrough(path, true)?;
    let storage = aligned(io);
    let buf = storage.aligned();
    let total = bytes - bytes % io as u64;
    let mut written = 0u64;
    let mut writes = Vec::with_capacity((total / io as u64) as usize);

    let started = Instant::now();
    while written < total {
        let t = Instant::now();
        file.write_all(buf)?;
        writes.push(t.elapsed());
        written += io as u64;
    }
    let elapsed = started.elapsed();

    Ok(DiskResult {
        pattern: Pattern::WriteThrough { io, qd: 1 },
        bytes: written,
        elapsed,
        write: Summary::of(&writes),
        fsync: None,
    })
}

/// Write-through with `qd` in flight from `qd` threads sharing one handle.
///
/// The file is filled once, untimed, so the timed pass overwrites in place.
/// Positioned writes are handed out from a shared counter, so the threads
/// stay within `qd` chunks of each other and the device sees a nearly
/// sequential stream with `qd` requests queued — the shape an overlapped
/// single-threaded writer would produce.
fn writethrough_queued(path: &Path, bytes: u64, io: usize, qd: usize) -> io::Result<DiskResult> {
    let total = bytes - bytes % io as u64;
    // Untimed prefill so no write in the timed pass extends the file.
    writethrough_append(path, total, io)?;

    let file = Arc::new(open_writethrough(path, false)?);
    let next = Arc::new(AtomicU64::new(0));
    let chunks = total / io as u64;
    let samples: Arc<Mutex<Vec<Duration>>> =
        Arc::new(Mutex::new(Vec::with_capacity(chunks as usize)));
    let failure: Arc<Mutex<Option<io::Error>>> = Arc::new(Mutex::new(None));

    let started = Instant::now();
    let workers: Vec<_> = (0..qd)
        .map(|_| {
            let file = Arc::clone(&file);
            let next = Arc::clone(&next);
            let samples = Arc::clone(&samples);
            let failure = Arc::clone(&failure);
            std::thread::spawn(move || {
                let storage = aligned(io);
                let buf = storage.aligned();
                let mut mine = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= chunks {
                        break;
                    }
                    let t = Instant::now();
                    if let Err(e) = write_at(&file, buf, i * io as u64) {
                        *failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
                        break;
                    }
                    mine.push(t.elapsed());
                }
                samples
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend(mine);
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    let elapsed = started.elapsed();

    if let Some(e) = failure.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(e);
    }
    let samples = samples.lock().unwrap_or_else(|p| p.into_inner());
    Ok(DiskResult {
        pattern: Pattern::WriteThrough { io, qd },
        bytes: samples.len() as u64 * io as u64,
        elapsed,
        write: Summary::of(&samples),
        fsync: None,
    })
}

/// Positioned write of the whole buffer, without moving a shared cursor.
fn write_at(file: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (file, buf, offset);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "positioned writes are not supported on this platform",
        ))
    }
}

/// A sector-aligned buffer, without unsafe: over-allocate and slice.
pub struct Aligned {
    storage: Vec<u8>,
    start: usize,
    len: usize,
}

impl Aligned {
    /// The aligned view.
    pub fn aligned(&self) -> &[u8] {
        &self.storage[self.start..self.start + self.len]
    }

    /// The aligned view, mutable, for reads into it.
    pub fn aligned_mut(&mut self) -> &mut [u8] {
        &mut self.storage[self.start..self.start + self.len]
    }
}

/// Allocate `len` bytes of agent-like filler at a [`SECTOR`]-aligned address.
pub fn aligned(len: usize) -> Aligned {
    let storage = filler(len + SECTOR);
    let misalign = storage.as_ptr() as usize % SECTOR;
    let start = if misalign == 0 { 0 } else { SECTOR - misalign };
    Aligned {
        storage,
        start,
        len,
    }
}

/// Bytes that look like terminal output rather than zeros, so a drive that
/// compresses or dedups zero pages cannot flatter the result.
fn filler(len: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(len);
    crate::emit::Generator::new(0xD15C_0DE5).fill(&mut buf, len);
    buf.truncate(len);
    buf
}

/// Render the results as the report table.
pub fn render(dir: &Path, bytes: u64, results: &[DiskResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "disk: {} written per pattern under {}\n\n",
        crate::report::mib(bytes),
        dir.display()
    ));
    out.push_str("| pattern | sustained | write p50 | p90 | p99 | fsync p50 | p99 |\n");
    out.push_str("| --- | --- | --- | --- | --- | --- | --- |\n");
    for r in results {
        let (f50, f99) = match &r.fsync {
            Some(f) => (crate::stats::ms(f.p50), crate::stats::ms(f.p99)),
            None => ("-".to_string(), "-".to_string()),
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            r.pattern.name(),
            crate::report::mib_per_sec(r.bytes_per_sec()),
            crate::stats::ms(r.write.p50),
            crate::stats::ms(r.write.p90),
            crate::stats::ms(r.write.p99),
            f50,
            f99,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fwbench-disk-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn aligned_buffer_is_sector_aligned_and_sized() {
        let a = aligned(3 * SECTOR);
        assert_eq!(a.aligned().as_ptr() as usize % SECTOR, 0);
        assert_eq!(a.aligned().len(), 3 * SECTOR);
    }

    #[test]
    fn filler_is_not_zeros() {
        let f = filler(SECTOR);
        assert_eq!(f.len(), SECTOR);
        assert!(f.iter().any(|&b| b != 0));
    }

    #[test]
    fn write_through_rejects_unaligned_io_size() {
        let dir = temp_dir("unaligned");
        let config = Config {
            dir: dir.clone(),
            bytes: SECTOR as u64 * 4,
            patterns: vec![Pattern::WriteThrough { io: 1000, qd: 1 }],
        };
        assert!(run(&config).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_pattern_writes_the_requested_bytes_and_cleans_up() {
        let dir = temp_dir("patterns");
        let bytes = 2 * 1024 * 1024;
        let config = Config {
            dir: dir.clone(),
            bytes,
            patterns: vec![
                Pattern::BufferedFsync {
                    io: 64 * 1024,
                    flush_every: 512 * 1024,
                },
                Pattern::WriteThrough {
                    io: 64 * 1024,
                    qd: 1,
                },
                Pattern::WriteThrough {
                    io: 64 * 1024,
                    qd: 4,
                },
            ],
        };
        let results = run(&config).unwrap();
        assert_eq!(results.len(), 3);
        for r in &results {
            assert_eq!(r.bytes, bytes, "{}", r.pattern.name());
            assert_eq!(r.write.count as u64, bytes / (64 * 1024));
            assert!(r.bytes_per_sec() > 0.0);
        }
        assert!(results[0].fsync.as_ref().unwrap().count >= 4);
        assert!(results[1].fsync.is_none());
        let leftovers = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 0, "probe files were not removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_matrix_is_control_then_qd1_then_qdn() {
        let m = Pattern::default_matrix(1024 * 1024, 8);
        assert_eq!(m.len(), 3);
        assert!(matches!(m[0], Pattern::BufferedFsync { .. }));
        assert_eq!(
            m[1],
            Pattern::WriteThrough {
                io: 1024 * 1024,
                qd: 1
            }
        );
        assert_eq!(
            m[2],
            Pattern::WriteThrough {
                io: 1024 * 1024,
                qd: 8
            }
        );
    }

    #[test]
    fn render_has_one_row_per_result() {
        let r = DiskResult {
            pattern: Pattern::WriteThrough { io: SECTOR, qd: 1 },
            bytes: SECTOR as u64 * 10,
            elapsed: Duration::from_millis(10),
            write: Summary::of(&[Duration::from_micros(500)]),
            fsync: None,
        };
        let text = render(Path::new("X:\\"), SECTOR as u64 * 10, &[r.clone(), r]);
        assert_eq!(
            text.lines()
                .filter(|l| l.starts_with("| writethrough"))
                .count(),
            2
        );
    }
}
