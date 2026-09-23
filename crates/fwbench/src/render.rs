//! M3: frame time and output-to-screen latency under a flood.
//!
//! An emulator thread feeds agent-like output into a `vterm` pane, paced at a
//! rate (or flat out). Every `marker_every` it stamps the arrival time of the
//! chunk it is about to feed, under the pane lock, so the stamp and the bytes
//! land in the screen together. The render thread (the caller's, since a
//! window belongs to the thread that made it) turns the screen into a
//! [`Frame`] — noting under the same lock how many stamps that screen includes
//! — draws it through a [`Target`], and for every stamp newly included records
//! the latency from arrival to present. Nothing is written to the screen to
//! measure it, so the workload is exactly what an agent would send. A phase
//! with no output then checks that nothing is drawn and prices idle CPU.
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
    /// Set up anything that needs the atlas (a GPU device, a swapchain)
    /// before the clock starts.
    fn prepare(&mut self, _atlas: &mut Atlas) -> io::Result<()> {
        Ok(())
    }
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
    /// Frame-rate cap; 0 draws whenever something changed.
    pub max_fps: u32,
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
    /// Stamp arrival to present returned, one sample per stamp.
    pub latency: Vec<Duration>,
    /// Stamps taken.
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
    /// Arrival time (ns since `epoch`) of stamp `n`, indexed by `n`. Pushed
    /// under the `term` lock together with the bytes it stamps.
    arrivals: Mutex<Vec<u64>>,
    /// The renderer is waiting for `term`; the emulator steps aside, since
    /// the lock is not fair and a flat-out feeder would starve the frame.
    want: AtomicBool,
    /// Bytes fed.
    fed: AtomicU64,
    /// Set by the emulator after a feed, cleared by the renderer: one wake per
    /// batch instead of one per chunk.
    pending: AtomicBool,
    stop: AtomicBool,
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
    target.prepare(&mut atlas)?;

    let pane = Arc::new(Pane {
        term: Mutex::new(Term::new(rows, cols)),
        arrivals: Mutex::new(Vec::new()),
        want: AtomicBool::new(false),
        fed: AtomicU64::new(0),
        pending: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });
    let epoch = Instant::now();
    let ns = move |t: Instant| t.duration_since(epoch).as_nanos() as u64;

    // Draw the blank screen once, uncounted, so the first measured frame
    // does not pay for pipeline and swapchain warm-up.
    {
        let term = pane.term.lock().unwrap();
        frame.update(term.screen(), true, &palette, &mut atlas, raster);
    }
    target.draw(&frame, &mut atlas)?;
    frame.clear_damage();

    let data = Arc::new(workload(32 * 1024 * 1024));
    let wake = target.waker();
    let emulator = {
        let (pane, data, wake) = (pane.clone(), data.clone(), wake.clone());
        let (rate, every, flood) = (config.rate_mib, config.marker_every, config.flood);
        std::thread::spawn(move || {
            const CHUNK: usize = 16 * 1024;
            let start = Instant::now();
            let (mut at, mut sent) = (0usize, 0u64);
            let mut next_marker = start + every;
            while !pane.stop.load(Ordering::Relaxed) && start.elapsed() < flood {
                if rate > 0.0 {
                    let due = (start.elapsed().as_secs_f64() * rate * 1048576.0) as u64;
                    if sent >= due {
                        std::thread::sleep(Duration::from_micros(200));
                        continue;
                    }
                }
                while pane.want.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                let take = CHUNK.min(data.len() - at);
                let mut t = pane.term.lock().unwrap();
                let now = Instant::now();
                if now >= next_marker {
                    pane.arrivals.lock().unwrap().push(ns(now));
                    while next_marker <= now {
                        next_marker += every;
                    }
                }
                t.feed(&data[at..at + take]);
                drop(t);
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
    let mut shown = 0usize;
    let started = Instant::now();
    let mut closed = false;
    let mut step = |target: &mut dyn Target,
                    frame: &mut Frame,
                    atlas: &mut Atlas,
                    out: &mut Rendered,
                    idle: bool| {
        pane.pending.store(false, Ordering::Release);
        let t = Instant::now();
        pane.want.store(true, Ordering::Release);
        let mut term = pane.term.lock().unwrap();
        pane.want.store(false, Ordering::Release);
        if !term.take_dirty() {
            return Ok(false);
        }
        frame.update(term.screen(), true, &palette, atlas, raster);
        let included = pane.arrivals.lock().unwrap().len();
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
        if included > shown {
            let arrivals = pane.arrivals.lock().unwrap();
            for &arrived in &arrivals[shown..included] {
                out.latency
                    .push(Duration::from_nanos(ns(done).saturating_sub(arrived)));
            }
            shown = included;
        }
        Ok::<bool, io::Error>(true)
    };

    let min_interval = if config.max_fps > 0 {
        Duration::from_secs_f64(1.0 / config.max_fps as f64)
    } else {
        Duration::ZERO
    };
    let mut last_draw: Option<Instant> = None;
    while started.elapsed() < config.flood + Duration::from_millis(100) {
        let since = last_draw.map_or(min_interval, |t| t.elapsed());
        let timeout = if since < min_interval {
            min_interval - since
        } else {
            if step(&mut *target, &mut frame, &mut atlas, &mut out, false)? {
                last_draw = Some(Instant::now());
            }
            Duration::from_millis(50)
        };
        if !target.wait(timeout) {
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

/// The CPU renderer in a window, blitted with GDI.
#[cfg(windows)]
pub struct WindowCpu {
    window: fwrender::win::Window,
    cpu: fwrender::CpuRenderer,
    presenter: fwrender::win::CpuPresenter,
    events: Vec<fwrender::win::Event>,
}

#[cfg(windows)]
impl WindowCpu {
    /// A window with a client area of `size` pixels.
    pub fn new(size: (u32, u32)) -> io::Result<WindowCpu> {
        let window = fwrender::win::Window::new("fwbench render - cpu", size.0, size.1)?;
        let (w, h) = window.client_size();
        Ok(WindowCpu {
            window,
            cpu: fwrender::CpuRenderer::new(w, h),
            presenter: fwrender::win::CpuPresenter::new(),
            events: Vec::new(),
        })
    }
}

#[cfg(windows)]
impl Target for WindowCpu {
    fn name(&self) -> String {
        "cpu (GDI blit)".into()
    }

    fn size(&self) -> (u32, u32) {
        (self.cpu.width(), self.cpu.height())
    }

    fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> io::Result<Draw> {
        let t = Instant::now();
        atlas.take_dirty();
        let (mut lo, mut hi) = (u32::MAX, 0u32);
        let ch = atlas.metrics().cell_h;
        for (r, _) in frame.damage().iter().enumerate().filter(|(_, &d)| d) {
            lo = lo.min(r as u32 * ch);
            hi = hi.max((r as u32 + 1) * ch);
        }
        self.cpu.draw(frame, atlas);
        let render = t.elapsed();
        let t = Instant::now();
        if lo < hi {
            self.presenter.present(&self.window, &self.cpu, lo..hi)?;
        }
        Ok(Draw {
            render,
            present: t.elapsed(),
        })
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        self.events.clear();
        self.window.wait(Some(timeout), &mut self.events);
        !self
            .events
            .iter()
            .any(|e| matches!(e, fwrender::win::Event::CloseRequested))
    }

    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let w = self.window.waker();
        Arc::new(move || w.wake())
    }
}

/// The Vulkan backend in a window. The renderer is made on the first draw,
/// because it needs the atlas, which [`run`] owns.
#[cfg(windows)]
pub struct WindowVulkan {
    state: VkState,
    mode: fwrender::vk::PresentMode,
    events: Vec<fwrender::win::Event>,
}

#[cfg(windows)]
enum VkState {
    Window(fwrender::win::Window),
    Ready(fwrender::vk::VkWindow),
    Failed,
}

#[cfg(windows)]
impl WindowVulkan {
    fn window(&mut self) -> Option<&mut fwrender::win::Window> {
        match &mut self.state {
            VkState::Window(w) => Some(w),
            VkState::Ready(v) => Some(v.window_mut()),
            VkState::Failed => None,
        }
    }
}

#[cfg(windows)]
impl Target for WindowVulkan {
    fn name(&self) -> String {
        match &self.state {
            VkState::Ready(_) => format!("vulkan ({:?})", self.mode),
            _ => format!("vulkan ({:?}, not prepared)", self.mode),
        }
    }

    fn size(&self) -> (u32, u32) {
        match &self.state {
            VkState::Window(w) => w.client_size(),
            VkState::Ready(v) => v.window().client_size(),
            VkState::Failed => (0, 0),
        }
    }

    fn prepare(&mut self, atlas: &mut Atlas) -> io::Result<()> {
        if let VkState::Window(_) = self.state {
            let VkState::Window(w) = std::mem::replace(&mut self.state, VkState::Failed) else {
                unreachable!()
            };
            let mut v = fwrender::vk::VkWindow::new(w, atlas, self.mode).map_err(vk_error)?;
            eprintln!("render: vulkan device {}", v.renderer().device_name());
            self.state = VkState::Ready(v);
        }
        Ok(())
    }

    fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> io::Result<Draw> {
        let t = Instant::now();
        let VkState::Ready(v) = &mut self.state else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "vulkan is not prepared",
            ));
        };
        let stats = v.renderer().draw(frame, atlas).map_err(vk_error)?;
        let total = t.elapsed();
        Ok(Draw {
            render: total.saturating_sub(stats.present),
            present: stats.present,
        })
    }

    fn wait(&mut self, timeout: Duration) -> bool {
        self.events.clear();
        let mut events = std::mem::take(&mut self.events);
        if let Some(w) = self.window() {
            w.wait(Some(timeout), &mut events);
        }
        let closed = events
            .iter()
            .any(|e| matches!(e, fwrender::win::Event::CloseRequested));
        self.events = events;
        !closed
    }

    fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let w = match &self.state {
            VkState::Window(w) => w.waker(),
            VkState::Ready(v) => v.window().waker(),
            VkState::Failed => return Arc::new(|| {}),
        };
        Arc::new(move || w.wake())
    }
}

#[cfg(windows)]
fn vk_error(e: fwrender::vk::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

/// Open the Vulkan backend in a window of `size` pixels, presenting with
/// `present` (`fifo`, `mailbox` or `immediate`).
#[cfg(windows)]
pub fn open_vulkan(size: (u32, u32), present: &str) -> io::Result<Box<dyn Target>> {
    use fwrender::vk::PresentMode;
    let mode = match present {
        "mailbox" => PresentMode::Mailbox,
        "immediate" => PresentMode::Immediate,
        _ => PresentMode::Fifo,
    };
    let window = fwrender::win::Window::new("fwbench render - vulkan", size.0, size.1)?;
    Ok(Box::new(WindowVulkan {
        state: VkState::Window(window),
        mode,
        events: Vec::new(),
    }))
}

/// The report.
pub fn render(config: &Config, r: &Rendered) -> String {
    let secs = r.elapsed.as_secs_f64().max(1e-9);
    let cap = if config.max_fps > 0 {
        format!(", capped at {} fps", config.max_fps)
    } else {
        String::new()
    };
    let rate = if config.rate_mib > 0.0 {
        format!("{:.1} MiB/s", config.rate_mib)
    } else {
        "flat out".into()
    };
    let mut o = format!(
        "render: {} - {}x{} cells in {}x{} px, output {}{}, {:.1} s\n\n\
         | measure | result |\n| --- | --- |\n",
        r.backend, r.cols, r.rows, r.size.0, r.size.1, rate, cap, secs
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
        "| output arrival to present, p50 / p90 / p99 | {} |\n",
        line(&Summary::of(&r.latency))
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
    fn a_short_headless_run_draws_measures_and_goes_quiet() {
        let config = Config {
            flood: Duration::from_millis(600),
            idle: Duration::from_millis(300),
            rate_mib: 8.0,
            marker_every: Duration::from_millis(20),
            max_fps: 0,
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
        assert!(r.markers >= 10, "{} stamps", r.markers);
        assert_eq!(
            r.latency.len() as u64,
            r.markers,
            "every stamp reaches a frame"
        );
        assert_eq!(r.idle_frames, 0, "nothing changes, nothing is drawn");
        assert!(render(&config, &r).contains("output arrival to present"));
    }
}

/// Where one pane's emulation time goes, stage by stage, on 64 MiB of agent
/// output (177x47): `cargo test --release -p nativelite-fwbench
/// profile_vterm_stages -- --ignored --nocapture`.
#[cfg(test)]
mod stage_profile {
    use ansi::{Cell, Event, Parser, Screen, Style, Utf8Decoder};
    use std::time::Instant;

    fn rate(label: &str, bytes: usize, t: Instant) {
        let s = t.elapsed().as_secs_f64();
        println!(
            "{label:<44} {:>8.1} MiB/s  ({:.1} ms)",
            bytes as f64 / 1048576.0 / s,
            s * 1e3
        );
    }

    #[test]
    #[ignore]
    fn profile_vterm_stages() {
        let data = super::workload(64 << 20);
        let n = data.len();
        let lines = data.iter().filter(|&&b| b == b'\n').count();
        let (mut text, mut ctl, mut csi, mut other) = (0usize, 0usize, 0usize, 0usize);
        let mut p = Parser::new();
        p.feed_with(&data, |e| match e {
            Event::Text(b) => text += b.len(),
            Event::Control(_) => ctl += 1,
            Event::Csi { .. } => csi += 1,
            _ => other += 1,
        });
        println!("{n} bytes, {lines} lines ({:.1} B/line), text {text} B, controls {ctl}, CSI {csi}, other {other}", n as f64 / lines as f64);
        for _ in 0..2 {
            let t = Instant::now();
            let mut p = Parser::new();
            let mut k = 0usize;
            for c in data.chunks(64 * 1024) {
                p.feed_with(c, |e| {
                    if let Event::Text(b) = e {
                        k += b.len()
                    }
                });
            }
            std::hint::black_box(k);
            rate("A parser only (feed_with)", n, t);

            let t = Instant::now();
            let mut p = Parser::new();
            let mut d = Utf8Decoder::new();
            let mut k = 0usize;
            for c in data.chunks(64 * 1024) {
                p.feed_with(c, |e| {
                    if let Event::Text(b) = e {
                        d.decode(b, |s| k += s.len())
                    }
                });
            }
            std::hint::black_box(k);
            rate("B parser + UTF-8 decode", n, t);

            let t = Instant::now();
            let mut term = vterm::Term::new(47, 177);
            for c in data.chunks(64 * 1024) {
                term.feed(c);
            }
            rate("C vterm Term::feed, 177x47", n, t);

            let t = Instant::now();
            let mut term = vterm::Term::new(47, 80);
            for c in data.chunks(64 * 1024) {
                term.feed(c);
            }
            rate("C' vterm Term::feed, 80x47", n, t);

            // Screen work alone: one scroll + one row blit per line.
            let mut s = Screen::new(47, 177);
            let row: Vec<Cell> = (0..(n / lines))
                .map(|_| Cell::new('x', Style::default()))
                .collect();
            let t = Instant::now();
            for _ in 0..lines {
                s.scroll_rows_up(0, 46, 1, Cell::default());
                s.copy_cells(46, 0, &row[..row.len().min(176)]);
            }
            std::hint::black_box(&s);
            rate("D screen only: scroll + blit per line", n, t);

            let t = Instant::now();
            for _ in 0..lines {
                s.scroll_rows_up(0, 46, 1, Cell::default());
            }
            std::hint::black_box(&s);
            rate("E scroll only (fill 177 cells)", n, t);
        }
        println!("size_of Cell = {} bytes", std::mem::size_of::<Cell>());
    }
}
