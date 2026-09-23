//! `fwterm` — foldwave's minimal terminal prototype.
//!
//! One pane, in one of two lanes, `vterm` for the screen, and the
//! `fwrender` window and backends for drawing. It exists to be measured
//! against other terminals with the same outside instruments, so it does the
//! least a terminal must: keys to the shell, output to the screen, resize.
//! No scrollback, selection, tabs or configuration yet.
//!
//! The lanes (design §8.1):
//! * **console** (default): the program runs under ConPTY — the built-in
//!   console host, or any `conpty.dll` given with `--conpty` — for shells and
//!   full-screen programs that need a console.
//! * **pipe** (`--pipe`): the program gets no console at all; its stdout and
//!   stderr come through plain pipes at native speed and its stdin takes the
//!   keys. For agent CLIs in their documented non-interactive modes. As a
//!   tty's line discipline would, a line feed also returns to column 0 (the
//!   emulator's new line mode, so no byte is rewritten).
//!
//! Threads: a reader moves shell output into the emulator (answering the
//! console host's startup queries on the way) and wakes the window; the UI
//! thread turns keys into bytes for the shell and draws when something
//! changed, at most once per display refresh.

#![forbid(unsafe_code)]

#[cfg(windows)]
fn main() {
    std::process::exit(app::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fwterm: the prototype is Windows-only for now");
    std::process::exit(1);
}

#[cfg(windows)]
mod app {
    use fwrender::win::{display_refresh_hz, CpuPresenter, Event, Window};
    use fwrender::{Atlas, CpuRenderer, FontRasterizer, Frame, Palette, Rasterizer, Sharpening};
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use vterm::Term;

    const USAGE: &str = "\
fwterm - foldwave's minimal terminal prototype

usage: fwterm [options] [-- SHELL [ARGS...]]   (default shell: cmd.exe)

options:
  --backend B   vulkan (default; falls back to cpu if Vulkan cannot start) or cpu
  --conpty DLL  host the shell with this conpty.dll (e.g. one that starts an
                OpenConsole.exe beside it). Without it, a conpty.dll beside
                fwterm.exe is used if present, else the built-in console host
  --pipe        run the program with no console: output over plain pipes
                (native speed), keys to its stdin
  --px N        font size in pixels per em (default 16)
  --size WxH    client area in pixels (default 1200x720)
  --no-sharpen  draw the raw glyph outlines (no fitting or stem snapping)
  --max-fps N   frame-rate cap (default: the display's refresh rate)
  --title T     window title (default fwterm)
  --history N   lines of scrollback kept in memory (default 10000); scroll
                with the mouse wheel or Shift+PageUp/PageDown, and typing
                returns to the bottom
  --stats PATH  on exit, write frame statistics for the time output flowed:
                frames, frames per second, gaps between frames
";

    struct Args {
        backend: String,
        conpty: Option<String>,
        px: f32,
        size: (u32, u32),
        sharpen: bool,
        max_fps: Option<u32>,
        title: String,
        pipe: bool,
        stats: Option<String>,
        history: usize,
        shell: Vec<String>,
    }

    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut a = Args {
            backend: "vulkan".into(),
            conpty: None,
            px: 16.0,
            size: (1200, 720),
            sharpen: true,
            max_fps: None,
            title: "fwterm".into(),
            pipe: false,
            stats: None,
            history: 10_000,
            shell: vec!["cmd.exe".into()],
        };
        let mut i = 0;
        let value = |i: &mut usize, flag: &str| -> Result<String, String> {
            let v = argv.get(*i + 1).ok_or(format!("{flag} needs a value"))?;
            *i += 2;
            Ok(v.clone())
        };
        while i < argv.len() {
            match argv[i].as_str() {
                "-h" | "--help" => return Err(USAGE.into()),
                "--backend" => {
                    a.backend = value(&mut i, "--backend")?;
                    if a.backend != "vulkan" && a.backend != "cpu" {
                        return Err(format!("bad --backend {:?}", a.backend));
                    }
                }
                "--conpty" => a.conpty = Some(value(&mut i, "--conpty")?),
                "--px" => {
                    let v = value(&mut i, "--px")?;
                    a.px = v
                        .parse()
                        .ok()
                        .filter(|p: &f32| *p >= 4.0)
                        .ok_or(format!("bad --px {v:?}"))?;
                }
                "--size" => {
                    let v = value(&mut i, "--size")?;
                    a.size = v
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .filter(|&(w, h): &(u32, u32)| w >= 64 && h >= 64)
                        .ok_or(format!("bad --size {v:?}"))?;
                }
                "--pipe" => {
                    a.pipe = true;
                    i += 1;
                }
                "--no-sharpen" => {
                    a.sharpen = false;
                    i += 1;
                }
                "--max-fps" => {
                    let v = value(&mut i, "--max-fps")?;
                    a.max_fps = Some(
                        v.parse()
                            .ok()
                            .filter(|&n| n > 0)
                            .ok_or(format!("bad --max-fps {v:?}"))?,
                    );
                }
                "--title" => a.title = value(&mut i, "--title")?,
                "--stats" => a.stats = Some(value(&mut i, "--stats")?),
                "--history" => {
                    let v = value(&mut i, "--history")?;
                    a.history = v.parse().map_err(|_| format!("bad --history {v:?}"))?;
                }
                "--" => {
                    if i + 1 < argv.len() {
                        a.shell = argv[i + 1..].to_vec();
                    }
                    break;
                }
                other => return Err(format!("unknown argument {other:?}\n\n{USAGE}")),
            }
        }
        Ok(a)
    }

    /// Where frames go.
    enum Surface {
        Cpu {
            window: Window,
            cpu: CpuRenderer,
            presenter: CpuPresenter,
        },
        Vulkan(fwrender::vk::VkWindow),
    }

    impl Surface {
        fn window(&mut self) -> &mut Window {
            match self {
                Surface::Cpu { window, .. } => window,
                Surface::Vulkan(v) => v.window_mut(),
            }
        }

        fn resize(&mut self, w: u32, h: u32) -> io::Result<()> {
            match self {
                Surface::Cpu { cpu, .. } => {
                    cpu.resize(w, h);
                    Ok(())
                }
                Surface::Vulkan(v) => v
                    .renderer()
                    .resize(w, h)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string())),
            }
        }

        fn draw(&mut self, frame: &Frame, atlas: &mut Atlas) -> io::Result<()> {
            match self {
                Surface::Cpu {
                    window,
                    cpu,
                    presenter,
                } => {
                    atlas.take_dirty();
                    let ch = atlas.metrics().cell_h;
                    let (mut lo, mut hi) = (u32::MAX, 0);
                    for (r, _) in frame.damage().iter().enumerate().filter(|(_, &d)| d) {
                        lo = lo.min(r as u32 * ch);
                        hi = hi.max((r as u32 + 1) * ch);
                    }
                    let fresh = cpu.draw(frame, atlas) == frame.rows();
                    // After a resize the margins changed too: present it all.
                    let rows = if fresh { 0..cpu.height() } else { lo..hi };
                    if rows.start < rows.end {
                        presenter.present(window, cpu, rows)?;
                    }
                    Ok(())
                }
                Surface::Vulkan(v) => v
                    .renderer()
                    .draw(frame, atlas)
                    .map(|_| ())
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string())),
            }
        }
    }

    /// Bytes for the shell for a typed character.
    fn char_bytes(c: char, out: &mut Vec<u8>) {
        match c {
            // Backspace arrives as BS; terminals send DEL.
            '\u{8}' => out.push(0x7F),
            c => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            }
        }
    }

    /// Bytes for the shell for a key with no character (VT sequences).
    fn key_bytes(vk: u32) -> Option<&'static [u8]> {
        Some(match vk {
            0x26 => b"\x1b[A",  // up
            0x28 => b"\x1b[B",  // down
            0x27 => b"\x1b[C",  // right
            0x25 => b"\x1b[D",  // left
            0x24 => b"\x1b[H",  // home
            0x23 => b"\x1b[F",  // end
            0x2D => b"\x1b[2~", // insert
            0x2E => b"\x1b[3~", // delete
            0x21 => b"\x1b[5~", // page up
            0x22 => b"\x1b[6~", // page down
            _ => return None,
        })
    }

    /// The program on the other side, in either lane.
    enum Session {
        Console(pty::Pty),
        Pipe {
            child: std::process::Child,
            stdin: Option<std::process::ChildStdin>,
        },
    }

    impl Session {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            match self {
                Session::Console(p) => p.write(bytes).map(|_| ()),
                Session::Pipe { stdin, .. } => match stdin {
                    Some(s) => io::Write::write_all(s, bytes),
                    None => Ok(()),
                },
            }
        }

        fn resize(&mut self, rows: u16, cols: u16) {
            if let Session::Console(p) = self {
                let _ = p.resize(rows, cols);
            }
        }

        fn kill(&mut self) {
            match self {
                Session::Console(p) => {
                    let _ = p.kill();
                }
                Session::Pipe { child, .. } => {
                    let _ = child.kill();
                }
            }
        }

        fn wait(&mut self) -> i32 {
            match self {
                Session::Console(p) => p.wait().unwrap_or(0),
                Session::Pipe { child, .. } => {
                    child.wait().ok().and_then(|s| s.code()).unwrap_or(0)
                }
            }
        }
    }

    struct Shared {
        term: Mutex<Term>,
        pending: AtomicBool,
        exited: AtomicBool,
        /// The renderer is waiting for `term`; readers step aside. The lock is
        /// not fair, and a reader that releases and immediately re-takes it
        /// otherwise holds the renderer off for tens of milliseconds.
        want: AtomicBool,
        /// Nanoseconds since `epoch` of the first and latest output read.
        first_output: std::sync::atomic::AtomicU64,
        last_output: std::sync::atomic::AtomicU64,
        epoch: Instant,
    }

    /// Frame statistics over the time output was arriving, for `--stats`.
    fn write_stats(path: &str, shared: &Shared, frames: &[u64]) -> io::Result<()> {
        let (first, last) = (
            shared.first_output.load(Ordering::Acquire),
            shared.last_output.load(Ordering::Acquire),
        );
        let during: Vec<u64> = frames
            .iter()
            .copied()
            .filter(|&t| t >= first && t <= last + 50_000_000)
            .collect();
        let secs = (last.saturating_sub(first)) as f64 / 1e9;
        let mut gaps: Vec<u64> = during.windows(2).map(|w| w[1] - w[0]).collect();
        gaps.sort_unstable();
        let pct = |q: f64| -> f64 {
            if gaps.is_empty() {
                0.0
            } else {
                gaps[((gaps.len() - 1) as f64 * q).round() as usize] as f64 / 1e6
            }
        };
        let text = format!(
            "frames {}\nseconds {:.3}\nfps {:.1}\ngap_p50_ms {:.2}\ngap_p90_ms {:.2}\ngap_p99_ms {:.2}\ngap_max_ms {:.2}\n",
            during.len(),
            secs,
            if secs > 0.0 { during.len() as f64 / secs } else { 0.0 },
            pct(0.5),
            pct(0.9),
            pct(0.99),
            gaps.last().map_or(0.0, |&g| g as f64 / 1e6),
        );
        std::fs::write(path, text)
    }

    pub fn run() -> i32 {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let args = match parse(&argv) {
            Ok(a) => a,
            Err(msg) => {
                eprintln!("{msg}");
                return 2;
            }
        };
        match start(&args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("fwterm: {e}");
                1
            }
        }
    }

    fn start(args: &Args) -> io::Result<i32> {
        // Console lane host (design §11 row 6): an explicit --conpty, else a
        // conpty.dll shipped beside fwterm.exe (Microsoft's ConPTY package,
        // with OpenConsole.exe next to it), else the built-in console host.
        if let Some(dll) = &args.conpty {
            pty::use_conpty_library(std::path::Path::new(dll))?;
        } else if !args.pipe {
            let beside = std::env::current_exe()
                .ok()
                .and_then(|e| Some(e.parent()?.join("conpty.dll")))
                .filter(|p| p.exists());
            if let Some(dll) = beside {
                if let Err(e) = pty::use_conpty_library(&dll) {
                    eprintln!(
                        "fwterm: {} would not load ({e}); using the built-in console host",
                        dll.display()
                    );
                }
            }
        }
        let mut raster = FontRasterizer::consolas(args.px)?;
        if !args.sharpen {
            raster.set_sharpening(Sharpening::OFF);
        }
        let m = raster.metrics();
        let window = Window::new(&args.title, args.size.0, args.size.1)?;
        let (w, h) = window.client_size();
        let mut atlas = Atlas::new(m, 2048, 2048);
        let mut surface = if args.backend == "vulkan" {
            match fwrender::vk::VkWindow::new(window, &atlas, fwrender::vk::PresentMode::Mailbox) {
                Ok(v) => Surface::Vulkan(v),
                Err(e) => {
                    eprintln!("fwterm: Vulkan unavailable ({e}); drawing on the CPU");
                    Surface::Cpu {
                        window: Window::new(&args.title, args.size.0, args.size.1)?,
                        cpu: CpuRenderer::new(w, h),
                        presenter: CpuPresenter::new(),
                    }
                }
            }
        } else {
            Surface::Cpu {
                window,
                cpu: CpuRenderer::new(w, h),
                presenter: CpuPresenter::new(),
            }
        };

        let grid = |w: u32, h: u32| ((h / m.cell_h).max(1) as u16, (w / m.cell_w).max(1) as u16);
        let (rows, cols) = grid(w, h);
        let shell_args: Vec<&str> = args.shell[1..].iter().map(String::as_str).collect();
        let shared = Arc::new(Shared {
            term: Mutex::new({
                let mut t = Term::with_history(rows as usize, cols as usize, args.history);
                if args.pipe {
                    // No tty turns LF into CR LF on a pipe: new line mode
                    // (LNM) makes the emulator do it, with no copy.
                    t.feed(b"[20h");
                }
                t
            }),
            pending: AtomicBool::new(true),
            exited: AtomicBool::new(false),
            want: AtomicBool::new(false),
            first_output: std::sync::atomic::AtomicU64::new(u64::MAX),
            last_output: std::sync::atomic::AtomicU64::new(0),
            epoch: Instant::now(),
        });
        let waker = surface.window().waker();
        // Move one output stream into the emulator until it ends.
        let pump = |mut source: Box<dyn FnMut(&mut [u8]) -> io::Result<usize> + Send>,
                    session: Option<Arc<Mutex<Session>>>,
                    last: bool| {
            let (shared, waker) = (shared.clone(), waker.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                let mut watcher = fwbench::terminal::Watcher::new();
                loop {
                    let n = match source(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if let Some(session) = &session {
                        for reply in watcher.replies(&buf[..n]) {
                            let _ = session.lock().unwrap().write(reply);
                        }
                    }
                    while shared.want.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                    shared.term.lock().unwrap().feed(&buf[..n]);
                    let now = shared.epoch.elapsed().as_nanos() as u64;
                    shared.first_output.fetch_min(now, Ordering::AcqRel);
                    shared.last_output.store(now, Ordering::Release);
                    if !shared.pending.swap(true, Ordering::AcqRel) {
                        waker.wake();
                    }
                }
                if last {
                    shared.exited.store(true, Ordering::Release);
                    waker.wake();
                }
            });
        };
        let session = if args.pipe {
            use std::os::windows::process::CommandExt;
            use std::process::{Command, Stdio};
            // CREATE_NO_WINDOW: no console at all, not even a hidden one.
            let mut child = Command::new(&args.shell[0])
                .args(&shell_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x0800_0000)
                .spawn()?;
            let (mut out, mut err) = (
                child.stdout.take().expect("piped"),
                child.stderr.take().expect("piped"),
            );
            pump(Box::new(move |b| io::Read::read(&mut err, b)), None, false);
            pump(Box::new(move |b| io::Read::read(&mut out, b)), None, true);
            let stdin = child.stdin.take();
            Arc::new(Mutex::new(Session::Pipe { child, stdin }))
        } else {
            let pty = pty::Pty::spawn(&args.shell[0], &shell_args, rows, cols)?;
            let mut reader = pty.reader()?;
            let session = Arc::new(Mutex::new(Session::Console(pty)));
            pump(
                Box::new(move |b| reader.read(b)),
                Some(session.clone()),
                true,
            );
            session
        };

        // This thread draws: it must win a core when a frame is due, even with
        // every core busy moving output.
        fwrender::win::raise_thread_priority();
        let hz = args.max_fps.or_else(display_refresh_hz).unwrap_or(60);
        let min_interval = Duration::from_secs_f64(1.0 / hz as f64);
        let palette = Palette::default();
        let mut frame = Frame::new();
        let mut events = Vec::new();
        let mut input = Vec::new();
        let mut last_draw: Option<Instant> = None;
        // Scrollback view: how many lines back from live, and the history's
        // line count when last drawn, so the view stays on the same text
        // while output keeps scrolling in.
        let (mut back, mut seen_scrolled) = (0usize, 0u64);
        let page = |h: u32| (h / m.cell_h).max(2) as usize - 1;
        let mut frames: Vec<u64> = Vec::new();
        let finish = |frames: &[u64]| {
            if let Some(path) = &args.stats {
                if let Err(e) = write_stats(path, &shared, frames) {
                    eprintln!("fwterm: --stats {path}: {e}");
                }
            }
        };
        loop {
            // Sleep until an event, new output, or the next allowed frame.
            let timeout = match last_draw {
                Some(t) if shared.pending.load(Ordering::Acquire) => {
                    Some(min_interval.saturating_sub(t.elapsed()))
                }
                _ => None,
            };
            events.clear();
            surface.window().wait(timeout, &mut events);
            input.clear();
            for e in &events {
                match *e {
                    Event::Char(c) => {
                        // Typing returns the view to live output.
                        back = 0;
                        char_bytes(c, &mut input);
                    }
                    Event::Key { vk, down: true } => {
                        let (_, h) = surface.window().client_size();
                        match vk {
                            0x21 if fwrender::win::shift_down() => back += page(h),
                            0x22 if fwrender::win::shift_down() => {
                                back = back.saturating_sub(page(h))
                            }
                            _ => {
                                if let Some(b) = key_bytes(vk) {
                                    back = 0;
                                    input.extend_from_slice(b);
                                }
                            }
                        }
                        shared.pending.store(true, Ordering::Release);
                    }
                    Event::Wheel(delta) => {
                        // Three lines a notch, away from the user = back.
                        let lines = (delta.unsigned_abs() as usize * 3).div_euclid(120).max(1);
                        back = if delta > 0 {
                            back + lines
                        } else {
                            back.saturating_sub(lines)
                        };
                        shared.pending.store(true, Ordering::Release);
                    }
                    Event::Resized(w, h) if w > 0 && h > 0 => {
                        surface.resize(w, h)?;
                        let (rows, cols) = grid(w, h);
                        shared
                            .term
                            .lock()
                            .unwrap()
                            .resize(rows as usize, cols as usize);
                        session.lock().unwrap().resize(rows, cols);
                        frame.damage_all();
                        shared.pending.store(true, Ordering::Release);
                    }
                    Event::CloseRequested => {
                        finish(&frames);
                        // Take the shell down with the window.
                        session.lock().unwrap().kill();
                        return Ok(0);
                    }
                    _ => {}
                }
            }
            if !input.is_empty() {
                // A pipe program that has exited or closed stdin just
                // drops the keys.
                let _ = session.lock().unwrap().write(&input);
            }
            if shared.exited.load(Ordering::Acquire) {
                finish(&frames);
                return Ok(session.lock().unwrap().wait());
            }
            if last_draw.map_or(false, |t| t.elapsed() < min_interval) {
                continue;
            }
            if !shared.pending.swap(false, Ordering::AcqRel) {
                continue;
            }
            {
                shared.want.store(true, Ordering::Release);
                let mut term = shared.term.lock().unwrap();
                shared.want.store(false, Ordering::Release);
                term.take_dirty();
                let history = term.scrollback();
                let scrolled = history.scrolled_lines();
                if term.in_alternate_screen() {
                    back = 0; // full-screen programs: always live
                } else if back > 0 {
                    // New lines pushed the text we are looking at further up.
                    back += (scrolled - seen_scrolled) as usize;
                }
                seen_scrolled = scrolled;
                back = back.min(history.history_len());
                frame.update_view(
                    term.screen(),
                    history,
                    back,
                    true,
                    &palette,
                    &mut atlas,
                    &mut raster,
                );
            }
            if frame.damaged_rows() > 0 {
                surface.draw(&frame, &mut atlas)?;
                frame.clear_damage();
                last_draw = Some(Instant::now());
                if args.stats.is_some() {
                    frames.push(shared.epoch.elapsed().as_nanos() as u64);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn argv(a: &[&str]) -> Vec<String> {
            a.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn arguments_parse_and_the_shell_follows_the_separator() {
            let a = parse(&argv(&[
                "--backend",
                "cpu",
                "--px",
                "18",
                "--",
                "pwsh",
                "-NoLogo",
            ]))
            .unwrap();
            assert_eq!((a.backend.as_str(), a.px), ("cpu", 18.0));
            assert_eq!(a.shell, ["pwsh", "-NoLogo"]);
            assert!(!a.pipe);
            assert!(parse(&argv(&["--pipe", "--", "x"])).unwrap().pipe);
            assert!(parse(&argv(&["--backend", "gl"])).is_err());
            assert!(parse(&argv(&["--size", "10x10"])).is_err());
            assert_eq!(parse(&argv(&[])).unwrap().shell, ["cmd.exe"]);
        }

        #[test]
        fn keys_become_the_bytes_terminals_send() {
            let mut out = Vec::new();
            char_bytes('a', &mut out);
            char_bytes('\u{8}', &mut out);
            char_bytes('\r', &mut out);
            char_bytes('界', &mut out);
            assert_eq!(out, b"a\x7f\r\xe7\x95\x8c");
            assert_eq!(key_bytes(0x26), Some(&b"\x1b[A"[..]));
            assert_eq!(key_bytes(0x41), None, "letters come as characters");
        }
    }
}
