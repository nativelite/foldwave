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
mod agents;
#[cfg_attr(not(windows), allow(dead_code))]
mod altscreen;
#[cfg_attr(not(windows), allow(dead_code))]
mod blocks;
#[cfg_attr(not(windows), allow(dead_code))]
mod emu;
#[cfg_attr(not(windows), allow(dead_code))]
mod find;
#[cfg_attr(not(windows), allow(dead_code))]
mod find_re;
#[cfg_attr(not(windows), allow(dead_code))]
mod first_run;
#[cfg_attr(not(windows), allow(dead_code))]
mod fold;
#[cfg_attr(not(windows), allow(dead_code))]
mod governor;
#[cfg_attr(not(windows), allow(dead_code))]
mod help;
#[cfg_attr(not(windows), allow(dead_code))]
mod hostlines;
#[cfg_attr(not(windows), allow(dead_code))]
mod icon;
#[cfg_attr(not(windows), allow(dead_code))]
mod indexer;
#[cfg_attr(not(windows), allow(dead_code))]
mod install;
#[cfg_attr(not(windows), allow(dead_code))]
mod layout;
#[cfg_attr(not(windows), allow(dead_code))]
mod logstore;
#[cfg_attr(not(windows), allow(dead_code))]
mod mouse;
#[cfg_attr(not(windows), allow(dead_code))]
mod ribbon;
#[cfg_attr(not(windows), allow(dead_code))]
mod select;
#[cfg_attr(not(windows), allow(dead_code))]
mod settings_ui;
#[cfg_attr(not(windows), allow(dead_code))]
mod strip;
#[cfg_attr(not(windows), allow(dead_code))]
mod theme;
#[cfg_attr(not(windows), allow(dead_code))]
mod wire;
// Built into the executable by build.rs; here for its tests.
#[cfg(test)]
mod winres;

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
    use crate::agents;
    use crate::altscreen;
    use crate::blocks;
    use crate::emu::Emu;
    use crate::fold;
    use crate::help;
    use crate::hostlines;
    use crate::icon;
    use crate::wire;

    mod host;
    use crate::find;
    use crate::first_run;
    use crate::governor::{self, Hidden, Level, Place};
    use crate::indexer;
    use crate::install;
    use crate::layout::{neighbor, pane_at, Dir, Layout, PaneId, Placement, Rect, Toward};
    use crate::logstore;
    use crate::mouse;
    use crate::ribbon;
    use crate::select::{self, Point, Selection};
    use crate::settings_ui;
    use crate::strip;
    use crate::theme::Theme;
    use fwrender::win::{display_refresh_hz, CpuPresenter, Event, Waker, Window};
    use fwrender::{
        Atlas, CellInstance, CpuRenderer, FontRasterizer, Frame, GlyphKey, GlyphPart, Highlight,
        Metrics, Palette, Rasterizer, Sharpening,
    };
    use std::collections::{HashMap, HashSet};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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

usage: fwterm [options] [-- SHELL [ARGS...]]   (default shell: shell = in the
       settings file, else cmd.exe)

options:
  --backend B   vulkan (default; falls back to cpu if Vulkan cannot start) or cpu
  --conpty DLL  host the shell with this conpty.dll (e.g. one that starts an
                OpenConsole.exe beside it). Without it, a conpty.dll beside
                fwterm.exe is used if present, else the built-in console host
  --pipe        run the program with no console: output over plain pipes
                (native speed), keys to its stdin
  --font NAME   font family (default JetBrains Mono, built into fwterm), or
                any installed one; an unknown name lists the monospaced
                families found
  (instruments = off in the settings file hides the strip of readings
  under the title bar; ribbon = off hides the activity ribbon at each
  pane's right edge, which maps the lines the pane holds onto its rows:
  where commands started, in red where one failed, and where the view is)
  --theme T     colours: mirai (default, foldwave's own) or classic; any
                colour can be changed in the settings file with
                color.<name> = #rrggbb (bg, fg, selection, dim, accent,
                rule, bar, bar_text, error, surface, title_bar,
                ansi0..ansi15)
  --licenses    show the licenses of what fwterm ships (JetBrains Mono,
                Microsoft's ConPTY) and exit
  --install     install foldwave for this user: fwterm.exe and ConPTY into
                %LOCALAPPDATA%\\Programs\\foldwave, a Start-menu shortcut and
                an Apps & features entry; then start it. Started as
                foldwave-setup-<version>.exe it does this by itself
  --uninstall   remove that install (settings and logs are kept)
  --quiet       with --install / --uninstall: no window, and --install
                starts nothing
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
  --first-run   show the first-run steps (shell, logs, look) before the
                panes start. They show by themselves while there is no
                settings file, unless --pipe or --stats is given
  --place P     where pane readers and the UI thread run: off (default: where
                the OS puts them) or fast (the fast cores of a hybrid CPU,
                CPU 0 left out; measured no better on an i5-12600K)
  --zoom        start with the first pane zoomed (Ctrl+Shift+Z toggles)
  --hidden H    when a pane hidden behind a zoomed one stops parsing and only
                logs, catching up from its log when shown again: always
                (default), busy (only while other programs keep the CPU
                nearly full), or never. Without a log it keeps parsing
  --no-isolate  run every pane's program, parser and log in this process.
                By default each pane has a pane host process of its own
                (design 8.8a): a crash there restarts that pane in ~0.1 s
                and leaves the window and the other panes running. With
                --no-log panes are not isolated: they scroll back through
                their log
  --shell-integration S  on (default): cmd.exe marks its prompts and commands
                (OSC 133, through PROMPT), so Ctrl+Up/Down and Ctrl+Shift+O
                work; other shells mark them themselves if set up to; off
  --gpu-index G on (default): build the logs' search signatures on the GPU
                (NVIDIA, through its driver), off the loggers' threads; off:
                on each pane's logger thread. Without CUDA it is off. It
                runs only for panes in this process (--no-isolate) or with
                --host-signs gpu: pane hosts sign their own by default
  --host-signs S  who builds a pane host's search signatures: self (default:
                its own logger thread) or gpu (the window's GPU indexer,
                which reads each block back from the log; measured about
                9 % more CPU in total)
  --governor G  on (default): while other programs keep the CPU nearly
                full, draw at most 30 frames a second except just after
                input; off: always draw at the display's rate
  --stats PATH  on exit, write frame statistics for the time output flowed:
                frames, frames per second, gaps between frames

keys:
";

    /// The usage text: the options, then the keys from `help::KEYS`, the
    /// list the help sheet (F1) shows.
    fn usage() -> String {
        format!("{USAGE}{}", help::usage_keys())
    }

    #[derive(Clone)]
    struct Args {
        backend: String,
        conpty: Option<String>,
        font: Option<String>,
        /// `--theme`; else the settings file's, else Mirai.
        theme: Option<String>,
        /// `--px`; else the settings file's, else 16.
        px: Option<f32>,
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
        /// Show the first-run steps even with a settings file.
        first_run: bool,
        place: Place,
        governor: bool,
        gpu_index: bool,
        /// The window's GPU indexer signs pane hosts' logs too; otherwise
        /// each host signs its own (cheaper, design §8.8a).
        host_signs_gpu: bool,
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
        /// The window builds this pane host's search signatures (GPU).
        host_window_signs: bool,
        hidden: Hidden,
        zoom: bool,
        /// Show the licenses of what fwterm ships, then exit.
        licenses: bool,
        install: bool,
        uninstall: bool,
        quiet: bool,
        panes: usize,
        shell: Vec<String>,
        /// The program came after `--`: the settings file's `shell =` is
        /// not used.
        shell_set: bool,
    }

    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut a = Args {
            backend: "vulkan".into(),
            conpty: None,
            font: None,
            theme: None,
            px: None,
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
            first_run: false,
            place: Place::Off,
            governor: true,
            gpu_index: true,
            host_signs_gpu: false,
            shell_integration: true,
            pane_host: false,
            isolate: true,
            host_size: (24, 80),
            host_log: None,
            host_gen: 256 << 20,
            host_id: 0,
            host_window_signs: false,
            hidden: Hidden::Always,
            zoom: false,
            licenses: false,
            install: false,
            uninstall: false,
            quiet: false,
            panes: 1,
            shell: vec!["cmd.exe".into()],
            shell_set: false,
        };
        let mut i = 0;
        let value = |i: &mut usize, flag: &str| -> Result<String, String> {
            let v = argv.get(*i + 1).ok_or(format!("{flag} needs a value"))?;
            *i += 2;
            Ok(v.clone())
        };
        while i < argv.len() {
            match argv[i].as_str() {
                "-h" | "--help" => return Err(usage()),
                "--backend" => {
                    a.backend = value(&mut i, "--backend")?;
                    if a.backend != "vulkan" && a.backend != "cpu" {
                        return Err(format!("bad --backend {:?}", a.backend));
                    }
                }
                "--conpty" => a.conpty = Some(value(&mut i, "--conpty")?),
                "--font" => a.font = Some(value(&mut i, "--font")?),
                "--theme" => {
                    let v = value(&mut i, "--theme")?;
                    if Theme::named(&v).is_none() {
                        return Err(format!(
                            "bad --theme {v:?} (one of {})",
                            crate::theme::NAMES.join(", ")
                        ));
                    }
                    a.theme = Some(v);
                }
                "--px" => {
                    let v = value(&mut i, "--px")?;
                    a.px = Some(
                        v.parse()
                            .ok()
                            .filter(|p: &f32| *p >= 4.0)
                            .ok_or(format!("bad --px {v:?}"))?,
                    );
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
                "--licenses" => {
                    a.licenses = true;
                    i += 1;
                }
                "--install" => {
                    a.install = true;
                    i += 1;
                }
                "--uninstall" => {
                    a.uninstall = true;
                    i += 1;
                }
                "--quiet" => {
                    a.quiet = true;
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
                "--no-isolate" => {
                    a.isolate = false;
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
                "--host-window-signs" => {
                    a.host_window_signs = true;
                    i += 1;
                }
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
                "--host-signs" => {
                    let v = value(&mut i, "--host-signs")?;
                    a.host_signs_gpu = match v.as_str() {
                        "self" => false,
                        "gpu" => true,
                        _ => return Err(format!("bad --host-signs {v:?}")),
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
                "--first-run" => {
                    a.first_run = true;
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
                        a.shell_set = true;
                    }
                    break;
                }
                other => return Err(format!("unknown argument {other:?}\n\n{}", usage())),
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

    /// How many times the GPU may fail within [`GPU_FAULT_SPAN`] and still get
    /// a new Vulkan device; past that the CPU draws for good.
    const GPU_REBUILDS: usize = 3;
    const GPU_FAULT_SPAN: Duration = Duration::from_secs(60);

    /// When drawing on the GPU last failed (design §4, fault isolation: the
    /// GPU device fails alone).
    #[derive(Default)]
    struct GpuFaults {
        at: Vec<Instant>,
    }

    impl GpuFaults {
        /// A failure at `now`: whether to try Vulkan again.
        fn retry(&mut self, now: Instant) -> bool {
            self.at.retain(|t| now.duration_since(*t) < GPU_FAULT_SPAN);
            self.at.push(now);
            self.at.len() <= GPU_REBUILDS
        }
    }

    /// `e` came from drawing or resizing `surface`: on the GPU, recover (see
    /// [`Surface::recover`]); a failure on the CPU is fatal.
    fn after_draw_error(
        surface: Surface,
        e: io::Error,
        atlas: &Atlas,
        faults: &mut GpuFaults,
    ) -> io::Result<Surface> {
        if !matches!(surface, Surface::Vulkan(_)) {
            return Err(e);
        }
        let t = Instant::now();
        let vulkan = faults.retry(t);
        let (surface, how) = surface.recover(atlas, vulkan);
        say!(
            "fwterm: drawing on the GPU failed ({e}); now drawing with {how} ({} ms)",
            t.elapsed().as_millis()
        );
        Ok(surface)
    }

    impl Surface {
        /// After drawing on the GPU failed (a lost device, a lost surface):
        /// the same window with a new Vulkan renderer when `vulkan` and one
        /// can be made, else the CPU renderer. Panes and their programs are
        /// untouched. Returns the new surface and what now draws.
        fn recover(self, atlas: &Atlas, vulkan: bool) -> (Surface, &'static str) {
            let window = match self {
                Surface::Cpu { window, .. } => window,
                Surface::Vulkan(v) => v.into_window(),
            };
            let window = if vulkan {
                match fwrender::vk::VkWindow::try_new(
                    window,
                    atlas,
                    fwrender::vk::PresentMode::Mailbox,
                ) {
                    Ok(v) => return (Surface::Vulkan(v), "a new Vulkan device"),
                    Err((_, window)) => window,
                }
            } else {
                window
            };
            let (w, h) = window.client_size();
            let cpu = Surface::Cpu {
                window,
                cpu: CpuRenderer::new(w, h),
                presenter: CpuPresenter::new(),
            };
            (cpu, "the CPU")
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
            /// Ends the program if this process ends first, however it ends.
            _job: Option<resmon::job::KillOnClose>,
        },
        /// `--isolate`: a pane host process runs the program (`app::host`).
        Host {
            child: std::process::Child,
            stdin: Option<std::process::ChildStdin>,
        },
    }

    impl Session {
        /// The pane's root process: its pane host, or its program. What runs
        /// in the pane descends from it.
        fn pid(&self) -> u32 {
            match self {
                Session::Console(p) => p.pid(),
                Session::Pipe { child, .. } | Session::Host { child, .. } => child.id(),
            }
        }

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

        /// Tell a pane host its pane is hidden (shed: only log) or shown.
        fn shed(&mut self, on: bool) -> bool {
            match self {
                Session::Host { stdin: Some(s), .. } => wire::ToHost::Shed(on).write(s).is_ok(),
                _ => false,
            }
        }

        /// Hand a pane host the search signature of a block it announced.
        fn sign(&mut self, generation: u32, block: u64, sig: Vec<u8>) {
            if let Session::Host { stdin: Some(s), .. } = self {
                let msg = wire::ToHost::Signature {
                    generation,
                    block,
                    sig,
                };
                let _ = msg.write(s);
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
        /// Fail as a disk error would (tests).
        #[cfg(test)]
        Fail(String),
    }

    /// Why a pane's logger stopped, once it has; and whether the pane has
    /// said so yet (both of a pipe-lane pane's readers see it).
    #[derive(Default)]
    struct LogHealth {
        error: Mutex<Option<String>>,
        told: AtomicBool,
    }

    impl LogHealth {
        fn fail(&self, what: &str, e: impl std::fmt::Display) {
            self.error
                .lock()
                .unwrap()
                .get_or_insert(format!("{what}: {e}"));
        }

        fn failed(&self) -> bool {
            self.error.lock().unwrap().is_some()
        }
    }

    /// What a pane shows when its log stops mid-session.
    fn log_stopped_note(error: &str, was_paused: bool) -> String {
        let hidden = if was_paused {
            " Output while it was hidden may be missing from the screen."
        } else {
            ""
        };
        format!(
            "\r\n{RED}[fwterm: this pane's log stopped ({error}). New output is kept \
             in memory only; earlier output stays searchable.{hidden}]{RESET}\r\n"
        )
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
    /// Where a finished search signature goes: (generation, block,
    /// signature) to a logger here, or to a pane host's.
    type SigReply = Arc<dyn Fn(u32, u64, Vec<u8>) + Send + Sync>;
    type GpuIndexer = indexer::Indexer<SigReply>;
    /// Blocks of pane hosts' logs, for the window to read and sign.
    type Fetch = SyncSender<indexer::Remote<SigReply>>;

    /// Tells the window a block is durable: (generation, its folder's name,
    /// block).
    type Announce = Box<dyn Fn(u32, &str, u64) + Send>;

    /// Who builds a logger's search signatures.
    enum SignBy {
        /// The logger, as it appends (panelog's own way).
        Itself,
        /// The window's GPU indexer, in this process.
        Gpu(GpuIndexer),
        /// The window, for a pane host's logger: each block is announced
        /// once durable, and its signature comes back on `sigs`.
        Window {
            announce: Announce,
            sigs: std::sync::mpsc::Receiver<(u32, u64, Vec<u8>)>,
        },
    }

    /// A logger's search signatures when it defers them: how they are
    /// built, and signatures back early, waiting for their turn.
    struct Signing {
        how: How,
        ready: std::collections::BTreeMap<u64, Vec<u8>>,
    }

    enum How {
        /// The blocks this logger has cut, for the GPU.
        Gpu {
            indexer: GpuIndexer,
            reply: SigReply,
            stager: indexer::BlockStager,
        },
        /// Blocks announced to the window so far, this generation.
        Window { announce: Announce, announced: u64 },
    }

    impl Signing {
        /// Hand every block `bytes` completes to the GPU, or build it here
        /// when the GPU cannot take it now; then store what is in order.
        fn appended(&mut self, bytes: &[u8], generation: u32, w: &mut panelog::Writer) {
            let How::Gpu {
                indexer,
                reply,
                stager,
            } = &mut self.how
            else {
                return;
            };
            for st in stager.push(bytes) {
                let local = match st.lead {
                    // A generation's first block: nothing before it to send.
                    None => Some(panelog::signature(None, &st.bytes)),
                    Some(lead) => {
                        let job = indexer::Job {
                            reply: reply.clone(),
                            generation,
                            block: st.block,
                            lead,
                            bytes: st.bytes,
                        };
                        indexer
                            .submit(job)
                            .err()
                            .map(|j| panelog::signature(Some(j.lead), &j.bytes))
                    }
                };
                if let Some(sig) = local {
                    indexer.counts.cpu.fetch_add(1, Ordering::Relaxed);
                    self.ready.insert(st.block, sig);
                }
            }
            self.store(w);
        }

        /// Announce the blocks `w` has made durable since the last call, when
        /// the window signs them.
        fn announce(&mut self, generation: u32, name: &str, w: &panelog::Writer) {
            if let How::Window {
                announce,
                announced,
            } = &mut self.how
            {
                while (*announced + 1) * BLOCK_BYTES <= w.durable_len() {
                    announce(generation, name, *announced);
                    *announced += 1;
                }
            }
        }

        /// A new generation starts: its blocks count from 0.
        fn rolled(&mut self) {
            match &mut self.how {
                How::Gpu { stager, .. } => *stager = indexer::BlockStager::default(),
                How::Window { announced, .. } => *announced = 0,
            }
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

    /// The logger stops at the first write, flush or roll-over that fails,
    /// and says why in `health`: output it cannot keep must not vanish
    /// silently.
    fn start_logger(
        pane_dir: PathBuf,
        id: PaneId,
        sign: SignBy,
        health: Arc<LogHealth>,
    ) -> io::Result<SyncSender<LogMsg>> {
        let cfg = panelog::Config {
            pane: u64::from(std::process::id()) << 16 | id as u64,
            ..panelog::Config::default()
        };
        let deferred = !matches!(sign, SignBy::Itself);
        let open = move |n: u32| {
            let name = logstore::gen_name(logstore::now_ms(), n);
            let mut w = panelog::Writer::create(&pane_dir.join(&name), cfg)?;
            if deferred {
                w.defer_signatures()?;
            }
            Ok::<_, io::Error>((w, name))
        };
        let mut generation = 0u32;
        let (mut writer, mut name) = open(generation)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<LogMsg>(LOG_QUEUE);
        let (sig_tx, own_sigs) = std::sync::mpsc::channel();
        let mut retiring: Option<Retiring> = None;
        let (how, sig_rx) = match sign {
            SignBy::Itself => (None, own_sigs),
            SignBy::Gpu(indexer) => {
                let reply: SigReply = Arc::new(move |g, b, sig| {
                    let _ = sig_tx.send((g, b, sig));
                });
                let how = How::Gpu {
                    indexer,
                    reply,
                    stager: indexer::BlockStager::default(),
                };
                (Some(how), own_sigs)
            }
            SignBy::Window { announce, sigs } => (
                Some(How::Window {
                    announce,
                    announced: 0,
                }),
                sigs,
            ),
        };
        let mut signing = how.map(|how| Signing {
            how,
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
                        if let Err(e) = writer.append(&bytes, t) {
                            return health.fail("writing", e);
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
                        if let Err(e) = writer.flush() {
                            return health.fail("writing", e);
                        }
                        if let Some(s) = signing.as_mut() {
                            s.announce(generation, &name, &writer);
                        }
                        let (next, next_name) = match open(generation + 1) {
                            Ok(w) => w,
                            Err(e) => return health.fail("starting a new log generation", e),
                        };
                        name = next_name;
                        let old = std::mem::replace(&mut writer, next);
                        if let Some(s) = signing.as_mut() {
                            s.rolled();
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
                        // Not answered on failure: the one waiting sees the
                        // logger gone.
                        if let Err(e) = writer.flush() {
                            return health.fail("writing", e);
                        }
                        dirty = false;
                        let _ = done.send(());
                    }
                    #[cfg(test)]
                    Ok(LogMsg::Fail(e)) => return health.fail("writing", e),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if dirty {
                            if let Err(e) = writer.flush() {
                                return health.fail("writing", e);
                            }
                            dirty = false;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        let _ = writer.flush();
                        return;
                    }
                }
                // Appends and flushes make blocks durable.
                if let Some(s) = signing.as_mut() {
                    s.announce(generation, &name, &writer);
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
    /// On each pane's first line: how to reach the settings. Spelled out,
    /// since "Ctrl+," reads as Ctrl and plus.
    const SETTINGS_HINT: &str = " · settings: Ctrl+Comma";

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
        /// Set when the logger stops on an error.
        health: Arc<LogHealth>,
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
                        "{DIM}fwterm {version} · pane logs in {}, keeping {keeping}\
                         {SETTINGS_HINT}{RESET}\r\n",
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
                        "{DIM}fwterm {version}{SETTINGS_HINT}{RESET}\r\n\
                         {RED}Logging is off: {problem}{RESET}\r\n"
                    )
                }
                (None, None) => {
                    format!("{DIM}fwterm {version} · logging off{SETTINGS_HINT}{RESET}\r\n")
                }
            }
        }
    }

    /// Durable appends to a scratch log at `root`: MiB/s and the p99 of one
    /// 1 MiB write, in ms. `on_write` hears how many writes are done after
    /// each, and stops the measurement by returning false.
    fn probe_drive(root: &Path, on_write: &mut dyn FnMut(u32) -> bool) -> io::Result<(f64, f64)> {
        const WRITES: usize = first_run::PROBE_WRITES as usize;
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
            for n in 1..=WRITES {
                let t = Instant::now();
                w.append(&chunk, 0)?;
                w.flush()?;
                times.push(t.elapsed().as_secs_f64() * 1e3);
                if !on_write(n as u32) {
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
                }
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
            match probe_drive(&root, &mut |_| true) {
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

    /// How the first run ended.
    enum FirstRunEnd {
        /// Finished or skipped: the panes start.
        Go,
        /// The window was closed.
        Closed,
    }

    /// The first run (design M5, the Mirai-06 board): its steps drawn in the
    /// window before any pane starts, every fixed drive measured meanwhile
    /// on a thread of its own. Finish saves to the settings file, other
    /// lines kept; Skip saves nothing.
    fn ask_first_run(
        mut surface: Surface,
        atlas: &mut Atlas,
        raster: &mut FontRasterizer,
        gpu_faults: &mut GpuFaults,
    ) -> io::Result<(Surface, FirstRunEnd)> {
        use crate::first_run::{Action, Key as K, Probe};
        let m = raster.metrics();
        let (app, conf) = settings();
        let var = |k: &str| std::env::var(k).ok();
        let shells = first_run::find_shells(&var, &|p: &Path| p.is_file());
        let drives: Vec<first_run::Drive> = resmon::volume::fixed()
            .iter()
            .filter_map(|root| resmon::volume::of(root).ok())
            .map(|v| first_run::Drive {
                root: v.root,
                free: v.free,
                total: v.total,
                probe: Probe::Waiting,
            })
            .collect();
        let mut fr = first_run::FirstRun::new(shells, drives, &conf);
        // One drive at a time (together they would share the disks and
        // each measure slower), stopped when the first run ends.
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Probe)>();
        let measuring = {
            let roots: Vec<PathBuf> = fr.drives.iter().map(|d| d.root.clone()).collect();
            let (stop, waker) = (stop.clone(), surface.window().waker());
            std::thread::spawn(move || {
                for (i, root) in roots.iter().enumerate() {
                    let send = |p: Probe| {
                        let sent = tx.send((i, p)).is_ok();
                        waker.wake();
                        sent
                    };
                    if stop.load(Ordering::Acquire) || !send(Probe::Measuring(0)) {
                        return;
                    }
                    // Every eighth write moves the meter.
                    let result = probe_drive(root, &mut |n| {
                        !stop.load(Ordering::Acquire) && (n % 8 != 0 || send(Probe::Measuring(n)))
                    });
                    let probe = match result {
                        Ok((rate, p99)) => Probe::Done(rate, p99),
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => return,
                        Err(e) => Probe::Failed(e.to_string()),
                    };
                    if !send(probe) {
                        return;
                    }
                }
            })
        };
        // The theme chosen is the one drawn, title bar too.
        let theme_for = |name: &str| {
            let mut t = Theme::named(name).unwrap_or_else(Theme::mirai);
            t.apply(&conf.colors);
            t
        };
        let frame_colors = |surface: &mut Surface, t: &Theme| match t.title_bar {
            Some(bar) => {
                surface.window().set_frame_colors(bar, t.palette.fg, t.rule);
            }
            None => surface.window().reset_frame_colors(),
        };
        let mut shown = fr.theme_name();
        let mut theme = theme_for(shown);
        frame_colors(&mut surface, &theme);
        let grid = |w: u32, h: u32| {
            (
                (h / m.cell_h).max(1) as usize,
                (w / m.cell_w).max(1) as usize,
            )
        };
        let (w, h) = surface.window().client_size();
        let (mut rows, mut cols) = grid(w, h);
        let mut frame = Frame::new();
        let mut events = Vec::new();
        let end = 'run: loop {
            if fr.theme_name() != shown {
                shown = fr.theme_name();
                theme = theme_for(shown);
                frame_colors(&mut surface, &theme);
            }
            frame.set_grid(rows, cols, &theme.palette);
            let cells = first_run::render(
                &fr,
                cols.saturating_sub(4).min(104),
                rows.saturating_sub(2).min(28),
            );
            let (top, left) = (
                rows.saturating_sub(cells.len()) / 2,
                cols.saturating_sub(cells[0].len()) / 2,
            );
            let mut pen = Pen {
                palette: &theme.palette,
                theme: &theme,
                atlas: &mut *atlas,
                raster: &mut *raster,
            };
            draw_sheet(&cells, top, left, &mut frame, &mut pen);
            if frame.damaged_rows() > 0 {
                if let Err(e) = surface.draw(&frame, atlas) {
                    surface = after_draw_error(surface, e, atlas, gpu_faults)?;
                    frame.damage_all();
                    surface.draw(&frame, atlas)?;
                }
                frame.clear_damage();
            }
            events.clear();
            surface.window().wait(None, &mut events);
            for e in &events {
                let key = match *e {
                    Event::Char('\r') => K::Enter,
                    Event::Char('\u{1b}') => K::Esc,
                    Event::Char('\u{8}') => K::Backspace,
                    Event::Char(c) => K::Char(c),
                    Event::Key {
                        vk: 0x26,
                        down: true,
                    } => K::Up,
                    Event::Key {
                        vk: 0x28,
                        down: true,
                    } => K::Down,
                    Event::Key {
                        vk: 0x25,
                        down: true,
                    } => K::Left,
                    Event::Key {
                        vk: 0x27,
                        down: true,
                    } => K::Right,
                    Event::Resized(w, h) if w > 0 && h > 0 => {
                        if let Err(e) = surface.resize(w, h) {
                            surface = after_draw_error(surface, e, atlas, gpu_faults)?;
                        }
                        (rows, cols) = grid(w, h);
                        frame.damage_all();
                        continue;
                    }
                    Event::Paint => {
                        frame.damage_all();
                        continue;
                    }
                    Event::CloseRequested => break 'run FirstRunEnd::Closed,
                    _ => continue,
                };
                match fr.key(key) {
                    Action::None => {}
                    Action::Skip => break 'run FirstRunEnd::Go,
                    Action::Finish(values) => {
                        let Some(app) = app.as_deref() else {
                            say!("fwterm: no LOCALAPPDATA: the first run's choices were not saved");
                            break 'run FirstRunEnd::Go;
                        };
                        let path = logstore::settings_path(app);
                        let old = std::fs::read_to_string(&path).unwrap_or_default();
                        let old = if old.trim().is_empty() {
                            "# fwterm settings. Ctrl+Comma in fwterm changes them, and\n\
                             # fwterm --first-run asks the first-run questions again.\n"
                                .to_string()
                        } else {
                            old
                        };
                        let saved = std::fs::create_dir_all(app).and_then(|_| {
                            std::fs::write(&path, settings_ui::merged(&old, &values))
                        });
                        if let Err(e) = saved {
                            say!("fwterm: could not save {}: {e}", path.display());
                        }
                        break 'run FirstRunEnd::Go;
                    }
                }
            }
            while let Ok((i, probe)) = rx.try_recv() {
                fr.update(i, probe);
            }
        };
        // It stops after its current 1 MiB write and removes its scratch
        // folder, before a closed window ends the process.
        stop.store(true, Ordering::Release);
        let _ = measuring.join();
        Ok((surface, end))
    }

    /// Lines of a log a regex is tried on, from the index's hits for the
    /// literal it requires (newest first).
    const REGEX_CANDIDATES: usize = 20_000;
    /// How long a regex may try log lines, every pane at once; past it, the
    /// search reports the newest it covered.
    const REGEX_SCAN: Duration = Duration::from_secs(5);

    /// Hits for `query` in every generation of one pane's logs, newest
    /// generation first, at most `limit` per generation, with lines numbered
    /// as [`Deep`] numbers them; and whether a regex stopped at `deadline`
    /// before trying every line it had to.
    ///
    /// Text goes to the log's index. A regex tries the lines the index finds
    /// for a literal it requires (three characters or more, as the index
    /// needs); without one it reads the lines, newest first.
    fn search_pane(
        id: PaneId,
        dir: &Path,
        query: &find::Query,
        limit: usize,
        deadline: Instant,
    ) -> (Vec<find::Hit>, bool) {
        // Every generation with the line it begins on, before reading any.
        let mut gens = Vec::new();
        let mut start = 0u64;
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
            let lines = r.line_count();
            gens.push((r, start, stamp));
            start += lines - 1;
        }
        let mut hits = Vec::new();
        let mut cut = false;
        for (r, start, stamp) in gens.iter().rev() {
            let mut push = |local: u64, offset: u64, text: String| {
                hits.push(find::Hit {
                    pane: id,
                    dir: dir.to_path_buf(),
                    line: start + local,
                    text,
                    generation: *stamp,
                    offset,
                    session: None,
                });
            };
            let re = match query {
                find::Query::Text(needle) => {
                    let opts = panelog::SearchOptions {
                        ignore_case: true,
                        limit,
                    };
                    for offset in r.search(needle.as_bytes(), &opts).unwrap_or_default() {
                        let Ok(local) = r.line_of(offset) else {
                            continue;
                        };
                        let text = r.line(local).map(|l| find::clean(&l, 240));
                        push(local, offset, text.unwrap_or_default());
                    }
                    continue;
                }
                find::Query::Regex(re) => re,
            };
            // A regex: whether line `local` matches, and its text if so.
            let try_line = |local: u64| -> Option<String> {
                let text = find::clean(&r.line(local).ok()?, 4096);
                let chars: Vec<char> = text.chars().collect();
                re.find(&chars).map(|_| text.chars().take(240).collect())
            };
            let mut found = 0;
            match re.literal().filter(|l| l.len() >= 3) {
                Some(literal) => {
                    let opts = panelog::SearchOptions {
                        ignore_case: true,
                        limit: REGEX_CANDIDATES,
                    };
                    let mut last = None;
                    for offset in r.search(literal.as_bytes(), &opts).unwrap_or_default() {
                        let Ok(local) = r.line_of(offset) else {
                            continue;
                        };
                        // Several hits on one line: tried once.
                        if last.replace(local) == Some(local) {
                            continue;
                        }
                        // A literal on most lines ("err") leaves nearly every
                        // line to try: the same deadline as reading them all.
                        if Instant::now() >= deadline {
                            cut = true;
                            break;
                        }
                        if let Some(text) = try_line(local) {
                            push(local, offset, text);
                            found += 1;
                            if found == limit {
                                break;
                            }
                        }
                    }
                }
                None => {
                    for local in (0..r.line_count()).rev() {
                        if Instant::now() >= deadline {
                            cut = true;
                            break;
                        }
                        if let Some(text) = try_line(local) {
                            push(local, r.line_start(local).unwrap_or(0), text);
                            found += 1;
                            if found == limit {
                                break;
                            }
                        }
                    }
                }
            }
            if cut {
                break;
            }
        }
        (hits, cut)
    }

    /// The search bar: what is typed, what was last searched, its hits and
    /// which one is shown.
    #[derive(Default)]
    struct FindBar {
        query: String,
        searched: String,
        hits: Vec<find::Hit>,
        at: usize,
        running: Option<std::sync::mpsc::Receiver<(Vec<find::Hit>, Duration, bool)>>,
        /// A pattern that did not parse, shown instead of results.
        error: Option<String>,
        /// Said after the results: a regex search that stopped early.
        note: Option<String>,
        /// How long the last search took, start to hits.
        took: Option<Duration>,
    }

    /// How often a selection dragged past a pane's edge scrolls it.
    const EDGE_SCROLL_EVERY: Duration = Duration::from_millis(50);

    /// Hits kept per generation of each pane.
    const FIND_LIMIT: usize = 500;

    /// A bar across window row `row`: `text`, then blanks, in the bar's
    /// colours, its first `lead` characters (the bar's name) in the accent
    /// (the search bar, a paste waiting to be confirmed).
    fn draw_bar(
        text: &str,
        lead: usize,
        row: usize,
        cols: usize,
        frame: &mut Frame,
        pen: &mut Pen,
    ) {
        let mut chars = text.chars();
        for col in 0..cols {
            let ch = chars.next().unwrap_or(' ');
            let glyph = pen.atlas.slot(
                GlyphKey {
                    ch,
                    style: 0,
                    part: GlyphPart::Single,
                },
                pen.raster,
            );
            let cell = CellInstance {
                glyph,
                fg: if col < lead {
                    pen.theme.accent
                } else {
                    pen.theme.bar_text
                },
                bg: pen.theme.bar,
                flags: 0,
            };
            frame.put(row, col, cell);
        }
    }

    /// Send a mouse event at client pixel `(x, y)` to `p`'s program, as the
    /// cell under it (kept inside the pane, for a drag that leaves it), if
    /// the program asked for the mouse; true if sent. Shift held keeps the
    /// mouse fwterm's (selecting, scrolling), and so does a view scrolled
    /// back from the live screen.
    fn report_mouse(
        p: &Pane,
        button: mouse::Button,
        action: mouse::Action,
        x: i32,
        y: i32,
        m: Metrics,
    ) -> bool {
        if fwrender::win::shift_down() || p.back != 0 || p.deep.is_some() {
            return false;
        }
        let (tracking, sgr) = p.shared.term.lock().unwrap().mouse();
        let within = |v: i32, cell: u32, start: usize, len: usize| {
            (v.div_euclid(cell as i32) - start as i32).clamp(0, len.max(1) as i32 - 1) as usize
        };
        let row = within(y, m.cell_h, p.rect.top, p.rect.rows);
        let col = within(x, m.cell_w, p.rect.left, p.rect.cols);
        let mods = mouse::Mods {
            shift: false,
            alt: fwrender::win::alt_down(),
            ctrl: fwrender::win::ctrl_down(),
        };
        match mouse::report(tracking, sgr, button, action, row, col, mods) {
            Some(bytes) => {
                let _ = p.session.lock().unwrap().write(&bytes);
                true
            }
            None => false,
        }
    }

    /// The wheel for the program in the pane under the pointer (else the
    /// focused one): as mouse reports if it asked for the mouse, else as
    /// arrow keys on the alternate screen, which has no history to scroll.
    /// False leaves the wheel to scroll back through history.
    fn wheel_to_program(
        panes: &[Pane],
        placement: &Placement,
        focus: PaneId,
        pointer: Option<(i32, i32)>,
        delta: i32,
        m: Metrics,
    ) -> bool {
        let over = pointer
            .and_then(|(x, y)| {
                let row = y.max(0) as usize / m.cell_h as usize;
                let col = x.max(0) as usize / m.cell_w as usize;
                pane_at(&placement.panes, row, col)
            })
            .unwrap_or(focus);
        let Some(p) = panes.iter().find(|p| p.id == over) else {
            return false;
        };
        let (x, y) = pointer.unwrap_or((0, 0));
        let notches = (delta.unsigned_abs() as usize / 120).max(1);
        let button = if delta > 0 {
            mouse::Button::WheelUp
        } else {
            mouse::Button::WheelDown
        };
        if report_mouse(p, button, mouse::Action::Press, x, y, m) {
            for _ in 1..notches {
                report_mouse(p, button, mouse::Action::Press, x, y, m);
            }
            return true;
        }
        let alt = p.shared.term.lock().unwrap().in_alternate_screen();
        if alt && !fwrender::win::shift_down() && p.back == 0 {
            let keys = mouse::wheel_as_arrows(delta > 0, notches * 3);
            let _ = p.session.lock().unwrap().write(&keys);
            return true;
        }
        false
    }

    /// The newest decision not yet looked at, as the strip shows it: who
    /// raised it (atrium's name for it) and what it asks, and with several
    /// panes, which of fwterm's it is in.
    fn newest_ask(
        asks: &[agents::Ask],
        looked_at: &HashSet<(u32, u64, u64)>,
        panes: &[Pane],
    ) -> Option<String> {
        let d = asks.iter().rev().find(|d| !looked_at.contains(&d.key))?;
        let pane = panes.iter().position(|p| p.id == d.pane)? + 1;
        let place = if panes.len() > 1 {
            format!(" (fwterm pane {pane})")
        } else {
            String::new()
        };
        Some(format!("{}{place}", d.label()))
    }

    /// The agents by pane for the strip: each pane's number (from 1) and
    /// its agents' statuses, in pane order; panes without agents left out.
    fn agents_by_pane(
        agents: &[agents::Agent],
        panes: &[Pane],
    ) -> Vec<(usize, Vec<agents::Status>)> {
        panes
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let statuses: Vec<agents::Status> = agents
                    .iter()
                    .filter(|a| a.pane == p.id)
                    .map(|a| a.status)
                    .collect();
                (!statuses.is_empty()).then_some((i + 1, statuses))
            })
            .collect()
    }

    /// Where the panes go in a `rows x cols` grid whose top `strip` rows are
    /// the instrument strip's.
    fn place_panes(
        layout: &Layout,
        zoom: Option<PaneId>,
        rows: usize,
        cols: usize,
        strip: usize,
        ribbon: bool,
    ) -> Placement {
        let mut p = layout.place_zoomed(zoom, rows.saturating_sub(strip).max(1), cols);
        for (_, r) in &mut p.panes {
            r.top += strip;
            // The activity ribbon takes the pane's rightmost column. A pane
            // of two columns or fewer keeps them: `draw` paints the ribbon
            // only beside two or more.
            if ribbon && r.cols > 2 {
                r.cols -= 1;
            }
        }
        for d in &mut p.dividers {
            d.0 += strip;
        }
        p
    }

    /// Draw the instrument strip across grid row 0: the readings, cut short
    /// of the SETTINGS and HELP buttons at the right edge.
    fn draw_strip(line: &[(char, strip::Kind)], cols: usize, frame: &mut Frame, pen: &mut Pen) {
        let t = pen.theme;
        let bg = t.surface;
        let row = strip::with_buttons(line, cols);
        for (col, &(ch, kind)) in row.iter().enumerate() {
            let fg = match kind {
                strip::Kind::Label => t.dim,
                strip::Kind::Value => t.palette.fg,
                strip::Kind::Accent => t.accent,
                strip::Kind::Good => t.palette.indexed[2],
                strip::Kind::Bad => t.error,
            };
            let key = GlyphKey {
                ch,
                style: 0,
                part: GlyphPart::Single,
            };
            let glyph = pen.atlas.slot(key, pen.raster);
            frame.put(
                0,
                col,
                CellInstance {
                    glyph,
                    fg,
                    bg,
                    flags: 0,
                },
            );
        }
    }

    /// Draw the settings sheet's cells with their top-left at `(top, left)`.
    fn draw_sheet(
        cells: &[Vec<settings_ui::Cell>],
        top: usize,
        left: usize,
        frame: &mut Frame,
        pen: &mut Pen,
    ) {
        use settings_ui::Paint as P;
        let t = pen.theme;
        let bg = t.surface;
        for (r, row) in cells.iter().enumerate() {
            for (c, &(ch, paint, sel)) in row.iter().enumerate() {
                let fg = match paint {
                    P::Rule => t.rule,
                    P::Corner | P::Title | P::NavOn | P::Button => t.accent,
                    P::Nav | P::Note | P::Hint => t.dim,
                    P::Label | P::Value => t.palette.fg,
                    P::Meter => t.accent,
                    P::Track => t.rule,
                    P::Warn => t.error,
                    P::Swatch(i) => t.palette.indexed[usize::from(i)],
                };
                let key = GlyphKey {
                    ch,
                    style: 0,
                    part: GlyphPart::Single,
                };
                let glyph = pen.atlas.slot(key, pen.raster);
                let bg = if sel { t.palette.selection } else { bg };
                frame.put(
                    top + r,
                    left + c,
                    CellInstance {
                        glyph,
                        fg,
                        bg,
                        flags: 0,
                    },
                );
            }
        }
    }

    /// Leave the search: every pane back to its live view.
    fn close_find(panes: &mut [Pane]) {
        for p in panes.iter_mut() {
            p.find_dir = None;
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
        // A pane closed since, or one of an earlier session: its log is
        // shown in the focused pane.
        let open = hit.session.is_none() && panes.iter().any(|p| p.id == hit.pane);
        let target = if open { hit.pane } else { *focus };
        if zoom.is_some() && *zoom != Some(target) {
            *zoom = None;
        }
        for p in panes.iter_mut() {
            if p.id == target {
                p.find_view = Some((hit.line, needle.to_string()));
                p.find_dir = (!open).then(|| hit.dir.clone());
                p.deep = None; // rebuilt from the right log
                p.shared.pending.store(true, Ordering::Release);
                *focus = target;
            } else if p.find_view.take().is_some() {
                p.find_dir = None;
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
        /// The log ends lines with bare line feeds: a pipe pane's, perhaps
        /// from another session in another lane, replayed as one.
        bare_lf: bool,
        /// What full-screen programs drew, as line ranges: scrolling back
        /// skips them (`altscreen`), search still reaches them.
        alt: Vec<(u64, u64)>,
    }

    /// Whether most line breaks in `bytes` are bare line feeds, as a pipe
    /// pane's program writes them; a console pane's host writes CR LF.
    fn bare_line_feeds(bytes: &[u8]) -> bool {
        let lf = bytes.iter().filter(|&&b| b == b'\n').count();
        let crlf = bytes.windows(2).filter(|w| w == b"\r\n").count();
        lf - crlf > crlf
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
            // The first 64 KiB of the newest generation tell the lane.
            let mut head = vec![0u8; 64 * 1024];
            let n = gens
                .last()
                .map_or(0, |(r, _)| r.read_at(0, &mut head).unwrap_or(0));
            // Where full-screen programs started and stopped, in log lines.
            let mut switches = Vec::new();
            for (reader, start) in &gens {
                let Ok(marks) = reader.marks_in(0..reader.len()) else {
                    continue;
                };
                for m in marks {
                    let enter = match m.kind {
                        altscreen::ENTER => true,
                        altscreen::LEAVE => false,
                        _ => continue,
                    };
                    if let Ok(line) = reader.line_of(m.offset) {
                        switches.push((start + line, enter));
                    }
                }
            }
            Ok(Deep {
                gens,
                total,
                at: u64::MAX,
                term: Term::new(1, 1),
                bare_lf: bare_line_feeds(&head[..n]),
                alt: altscreen::spans(&switches, total),
            })
        }

        /// Lines scrolling back can reach: those no full-screen program drew.
        fn visible(&self) -> u64 {
            altscreen::visible(self.total, &self.alt)
        }

        /// How far back a view of `rows` rows can go: its top row is the
        /// first line of the log, as memory's clamp puts the oldest held
        /// line at the top. Any further and the view's bottom would climb
        /// through the log, leaving blank rows below the last line shown.
        fn deepest(&self, rows: usize) -> u64 {
            self.visible().saturating_sub(rows as u64)
        }

        /// The log line a view `back` lines above live ends before.
        fn line_back(&self, back: u64) -> u64 {
            altscreen::line_back(self.total, &self.alt, back)
        }

        /// How far back a view ending before `line` is.
        fn back_of(&self, line: u64) -> u64 {
            altscreen::back_of(self.total, &self.alt, line)
        }

        /// The last prompt (a shell's `A` mark) on a line before `line`, in
        /// the logs' line numbers.
        fn prev_prompt(&self, line: u64) -> Option<u64> {
            for (reader, start) in self.gens.iter().rev() {
                if *start >= line {
                    continue;
                }
                let local = line - start;
                let before = if local >= reader.line_count() {
                    reader.len()
                } else {
                    reader.line_start(local).ok()?
                };
                if before == 0 {
                    continue;
                }
                if let Ok(Some(m)) = reader.last_mark_before(before - 1, |m| m.kind == b'A') {
                    return reader.line_of(m.offset).ok().map(|l| start + l);
                }
            }
            None
        }

        /// The first prompt on a line after `line`, in the logs' line numbers.
        fn next_prompt(&self, line: u64) -> Option<u64> {
            for (reader, start) in &self.gens {
                if start + reader.line_count() <= line + 1 {
                    continue;
                }
                let from = if line < *start {
                    0
                } else {
                    reader.line_start(line - start + 1).ok()?
                };
                let marks = reader.marks_in(from..reader.len()).ok()?;
                if let Some(m) = marks.iter().find(|m| m.kind == b'A') {
                    return reader.line_of(m.offset).ok().map(|l| start + l);
                }
            }
            None
        }

        /// Rebuild the screen as it was when the logs reached line `end`:
        /// restore the nearest checkpoint before it in its generation, replay
        /// the rest. The line break ending line `end - 1` is left out, so that
        /// line is the bottom row and the screen shows the `rows` lines before
        /// `end`, not a blank row for the line to come.
        fn rebuild(&mut self, end: u64, rows: usize, cols: usize, pipe: bool) -> io::Result<()> {
            if end == self.at {
                return Ok(());
            }
            // The generation holding the break before `end`: at a roll-over
            // line (the end of one and the start of the next), the earlier.
            let k = self
                .gens
                .partition_point(|(_, start)| *start < end)
                .saturating_sub(1);
            let (reader, start) = &self.gens[k];
            let local = (end.saturating_sub(*start)).min(reader.line_count() - 1);
            let offset = match reader.line_start(local)? {
                o if local > 0 => o - 1,
                o => o,
            };
            let (mut term, from) = match reader.checkpoint_before(offset)? {
                Some((at, blob)) => match Term::restore(&blob) {
                    Ok(t) => (t, at),
                    Err(_) => (Term::new(rows, cols), 0),
                },
                None => (Term::new(rows, cols), 0),
            };
            if from == 0 && (pipe || self.bare_lf) {
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
        /// An isolated pane waiting for history lines from its host: the
        /// first and how many, and where they go.
        #[allow(clippy::type_complexity)]
        history_reply: Mutex<
            Option<(
                u64,
                usize,
                std::sync::mpsc::Sender<(u64, Vec<Vec<ansi::Cell>>, Vec<bool>)>,
            )>,
        >,
        /// An isolated pane's history lines in a scrolled-back view, asked
        /// for without waiting.
        host_lines: Mutex<hostlines::HostLines>,
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
                "{text}index_gpu_blocks {}\nindex_cpu_blocks {}\nindex_launches {}\n\
                 index_faults {}\n",
                c.gpu.load(Ordering::Relaxed),
                c.cpu.load(Ordering::Relaxed),
                c.launches.load(Ordering::Relaxed),
                c.faults.load(Ordering::Relaxed)
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
        /// Showing a search hit: its line in the pane's logs, and the needle;
        /// and, for a hit in a pane closed since, that pane's log folder.
        find_view: Option<(u64, String)>,
        find_dir: Option<PathBuf>,
        /// Folded commands (Ctrl+Shift+[), and the line each view row showed
        /// when last drawn with folds in view (a folded row: its first line;
        /// empty when the view had no folds).
        folds: fold::Folds,
        row_lines: Vec<u64>,
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
        /// into the pane (a drag can leave it). While the view shows the
        /// screen rebuilt from the log, whose lines are not the emulator's,
        /// the line is that screen's row (see [`Selection::log`]).
        fn point(&self, x: i32, y: i32, m: Metrics) -> Option<Point> {
            if !self.visible() {
                return None;
            }
            let r = self.rect;
            let row = (y.div_euclid(m.cell_h as i32) - r.top as i32).clamp(0, r.rows as i32 - 1);
            let col = (x.div_euclid(m.cell_w as i32) - r.left as i32).clamp(0, r.cols as i32 - 1);
            if self.deep.is_some() {
                return Some(Point {
                    line: row as u64,
                    col: col as usize,
                });
            }
            // The view as last drawn: its top row is `seen_scrolled - back`,
            // or, with folds in view, the rows it drew.
            let top = self.seen_scrolled.saturating_sub(self.back as u64);
            let line = match self.row_lines.last() {
                None => top + row as u64,
                Some(&last) => self.row_lines.get(row as usize).copied().unwrap_or(last),
            };
            Some(Point {
                line,
                col: col as usize,
            })
        }

        /// The output of the command at the top of the view (at the live
        /// view, the latest with output), from shell-integration marks,
        /// without the blank lines a shell leaves before its next prompt (cmd
        /// prints one after every command). What Ctrl+Shift+O selects and
        /// Ctrl+Shift+[ folds.
        fn command_output(&self) -> Option<std::ops::Range<u64>> {
            let top = self.seen_scrolled.saturating_sub(self.back as u64);
            let at = if self.back == 0 { u64::MAX } else { top };
            let term = self.shared.term.lock().unwrap();
            let mut lines = self.shared.blocks.lock().unwrap().output_at(at)?;
            let blank = |n: u64| {
                Pane::line_chars(&term, n, &HashMap::new())
                    .is_some_and(|l| l.iter().all(|(c, _)| *c == ' '))
            };
            while lines.end > lines.start && blank(lines.end - 1) {
                lines.end -= 1;
            }
            (!lines.is_empty()).then_some(lines)
        }

        /// For an isolated pane, the lines in `lines` that have left its
        /// screen, fetched from its host (which holds its history), waiting
        /// up to half a second. Empty for a local pane: its history is here.
        #[allow(clippy::type_complexity)]
        fn history_for(
            &self,
            lines: std::ops::Range<u64>,
        ) -> (HashMap<u64, Vec<ansi::Cell>>, HashSet<u64>) {
            let none = || (HashMap::new(), HashSet::new());
            let scrolled = match &*self.shared.term.lock().unwrap() {
                Emu::Remote(m) => m.scrolled,
                Emu::Local(_) => return none(),
            };
            let end = lines.end.min(scrolled);
            if lines.start >= end {
                return none();
            }
            let count = (end - lines.start).min(100_000) as u32;
            let (tx, rx) = std::sync::mpsc::channel();
            *self.shared.history_reply.lock().unwrap() = Some((lines.start, count as usize, tx));
            if self
                .session
                .lock()
                .unwrap()
                .ask_history(lines.start, count)
                .is_err()
            {
                return none();
            }
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok((first, got, wrapped)) => {
                    let runs_on = wrapped
                        .iter()
                        .enumerate()
                        .filter(|(_, &w)| w)
                        .map(|(i, _)| first + i as u64)
                        .collect();
                    let lines = got
                        .into_iter()
                        .enumerate()
                        .filter(|(_, l)| !l.is_empty())
                        .map(|(i, l)| (first + i as u64, l))
                        .collect();
                    (lines, runs_on)
                }
                Err(_) => none(),
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
            if let Some(at) = sel.log {
                // Made in the view rebuilt from the log: its rows, while that
                // screen is still the one shown.
                return match self.deep.as_ref().filter(|d| d.at == at) {
                    Some(d) => screen_text(d.term.screen(), start, end),
                    None => String::new(),
                };
            }
            let (fetched, fetched_wrapped) = self.history_for(start.line..end.line + 1);
            let term = self.shared.term.lock().unwrap();
            select::text(start, end, term.screen().cols(), |n, from, to| {
                let chars = Self::line_chars(&term, n, &fetched)?;
                // Whether line `n` ran on into the next: from the host for
                // lines fetched from it, else from what the pane holds.
                let wrapped = if fetched.contains_key(&n) {
                    fetched_wrapped.contains(&n)
                } else {
                    term.line_wrapped(n)
                };
                let text = chars[from.min(chars.len())..to.min(chars.len())]
                    .iter()
                    .filter(|(_, half)| !half)
                    .map(|(c, _)| c)
                    .collect();
                Some((text, wrapped))
            })
        }

        /// The word under `at`, as a selection.
        fn word_at(&self, at: Point) -> Option<Selection> {
            let log = self.deep.as_ref().map(|d| d.at);
            let chars: Vec<char> = match &self.deep {
                // The view rebuilt from the log: `at.line` is its row.
                Some(d) => (at.line < d.term.screen().rows() as u64)
                    .then(|| screen_row(d.term.screen(), at.line as usize))?,
                None => {
                    let term = self.shared.term.lock().unwrap();
                    Self::line_chars(&term, at.line, &HashMap::new())?
                }
            }
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
                log,
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
                let wake: Wake = {
                    let w = waker.clone();
                    Arc::new(move || w.wake())
                };
                if let Err(e) = catch_up(&shared, &tx, &dir, &wake) {
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
            ribbon: bool,
            frame: &mut Frame,
            pen: &mut Pen,
        ) {
            let r = self.rect;
            let shared = self.shared.clone();
            let mut term = shared.term.lock().unwrap();
            shared.want.store(false, Ordering::Release);
            term.take_dirty();
            // The cursor, where the pane has focus and the program shows it.
            let focused = focused && term.cursor_visible();
            let history = term.scrollback();
            let scrolled = term.scrolled_lines();
            if term.in_alternate_screen() {
                self.back = 0; // full-screen programs: always live
            } else if self.back > 0 && self.deep.is_none() {
                // New lines pushed the text we are looking at further up.
                self.back += (scrolled - self.seen_scrolled) as usize;
            }
            self.seen_scrolled = scrolled;
            // An isolated pane's history is in its host, which sends the
            // lines a scrolled-back view shows.
            let remote = matches!(&*term, Emu::Remote(_));
            let held = term.history_len();
            let (rows, cols) = (term.screen().rows(), term.screen().cols());
            let logged = self.find_dir.is_some() || self.pane_dir.is_some();
            // A search hit in a pane without a log: shown from memory, below.
            let find_here = self.find_view.clone().filter(|_| !logged);
            if let Some((line, needle)) = self.find_view.clone().filter(|_| logged) {
                // A search hit: the log rebuilt a little past its line, the
                // match highlighted where it landed.
                let query = find::Query::parse(&needle).ok();
                if self.deep.as_ref().map_or(true, |d| d.total <= line) {
                    let dir = self.find_dir.as_deref().or(self.pane_dir.as_deref());
                    self.deep = dir.and_then(|f| Deep::open(f).ok());
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
                            query.as_ref()?.find_in(&chars).map(|(a, b)| (row, a, b))
                        })
                        .min_by_key(|&(row, _, _)| row.abs_diff(expect))
                        .map(|(row, a, b)| Highlight {
                            start: (row, a),
                            end: (row, b),
                        });
                    let highlight = log_selection(selection, d, rows, cols).or(highlight);
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
                    self.back = self.back.min(d.deepest(rows) as usize);
                    // The view's bottom is `back` lines above live, in log
                    // lines (a wrapped line counts once here, once per row in
                    // memory, so the hand-over is approximate), not counting
                    // what full-screen programs drew.
                    let end = d.line_back(self.back as u64);
                    let _ = d.rebuild(end.max(1), rows, cols, pipe);
                    let s = d.term.screen();
                    let highlight = log_selection(selection, d, rows, cols);
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
            } else {
                self.deep = None;
                let oldest = scrolled - held as u64;
                if let Some((line, _)) = &find_here {
                    // Its line a third of the way down the view.
                    let top = line.saturating_sub(rows as u64 / 3).max(oldest);
                    self.back = scrolled.saturating_sub(top) as usize;
                }
                self.back = self.back.min(held);
                let top = scrolled.saturating_sub(self.back as u64);
                let live_end = scrolled + rows as u64;
                self.folds.forget_before(oldest);
                // Live, the prompt stays at the bottom and folds let older
                // lines show above; scrolled back, the view keeps its top.
                // A search hit shows its lines unfolded.
                let folded = (!term.in_alternate_screen() && find_here.is_none())
                    .then(|| match self.back {
                        0 => self.folds.live_view(live_end, rows, oldest),
                        _ => self.folds.view(top, rows, live_end),
                    })
                    .flatten()
                    .or_else(|| {
                        // An isolated pane scrolled back: its rows composed
                        // like a view with folds, history lines from its host.
                        (remote && self.back > 0 && !term.in_alternate_screen())
                            .then(|| (top..top + rows as u64).map(fold::Row::Line).collect())
                    });
                // An isolated pane's history lines in view: ask its host for
                // those not here yet (they are drawn when they come), and let
                // go of those far from the view or no longer held. Lock
                // order here: `term` (held for the whole draw), then
                // `host_lines`, then `session`; nothing may take them in
                // the other order.
                let host_lines = folded.as_ref().filter(|_| remote).map(|view| {
                    let mut h = self.shared.host_lines.lock().unwrap();
                    let need: Vec<u64> = view
                        .iter()
                        .filter_map(|row| match *row {
                            fold::Row::Line(l) if l < scrolled => Some(l),
                            _ => None,
                        })
                        .collect();
                    if let Some(ask) = h.to_ask(need.iter().copied(), Instant::now()) {
                        let n = (ask.end - ask.start) as u32;
                        let _ = self.session.lock().unwrap().ask_history(ask.start, n);
                    }
                    // A screen either side of what is in view (a live view
                    // with a fold reaches well above `top`).
                    let (lo, hi) = match (need.first(), need.last()) {
                        (Some(&a), Some(&b)) => (a, b + 1),
                        _ => (top, top),
                    };
                    let near = lo.saturating_sub(rows as u64).max(oldest);
                    h.keep(near..hi + rows as u64);
                    h
                });
                if let Some(view) = &folded {
                    // Folds in view: compose its rows, each a line from the
                    // screen or history, or a folded command's one line.
                    let screen = term.screen();
                    let dim = ansi::Style {
                        dim: true,
                        ..Default::default()
                    };
                    let cells: Vec<Vec<ansi::Cell>> = view
                        .iter()
                        .map(|row| match *row {
                            fold::Row::Line(l) if l >= scrolled => (0..cols)
                                .map(|c| screen.cell((l - scrolled) as usize, c))
                                .collect(),
                            fold::Row::Line(l) => {
                                let mut out = Vec::new();
                                match &host_lines {
                                    Some(h) => out.extend(h.get(l).into_iter().flatten()),
                                    None => {
                                        history.history_row((scrolled - 1 - l) as usize, &mut out);
                                    }
                                }
                                out
                            }
                            fold::Row::Folded { lines, .. } => fold::summary(lines)
                                .chars()
                                .map(|ch| ansi::Cell {
                                    ch,
                                    style: dim,
                                    ..Default::default()
                                })
                                .collect(),
                        })
                        .collect();
                    let at = screen.cursor();
                    let cursor = focused
                        .then(|| {
                            view.iter()
                                .position(|&x| x == fold::Row::Line(scrolled + at.row as u64))
                        })
                        .flatten()
                        .map(|row| ansi::Cursor::new(row, at.col));
                    let highlight = selection
                        .filter(|sel| sel.log.is_none())
                        .and_then(|sel| {
                            let (a, b) = sel.ordered();
                            fold::highlight(view, cols, (a.line, a.col), (b.line, b.col))
                        })
                        .map(|(start, end)| Highlight { start, end });
                    frame.paint_at(
                        r.top,
                        r.left,
                        rows,
                        cols,
                        cursor,
                        highlight,
                        |row, c| {
                            cells
                                .get(row)
                                .and_then(|l| l.get(c))
                                .copied()
                                .unwrap_or_default()
                        },
                        pen.palette,
                        pen.atlas,
                        pen.raster,
                    );
                    // Rows that are just `top + row` (an isolated pane's
                    // view with no fold in it) keep no list: clicks then
                    // follow `back` at once, as an edge-scrolled drag needs.
                    let plain =
                        view.iter().all(|x| matches!(x, fold::Row::Line(_))) && self.back > 0;
                    self.row_lines = if plain {
                        Vec::new()
                    } else {
                        view.iter()
                            .map(|x| match *x {
                                fold::Row::Line(l) => l,
                                fold::Row::Folded { first, .. } => first,
                            })
                            .collect()
                    };
                } else {
                    self.row_lines.clear();
                    let highlight = match &find_here {
                        // The match on the hit's line.
                        Some((line, needle)) => (*line >= top && *line < top + rows as u64)
                            .then(|| Pane::line_chars(&term, *line, &HashMap::new()))
                            .flatten()
                            .and_then(|l| {
                                let chars: Vec<char> = l.into_iter().map(|(c, _)| c).collect();
                                find::Query::parse(needle).ok()?.find_in(&chars)
                            })
                            .map(|(a, b)| {
                                let row = (*line - top) as usize;
                                Highlight {
                                    start: (row, a),
                                    end: (row, b),
                                }
                            }),
                        None => selection
                            .filter(|sel| sel.log.is_none())
                            .and_then(|sel| sel.in_view(top, rows, cols))
                            .map(|(start, end)| Highlight { start, end }),
                    };
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
                if !term.in_alternate_screen() {
                    // Failed commands: `exit N` at the end of the command line.
                    let view_row = |line: u64| match &folded {
                        Some(view) => view.iter().position(|&x| x == fold::Row::Line(line)),
                        None => {
                            (line >= top && line < top + rows as u64).then(|| (line - top) as usize)
                        }
                    };
                    let blocks = shared.blocks.lock().unwrap();
                    // A live view with folds can start above `top`.
                    let first = match folded.as_ref().and_then(|v| v.first()) {
                        Some(&fold::Row::Line(l)) => l,
                        Some(&fold::Row::Folded { first, .. }) => first,
                        None => top,
                    };
                    for (line, code) in blocks.failed_in(first..live_end) {
                        let Some(row) = view_row(line) else {
                            continue;
                        };
                        let row = r.top + row;
                        let blank = |c: usize| {
                            frame
                                .cell(row, r.left + c)
                                .is_some_and(|x| x.glyph == 0 && x.bg == pen.palette.bg)
                        };
                        let Some((start, label)) = blocks::exit_label(code, cols, blank) else {
                            continue;
                        };
                        for (i, ch) in label.chars().enumerate() {
                            let key = GlyphKey {
                                ch,
                                style: 0,
                                part: GlyphPart::Single,
                            };
                            let glyph = pen.atlas.slot(key, pen.raster);
                            let cell = CellInstance {
                                glyph,
                                fg: pen.theme.error,
                                bg: pen.palette.bg,
                                flags: 0,
                            };
                            frame.put(row, r.left + start + i, cell);
                        }
                    }
                }
            }
            // The activity ribbon, in the column right of the pane's cells
            // (`place_panes` keeps it): what memory holds mapped onto the
            // rows, commands marked, failed ones in red, the view's lines on
            // the surface colour. Blank while a full-screen program draws.
            // `place_panes` reserves the column only from a pane of three,
            // so a pane of two columns or fewer kept them all and has no
            // ribbon.
            if ribbon && rows > 0 && r.cols >= 2 {
                let col = r.left + r.cols;
                let oldest = scrolled - held as u64;
                let end = scrolled + rows as u64;
                let view_top = scrolled.saturating_sub(self.back as u64);
                let cells = {
                    let blocks = shared.blocks.lock().unwrap();
                    ribbon::cells(rows, oldest, end, view_top, blocks.commands())
                };
                let alt = term.in_alternate_screen();
                for (i, c) in cells.iter().enumerate() {
                    let (ch, fg) = match (alt, c.failed, c.prompt) {
                        (true, _, _) | (false, false, false) => (' ', pen.theme.dim),
                        (false, true, _) => ('\u{2588}', pen.theme.error),
                        (false, false, true) => ('\u{2501}', pen.theme.accent),
                    };
                    let glyph = if ch == ' ' {
                        0
                    } else {
                        pen.atlas.slot(
                            GlyphKey {
                                ch,
                                style: 0,
                                part: GlyphPart::Single,
                            },
                            pen.raster,
                        )
                    };
                    let bg = if c.in_view && !alt {
                        pen.theme.surface
                    } else {
                        pen.palette.bg
                    };
                    frame.put(
                        r.top + i,
                        col,
                        CellInstance {
                            glyph,
                            fg,
                            bg,
                            flags: 0,
                        },
                    );
                }
            }
        }
    }

    /// A selection made on the screen `d` has rebuilt from the log, as that
    /// screen's highlight; `None` for any other selection.
    fn log_selection(
        selection: Option<&Selection>,
        d: &Deep,
        rows: usize,
        cols: usize,
    ) -> Option<Highlight> {
        selection
            .filter(|sel| sel.log == Some(d.at))
            .and_then(|sel| sel.in_view(0, rows, cols))
            .map(|(start, end)| Highlight { start, end })
    }

    /// A screen row as characters, each with whether it is the second half
    /// of a wide one.
    fn screen_row(s: &ansi::Screen, r: usize) -> Vec<(char, bool)> {
        let mut lead = ' ';
        (0..s.cols())
            .map(|c| {
                let cell = s.cell(r, c);
                let half = cell.width == ansi::CellWidth::Continuation;
                if !half {
                    lead = cell.ch;
                }
                (if half { lead } else { cell.ch }, half)
            })
            .collect()
    }

    /// The text of a screen from `start` to `end` (rows and columns), as a
    /// selection in the view rebuilt from the log copies it.
    fn screen_text(s: &ansi::Screen, start: Point, end: Point) -> String {
        select::text(start, end, s.cols(), |n, from, to| {
            let n = usize::try_from(n).ok().filter(|&n| n < s.rows())?;
            let chars = screen_row(s, n);
            let text = chars[from.min(chars.len())..to.min(chars.len())]
                .iter()
                .filter(|(_, half)| !half)
                .map(|(c, _)| c)
                .collect();
            Some((text, s.row_wrapped(n)))
        })
    }

    /// What drawing a cell needs.
    struct Pen<'a> {
        palette: &'a Palette,
        theme: &'a Theme,
        atlas: &'a mut Atlas,
        raster: &'a mut FontRasterizer,
    }

    /// How far the focused pane's corner brackets run along the dividers,
    /// in cells: across, and down (a cell is about twice as tall).
    const BRACKET_COLS: i64 = 4;
    const BRACKET_ROWS: i64 = 2;

    /// Whether divider cell `(row, col)` is part of the corner brackets that
    /// mark focused pane `f` (design §11 row 17): on the line around it,
    /// within a few cells of one of its corners.
    fn in_bracket(row: usize, col: usize, f: Rect) -> bool {
        let (r, c) = (row as i64, col as i64);
        let (top, bottom) = (f.top as i64 - 1, (f.top + f.rows) as i64);
        let (left, right) = (f.left as i64 - 1, (f.left + f.cols) as i64);
        let across = (r == top || r == bottom) && (left..=right).contains(&c);
        let down = (c == left || c == right) && (top..=bottom).contains(&r);
        let near_col = (c - left).abs().min((c - right).abs()) <= BRACKET_COLS;
        let near_row = (r - top).abs().min((r - bottom).abs()) <= BRACKET_ROWS;
        (across && near_col) || (down && near_row)
    }

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
            let near = in_bracket(row, col, f);
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
                    fg: if near {
                        pen.theme.accent
                    } else {
                        pen.theme.rule
                    },
                    bg: pen.palette.bg,
                    flags: 0,
                },
            );
        }
    }

    /// Feed `chunk` to a pane's emulator, and make blocks of the
    /// shell-integration marks `found` in it (stream offsets, the chunk
    /// starting at `base`): the emulator is fed up to each mark, and the line
    /// its cursor is on there is where the block boundary goes. Live output
    /// and a hidden pane's catch-up from its log both come through here.
    fn feed_marked(
        term: &mut Emu,
        shared: &Shared,
        chunk: &[u8],
        found: &[ansi::ShellMark],
        base: u64,
    ) {
        if found.is_empty() {
            term.feed(chunk);
            return;
        }
        let n = chunk.len();
        let mut blocks = shared.blocks.lock().unwrap();
        let mut at = 0;
        for m in found {
            let upto = ((m.offset.saturating_sub(base) as usize).min(n)).max(at);
            term.feed(&chunk[at..upto]);
            at = upto;
            if !term.in_alternate_screen() {
                let line = term.scrollback().scrolled_lines() + term.screen().cursor().row as u64;
                blocks.mark(m.kind, line, m.exit_code);
                if let Some(out) = &shared.marks_out {
                    out.lock()
                        .unwrap()
                        .push((m.kind.letter(), line, m.exit_code));
                }
            }
        }
        term.feed(&chunk[at..]);
        let history = term.scrollback();
        blocks.forget_before(history.scrolled_lines() - history.history_len() as u64);
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
            let mut log = log;
            if !place.is_empty() {
                let _ = resmon::cpu::place_current_thread(&place);
            }
            let mut buf = vec![0u8; 64 * 1024];
            let mut watcher = fwbench::terminal::Watcher::new();
            let mut scanner = ansi::MarkScanner::new();
            let mut alt_scanner = altscreen::Scanner::default();
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
                    let stopped = log.as_ref().is_some_and(|l| {
                        let sent = l.tx.send(LogMsg::Data(buf[..n].to_vec(), now)).is_ok();
                        !sent || l.health.failed()
                    });
                    if stopped {
                        // The log is gone: say so once, and keep the screen
                        // live, since a paused pane can no longer catch up.
                        let health = log.take().expect("stopped implies a log").health;
                        if !health.told.swap(true, Ordering::AcqRel) {
                            let error = health.error.lock().unwrap().clone();
                            let error = error.unwrap_or_else(|| "the logger stopped".into());
                            let paused = !shared.parsing.load(Ordering::Acquire);
                            term.feed(log_stopped_note(&error, paused).as_bytes());
                        }
                        shared.parsing.store(true, Ordering::Release);
                    }
                    // A shed pane's emulator waits; the log has the bytes.
                    let parse = log.is_none() || shared.parsing.load(Ordering::Acquire);
                    // Shell-integration marks: where in this chunk each falls.
                    let base = scanner.position();
                    found.clear();
                    scanner.feed(&buf[..n], &mut found);
                    let at_in_chunk =
                        |m: &ansi::ShellMark| (m.offset.saturating_sub(base) as usize).min(n);
                    if parse {
                        feed_marked(&mut term, &shared, &buf[..n], &found, base);
                    }
                    if let Some(log) = &log {
                        let chunk_at = shared.gen_bytes.load(Ordering::Acquire);
                        // Shell marks, and where a full-screen program starts
                        // and stops (what scrolling back skips), in offset
                        // order as the log takes them.
                        let mut marks: Vec<panelog::Mark> = found
                            .iter()
                            .map(|m| panelog::Mark {
                                offset: chunk_at + at_in_chunk(m) as u64,
                                kind: m.kind.letter(),
                                value: m.exit_code.map_or(-1, i64::from),
                                time_ns: now,
                            })
                            .collect();
                        marks.extend(alt_scanner.feed(&buf[..n]).into_iter().map(|(at, enter)| {
                            panelog::Mark {
                                offset: chunk_at + at as u64,
                                kind: if enter {
                                    altscreen::ENTER
                                } else {
                                    altscreen::LEAVE
                                },
                                value: 0,
                                time_ns: now,
                            }
                        }));
                        marks.sort_by_key(|m| m.offset);
                        for m in marks {
                            let _ = log.tx.send(LogMsg::Mark(m));
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
        /// Reads pane hosts' blocks for it.
        fetch: Option<Fetch>,
        waker: Waker,
        activity: Arc<Activity>,
        /// The pane hosts' CPU time, counted as fwterm's own by the governor.
        hosts: Arc<Mutex<governor::Family>>,
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
                self.fetch.as_ref(),
                &self.hosts,
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
                    t.set_history_bytes(governor::machine_history_budget());
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
                host_lines: Mutex::new(hostlines::HostLines::default()),
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
                    let sign = self.gpu.clone().map_or(SignBy::Itself, SignBy::Gpu);
                    let health = Arc::new(LogHealth::default());
                    match start_logger(dir.clone(), id, sign, health.clone()) {
                        Ok(tx) => (
                            Some(LogLink {
                                tx,
                                gen_limit: store.gen_limit.clone(),
                                nudge: store.nudge.clone(),
                                health,
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
            // An isolated pane scrolls back through its log; without logs a
            // pane stays in this process, where its history is kept.
            if args.isolate && self.logs.store.is_some() {
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
                    self.fetch.as_ref(),
                    &self.hosts,
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
                    find_dir: None,
                    folds: fold::Folds::default(),
                    row_lines: Vec::new(),
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
                find_dir: None,
                folds: fold::Folds::default(),
                row_lines: Vec::new(),
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
        fetch: Option<&Fetch>,
        hosts: &Mutex<governor::Family>,
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
        // Blocks of its log come here to be signed on the GPU.
        let fetch = fetch.filter(|_| pane_dir.is_some()).cloned();
        if fetch.is_some() {
            cmd.arg("--host-window-signs");
        }
        cmd.arg("--").args(&args.shell);
        cmd.env("FWTERM_BANNER", format!("{}{note}", logs.banner()));
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
        // Every host is in one job that closes with this process: if the
        // window dies, however it dies, its hosts and their programs go at
        // once instead of flooding on until they notice (design §4, crash).
        // A normal close still lets each host shut down first (`kill`).
        static HOSTS: std::sync::OnceLock<Option<resmon::job::KillOnClose>> =
            std::sync::OnceLock::new();
        if let Some(job) = HOSTS.get_or_init(|| resmon::job::KillOnClose::new().ok()) {
            let _ = job.assign(&child);
        }
        if let Ok(clock) = resmon::cpu::ProcessClock::of(&child) {
            hosts.lock().unwrap().add(clock);
        }
        let mut out = child.stdout.take().expect("piped");
        let stdin = child.stdin.take();
        let session = Arc::new(Mutex::new(Session::Host { child, stdin }));
        let (shared, activity) = (shared.clone(), activity.clone());
        let pane_dir = pane_dir.map(Path::to_path_buf);
        // A signature goes back to this host while it runs.
        let reply: SigReply = {
            let s = Arc::downgrade(&session);
            Arc::new(move |g, b, sig| {
                if let Some(s) = s.upgrade() {
                    s.lock().unwrap().sign(g, b, sig);
                }
            })
        };
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
                    Ok(wire::FromHost::History {
                        first,
                        lines,
                        wrapped,
                    }) => {
                        // A copy waiting for exactly these lines gets them;
                        // any other answer is a view's, drawn from here.
                        let mut waiting = shared.history_reply.lock().unwrap();
                        if waiting
                            .as_ref()
                            .is_some_and(|(f, n, _)| *f == first && *n == lines.len())
                        {
                            if let Some((_, _, tx)) = waiting.take() {
                                let _ = tx.send((first, lines, wrapped));
                            }
                            continue;
                        }
                        drop(waiting);
                        shared.host_lines.lock().unwrap().store(first, lines);
                    }
                    Ok(wire::FromHost::BlockReady {
                        generation,
                        dir,
                        block,
                    }) => {
                        // A generation folder in this pane's folder, by name.
                        let named =
                            logstore::gen_stamp(&dir).is_some() && !dir.contains(['/', '\\', ':']);
                        if let (Some(fetch), Some(pane_dir), true) = (&fetch, &pane_dir, named) {
                            let _ = fetch.send(indexer::Remote {
                                reply: reply.clone(),
                                dir: pane_dir.join(&dir),
                                generation,
                                block,
                            });
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
        Ok(session)
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
            // In a job that closes with this process: a crash here (a pane
            // host's, with --isolate) cannot leave the program running.
            let job = resmon::job::KillOnClose::new()
                .ok()
                .filter(|j| j.assign(&child).is_ok());
            Arc::new(Mutex::new(Session::Pipe {
                child,
                stdin,
                _job: job,
            }))
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

    /// fwterm's CPU as the pressure watch last read it: a share of the
    /// machine in millionths; `NO_READING` before the first.
    const NO_READING: u64 = u64::MAX;

    /// Watch how busy other programs keep the machine; the flag is true while
    /// it is [`Level::Busy`]. fwterm's own time is the window's plus its pane
    /// hosts' (`hosts`); its share goes to `cpu`, and `wake` (the instrument
    /// strip's window) is woken when the strip's reading of it changes.
    fn start_pressure_watch(
        hosts: Arc<Mutex<governor::Family>>,
        cpu: Arc<AtomicU64>,
        wake: Option<fwrender::win::Waker>,
    ) -> Arc<AtomicBool> {
        let busy = Arc::new(AtomicBool::new(false));
        let flag = busy.clone();
        std::thread::spawn(move || {
            let own = || {
                let mut t = resmon::cpu::times().ok()?;
                t.process += hosts.lock().unwrap().cpu_ns();
                Some(t)
            };
            let Some(mut before) = own() else {
                return;
            };
            let mut level = Level::Calm;
            loop {
                std::thread::sleep(PRESSURE_EVERY);
                let Some(now) = own() else {
                    return;
                };
                let load = resmon::cpu::load(&before, &now);
                level = governor::next(level, load);
                flag.store(level == Level::Busy, Ordering::Release);
                let share = (load.process * 1e6) as u64;
                let old = cpu.swap(share, Ordering::AcqRel);
                // The strip shows tenths of a percent: wake it only for those.
                if old == NO_READING || old / 1000 != share / 1000 {
                    if let Some(w) = &wake {
                        w.wake();
                    }
                }
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
        if args.licenses {
            // What fwterm ships and their licenses, compiled in with it.
            say!("{}", include_str!("../../../THIRD-PARTY-NOTICES.md"));
            return 0;
        }
        if args.choose_log_drive {
            return choose_log_drive(&args);
        }
        if args.uninstall {
            return uninstall(&args);
        }
        let setup = std::env::current_exe()
            .ok()
            .is_some_and(|e| install::is_setup(&e));
        if args.install || setup {
            return install_self(&args);
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

    /// Microsoft's ConPTY files as `tools/package.py` builds them in (empty
    /// in other builds: an install then takes them from beside fwterm.exe).
    const CARRIED: [(&str, &[u8]); 2] = [
        (
            "conpty.dll",
            include_bytes!(concat!(env!("OUT_DIR"), "/conpty.dll")),
        ),
        (
            "OpenConsole.exe",
            include_bytes!(concat!(env!("OUT_DIR"), "/OpenConsole.exe")),
        ),
    ];

    /// Where this user's install goes, from the environment.
    fn install_places() -> Option<install::Places> {
        let var = |k| std::env::var_os(k).map(PathBuf::from);
        Some(install::places(
            &var("LOCALAPPDATA")?,
            &var("APPDATA")?,
            &var("USERPROFILE")?,
        ))
    }

    /// `reg.exe` from the system folder, with `args`; its error text if it
    /// fails.
    fn reg(args: &[&std::ffi::OsStr]) -> Result<(), String> {
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        let out = std::process::Command::new(Path::new(&system).join("System32\\reg.exe"))
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// Change the user's own PATH (not the machine's) with `change`, which
    /// returns the new value or `None` to leave it; then tell running
    /// programs, so terminals opened from now on find `fwterm`. The value
    /// goes out and back through `.reg` files in UTF-16, so no other entry
    /// changes, and keeps its type. True if it changed.
    fn change_user_path(change: impl Fn(&str) -> Option<String>) -> Result<bool, String> {
        let file = std::env::temp_dir().join(format!("foldwave-env-{}.reg", std::process::id()));
        let exported = reg(&[
            "export".as_ref(),
            r"HKCU\Environment".as_ref(),
            file.as_os_str(),
            "/y".as_ref(),
        ])
        .and_then(|_| std::fs::read(&file).map_err(|e| e.to_string()));
        let _ = std::fs::remove_file(&file);
        let (expand, old) = install::reg_value(&install::reg_text(&exported?), "Path")
            .unwrap_or((true, String::new()));
        let Some(new) = change(&old) else {
            return Ok(false);
        };
        let written = std::fs::write(
            &file,
            install::reg_bytes(&install::environment_entry("Path", expand, &new)),
        )
        .map_err(|e| e.to_string())
        .and_then(|_| reg(&["import".as_ref(), file.as_os_str()]));
        let _ = std::fs::remove_file(&file);
        written?;
        fwrender::win::announce_environment_change();
        Ok(true)
    }

    /// `--install`, or started as `foldwave-setup-*.exe`: install for this
    /// user, list it in Apps & features, and start the installed fwterm
    /// (its first run, on a new machine). A failure is shown in a window.
    fn install_self(args: &Args) -> i32 {
        let tell = |title: &str, lines: &[String]| {
            for l in lines {
                say!("{l}");
            }
            if !args.quiet {
                if let Err(e) = notice(title, lines) {
                    say!("fwterm: {e}");
                }
            }
        };
        let (Some(p), Ok(me)) = (install_places(), std::env::current_exe()) else {
            tell(
                "SETUP",
                &[
                    "foldwave could not be installed: LOCALAPPDATA, APPDATA or USERPROFILE \
                   is not set."
                        .into(),
                ],
            );
            return 1;
        };
        let version = env!("CARGO_PKG_VERSION");
        let bytes = match install::install_files(&p, &me, &CARRIED, logstore::now_ms()) {
            Ok(b) => b,
            Err(e) => {
                tell(
                    "SETUP",
                    &[
                        format!("foldwave could not be installed in {}.", p.dir.display()),
                        e.to_string(),
                    ],
                );
                return 1;
            }
        };
        // Apps & features, through a .reg file imported by reg.exe.
        let file = std::env::temp_dir().join(format!("foldwave-{}.reg", std::process::id()));
        let listed = std::fs::write(
            &file,
            install::reg_bytes(&install::uninstall_entry(&p, version, bytes / 1024)),
        )
        .map_err(|e| e.to_string())
        .and_then(|_| reg(&["import".as_ref(), file.as_os_str()]));
        let _ = std::fs::remove_file(&file);
        say!(
            "foldwave {version} installed in {}; Start menu: foldwave",
            p.dir.display()
        );
        if let Err(e) = listed {
            tell(
                "SETUP",
                &[
                    format!(
                        "foldwave is installed in {}, but Apps & features could not list it, \
                         so uninstall it with fwterm --uninstall.",
                        p.dir.display()
                    ),
                    e,
                ],
            );
        }
        // `fwterm` by name in any terminal opened from now on.
        let dir = p.dir.display().to_string();
        if let Err(e) = change_user_path(|path| install::with_dir(path, &dir)) {
            tell(
                "SETUP",
                &[
                    format!(
                        "foldwave is installed, but {dir} could not be added to your PATH; \
                         start it from the Start menu or by its full path."
                    ),
                    e,
                ],
            );
        }
        if !args.quiet {
            let started = std::process::Command::new(&p.exe)
                .current_dir(&p.home)
                .spawn();
            if let Err(e) = started {
                tell(
                    "SETUP",
                    &[
                        format!(
                            "foldwave is installed, but {} did not start.",
                            p.exe.display()
                        ),
                        e.to_string(),
                    ],
                );
                return 1;
            }
        }
        0
    }

    /// `--uninstall` (Apps & features runs it): the entry, the shortcut and
    /// the install folder's own files go; settings and logs stay. Run from
    /// the install folder, fwterm.exe is removed just after it exits.
    fn uninstall(args: &Args) -> i32 {
        let (Some(p), Ok(me)) = (install_places(), std::env::current_exe()) else {
            say!("fwterm: LOCALAPPDATA, APPDATA or USERPROFILE is not set");
            return 1;
        };
        let key: &std::ffi::OsStr = install::UNINSTALL_KEY.as_ref();
        // Not listed (an install that never was) is fine.
        let _ = reg(&["delete".as_ref(), key, "/f".as_ref()]);
        let dir = p.dir.display().to_string();
        if let Err(e) = change_user_path(|path| install::without_dir(path, &dir)) {
            say!("fwterm: {dir} stays in your PATH: {e}");
        }
        let left = install::remove_files(&p, &me);
        let (running, busy): (Vec<PathBuf>, Vec<PathBuf>) = left
            .into_iter()
            .partition(|f| std::fs::canonicalize(f).ok() == std::fs::canonicalize(&me).ok());
        let (app, conf) = settings();
        let kept = format!(
            "Your settings ({}) and pane logs ({}) are kept; delete them by hand to remove \
             them too.",
            app.as_deref()
                .map(|a| logstore::settings_path(a).display().to_string())
                .unwrap_or_default(),
            conf.log_dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| "%LOCALAPPDATA%\\foldwave\\logs".into()),
        );
        let lines = if busy.is_empty() {
            vec!["foldwave is uninstalled.".to_string(), kept]
        } else {
            vec![
                "Some of foldwave's files are in use. Close every foldwave window, then \
                 uninstall again."
                    .to_string(),
                busy.iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
                kept,
            ]
        };
        for l in &lines {
            say!("{l}");
        }
        if !args.quiet {
            if let Err(e) = notice("UNINSTALL", &lines) {
                say!("fwterm: {e}");
            }
        }
        if !running.is_empty() {
            // This program's own file: removed once it has exited, with the
            // folder if nothing else is left in it.
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let script = format!(
                "ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
                p.exe.display(),
                p.dir.display()
            );
            let system = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
            let started = std::process::Command::new(Path::new(&system).join("System32\\cmd.exe"))
                .args(["/d", "/c"])
                .raw_arg(&script)
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
            if let Err(e) = started {
                say!("fwterm: {} stays: {e}", p.exe.display());
            }
        }
        i32::from(!busy.is_empty())
    }

    /// A message in a small window of its own, Mirai-styled like the rest,
    /// until Enter, Esc or the window is closed.
    fn notice(title: &str, lines: &[String]) -> io::Result<()> {
        let mut raster = FontRasterizer::from_family(&fwrender::builtin_default(), 16.0)?;
        let m = raster.metrics();
        let window = Window::new("foldwave", 900, 330)?;
        let (w, h) = window.client_size();
        let mut atlas = Atlas::new(m, 1024, 1024);
        let mut surface = Surface::Cpu {
            window,
            cpu: CpuRenderer::new(w, h),
            presenter: CpuPresenter::new(),
        };
        let theme = Theme::mirai();
        let icons: Vec<(u32, Vec<u8>)> = icon::sizes_for(surface.window().dpi())
            .iter()
            .map(|&s| (s, icon::reticle(s)))
            .collect();
        let _ = surface.window().set_icon(&icons);
        if let Some(bar) = theme.title_bar {
            surface
                .window()
                .set_frame_colors(bar, theme.palette.fg, theme.rule);
        }
        let grid = |w: u32, h: u32| {
            (
                (h / m.cell_h).max(1) as usize,
                (w / m.cell_w).max(1) as usize,
            )
        };
        let (mut rows, mut cols) = grid(w, h);
        let mut frame = Frame::new();
        let mut events = Vec::new();
        loop {
            frame.set_grid(rows, cols, &theme.palette);
            let cells =
                install::notice(title, lines, cols.saturating_sub(2), rows.saturating_sub(1));
            let mut pen = Pen {
                palette: &theme.palette,
                theme: &theme,
                atlas: &mut atlas,
                raster: &mut raster,
            };
            draw_sheet(&cells, 0, 1, &mut frame, &mut pen);
            if frame.damaged_rows() > 0 {
                surface.draw(&frame, &mut atlas)?;
                frame.clear_damage();
            }
            events.clear();
            surface.window().wait(None, &mut events);
            for e in &events {
                match *e {
                    Event::Char('\r') | Event::Char('\u{1b}') | Event::CloseRequested => {
                        return Ok(())
                    }
                    Event::Resized(w, h) if w > 0 && h > 0 => {
                        surface.resize(w, h)?;
                        (rows, cols) = grid(w, h);
                        frame.damage_all();
                    }
                    Event::Paint => frame.damage_all(),
                    _ => {}
                }
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
        wake: &Wake,
    ) -> io::Result<()> {
        let current = || {
            logstore::generations(pane_dir)
                .pop()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no log generation"))
        };
        let mut buf = vec![0u8; CATCH_UP_CHUNK];
        // The commands run while hidden become blocks as they are replayed.
        let mut scanner = ansi::MarkScanner::new();
        let mut found = Vec::new();
        let mut feed = |term: &mut Emu, bytes: &[u8]| {
            let base = scanner.position();
            found.clear();
            scanner.feed(bytes, &mut found);
            feed_marked(term, shared, bytes, &found, base);
        };
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
                feed(&mut shared.term.lock().unwrap(), &buf[..n]);
                at += n as u64;
                shared.paused_at.store(at, Ordering::Release);
                shared.pending.store(true, Ordering::Release);
                wake();
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
                feed(&mut term, &buf[..n]);
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
        // The font: --font / --px, else the settings file's, else JetBrains
        // Mono at 16 px.
        let conf = settings().1;
        let px = args.px.or(conf.px.map(|p| p as f32)).unwrap_or(16.0);
        let font = args.font.clone().or(conf.font.clone());
        let mut raster = match &font {
            // JetBrains Mono, compiled in: no font folder to scan.
            None => FontRasterizer::from_family(&fwrender::builtin_default(), px)?,
            Some(name) => {
                let catalog = fwrender::FontCatalog::system();
                let family = catalog.family(name).map_err(|e| {
                    let known = catalog.monospace_families().join(", ");
                    io::Error::new(e.kind(), format!("{e}; monospaced fonts found: {known}"))
                })?;
                FontRasterizer::from_family(&family, px)?
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

        let mut gpu_faults = GpuFaults::default();
        // foldwave's icon, drawn at the sizes a window icon may need.
        let icons: Vec<(u32, Vec<u8>)> = icon::sizes_for(surface.window().dpi())
            .iter()
            .map(|&s| (s, icon::reticle(s)))
            .collect();
        if let Err(e) = surface.window().set_icon(&icons) {
            say!("fwterm: could not set the window's icon: {e}");
        }
        // The first run: asked for, or no settings file yet (not for
        // --pipe or --stats, which tests and benchmarks use).
        let fresh = logstore::app_dir(std::env::var_os("LOCALAPPDATA"))
            .is_some_and(|a| !logstore::settings_path(&a).exists());
        if args.first_run || (fresh && !args.pipe && args.stats.is_none()) {
            let (s, end) = ask_first_run(surface, &mut atlas, &mut raster, &mut gpu_faults)?;
            surface = s;
            if let FirstRunEnd::Closed = end {
                return Ok(0);
            }
        }
        let (w, h) = surface.window().client_size();
        // Each pane runs the program after `--`, else the settings file's
        // `shell =`, else cmd.exe.
        let resolved;
        let args = match settings().1.shell.as_deref().map(first_run::split_command) {
            Some(shell) if !args.shell_set && !shell.is_empty() => {
                resolved = Args {
                    shell,
                    ..args.clone()
                };
                &resolved
            }
            _ => args,
        };
        // A test hook: FOLDWAVE_TEST_GPU_LOSS=<seconds>[,<seconds>...] takes
        // the first Vulkan frame drawn after each of those times as a lost
        // device.
        let started_at = Instant::now();
        let mut gpu_losses: Vec<Instant> = std::env::var("FOLDWAVE_TEST_GPU_LOSS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|s| s.trim().parse::<f64>().ok())
            .map(|s| started_at + Duration::from_secs_f64(s))
            .rev()
            .collect();

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
        let hosts = Arc::new(Mutex::new(governor::Family::default()));
        // The instrument strip takes the grid's top row (design §11 row 17).
        let mut strip_rows = usize::from(settings().1.instruments.unwrap_or(true));
        let mut ribbon = settings().1.ribbon.unwrap_or(true);
        let cpu_share = Arc::new(AtomicU64::new(NO_READING));
        let busy = if args.governor || strip_rows > 0 {
            let wake = (strip_rows > 0).then(|| surface.window().waker());
            start_pressure_watch(hosts.clone(), cpu_share.clone(), wake)
        } else {
            Arc::new(AtomicBool::new(false))
        };
        // The GPU indexer signs this process's panes' logs, and pane hosts'
        // only with --host-signs gpu: isolated panes (the default) sign their
        // own, and an idle CUDA context costs the window ~170 MiB.
        let signs_here = !args.isolate || args.host_signs_gpu;
        let gpu = (args.gpu_index && logs.store.is_some() && signs_here)
            .then(|| indexer::Indexer::start(|r: &SigReply, g, b, sig| r(g, b, sig)))
            .flatten();
        let index_counts = gpu.as_ref().map(|g| g.counts.clone());
        let spawner = Spawner {
            args,
            logs: &logs,
            place: place.clone(),
            fetch: gpu
                .as_ref()
                .filter(|_| args.host_signs_gpu)
                .map(|g| g.fetcher(|r: &SigReply, g, b, sig| r(g, b, sig))),
            gpu,
            waker: surface.window().waker(),
            activity: activity.clone(),
            hosts,
        };
        let ids: Vec<PaneId> = (0..args.panes).collect();
        let mut layout = Layout::grid(&ids);
        let mut placement = place_panes(&layout, None, grid_rows, grid_cols, strip_rows, ribbon);
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
        // A paste that would run several lines, waiting for Enter or Esc
        // (design §11 row 14; `paste_warning = off` in the settings file
        // pastes at once).
        let mut paste_warning = settings().1.paste_warning.unwrap_or(true);
        // The settings sheet, while open (Ctrl+,).
        let mut sheet: Option<settings_ui::Form> = None;
        // The help sheet (F1, or ? HELP in the strip); never open with the
        // settings sheet.
        let mut help_sheet: Option<help::Sheet> = None;
        // Decisions agents in the panes wait on (atrium's bus, design §8.7),
        // the ones already looked at with Ctrl+Shift+J, and each pane's root
        // process for the watcher to place atrium's under.
        let pane_roots: Arc<Mutex<Vec<(PaneId, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let decisions_rx = {
            let waker = surface.window().waker();
            agents::watch(pane_roots.clone(), move || waker.wake())
        };
        let mut report = agents::Report::default();
        let mut looked_at: HashSet<(u32, u64, u64)> = HashSet::new();
        let mut roots_checked = Instant::now() - agents::EVERY;
        let mut paste_ask: Option<(PaneId, String, usize)> = None;
        // The instrument strip's last line, when it next reads, and the log
        // drive's free space (read every 5 s).
        let mut strip_line: Vec<(char, strip::Kind)> = Vec::new();
        let mut strip_next = Instant::now();
        #[allow(clippy::type_complexity)]
        let mut log_free: Option<(Instant, Option<(String, u64)>)> = None;
        // Every pane starts at its real size; a zoom then hides the rest.
        if zoom.is_some() {
            placement = place_panes(&layout, zoom, grid_rows, grid_cols, strip_rows, ribbon);
            place_all(&mut panes, &placement);
        }

        // This thread draws: it must win a core when a frame is due, even with
        // every core busy moving output.
        fwrender::win::raise_thread_priority();
        let hz = args.max_fps.or_else(display_refresh_hz).unwrap_or(60);
        let min_interval = Duration::from_secs_f64(1.0 / hz as f64);
        // The look: --theme, else the settings file's, else Mirai; then the
        // file's colour overrides.
        let conf = settings().1;
        let name = args.theme.clone().or(conf.theme.clone());
        let mut theme_name = name.clone().unwrap_or_else(|| "mirai".into());
        let mut theme = name
            .as_deref()
            .map_or(Some(Theme::mirai()), Theme::named)
            .unwrap_or_else(|| {
                say!("fwterm: unknown theme {name:?} in the settings file; using mirai");
                Theme::mirai()
            });
        let bad = theme.apply(&conf.colors);
        if !bad.is_empty() {
            say!(
                "fwterm: colours not understood in the settings file: {}",
                bad.join(", ")
            );
        }
        let mut palette = theme.palette.clone();
        // The title bar in the theme's colours (Windows 11).
        if let Some(bar) = theme.title_bar {
            surface
                .window()
                .set_frame_colors(bar, theme.palette.fg, theme.rule);
        }
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
        // A button held down over a pane whose program takes the mouse: the
        // drag and release go to it too, wherever the pointer moves.
        let mut mouse_to: Option<PaneId> = None;
        // A drag held past a pane's top or bottom: the pointer (client
        // pixels), and when the view last scrolled for it.
        let mut drag_edge: Option<(i32, i32)> = None;
        let mut edge_scrolled = Instant::now();
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
            let level = if args.governor && busy.load(Ordering::Acquire) {
                Level::Busy
            } else {
                Level::Calm
            };
            let interval = governor::frame_interval(min_interval, level, last_input.elapsed());
            let timeout = match last_draw {
                Some(t) if due => Some(interval.saturating_sub(t.elapsed())),
                _ => None,
            };
            // A drag held past the edge scrolls on a tick, moving or not.
            let timeout = match (drag_edge, timeout) {
                (Some(_), Some(t)) => Some(t.min(EDGE_SCROLL_EVERY)),
                (Some(_), None) => Some(EDGE_SCROLL_EVERY),
                (None, t) => t,
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
                // The settings sheet takes every key and click while open.
                if let Some(form) = sheet.as_mut() {
                    use settings_ui::Key as K;
                    let key = match *e {
                        Event::Char(_) if drop_char => {
                            drop_char = false;
                            continue;
                        }
                        Event::Char('\r') => K::Enter,
                        Event::Char('\u{1b}') => K::Esc,
                        Event::Char('\u{8}') => K::Backspace,
                        // Ctrl+S.
                        Event::Char('\u{13}') => K::Save,
                        Event::Char(c) => K::Char(c),
                        Event::Key {
                            vk: 0x26,
                            down: true,
                        } => K::Up,
                        Event::Key {
                            vk: 0x28,
                            down: true,
                        } => K::Down,
                        Event::Key {
                            vk: 0x25,
                            down: true,
                        } => K::Left,
                        Event::Key {
                            vk: 0x27,
                            down: true,
                        } => K::Right,
                        Event::Key { .. }
                        | Event::SysKey { .. }
                        | Event::Click(..)
                        | Event::DoubleClick(..)
                        | Event::Drag(..)
                        | Event::Release(..)
                        | Event::Wheel(_) => continue,
                        _ => K::Char('\u{0}'),
                    };
                    if key != K::Char('\u{0}') {
                        match form.key(key) {
                            settings_ui::Action::None => {}
                            settings_ui::Action::Close => sheet = None,
                            settings_ui::Action::Save(values) => {
                                // Into the settings file, other lines kept;
                                // then what applies at once, applied.
                                let (app, _) = settings();
                                let saved = app.as_deref().map(|a| {
                                    let path = logstore::settings_path(a);
                                    let old = std::fs::read_to_string(&path).unwrap_or_default();
                                    std::fs::create_dir_all(a)
                                        .and_then(|_| {
                                            std::fs::write(
                                                &path,
                                                settings_ui::merged(&old, &values),
                                            )
                                        })
                                        .map(|_| path)
                                });
                                match saved {
                                    Some(Err(e)) => {
                                        say!("fwterm: could not save the settings: {e}")
                                    }
                                    None => {
                                        say!("fwterm: no LOCALAPPDATA: the settings were not saved")
                                    }
                                    Some(Ok(_)) => {}
                                }
                                let conf = settings().1;
                                if let Some(t) = conf.theme.as_deref().and_then(Theme::named) {
                                    theme = t;
                                    theme_name = conf.theme.clone().unwrap_or_default();
                                    theme.apply(&conf.colors);
                                    palette = theme.palette.clone();
                                    match theme.title_bar {
                                        Some(bar) => {
                                            surface.window().set_frame_colors(
                                                bar,
                                                theme.palette.fg,
                                                theme.rule,
                                            );
                                        }
                                        None => {
                                            surface.window().reset_frame_colors();
                                        }
                                    }
                                }
                                paste_warning = conf.paste_warning.unwrap_or(true);
                                let rows = usize::from(conf.instruments.unwrap_or(true));
                                let rib = conf.ribbon.unwrap_or(true);
                                if rows != strip_rows || rib != ribbon {
                                    strip_rows = rows;
                                    ribbon = rib;
                                    placement = place_panes(
                                        &layout, zoom, grid_rows, grid_cols, strip_rows, ribbon,
                                    );
                                    place_all(&mut panes, &placement);
                                }
                                frame.damage_all();
                                sheet = None;
                            }
                        }
                        layout_dirty = true;
                    }
                    continue;
                }
                // The help sheet takes every key and click while open: Esc,
                // Enter, F1 and a click close it; the arrows, the page keys
                // and the wheel scroll it.
                if let Some(h) = help_sheet.as_mut() {
                    use settings_ui::Key as K;
                    let key = match *e {
                        Event::Char(_) if drop_char => {
                            drop_char = false;
                            continue;
                        }
                        Event::Char('\r') => K::Enter,
                        Event::Char('\u{1b}') => K::Esc,
                        Event::Char(_) => continue,
                        Event::Key {
                            vk: 0x26,
                            down: true,
                        } => K::Up,
                        Event::Key {
                            vk: 0x28,
                            down: true,
                        } => K::Down,
                        Event::Key {
                            vk: 0x21,
                            down: true,
                        } => K::PageUp,
                        Event::Key {
                            vk: 0x22,
                            down: true,
                        } => K::PageDown,
                        // F1 again, or a click anywhere: closed.
                        Event::Key {
                            vk: 0x70,
                            down: true,
                        }
                        | Event::Click(..)
                        | Event::DoubleClick(..) => K::Esc,
                        Event::Wheel(delta) => {
                            let lines = (delta.unsigned_abs() as usize * 3).div_euclid(120).max(1);
                            for _ in 0..lines {
                                h.key(if delta > 0 { K::Up } else { K::Down });
                            }
                            layout_dirty = true;
                            continue;
                        }
                        Event::Key { .. }
                        | Event::SysKey { .. }
                        | Event::Drag(..)
                        | Event::Release(..) => continue,
                        _ => K::Char('\u{0}'),
                    };
                    if key != K::Char('\u{0}') {
                        if h.key(key) {
                            help_sheet = None;
                        }
                        layout_dirty = true;
                    }
                    continue;
                }
                // A paste waiting to be confirmed: Enter pastes it into the
                // pane it was meant for, Esc drops it; other keys wait.
                if paste_ask.is_some() {
                    match *e {
                        Event::Char(_) if drop_char => {
                            drop_char = false;
                            continue;
                        }
                        Event::Char('\r') => {
                            if let Some((id, text, _)) = paste_ask.take() {
                                if let Some(p) = panes.iter_mut().find(|p| p.id == id) {
                                    let bracketed = p.shared.term.lock().unwrap().bracketed_paste();
                                    let bytes = select::paste_bytes(&text, bracketed);
                                    let _ = p.session.lock().unwrap().write(&bytes);
                                    p.back = 0;
                                }
                            }
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char('\u{1b}') => {
                            paste_ask = None;
                            layout_dirty = true;
                            continue;
                        }
                        Event::Char(_) | Event::Key { .. } | Event::SysKey { .. } => continue,
                        _ => {}
                    }
                }
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
                                f.searched = f.query.clone();
                                f.hits.clear();
                                f.at = 0;
                                f.note = None;
                                let query = match find::Query::parse(&f.query) {
                                    Ok(q) => q,
                                    Err(e) => {
                                        f.error = Some(format!("bad pattern: {e}"));
                                        layout_dirty = true;
                                        continue;
                                    }
                                };
                                f.error = None;
                                // Every pane this session logged, closed
                                // ones too; earlier sessions' panes are
                                // listed on the search thread.
                                let (jobs, own): (Vec<(PaneId, PathBuf, Option<u64>)>, _) =
                                    match &logs.store {
                                        Some(store) => (
                                            store
                                                .session
                                                .pane_dirs()
                                                .into_iter()
                                                .map(|(id, d)| (id, d, None))
                                                .collect(),
                                            Some(store.session.dir.clone()),
                                        ),
                                        None => (Vec::new(), None),
                                    };
                                // Panes without a log: what memory holds.
                                let started = Instant::now();
                                let memory: Vec<find::Hit> = panes
                                    .iter()
                                    .filter(|p| p.pane_dir.is_none())
                                    .flat_map(|p| {
                                        let term = p.shared.term.lock().unwrap();
                                        let scrolled = term.scrolled_lines();
                                        let held = term.scrollback().history_len() as u64;
                                        let end = scrolled + term.screen().rows() as u64;
                                        let none = HashMap::new();
                                        let lines = (scrolled - held..end).filter_map(|n| {
                                            let l = Pane::line_chars(&term, n, &none)?;
                                            Some((n, l.into_iter().map(|(c, _)| c).collect()))
                                        });
                                        find::search_lines(p.id, lines, &query, FIND_LIMIT)
                                    })
                                    .collect();
                                let (tx, rx) = std::sync::mpsc::channel();
                                let w = waker.clone();
                                std::thread::spawn(move || {
                                    let deadline = started + REGEX_SCAN;
                                    // Then every earlier session's panes, in
                                    // the same log root (design §11 row 13).
                                    let mut jobs = jobs;
                                    if let Some(own) = &own {
                                        let root = own.parent().unwrap_or(own);
                                        for (stamp, dirs) in logstore::earlier_sessions(root, own) {
                                            jobs.extend(
                                                dirs.into_iter()
                                                    .map(|(id, d)| (id, d, Some(stamp))),
                                            );
                                        }
                                    }
                                    // Panes searched side by side, a worker
                                    // per core: each opens and searches its
                                    // own generations.
                                    let workers = std::thread::available_parallelism()
                                        .map_or(4, |n| n.get())
                                        .clamp(1, jobs.len().max(1));
                                    let next = AtomicUsize::new(0);
                                    let (mut all, cut) = std::thread::scope(|sc| {
                                        let handles: Vec<_> = (0..workers)
                                            .map(|_| {
                                                let (query, jobs, next) = (&query, &jobs, &next);
                                                sc.spawn(move || {
                                                    let mut hits = Vec::new();
                                                    let mut cut = false;
                                                    while let Some((id, dir, session)) = jobs
                                                        .get(next.fetch_add(1, Ordering::Relaxed))
                                                    {
                                                        let (h, c) = search_pane(
                                                            *id, dir, query, FIND_LIMIT, deadline,
                                                        );
                                                        hits.extend(h.into_iter().map(|mut h| {
                                                            h.session = *session;
                                                            h
                                                        }));
                                                        cut |= c;
                                                    }
                                                    (hits, cut)
                                                })
                                            })
                                            .collect();
                                        let mut all: Vec<find::Hit> = Vec::new();
                                        let mut cut = false;
                                        for h in handles {
                                            let (hits, c) = h.join().unwrap_or_default();
                                            all.extend(hits);
                                            cut |= c;
                                        }
                                        (all, cut)
                                    });
                                    all.extend(memory);
                                    find::order(&mut all);
                                    let _ = tx.send((all, started.elapsed(), cut));
                                    w.wake();
                                });
                                f.running = Some(rx);
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
                                    let (bracketed, full) = {
                                        let t = panes[fi].shared.term.lock().unwrap();
                                        (t.bracketed_paste(), t.in_alternate_screen())
                                    };
                                    match select::paste_asks(&text, bracketed, full)
                                        .filter(|_| paste_warning)
                                    {
                                        Some(n) => {
                                            paste_ask = Some((panes[fi].id, text, n));
                                            layout_dirty = true;
                                        }
                                        None => input.extend_from_slice(&select::paste_bytes(
                                            &text, bracketed,
                                        )),
                                    }
                                    panes[fi].back = 0;
                                    clear(&mut selection, &panes);
                                }
                                Ok(None) => {}
                                Err(e) => say!("fwterm: paste: {e}"),
                            }
                            drop_char = true;
                            continue;
                        }
                        // Ctrl+, : the settings sheet.
                        // (Ctrl+, types no character, so none is dropped.)
                        if vk == 0xBC && ctrl && !shift {
                            help_sheet = None;
                            sheet = Some(settings_ui::Form::new(&settings().1, &theme_name));
                            layout_dirty = true;
                            continue;
                        }
                        // F1: the help sheet, the keys. (It types no
                        // character either.)
                        if vk == 0x70 && !ctrl && !shift && !fwrender::win::alt_down() {
                            help_sheet = Some(help::Sheet::new());
                            layout_dirty = true;
                            continue;
                        }
                        // Ctrl+Shift+J: show the pane of the newest thing
                        // not yet looked at (a decision waiting, an agent
                        // finished or failed; else the next one waiting),
                        // zooming to it if another pane is zoomed.
                        if vk == 0x4A && ctrl && shift {
                            let target =
                                agents::next_to_show(&report.asks, &looked_at, focus).cloned();
                            if let Some(d) = target {
                                if panes.iter().any(|p| p.id == d.pane) {
                                    focus = d.pane;
                                    if zoom.is_some() {
                                        zoom = Some(d.pane);
                                    }
                                    looked_at.insert(d.key);
                                }
                            }
                            layout_dirty = true;
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
                        // Ctrl+Shift+[ / Ctrl+Shift+]: fold that same
                        // command's output to one line, or unfold (at the
                        // live view, the latest fold; else the one at the top).
                        if (vk == 0xDB || vk == 0xDD) && ctrl && shift {
                            let p = &mut panes[fi];
                            if vk == 0xDB {
                                if let Some(lines) = p.command_output() {
                                    p.folds.fold(lines);
                                }
                            } else {
                                let top = p.seen_scrolled.saturating_sub(p.back as u64);
                                p.folds.unfold((p.back > 0).then_some(top));
                            }
                            p.shared.pending.store(true, Ordering::Release);
                            drop_char = true;
                            continue;
                        }
                        if vk == 0x4F && ctrl && shift {
                            let p = &panes[fi];
                            let top = p.seen_scrolled.saturating_sub(p.back as u64);
                            let cols = p.shared.term.lock().unwrap().screen().cols();
                            if let Some(lines) = p.command_output() {
                                let sel = Selection {
                                    anchor: Point {
                                        line: lines.start,
                                        col: 0,
                                    },
                                    head: Point {
                                        line: lines.end - 1,
                                        col: cols.saturating_sub(1),
                                    },
                                    log: None,
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
                                let up = vk == 0x26;
                                let held = pane.shared.term.lock().unwrap().history_len();
                                let in_log = pane.deep.is_some() && pane.back > held;
                                let to = (!in_log)
                                    .then(|| {
                                        let top =
                                            pane.seen_scrolled.saturating_sub(pane.back as u64);
                                        let blocks = pane.shared.blocks.lock().unwrap();
                                        if up {
                                            blocks.prev_prompt(top)
                                        } else {
                                            blocks.next_prompt(top)
                                        }
                                    })
                                    .flatten();
                                if let Some(line) = to {
                                    pane.back = pane.seen_scrolled.saturating_sub(line) as usize;
                                } else if in_log || up {
                                    // Past what memory holds: the prompts the
                                    // logs keep, shown in the view rebuilt
                                    // from them (lines line up approximately,
                                    // as scrolling back does).
                                    if !in_log {
                                        pane.deep = pane
                                            .pane_dir
                                            .as_deref()
                                            .and_then(|f| Deep::open(f).ok());
                                    }
                                    if let Some(d) = pane.deep.as_ref() {
                                        let rows = pane.rect.rows as u64;
                                        let top = d.line_back(pane.back as u64 + rows);
                                        let to = if up {
                                            d.prev_prompt(top)
                                        } else {
                                            d.next_prompt(top)
                                        };
                                        if let Some(line) = to {
                                            let back = d.back_of(line + rows);
                                            pane.back = (back as usize).max(held + 1);
                                        }
                                    }
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
                            placement = place_panes(
                                &layout, zoom, grid_rows, grid_cols, strip_rows, ribbon,
                            );
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
                                placement = place_panes(
                                    &layout, zoom, grid_rows, grid_cols, strip_rows, ribbon,
                                );
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
                                        placement = place_panes(
                                            &layout, zoom, grid_rows, grid_cols, strip_rows, ribbon,
                                        );
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
                        // The strip's buttons; the rest of the strip takes
                        // the click and does nothing with it.
                        if strip_rows > 0 && row == 0 {
                            match strip::button_at(grid_cols, col) {
                                Some(strip::Button::Settings) => {
                                    help_sheet = None;
                                    sheet =
                                        Some(settings_ui::Form::new(&settings().1, &theme_name));
                                    layout_dirty = true;
                                }
                                Some(strip::Button::Help) => {
                                    sheet = None;
                                    help_sheet = Some(help::Sheet::new());
                                    layout_dirty = true;
                                }
                                None => {}
                            }
                            continue;
                        }
                        clear(&mut selection, &panes);
                        press = None;
                        if let Some(id) = pane_at(&placement.panes, row, col) {
                            if id != focus {
                                focus = id;
                                layout_dirty = true;
                            }
                            // A program that asked for the mouse gets the
                            // press (a double-click is a second press to it).
                            if let Some(p) = panes.iter().find(|p| p.id == id) {
                                if report_mouse(
                                    p,
                                    mouse::Button::Left,
                                    mouse::Action::Press,
                                    x,
                                    y,
                                    m,
                                ) {
                                    mouse_to = Some(id);
                                    continue;
                                }
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
                        if let Some(id) = mouse_to {
                            if let Some(p) = panes.iter().find(|p| p.id == id) {
                                report_mouse(p, mouse::Button::Left, mouse::Action::Drag, x, y, m);
                            }
                            continue;
                        }
                        if let Some((id, anchor)) = press {
                            if let Some(pane) = panes.iter().find(|p| p.id == id) {
                                let row = y.div_euclid(m.cell_h as i32) - pane.rect.top as i32;
                                let out = select::edge_scroll(row, pane.rect.rows) != 0;
                                drag_edge = out.then_some((x, y));
                                if let Some(head) = pane.point(x, y, m) {
                                    if head != anchor || selection.is_some() {
                                        let log = pane.deep.as_ref().map(|d| d.at);
                                        selection = Some((id, Selection { anchor, head, log }));
                                        pane.shared.pending.store(true, Ordering::Release);
                                    }
                                }
                            }
                        }
                    }
                    Event::Release(x, y) => {
                        if let Some(id) = mouse_to.take() {
                            if let Some(p) = panes.iter().find(|p| p.id == id) {
                                report_mouse(
                                    p,
                                    mouse::Button::Left,
                                    mouse::Action::Release,
                                    x,
                                    y,
                                    m,
                                );
                            }
                        }
                        press = None;
                        drag_edge = None;
                    }
                    // The wheel over a pane whose program asked for the
                    // mouse goes to it; over a full-screen program that did
                    // not, it becomes arrow keys; else it scrolls back.
                    Event::Wheel(delta)
                        if wheel_to_program(
                            &panes,
                            &placement,
                            focus,
                            surface.window().pointer(),
                            delta,
                            m,
                        ) => {}
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
                        if let Err(e) = surface.resize(w, h) {
                            surface = after_draw_error(surface, e, &atlas, &mut gpu_faults)?;
                        }
                        (grid_rows, grid_cols) = grid(w, h);
                        placement =
                            place_panes(&layout, zoom, grid_rows, grid_cols, strip_rows, ribbon);
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
            // A selection dragged past a pane's edge: scroll and extend it,
            // in the live and memory view (the log view's rows would not
            // stay under the selection), no further back than memory holds.
            if let (Some((id, anchor)), Some((x, y))) = (press, drag_edge) {
                if edge_scrolled.elapsed() >= EDGE_SCROLL_EVERY {
                    edge_scrolled = Instant::now();
                    if let Some(p) = panes.iter_mut().find(|p| p.id == id && p.deep.is_none()) {
                        let row = y.div_euclid(m.cell_h as i32) - p.rect.top as i32;
                        let step = select::edge_scroll(row, p.rect.rows);
                        let held = p.shared.term.lock().unwrap().history_len();
                        p.back = if step < 0 {
                            (p.back + step.unsigned_abs() as usize).min(held)
                        } else {
                            p.back.saturating_sub(step as usize)
                        };
                        if let Some(head) = p.point(x, y, m) {
                            selection = Some((
                                id,
                                Selection {
                                    anchor,
                                    head,
                                    log: None,
                                },
                            ));
                        }
                        p.shared.pending.store(true, Ordering::Release);
                    }
                }
            }
            // Agents' open decisions, when they change; the panes' root
            // processes for the watcher, now and then.
            while let Ok(open) = decisions_rx.try_recv() {
                looked_at.retain(|k| open.asks.iter().any(|d| d.key == *k));
                report = open;
                layout_dirty = true;
            }
            if roots_checked.elapsed() >= agents::EVERY {
                roots_checked = Instant::now();
                let roots: Vec<(PaneId, u32)> = panes
                    .iter()
                    .map(|p| (p.id, p.session.lock().unwrap().pid()))
                    .collect();
                if let Ok(mut r) = pane_roots.lock() {
                    if *r != roots {
                        *r = roots;
                    }
                }
            }
            // Search results, when the search thread has them.
            if let Some(f) = find_bar.as_mut() {
                if let Some((hits, took, cut)) =
                    f.running.as_ref().and_then(|rx| rx.try_recv().ok())
                {
                    f.running = None;
                    f.took = Some(took);
                    f.note = cut.then(|| {
                        format!(
                            "stopped after {} s: the newest output searched first",
                            REGEX_SCAN.as_secs()
                        )
                    });
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
                placement = place_panes(&layout, zoom, grid_rows, grid_cols, strip_rows, ribbon);
                place_all(&mut panes, &placement);
            }
            // Hidden panes stop parsing when the governor says so, and catch
            // up from their logs when shown or when it no longer does.
            for p in &panes {
                // An isolated pane's host shed and catches up itself.
                if matches!(&*p.session.lock().unwrap(), Session::Host { .. }) {
                    let logged = p.pane_dir.is_some();
                    let keep = governor::keep_parsing(p.hidden, logged, args.hidden, level);
                    let parsing = p.shared.parsing.load(Ordering::Acquire);
                    if keep != parsing && p.session.lock().unwrap().shed(!keep) {
                        p.shared.parsing.store(keep, Ordering::Release);
                    }
                    continue;
                }
                let logged = p.log.as_ref().is_some_and(|l| !l.health.failed());
                let keep = governor::keep_parsing(p.hidden, logged, args.hidden, level);
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
            let relayout = layout_dirty;
            let mut pen = Pen {
                palette: &palette,
                theme: &theme,
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
                    p.draw(p.id == focus, sel, args.pipe, ribbon, &mut frame, &mut pen);
                    drew = true;
                }
            }
            if layout_dirty {
                draw_dividers(&placement, focus, &mut frame, &mut pen);
                layout_dirty = false;
            }
            // The instrument strip, over the grid's top row: its readings
            // at most twice a second, drawn when they change.
            if strip_rows > 0 && (relayout || Instant::now() >= strip_next) {
                strip_next = Instant::now() + Duration::from_millis(500);
                if log_free
                    .as_ref()
                    .map_or(true, |(at, _)| at.elapsed() >= Duration::from_secs(5))
                {
                    let vol = logs
                        .store
                        .as_ref()
                        .and_then(|st| resmon::volume::of(&st.root).ok());
                    log_free = Some((
                        Instant::now(),
                        vol.map(|v| {
                            let root = v
                                .root
                                .to_string_lossy()
                                .trim_end_matches(['\\', '/'])
                                .to_string();
                            (root, v.free)
                        }),
                    ));
                }
                let isolated: Vec<bool> = panes
                    .iter()
                    .filter(|p| matches!(&*p.session.lock().unwrap(), Session::Host { .. }))
                    .map(|p| !p.shared.host_died.load(Ordering::Acquire))
                    .collect();
                let share = cpu_share.load(Ordering::Acquire);
                let readings = strip::Readings {
                    panes: panes.len(),
                    focus: panes
                        .iter()
                        .position(|p| p.id == focus)
                        .map_or(1, |i| i + 1),
                    cpu: (share != NO_READING).then(|| share as f64 / 1e6),
                    log: log_free.as_ref().and_then(|(_, v)| v.clone()),
                    index: if index_counts.is_some() {
                        "GPU"
                    } else if logs.store.is_some() {
                        "CPU"
                    } else {
                        "OFF"
                    },
                    hosts: (!isolated.is_empty())
                        .then(|| (isolated.iter().filter(|&&up| up).count(), isolated.len())),
                    agents: agents_by_pane(&report.agents, &panes),
                    decisions: report
                        .asks
                        .iter()
                        .filter(|a| a.kind == agents::AskKind::Decision)
                        .count(),
                    ask: newest_ask(&report.asks, &looked_at, &panes),
                };
                let line = strip::line(&readings);
                if relayout || line != strip_line {
                    draw_strip(&line, grid_cols, &mut frame, &mut pen);
                    strip_line = line;
                    drew = true;
                }
            }
            // The search bar, over the window's bottom row.
            if let Some(f) = &find_bar {
                let status = match (&f.running, f.hits.len()) {
                    _ if f.searched == f.query && f.error.is_some() => {
                        f.error.clone().unwrap_or_default()
                    }
                    (Some(_), _) => "searching every log, earlier sessions' too...".to_string(),
                    (None, 0) if !f.searched.is_empty() && f.searched == f.query => {
                        let ms = f.took.map_or(0.0, |t| t.as_secs_f64() * 1e3);
                        format!("no matches ({ms:.0} ms)")
                    }
                    (None, 0) => {
                        "Enter searches every pane's log and earlier sessions', Esc closes".into()
                    }
                    (None, n) if f.searched == f.query => {
                        let h = &f.hits[f.at];
                        let ms = f.took.map_or(0.0, |t| t.as_secs_f64() * 1e3);
                        let pane = h.pane + 1;
                        let place = match h.session {
                            Some(s) => format!(
                                "pane {pane} of a session started {}",
                                find::ago(logstore::now_ms().saturating_sub(s))
                            ),
                            None if panes.iter().any(|p| p.id == h.pane) => format!("pane {pane}"),
                            None => format!("closed pane {pane}"),
                        };
                        format!("{}/{} ({ms:.0} ms) in {place}: {}", f.at + 1, n, h.text)
                    }
                    (None, _) => "Enter searches again".into(),
                };
                let status = match (&f.note, f.searched == f.query) {
                    (Some(note), true) => format!("{status}  [{note}]"),
                    _ => status,
                };
                let text = format!(" FIND › {}_   {status}", f.query);
                draw_bar(
                    &text,
                    7,
                    grid_rows.saturating_sub(1),
                    grid_cols,
                    &mut frame,
                    &mut pen,
                );
                drew = true;
            }
            // Without the strip, a decision not yet looked at shows on the
            // bottom row instead (the search and paste bars come first).
            if strip_rows == 0 && find_bar.is_none() && paste_ask.is_none() {
                if let Some(ask) = newest_ask(&report.asks, &looked_at, &panes) {
                    let text = format!(" AGENT › Ctrl+Shift+J · {ask}");
                    draw_bar(
                        &text,
                        8,
                        grid_rows.saturating_sub(1),
                        grid_cols,
                        &mut frame,
                        &mut pen,
                    );
                    drew = true;
                }
            }
            // A paste waiting to be confirmed, over the same row.
            if let Some((_, _, n)) = &paste_ask {
                let lines = if *n == 1 {
                    "1 line".to_string()
                } else {
                    format!("{n} lines")
                };
                let text = format!(
                    " PASTE › {lines}? The program runs each line as it arrives.   \
                     Enter pastes, Esc cancels"
                );
                draw_bar(
                    &text,
                    8,
                    grid_rows.saturating_sub(1),
                    grid_cols,
                    &mut frame,
                    &mut pen,
                );
                drew = true;
            }
            // The settings sheet, over everything, centred.
            if let Some(form) = &sheet {
                let w = grid_cols.saturating_sub(4).min(96);
                let h = grid_rows.saturating_sub(2).min(22);
                let cells = settings_ui::render(form, w, h);
                let (top, left) = (
                    grid_rows.saturating_sub(h) / 2,
                    grid_cols.saturating_sub(w) / 2,
                );
                draw_sheet(&cells, top, left, &mut frame, &mut pen);
                drew = true;
            }
            // The help sheet, the same way.
            if let Some(h) = &help_sheet {
                let w = grid_cols.saturating_sub(4).min(96);
                let h_rows = grid_rows.saturating_sub(2).min(22);
                let cells = h.render(w, h_rows);
                let (top, left) = (
                    grid_rows.saturating_sub(h_rows) / 2,
                    grid_cols.saturating_sub(w) / 2,
                );
                draw_sheet(&cells, top, left, &mut frame, &mut pen);
                drew = true;
            }
            if drew && frame.damaged_rows() > 0 {
                let injected = matches!(surface, Surface::Vulkan(_))
                    && gpu_losses.last().is_some_and(|&t| Instant::now() >= t);
                let drawn = if injected {
                    gpu_losses.pop();
                    Err(io::Error::other(
                        "VK_ERROR_DEVICE_LOST, injected by FOLDWAVE_TEST_GPU_LOSS",
                    ))
                } else {
                    surface.draw(&frame, &mut atlas)
                };
                if let Err(e) = drawn {
                    // The GPU fails alone: the window and every pane go on,
                    // redrawn in full on what draws now.
                    surface = after_draw_error(surface, e, &atlas, &mut gpu_faults)?;
                    frame.damage_all();
                    surface.draw(&frame, &mut atlas)?;
                }
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

        /// Search timing over a real session's logs, every pane at once as
        /// the app searches (design §4 history target). Run with
        /// `FWTERM_SEARCH_BENCH=<session folder> cargo test --release -p
        /// nativelite-fwterm --bin fwterm search_benchmark -- --ignored
        /// --nocapture`.
        #[test]
        #[ignore = "benchmark: needs FWTERM_SEARCH_BENCH, a session folder of logs"]
        fn search_benchmark() {
            let Ok(session) = std::env::var("FWTERM_SEARCH_BENCH") else {
                return;
            };
            let panes: Vec<(usize, PathBuf)> = std::fs::read_dir(&session)
                .unwrap()
                .filter_map(|e| {
                    let e = e.ok()?;
                    let id = e
                        .file_name()
                        .to_str()?
                        .strip_prefix("pane-")?
                        .parse()
                        .ok()?;
                    Some((id, e.path()))
                })
                .collect();
            let queries = [
                "zqx-marker-7",
                "absent-needle-000",
                r"/zqx-marker-\d/",
                r"/absent-needle-\d{3}/",
                r"/\d{9}x/",
            ];
            const RUNS: usize = 5;
            for q in queries {
                let query = find::Query::parse(q).unwrap();
                let mut ms = Vec::new();
                let (mut hits, mut cut) = (0, false);
                for _ in 0..RUNS {
                    let t = Instant::now();
                    let deadline = t + REGEX_SCAN;
                    let results: Vec<(Vec<find::Hit>, bool)> = std::thread::scope(|sc| {
                        let hs: Vec<_> = panes
                            .iter()
                            .map(|(id, dir)| {
                                let query = &query;
                                sc.spawn(move || search_pane(*id, dir, query, FIND_LIMIT, deadline))
                            })
                            .collect();
                        hs.into_iter().map(|h| h.join().unwrap()).collect()
                    });
                    ms.push(t.elapsed().as_secs_f64() * 1e3);
                    hits = results.iter().map(|(h, _)| h.len()).sum();
                    cut = results.iter().any(|(_, c)| *c);
                }
                ms.sort_by(f64::total_cmp);
                let p90 = ms[((ms.len() - 1) as f64 * 0.9).round() as usize];
                println!(
                    "{q:<24} median {:>8.1} ms  p90 {:>8.1}  max {:>8.1}  n={RUNS}  hits {hits}{}",
                    ms[ms.len() / 2],
                    p90,
                    ms[ms.len() - 1],
                    if cut { "  (cut at the deadline)" } else { "" }
                );
            }
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
            let later = Instant::now() + Duration::from_secs(60);
            let search = |q: &str| {
                let (mut hits, cut) =
                    search_pane(7, &pane, &find::Query::parse(q).unwrap(), 100, later);
                assert!(!cut);
                find::order(&mut hits);
                hits
            };
            // A regex with a literal (the index finds "needle"), and one
            // without (every line read): the same two hits.
            let lines = |hits: Vec<find::Hit>| hits.iter().map(|h| h.line).collect::<Vec<_>>();
            assert_eq!(lines(search(r"/needle [a-z]$/")), [4, 1]);
            assert_eq!(lines(search(r"/^l\d \w+ [ab]/")), [4, 1]);
            assert_eq!(lines(search(r"/needle c/")), Vec::<u64>::new());
            // Out of time before a no-literal regex reads anything: cut short.
            let past = Instant::now();
            let q = find::Query::parse(r"/\w+/").unwrap();
            assert!(search_pane(7, &pane, &q, 100, past).1);
            // A regex whose literal is on many lines is cut short the same way.
            let q = find::Query::parse(r"/needle [a-z]$/").unwrap();
            assert!(search_pane(7, &pane, &q, 100, past).1);
            let hits = search("needle");
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
        fn the_focused_panes_corners_are_bracketed_and_the_rest_of_its_lines_are_not() {
            // A pane at rows 5..25, columns 10..70: its lines are row 4 and
            // row 25, column 9 and column 70.
            let f = Rect {
                top: 5,
                left: 10,
                rows: 20,
                cols: 60,
            };
            assert!(in_bracket(4, 9, f), "the top-left corner");
            assert!(
                in_bracket(4, 13, f) && !in_bracket(4, 14, f),
                "4 cells along"
            );
            assert!(in_bracket(6, 9, f) && !in_bracket(7, 9, f), "2 cells down");
            assert!(
                in_bracket(25, 70, f) && in_bracket(23, 70, f),
                "the bottom-right"
            );
            assert!(!in_bracket(15, 70, f), "the middle of a side");
            assert!(!in_bracket(4, 40, f), "the middle of the top");
            assert!(!in_bracket(15, 40, f), "inside the pane");
            assert!(!in_bracket(3, 9, f), "past the corner");
        }

        #[test]
        fn the_gpu_is_rebuilt_a_few_times_a_minute_then_the_cpu_draws() {
            let t = Instant::now();
            let mut f = GpuFaults::default();
            let s = Duration::from_secs;
            assert!(f.retry(t) && f.retry(t + s(1)) && f.retry(t + s(2)));
            assert!(!f.retry(t + s(3)), "a fourth within the minute: the CPU");
            // A minute after the first three, the GPU gets another chance.
            let mut f = GpuFaults::default();
            assert!(f.retry(t) && f.retry(t + s(1)) && f.retry(t + s(2)));
            assert!(f.retry(t + s(62)), "the old failures have aged out");
        }

        #[test]
        fn a_log_of_bare_line_feeds_is_replayed_as_a_pipe_panes() {
            assert!(bare_line_feeds(b"one\ntwo\nthree\n"));
            assert!(!bare_line_feeds(b"one\r\ntwo\r\n"));
            // A console pane's output: CR LF, with cursor moves between.
            assert!(!bare_line_feeds(b"\x1b[2;1Hdir\r\n a\r\n b\n"));
            assert!(!bare_line_feeds(b"no breaks at all"));
            // A pipe pane running cmd writes CR LF too: either way is right.
            assert!(bare_line_feeds(b"a\nb\nc\r\n"));
        }

        #[test]
        fn scrolling_back_skips_what_a_full_screen_program_drew() {
            let pane = std::env::temp_dir().join(format!("fwterm-alt-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&pane);
            // The shell's L0..L9, a full-screen program's 20 lines, then the
            // shell again: L30..L34.
            let shell = lines(0..10);
            let mut prog = b"\x1b[?1049h".to_vec();
            prog.extend(lines(10..30));
            prog.extend(b"\x1b[?1049l");
            let after = lines(30..35);
            {
                let mut w = panelog::Writer::create(
                    &pane.join(logstore::gen_name(1, 0)),
                    panelog::Config::default(),
                )
                .unwrap();
                let mut scanner = altscreen::Scanner::default();
                let mut at = 0u64;
                for chunk in [&shell, &prog, &after] {
                    // The bytes first: a mark may only point into the log.
                    w.append(chunk, 0).unwrap();
                    for (off, enter) in scanner.feed(chunk) {
                        w.mark(panelog::Mark {
                            offset: at + off as u64,
                            kind: if enter {
                                altscreen::ENTER
                            } else {
                                altscreen::LEAVE
                            },
                            value: 0,
                            time_ns: 0,
                        })
                        .unwrap();
                    }
                    at += chunk.len() as u64;
                }
            }
            let mut d = Deep::open(&pane).unwrap();
            assert_eq!(d.alt.len(), 1, "one stretch: {:?}", d.alt);
            let (a, b) = d.alt[0];
            assert_eq!(b - a, 20, "the program's 20 lines");
            assert_eq!(d.visible(), d.total - 20);
            // Five lines back is still after the program; six is the shell
            // before it.
            let rebuilt_top = |d: &mut Deep, back: u64| {
                let end = d.line_back(back);
                d.rebuild(end.max(1), 4, 10, false).unwrap();
                (0..4)
                    .map(|r| {
                        (0..10)
                            .map(|c| d.term.screen().cell(r, c).ch)
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            };
            let near = rebuilt_top(&mut d, 5);
            assert!(
                near.contains("L30") && !near.contains("L29"),
                "after it: {near}"
            );
            let before = rebuilt_top(&mut d, 6);
            assert!(
                before.contains("L9") && !before.contains("L1") && !before.contains("L2"),
                "the shell before it: {before}"
            );
            for back in 0..=d.visible() {
                assert_eq!(d.back_of(d.line_back(back)), back, "round trip {back}");
            }
            std::fs::remove_dir_all(&pane).unwrap();
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
            // The screen's rows are the 4 lines before `end`: no blank row
            // at the bottom, so a view of `rows` lines ending at `end` starts
            // exactly at `end - rows` (what Ctrl+Up puts at the top).
            let at = |d: &mut Deep, end: u64| {
                d.rebuild(end, 4, 10, false).unwrap();
                (row(&d.term, 0), row(&d.term, 3))
            };
            let pair = |a: &str, b: &str| (a.to_string(), b.to_string());
            assert_eq!(
                at(&mut d, 5),
                pair("L1", "L4"),
                "inside the first generation"
            );
            assert_eq!(at(&mut d, 10), pair("L6", "L9"), "at the roll");
            assert_eq!(
                at(&mut d, 15),
                pair("L11", "L14"),
                "inside the second, from its checkpoint"
            );

            // Retention deleted the first: the second still rebuilds alone.
            logstore::delete(&pane.join(logstore::gen_name(1, 0)));
            let mut d = Deep::open(&pane).unwrap();
            assert_eq!(d.total, 11);
            assert_eq!(at(&mut d, 5).1, "L14");
            let _ = std::fs::remove_dir_all(&pane);
        }

        #[test]
        fn scrolling_back_past_the_log_stops_with_its_first_line_on_top() {
            let pane = std::env::temp_dir().join(format!("fwterm-deepest-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&pane);
            std::fs::create_dir_all(&pane).unwrap();
            let cfg = panelog::Config::default();
            {
                let mut w =
                    panelog::Writer::create(&pane.join(logstore::gen_name(1, 0)), cfg).unwrap();
                w.append(&lines(0..10), 0).unwrap();
            }
            let mut d = Deep::open(&pane).unwrap();
            let (rows, cols) = (4, 10);
            // The wheel asked for far more than the log holds: the view
            // stops where the first line is the top row and the screen is
            // still full, as memory's clamp stops at the oldest held line.
            let back = (1_000u64).min(d.deepest(rows));
            let end = d.line_back(back);
            assert_eq!(end, rows as u64, "the view ends after the first screen");
            d.rebuild(end.max(1), rows, cols, false).unwrap();
            assert_eq!(
                (0..rows).map(|r| row(&d.term, r)).collect::<Vec<_>>(),
                ["L0", "L1", "L2", "L3"]
            );
            // Capped at the whole log instead, the view ends after line 0:
            // one line and blank rows under it, the history gone.
            let end = d.line_back(d.visible());
            d.rebuild(end.max(1), rows, cols, false).unwrap();
            assert_eq!(
                (0..rows).map(|r| row(&d.term, r)).collect::<Vec<_>>(),
                ["L0", "", "", ""],
                "what the old cap showed"
            );
            // A log shorter than a screen has nowhere to scroll to.
            assert_eq!(d.deepest(100), 0);
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
            assert_eq!((a.backend.as_str(), a.px), ("cpu", Some(18.0)));
            assert_eq!(a.shell, ["pwsh", "-NoLogo"]);
            assert!(!a.pipe);
            assert!(parse(&argv(&["--pipe", "--", "x"])).unwrap().pipe);
            assert!(parse(&argv(&["--backend", "gl"])).is_err());
            assert!(parse(&argv(&["--size", "10x10"])).is_err());
            assert_eq!(parse(&argv(&[])).unwrap().shell, ["cmd.exe"]);
            let f = parse(&argv(&["--font", "Cascadia Mono"])).unwrap();
            assert_eq!(f.font.as_deref(), Some("Cascadia Mono"));
            assert!(parse(&argv(&[])).unwrap().font.is_none());
            // Pane-host flags, as `start_host` passes them.
            let h = parse(&argv(&[
                "--pane-host",
                "--host-window-signs",
                "--host-id",
                "3",
                "--",
                "cmd",
            ]))
            .unwrap();
            assert!(h.pane_host && h.host_window_signs);
            assert_eq!(
                (h.host_id, h.shell.as_slice()),
                (3, &["cmd".to_string()][..])
            );
            // Hosts sign their own logs unless the window is told to.
            assert!(!parse(&argv(&[])).unwrap().host_signs_gpu);
            assert!(
                !parse(&argv(&["--host-signs", "self"]))
                    .unwrap()
                    .host_signs_gpu
            );
            assert!(
                parse(&argv(&["--host-signs", "gpu"]))
                    .unwrap()
                    .host_signs_gpu
            );
            assert!(parse(&argv(&["--host-signs", "cpu"])).is_err());
        }

        /// A pane's shared state, paused (hidden and shed) or live.
        fn test_shared(parsing: bool) -> Arc<Shared> {
            Arc::new(Shared {
                term: Mutex::new(Emu::Local(Term::with_history(6, 100, 50))),
                pending: AtomicBool::new(false),
                exited: AtomicBool::new(false),
                blocks: Mutex::new(blocks::Blocks::default()),
                marks_out: None,
                history_reply: Mutex::new(None),
                host_lines: Mutex::new(hostlines::HostLines::default()),
                host_died: AtomicBool::new(false),
                since_checkpoint: AtomicU64::new(0),
                gen_bytes: AtomicU64::new(0),
                parsing: AtomicBool::new(parsing),
                paused_at: AtomicU64::new(0),
                catching_up: AtomicBool::new(false),
                want: AtomicBool::new(false),
            })
        }

        #[test]
        fn a_pane_knows_which_lines_wrapped_on_screen_and_in_history() {
            let mut t = Term::with_history(2, 4, 10);
            // "abcdefgh" wraps (line 0 runs on into 1), then "xy" alone.
            t.feed(b"abcdefgh\r\nxy");
            let emu = Emu::Local(t);
            // Lines: 0 "abcd" (history, wrapped), 1 "efgh", 2 "xy".
            assert_eq!(emu.scrolled_lines(), 1);
            assert!(emu.line_wrapped(0), "in history");
            assert!(!emu.line_wrapped(1) && !emu.line_wrapped(2));
            let none = HashMap::new();
            let (start, end) = (Point { line: 0, col: 0 }, Point { line: 1, col: 3 });
            let text = select::text(start, end, 4, |n, from, to| {
                let chars = Pane::line_chars(&emu, n, &none)?;
                let s = chars[from..to.min(chars.len())]
                    .iter()
                    .map(|(c, _)| c)
                    .collect();
                Some((s, emu.line_wrapped(n)))
            });
            assert_eq!(text, "abcdefgh", "copied as the one line it was");
        }

        #[test]
        fn a_selection_in_the_log_view_copies_and_shows_on_its_screen_only() {
            let mut term = Term::new(4, 12);
            term.feed("first line\r\nsecond 界 x\r\nthird".as_bytes());
            let d = Deep {
                gens: Vec::new(),
                total: 3,
                at: 3,
                term,
                bare_lf: false,
                alt: Vec::new(),
            };
            let pt = |line, col| Point { line, col };
            // From "line" on row 0 to "界" on row 1 (a wide glyph: once).
            let text = screen_text(d.term.screen(), pt(0, 6), pt(1, 8));
            assert_eq!(text, "line\r\nsecond 界");
            let sel = Selection {
                anchor: pt(1, 0),
                head: pt(0, 6),
                log: Some(3),
            };
            let hl = log_selection(Some(&sel), &d, 4, 12).unwrap();
            assert_eq!((hl.start, hl.end), ((0, 6), (1, 0)));
            // Made on another rebuilt screen, or in the live view: not here.
            let other = Selection {
                log: Some(9),
                ..sel
            };
            assert!(log_selection(Some(&other), &d, 4, 12).is_none());
            let live = Selection { log: None, ..sel };
            assert!(log_selection(Some(&live), &d, 4, 12).is_none());
        }

        #[test]
        fn prompts_are_found_in_the_logs_across_generations() {
            let root = std::env::temp_dir().join("fwterm-log-prompts");
            let _ = std::fs::remove_dir_all(&root);
            let tx = start_logger(root.clone(), 0, SignBy::Itself, Default::default()).unwrap();
            let prompt = |offset| {
                LogMsg::Mark(panelog::Mark {
                    offset,
                    kind: b'A',
                    value: -1,
                    time_ns: 1,
                })
            };
            // Generation 0: prompts on lines 0 and 2. Generation 1 begins on
            // the line generation 0 ends in (3), with a prompt there.
            tx.send(LogMsg::Data(b"p0\r\nout\r\np1\r\n".to_vec(), 1))
                .unwrap();
            tx.send(prompt(0)).unwrap();
            tx.send(prompt(9)).unwrap();
            tx.send(LogMsg::Roll(Vec::new())).unwrap();
            tx.send(LogMsg::Data(b"q0\r\nmore\r\n".to_vec(), 1))
                .unwrap();
            tx.send(prompt(0)).unwrap();
            flush_logger(&tx).unwrap();
            let d = Deep::open(&root).unwrap();
            assert_eq!(d.prev_prompt(d.total), Some(3));
            assert_eq!(d.prev_prompt(3), Some(2));
            assert_eq!(d.prev_prompt(2), Some(0));
            assert_eq!(d.prev_prompt(0), None);
            assert_eq!(d.next_prompt(0), Some(2));
            assert_eq!(d.next_prompt(2), Some(3));
            assert_eq!(d.next_prompt(3), None);
            drop((d, tx));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn commands_run_while_hidden_become_blocks_on_catch_up() {
            let root = std::env::temp_dir().join("fwterm-hidden-marks");
            let _ = std::fs::remove_dir_all(&root);
            let tx = start_logger(root.clone(), 0, SignBy::Itself, Default::default()).unwrap();
            // Two commands while hidden; the second fails (as PowerShell marks it).
            let out = b"\x1b]133;A\x07> \x1b]133;B\x07ok\r\nfine\r\n\x1b]133;D;0\x07\
                        \x1b]133;A\x07> \x1b]133;B\x07bad\r\noops\r\n\x1b]133;D;3\x07\
                        \x1b]133;A\x07> ";
            tx.send(LogMsg::Data(out.to_vec(), 1)).unwrap();
            let shared = test_shared(false);
            shared.gen_bytes.store(out.len() as u64, Ordering::Release);
            let wake: Wake = Arc::new(|| {});
            catch_up(&shared, &tx, &root, &wake).unwrap();
            assert!(shared.parsing.load(Ordering::Acquire), "live again");
            let blocks = shared.blocks.lock().unwrap();
            assert_eq!(blocks.len(), 3, "every prompt replayed became a block");
            assert_eq!(blocks.failed_in(0..100).collect::<Vec<_>>(), [(2, 3)]);
            drop(blocks);
            drop(tx);
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_pane_whose_log_stops_says_so_once_and_goes_live() {
            let root = std::env::temp_dir().join("fwterm-log-stops");
            let _ = std::fs::remove_dir_all(&root);
            let health = Arc::new(LogHealth::default());
            let tx = start_logger(root.clone(), 0, SignBy::Itself, health.clone()).unwrap();
            let log = LogLink {
                tx: tx.clone(),
                gen_limit: Arc::new(AtomicU64::new(u64::MAX)),
                nudge: std::sync::mpsc::sync_channel(1).0,
                health: health.clone(),
            };
            // A hidden, shed pane: its emulator waits on the log.
            let shared = test_shared(false);
            let activity = Arc::new(Activity {
                epoch: Instant::now(),
                first_output: AtomicU64::new(u64::MAX),
                last_output: AtomicU64::new(0),
            });
            let (chunks, rx) = std::sync::mpsc::channel::<Vec<u8>>();
            let source = Box::new(move |b: &mut [u8]| match rx.recv() {
                Ok(c) => {
                    b[..c.len()].copy_from_slice(&c);
                    Ok(c.len())
                }
                Err(_) => Ok(0),
            });
            let wake: Wake = Arc::new(|| {});
            pump(
                source,
                None,
                true,
                shared.clone(),
                activity,
                wake,
                Some(log),
                Arc::new(Vec::new()),
            );
            let text = |shared: &Shared| -> String {
                let term = shared.term.lock().unwrap();
                let s = term.screen();
                (0..s.rows())
                    .map(|r| (0..s.cols()).map(|c| s.cell(r, c).ch).collect::<String>())
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let settle = |cond: &dyn Fn() -> bool| {
                let until = Instant::now() + Duration::from_secs(5);
                while !cond() {
                    assert!(Instant::now() < until, "timed out");
                    std::thread::sleep(Duration::from_millis(10));
                }
            };
            chunks.send(b"one\r\n".to_vec()).unwrap();
            settle(&|| shared.gen_bytes.load(Ordering::Acquire) == 5);
            assert!(!text(&shared).contains("one"), "paused: only logged");
            // The disk fails.
            tx.send(LogMsg::Fail("disk full".into())).unwrap();
            settle(&|| health.failed());
            chunks.send(b"two\r\n".to_vec()).unwrap();
            chunks.send(b"three\r\n".to_vec()).unwrap();
            drop(chunks);
            settle(&|| text(&shared).contains("three"));
            let t = text(&shared);
            assert_eq!(
                t.matches("log stopped (writing: disk full)").count(),
                1,
                "{t}"
            );
            assert!(t.contains("while it was hidden"), "{t}");
            assert!(t.contains("two"), "live again: {t}");
            assert!(shared.parsing.load(Ordering::Acquire));
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_host_logger_announces_durable_blocks_and_stores_the_window_signatures() {
            let root = std::env::temp_dir().join("fwterm-window-signs");
            let _ = std::fs::remove_dir_all(&root);
            let (inline, remote) = (root.join("inline"), root.join("remote"));
            let (ann_tx, announced) = std::sync::mpsc::channel();
            let (sig_tx, sigs) = std::sync::mpsc::channel();
            let announce: Announce = Box::new(move |g, dir: &str, b| {
                let _ = ann_tx.send((g, dir.to_string(), b));
            });
            let window = start_logger(
                remote.clone(),
                0,
                SignBy::Window { announce, sigs },
                Default::default(),
            )
            .unwrap();
            let itself =
                start_logger(inline.clone(), 0, SignBy::Itself, Default::default()).unwrap();
            let block = BLOCK_BYTES as usize;
            let data: Vec<u8> = (0..3 * block + 500)
                .map(|i| b"cargo: warning unused \r\n"[i % 24])
                .collect();
            for chunk in data.chunks(64 << 10) {
                window.send(LogMsg::Data(chunk.to_vec(), 1)).unwrap();
                itself.send(LogMsg::Data(chunk.to_vec(), 1)).unwrap();
            }
            // The window reads each announced block and signs it.
            for k in 0..3u64 {
                let (g, dir, b) = announced.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!((g, b), (0, k));
                let r = panelog::ByteReader::open(&remote.join(&dir)).unwrap();
                let at = k as usize * block;
                let lead = (k > 0).then(|| [data[at - 2], data[at - 1]]);
                let mut bytes = vec![0u8; block];
                r.read_exact_at(at as u64, &mut bytes).unwrap();
                sig_tx
                    .send((g, b, panelog::signature(lead, &bytes)))
                    .unwrap();
            }
            assert!(announced.recv_timeout(Duration::from_millis(300)).is_err());
            // The logger stores them within its idle wake (1 s).
            std::thread::sleep(Duration::from_millis(1500));
            for tx in [&window, &itself] {
                let (done, wait) = std::sync::mpsc::channel();
                tx.send(LogMsg::Flush(done)).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            let sig_file = |dir: &Path| {
                let gen = std::fs::read_dir(dir)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::read(gen.join("search.sig")).unwrap()
            };
            let got = sig_file(&remote);
            assert_eq!(got.len(), 3 * (panelog::SIG_BYTES + 4));
            assert_eq!(got, sig_file(&inline));
            drop((window, itself));
            let _ = std::fs::remove_dir_all(&root);
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
