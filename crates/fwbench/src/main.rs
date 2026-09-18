//! `fwbench` — the foldwave M0 harness.

use fwbench::flood::Transport;
use fwbench::{disk, echo, emit, flood, report, Run};
use std::io::{self, Write};
use std::time::Duration;

const USAGE: &str = "\
fwbench - foldwave M0 harness

usage: fwbench <mode> [options]

modes:
  flood         N panes flooding at once; measures throughput
  echo          key-echo latency through a shell, idle machine
  combined      echo latency measured WHILE the panes flood (the section 4 target)
  disk          durable write rate of the log drive: buffered+fsync (the old
                probe, as control), then write-through at qd1 and qdN

options:
  --panes N     panes to flood (default 8)
  --mib N       MiB each pane emits (default 32); for disk, MiB per pattern
                (default 2048)
  --samples N   key-echo samples (default 200)
  --gap-ms N    milliseconds between keystrokes (default 25; below ~16 the
                Windows console's coalescing tick dominates the measurement)
  --shell NAME  shell to measure (default: cmd on Windows, sh elsewhere)
  --transport T pty (default, a real console) or pipe (headless, no console host)
  --no-attrib   skip resource attribution
  --path DIR    disk: directory on the drive to measure (default: current dir;
                the reference log drive is E:\\)
  --io-kib N    disk: bytes per write for the write-through patterns (default 1024)
  --qd N        disk: queue depth for the queued pattern (default 8)
  -h, --help    this message

internal:
  --emit --bytes N   emit N bytes of agent-like output to stdout
";

#[derive(Debug, PartialEq)]
enum Mode {
    Flood,
    Echo,
    Combined,
    Disk,
    Emit { bytes: u64 },
}

#[derive(Debug)]
struct Args {
    mode: Mode,
    panes: usize,
    mib: u64,
    samples: usize,
    gap: Duration,
    shell: Option<String>,
    attribution: bool,
    transport: Transport,
    path: Option<String>,
    io_kib: usize,
    qd: usize,
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut mode: Option<Mode> = None;
    let mut panes = 8usize;
    let mut mib: Option<u64> = None;
    let mut samples = 200usize;
    let mut gap_ms = 25u64;
    let mut shell = None;
    let mut attribution = true;
    let mut transport = Transport::Pty;
    let mut path = None;
    let mut io_kib = 1024usize;
    let mut qd = 8usize;
    let mut emit_bytes: Option<u64> = None;

    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        let v = argv
            .get(*i + 1)
            .ok_or_else(|| format!("{flag} needs a value"))?
            .clone();
        *i += 2;
        Ok(v)
    };

    while i < argv.len() {
        match argv[i].as_str() {
            "-h" | "--help" => return Err(USAGE.to_string()),
            "flood" => {
                mode = Some(Mode::Flood);
                i += 1;
            }
            "echo" => {
                mode = Some(Mode::Echo);
                i += 1;
            }
            "combined" => {
                mode = Some(Mode::Combined);
                i += 1;
            }
            "disk" => {
                mode = Some(Mode::Disk);
                i += 1;
            }
            "--path" => path = Some(value(&mut i, "--path")?),
            "--io-kib" => {
                let v = value(&mut i, "--io-kib")?;
                io_kib = v.parse().map_err(|_| format!("bad --io-kib {v:?}"))?;
                if io_kib == 0 || (io_kib * 1024) % disk::SECTOR != 0 {
                    return Err(format!(
                        "--io-kib must be a non-zero multiple of {}",
                        disk::SECTOR / 1024
                    ));
                }
            }
            "--qd" => {
                let v = value(&mut i, "--qd")?;
                qd = v.parse().map_err(|_| format!("bad --qd {v:?}"))?;
                if qd == 0 {
                    return Err("--qd must be at least 1".into());
                }
            }
            "--emit" => {
                mode = Some(Mode::Emit { bytes: 0 });
                i += 1;
            }
            "--bytes" => {
                let v = value(&mut i, "--bytes")?;
                emit_bytes = Some(v.parse().map_err(|_| format!("bad --bytes {v:?}"))?);
            }
            "--panes" => {
                let v = value(&mut i, "--panes")?;
                panes = v.parse().map_err(|_| format!("bad --panes {v:?}"))?;
                if panes == 0 {
                    return Err("--panes must be at least 1".into());
                }
            }
            "--mib" => {
                let v = value(&mut i, "--mib")?;
                let n: u64 = v.parse().map_err(|_| format!("bad --mib {v:?}"))?;
                if n == 0 {
                    return Err("--mib must be at least 1".into());
                }
                mib = Some(n);
            }
            "--samples" => {
                let v = value(&mut i, "--samples")?;
                samples = v.parse().map_err(|_| format!("bad --samples {v:?}"))?;
                if samples == 0 {
                    return Err("--samples must be at least 1".into());
                }
            }
            "--gap-ms" => {
                let v = value(&mut i, "--gap-ms")?;
                gap_ms = v.parse().map_err(|_| format!("bad --gap-ms {v:?}"))?;
            }
            "--shell" => shell = Some(value(&mut i, "--shell")?),
            "--transport" => {
                let v = value(&mut i, "--transport")?;
                transport = match v.as_str() {
                    "pty" => Transport::Pty,
                    "pipe" => Transport::Pipe,
                    other => return Err(format!("bad --transport {other:?}, want pty or pipe")),
                };
            }
            "--no-attrib" => {
                attribution = false;
                i += 1;
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    let mode = match mode {
        Some(Mode::Emit { .. }) => Mode::Emit {
            bytes: emit_bytes.ok_or("--emit needs --bytes")?,
        },
        Some(m) => m,
        None => return Err(USAGE.to_string()),
    };
    // A drive needs a long stream to show its sustained rate; panes do not.
    let mib = mib.unwrap_or(if mode == Mode::Disk { 2048 } else { 32 });

    Ok(Args {
        mode,
        panes,
        mib,
        samples,
        gap: Duration::from_millis(gap_ms),
        shell,
        attribution,
        transport,
        path,
        io_kib,
        qd,
    })
}

/// Emit the synthetic workload to stdout, as a flooding pane does.
fn emit_workload(bytes: u64) -> io::Result<()> {
    const CHUNK: usize = 64 * 1024;
    let mut generator = emit::Generator::new(0xF01D_2A7E);
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut written = 0u64;
    let mut buf = Vec::with_capacity(CHUNK + 512);

    while written < bytes {
        buf.clear();
        let want = CHUNK.min((bytes - written) as usize);
        generator.fill(&mut buf, want);
        out.write_all(&buf)?;
        written += buf.len() as u64;
    }
    out.flush()
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(a) => a,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    if let Mode::Emit { bytes } = args.mode {
        if let Err(e) = emit_workload(bytes) {
            // A closed pipe is the normal way a pane ends early.
            if e.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("fwbench: emit failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if args.mode == Mode::Disk {
        let dir = std::path::PathBuf::from(args.path.as_deref().unwrap_or("."));
        let bytes = args.mib * 1024 * 1024;
        let config = disk::Config {
            dir: dir.clone(),
            bytes,
            patterns: disk::Pattern::default_matrix(args.io_kib * 1024, args.qd),
        };
        eprintln!(
            "disk: {} MiB per pattern under {}...",
            args.mib,
            dir.display()
        );
        match disk::run(&config) {
            Ok(results) => print!("{}", disk::render(&dir, bytes, &results)),
            Err(e) => {
                eprintln!("fwbench: disk measurement failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            eprintln!("fwbench: cannot find own path: {e}");
            std::process::exit(1);
        }
    };
    let shell = args
        .shell
        .clone()
        .unwrap_or_else(|| echo::default_shell().to_string());
    let bytes_per_pane = args.mib * 1024 * 1024;

    let before = if args.attribution {
        resmon::sys::snapshot().ok()
    } else {
        None
    };
    let started = std::time::Instant::now();

    let run = match args.mode {
        Mode::Emit { .. } | Mode::Disk => unreachable!("handled above"),
        Mode::Flood => {
            eprintln!("flooding {} panes x {} MiB...", args.panes, args.mib);
            Run {
                flood: Some(flood::run(
                    &exe,
                    args.panes,
                    bytes_per_pane,
                    24,
                    80,
                    args.transport,
                )),
                echo: None,
                attribution: None,
            }
        }
        Mode::Echo => {
            eprintln!("measuring {} key echoes through `{shell}`...", args.samples);
            Run {
                flood: None,
                echo: measure_echo(&shell, args.samples, args.gap),
                attribution: None,
            }
        }
        Mode::Combined => {
            eprintln!(
                "flooding {} panes x {} MiB while measuring {} key echoes through `{shell}`...",
                args.panes, args.mib, args.samples
            );
            let exe_for_flood = exe.clone();
            let panes = args.panes;
            let transport = args.transport;
            let flooder = std::thread::spawn(move || {
                flood::run(&exe_for_flood, panes, bytes_per_pane, 24, 80, transport)
            });
            let echo = measure_echo(&shell, args.samples, args.gap);
            let flood = flooder.join().ok();
            Run {
                flood,
                echo,
                attribution: None,
            }
        }
    };

    let attribution = match before {
        Some(before) => resmon::sys::snapshot()
            .ok()
            .map(|after| resmon::attribute(&before, &after, started.elapsed().as_secs_f64())),
        None => None,
    };
    let run = Run { attribution, ..run };

    print!("{}", report::render(&run));
    println!("verdict: {}", report::verdict(&run));
}

fn measure_echo(shell: &str, samples: usize, gap: Duration) -> Option<echo::EchoResult> {
    match echo::run(shell, samples, gap) {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("fwbench: echo measurement failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_mode_is_required() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn each_mode_parses() {
        assert_eq!(parse(&argv(&["flood"])).unwrap().mode, Mode::Flood);
        assert_eq!(parse(&argv(&["echo"])).unwrap().mode, Mode::Echo);
        assert_eq!(parse(&argv(&["combined"])).unwrap().mode, Mode::Combined);
        assert_eq!(parse(&argv(&["disk"])).unwrap().mode, Mode::Disk);
    }

    #[test]
    fn disk_options_parse_and_guard_alignment() {
        let a = parse(&argv(&[
            "disk", "--path", "E:\\", "--io-kib", "256", "--qd", "4",
        ]))
        .unwrap();
        assert_eq!(a.path.as_deref(), Some("E:\\"));
        assert_eq!(a.io_kib, 256);
        assert_eq!(a.qd, 4);
        let d = parse(&argv(&["disk"])).unwrap();
        assert_eq!((d.io_kib, d.qd, d.path), (1024, 8, None));
        assert_eq!(d.mib, 2048);
        assert_eq!(parse(&argv(&["disk", "--mib", "64"])).unwrap().mib, 64);
        assert!(parse(&argv(&["disk", "--io-kib", "3"])).is_err());
        assert!(parse(&argv(&["disk", "--qd", "0"])).is_err());
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let a = parse(&argv(&["flood"])).unwrap();
        assert_eq!(a.panes, 8);
        assert_eq!(a.mib, 32);
        assert_eq!(a.samples, 200);
        assert_eq!(a.gap, Duration::from_millis(25));
        assert!(a.attribution);
    }

    #[test]
    fn options_override_the_defaults() {
        let a = parse(&argv(&[
            "combined",
            "--panes",
            "16",
            "--mib",
            "64",
            "--samples",
            "50",
            "--gap-ms",
            "10",
            "--shell",
            "pwsh",
            "--no-attrib",
        ]))
        .unwrap();
        assert_eq!(a.panes, 16);
        assert_eq!(a.mib, 64);
        assert_eq!(a.samples, 50);
        assert_eq!(a.gap, Duration::from_millis(10));
        assert_eq!(a.shell.as_deref(), Some("pwsh"));
        assert!(!a.attribution);
    }

    #[test]
    fn emit_mode_requires_a_byte_count() {
        assert!(parse(&argv(&["--emit"])).is_err());
        assert_eq!(
            parse(&argv(&["--emit", "--bytes", "1024"])).unwrap().mode,
            Mode::Emit { bytes: 1024 }
        );
    }

    #[test]
    fn zero_valued_options_are_rejected_rather_than_silently_useless() {
        assert!(parse(&argv(&["flood", "--panes", "0"])).is_err());
        assert!(parse(&argv(&["flood", "--mib", "0"])).is_err());
        assert!(parse(&argv(&["echo", "--samples", "0"])).is_err());
    }

    #[test]
    fn transport_parses_and_rejects_nonsense() {
        assert_eq!(parse(&argv(&["flood"])).unwrap().transport, Transport::Pty);
        assert_eq!(
            parse(&argv(&["flood", "--transport", "pipe"]))
                .unwrap()
                .transport,
            Transport::Pipe
        );
        assert!(parse(&argv(&["flood", "--transport", "carrier-pigeon"])).is_err());
    }

    #[test]
    fn help_returns_usage() {
        assert!(parse(&argv(&["--help"])).unwrap_err().contains("usage:"));
    }

    #[test]
    fn bad_and_missing_values_are_rejected() {
        assert!(parse(&argv(&["flood", "--panes", "many"])).is_err());
        assert!(parse(&argv(&["flood", "--panes"])).is_err());
        assert!(parse(&argv(&["--wat"])).is_err());
    }

    #[test]
    fn emit_writes_at_least_the_requested_bytes() {
        // Exercises the generator path the panes use, without a pty.
        let mut buf = Vec::new();
        let mut g = emit::Generator::new(0xF01D_2A7E);
        g.fill(&mut buf, 8 * 1024);
        assert!(buf.len() >= 8 * 1024);
    }
}
