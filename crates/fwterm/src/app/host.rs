//! `fwterm --pane-host`: one pane's program, parser and log in a process of
//! their own (design §8.8a), so a crash in any of them ends this process and
//! not the window.
//!
//! The window starts one per pane with `--isolate`, talks to it over its
//! stdin and stdout (`wire`): it sends keys, resizes and shedding; this sends
//! screen updates, at most every [`UPDATE_EVERY`] and at once after quiet,
//! the pane's shell-integration marks, and the program's exit code. With
//! `--host-window-signs` it also announces each durable block of its log;
//! the window builds the block's search signature on the GPU and sends it
//! back for the logger to store.

use super::*;
use crate::wire::{FromHost, Sender, ToHost};
use std::io::Write;
use std::sync::Condvar;

/// Most history lines one request returns.
const HISTORY_MAX: u32 = 100_000;

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
        history_reply: Mutex::new(None),
        host_died: AtomicBool::new(false),
        pending: AtomicBool::new(true),
        exited: AtomicBool::new(false),
        since_checkpoint: AtomicU64::new(0),
        gen_bytes: AtomicU64::new(0),
        parsing: AtomicBool::new(true),
        paused_at: AtomicU64::new(0),
        catching_up: AtomicBool::new(false),
        want: AtomicBool::new(false),
    });
    // To the window: updates from the loop below, history from the input
    // thread, blocks from the logger; one whole message at a time.
    let out = Arc::new(Mutex::new(io::BufWriter::new(io::stdout())));

    // The window builds the log's search signatures on the GPU: the logger
    // announces each block once durable and stores what comes back.
    let (sign, sig_tx) = if args.host_window_signs {
        let (tx, sigs) = std::sync::mpsc::channel();
        let o = out.clone();
        let announce: Announce = Box::new(move |generation, dir: &str, block| {
            let mut o = o.lock().unwrap();
            let dir = dir.to_string();
            let msg = FromHost::BlockReady {
                generation,
                dir,
                block,
            };
            let _ = msg.write(&mut *o).and_then(|()| o.flush());
        });
        (SignBy::Window { announce, sigs }, Some(tx))
    } else {
        (SignBy::Itself, None)
    };
    let log = match &args.host_log {
        Some(dir) => Some(LogLink {
            tx: start_logger(PathBuf::from(dir), args.host_id, sign)?,
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

    // From the window: keys, resizes, requests for history. Its end is the
    // window's: take the program down too.
    {
        let (session, shared, wake) = (session.clone(), shared.clone(), wake.clone());
        let out = out.clone();
        let (log_tx, log_dir) = (log.as_ref().map(|l| l.tx.clone()), args.host_log.clone());
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
                    // Hidden: only log. Shown: catch up from the log, then
                    // parse live again (as the window does for its own panes).
                    Ok(ToHost::Shed(true)) if log_tx.is_some() => {
                        let _term = shared.term.lock().unwrap();
                        let at = shared.gen_bytes.load(Ordering::Acquire);
                        shared.paused_at.store(at, Ordering::Release);
                        shared.parsing.store(false, Ordering::Release);
                    }
                    Ok(ToHost::Shed(false)) => {
                        let parsing = shared.parsing.load(Ordering::Acquire);
                        if let (false, Some(tx), Some(dir)) = (parsing, &log_tx, &log_dir) {
                            if !shared.catching_up.swap(true, Ordering::AcqRel) {
                                let (shared, tx, dir, wake) =
                                    (shared.clone(), tx.clone(), PathBuf::from(dir), wake.clone());
                                std::thread::spawn(move || {
                                    if catch_up(&shared, &tx, &dir, &wake).is_err() {
                                        shared.parsing.store(true, Ordering::Release);
                                    }
                                    shared.catching_up.store(false, Ordering::Release);
                                    shared.pending.store(true, Ordering::Release);
                                    wake();
                                });
                            }
                        }
                    }
                    Ok(ToHost::Shed(_)) => {}
                    Ok(ToHost::Signature {
                        generation,
                        block,
                        sig,
                    }) => {
                        if let Some(tx) = &sig_tx {
                            let _ = tx.send((generation, block, sig));
                        }
                    }
                    Ok(ToHost::History { first, count }) => {
                        let lines: Vec<_> = match &*shared.term.lock().unwrap() {
                            Emu::Local(t) => (first..first + u64::from(count.min(HISTORY_MAX)))
                                .map(|n| crate::wire::host_line(t, n).unwrap_or_default())
                                .collect(),
                            Emu::Remote(_) => Vec::new(),
                        };
                        let mut o = out.lock().unwrap();
                        let _ = FromHost::History { first, lines }.write(&mut *o);
                        let _ = o.flush();
                    }
                    Err(_) => {
                        session.lock().unwrap().kill();
                        std::process::exit(0);
                    }
                }
            }
        });
    }

    let mut sender = Sender::default();
    let mut last = Instant::now() - UPDATE_EVERY;
    loop {
        {
            let mut ready = signal.0.lock().unwrap();
            // Pending output (set before this loop first ran, too) or an
            // exit ends the wait as well as a wake.
            while !*ready
                && !shared.pending.load(Ordering::Acquire)
                && !shared.exited.load(Ordering::Acquire)
            {
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
        // Readers wake this loop only when they set `pending`; clear it
        // before reading the screen, so output after this read wakes it again.
        shared.pending.store(false, Ordering::Release);
        let update = {
            let term = shared.term.lock().unwrap();
            match &*term {
                Emu::Local(t) => sender.update(t),
                Emu::Remote(_) => None,
            }
        };
        let mut o = out.lock().unwrap();
        if let Some(u) = update {
            FromHost::Update(u).write(&mut *o)?;
        }
        let marks: Vec<_> = shared
            .marks_out
            .as_ref()
            .map(|m| std::mem::take(&mut *m.lock().unwrap()))
            .unwrap_or_default();
        for (kind, line, exit) in marks {
            FromHost::Mark { kind, line, exit }.write(&mut *o)?;
        }
        o.flush()?;
        drop(o);
        last = Instant::now();
        if shared.exited.load(Ordering::Acquire) {
            if let Some(log) = &log {
                let (done, wait) = std::sync::mpsc::channel();
                if log.tx.send(LogMsg::Flush(done)).is_ok() {
                    let _ = wait.recv_timeout(Duration::from_secs(5));
                }
            }
            let code = session.lock().unwrap().wait();
            let mut o = out.lock().unwrap();
            FromHost::Exited(code).write(&mut *o)?;
            o.flush()?;
            return Ok(code);
        }
    }
}
