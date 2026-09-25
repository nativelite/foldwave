//! What a process boundary costs a terminal (design §8.8a).
//!
//! Fault isolation puts each pane's parser in a pane-host process. A key
//! then crosses from the window to the host and its echo comes back as a
//! screen update, and every frame's changed rows travel from host to window.
//! This measures those two costs through the plainest channel std offers, a
//! child's stdin and stdout pipes:
//!
//! - **round trip:** a small message to the child and straight back, as a key
//!   and its echo would go (timed one at a time);
//! - **screen updates:** frames the size of a full pane screen, sent by the
//!   child as fast as the window takes them, the upper bound of what a host
//!   ever sends, since it sends changed rows at most once per frame.

use crate::stats::Summary;
use std::io::{self, Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

/// What the child does, told by its first byte.
const ECHO: u8 = b'e';
const FRAMES: u8 = b'f';

/// The child's side: echo messages back, or send frames on request.
pub fn child() -> io::Result<()> {
    let (mut input, mut output) = (io::stdin().lock(), io::stdout().lock());
    let mut mode = [0u8; 1];
    input.read_exact(&mut mode)?;
    match mode[0] {
        ECHO => {
            // Length-prefixed messages, echoed until the pipe closes.
            let mut len = [0u8; 4];
            let mut buf = Vec::new();
            while input.read_exact(&mut len).is_ok() {
                buf.resize(u32::from_le_bytes(len) as usize, 0);
                input.read_exact(&mut buf)?;
                output.write_all(&len)?;
                output.write_all(&buf)?;
                output.flush()?;
            }
        }
        FRAMES => {
            let mut spec = [0u8; 8];
            input.read_exact(&mut spec)?;
            let size = u32::from_le_bytes(spec[..4].try_into().unwrap()) as usize;
            let count = u32::from_le_bytes(spec[4..].try_into().unwrap());
            let frame = vec![0x5Au8; size];
            for _ in 0..count {
                output.write_all(&(size as u32).to_le_bytes())?;
                output.write_all(&frame)?;
            }
            output.flush()?;
        }
        _ => {}
    }
    Ok(())
}

/// The results.
#[derive(Debug)]
pub struct Report {
    /// Time for one message to the child and back.
    pub round_trip: Summary,
    /// Bytes per round-trip message.
    pub message: usize,
    /// Bytes per screen-update frame, and how many were sent.
    pub frame_bytes: usize,
    pub frames: u32,
    /// How fast the window side received them.
    pub frames_per_sec: f64,
    pub mib_per_sec: f64,
}

fn spawn(exe: &std::path::Path) -> io::Result<std::process::Child> {
    Command::new(exe)
        .arg("--ipc-child")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
}

/// Run both measurements against a child started from `exe`.
pub fn run(exe: &std::path::Path, samples: usize) -> io::Result<Report> {
    // Round trips: 64-byte messages, the size of a key plus a little framing.
    let message = 64;
    let mut c = spawn(exe)?;
    let (mut to, mut from) = (c.stdin.take().unwrap(), c.stdout.take().unwrap());
    to.write_all(&[ECHO])?;
    let msg = vec![7u8; message];
    let mut back = vec![0u8; message + 4];
    let mut times = Vec::with_capacity(samples);
    for i in 0..samples + 200 {
        let t = Instant::now();
        to.write_all(&(message as u32).to_le_bytes())?;
        to.write_all(&msg)?;
        to.flush()?;
        from.read_exact(&mut back)?;
        if i >= 200 {
            times.push(t.elapsed()); // the first 200 warm up
        }
    }
    drop(to);
    let _ = c.wait();

    // Screen updates: a full 2400x1300 pane of cells (about 266 x 68 cells
    // at 12 bytes each), sent back to back.
    let frame_bytes = 266 * 68 * 12;
    let frames = 5000u32;
    let mut c = spawn(exe)?;
    let (mut to, mut from) = (c.stdin.take().unwrap(), c.stdout.take().unwrap());
    let mut spec = vec![FRAMES];
    spec.extend_from_slice(&(frame_bytes as u32).to_le_bytes());
    spec.extend_from_slice(&frames.to_le_bytes());
    to.write_all(&spec)?;
    to.flush()?;
    let mut buf = vec![0u8; frame_bytes];
    let mut len = [0u8; 4];
    let t = Instant::now();
    for _ in 0..frames {
        from.read_exact(&mut len)?;
        from.read_exact(&mut buf)?;
    }
    let secs = t.elapsed().as_secs_f64().max(1e-9);
    let _ = c.wait();
    Ok(Report {
        round_trip: Summary::of(&times),
        message,
        frame_bytes,
        frames,
        frames_per_sec: frames as f64 / secs,
        mib_per_sec: frames as f64 * frame_bytes as f64 / secs / (1 << 20) as f64,
    })
}

/// The report as text.
pub fn render(r: &Report) -> String {
    format!(
        "hostipc  round trip, {} B messages over pipes: {}\n\
         hostipc  screen updates, {} KiB frames: {:.0} frames/s ({:.0} MiB/s)\n",
        r.message,
        r.round_trip.line(),
        r.frame_bytes / 1024,
        r.frames_per_sec,
        r.mib_per_sec,
    )
}
