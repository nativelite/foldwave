//! `fwterm --pane-host`: one pane's program, parser and log in a process of
//! their own (design §8.8a), so a crash in any of them ends this process and
//! not the window.
//!
//! The window starts one per pane with `--isolate`, talks to it over its
//! stdin and stdout (`wire`): it sends keys, resizes and shedding; this sends
//! screen updates, at most every [`UPDATE_EVERY`] and at once after quiet,
//! the pane's shell-integration marks, and the program's exit code.

use super::*;
use crate::wire::{FromHost, Sender, ToHost};
use std::io::Write;
use std::sync::Condvar;

/// Fastest updates go out: a key's echo waits for none of it after quiet.
const UPDATE_EVERY: Duration = Duration::from_millis(4);

/// Run the pane host; returns the program's exit code.
pub fn run(args: &Args) -> io::Result<i32> {
    choose_console_host(args)?;
    let (rows, cols) = (args.host_size.0 as usize, args.host_size.1 as usize);
    let shared = Arc::new(Shared {
        term: Mutex::new(Emu::Local({
            let mut t = Term::with_history(rows, cols, args.history);
            if args.pipe {
                t.feed(b"\x1b[20h");
            }
            // The window's banner, handed over in the environment.
            if let Ok(banner) = std::env::var("FWTERM_BANNER") {
                t.feed(banner.as_bytes());
            }
            t
        })),
        blocks: Mutex::new(blocks::Blocks::default()),
        marks_out: Some(Mutex::new(Vec::new())),
        pending: AtomicBool::new(true),
        exited: AtomicBool::new(false),
        since_checkpoint: AtomicU64::new(0),
        gen_bytes: AtomicU64::new(0),
        parsing: AtomicBool::new(true),
        paused_at: AtomicU64::new(0),
        catching_up: AtomicBool::new(false),
        want: AtomicBool::new(false),
    });
    let log = match &args.host_log {
        Some(dir) => Some(LogLink {
            tx: start_logger(PathBuf::from(dir), args.host_id, None)?,
            gen_limit: Arc::new(AtomicU64::new(args.host_gen)),
            nudge: std::sync::mpsc::sync_channel(1).0,
        }),
        None => None,
    };
    // Output wakes the update loop.
    let signal = Arc::new((Mutex::new(false), Condvar::new()));
    let wake: Wake = {
        let s = signal.clone();
        Arc::new(move || {
            *s.0.lock().unwrap() = true;
            s.1.notify_one();
        })
    };
    let activity = Arc::new(Activity {
        epoch: Instant::now(),
        first_output: AtomicU64::new(u64::MAX),
        last_output: AtomicU64::new(0),
    });
    let session = start_session(
        args,
        rows,
        cols,
        &shared,
        &activity,
        wake.clone(),
        log.clone(),
        &Arc::new(Vec::new()),
    )?;

    // From the window: keys, resizes. Its end is the window's: take the
    // program down too.
    {
        let (session, shared, wake) = (session.clone(), shared.clone(), wake.clone());
        std::thread::spawn(move || {
            let mut input = io::stdin().lock();
            loop {
                match ToHost::read(&mut input) {
                    Ok(ToHost::Input(bytes)) => {
                        let _ = session.lock().unwrap().write(&bytes);
                    }
                    Ok(ToHost::Resize { rows, cols }) => {
                        shared
                            .term
                            .lock()
                            .unwrap()
                            .resize(rows.max(1) as usize, cols.max(1) as usize);
                        session.lock().unwrap().resize(rows, cols);
                        wake();
                    }
                    Ok(ToHost::Shed(_)) => {}
                    Err(_) => {
                        session.lock().unwrap().kill();
                        std::process::exit(0);
                    }
                }
            }
        });
    }

    let mut out = io::BufWriter::new(io::stdout().lock());
    let mut sender = Sender::default();
    let mut last = Instant::now() - UPDATE_EVERY;
    loop {
        {
            let mut ready = signal.0.lock().unwrap();
            while !*ready && !shared.exited.load(Ordering::Acquire) {
                ready = signal
                    .1
                    .wait_timeout(ready, Duration::from_millis(250))
                    .unwrap()
                    .0;
            }
            *ready = false;
        }
        // During a flood, one update per interval carries it all.
        let since = last.elapsed();
        if since < UPDATE_EVERY {
            std::thread::sleep(UPDATE_EVERY - since);
        }
        let update = {
            let term = shared.term.lock().unwrap();
            match &*term {
                Emu::Local(t) => sender.update(t),
                Emu::Remote(_) => None,
            }
        };
        if let Some(u) = update {
            FromHost::Update(u).write(&mut out)?;
        }
        let marks: Vec<_> = shared
            .marks_out
            .as_ref()
            .map(|m| std::mem::take(&mut *m.lock().unwrap()))
            .unwrap_or_default();
        for (kind, line, exit) in marks {
            FromHost::Mark { kind, line, exit }.write(&mut out)?;
        }
        out.flush()?;
        last = Instant::now();
        if shared.exited.load(Ordering::Acquire) {
            if let Some(log) = &log {
                let (done, wait) = std::sync::mpsc::channel();
                if log.tx.send(LogMsg::Flush(done)).is_ok() {
                    let _ = wait.recv_timeout(Duration::from_secs(5));
                }
            }
            let code = session.lock().unwrap().wait();
            FromHost::Exited(code).write(&mut out)?;
            out.flush()?;
            return Ok(code);
        }
    }
}
