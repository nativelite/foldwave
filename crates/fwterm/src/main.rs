//! `fwterm` — foldwave's minimal terminal prototype.
//!
//! Panes in one window, each in one of two lanes, `vterm` for the screen,
//! and the `fwrender` window and backends for drawing. It exists to be
//! measured against other terminals with the same outside instruments, so it
//! does the least a terminal must: keys to the shell, output to the screen,
//! resize, split panes, select, copy and paste, and scrollback — recent
//! lines in memory, older ones rebuilt from the pane's log. Every pane is
//! logged by default, within a size budget that never fills the drive
//! (`logstore`). No tabs yet.
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
//! Panes split the window's cell grid (`layout`); every pane draws into the
//! same frame, so the renderer sees one grid whatever the layout. The keys
//! are Windows Terminal's defaults: Alt+Shift+Plus / Minus / D split, Alt+Arrow
//! and a click move the focus, Ctrl+Shift+W closes a pane. Dragging selects
//! (double-click: a word), Ctrl+Shift+C or Ctrl+C with a selection copies,
//! Ctrl+V or Ctrl+Shift+V pastes.
//!
//! Threads: per pane, a reader moves program output into its emulator
//! (answering the console host's startup queries on the way) and wakes the
//! window; the UI thread turns keys into bytes for the focused pane and draws
//! the panes that changed, at most once per display refresh.

#![forbid(unsafe_code)]
// A GUI program: Windows gives it no console window of its own. Messages
// still reach a terminal it was run from (`app::say`).
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod logstore;
#[cfg_attr(not(windows), allow(dead_code))]
mod select;

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
    use crate::layout::{neighbor, pane_at, Dir, Layout, PaneId, Placement, Rect, Toward};
    use crate::logstore;
    use crate::select::{self, Point, Selection};
    use fwrender::win::{display_refresh_hz, CpuPresenter, Event, Waker, Window};
    use fwrender::{
        Atlas, CellInstance, CpuRenderer, FontRasterizer, Frame, GlyphKey, GlyphPart, Highlight,
        Metrics, Palette, Rasterizer, Sharpening,
    };
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::SyncSender;
    use std::sync::{Arc, Mutex, Weak};
    use std::time::{Duration, Instant};
    use vterm::Term;

    /// Print a message for whoever started fwterm: to stderr when it goes
    /// somewhere (redirected to a file or pipe), else to the terminal fwterm
    /// was run from, if any.
    macro_rules! say {
        ($($t:tt)*) => {
            say(&format!($($t)*))
        };
    }

    fn say(msg: &str) {
        use std::os::windows::io::AsRawHandle;
        if !io::stderr().as_raw_handle().is_null() {
            eprintln!("{msg}");
        } else {
            // A console wants CR LF.
            let text = format!("{}\r\n", msg.replace('\n', "\r\n"));
            fwrender::win::write_parent_console(&text);
        }
    }

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
  --font NAME   font family, as installed (default Consolas); an unknown
                name lists the monospaced families found
  --px N        font size in pixels per em (default 16)
  --size WxH    client area in pixels (default 1200x720)
  --no-sharpen  draw the raw glyph outlines (no fitting or stem snapping)
  --max-fps N   frame-rate cap (default: the display's refresh rate)
  --title T     window title (default fwterm)
  --panes N     open N panes (1-64, default 1) in a near-square grid, each
                running the program
  --history N   lines of scrollback kept in memory (default 10000); scroll
                with the mouse wheel or Shift+PageUp/PageDown, and typing
                returns to the bottom
  --log DIR     keep pane logs under DIR. Every pane's whole output is logged
                (with a screen checkpoint each MiB), and scrolling past the
                in-memory history continues from the log. Default: the
                FOLDWAVE_LOG_DIR variable, else log_dir in
                %LOCALAPPDATA%\\foldwave\\fwterm.conf, else
                %LOCALAPPDATA%\\foldwave\\logs
  --log-cap N   keep at most N GiB of logs across every run (default 10, or
                log_cap_gib in the settings file). Oldest go first, and less is
                kept when the drive is nearly full: a tenth of it (2-20 GiB)
                always stays free
  --no-log      log nothing; scrollback is the in-memory history only
  --choose-log-drive   measure how fast each fixed drive takes durable writes,
                save the fastest with room as log_dir in the settings file,
                and exit
  --stats PATH  on exit, write frame statistics for the time output flowed:
                frames, frames per second, gaps between frames

keys:
  Alt+Shift+Plus    split the focused pane, the new one to the right
  Alt+Shift+Minus   split it, the new one below
  Alt+Shift+D       split it along its longer side
  Alt+Arrow, click  focus the pane in that direction / under the pointer
  Ctrl+Shift+W      close the focused pane (the last one closes the window)
  drag, double-click   select text in a pane / select a word
  Ctrl+Shift+C      copy the selection (Ctrl+C too while there is one;
                    otherwise Ctrl+C goes to the program)
  Ctrl+V, Ctrl+Shift+V   paste
  Shift+PgUp/PgDn, wheel   scroll the focused pane back / forward
";

    struct Args {
        backend: String,
        conpty: Option<String>,
        font: Option<String>,
        px: f32,
        size: (u32, u32),
        sharpen: bool,
        max_fps: Option<u32>,
        title: String,
        pipe: bool,
        stats: Option<String>,
        history: usize,
        log: Option<String>,
        log_cap: Option<u64>,
        no_log: bool,
        choose_log_drive: bool,
        panes: usize,
        shell: Vec<String>,
    }

    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut a = Args {
            backend: "vulkan".into(),
            conpty: None,
            font: None,
            px: 16.0,
            size: (1200, 720),
            sharpen: true,
            max_fps: None,
            title: "fwterm".into(),
            pipe: false,
            stats: None,
            history: 10_000,
            log: None,
            log_cap: None,
            no_log: false,
            choose_log_drive: false,
            panes: 1,
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
                "--font" => a.font = Some(value(&mut i, "--font")?),
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
                "--log" => a.log = Some(value(&mut i, "--log")?),
                "--log-cap" => {
                    let v = value(&mut i, "--log-cap")?;
                    let gib: u64 = v.parse().map_err(|_| format!("bad --log-cap {v:?}"))?;
                    a.log_cap = Some(gib << 30);
                }
                "--no-log" => {
                    a.no_log = true;
                    i += 1;
                }
                "--choose-log-drive" => {
                    a.choose_log_drive = true;
                    i += 1;
                }
                "--panes" => {
                    let v = value(&mut i, "--panes")?;
                    a.panes = v
                        .parse()
                        .ok()
                        .filter(|n| (1..=64).contains(n))
                        .ok_or(format!("bad --panes {v:?}"))?;
                }
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

    /// What the reader threads hand the logger, in the order the emulator saw
    /// it (sent while holding the pane lock).
    enum LogMsg {
        Data(Vec<u8>, u64),
        Checkpoint(Vec<u8>),
        /// Start the pane's next generation with this checkpoint at its
        /// offset 0 (`logstore`).
        Roll(Vec<u8>),
        /// Flush now and answer (on exit, so no output is lost).
        Flush(std::sync::mpsc::Sender<()>),
    }

    /// Chunks the logger may lag behind by (64 KiB each: 4 MiB). Past this the
    /// reader waits for the disk: memory stays bounded, output is never lost.
    const LOG_QUEUE: usize = 64;

    /// Checkpoint the screen once this much output has passed since the last.
    const CHECKPOINT_EVERY: u64 = 1024 * 1024;

    /// Start the logger thread for pane `id`: appends output and checkpoints
    /// to the pane's current generation under `pane_dir`, starting the next
    /// one when told, and flushing whenever it has nothing waiting so the log
    /// is readable up to the recent past.
    fn start_logger(pane_dir: PathBuf, id: PaneId) -> io::Result<SyncSender<LogMsg>> {
        let cfg = panelog::Config {
            pane: u64::from(std::process::id()) << 16 | id as u64,
            ..panelog::Config::default()
        };
        let open = move |n: u32| {
            let dir = pane_dir.join(logstore::gen_name(logstore::now_ms(), n));
            panelog::Writer::create(&dir, cfg)
        };
        let mut generation = 0;
        let mut writer = open(generation)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<LogMsg>(LOG_QUEUE);
        std::thread::spawn(move || {
            let mut dirty = false;
            loop {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(LogMsg::Data(bytes, t)) => {
                        if writer.append(&bytes, t).is_err() {
                            return;
                        }
                        dirty = true;
                    }
                    Ok(LogMsg::Checkpoint(blob)) => {
                        let _ = writer.checkpoint(&blob);
                        dirty = true;
                    }
                    Ok(LogMsg::Roll(blob)) => {
                        let _ = writer.flush();
                        generation += 1;
                        writer = match open(generation) {
                            Ok(w) => w,
                            Err(_) => return,
                        };
                        let _ = writer.checkpoint(&blob);
                        dirty = true;
                    }
                    Ok(LogMsg::Flush(done)) => {
                        let _ = writer.flush();
                        dirty = false;
                        let _ = done.send(());
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if dirty {
                            let _ = writer.flush();
                            dirty = false;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = writer.flush();
                        return;
                    }
                }
            }
        });
        Ok(tx)
    }

    const RED: &str = "\x1b[1;31m";
    const YELLOW: &str = "\x1b[1;33m";
    const CYAN: &str = "\x1b[36m";
    const DIM: &str = "\x1b[90m";
    const RESET: &str = "\x1b[0m";

    /// How often retention re-measures the volume and trims the logs.
    const RETENTION_EVERY: Duration = Duration::from_secs(2);

    /// A pane's way into its log: the logger, and the size at which retention
    /// wants generations rolled over.
    #[derive(Clone)]
    struct LogLink {
        tx: SyncSender<LogMsg>,
        gen_limit: Arc<AtomicU64>,
        /// Wakes retention: a generation just filled.
        nudge: SyncSender<()>,
    }

    /// The volume and the budget, as retention last measured them.
    type LogStatus = (Option<resmon::Volume>, logstore::Budget);

    /// Where this run logs, and the budget retention holds it to.
    struct LogStore {
        root: PathBuf,
        source: logstore::Source,
        session: logstore::Session,
        gen_limit: Arc<AtomicU64>,
        nudge: SyncSender<()>,
        status: Arc<Mutex<LogStatus>>,
    }

    /// This run's pane logs: on, or off and why.
    struct Logs {
        store: Option<LogStore>,
        /// Why logging is off when it was meant to be on.
        problem: Option<String>,
    }

    /// Measure the volume, set the budget and generation size, and delete the
    /// oldest generations over budget.
    fn enforce(
        root: &Path,
        own: &Path,
        cap: u64,
        gen_limit: &AtomicU64,
        status: &Mutex<LogStatus>,
    ) {
        let gens = logstore::scan(root, own);
        let logs: u64 = gens.iter().map(|g| g.bytes).sum();
        let volume = resmon::volume::of(root).ok();
        let budget = match &volume {
            Some(v) => logstore::budget(cap, logs, v.free, v.total),
            None => logstore::Budget {
                bytes: cap,
                squeezed: false,
            },
        };
        gen_limit.store(logstore::generation_bytes(budget.bytes), Ordering::Release);
        for gen in logstore::plan(&gens, budget.bytes) {
            logstore::delete(&gen);
        }
        *status.lock().unwrap() = (volume, budget);
    }

    /// The settings file, if there is one.
    fn settings() -> (Option<PathBuf>, logstore::Settings) {
        let app = logstore::app_dir(std::env::var_os("LOCALAPPDATA"));
        let settings = app
            .as_deref()
            .and_then(|a| std::fs::read_to_string(logstore::settings_path(a)).ok())
            .map(|t| logstore::parse_settings(&t))
            .unwrap_or_default();
        (app, settings)
    }

    impl Logs {
        fn open(args: &Args) -> Logs {
            let off = |problem: String| Logs {
                store: None,
                problem: Some(problem),
            };
            if args.no_log {
                return Logs {
                    store: None,
                    problem: None,
                };
            }
            let (app, settings) = settings();
            let Some((root, source)) = logstore::resolve(
                args.log.as_deref(),
                std::env::var_os("FOLDWAVE_LOG_DIR"),
                &settings,
                app.as_deref(),
            ) else {
                return off("no log folder (LOCALAPPDATA is not set); use --log DIR".into());
            };
            let cap = args
                .log_cap
                .or(settings.log_cap)
                .unwrap_or(logstore::DEFAULT_CAP);
            let session = match logstore::Session::create(&root) {
                Ok(s) => s,
                Err(e) => return off(format!("{}: {e}", root.display())),
            };
            let gen_limit = Arc::new(AtomicU64::new(logstore::generation_bytes(cap)));
            let status = Arc::new(Mutex::new((
                None,
                logstore::Budget {
                    bytes: cap,
                    squeezed: false,
                },
            )));
            // Once now, so the first panes' banners are right; then in the
            // background for as long as fwterm runs.
            enforce(&root, &session.dir, cap, &gen_limit, &status);
            // Every few seconds, and at once when a generation fills, so the
            // logs overshoot the budget by about one generation per pane.
            let (nudge, woken) = std::sync::mpsc::sync_channel::<()>(1);
            {
                let (root, own) = (root.clone(), session.dir.clone());
                let (gen_limit, status) = (gen_limit.clone(), status.clone());
                std::thread::spawn(move || loop {
                    if let Err(std::sync::mpsc::RecvTimeoutError::Disconnected) =
                        woken.recv_timeout(RETENTION_EVERY)
                    {
                        return;
                    }
                    enforce(&root, &own, cap, &gen_limit, &status);
                });
            }
            Logs {
                store: Some(LogStore {
                    root,
                    source,
                    session,
                    gen_limit,
                    nudge,
                    status,
                }),
                problem: None,
            }
        }

        /// The lines a new pane opens with: the version and where its log
        /// goes, and in colour anything to act on.
        fn banner(&self) -> String {
            let version = env!("CARGO_PKG_VERSION");
            match (&self.store, &self.problem) {
                (Some(store), _) => {
                    let (volume, budget) = store.status.lock().unwrap().clone();
                    // Below one generation per pane, each pane's newest
                    // generation is all that stays.
                    let keeping = if budget.bytes < logstore::generation_bytes(0) {
                        format!(
                            "only each pane's latest {}",
                            logstore::human(logstore::generation_bytes(budget.bytes))
                        )
                    } else {
                        format!("up to {}", logstore::human(budget.bytes))
                    };
                    let mut s = format!(
                        "{DIM}fwterm {version} · pane logs in {}, keeping {keeping}{RESET}\r\n",
                        store.root.display(),
                    );
                    if let (true, Some(v)) = (budget.squeezed, volume) {
                        s += &format!(
                            "{YELLOW}{} is almost full ({} free). Old logs will be overwritten \
                             to protect system integrity.{RESET}\r\n",
                            v.root.display(),
                            logstore::human(v.free)
                        );
                    }
                    if store.source == logstore::Source::Default {
                        s += &format!(
                            "{CYAN}Tip: fwterm --choose-log-drive measures your drives and \
                             keeps logs on the fastest.{RESET}\r\n"
                        );
                    }
                    s
                }
                (None, Some(problem)) => {
                    format!(
                        "{DIM}fwterm {version}{RESET}\r\n{RED}Logging is off: {problem}{RESET}\r\n"
                    )
                }
                (None, None) => format!("{DIM}fwterm {version} · logging off{RESET}\r\n"),
            }
        }
    }

    /// Durable appends to a scratch log at `root`: MiB/s and the p99 of one
    /// 1 MiB write, in ms.
    fn probe_drive(root: &Path) -> io::Result<(f64, f64)> {
        const WRITES: usize = 128;
        let dir = root.join(format!(
            "foldwave-drive-probe-{}-{}",
            logstore::now_ms(),
            std::process::id()
        ));
        // Made here or not at all: only a folder this call created is removed.
        std::fs::create_dir(&dir)?;
        let result = (|| {
            let mut w = panelog::Writer::create(&dir, panelog::Config::default())?;
            // One full record each: the payload a 1 MiB record carries.
            let chunk: Vec<u8> = (0..(1usize << 20) - 64)
                .map(|i| b"0123456789abcdef\n"[i % 17])
                .collect();
            let mut times = Vec::with_capacity(WRITES);
            let start = Instant::now();
            for _ in 0..WRITES {
                let t = Instant::now();
                w.append(&chunk, 0)?;
                w.flush()?;
                times.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let secs = start.elapsed().as_secs_f64();
            times.sort_by(f64::total_cmp);
            let mib = (chunk.len() * WRITES) as f64 / (1 << 20) as f64;
            Ok((mib / secs, times[times.len() * 99 / 100]))
        })();
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    /// `--choose-log-drive`: measure every fixed drive, save the fastest with
    /// room for the cap above its reserve.
    fn choose_log_drive(args: &Args) -> i32 {
        let (app, settings) = settings();
        let cap = args
            .log_cap
            .or(settings.log_cap)
            .unwrap_or(logstore::DEFAULT_CAP);
        say!("Measuring durable writes on each fixed drive (128 MiB each)...");
        let mut best: Option<(f64, PathBuf)> = None;
        for root in resmon::volume::fixed() {
            let Ok(v) = resmon::volume::of(&root) else {
                continue;
            };
            let budget = logstore::budget(cap, 0, v.free, v.total);
            match probe_drive(&root) {
                Ok((rate, p99)) => {
                    let room = if budget.squeezed {
                        format!(
                            "too full: room for {} of logs",
                            logstore::human(budget.bytes)
                        )
                    } else {
                        "room for the cap".into()
                    };
                    say!(
                        "  {}  {rate:7.0} MiB/s  p99 {p99:6.2} ms  {} free  {room}",
                        root.display(),
                        logstore::human(v.free)
                    );
                    if !budget.squeezed && best.as_ref().map_or(true, |(r, _)| rate > *r) {
                        best = Some((rate, root));
                    }
                }
                Err(e) => say!("  {}  cannot write here: {e}", root.display()),
            }
        }
        let Some((_, root)) = best else {
            say!(
                "No drive has room for {} of logs above its reserve; nothing saved.",
                logstore::human(cap)
            );
            return 1;
        };
        let Some(app) = app else {
            say!("LOCALAPPDATA is not set: nowhere to save the choice.");
            return 1;
        };
        let dir = root.join("foldwave").join("logs");
        let path = logstore::settings_path(&app);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let saved = std::fs::create_dir_all(&app)
            .and_then(|_| std::fs::write(&path, logstore::with_log_dir(&old, &dir)));
        match saved {
            Ok(()) => {
                say!(
                    "Logs will go to {} (saved in {}).",
                    dir.display(),
                    path.display()
                );
                0
            }
            Err(e) => {
                say!("Could not save {}: {e}", path.display());
                1
            }
        }
    }

    /// A pane rebuilt from its log at some past line: the scrollback beyond
    /// what memory holds.
    struct Deep {
        /// The pane's surviving generations, oldest first, each with the line
        /// it begins on, counted across them all: a generation begins on the
        /// line the one before it ended in.
        gens: Vec<(panelog::Reader, u64)>,
        /// Lines in the logs when they were opened; the view is measured from
        /// here.
        total: u64,
        /// The line the rebuilt screen ends before, and the rebuilt term.
        at: u64,
        term: Term,
    }

    impl Deep {
        fn open(pane_dir: &std::path::Path) -> io::Result<Deep> {
            let mut gens = Vec::new();
            let (mut start, mut total) = (0u64, 0u64);
            for g in logstore::generations(pane_dir) {
                // One retention is deleting cannot be opened; skip it.
                let Ok(reader) = panelog::Reader::open(&g) else {
                    continue;
                };
                let lines = reader.line_count();
                gens.push((reader, start));
                total = start + lines;
                start = total - 1;
            }
            if gens.is_empty() {
                return Err(io::ErrorKind::NotFound.into());
            }
            Ok(Deep {
                gens,
                total,
                at: u64::MAX,
                term: Term::new(1, 1),
            })
        }

        /// Rebuild the screen as it was when the logs reached line `end`:
        /// restore the nearest checkpoint before it in its generation, replay
        /// the rest.
        fn rebuild(&mut self, end: u64, rows: usize, cols: usize, pipe: bool) -> io::Result<()> {
            if end == self.at {
                return Ok(());
            }
            let k = self
                .gens
                .partition_point(|(_, start)| *start <= end)
                .saturating_sub(1);
            let (reader, start) = &self.gens[k];
            let local = (end.saturating_sub(*start)).min(reader.line_count() - 1);
            let offset = reader.line_start(local)?;
            let (mut term, from) = match reader.checkpoint_before(offset)? {
                Some((at, blob)) => match Term::restore(&blob) {
                    Ok(t) => (t, at),
                    Err(_) => (Term::new(rows, cols), 0),
                },
                None => (Term::new(rows, cols), 0),
            };
            if from == 0 && pipe {
                term.feed(b"\x1b[20h");
            }
            let mut buf = vec![0u8; 256 * 1024];
            let mut at = from;
            while at < offset {
                let want = ((offset - at) as usize).min(buf.len());
                let n = reader.read_at(at, &mut buf[..want])?;
                if n == 0 {
                    break;
                }
                term.feed(&buf[..n]);
                at += n as u64;
            }
            self.term = term;
            self.at = end;
            Ok(())
        }
    }

    /// One pane's emulator and the flags its reader threads share with the
    /// UI thread.
    struct Shared {
        term: Mutex<Term>,
        pending: AtomicBool,
        exited: AtomicBool,
        /// Output since the last checkpoint sent to the log.
        since_checkpoint: AtomicU64,
        /// Output in the log's current generation.
        gen_bytes: AtomicU64,
        /// The renderer is waiting for `term`; readers step aside. The lock is
        /// not fair, and a reader that releases and immediately re-takes it
        /// otherwise holds the renderer off for tens of milliseconds.
        want: AtomicBool,
    }

    /// When output flowed, across every pane, for `--stats`.
    struct Activity {
        epoch: Instant,
        /// Nanoseconds since `epoch` of the first and latest output read.
        first_output: AtomicU64,
        last_output: AtomicU64,
    }

    /// Frame statistics over the time output was arriving, for `--stats`.
    fn write_stats(path: &str, activity: &Activity, frames: &[u64]) -> io::Result<()> {
        let (first, last) = (
            activity.first_output.load(Ordering::Acquire),
            activity.last_output.load(Ordering::Acquire),
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

    /// One pane: a program in either lane, its emulator, its log, and where
    /// it sits and how far back it is scrolled.
    struct Pane {
        id: PaneId,
        shared: Arc<Shared>,
        session: Arc<Mutex<Session>>,
        log: Option<LogLink>,
        pane_dir: Option<PathBuf>,
        rect: Rect,
        /// Scrollback view: how many lines back from live, and the history's
        /// line count when last drawn, so the view stays on the same text
        /// while output keeps scrolling in.
        back: usize,
        seen_scrolled: u64,
        deep: Option<Deep>,
    }

    impl Pane {
        /// Give the pane new cells; its emulator and program follow.
        fn place(&mut self, rect: Rect) {
            if rect == self.rect {
                return;
            }
            self.rect = rect;
            let (rows, cols) = (rect.rows.max(1), rect.cols.max(1));
            self.shared.term.lock().unwrap().resize(rows, cols);
            self.session
                .lock()
                .unwrap()
                .resize(rows as u16, cols as u16);
            self.deep = None;
        }

        /// Have the logger write out what it holds (bounded wait).
        fn flush_log(&self) {
            if let Some(tx) = self.log.as_ref().map(|l| &l.tx) {
                let (done, wait) = std::sync::mpsc::channel();
                if tx.send(LogMsg::Flush(done)).is_ok() {
                    let _ = wait.recv_timeout(Duration::from_secs(2));
                }
            }
        }

        /// The output line and column under client pixel `(x, y)`, clamped
        /// into the pane (a drag can leave it). `None` while the view shows
        /// the log, whose lines are not the emulator's.
        fn point(&self, x: i32, y: i32, m: Metrics) -> Option<Point> {
            if self.deep.is_some() || !self.visible() {
                return None;
            }
            let r = self.rect;
            let row = (y.div_euclid(m.cell_h as i32) - r.top as i32).clamp(0, r.rows as i32 - 1);
            let col = (x.div_euclid(m.cell_w as i32) - r.left as i32).clamp(0, r.cols as i32 - 1);
            // The view as last drawn: its top row is `seen_scrolled - back`.
            let top = self.seen_scrolled.saturating_sub(self.back as u64);
            Some(Point {
                line: top + row as u64,
                col: col as usize,
            })
        }

        /// The characters of output line `n`, one per column (a wide
        /// character's second half repeats it), if the pane still holds it.
        fn line_chars(term: &Term, n: u64) -> Option<Vec<(char, bool)>> {
            let history = term.scrollback();
            let screen = term.screen();
            let scrolled = history.scrolled_lines();
            let mut row = Vec::new();
            let held = if n >= scrolled {
                let r = (n - scrolled) as usize;
                row.extend((0..screen.cols()).map(|c| screen.cell(r, c)));
                r < screen.rows()
            } else {
                history.history_row((scrolled - 1 - n) as usize, &mut row)
            };
            if !held {
                return None;
            }
            let mut lead = ' ';
            Some(
                (0..screen.cols())
                    .map(|c| {
                        let cell = row.get(c).copied().unwrap_or_default();
                        let half = cell.width == ansi::CellWidth::Continuation;
                        if !half {
                            lead = cell.ch;
                        }
                        (if half { lead } else { cell.ch }, half)
                    })
                    .collect(),
            )
        }

        /// The selected text.
        fn selection_text(&self, sel: &Selection) -> String {
            let term = self.shared.term.lock().unwrap();
            let (start, end) = sel.ordered();
            select::text(start, end, term.screen().cols(), |n, from, to| {
                let chars = Self::line_chars(&term, n)?;
                Some(
                    chars[from.min(chars.len())..to.min(chars.len())]
                        .iter()
                        .filter(|(_, half)| !half)
                        .map(|(c, _)| c)
                        .collect(),
                )
            })
        }

        /// The word under `at`, as a selection.
        fn word_at(&self, at: Point) -> Option<Selection> {
            let term = self.shared.term.lock().unwrap();
            let chars: Vec<char> = Self::line_chars(&term, at.line)?
                .into_iter()
                .map(|(c, _)| c)
                .collect();
            let (first, last) = select::word_at(&chars, at.col);
            Some(Selection {
                anchor: Point {
                    line: at.line,
                    col: first,
                },
                head: Point {
                    line: at.line,
                    col: last,
                },
            })
        }

        /// Whether the pane has any cells to draw.
        fn visible(&self) -> bool {
            self.rect.rows > 0 && self.rect.cols > 0
        }

        /// Bring this pane's part of `frame` up to date. The caller has set
        /// `want` (see the draw loop); it is cleared once the lock is held.
        fn draw(
            &mut self,
            focused: bool,
            selection: Option<&Selection>,
            pipe: bool,
            frame: &mut Frame,
            pen: &mut Pen,
        ) {
            let r = self.rect;
            let shared = self.shared.clone();
            let mut term = shared.term.lock().unwrap();
            shared.want.store(false, Ordering::Release);
            term.take_dirty();
            let history = term.scrollback();
            let scrolled = history.scrolled_lines();
            if term.in_alternate_screen() {
                self.back = 0; // full-screen programs: always live
            } else if self.back > 0 && self.deep.is_none() {
                // New lines pushed the text we are looking at further up.
                self.back += (scrolled - self.seen_scrolled) as usize;
            }
            self.seen_scrolled = scrolled;
            let held = history.history_len();
            let (rows, cols) = (term.screen().rows(), term.screen().cols());
            if self.back > held && self.pane_dir.is_some() && !term.in_alternate_screen() {
                // Past what memory holds: continue from the log. Its snapshot
                // is frozen, so the view needs no anchoring.
                if self.deep.is_none() {
                    self.deep = self.pane_dir.as_deref().and_then(|f| Deep::open(f).ok());
                }
                if let Some(d) = self.deep.as_mut() {
                    self.back = self.back.min(d.total as usize);
                    // The view's bottom is `back` lines above live, in log
                    // lines (a wrapped line counts once here, once per row in
                    // memory, so the hand-over is approximate).
                    let end = d.total.saturating_sub(self.back as u64);
                    let _ = d.rebuild(end.max(1), rows, cols, pipe);
                    let s = d.term.screen();
                    frame.update_view_at(
                        r.top,
                        r.left,
                        s,
                        s,
                        0,
                        false,
                        None,
                        pen.palette,
                        pen.atlas,
                        pen.raster,
                    );
                }
            } else {
                self.deep = None;
                self.back = self.back.min(held);
                let top = scrolled.saturating_sub(self.back as u64);
                let highlight = selection
                    .and_then(|sel| sel.in_view(top, rows, cols))
                    .map(|(start, end)| Highlight { start, end });
                frame.update_view_at(
                    r.top,
                    r.left,
                    term.screen(),
                    history,
                    self.back,
                    focused,
                    highlight,
                    pen.palette,
                    pen.atlas,
                    pen.raster,
                );
            }
        }
    }

    /// What drawing a cell needs.
    struct Pen<'a> {
        palette: &'a Palette,
        atlas: &'a mut Atlas,
        raster: &'a mut FontRasterizer,
    }

    /// Divider colours: the lines around the focused pane stand out.
    const DIVIDER: u32 = 0xFF4A_4A4A;
    const DIVIDER_FOCUSED: u32 = 0xFF3A_96DD;

    /// Draw the dividers of `placement`, those touching the focused pane in
    /// the focus colour.
    fn draw_dividers(placement: &Placement, focus: PaneId, frame: &mut Frame, pen: &mut Pen) {
        let f = placement
            .panes
            .iter()
            .find(|(id, _)| *id == focus)
            .map(|(_, r)| *r)
            .unwrap_or_default();
        for &(row, col, ch) in &placement.dividers {
            let near = row + 1 >= f.top
                && row <= f.top + f.rows
                && col + 1 >= f.left
                && col <= f.left + f.cols;
            let glyph = pen.atlas.slot(
                GlyphKey {
                    ch,
                    style: 0,
                    part: GlyphPart::Single,
                },
                pen.raster,
            );
            frame.put(
                row,
                col,
                CellInstance {
                    glyph,
                    fg: if near { DIVIDER_FOCUSED } else { DIVIDER },
                    bg: pen.palette.bg,
                    flags: 0,
                },
            );
        }
    }

    /// Move one output stream into a pane's emulator until it ends. The
    /// session, for answering the console host, is held weakly: a closed
    /// pane's session must drop, which closes the pseudoconsole and so ends
    /// this stream.
    #[allow(clippy::too_many_arguments)]
    fn pump(
        mut source: Box<dyn FnMut(&mut [u8]) -> io::Result<usize> + Send>,
        session: Option<Weak<Mutex<Session>>>,
        last: bool,
        shared: Arc<Shared>,
        activity: Arc<Activity>,
        waker: Waker,
        log: Option<LogLink>,
    ) {
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            let mut watcher = fwbench::terminal::Watcher::new();
            loop {
                let n = match source(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if let Some(session) = session.as_ref().and_then(Weak::upgrade) {
                    for reply in watcher.replies(&buf[..n]) {
                        let _ = session.lock().unwrap().write(reply);
                    }
                }
                while shared.want.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                let now = activity.epoch.elapsed().as_nanos() as u64;
                {
                    let mut term = shared.term.lock().unwrap();
                    // The log gets the bytes before the emulator does (design
                    // §8.1), so a parser failure cannot lose them; both see
                    // the same order, under the pane lock.
                    if let Some(log) = &log {
                        let _ = log.tx.send(LogMsg::Data(buf[..n].to_vec(), now));
                    }
                    term.feed(&buf[..n]);
                    // Checkpoint at a point the replay can resume from; roll
                    // over to a new generation at one.
                    if let Some(log) = &log {
                        let since = shared
                            .since_checkpoint
                            .fetch_add(n as u64, Ordering::AcqRel)
                            + n as u64;
                        let generation =
                            shared.gen_bytes.fetch_add(n as u64, Ordering::AcqRel) + n as u64;
                        if since >= CHECKPOINT_EVERY {
                            if let Some(blob) = term.checkpoint() {
                                if generation >= log.gen_limit.load(Ordering::Acquire) {
                                    let _ = log.tx.send(LogMsg::Roll(blob));
                                    let _ = log.nudge.try_send(());
                                    shared.gen_bytes.store(0, Ordering::Release);
                                } else {
                                    let _ = log.tx.send(LogMsg::Checkpoint(blob));
                                }
                                shared.since_checkpoint.store(0, Ordering::Release);
                            }
                        }
                    }
                }
                activity.first_output.fetch_min(now, Ordering::AcqRel);
                activity.last_output.store(now, Ordering::Release);
                if !shared.pending.swap(true, Ordering::AcqRel) {
                    waker.wake();
                }
            }
            if last {
                shared.exited.store(true, Ordering::Release);
                waker.wake();
            }
        });
    }

    /// How often a console pane checks whether its program has exited.
    const EXIT_POLL: Duration = Duration::from_millis(50);

    /// Mark the pane exited when its console program ends. The console host
    /// keeps the output pipe open after its last client exits (until the
    /// pseudoconsole is closed), so the end of output cannot tell. Ends when
    /// the pane is gone.
    fn watch_exit(session: Weak<Mutex<Session>>, shared: Arc<Shared>, waker: Waker) {
        std::thread::spawn(move || loop {
            std::thread::sleep(EXIT_POLL);
            let Some(session) = session.upgrade() else {
                return;
            };
            let running = match &mut *session.lock().unwrap() {
                Session::Console(p) => matches!(p.try_wait(), Ok(None)),
                Session::Pipe { .. } => false,
            };
            if !running {
                shared.exited.store(true, Ordering::Release);
                waker.wake();
                return;
            }
        });
    }

    /// Starts panes: the program from the command line, in the chosen lane.
    struct Spawner<'a> {
        args: &'a Args,
        logs: &'a Logs,
        waker: Waker,
        activity: Arc<Activity>,
    }

    impl Spawner<'_> {
        fn spawn(&self, id: PaneId, rect: Rect) -> io::Result<Pane> {
            let args = self.args;
            let (rows, cols) = (rect.rows.max(1), rect.cols.max(1));
            let shell_args: Vec<&str> = args.shell[1..].iter().map(String::as_str).collect();
            let shared = Arc::new(Shared {
                term: Mutex::new({
                    let mut t = Term::with_history(rows, cols, args.history);
                    if args.pipe {
                        // No tty turns LF into CR LF on a pipe: new line mode
                        // (LNM) makes the emulator do it, with no copy.
                        t.feed(b"\x1b[20h");
                    }
                    t.feed(self.logs.banner().as_bytes());
                    t
                }),
                pending: AtomicBool::new(true),
                exited: AtomicBool::new(false),
                since_checkpoint: AtomicU64::new(0),
                gen_bytes: AtomicU64::new(0),
                want: AtomicBool::new(false),
            });
            let (log, pane_dir) = match &self.logs.store {
                Some(store) => {
                    let dir = store.session.pane_dir(id);
                    match start_logger(dir.clone(), id) {
                        Ok(tx) => (
                            Some(LogLink {
                                tx,
                                gen_limit: store.gen_limit.clone(),
                                nudge: store.nudge.clone(),
                            }),
                            Some(dir),
                        ),
                        Err(e) => {
                            let mut t = shared.term.lock().unwrap();
                            t.feed(
                                format!("{RED}This pane is not logged: {e}{RESET}\r\n").as_bytes(),
                            );
                            (None, None)
                        }
                    }
                }
                None => (None, None),
            };
            let pump = |source, session, last| {
                pump(
                    source,
                    session,
                    last,
                    shared.clone(),
                    self.activity.clone(),
                    self.waker.clone(),
                    log.clone(),
                )
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
                let pty = pty::Pty::spawn(&args.shell[0], &shell_args, rows as u16, cols as u16)?;
                let mut reader = pty.reader()?;
                let session = Arc::new(Mutex::new(Session::Console(pty)));
                pump(
                    Box::new(move |b| reader.read(b)),
                    Some(Arc::downgrade(&session)),
                    true,
                );
                watch_exit(Arc::downgrade(&session), shared.clone(), self.waker.clone());
                session
            };
            Ok(Pane {
                id,
                shared,
                session,
                log,
                pane_dir,
                rect,
                back: 0,
                seen_scrolled: 0,
                deep: None,
            })
        }
    }

    pub fn run() -> i32 {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let args = match parse(&argv) {
            Ok(a) => a,
            Err(msg) => {
                say!("{msg}");
                return 2;
            }
        };
        if args.choose_log_drive {
            return choose_log_drive(&args);
        }
        match start(&args) {
            Ok(code) => code,
            Err(e) => {
                say!("fwterm: {e}");
                1
            }
        }
    }

    /// Give every pane its cells for `placement`.
    fn place_all(panes: &mut [Pane], placement: &Placement) {
        for &(id, rect) in &placement.panes {
            if let Some(p) = panes.iter_mut().find(|p| p.id == id) {
                p.place(rect);
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
                    say!(
                        "fwterm: {} would not load ({e}); using the built-in console host",
                        dll.display()
                    );
                }
            }
        }
        let mut raster = match &args.font {
            // Consolas by its known files: no need to scan the font folders.
            None => FontRasterizer::consolas(args.px)?,
            Some(name) => {
                let catalog = fwrender::FontCatalog::system();
                let family = catalog.family(name).map_err(|e| {
                    let known = catalog.monospace_families().join(", ");
                    io::Error::new(e.kind(), format!("{e}; monospaced fonts found: {known}"))
                })?;
                FontRasterizer::from_family(&family, args.px)?
            }
        };
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
                    say!("fwterm: Vulkan unavailable ({e}); drawing on the CPU");
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

        let grid = |w: u32, h: u32| {
            (
                (h / m.cell_h).max(1) as usize,
                (w / m.cell_w).max(1) as usize,
            )
        };
        let (mut grid_rows, mut grid_cols) = grid(w, h);
        let activity = Arc::new(Activity {
            epoch: Instant::now(),
            first_output: AtomicU64::new(u64::MAX),
            last_output: AtomicU64::new(0),
        });
        let logs = Logs::open(args);
        let spawner = Spawner {
            args,
            logs: &logs,
            waker: surface.window().waker(),
            activity: activity.clone(),
        };
        let ids: Vec<PaneId> = (0..args.panes).collect();
        let mut layout = Layout::grid(&ids);
        let mut placement = layout.place(grid_rows, grid_cols);
        let mut panes = placement
            .panes
            .iter()
            .map(|&(id, rect)| spawner.spawn(id, rect))
            .collect::<io::Result<Vec<Pane>>>()?;
        let mut next_id = args.panes;
        let mut focus: PaneId = 0;

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
        // The panes moved, or the focus did: redraw every pane and divider.
        let mut layout_dirty = true;
        // A key taken as a command still sends its character; drop it.
        let mut drop_char = false;
        // The selection (one at a time, in one pane), and where a press that
        // may become a drag started.
        let mut selection: Option<(PaneId, Selection)> = None;
        let mut press: Option<(PaneId, Point)> = None;
        // Mark the pane holding the selection for redraw, and drop it.
        let clear = |selection: &mut Option<(PaneId, Selection)>, panes: &[Pane]| {
            if let Some((id, _)) = selection.take() {
                if let Some(p) = panes.iter().find(|p| p.id == id) {
                    p.shared.pending.store(true, Ordering::Release);
                }
            }
        };
        let mut frames: Vec<u64> = Vec::new();
        let finish = |frames: &[u64]| {
            if let Some(path) = &args.stats {
                if let Err(e) = write_stats(path, &activity, frames) {
                    say!("fwterm: --stats {path}: {e}");
                }
            }
        };
        loop {
            // Sleep until an event, new output, or the next allowed frame.
            let due = layout_dirty
                || panes
                    .iter()
                    .any(|p| p.shared.pending.load(Ordering::Acquire));
            let timeout = match last_draw {
                Some(t) if due => Some(min_interval.saturating_sub(t.elapsed())),
                _ => None,
            };
            events.clear();
            surface.window().wait(timeout, &mut events);
            input.clear();
            // Panes to take out after the events: (pane, kill it).
            let mut closing: Vec<(PaneId, bool)> = Vec::new();
            for e in &events {
                let Some(fi) = panes.iter().position(|p| p.id == focus) else {
                    break;
                };
                match *e {
                    Event::Char(_) if drop_char => drop_char = false,
                    Event::Char(c) => {
                        // Typing returns the view to live output and ends a
                        // selection.
                        panes[fi].back = 0;
                        clear(&mut selection, &panes);
                        char_bytes(c, &mut input);
                    }
                    Event::Key { vk, down: true } => {
                        let (ctrl, shift) =
                            (fwrender::win::ctrl_down(), fwrender::win::shift_down());
                        // AltGr is Ctrl+Alt: leave its characters alone.
                        let ctrl = ctrl && !fwrender::win::alt_down();
                        let page = panes[fi].rect.rows.max(2) - 1;
                        // Ctrl+Shift+C, or Ctrl+C while something is selected:
                        // copy. Plain Ctrl+C otherwise reaches the program.
                        if vk == 0x43 && ctrl && (shift || selection.is_some()) {
                            if let Some((id, sel)) = selection {
                                if let Some(p) = panes.iter().find(|p| p.id == id) {
                                    let text = p.selection_text(&sel);
                                    if let Err(e) = surface.window().set_clipboard(&text) {
                                        say!("fwterm: copy: {e}");
                                    }
                                }
                                if !shift {
                                    clear(&mut selection, &panes);
                                }
                            }
                            drop_char = true;
                            continue;
                        }
                        // Ctrl+V / Ctrl+Shift+V: paste.
                        if vk == 0x56 && ctrl {
                            match surface.window().clipboard() {
                                Ok(Some(text)) => {
                                    input.extend_from_slice(&select::paste_bytes(&text));
                                    panes[fi].back = 0;
                                    clear(&mut selection, &panes);
                                }
                                Ok(None) => {}
                                Err(e) => say!("fwterm: paste: {e}"),
                            }
                            drop_char = true;
                            continue;
                        }
                        let pane = &mut panes[fi];
                        match vk {
                            // Ctrl+Shift+W: close the pane.
                            0x57 if ctrl && shift => {
                                closing.push((focus, true));
                                drop_char = true;
                            }
                            0x21 if shift => pane.back += page,
                            0x22 if shift => pane.back = pane.back.saturating_sub(page),
                            _ => {
                                if let Some(b) = key_bytes(vk) {
                                    pane.back = 0;
                                    input.extend_from_slice(b);
                                }
                            }
                        }
                        pane.shared.pending.store(true, Ordering::Release);
                    }
                    Event::SysKey { vk, down: true } => {
                        let shift = fwrender::win::shift_down();
                        let r = panes[fi].rect;
                        let dir = match vk {
                            // Alt+Shift+Plus / Minus / D: split the pane.
                            0xBB | 0x6B if shift => Some(Dir::Across),
                            0xBD | 0x6D if shift => Some(Dir::Down),
                            0x44 if shift => {
                                Some(if r.cols as u32 * m.cell_w >= r.rows as u32 * m.cell_h {
                                    Dir::Across
                                } else {
                                    Dir::Down
                                })
                            }
                            _ => None,
                        };
                        let toward = match vk {
                            0x25 if !shift => Some(Toward::Left),
                            0x27 if !shift => Some(Toward::Right),
                            0x26 if !shift => Some(Toward::Up),
                            0x28 if !shift => Some(Toward::Down),
                            _ => None,
                        };
                        if let Some(dir) = dir {
                            // Both halves need a cell, with a divider between.
                            let room = match dir {
                                Dir::Across => r.cols,
                                Dir::Down => r.rows,
                            };
                            let id = next_id;
                            if room >= 3 && layout.split(focus, dir, id) {
                                next_id += 1;
                                placement = layout.place(grid_rows, grid_cols);
                                let rect = placement
                                    .panes
                                    .iter()
                                    .find(|(p, _)| *p == id)
                                    .map(|(_, r)| *r)
                                    .unwrap_or_default();
                                match spawner.spawn(id, rect) {
                                    Ok(p) => {
                                        panes.push(p);
                                        focus = id;
                                    }
                                    Err(e) => {
                                        say!("fwterm: new pane: {e}");
                                        layout.remove(id);
                                        placement = layout.place(grid_rows, grid_cols);
                                    }
                                }
                                place_all(&mut panes, &placement);
                                layout_dirty = true;
                                selection = None;
                                press = None;
                            }
                        } else if let Some(to) = toward {
                            if let Some(next) = neighbor(&placement.panes, focus, to) {
                                focus = next;
                                layout_dirty = true;
                            }
                        }
                    }
                    Event::Click(x, y) | Event::DoubleClick(x, y) if x >= 0 && y >= 0 => {
                        let (row, col) = (
                            y as usize / m.cell_h as usize,
                            x as usize / m.cell_w as usize,
                        );
                        clear(&mut selection, &panes);
                        press = None;
                        if let Some(id) = pane_at(&placement.panes, row, col) {
                            if id != focus {
                                focus = id;
                                layout_dirty = true;
                            }
                            let pane = panes.iter().find(|p| p.id == id);
                            if let Some((pane, at)) =
                                pane.and_then(|p| Some((p, p.point(x, y, m)?)))
                            {
                                if matches!(*e, Event::DoubleClick(..)) {
                                    selection = pane.word_at(at).map(|s| (id, s));
                                    pane.shared.pending.store(true, Ordering::Release);
                                } else {
                                    // A selection starts once the drag leaves
                                    // this cell.
                                    press = Some((id, at));
                                }
                            }
                        }
                    }
                    Event::Drag(x, y) => {
                        if let Some((id, anchor)) = press {
                            if let Some(pane) = panes.iter().find(|p| p.id == id) {
                                if let Some(head) = pane.point(x, y, m) {
                                    if head != anchor || selection.is_some() {
                                        selection = Some((id, Selection { anchor, head }));
                                        pane.shared.pending.store(true, Ordering::Release);
                                    }
                                }
                            }
                        }
                    }
                    Event::Release(..) => press = None,
                    Event::Wheel(delta) => {
                        // Three lines a notch, away from the user = back.
                        let lines = (delta.unsigned_abs() as usize * 3).div_euclid(120).max(1);
                        let pane = &mut panes[fi];
                        pane.back = if delta > 0 {
                            pane.back + lines
                        } else {
                            pane.back.saturating_sub(lines)
                        };
                        pane.shared.pending.store(true, Ordering::Release);
                    }
                    Event::Resized(w, h) if w > 0 && h > 0 => {
                        surface.resize(w, h)?;
                        (grid_rows, grid_cols) = grid(w, h);
                        placement = layout.place(grid_rows, grid_cols);
                        place_all(&mut panes, &placement);
                        frame.damage_all();
                        layout_dirty = true;
                        // Rows rewrap at a new width; the selection would
                        // point at other text.
                        selection = None;
                        press = None;
                    }
                    Event::CloseRequested => {
                        finish(&frames);
                        for p in &panes {
                            p.flush_log();
                        }
                        // Take the programs down with the window.
                        for p in &panes {
                            p.session.lock().unwrap().kill();
                        }
                        return Ok(0);
                    }
                    _ => {}
                }
            }
            if !input.is_empty() {
                // A pipe program that has exited or closed stdin just drops
                // the keys.
                if let Some(p) = panes.iter().find(|p| p.id == focus) {
                    let _ = p.session.lock().unwrap().write(&input);
                }
            }
            closing.extend(
                panes
                    .iter()
                    .filter(|p| p.shared.exited.load(Ordering::Acquire))
                    .map(|p| (p.id, false)),
            );
            let mut code = 0;
            for (id, kill) in closing {
                let Some(i) = panes.iter().position(|p| p.id == id) else {
                    continue;
                };
                let pane = panes.remove(i);
                pane.flush_log();
                let mut session = pane.session.lock().unwrap();
                code = if kill {
                    session.kill();
                    0
                } else {
                    session.wait()
                };
                drop(session);
                let next = layout.remove(id);
                if focus == id {
                    focus = next.unwrap_or(0);
                }
                layout_dirty = true;
                selection = None;
                press = None;
            }
            if layout.is_empty() {
                // The last pane closed: so does the window, with its code.
                finish(&frames);
                return Ok(code);
            }
            if layout_dirty {
                placement = layout.place(grid_rows, grid_cols);
                place_all(&mut panes, &placement);
            }
            if last_draw.map_or(false, |t| t.elapsed() < min_interval) {
                continue;
            }
            let mut pen = Pen {
                palette: &palette,
                atlas: &mut atlas,
                raster: &mut raster,
            };
            frame.set_grid(grid_rows, grid_cols, &palette);
            // Ask every pane to be drawn for its lock first, then take them in
            // turn: the readers finish their current chunks at the same time,
            // so the frame waits for the slowest, not the sum of all.
            let due: Vec<bool> = panes
                .iter()
                .map(|p| {
                    let due = (p.shared.pending.swap(false, Ordering::AcqRel) || layout_dirty)
                        && p.visible();
                    if due {
                        p.shared.want.store(true, Ordering::Release);
                    }
                    due
                })
                .collect();
            let mut drew = false;
            for (p, due) in panes.iter_mut().zip(due) {
                if due {
                    let sel = selection
                        .as_ref()
                        .filter(|(id, _)| *id == p.id)
                        .map(|(_, s)| s);
                    p.draw(p.id == focus, sel, args.pipe, &mut frame, &mut pen);
                    drew = true;
                }
            }
            if layout_dirty {
                draw_dividers(&placement, focus, &mut frame, &mut pen);
                layout_dirty = false;
            }
            if drew && frame.damaged_rows() > 0 {
                surface.draw(&frame, &mut atlas)?;
                frame.clear_damage();
                last_draw = Some(Instant::now());
                if args.stats.is_some() {
                    frames.push(activity.epoch.elapsed().as_nanos() as u64);
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

        fn lines(range: std::ops::Range<u32>) -> Vec<u8> {
            range
                .map(|i| format!("L{i}\r\n"))
                .collect::<String>()
                .into_bytes()
        }

        fn row(term: &Term, r: usize) -> String {
            let s = term.screen();
            (0..s.cols())
                .map(|c| s.cell(r, c).ch)
                .collect::<String>()
                .trim_end()
                .to_string()
        }

        #[test]
        fn scrollback_continues_across_log_generations() {
            let pane = std::env::temp_dir().join(format!("fwterm-deep-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&pane);
            let cfg = panelog::Config::default();
            // Generation 0 holds L0..L9; the pane rolls over at a checkpoint,
            // and generation 1 holds L10..L19.
            let mut term = Term::new(4, 10);
            {
                let mut w =
                    panelog::Writer::create(&pane.join(logstore::gen_name(1, 0)), cfg).unwrap();
                w.append(&lines(0..10), 0).unwrap();
                term.feed(&lines(0..10));
            }
            {
                let mut w =
                    panelog::Writer::create(&pane.join(logstore::gen_name(2, 1)), cfg).unwrap();
                w.checkpoint(&term.checkpoint().unwrap()).unwrap();
                w.append(&lines(10..20), 0).unwrap();
            }
            let mut d = Deep::open(&pane).unwrap();
            // 11 line starts in each; they share the line the roll fell on.
            assert_eq!(d.total, 21);
            let at = |d: &mut Deep, end: u64| {
                d.rebuild(end, 4, 10, false).unwrap();
                row(&d.term, 2)
            };
            assert_eq!(at(&mut d, 5), "L4", "inside the first generation");
            assert_eq!(at(&mut d, 10), "L9", "at the roll: the checkpoint alone");
            assert_eq!(
                at(&mut d, 15),
                "L14",
                "inside the second, from its checkpoint"
            );

            // Retention deleted the first: the second still rebuilds alone.
            logstore::delete(&pane.join(logstore::gen_name(1, 0)));
            let mut d = Deep::open(&pane).unwrap();
            assert_eq!(d.total, 11);
            assert_eq!(at(&mut d, 5), "L14");
            let _ = std::fs::remove_dir_all(&pane);
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
            let f = parse(&argv(&["--font", "Cascadia Mono"])).unwrap();
            assert_eq!(f.font.as_deref(), Some("Cascadia Mono"));
            assert!(parse(&argv(&[])).unwrap().font.is_none());
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
