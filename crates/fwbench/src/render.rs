//! M3: frame time and output-to-screen latency under a flood.
//!
//! An emulator thread feeds agent-like output into a `vterm` pane, paced at a
//! rate (or flat out), and keeps the bottom row out of the scroll region as a
//! status line. Every `marker_every` it stamps a numbered marker on that line
//! and records when those bytes reached the emulator. The render thread (the
//! caller's, since a window belongs to the thread that made it) turns the
//! screen into a [`Frame`], draws it through a [`Target`] and, when the frame
//! it has just presented shows a new marker, records the latency from arrival
//! to present. A phase with no output then checks that nothing is drawn and
//! prices idle CPU.
//!
//! The latency is to *present returned*, not to photons: the display adds its
//! own scan-out on top, the same for every terminal.

use crate::emit;
use crate::stats::{ms, Summary};
use fwrender::{Atlas, Frame, Palette, Rasterizer};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vterm::Term;

/// What a target reports for one drawn frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct Draw {
    /// Rendering and submitting (CPU time on the render thread).
    pub render: Duration,
    /// Presenting: blit, or acquire + present (includes any vsync wait).
    pub present: Duration,
}

/// Where frames go: a window through some backend, or nowhere (headless).
pub trait Target {
    /// Backend name for the report.
    fn name(&self) -> String;
    /// Drawable size in pixels.
    fn size(&self) -> (u32, u32);
    /// Draw and present the frame. Called only when something changed.
    fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> io::Result<Draw>;
    /// Block until [`Target::waker`] fires or `timeout` passes, handling any
    /// window events. Returns false when the user closed the window.
    fn wait(&mut self, timeout: Duration) -> bool;
    /// A function any thread may call to end a [`Target::wait`] early.
    fn waker(&self) -> Arc<dyn Fn() + Send + Sync>;
}

/// What to run.
#[derive(Debug, Clone)]
pub struct Config {
    /// Seconds of flood.
    pub flood: Duration,
    /// Seconds of idle afterwards.
    pub idle: Duration,
    /// Output rate in MiB/s; 0 is flat out.
    pub rate_mib: f64,
    /// Time between markers.
    pub marker_every: Duration,
}

/// What a run measured.
#[derive(Debug, Clone)]
pub struct Rendered {
    /// Backend name.
    pub backend: String,
    /// Grid size and cell size.
    pub rows: usize,
    /// Columns.
    pub cols: usize,
    /// Pixel size of the drawable.
    pub size: (u32, u32),
    /// Bytes the emulator consumed during the flood, and over how long.
    pub bytes: u64,
    /// Flood duration actually run.
    pub elapsed: Duration,
    /// Frames drawn during the flood.
    pub frames: usize,
    /// `Frame::update` (screen to instances, under the pane lock).
    pub update: Vec<Duration>,
    /// Target render + submit.
    pub render: Vec<Duration>,
    /// Target present.
    pub present: Vec<Duration>,
    /// Marker arrival to present returned.
    pub latency: Vec<Duration>,
    /// Markers stamped, and how many were never shown on their own (a later
    /// one reached the screen first).
    pub markers: u64,
    /// Frames drawn during the idle phase (should be 0).
    pub idle_frames: usize,
    /// Idle phase length.
    pub idle: Duration,
    /// This process's CPU seconds during the idle phase, when measurable.
    pub idle_cpu: Option<f64>,
    /// Glyphs rasterized, and atlas misses.
    pub glyphs: u32,
    /// Atlas misses.
    pub atlas_misses: u64,
}

/// A pane shared between the emulator thread and the renderer.
struct Pane {
    term: Mutex<Term>,
    /// Arrival time (ns since `epoch`) of marker `n`, indexed by `n`.
    arrivals: Mutex<Vec<u64>>,
    /// Bytes fed.
    fed: AtomicU64,
    /// Set by the emulator after a feed, cleared by the renderer: one wake per
    /// batch instead of one per chunk.
    pending: AtomicBool,
    stop: AtomicBool,
}

/// The marker line: `MARK nnnnnnnn` at the start of the bottom row.
fn marker_bytes(rows: usize, n: u64) -> Vec<u8> {
    format!("\x1b7\x1b[{rows};1H\x1b[0mMARK {n:08}\x1b8").into_bytes()
}

/// The marker number the bottom row shows, if any.
fn read_marker(term: &Term) -> Option<u64> {
    let s = term.screen();
    let row = s.rows() - 1;
    let text: String = (0..13).map(|c| s.cell(row, c).ch).collect();
    text.strip_prefix("MARK ")?.parse().ok()
}

/// Agent-like output with `\r\n` line ends, as a pty's output processing
/// delivers it.
fn workload(bytes: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity(bytes + 512);
    emit::Generator::new(0x3E4D_E12A).fill(&mut raw, bytes);
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 32);
    for &b in &raw {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

/// Run the flood then the idle phase against `target`.
pub fn run(
    config: &Config,
    target: &mut dyn Target,
    raster: &mut dyn Rasterizer,
) -> io::Result<Rendered> {
    let m = raster.metrics();
    let size = target.size();
    let (cols, rows) = (
        (size.0 / m.cell_w).max(2) as usize,
        (size.1 / m.cell_h).max(3) as usize,
    );
    let mut atlas = Atlas::new(m, 2048, 2048);
    let palette = Palette::default();
    let mut frame = Frame::new();

    let mut term = Term::new(rows, cols);
    // Reserve the bottom row for the marker line.
    term.feed(format!("\x1b[1;{}r\x1b[H", rows - 1).as_bytes());
    let pane = Arc::new(Pane {
        term: Mutex::new(term),
        arrivals: Mutex::new(Vec::new()),
        fed: AtomicU64::new(0),
        pending: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });
    let epoch = Instant::now();
    let ns = move |t: Instant| t.duration_since(epoch).as_nanos() as u64;

    let data = Arc::new(workload(32 * 1024 * 1024));
    let wake = target.waker();
    let emulator = {
        let (pane, data, wake) = (pane.clone(), data.clone(), wake.clone());
        let (rate, every, flood) = (config.rate_mib, config.marker_every, config.flood);
        std::thread::spawn(move || {
            const CHUNK: usize = 16 * 1024;
            let start = Instant::now();
            let (mut at, mut sent, mut n) = (0usize, 0u64, 0u64);
            let mut next_marker = start + every;
            while !pane.stop.load(Ordering::Relaxed) && start.elapsed() < flood {
                let now = Instant::now();
                if now >= next_marker {
                    let bytes = marker_bytes(rows, n);
                    let mut t = pane.term.lock().unwrap();
                    pane.arrivals.lock().unwrap().push(ns(Instant::now()));
                    t.feed(&bytes);
                    drop(t);
                    n += 1;
                    next_marker += every;
                } else if rate > 0.0 {
                    let due = (start.elapsed().as_secs_f64() * rate * 1048576.0) as u64;
                    if sent >= due {
                        std::thread::sleep(Duration::from_micros(200));
                        continue;
                    }
                }
                let take = CHUNK.min(data.len() - at);
                pane.term.lock().unwrap().feed(&data[at..at + take]);
                sent += take as u64;
                pane.fed.store(sent, Ordering::Relaxed);
                at = (at + take) % data.len();
                if !pane.pending.swap(true, Ordering::AcqRel) {
                    wake();
                }
            }
            wake();
        })
    };

    let mut out = Rendered {
        backend: target.name(),
        rows,
        cols,
        size,
        bytes: 0,
        elapsed: Duration::ZERO,
        frames: 0,
        update: Vec::new(),
        render: Vec::new(),
        present: Vec::new(),
        latency: Vec::new(),
        markers: 0,
        idle_frames: 0,
        idle: config.idle,
        idle_cpu: None,
        glyphs: 0,
        atlas_misses: 0,
    };
    let mut shown: Option<u64> = None;
    let started = Instant::now();
    let mut closed = false;
    let mut step = |target: &mut dyn Target,
                    frame: &mut Frame,
                    atlas: &mut Atlas,
                    out: &mut Rendered,
                    idle: bool| {
        pane.pending.store(false, Ordering::Release);
        let t = Instant::now();
        let mut term = pane.term.lock().unwrap();
        if !term.take_dirty() {
            return Ok(false);
        }
        frame.update(term.screen(), true, &palette, atlas, raster);
        let marker = read_marker(&term);
        drop(term);
        let update = t.elapsed();
        if frame.damaged_rows() == 0 {
            return Ok(false);
        }
        let d = target.draw(frame, atlas)?;
        let done = Instant::now();
        frame.clear_damage();
        if idle {
            out.idle_frames += 1;
            return Ok(true);
        }
        out.frames += 1;
        out.update.push(update);
        out.render.push(d.render);
        out.present.push(d.present);
        if let Some(k) = marker.filter(|&k| Some(k) != shown) {
            shown = Some(k);
            if let Some(&arrived) = pane.arrivals.lock().unwrap().get(k as usize) {
                out.latency
                    .push(Duration::from_nanos(ns(done).saturating_sub(arrived)));
            }
        }
        Ok::<bool, io::Error>(true)
    };

    while started.elapsed() < config.flood + Duration::from_millis(100) {
        step(&mut *target, &mut frame, &mut atlas, &mut out, false)?;
        if !target.wait(Duration::from_millis(50)) {
            closed = true;
            break;
        }
    }
    pane.stop.store(true, Ordering::Relaxed);
    emulator
        .join()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "emulator thread panicked"))?;
    out.elapsed = started.elapsed().min(config.flood);
    out.bytes = pane.fed.load(Ordering::Relaxed);
    out.markers = pane.arrivals.lock().unwrap().len() as u64;
    // Draw whatever the flood left, so the idle phase starts clean.
    step(&mut *target, &mut frame, &mut atlas, &mut out, true)?;
    out.idle_frames = 0;

    if !closed && !config.idle.is_zero() {
        let pid = std::process::id();
        let cpu = || {
            resmon::sys::snapshot()
                .ok()
                .and_then(|s| crate::noise::own_cpu_secs(&s, pid))
        };
        let before = cpu();
        let idle_start = Instant::now();
        while idle_start.elapsed() < config.idle {
            step(&mut *target, &mut frame, &mut atlas, &mut out, true)?;
            let left = config.idle.saturating_sub(idle_start.elapsed());
            if left.is_zero() || !target.wait(left) {
                break;
            }
        }
        out.idle_cpu = match (before, cpu()) {
            (Some(a), Some(b)) => Some(b - a),
            _ => None,
        };
    }
    out.glyphs = atlas.used() - 1;
    out.atlas_misses = atlas.misses();
    Ok(out)
}

/// A headless CPU target: renders into memory, presents nowhere.
pub struct Headless {
    cpu: fwrender::CpuRenderer,
    signal: Arc<(Mutex<bool>, Condvar)>,
}

impl Headless {
    /// A `width x height` headless target.
    pub fn new(width: u32, height: u32) -> Headless {
        Headless {
            cpu: fwrender::CpuRenderer::new(width, height),
            signal: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }
}

impl Target for Headless {
    fn name(&self) -> String {
        "cpu (headless)".into()
    }

    fn size(&self) -> (u32, u32) {
        (self.cpu.width(), self.cpu.height())
    }

    fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> io::Result<Draw> {
        let t = Instant::now();
        atlas.take_dirty();
        self.cpu.draw(frame, atlas);
        Ok(Draw {
            render: t.elapsed(),
            present: Duration::ZERO,
        })
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.signal;
        let mut woken = lock.lock().unwrap();
        if !*woken {
            woken = cv.wait_timeout(woken, timeout).unwrap().0;
        }
        *woken = false;
        true
    }

    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let signal = self.signal.clone();
        Arc::new(move || {
            let (lock, cv) = &*signal;
            *lock.lock().unwrap() = true;
            cv.notify_one();
        })
    }
}

/// The report.
pub fn render(config: &Config, r: &Rendered) -> String {
    let secs = r.elapsed.as_secs_f64().max(1e-9);
    let rate = if config.rate_mib > 0.0 {
        format!("{:.1} MiB/s", config.rate_mib)
    } else {
        "flat out".into()
    };
    let mut o = format!(
        "render: {} - {}x{} cells in {}x{} px, output {}, {:.1} s\n\n\
         | measure | result |\n| --- | --- |\n",
        r.backend, r.cols, r.rows, r.size.0, r.size.1, rate, secs
    );
    let line = |s: &Summary| {
        format!(
            "{} / {} / {} ms (max {}, n = {})",
            ms(s.p50).trim_end_matches("ms"),
            ms(s.p90).trim_end_matches("ms"),
            ms(s.p99).trim_end_matches("ms"),
            ms(s.max),
            s.count
        )
    };
    o += &format!(
        "| output consumed | {:.1} MiB/s ({:.0} MiB) |\n",
        r.bytes as f64 / 1048576.0 / secs,
        r.bytes as f64 / 1048576.0
    );
    o += &format!(
        "| frames drawn | {} ({:.1} fps) |\n",
        r.frames,
        r.frames as f64 / secs
    );
    o += &format!(
        "| frame update (screen to instances), p50 / p90 / p99 | {} |\n",
        line(&Summary::of(&r.update))
    );
    o += &format!(
        "| render + submit, p50 / p90 / p99 | {} |\n",
        line(&Summary::of(&r.render))
    );
    o += &format!(
        "| present, p50 / p90 / p99 | {} |\n",
        line(&Summary::of(&r.present))
    );
    o += &format!(
        "| output to present, p50 / p90 / p99 | {} |\n",
        line(&Summary::of(&r.latency))
    );
    o += &format!(
        "| markers shown / stamped | {} / {} |\n",
        r.latency.len(),
        r.markers
    );
    o += &format!(
        "| glyphs rasterized (atlas misses) | {} ({}) |\n",
        r.glyphs, r.atlas_misses
    );
    o += &format!(
        "| idle: frames drawn in {:.1} s | {} |\n",
        r.idle.as_secs_f64(),
        r.idle_frames
    );
    if let Some(c) = r.idle_cpu {
        o += &format!(
            "| idle: CPU | {:.2} % of one core |\n",
            100.0 * c / r.idle.as_secs_f64().max(1e-9)
        );
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use fwrender::{Metrics, TestRasterizer};

    #[test]
    fn markers_round_trip_through_the_emulator() {
        let mut t = Term::new(5, 20);
        t.feed(b"\x1b[1;4r\x1b[H");
        for i in 0..10 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.feed(&marker_bytes(5, 42));
        t.feed(b"more\r\n");
        assert_eq!(read_marker(&t), Some(42));
    }

    #[test]
    fn a_short_headless_run_draws_measures_and_goes_quiet() {
        let config = Config {
            flood: Duration::from_millis(600),
            idle: Duration::from_millis(300),
            rate_mib: 8.0,
            marker_every: Duration::from_millis(20),
        };
        let mut target = Headless::new(80 * 8, 25 * 16);
        let mut raster = TestRasterizer {
            metrics: Metrics {
                cell_w: 8,
                cell_h: 16,
                baseline: 12,
            },
        };
        let r = run(&config, &mut target, &mut raster).expect("run");
        assert_eq!((r.cols, r.rows), (80, 25));
        assert!(r.frames > 5, "{} frames", r.frames);
        assert!(r.bytes > 1024 * 1024, "{} bytes", r.bytes);
        assert!(!r.latency.is_empty() && r.markers >= 10);
        assert_eq!(r.idle_frames, 0, "nothing changes, nothing is drawn");
        assert!(render(&config, &r).contains("output to present"));
    }
}
