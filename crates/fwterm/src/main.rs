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
//! and a click move the focus, Ctrl+Shift+W closes a pane, Ctrl+Shift+Z
//! zooms one (the others hidden: they only log until shown, `governor`). Dragging selects
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
mod blocks;
#[cfg_attr(not(windows), allow(dead_code))]
mod emu;
#[cfg_attr(not(windows), allow(dead_code))]
mod find;
#[cfg_attr(not(windows), allow(dead_code))]
mod governor;
#[cfg_attr(not(windows), allow(dead_code))]
mod indexer;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod logstore;
#[cfg_attr(not(windows), allow(dead_code))]
mod select;
#[cfg_attr(not(windows), allow(dead_code))]
mod wire;

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
    use crate::blocks;
    use crate::emu::Emu;
    use crate::wire;

    mod host;
    use crate::find;
    use crate::governor::{self, Hidden, Level, Place};
    use crate::indexer;
    use crate::layout::{neighbor, pane_at, Dir, Layout, PaneId, Placement, Rect, Toward};
    use crate::logstore;
    use crate::select::{self, Point, Selection};
    use fwrender::win::{display_refresh_hz, CpuPresenter, Event, Waker, Window};
    use fwrender::{
        Atlas, CellInstance, CpuRenderer, FontRasterizer, Frame, GlyphKey, GlyphPart, Highlight,
        Metrics, Palette, Rasterizer, Sharpening,
    };
    use std::collections::HashMap;
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
  --place P     where pane readers and the UI thread run: off (default: where
                the OS puts them) or fast (the fast cores of a hybrid CPU,
                CPU 0 left out; measured no better on an i5-12600K)
  --zoom        start with the first pane zoomed (Ctrl+Shift+Z toggles)
  --hidden H    when a pane hidden behind a zoomed one stops parsing and only
                logs, catching up from its log when shown again: always
                (default), busy (only while other programs keep the CPU
                nearly full), or never. Without a log it keeps parsing
  --isolate     run each pane's program, parser and log in a pane host
                process of its own (design 8.8a): a crash there ends that
                pane, not the window
  --shell-integration S  on (default): cmd.exe marks its prompts and commands
                (OSC 133, through PROMPT), so Ctrl+Up/Down and Ctrl+Shift+O
                work; other shells mark them themselves if set up to; off
  --gpu-index G on (default): build the logs' search signatures on the GPU
                (NVIDIA, through its driver), off the loggers' threads; off:
                on each pane's logger thread. Without CUDA it is off
  --governor G  on (default): while other programs keep the CPU nearly
                full, draw at most 30 frames a second except just after
                input; off: always draw at the display's rate
  --stats PATH  on exit, write frame statistics for the time output flowed:
                frames, frames per second, gaps between frames

keys:
  Alt+Shift+Plus    split the focused pane, the new one to the right
  Alt+Shift+Minus   split it, the new one below
  Alt+Shift+D       split it along its longer side
  Alt+Arrow, click  focus the pane in that direction / under the pointer
  Ctrl+Shift+W      close the focused pane (the last one closes the window)
  Ctrl+Shift+Z      zoom the focused pane to the whole window, or back
  Ctrl+Up/Down      jump to the previous / next command's prompt
  Ctrl+Shift+O      select a command's output (then Ctrl+C copies it)
  Ctrl+Shift+F      search every pane's log: type, Enter searches, Enter /
                    Shift+Enter step through hits (newest first), Esc closes
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
        place: Place,
        governor: bool,
        gpu_index: bool,
        shell_integration: bool,
        /// Run as a pane host (`app::host`), for a window started with
        /// `--isolate`: its screen size, log folder, generation size, pane id.
        pane_host: bool,
        /// Each pane's program and parser in a pane host process.
        isolate: bool,
        host_size: (u16, u16),
        host_log: Option<String>,
        host_gen: u64,
        host_id: usize,
        hidden: Hidden,
        zoom: bool,
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
            place: Place::Off,
            governor: true,
            gpu_index: true,
            shell_integration: true,
            pane_host: false,
            isolate: false,
            host_size: (24, 80),
            host_log: None,
            host_gen: 256 << 20,
            host_id: 0,
            hidden: Hidden::Always,
            zoom: false,
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
                "--place" => {
                    let v = value(&mut i, "--place")?;
                    a.place = match v.as_str() {
                        "fast" => Place::Fast,
                        "off" => Place::Off,
                        _ => return Err(format!("bad --place {v:?}")),
                    };
                }
                "--zoom" => {
                    a.zoom = true;
                    i += 1;
                }
                "--hidden" => {
                    let v = value(&mut i, "--hidden")?;
                    a.hidden = match v.as_str() {
                        "busy" => Hidden::Busy,
                        "always" => Hidden::Always,
                        "never" => Hidden::Never,
                        _ => return Err(format!("bad --hidden {v:?}")),
                    };
                }
                "--pane-host" => {
                    a.pane_host = true;
                    i += 1;
                }
                "--isolate" => {
                    a.isolate = true;
                    i += 1;
                }
                "--host-size" => {
                    let v = value(&mut i, "--host-size")?;
                    a.host_size = v
                        .split_once('x')
                        .and_then(|(r, c)| Some((r.parse().ok()?, c.parse().ok()?)))
                        .ok_or(format!("bad --host-size {v:?}"))?;
                }
                "--host-log" => a.host_log = Some(value(&mut i, "--host-log")?),
                "--host-gen" => {
                    let v = value(&mut i, "--host-gen")?;
                    a.host_gen = v.parse().map_err(|_| format!("bad --host-gen {v:?}"))?;
                }
                "--host-id" => {
                    let v = value(&mut i, "--host-id")?;
                    a.host_id = v.parse().map_err(|_| format!("bad --host-id {v:?}"))?;
                }
                "--shell-integration" => {
                    let v = value(&mut i, "--shell-integration")?;
                    a.shell_integration = match v.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err(format!("bad --shell-integration {v:?}")),
                    };
                }
                "--gpu-index" => {
                    let v = value(&mut i, "--gpu-index")?;
                    a.gpu_index = match v.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err(format!("bad --gpu-index {v:?}")),
                    };
                }
                "--governor" => {
                    let v = value(&mut i, "--governor")?;
                    a.governor = match v.as_str() {
                        "on" => true,
                        "off" => false,
                        _ => return Err(format!("bad --governor {v:?}")),
                    };
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
        /// `--isolate`: a pane host process runs the program (`app::host`).
        Host {
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
                Session::Host { stdin, .. } => match stdin {
                    Some(s) => wire::ToHost::Input(bytes.to_vec()).write(s),
                    None => Ok(()),
                },
            }
        }

        fn resize(&mut self, rows: u16, cols: u16) {
            match self {
                Session::Console(p) => {
                    let _ = p.resize(rows, cols);
                }
                Session::Host { stdin: Some(s), .. } => {
                    let _ = wire::ToHost::Resize { rows, cols }.write(s);
                }
                _ => {}
            }
        }

        /// Ask a pane host for history lines (see [`Pane::history_for`]).
        fn ask_history(&mut self, first: u64, count: u32) -> io::Result<()> {
            match self {
                Session::Host { stdin: Some(s), .. } => {
                    wire::ToHost::History { first, count }.write(s)
                }
                _ => Err(io::ErrorKind::Unsupported.into()),
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
                Session::Host { child, stdin } => {
                    // Closing its input tells the host to take its program
                    // down; it gets a moment before it is killed.
                    drop(stdin.take());
                    let until = Instant::now() + Duration::from_secs(1);
                    while Instant::now() < until && matches!(child.try_wait(), Ok(None)) {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let _ = child.kill();
                }
            }
        }

        fn wait(&mut self) -> i32 {
            match self {
                Session::Console(p) => p.wait().unwrap_or(0),
                Session::Pipe { child, .. } | Session::Host { child, .. } => {
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
        /// A shell-integration mark, at its offset in the current generation.
        Mark(panelog::Mark),
    }

    /// How soon after output stops the logger flushes it.
    const LOG_FLUSH_AFTER: Duration = Duration::from_millis(50);
    /// How often an idle logger wakes to store signatures from the GPU.
    const LOG_IDLE_WAKE: Duration = Duration::from_secs(1);

    /// Chunks the logger may lag behind by (64 KiB each: 4 MiB). Past this the
    /// reader waits for the disk: memory stays bounded, output is never lost.
    const LOG_QUEUE: usize = 64;

    /// Checkpoint the screen once this much output has passed since the last.
    const CHECKPOINT_EVERY: u64 = 1024 * 1024;

    /// Start the logger thread for pane `id`: appends output and checkpoints
    /// to the pane's current generation under `pane_dir`, starting the next
    /// one when told, and flushing whenever it has nothing waiting so the log
    /// is readable up to the recent past.
    /// A finished search signature on its way back to a logger:
    /// (generation, block, signature).
    type SigReply = std::sync::mpsc::Sender<(u32, u64, Vec<u8>)>;
    type GpuIndexer = indexer::Indexer<SigReply>;

    /// A logger's search signatures when the GPU builds them: the blocks it
    /// has cut, and signatures back early, waiting for their turn.
    struct Signing {
        indexer: GpuIndexer,
        reply: SigReply,
        stager: indexer::BlockStager,
        ready: std::collections::BTreeMap<u64, Vec<u8>>,
    }

    impl Signing {
        /// Hand every block `bytes` completes to the GPU, or build it here
        /// when the GPU cannot take it now; then store what is in order.
        fn appended(&mut self, bytes: &[u8], generation: u32, w: &mut panelog::Writer) {
            for st in self.stager.push(bytes) {
                let local = match st.lead {
                    // A generation's first block: nothing before it to send.
                    None => Some(panelog::signature(None, &st.bytes)),
                    Some(lead) => {
                        let job = indexer::Job {
                            reply: self.reply.clone(),
                            generation,
                            block: st.block,
                            lead,
                            bytes: st.bytes,
                        };
                        self.indexer
                            .submit(job)
                            .err()
                            .map(|j| panelog::signature(Some(j.lead), &j.bytes))
                    }
                };
                if let Some(sig) = local {
                    self.indexer.counts.cpu.fetch_add(1, Ordering::Relaxed);
                    self.ready.insert(st.block, sig);
                }
            }
            self.store(w);
        }

        fn arrived(&mut self, block: u64, sig: Vec<u8>, w: &mut panelog::Writer) {
            self.ready.insert(block, sig);
            self.store(w);
        }

        fn store(&mut self, w: &mut panelog::Writer) {
            Self::store_into(&mut self.ready, w);
        }

        /// Store the signatures in `ready` that are next in `w`'s order.
        fn store_into(
            ready: &mut std::collections::BTreeMap<u64, Vec<u8>>,
            w: &mut panelog::Writer,
        ) {
            while let Some(sig) = ready.remove(&w.signature_count()) {
                if w.add_signature(w.signature_count(), &sig).is_err() {
                    break;
                }
            }
        }
    }

    /// A generation that has rolled over, kept open a little longer so the
    /// signatures still on the GPU land in its log.
    struct Retiring {
        generation: u32,
        writer: panelog::Writer,
        ready: std::collections::BTreeMap<u64, Vec<u8>>,
        until: Instant,
    }

    /// How long a rolled-over generation waits for its last signatures: the
    /// indexer gathers for up to 400 ms, then builds and returns them.
    const RETIRE_FOR: Duration = Duration::from_secs(2);
    const BLOCK_BYTES: u64 = panelog::SIG_BLOCK;

    fn start_logger(
        pane_dir: PathBuf,
        id: PaneId,
        gpu: Option<GpuIndexer>,
    ) -> io::Result<SyncSender<LogMsg>> {
        let cfg = panelog::Config {
            pane: u64::from(std::process::id()) << 16 | id as u64,
            ..panelog::Config::default()
        };
        let deferred = gpu.is_some();
        let open = move |n: u32| {
            let dir = pane_dir.join(logstore::gen_name(logstore::now_ms(), n));
            let mut w = panelog::Writer::create(&dir, cfg)?;
            if deferred {
                w.defer_signatures()?;
            }
            Ok::<_, io::Error>(w)
        };
        let mut generation = 0u32;
        let mut writer = open(generation)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<LogMsg>(LOG_QUEUE);
        let (sig_tx, sig_rx) = std::sync::mpsc::channel();
        let mut retiring: Option<Retiring> = None;
        let mut signing = gpu.map(|indexer| Signing {
            indexer,
            reply: sig_tx,
            stager: indexer::BlockStager::default(),
            ready: Default::default(),
        });
        std::thread::spawn(move || {
            let mut dirty = false;
            loop {
                // Flush 50 ms after output stops; with nothing to flush, wake
                // only to store signatures back from the GPU (design §4: an
                // idle terminal does not poll).
                let wait = if dirty || retiring.is_some() {
                    LOG_FLUSH_AFTER
                } else {
                    LOG_IDLE_WAKE
                };
                let msg = rx.recv_timeout(wait);
                // Signatures back from the GPU: for this generation, or for
                // the one just rolled over while it is still kept open. Any
                // later than that are rebuilt by panelog when its log opens.
                if let Some(s) = signing.as_mut() {
                    while let Ok((g, block, sig)) = sig_rx.try_recv() {
                        if g == generation {
                            s.arrived(block, sig, &mut writer);
                            dirty = true;
                        } else if let Some(r) = retiring.as_mut().filter(|r| r.generation == g) {
                            r.ready.insert(block, sig);
                            Signing::store_into(&mut r.ready, &mut r.writer);
                        }
                    }
                }
                // Let the old generation go once every whole block has its
                // signature, or its time is up.
                if retiring.as_ref().is_some_and(|r| {
                    r.writer.signature_count() >= r.writer.len() / BLOCK_BYTES
                        || Instant::now() >= r.until
                }) {
                    if let Some(mut r) = retiring.take() {
                        let _ = r.writer.flush();
                    }
                }
                match msg {
                    Ok(LogMsg::Data(bytes, t)) => {
                        if writer.append(&bytes, t).is_err() {
                            return;
                        }
                        if let Some(s) = signing.as_mut() {
                            s.appended(&bytes, generation, &mut writer);
                        }
                        dirty = true;
                    }
                    Ok(LogMsg::Checkpoint(blob)) => {
                        let _ = writer.checkpoint(&blob);
                        dirty = true;
                    }
                    Ok(LogMsg::Mark(mark)) => {
                        let _ = writer.mark(mark);
                        dirty = true;
                    }
                    Ok(LogMsg::Roll(blob)) => {
                        let _ = writer.flush();
                        let next = match open(generation + 1) {
                            Ok(w) => w,
                            Err(_) => return,
                        };
                        let old = std::mem::replace(&mut writer, next);
                        if let Some(s) = signing.as_mut() {
                            s.stager = indexer::BlockStager::default();
                            // Keep the old generation open for its signatures
                            // still on the GPU; the one before goes now.
                            if let Some(mut r) = retiring.take() {
                                let _ = r.writer.flush();
                            }
                            retiring = Some(Retiring {
                                generation,
                                writer: old,
                                ready: std::mem::take(&mut s.ready),
                                until: Instant::now() + RETIRE_FOR,
                            });
                        }
                        generation += 1;
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

    /// Hits for `needle` (ignoring ASCII case) in every generation of one
    /// pane's logs, at most `limit` per generation, with lines numbered as
    /// [`Deep`] numbers them.
    fn search_pane(id: PaneId, dir: &Path, needle: &str, limit: usize) -> Vec<find::Hit> {
        let mut hits = Vec::new();
        let mut start = 0u64;
        let opts = panelog::SearchOptions {
            ignore_case: true,
            limit,
        };
        for g in logstore::generations(dir) {
            // One retention is deleting cannot be opened: skipped, as Deep does.
            let Ok(r) = panelog::Reader::open(&g) else {
                continue;
            };
            let stamp = g
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(logstore::gen_stamp)
                .unwrap_or(0);
            for offset in r.search(needle.as_bytes(), &opts).unwrap_or_default() {
                let Ok(local) = r.line_of(offset) else {
                    continue;
                };
                let text = r
                    .line(local)
                    .map(|l| find::clean(&l, 240))
                    .unwrap_or_default();
                hits.push(find::Hit {
                    pane: id,
                    line: start + local,
                    text,
                    generation: stamp,
                    offset,
                });
            }
            start += r.line_count() - 1;
        }
        hits
    }

    /// The search bar: what is typed, what was last searched, its hits and
    /// which one is shown.
    #[derive(Default)]
    struct FindBar {
        query: String,
        searched: String,
        hits: Vec<find::Hit>,
        at: usize,
        running: Option<std::sync::mpsc::Receiver<(Vec<find::Hit>, Duration)>>,
        /// How long the last search took, start to hits.
        took: Option<Duration>,
    }

    /// Hits kept per generation of each pane.
    const FIND_LIMIT: usize = 500;

    /// Search bar colours.
    const BAR_BG: u32 = 0xFF26_4F78;
    const BAR_FG: u32 = 0xFFE8_E8E8;

    /// Leave the search: every pane back to its live view.
    fn close_find(panes: &mut [Pane]) {
        for p in panes.iter_mut() {
            if p.find_view.take().is_some() {
                p.deep = None;
                p.back = 0;
                p.shared.pending.store(true, Ordering::Release);
            }
        }
    }

    /// Show `hit`: its pane focused (unzoomed if another was zoomed), its log
    /// rebuilt there, any other pane's hit put away.
    fn show_hit(
        hit: &find::Hit,
        needle: &str,
        panes: &mut [Pane],
        zoom: &mut Option<PaneId>,
        focus: &mut PaneId,
    ) {
        if zoom.is_some() && *zoom != Some(hit.pane) {
            *zoom = None;
        }
        for p in panes.iter_mut() {
            if p.id == hit.pane {
                p.find_view = Some((hit.line, needle.to_string()));
                p.shared.pending.store(true, Ordering::Release);
                *focus = hit.pane;
            } else if p.find_view.take().is_some() {
                p.deep = None;
                p.back = 0;
                p.shared.pending.store(true, Ordering::Release);
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
        term: Mutex<Emu>,
        pending: AtomicBool,
        exited: AtomicBool,
        /// The commands in the output, from shell-integration marks. Taken
        /// while holding `term`.
        blocks: Mutex<blocks::Blocks>,
        /// In a pane host: marks waiting to go to the window.
        marks_out: Option<Mutex<Vec<(u8, u64, Option<i32>)>>>,
        /// An isolated pane's host crashed and is to be restarted.
        host_died: AtomicBool,
        /// An isolated pane waiting for history lines from its host.
        history_reply: Mutex<Option<std::sync::mpsc::Sender<(u64, Vec<Vec<ansi::Cell>>)>>>,
        /// Output since the last checkpoint sent to the log.
        since_checkpoint: AtomicU64,
        /// Output in the log's current generation.
        gen_bytes: AtomicU64,
        /// The emulator is fed as output arrives. False while the pane is
        /// hidden and shed: output then only reaches the log, and the
        /// emulator waits at `paused_at` (an offset in the current
        /// generation) until it catches up from there.
        parsing: AtomicBool,
        paused_at: AtomicU64,
        catching_up: AtomicBool,
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
    fn write_stats(
        path: &str,
        activity: &Activity,
        frames: &[u64],
        index: Option<&indexer::Counts>,
        search: Option<(Duration, usize)>,
    ) -> io::Result<()> {
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
        // Search signatures: how many the GPU built, how many the loggers did.
        let text = match index {
            Some(c) => format!(
                "{text}index_gpu_blocks {}\nindex_cpu_blocks {}\nindex_launches {}\n",
                c.gpu.load(Ordering::Relaxed),
                c.cpu.load(Ordering::Relaxed),
                c.launches.load(Ordering::Relaxed)
            ),
            None => text,
        };
        // The last search: how long it took and how many hits.
        let text = match search {
            Some((took, hits)) => format!(
                "{text}last_search_ms {:.1}\nlast_search_hits {hits}\n",
                took.as_secs_f64() * 1e3
            ),
            None => text,
        };
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
        /// Behind a zoomed pane: kept, not drawn.
        hidden: bool,
        /// Showing a search hit: its line in the pane's logs, and the needle.
        find_view: Option<(u64, String)>,
        /// When this pane's host was restarted, for the crash-loop guard.
        restarts: Vec<Instant>,
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

        /// For an isolated pane, the lines in `lines` that have left its
        /// screen, fetched from its host (which holds its history), waiting
        /// up to half a second. Empty for a local pane: its history is here.
        fn history_for(&self, lines: std::ops::Range<u64>) -> HashMap<u64, Vec<ansi::Cell>> {
            let scrolled = match &*self.shared.term.lock().unwrap() {
                Emu::Remote(m) => m.scrolled,
                Emu::Local(_) => return HashMap::new(),
            };
            let end = lines.end.min(scrolled);
            if lines.start >= end {
                return HashMap::new();
            }
            let count = (end - lines.start).min(100_000) as u32;
            let (tx, rx) = std::sync::mpsc::channel();
            *self.shared.history_reply.lock().unwrap() = Some(tx);
            if self
                .session
                .lock()
                .unwrap()
                .ask_history(lines.start, count)
                .is_err()
            {
                return HashMap::new();
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok((first, got)) => got
                    .into_iter()
                    .enumerate()
                    .filter(|(_, l)| !l.is_empty())
                    .map(|(i, l)| (first + i as u64, l))
                    .collect(),
                Err(_) => HashMap::new(),
            }
        }

        /// The characters of output line `n`, one per column (a wide
        /// character's second half repeats it), if the pane still holds it
        /// (or `fetched` has it, for an isolated pane).
        fn line_chars(
            term: &Emu,
            n: u64,
            fetched: &HashMap<u64, Vec<ansi::Cell>>,
        ) -> Option<Vec<(char, bool)>> {
            let history = term.scrollback();
            let screen = term.screen();
            let scrolled = term.scrolled_lines();
            let mut row = Vec::new();
            let held = if let Some(cells) = fetched.get(&n) {
                row.extend_from_slice(cells);
                true
            } else if n >= scrolled {
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
            let (start, end) = sel.ordered();
            let fetched = self.history_for(start.line..end.line + 1);
            let term = self.shared.term.lock().unwrap();
            select::text(start, end, term.screen().cols(), |n, from, to| {
                let chars = Self::line_chars(&term, n, &fetched)?;
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
            let chars: Vec<char> = Self::line_chars(&term, at.line, &HashMap::new())?
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
            !self.hidden && self.rect.rows > 0 && self.rect.cols > 0
        }

        /// Stop feeding the emulator; output goes only to the log.
        fn pause(&self) {
            let _term = self.shared.term.lock().unwrap();
            let at = self.shared.gen_bytes.load(Ordering::Acquire);
            self.shared.paused_at.store(at, Ordering::Release);
            self.shared.parsing.store(false, Ordering::Release);
        }

        /// Catch the emulator up from the log, on its own thread, and feed
        /// it live again once it has.
        fn resume(&self, waker: Waker) {
            let (Some(log), Some(dir)) = (&self.log, &self.pane_dir) else {
                self.shared.parsing.store(true, Ordering::Release);
                return;
            };
            if self.shared.catching_up.swap(true, Ordering::AcqRel) {
                return;
            }
            let (shared, tx, dir) = (self.shared.clone(), log.tx.clone(), dir.clone());
            std::thread::spawn(move || {
                if let Err(e) = catch_up(&shared, &tx, &dir, &waker) {
                    // Live again regardless, with the gap said out loud.
                    let mut term = shared.term.lock().unwrap();
                    term.feed(format!("\r\n{RED}[fwterm: output while hidden could not be replayed: {e}]{RESET}\r\n").as_bytes());
                    shared.parsing.store(true, Ordering::Release);
                }
                shared.catching_up.store(false, Ordering::Release);
                shared.pending.store(true, Ordering::Release);
                waker.wake();
            });
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
            let scrolled = term.scrolled_lines();
            if term.in_alternate_screen() {
                self.back = 0; // full-screen programs: always live
            } else if self.back > 0 && self.deep.is_none() {
                // New lines pushed the text we are looking at further up.
                self.back += (scrolled - self.seen_scrolled) as usize;
            }
            self.seen_scrolled = scrolled;
            let held = history.history_len();
            let (rows, cols) = (term.screen().rows(), term.screen().cols());
            if let Some((line, needle)) = self.find_view.clone() {
                // A search hit: the log rebuilt a little past its line, the
                // match highlighted where it landed.
                if self.deep.as_ref().map_or(true, |d| d.total <= line) {
                    self.deep = self.pane_dir.as_deref().and_then(|f| Deep::open(f).ok());
                }
                if let Some(d) = self.deep.as_mut() {
                    let end = (line + 1 + rows as u64 / 2).min(d.total);
                    let _ = d.rebuild(end.max(1), rows, cols, pipe);
                    let s = d.term.screen();
                    // Where the hit should be: `end - 1 - line` lines above
                    // the last one written; wrapped lines move it up.
                    let bottom = s.cursor().row;
                    let expect = bottom.saturating_sub((end - 1 - line) as usize);
                    let highlight = (0..s.rows())
                        .filter_map(|row| {
                            let chars: Vec<char> =
                                (0..s.cols()).map(|c| s.cell(row, c).ch).collect();
                            find::match_in_row(&chars, &needle).map(|(a, b)| (row, a, b))
                        })
                        .min_by_key(|&(row, _, _)| row.abs_diff(expect))
                        .map(|(row, a, b)| Highlight {
                            start: (row, a),
                            end: (row, b),
                        });
                    frame.update_view_at(
                        r.top,
                        r.left,
                        s,
                        s,
                        0,
                        false,
                        highlight,
                        pen.palette,
                        pen.atlas,
                        pen.raster,
                    );
                }
            } else if self.back > held && self.pane_dir.is_some() && !term.in_alternate_screen() {
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
        wake: Wake,
        log: Option<LogLink>,
        place: Arc<Vec<u32>>,
    ) {
        std::thread::spawn(move || {
            if !place.is_empty() {
                let _ = resmon::cpu::place_current_thread(&place);
            }
            let mut buf = vec![0u8; 64 * 1024];
            let mut watcher = fwbench::terminal::Watcher::new();
            let mut scanner = ansi::MarkScanner::new();
            let mut found = Vec::new();
            loop {
                let n = match source(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                if let Some(session) = session.as_ref().and_then(Weak::upgrade) {
                    for reply in watcher.replies(&buf[..n]) {
                        // Where the cursor really is (the banner moved it),
                        // as of the bytes before this chunk.
                        let cpr;
                        let reply = if reply == fwbench::terminal::CPR_REPLY {
                            let c = shared.term.lock().unwrap().screen().cursor();
                            cpr = format!("\x1b[{};{}R", c.row + 1, c.col + 1);
                            cpr.as_bytes()
                        } else {
                            reply
                        };
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
                    // A shed pane's emulator waits; the log has the bytes.
                    let parse = log.is_none() || shared.parsing.load(Ordering::Acquire);
                    // Shell-integration marks: where in this chunk each falls.
                    let base = scanner.position();
                    found.clear();
                    scanner.feed(&buf[..n], &mut found);
                    let at_in_chunk =
                        |m: &ansi::ShellMark| (m.offset.saturating_sub(base) as usize).min(n);
                    if parse && !found.is_empty() {
                        // Feed up to each mark and note the line the cursor is
                        // on there: that is where the block boundary goes.
                        let mut blocks = shared.blocks.lock().unwrap();
                        let mut at = 0;
                        for m in &found {
                            let upto = at_in_chunk(m).max(at);
                            term.feed(&buf[at..upto]);
                            at = upto;
                            if !term.in_alternate_screen() {
                                let line = term.scrollback().scrolled_lines()
                                    + term.screen().cursor().row as u64;
                                blocks.mark(m.kind, line, m.exit_code);
                                if let Some(out) = &shared.marks_out {
                                    out.lock()
                                        .unwrap()
                                        .push((m.kind.letter(), line, m.exit_code));
                                }
                            }
                        }
                        term.feed(&buf[at..n]);
                        let history = term.scrollback();
                        blocks
                            .forget_before(history.scrolled_lines() - history.history_len() as u64);
                    } else if parse {
                        term.feed(&buf[..n]);
                    }
                    if let Some(log) = &log {
                        let chunk_at = shared.gen_bytes.load(Ordering::Acquire);
                        for m in &found {
                            let _ = log.tx.send(LogMsg::Mark(panelog::Mark {
                                offset: chunk_at + at_in_chunk(m) as u64,
                                kind: m.kind.letter(),
                                value: m.exit_code.map_or(-1, i64::from),
                                time_ns: now,
                            }));
                        }
                    }
                    // Checkpoint at a point the replay can resume from; roll
                    // over to a new generation at one.
                    if let Some(log) = &log {
                        let since = shared
                            .since_checkpoint
                            .fetch_add(n as u64, Ordering::AcqRel)
                            + n as u64;
                        let generation =
                            shared.gen_bytes.fetch_add(n as u64, Ordering::AcqRel) + n as u64;
                        // Checkpoints need the live screen; a generation does
                        // not roll over while it is paused either, so a shed
                        // pane catches up within one generation.
                        if parse && since >= CHECKPOINT_EVERY {
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
                    wake();
                }
            }
            if last {
                shared.exited.store(true, Ordering::Release);
                wake();
            }
        });
    }

    /// Tells whoever draws the pane that it changed: the window's waker, or
    /// a pane host's update loop.
    type Wake = Arc<dyn Fn() + Send + Sync>;

    /// How often a console pane checks whether its program has exited.
    const EXIT_POLL: Duration = Duration::from_millis(200);

    /// Mark the pane exited when its console program ends. The console host
    /// keeps the output pipe open after its last client exits (until the
    /// pseudoconsole is closed), so the end of output cannot tell. Ends when
    /// the pane is gone.
    fn watch_exit(session: Weak<Mutex<Session>>, shared: Arc<Shared>, wake: Wake) {
        std::thread::spawn(move || loop {
            std::thread::sleep(EXIT_POLL);
            let Some(session) = session.upgrade() else {
                return;
            };
            let running = match &mut *session.lock().unwrap() {
                Session::Console(p) => matches!(p.try_wait(), Ok(None)),
                Session::Pipe { .. } | Session::Host { .. } => false,
            };
            if !running {
                shared.exited.store(true, Ordering::Release);
                wake();
                return;
            }
        });
    }

    /// Starts panes: the program from the command line, in the chosen lane.
    struct Spawner<'a> {
        args: &'a Args,
        logs: &'a Logs,
        /// CPU sets for pane readers (empty: anywhere).
        place: Arc<Vec<u32>>,
        /// Builds the logs' search signatures on the GPU, if it can.
        gpu: Option<GpuIndexer>,
        waker: Waker,
        activity: Arc<Activity>,
    }

    /// Restarts a crashed host at most this often before its pane closes.
    const RESTARTS: usize = 3;
    const RESTART_WINDOW: Duration = Duration::from_secs(30);

    impl Spawner<'_> {
        /// Start a new host for an isolated pane whose host crashed: a fresh
        /// program, a new log generation in the same pane folder (so the
        /// earlier output is still there to scroll back to and search), and
        /// a red line saying what happened. False when the pane has crashed
        /// too often and should close.
        fn restart(&self, pane: &mut Pane) -> bool {
            let now = Instant::now();
            pane.restarts
                .retain(|t| now.duration_since(*t) < RESTART_WINDOW);
            if pane.restarts.len() >= RESTARTS {
                return false;
            }
            pane.restarts.push(now);
            let (rows, cols) = (pane.rect.rows.max(1), pane.rect.cols.max(1));
            let gen = self
                .logs
                .store
                .as_ref()
                .map_or(256 << 20, |st| st.gen_limit.load(Ordering::Acquire));
            let wake: Wake = {
                let w = self.waker.clone();
                Arc::new(move || w.wake())
            };
            *pane.shared.blocks.lock().unwrap() = blocks::Blocks::default();
            let note = format!(
                "{RED}[fwterm: this pane's host process stopped; started a new one. \
                 Earlier output is in the log: scroll back or search.]{RESET}\r\n"
            );
            match start_host(
                &self.activity,
                self.args,
                pane.id,
                rows,
                cols,
                &pane.shared,
                pane.pane_dir.as_deref(),
                gen,
                self.logs,
                wake,
                &note,
            ) {
                Ok(session) => {
                    pane.session = session;
                    pane.back = 0;
                    pane.deep = None;
                    pane.find_view = None;
                    pane.shared.pending.store(true, Ordering::Release);
                    true
                }
                Err(_) => false,
            }
        }

        fn spawn(&self, id: PaneId, rect: Rect) -> io::Result<Pane> {
            let args = self.args;
            let (rows, cols) = (rect.rows.max(1), rect.cols.max(1));
            let shared = Arc::new(Shared {
                term: Mutex::new(Emu::Local({
                    let mut t = Term::with_history(rows, cols, args.history);
                    if args.pipe {
                        // No tty turns LF into CR LF on a pipe: new line mode
                        // (LNM) makes the emulator do it, with no copy.
                        t.feed(b"\x1b[20h");
                    }
                    t.feed(self.logs.banner().as_bytes());
                    t
                })),
                pending: AtomicBool::new(true),
                exited: AtomicBool::new(false),
                blocks: Mutex::new(blocks::Blocks::default()),
                marks_out: None,
                history_reply: Mutex::new(None),
                host_died: AtomicBool::new(false),
                since_checkpoint: AtomicU64::new(0),
                gen_bytes: AtomicU64::new(0),
                parsing: AtomicBool::new(true),
                paused_at: AtomicU64::new(0),
                catching_up: AtomicBool::new(false),
                want: AtomicBool::new(false),
            });
            let (log, pane_dir) = match &self.logs.store {
                // The pane host logs; this window only reads the logs.
                Some(store) if args.isolate => (None, Some(store.session.pane_dir(id))),
                Some(store) => {
                    let dir = store.session.pane_dir(id);
                    match start_logger(dir.clone(), id, self.gpu.clone()) {
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
            let wake: Wake = {
                let w = self.waker.clone();
                Arc::new(move || w.wake())
            };
            if args.isolate {
                let gen = self
                    .logs
                    .store
                    .as_ref()
                    .map_or(256 << 20, |st| st.gen_limit.load(Ordering::Acquire));
                let session = start_host(
                    &self.activity,
                    args,
                    id,
                    rows,
                    cols,
                    &shared,
                    pane_dir.as_deref(),
                    gen,
                    &self.logs,
                    wake,
                    "",
                )?;
                return Ok(Pane {
                    id,
                    shared,
                    session,
                    log: None,
                    pane_dir,
                    rect,
                    back: 0,
                    seen_scrolled: 0,
                    deep: None,
                    hidden: false,
                    find_view: None,
                    restarts: Vec::new(),
                });
            }
            let session = start_session(
                args,
                rows,
                cols,
                &shared,
                &self.activity,
                wake,
                log.clone(),
                &self.place,
            )?;
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
                hidden: false,
                find_view: None,
                restarts: Vec::new(),
            })
        }
    }

    /// Start a pane host for pane `id` and a thread that applies what it
    /// sends to the pane's mirror.
    #[allow(clippy::too_many_arguments)]
    fn start_host(
        activity: &Arc<Activity>,
        args: &Args,
        id: PaneId,
        rows: usize,
        cols: usize,
        shared: &Arc<Shared>,
        pane_dir: Option<&Path>,
        gen: u64,
        logs: &Logs,
        wake: Wake,
        note: &str,
    ) -> io::Result<Arc<Mutex<Session>>> {
        use std::process::{Command, Stdio};
        *shared.term.lock().unwrap() = Emu::Remote(wire::Mirror::new(rows, cols));
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("--pane-host")
            .args(["--host-size", &format!("{rows}x{cols}")])
            .args(["--history", &args.history.to_string()])
            .args(["--host-id", &id.to_string()])
            .args(["--host-gen", &gen.to_string()]);
        if args.pipe {
            cmd.arg("--pipe");
        }
        if !args.shell_integration {
            cmd.args(["--shell-integration", "off"]);
        }
        if let Some(dll) = &args.conpty {
            cmd.args(["--conpty", dll]);
        }
        if let Some(dir) = pane_dir {
            cmd.arg("--host-log").arg(dir);
        }
        cmd.arg("--").args(&args.shell);
        cmd.env("FWTERM_BANNER", format!("{}{note}", logs.banner()));
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        let mut out = child.stdout.take().expect("piped");
        let stdin = child.stdin.take();
        let (shared, activity) = (shared.clone(), activity.clone());
        std::thread::spawn(move || {
            let mut out = io::BufReader::new(&mut out);
            loop {
                match wire::FromHost::read(&mut out) {
                    Ok(wire::FromHost::Update(u)) => {
                        if let Emu::Remote(m) = &mut *shared.term.lock().unwrap() {
                            m.apply(&u);
                        }
                        // For --stats: output flowed until now.
                        let now = activity.epoch.elapsed().as_nanos() as u64;
                        activity.first_output.fetch_min(now, Ordering::AcqRel);
                        activity.last_output.store(now, Ordering::Release);
                    }
                    Ok(wire::FromHost::Mark { kind, line, exit }) => {
                        let kind = match kind {
                            b'A' => ansi::MarkKind::PromptStart,
                            b'B' => ansi::MarkKind::CommandStart,
                            b'C' => ansi::MarkKind::OutputStart,
                            _ => ansi::MarkKind::CommandEnd,
                        };
                        shared.blocks.lock().unwrap().mark(kind, line, exit);
                    }
                    Ok(wire::FromHost::History { first, lines }) => {
                        if let Some(tx) = shared.history_reply.lock().unwrap().take() {
                            let _ = tx.send((first, lines));
                        }
                        continue;
                    }
                    Ok(wire::FromHost::Exited(_)) => {
                        // The program ended: the pane closes, as any pane does.
                        shared.exited.store(true, Ordering::Release);
                        wake();
                        return;
                    }
                    Err(_) => {
                        // The host ended without saying so: it crashed. The
                        // window restarts it (see `Spawner::restart`).
                        shared.host_died.store(true, Ordering::Release);
                        wake();
                        return;
                    }
                }
                shared.pending.store(true, Ordering::Release);
                wake();
            }
        });
        Ok(Arc::new(Mutex::new(Session::Host { child, stdin })))
    }

    /// Start the pane's program in its lane, with reader threads feeding
    /// `shared`: the part of a pane that a pane host runs too.
    #[allow(clippy::too_many_arguments)]
    fn start_session(
        args: &Args,
        rows: usize,
        cols: usize,
        shared: &Arc<Shared>,
        activity: &Arc<Activity>,
        wake: Wake,
        log: Option<LogLink>,
        place: &Arc<Vec<u32>>,
    ) -> io::Result<Arc<Mutex<Session>>> {
        let shell_args: Vec<&str> = args.shell[1..].iter().map(String::as_str).collect();
        let pump = |source, session, last| {
            pump(
                source,
                session,
                last,
                shared.clone(),
                activity.clone(),
                wake.clone(),
                log.clone(),
                place.clone(),
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
            let env = if args.shell_integration {
                shell_integration(&args.shell[0])
            } else {
                Vec::new()
            };
            let pty = pty::Pty::spawn_with_env(
                &args.shell[0],
                &shell_args,
                rows as u16,
                cols as u16,
                &env,
            )?;
            let mut reader = pty.reader()?;
            let session = Arc::new(Mutex::new(Session::Console(pty)));
            pump(
                Box::new(move |b| reader.read(b)),
                Some(Arc::downgrade(&session)),
                true,
            );
            watch_exit(Arc::downgrade(&session), shared.clone(), wake.clone());
            session
        };
        Ok(session)
    }

    /// Environment that makes `shell` mark its prompts and commands (OSC
    /// 133). cmd.exe takes it in PROMPT: end of the last command (`D`),
    /// prompt start (`A`), the prompt itself (the user's own PROMPT if set),
    /// command start (`B`). cmd cannot mark output start or send an exit code.
    fn shell_integration(shell: &str) -> Vec<(String, String)> {
        let name = std::path::Path::new(shell)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name != "cmd" {
            return Vec::new();
        }
        let own = std::env::var("PROMPT").unwrap_or_else(|_| "$P$G".into());
        vec![(
            "PROMPT".into(),
            format!(r"$E]133;D$E\$E]133;A$E\{own}$E]133;B$E\"),
        )]
    }

    /// How often the governor reads the CPU counters.
    const PRESSURE_EVERY: Duration = Duration::from_millis(500);

    /// Watch how busy other programs keep the machine; the flag is true while
    /// it is [`Level::Busy`].
    fn start_pressure_watch() -> Arc<AtomicBool> {
        let busy = Arc::new(AtomicBool::new(false));
        let flag = busy.clone();
        std::thread::spawn(move || {
            let Ok(mut before) = resmon::cpu::times() else {
                return;
            };
            let mut level = Level::Calm;
            loop {
                std::thread::sleep(PRESSURE_EVERY);
                let Ok(now) = resmon::cpu::times() else {
                    return;
                };
                level = governor::next(level, resmon::cpu::load(&before, &now));
                flag.store(level == Level::Busy, Ordering::Release);
                before = now;
            }
        });
        busy
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
        if args.pane_host {
            return match host::run(&args) {
                Ok(code) => code,
                Err(_) => 1,
            };
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
        for p in panes.iter_mut() {
            match placement.panes.iter().find(|(id, _)| *id == p.id) {
                Some(&(_, rect)) => {
                    p.hidden = false;
                    p.place(rect);
                }
                // Its emulator and program keep their size while hidden.
                None => p.hidden = true,
            }
        }
    }

    /// Replay chunk size while catching up, and the tail small enough to
    /// finish under the pane lock.
    const CATCH_UP_CHUNK: usize = 1024 * 1024;
    const CATCH_UP_FINISH: u64 = 1024 * 1024;

    /// Have the logger write out everything sent so far (waits).
    fn flush_logger(tx: &SyncSender<LogMsg>) -> io::Result<()> {
        let (done, wait) = std::sync::mpsc::channel();
        tx.send(LogMsg::Flush(done))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the logger stopped"))?;
        wait.recv_timeout(Duration::from_secs(30))
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the log did not flush"))
    }

    /// Feed a paused emulator the log from `paused_at` on: in chunks with the
    /// pane lock taken per chunk while output keeps arriving, then the last
    /// small tail under the lock, where it turns live again.
    fn catch_up(
        shared: &Shared,
        tx: &SyncSender<LogMsg>,
        pane_dir: &Path,
        waker: &Waker,
    ) -> io::Result<()> {
        let current = || {
            logstore::generations(pane_dir)
                .pop()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no log generation"))
        };
        let mut buf = vec![0u8; CATCH_UP_CHUNK];
        loop {
            flush_logger(tx)?;
            let reader = panelog::Reader::open(&current()?)?;
            let mut at = shared.paused_at.load(Ordering::Acquire);
            let end = reader.len();
            while at < end {
                let want = ((end - at) as usize).min(buf.len());
                let n = reader.read_at(at, &mut buf[..want])?;
                if n == 0 {
                    break;
                }
                shared.term.lock().unwrap().feed(&buf[..n]);
                at += n as u64;
                shared.paused_at.store(at, Ordering::Release);
                shared.pending.store(true, Ordering::Release);
                waker.wake();
            }
            let mut term = shared.term.lock().unwrap();
            let logged = shared.gen_bytes.load(Ordering::Acquire);
            if logged.saturating_sub(at) > CATCH_UP_FINISH {
                continue; // more arrived meanwhile: another round unlocked
            }
            // Readers wait on the lock now; what they sent is all there is.
            flush_logger(tx)?;
            let reader = panelog::Reader::open(&current()?)?;
            let end = reader.len();
            while at < end {
                let want = ((end - at) as usize).min(buf.len());
                let n = reader.read_at(at, &mut buf[..want])?;
                if n == 0 {
                    break;
                }
                term.feed(&buf[..n]);
                at += n as u64;
            }
            shared.paused_at.store(at, Ordering::Release);
            shared.parsing.store(true, Ordering::Release);
            if at < logged {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{} bytes never reached the log", logged - at),
                ));
            }
            return Ok(());
        }
    }

    /// Console lane host (design §11 row 6), for the window's panes or a pane
    /// host's: set once, before the first console starts.
    fn choose_console_host(args: &Args) -> io::Result<()> {
        // An explicit --conpty, else a
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
        // The banner is on screen before the console starts: the console
        // must start below it, at our cursor, or its output lands offset.
        if !args.pipe {
            pty::inherit_cursor(true);
        }
        Ok(())
    }

    fn start(args: &Args) -> io::Result<i32> {
        choose_console_host(args)?;
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
        let cpus = resmon::cpu::topology().unwrap_or_default();
        let place = Arc::new(governor::placement(&cpus, args.place));
        if !place.is_empty() {
            let _ = resmon::cpu::place_current_thread(&place);
        }
        let busy = if args.governor {
            start_pressure_watch()
        } else {
            Arc::new(AtomicBool::new(false))
        };
        let gpu = (args.gpu_index && logs.store.is_some())
            .then(|| {
                indexer::Indexer::start(|r: &SigReply, g, b, sig| {
                    let _ = r.send((g, b, sig));
                })
            })
            .flatten();
        let index_counts = gpu.as_ref().map(|g| g.counts.clone());
        let spawner = Spawner {
            args,
            logs: &logs,
            place: place.clone(),
            gpu,
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
        let waker = surface.window().waker();
        let mut next_id = args.panes;
        let mut focus: PaneId = 0;
        let mut zoom: Option<PaneId> = (args.zoom && args.panes > 1).then_some(0);
        let mut find_bar: Option<FindBar> = None;
        // Every pane starts at its real size; a zoom then hides the rest.
        if zoom.is_some() {
            placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
            place_all(&mut panes, &placement);
        }

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
        let mut last_input = Instant::now();
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
        // The last search's time and hits, for --stats.
        let last_search: Mutex<Option<(Duration, usize)>> = Mutex::new(None);
        let finish = |frames: &[u64]| {
            if let Some(path) = &args.stats {
                let search = *last_search.lock().unwrap();
                if let Err(e) =
                    write_stats(path, &activity, frames, index_counts.as_deref(), search)
                {
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
            let level = if busy.load(Ordering::Acquire) {
                Level::Busy
            } else {
                Level::Calm
            };
            let interval = governor::frame_interval(min_interval, level, last_input.elapsed());
            let timeout = match last_draw {
                Some(t) if due => Some(interval.saturating_sub(t.elapsed())),
                _ => None,
            };
            events.clear();
            surface.window().wait(timeout, &mut events);
            if events
                .iter()
                .any(|e| !matches!(e, Event::Woken | Event::Paint))
            {
                last_input = Instant::now();
            }
            input.clear();
            // Panes to take out after the events: (pane, kill it).
            let mut closing: Vec<(PaneId, bool)> = Vec::new();
            for e in &events {
                let Some(fi) = panes.iter().position(|p| p.id == focus) else {
                    break;
                };
                // The search bar takes what is typed while it is open; keys
                // other than its own toggle do not reach the program.
                if let Some(f) = find_bar.as_mut() {
                    match *e {
                        Event::Char(_) if drop_char => {
                            drop_char = false;
                            continue;
                        }
                        Event::Char('\u{1b}') => {
                            find_bar = None;
                            close_find(&mut panes);
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char('\r') => {
                            if f.query.is_empty() {
                            } else if f.query != f.searched {
                                let jobs: Vec<(PaneId, PathBuf)> = panes
                                    .iter()
                                    .filter_map(|p| Some((p.id, p.pane_dir.clone()?)))
                                    .collect();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let (needle, w) = (f.query.clone(), waker.clone());
                                std::thread::spawn(move || {
                                    let started = Instant::now();
                                    // Every pane at once: each opens and
                                    // searches its own generations.
                                    let mut all: Vec<find::Hit> = std::thread::scope(|sc| {
                                        let handles: Vec<_> = jobs
                                            .iter()
                                            .map(|(id, dir)| {
                                                let needle = &needle;
                                                sc.spawn(move || {
                                                    search_pane(*id, dir, needle, FIND_LIMIT)
                                                })
                                            })
                                            .collect();
                                        handles
                                            .into_iter()
                                            .flat_map(|h| h.join().unwrap_or_default())
                                            .collect()
                                    });
                                    find::order(&mut all);
                                    let _ = tx.send((all, started.elapsed()));
                                    w.wake();
                                });
                                f.running = Some(rx);
                                f.searched = f.query.clone();
                                f.hits.clear();
                                f.at = 0;
                            } else if !f.hits.is_empty() {
                                let n = f.hits.len();
                                f.at = if fwrender::win::shift_down() {
                                    (f.at + n - 1) % n
                                } else {
                                    (f.at + 1) % n
                                };
                                let hit = f.hits[f.at].clone();
                                show_hit(&hit, &f.searched, &mut panes, &mut zoom, &mut focus);
                            }
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char('\u{8}') => {
                            f.query.pop();
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char(c) if !c.is_control() => {
                            f.query.push(c);
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char(_) => continue,
                        Event::Key { vk, down: true }
                            if !(vk == 0x46
                                && fwrender::win::ctrl_down()
                                && fwrender::win::shift_down()) =>
                        {
                            continue
                        }
                        _ => {}
                    }
                }
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
                        // Ctrl+Shift+F: search every pane's log, or stop.
                        if vk == 0x46 && ctrl && shift {
                            if find_bar.take().is_some() {
                                close_find(&mut panes);
                            } else {
                                find_bar = Some(FindBar::default());
                            }
                            layout_dirty = true;
                            drop_char = true;
                            continue;
                        }
                        // Ctrl+Shift+O: select the output of the command at
                        // the top of the view (at the live view, the latest
                        // with output), from shell-integration marks.
                        if vk == 0x4F && ctrl && shift {
                            let p = &panes[fi];
                            let top = p.seen_scrolled.saturating_sub(p.back as u64);
                            let at = if p.back == 0 { u64::MAX } else { top };
                            let term = p.shared.term.lock().unwrap();
                            let cols = term.screen().cols();
                            let mut out = p.shared.blocks.lock().unwrap().output_at(at);
                            // Not the blank lines a shell leaves before its
                            // next prompt (cmd prints one after every command).
                            if let Some(lines) = out.as_mut() {
                                let blank = |n: u64| {
                                    Pane::line_chars(&term, n, &HashMap::new())
                                        .is_some_and(|l| l.iter().all(|(c, _)| *c == ' '))
                                };
                                while lines.end > lines.start && blank(lines.end - 1) {
                                    lines.end -= 1;
                                }
                            }
                            let out = out.filter(|l| !l.is_empty());
                            drop(term);
                            if let Some(lines) = out {
                                let sel = Selection {
                                    anchor: Point {
                                        line: lines.start,
                                        col: 0,
                                    },
                                    head: Point {
                                        line: lines.end - 1,
                                        col: cols.saturating_sub(1),
                                    },
                                };
                                let p = &mut panes[fi];
                                // Bring its first line into view if it is above.
                                if lines.start < top {
                                    p.back = (p.seen_scrolled - lines.start) as usize;
                                }
                                p.shared.pending.store(true, Ordering::Release);
                                selection = Some((focus, sel));
                            }
                            drop_char = true;
                            continue;
                        }
                        // Ctrl+Shift+Z: zoom the pane, or unzoom.
                        if vk == 0x5A && ctrl && shift {
                            zoom = match zoom {
                                Some(_) => None,
                                None if panes.len() > 1 => Some(focus),
                                None => None,
                            };
                            clear(&mut selection, &panes);
                            layout_dirty = true;
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
                            // Ctrl+Up / Ctrl+Down: the previous / next prompt
                            // at the top of the view.
                            0x26 | 0x28 if ctrl && !shift => {
                                let top = pane.seen_scrolled.saturating_sub(pane.back as u64);
                                let blocks = pane.shared.blocks.lock().unwrap();
                                let to = if vk == 0x26 {
                                    blocks.prev_prompt(top)
                                } else {
                                    blocks.next_prompt(top)
                                };
                                drop(blocks);
                                if let Some(line) = to {
                                    pane.back = pane.seen_scrolled.saturating_sub(line) as usize;
                                }
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
                        // Splitting or moving the focus shows every pane again.
                        if (dir.is_some() || toward.is_some()) && zoom.take().is_some() {
                            placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
                            place_all(&mut panes, &placement);
                            layout_dirty = true;
                        }
                        let r = panes.iter().find(|p| p.id == focus).map_or(r, |p| p.rect);
                        if let Some(dir) = dir {
                            // Both halves need a cell, with a divider between.
                            let room = match dir {
                                Dir::Across => r.cols,
                                Dir::Down => r.rows,
                            };
                            let id = next_id;
                            if room >= 3 && layout.split(focus, dir, id) {
                                next_id += 1;
                                placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
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
                                        placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
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
                        // Scrolling leaves a search hit for the live view.
                        if panes[fi].find_view.take().is_some() {
                            panes[fi].deep = None;
                            panes[fi].back = 0;
                        }
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
                        placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
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
            // A crashed pane host is restarted; one that crashes too often
            // closes its pane.
            for p in panes.iter_mut() {
                if p.shared.host_died.swap(false, Ordering::AcqRel) {
                    if spawner.restart(p) {
                        layout_dirty = true;
                    } else {
                        p.shared.exited.store(true, Ordering::Release);
                    }
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
                if zoom == Some(id) {
                    zoom = None;
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
            // Search results, when the search thread has them.
            if let Some(f) = find_bar.as_mut() {
                if let Some((hits, took)) = f.running.as_ref().and_then(|rx| rx.try_recv().ok()) {
                    f.running = None;
                    f.took = Some(took);
                    *last_search.lock().unwrap() = Some((took, hits.len()));
                    f.hits = hits;
                    f.at = 0;
                    if let Some(hit) = f.hits.first().cloned() {
                        show_hit(&hit, &f.searched, &mut panes, &mut zoom, &mut focus);
                    }
                    layout_dirty = true;
                }
            }
            if layout_dirty {
                placement = layout.place_zoomed(zoom, grid_rows, grid_cols);
                place_all(&mut panes, &placement);
            }
            // Hidden panes stop parsing when the governor says so, and catch
            // up from their logs when shown or when it no longer does.
            for p in &panes {
                let keep = governor::keep_parsing(p.hidden, p.log.is_some(), args.hidden, level);
                let parsing = p.shared.parsing.load(Ordering::Acquire);
                if !keep && parsing && !p.shared.catching_up.load(Ordering::Acquire) {
                    p.pause();
                } else if keep && !parsing {
                    p.resume(waker.clone());
                }
            }
            if last_draw.map_or(false, |t| t.elapsed() < interval) {
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
            // The search bar, over the window's bottom row.
            if let Some(f) = &find_bar {
                let status = match (&f.running, f.hits.len()) {
                    (Some(_), _) => "searching every pane's log...".to_string(),
                    (None, 0) if !f.searched.is_empty() && f.searched == f.query => {
                        let ms = f.took.map_or(0.0, |t| t.as_secs_f64() * 1e3);
                        format!("no matches ({ms:.0} ms)")
                    }
                    (None, 0) => "Enter searches every pane's log, Esc closes".into(),
                    (None, n) if f.searched == f.query => {
                        let h = &f.hits[f.at];
                        let ms = f.took.map_or(0.0, |t| t.as_secs_f64() * 1e3);
                        format!(
                            "{}/{} ({ms:.0} ms) in pane {}: {}",
                            f.at + 1,
                            n,
                            h.pane + 1,
                            h.text
                        )
                    }
                    (None, _) => "Enter searches again".into(),
                };
                let text: Vec<char> = format!(" find: {}_   {status}", f.query).chars().collect();
                let row = grid_rows.saturating_sub(1);
                for col in 0..grid_cols {
                    let ch = text.get(col).copied().unwrap_or(' ');
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
                            fg: BAR_FG,
                            bg: BAR_BG,
                            flags: 0,
                        },
                    );
                }
                drew = true;
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
        fn search_hits_are_numbered_as_the_log_view_numbers_lines() {
            let pane = std::env::temp_dir().join(format!("fwterm-find-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&pane);
            let cfg = panelog::Config::default();
            let mut term = Term::new(4, 20);
            let first = b"L0\r\nL1 needle A\r\nL2\r\n".to_vec();
            {
                let mut w =
                    panelog::Writer::create(&pane.join(logstore::gen_name(1, 0)), cfg).unwrap();
                w.append(&first, 0).unwrap();
                term.feed(&first);
            }
            {
                let mut w =
                    panelog::Writer::create(&pane.join(logstore::gen_name(2, 1)), cfg).unwrap();
                w.checkpoint(&term.checkpoint().unwrap()).unwrap();
                w.append(b"L3\r\nL4 NEEDLE B\r\n", 0).unwrap();
            }
            let mut hits = search_pane(7, &pane, "needle", 100);
            find::order(&mut hits);
            let got: Vec<(usize, u64, &str)> = hits
                .iter()
                .map(|h| (h.pane, h.line, h.text.as_str()))
                .collect();
            // Newest first; lines counted across generations as Deep counts.
            assert_eq!(got, vec![(7, 4, "L4 NEEDLE B"), (7, 1, "L1 needle A")]);
            let mut d = Deep::open(&pane).unwrap();
            for h in &hits {
                d.rebuild(h.line + 1, 4, 20, false).unwrap();
                let s = d.term.screen();
                let on_screen = (0..s.rows()).any(|r| row(&d.term, r) == h.text);
                assert!(on_screen, "line {} shows {:?}", h.line, h.text);
            }
            let _ = std::fs::remove_dir_all(&pane);
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
