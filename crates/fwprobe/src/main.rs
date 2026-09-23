//! `fwprobe` — measure any terminal from the outside, the same way for all.
//!
//! * **latency**: launch the terminal running `cmd`, put its window at a fixed
//!   place and size on top, and type. Each sample types one character with
//!   `SendInput`, then reads the screen around the prompt with `BitBlt` until
//!   the echo shows, and erases it with Backspace. What it measures is key
//!   press to pixels on screen, through the console host, the terminal and
//!   the compositor — what a user sees — the method of Pavel Fatin's
//!   Typometer. The prompt sets a steady cursor (`ESC [2 q`), so a blinking
//!   cursor is never taken for an echo.
//! * **throughput**: the terminal runs `fwprobe emit`, which writes a fixed
//!   amount of agent-like output to its console and records how long that
//!   took: how fast the terminal (and its console host) swallow output.
//!
//! Safety for the person at the machine: before every keystroke the probe
//! checks that the terminal under test still has the keyboard focus, and
//! stops the run if not, so it never types into anything else.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(windows)]
mod sys;

#[cfg(windows)]
fn main() {
    std::process::exit(probe::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fwprobe is Windows-only");
    std::process::exit(1);
}

#[cfg(windows)]
mod probe {
    use crate::sys;
    use fwbench::stats::Summary;
    use std::io;
    use std::path::PathBuf;
    use std::process::{Child, Command};
    use std::time::{Duration, Instant};

    const USAGE: &str = "\
fwprobe - measure a terminal from the outside

usage:
  fwprobe latency    --name NAME [--samples N] [--size WxH] -- LAUNCH...
  fwprobe throughput --name NAME [--mib N] [--runs N] -- LAUNCH...
  fwprobe emit BYTES REPORT        (run inside the terminal by throughput)

LAUNCH is the terminal's command line, with {cmd} where the shell command
goes and {title} where a window title can go, e.g.
  fwprobe latency --name alacritty -- alacritty.exe --title {title} -e {cmd}
{cmd} expands to several arguments: cmd.exe /k <setup>.
";

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }

    struct Args {
        mode: String,
        name: String,
        samples: usize,
        size: (i32, i32),
        mib: u64,
        runs: usize,
        launch: Vec<String>,
        rest: Vec<String>,
    }

    fn parse(argv: &[String]) -> Result<Args, String> {
        let mut a = Args {
            mode: argv.first().cloned().ok_or(USAGE)?,
            name: String::new(),
            samples: 200,
            size: (1200, 760),
            mib: 256,
            runs: 3,
            launch: Vec::new(),
            rest: Vec::new(),
        };
        let mut i = 1;
        while i < argv.len() {
            let v = |i: usize| {
                argv.get(i + 1)
                    .cloned()
                    .ok_or(format!("{} needs a value", argv[i]))
            };
            match argv[i].as_str() {
                "--name" => a.name = v(i)?,
                "--samples" => a.samples = v(i)?.parse().map_err(|_| "bad --samples")?,
                "--mib" => a.mib = v(i)?.parse().map_err(|_| "bad --mib")?,
                "--runs" => a.runs = v(i)?.parse().map_err(|_| "bad --runs")?,
                "--size" => {
                    let s = v(i)?;
                    a.size = s
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .ok_or("bad --size")?;
                }
                "--" => {
                    a.launch = argv[i + 1..].to_vec();
                    break;
                }
                _ => {
                    a.rest.push(argv[i].clone());
                    i += 1;
                    continue;
                }
            }
            i += 2;
        }
        Ok(a)
    }

    pub fn run() -> i32 {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let a = match parse(&argv) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        let r = match a.mode.as_str() {
            "latency" if !a.launch.is_empty() => latency(&a),
            "throughput" if !a.launch.is_empty() => throughput(&a),
            "emit" if a.rest.len() == 2 => emit(&a.rest[0], &a.rest[1]),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        };
        match r {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("fwprobe: {e}");
                1
            }
        }
    }

    /// Launch the terminal with `shell_setup` run by `cmd /k` (or `/c`).
    fn launch(a: &Args, title: &str, cmd: &[String]) -> io::Result<Child> {
        let mut words = Vec::new();
        for w in &a.launch {
            if w == "{cmd}" {
                words.extend(cmd.iter().cloned());
            } else {
                words.push(w.replace("{title}", title));
            }
        }
        Command::new(&words[0]).args(&words[1..]).spawn()
    }

    /// Wait for a visible window whose title contains `title`.
    fn find(title: &str, within: Duration) -> io::Result<sys::Hwnd> {
        let start = Instant::now();
        while start.elapsed() < within {
            if let Some((h, _)) = sys::find_windows(title).into_iter().next() {
                return Ok(h);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no window titled {title:?}"),
        ))
    }

    fn unique(name: &str) -> String {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("fwprobe-{name}-{}", t % 1_000_000)
    }

    fn differs(a: &[u32], b: &[u32]) -> bool {
        a.iter().zip(b).any(|(x, y)| x != y)
    }

    /// Wait until the region stops changing for `quiet`, up to `limit`.
    fn settle(cap: &sys::Capture, quiet: Duration, limit: Duration) -> io::Result<Vec<u32>> {
        let (mut last, mut cur) = (Vec::new(), Vec::new());
        cap.grab(&mut last)?;
        let (start, mut stable) = (Instant::now(), Instant::now());
        while start.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(5));
            cap.grab(&mut cur)?;
            if differs(&cur, &last) {
                std::mem::swap(&mut cur, &mut last);
                stable = Instant::now();
            } else if stable.elapsed() >= quiet {
                break;
            }
        }
        Ok(last)
    }

    fn guard(h: sys::Hwnd) -> io::Result<()> {
        let now = sys::foreground();
        if now == h {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!(
                    "the terminal lost keyboard focus to {:?} ({now:?}); stopped so no keys go elsewhere",
                    sys::title(now)
                ),
            ))
        }
    }

    fn latency(a: &Args) -> io::Result<()> {
        let title = unique(&a.name);
        // A steady block cursor at every prompt, and a title to find us by.
        let setup = format!("title {title} & prompt $E[2 q$P$G");
        let cmd = vec!["cmd.exe".into(), "/k".into(), setup];
        let mut child = launch(a, &title, &cmd)?;
        let h = find(&title, Duration::from_secs(20))?;
        sys::place(h, 80, 80, a.size.0, a.size.1);
        sys::topmost(h, true);
        let result = measure(a, h);
        sys::topmost(h, false);
        sys::close(h);
        std::thread::sleep(Duration::from_millis(300));
        let _ = child.kill();
        let samples = result?;
        report(&a.name, &samples.0, samples.1, samples.2);
        Ok(())
    }

    /// Returns (latencies, misses, capture cost).
    fn measure(a: &Args, h: sys::Hwnd) -> io::Result<(Vec<Duration>, usize, Duration)> {
        std::thread::sleep(Duration::from_millis(1500)); // shell start, prompt
        if !sys::focus(h) {
            std::thread::sleep(Duration::from_millis(300));
            if !sys::focus(h) {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "could not focus the terminal",
                ));
            }
        }
        // Calibrate: find where an echo lands by typing one and diffing the
        // whole client area.
        let client = sys::client_rect_on_screen(h);
        let whole = sys::Capture::new(client)?;
        let before = settle(&whole, Duration::from_millis(300), Duration::from_secs(3))?;
        guard(h)?;
        sys::type_char('x')?;
        let after = settle(&whole, Duration::from_millis(300), Duration::from_secs(3))?;
        guard(h)?;
        sys::press(sys::VK_BACK)?;
        let (w, hh) = whole.size();
        let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..hh {
            for x in 0..w {
                if before[y * w + x] != after[y * w + x] {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x + 1);
                    y1 = y1.max(y + 1);
                }
            }
        }
        if x0 == usize::MAX {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "typing changed nothing on screen",
            ));
        }
        let region = sys::Rect {
            left: client.left + x0 as i32,
            top: client.top + y0 as i32,
            right: client.left + x1 as i32,
            bottom: client.top + y1 as i32,
        };
        drop(whole);
        let cap = sys::Capture::new(region)?;
        let mut baseline = settle(&cap, Duration::from_millis(300), Duration::from_secs(3))?;

        // What one read of the region costs: the resolution of every sample.
        let mut scratch = Vec::new();
        let mut costs = Vec::new();
        for _ in 0..50 {
            let t = Instant::now();
            cap.grab(&mut scratch)?;
            costs.push(t.elapsed());
        }
        let cost = Summary::of(&costs).p50;

        let mut rng = 0x2545_F491_4F6C_DD1Du64;
        let (mut out, mut misses) = (Vec::with_capacity(a.samples), 0);
        for i in 0..a.samples {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            std::thread::sleep(Duration::from_millis(60 + rng % 80));
            guard(h)?;
            let t0 = Instant::now();
            sys::type_char('x')?;
            let mut seen = None;
            while t0.elapsed() < Duration::from_millis(500) {
                cap.grab(&mut scratch)?;
                if differs(&scratch, &baseline) {
                    seen = Some(t0.elapsed());
                    break;
                }
            }
            match seen {
                Some(d) => out.push(d),
                None => misses += 1,
            }
            guard(h)?;
            sys::press(sys::VK_BACK)?;
            // Back to the baseline before the next sample; re-take it if the
            // screen settled somewhere else (a prompt redraw).
            let back = Instant::now();
            loop {
                cap.grab(&mut scratch)?;
                if !differs(&scratch, &baseline) {
                    break;
                }
                if back.elapsed() > Duration::from_millis(500) {
                    baseline = settle(&cap, Duration::from_millis(200), Duration::from_secs(2))?;
                    break;
                }
            }
            if (i + 1) % 50 == 0 {
                eprintln!("  {}: {} samples", a.name, i + 1);
            }
        }
        Ok((out, misses, cost))
    }

    fn report(name: &str, samples: &[Duration], misses: usize, cost: Duration) {
        let s = Summary::of(samples);
        println!(
            "| {name} | {:.2} / {:.2} / {:.2} ms | {:.2} / {:.2} ms | {} | {} | {:.2} ms |",
            ms(s.p50),
            ms(s.p90),
            ms(s.p99),
            ms(s.min),
            ms(s.max),
            s.count,
            misses,
            ms(cost)
        );
    }

    fn throughput(a: &Args) -> io::Result<()> {
        let me = std::env::current_exe()?;
        let bytes = a.mib * 1024 * 1024;
        let mut rates = Vec::new();
        for run in 0..a.runs {
            let title = unique(&a.name);
            let report: PathBuf = std::env::temp_dir().join(format!("{title}.txt"));
            let _ = std::fs::remove_file(&report);
            let setup = format!(
                "title {title} & \"{}\" emit {bytes} \"{}\"",
                me.display(),
                report.display()
            );
            let cmd = vec!["cmd.exe".into(), "/k".into(), setup];
            let mut child = launch(a, &title, &cmd)?;
            let h = find(&title, Duration::from_secs(20))?;
            sys::place(h, 80, 80, a.size.0, a.size.1);
            let start = Instant::now();
            let secs = loop {
                if let Ok(s) = std::fs::read_to_string(&report) {
                    if let Ok(v) = s.trim().parse::<f64>() {
                        break Some(v);
                    }
                }
                if start.elapsed() > Duration::from_secs(300) {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            sys::close(h);
            std::thread::sleep(Duration::from_millis(500));
            let _ = child.kill();
            let _ = std::fs::remove_file(&report);
            match secs {
                Some(s) => {
                    let rate = a.mib as f64 / s;
                    eprintln!(
                        "  {} run {}: {:.1} MiB/s ({:.2} s)",
                        a.name,
                        run + 1,
                        rate,
                        s
                    );
                    rates.push(rate);
                }
                None => eprintln!("  {} run {}: timed out", a.name, run + 1),
            }
        }
        rates.sort_by(|x, y| x.total_cmp(y));
        let med = rates.get(rates.len() / 2).copied().unwrap_or(0.0);
        println!(
            "| {} | {:.1} MiB/s | {} | {} |",
            a.name,
            med,
            rates
                .iter()
                .map(|r| format!("{r:.1}"))
                .collect::<Vec<_>>()
                .join(", "),
            a.mib
        );
        Ok(())
    }

    /// Inside the terminal: write `bytes` of agent-like output to the console
    /// and record the seconds it took in `report`.
    fn emit(bytes: &str, report: &str) -> io::Result<()> {
        let bytes: usize = bytes
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad byte count"))?;
        let mut source = Vec::with_capacity(4 << 20);
        fwbench::emit::Generator::new(0xF01D_2A7E).fill(&mut source, 4 << 20);
        let start = Instant::now();
        let mut left = bytes;
        while left > 0 {
            let n = left.min(source.len());
            sys::write_stdout(&source[..n])?;
            left -= n;
        }
        let secs = start.elapsed().as_secs_f64();
        std::fs::write(report, format!("{secs}"))?;
        Ok(())
    }
}
