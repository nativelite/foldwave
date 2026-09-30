//! Between a pane host and the window (design §8.8a).
//!
//! With `--isolate`, a pane's program, parser and log live in a pane-host
//! process, so a crash there cannot take the window down. The window keeps a
//! display-only copy of the pane's screen, a [`Mirror`], fed with
//! [`Update`]s: how many lines scrolled off the top since the last update, and
//! the rows that differ from what was last sent. The [`Sender`] on the host
//! works out each update by comparing the screen with its copy of what it
//! sent.
//!
//! History does not travel with updates. A first cut sent every line that
//! scrolled off, up to 10,000 an update, and cost a flood half its throughput
//! and four times the CPU. The mirror counts lines, so selections and blocks
//! keep their numbering, and knows how many the host still holds; a view
//! scrolled back into those asks for just the lines it shows
//! ([`ToHost::History`]). Past them, scrollback reads the log the host writes.
//!
//! Rows travel as UTF-8 text plus runs of style, trailing blanks dropped, so
//! a line costs about what its text does.
//!
//! Every message is a tag byte, a little-endian `u32` length, then the body.
//! Decoding checks every length: a damaged or hostile stream is an error,
//! never a panic.

use ansi::{Cell, CellWidth, Color, Cursor, Screen, Style};
use std::io::{self, Read, Write};

/// From the host.
#[derive(Debug, Clone, PartialEq)]
pub enum FromHost {
    Update(Update),
    /// A shell-integration mark at an output line.
    Mark {
        kind: u8,
        line: u64,
        exit: Option<i32>,
    },
    /// The program ended with this code; the host exits after sending it.
    Exited(i32),
    /// Lines `first..` asked for with [`ToHost::History`]; a line the host no
    /// longer holds comes back empty. `wrapped[i]`: line `first + i` ran on
    /// into the next (autowrap).
    History {
        first: u64,
        lines: Vec<Vec<Cell>>,
        wrapped: Vec<bool>,
    },
    /// Block `block` of generation `generation` (the folder `dir` in the
    /// pane's log folder) is durable in the log: the window may read it and
    /// build its search signature on the GPU (design §8.8a).
    BlockReady {
        generation: u32,
        dir: String,
        block: u64,
    },
}

/// To the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHost {
    Input(Vec<u8>),
    Resize {
        rows: u16,
        cols: u16,
    },
    /// Hidden: stop parsing, only log; or catch up and parse again.
    Shed(bool),
    /// Send lines `first..first + count` (numbered as the mirror counts:
    /// lines scrolled off, then screen rows).
    History {
        first: u64,
        count: u32,
    },
    /// The search signature of a block announced with
    /// [`FromHost::BlockReady`], for the host's logger to store.
    Signature {
        generation: u32,
        block: u64,
        sig: Vec<u8>,
    },
}

/// One screen update.
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// Lines that scrolled off the top of the primary screen since the last
    /// update.
    pub scrolled: u64,
    /// How many of the lines that scrolled off the host still holds: how far
    /// back the window may ask for them ([`ToHost::History`]).
    pub held: u64,
    /// The full-screen (alternate) screen is showing.
    pub alt: bool,
    /// The program's cursor is shown (`?25h`), and it wants bracketed paste
    /// (`?2004h`).
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    /// The mouse reporting it asked for (`?1000`/`?1002`/`?1003`, `?1006`).
    pub mouse: vterm::MouseTracking,
    pub mouse_sgr: bool,
    /// Every row's wrap mark (it ran on into the next row), top first.
    pub wrapped: Vec<bool>,
    /// Rows that changed, with their cells.
    pub rows: Vec<(u16, Vec<Cell>)>,
    pub cursor: (u16, u16),
    /// The screen's size, so a resize is seen with its first rows.
    pub size: (u16, u16),
}

/// Works out updates from the host's emulator.
#[derive(Debug, Default)]
pub struct Sender {
    sent: Vec<Vec<Cell>>,
    scrolled: u64,
    alt: bool,
    size: (usize, usize),
}

fn row_of(s: &Screen, r: usize) -> Vec<Cell> {
    (0..s.cols()).map(|c| s.cell(r, c)).collect()
}

impl Sender {
    /// What changed since the last update, or `None` if nothing did.
    pub fn update(&mut self, term: &vterm::Term) -> Option<Update> {
        let screen = term.screen();
        let history = term.scrollback();
        let (rows, cols) = (screen.rows(), screen.cols());
        let alt = term.in_alternate_screen();
        let scrolled = history.scrolled_lines().saturating_sub(self.scrolled);
        let resized = (rows, cols) != self.size;
        let switched = alt != self.alt;
        // What the mirror shows after it scrolls the same way.
        if resized || switched {
            self.sent.clear();
        } else if !alt {
            let k = (scrolled as usize).min(self.sent.len());
            self.sent.drain(..k);
        }
        let mut changed = Vec::new();
        for r in 0..rows {
            let now = row_of(screen, r);
            if self.sent.get(r) != Some(&now) {
                changed.push((r as u16, now));
            }
        }
        self.sent = (0..rows).map(|r| row_of(screen, r)).collect();
        self.scrolled = history.scrolled_lines();
        self.alt = alt;
        self.size = (rows, cols);
        if changed.is_empty() && scrolled == 0 && !resized && !switched {
            // The cursor alone moved: still worth sending.
        }
        let c = screen.cursor();
        Some(Update {
            scrolled,
            held: history.history_len() as u64,
            alt,
            cursor_visible: term.cursor_visible(),
            bracketed_paste: term.bracketed_paste(),
            mouse: term.mouse_tracking(),
            mouse_sgr: term.mouse_sgr(),
            wrapped: (0..rows).map(|r| screen.row_wrapped(r)).collect(),
            rows: changed,
            cursor: (c.row as u16, c.col as u16),
            size: (rows as u16, cols as u16),
        })
    }
}

/// Line `n` of the host's pane, numbered as the mirror numbers lines: from
/// history while it holds it, from the screen for rows still on it; `None`
/// once it is gone from both.
pub fn host_line(term: &vterm::Term, n: u64) -> Option<Vec<Cell>> {
    let history = term.scrollback();
    let scrolled = history.scrolled_lines();
    if n >= scrolled {
        let r = (n - scrolled) as usize;
        let s = term.scrollback();
        return (r < s.rows()).then(|| row_of(s, r));
    }
    let age = (scrolled - 1 - n) as usize;
    let mut line = Vec::new();
    history.history_row(age, &mut line).then_some(line)
}

/// Whether line `n` of the host's pane (numbered as [`host_line`] numbers
/// them) ran on into the next.
pub fn host_wrapped(term: &vterm::Term, n: u64) -> bool {
    let history = term.scrollback();
    let scrolled = history.scrolled_lines();
    if n >= scrolled {
        return history.row_wrapped((n - scrolled) as usize);
    }
    history.history_wrapped((scrolled - 1 - n) as usize)
}

/// The window's display-only copy of a pane's screen.
pub struct Mirror {
    pub primary: Screen,
    pub alt: Screen,
    pub in_alt: bool,
    /// Lines scrolled off the top of the primary screen, as the host counts.
    pub scrolled: u64,
    /// How many of those the host still holds in memory.
    pub held: u64,
    /// The program's modes, as the host's emulator last had them.
    pub cursor_visible: bool,
    pub bracketed_paste: bool,
    pub mouse: vterm::MouseTracking,
    pub mouse_sgr: bool,
}

impl Mirror {
    pub fn new(rows: usize, cols: usize) -> Mirror {
        Mirror {
            primary: Screen::new(rows, cols),
            alt: Screen::new(rows, cols),
            in_alt: false,
            scrolled: 0,
            held: 0,
            cursor_visible: true,
            bracketed_paste: false,
            mouse: vterm::MouseTracking::Off,
            mouse_sgr: false,
        }
    }

    /// The screen to draw.
    pub fn screen(&self) -> &Screen {
        if self.in_alt {
            &self.alt
        } else {
            &self.primary
        }
    }

    pub fn apply(&mut self, u: &Update) {
        let (rows, cols) = (u.size.0.max(1) as usize, u.size.1.max(1) as usize);
        if (rows, cols) != (self.primary.rows(), self.primary.cols()) {
            self.primary = Screen::new(rows, cols);
            self.alt = Screen::new(rows, cols);
        }
        // Scroll as the host did; a whole screen or more of scrolling
        // replaces every row, and the update carries them all.
        self.scrolled += u.scrolled;
        self.held = u.held.min(self.scrolled);
        let blank = Cell::default();
        let n = (u.scrolled as usize).min(rows);
        if n > 0 {
            self.primary.scroll_rows_up(0, rows - 1, n, blank);
        }
        let mut row = Vec::with_capacity(cols);
        self.in_alt = u.alt;
        self.cursor_visible = u.cursor_visible;
        self.bracketed_paste = u.bracketed_paste;
        self.mouse = u.mouse;
        self.mouse_sgr = u.mouse_sgr;
        let screen = if u.alt {
            &mut self.alt
        } else {
            &mut self.primary
        };
        for (r, cells) in &u.rows {
            let r = *r as usize;
            if r < rows {
                padded(cells, cols, &mut row);
                screen.copy_cells(r, 0, &row);
            }
        }
        for r in 0..rows {
            screen.set_row_wrapped(r, u.wrapped.get(r).copied().unwrap_or(false));
        }
        screen.set_cursor(Cursor::new(
            (u.cursor.0 as usize).min(rows - 1),
            (u.cursor.1 as usize).min(cols - 1),
        ));
    }
}

/// `cells` cut or padded with blanks to exactly `cols`.
fn padded(cells: &[Cell], cols: usize, out: &mut Vec<Cell>) {
    out.clear();
    out.extend_from_slice(&cells[..cells.len().min(cols)]);
    out.resize(cols, Cell::default());
}

// --- encoding ----------------------------------------------------------------

const T_UPDATE: u8 = 1;
const T_MARK: u8 = 2;
const T_EXITED: u8 = 3;
const T_HISTORY: u8 = 4;
const T_BLOCK: u8 = 5;
const T_INPUT: u8 = 11;
const T_RESIZE: u8 = 12;
const T_SHED: u8 = 13;
const T_ASK: u8 = 14;
const T_SIG: u8 = 15;

/// Largest message body accepted: a bound on what a damaged stream can make
/// the reader allocate.
const MAX_BODY: usize = 64 << 20;

fn put_color(out: &mut Vec<u8>, c: Color) {
    match c {
        Color::Default => out.extend_from_slice(&[0, 0, 0, 0]),
        Color::Indexed(i) => out.extend_from_slice(&[1, i, 0, 0]),
        Color::Rgb(r, g, b) => out.extend_from_slice(&[2, r, g, b]),
    }
}

/// A run: `count` cells of one style and width.
fn put_run(out: &mut Vec<u8>, count: u16, c: &Cell) {
    out.extend_from_slice(&count.to_le_bytes());
    put_color(out, c.style.fg);
    put_color(out, c.style.bg);
    let s = c.style;
    let flags = (s.bold as u8)
        | (s.dim as u8) << 1
        | (s.italic as u8) << 2
        | (s.underline as u8) << 3
        | (s.reverse as u8) << 4
        | (s.strike as u8) << 5
        | ((c.width == CellWidth::Continuation) as u8) << 6
        | ((c.width == CellWidth::Wide) as u8) << 7;
    out.push(flags);
}

/// A row as runs of style then its text: trailing blank cells dropped.
fn put_row(out: &mut Vec<u8>, cells: &[Cell]) {
    let blank = Cell::default();
    let n = cells.iter().rposition(|c| *c != blank).map_or(0, |i| i + 1);
    let cells = &cells[..n.min(u16::MAX as usize)];
    let mut runs: Vec<(u16, Cell)> = Vec::new();
    for c in cells {
        match runs.last_mut() {
            Some((k, r)) if r.style == c.style && r.width == c.width && *k < u16::MAX => *k += 1,
            _ => runs.push((1, *c)),
        }
    }
    out.extend_from_slice(&(runs.len() as u16).to_le_bytes());
    for (k, c) in &runs {
        put_run(out, *k, c);
    }
    let text: String = cells.iter().map(|c| c.ch).collect();
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Reads a message body, checking every length.
struct Body<'a> {
    b: &'a [u8],
    at: usize,
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "damaged pane-host message")
}

impl Body<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.b.len())
            .ok_or_else(bad)?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn color(&mut self) -> io::Result<Color> {
        let c = self.take(4)?;
        Ok(match c[0] {
            0 => Color::Default,
            1 => Color::Indexed(c[1]),
            2 => Color::Rgb(c[1], c[2], c[3]),
            _ => return Err(bad()),
        })
    }
    /// A run's count and its cell, with a placeholder char.
    fn run(&mut self) -> io::Result<(u16, Cell)> {
        let count = self.u16()?;
        let (fg, bg) = (self.color()?, self.color()?);
        let f = self.u8()?;
        let bit = |n: u8| f & (1 << n) != 0;
        let cell = Cell {
            ch: ' ',
            style: Style {
                fg,
                bg,
                bold: bit(0),
                dim: bit(1),
                italic: bit(2),
                underline: bit(3),
                reverse: bit(4),
                strike: bit(5),
            },
            width: if bit(6) {
                CellWidth::Continuation
            } else if bit(7) {
                CellWidth::Wide
            } else {
                CellWidth::default()
            },
        };
        Ok((count, cell))
    }
    fn row(&mut self) -> io::Result<Vec<Cell>> {
        let nruns = self.u16()? as usize;
        // Each run is 11 bytes: refuse a count the body cannot hold.
        if nruns > (self.b.len() - self.at) / 11 {
            return Err(bad());
        }
        let runs = (0..nruns)
            .map(|_| self.run())
            .collect::<io::Result<Vec<_>>>()?;
        let len = self.u32()? as usize;
        let text = std::str::from_utf8(self.take(len)?).map_err(|_| bad())?;
        let mut chars = text.chars();
        let mut out = Vec::with_capacity(runs.iter().map(|r| r.0 as usize).sum());
        for (count, cell) in runs {
            for _ in 0..count {
                let ch = chars.next().ok_or_else(bad)?;
                out.push(Cell { ch, ..cell });
            }
        }
        if chars.next().is_some() {
            return Err(bad());
        }
        Ok(out)
    }
}

fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(tag);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes(head[1..].try_into().unwrap()) as usize;
    if len > MAX_BODY {
        return Err(bad());
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok((head[0], body))
}

impl FromHost {
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let mut b = Vec::new();
        let tag = match self {
            FromHost::Update(u) => {
                b.extend_from_slice(&u.scrolled.to_le_bytes());
                // Flags: the alternate screen, the cursor shown, bracketed
                // paste, the mouse tracking (two bits) and its SGR encoding.
                let tracking = match u.mouse {
                    vterm::MouseTracking::Off => 0u8,
                    vterm::MouseTracking::Click => 1,
                    vterm::MouseTracking::Drag => 2,
                    vterm::MouseTracking::Motion => 3,
                };
                b.push(
                    u.alt as u8
                        | (u.cursor_visible as u8) << 1
                        | (u.bracketed_paste as u8) << 2
                        | tracking << 3
                        | (u.mouse_sgr as u8) << 5,
                );
                b.extend_from_slice(&u.size.0.to_le_bytes());
                b.extend_from_slice(&u.size.1.to_le_bytes());
                b.extend_from_slice(&u.cursor.0.to_le_bytes());
                b.extend_from_slice(&u.cursor.1.to_le_bytes());
                b.extend_from_slice(&(u.rows.len() as u32).to_le_bytes());
                for (r, cells) in &u.rows {
                    b.extend_from_slice(&r.to_le_bytes());
                    put_row(&mut b, cells);
                }
                // Wrap marks: a count, then a bit per row.
                b.extend_from_slice(&(u.wrapped.len() as u16).to_le_bytes());
                for chunk in u.wrapped.chunks(8) {
                    let byte = chunk
                        .iter()
                        .enumerate()
                        .fold(0u8, |acc, (i, &w)| acc | (w as u8) << i);
                    b.push(byte);
                }
                b.extend_from_slice(&u.held.to_le_bytes());
                T_UPDATE
            }
            FromHost::Mark { kind, line, exit } => {
                b.push(*kind);
                b.extend_from_slice(&line.to_le_bytes());
                b.push(exit.is_some() as u8);
                b.extend_from_slice(&exit.unwrap_or(0).to_le_bytes());
                T_MARK
            }
            FromHost::Exited(code) => {
                b.extend_from_slice(&code.to_le_bytes());
                T_EXITED
            }
            FromHost::History {
                first,
                lines,
                wrapped,
            } => {
                b.extend_from_slice(&first.to_le_bytes());
                b.extend_from_slice(&(lines.len() as u32).to_le_bytes());
                for (i, line) in lines.iter().enumerate() {
                    put_row(&mut b, line);
                    b.push(wrapped.get(i).copied().unwrap_or(false) as u8);
                }
                T_HISTORY
            }
            FromHost::BlockReady {
                generation,
                dir,
                block,
            } => {
                b.extend_from_slice(&generation.to_le_bytes());
                b.extend_from_slice(&block.to_le_bytes());
                b.extend_from_slice(dir.as_bytes());
                T_BLOCK
            }
        };
        w.write_all(&frame(tag, &b))
    }

    pub fn read(r: &mut impl Read) -> io::Result<FromHost> {
        let (tag, body) = read_frame(r)?;
        let mut b = Body { b: &body, at: 0 };
        let msg = match tag {
            T_UPDATE => {
                let scrolled = b.u64()?;
                let flags = b.u8()?;
                let alt = flags & 1 != 0;
                let size = (b.u16()?, b.u16()?);
                let cursor = (b.u16()?, b.u16()?);
                let n = b.u32()? as usize;
                let mut rows = Vec::new();
                for _ in 0..n {
                    let r = b.u16()?;
                    rows.push((r, b.row()?));
                }
                let marks = b.u16()? as usize;
                let mut wrapped = Vec::with_capacity(marks);
                for k in 0..marks.div_ceil(8) {
                    let byte = b.u8()?;
                    for i in 0..8 {
                        if k * 8 + i < marks {
                            wrapped.push(byte & (1 << i) != 0);
                        }
                    }
                }
                let held = b.u64()?;
                FromHost::Update(Update {
                    scrolled,
                    held,
                    alt,
                    cursor_visible: flags & 2 != 0,
                    bracketed_paste: flags & 4 != 0,
                    mouse: match (flags >> 3) & 3 {
                        0 => vterm::MouseTracking::Off,
                        1 => vterm::MouseTracking::Click,
                        2 => vterm::MouseTracking::Drag,
                        _ => vterm::MouseTracking::Motion,
                    },
                    mouse_sgr: flags & 32 != 0,
                    wrapped,
                    rows,
                    cursor,
                    size,
                })
            }
            T_MARK => {
                let kind = b.u8()?;
                let line = b.u64()?;
                let has = b.u8()? != 0;
                let exit = b.u32()? as i32;
                FromHost::Mark {
                    kind,
                    line,
                    exit: has.then_some(exit),
                }
            }
            T_EXITED => FromHost::Exited(b.u32()? as i32),
            T_HISTORY => {
                let first = b.u64()?;
                let n = b.u32()? as usize;
                // Each row is at least 6 bytes: refuse a count the body cannot hold.
                if n > body.len() / 6 {
                    return Err(bad());
                }
                let mut lines = Vec::with_capacity(n);
                let mut wrapped = Vec::with_capacity(n);
                for _ in 0..n {
                    lines.push(b.row()?);
                    wrapped.push(b.u8()? != 0);
                }
                FromHost::History {
                    first,
                    lines,
                    wrapped,
                }
            }
            T_BLOCK => {
                let generation = b.u32()?;
                let block = b.u64()?;
                let dir = std::str::from_utf8(&body[b.at..]).map_err(|_| bad())?;
                b.at = body.len();
                FromHost::BlockReady {
                    generation,
                    dir: dir.to_string(),
                    block,
                }
            }
            _ => return Err(bad()),
        };
        if b.at != body.len() {
            return Err(bad());
        }
        Ok(msg)
    }
}

impl ToHost {
    pub fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let (tag, b) = match self {
            ToHost::Input(bytes) => (T_INPUT, bytes.clone()),
            ToHost::Resize { rows, cols } => {
                let mut b = rows.to_le_bytes().to_vec();
                b.extend_from_slice(&cols.to_le_bytes());
                (T_RESIZE, b)
            }
            ToHost::Shed(on) => (T_SHED, vec![*on as u8]),
            ToHost::History { first, count } => {
                let mut b = first.to_le_bytes().to_vec();
                b.extend_from_slice(&count.to_le_bytes());
                (T_ASK, b)
            }
            ToHost::Signature {
                generation,
                block,
                sig,
            } => {
                let mut b = generation.to_le_bytes().to_vec();
                b.extend_from_slice(&block.to_le_bytes());
                b.extend_from_slice(sig);
                (T_SIG, b)
            }
        };
        w.write_all(&frame(tag, &b))
    }

    pub fn read(r: &mut impl Read) -> io::Result<ToHost> {
        let (tag, body) = read_frame(r)?;
        let mut b = Body { b: &body, at: 0 };
        let msg = match tag {
            T_INPUT => return Ok(ToHost::Input(body)),
            T_RESIZE => ToHost::Resize {
                rows: b.u16()?,
                cols: b.u16()?,
            },
            T_SHED => ToHost::Shed(b.u8()? != 0),
            T_ASK => ToHost::History {
                first: b.u64()?,
                count: b.u32()?,
            },
            T_SIG => {
                let generation = b.u32()?;
                let block = b.u64()?;
                let sig = body[b.at..].to_vec();
                b.at = body.len();
                ToHost::Signature {
                    generation,
                    block,
                    sig,
                }
            }
            _ => return Err(bad()),
        };
        if b.at != body.len() {
            return Err(bad());
        }
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_rows(s: &Screen) -> Vec<Vec<Cell>> {
        (0..s.rows()).map(|r| row_of(s, r)).collect()
    }

    /// Output with colours, long lines that wrap, and a trip to the
    /// alternate screen and back.
    fn output(seed: u32) -> Vec<u8> {
        let mut x = seed.max(1);
        let mut out = Vec::new();
        for i in 0..3000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            match x % 11 {
                0 => out
                    .extend_from_slice(format!("\x1b[3{}mcolour {i}\x1b[0m\r\n", x % 8).as_bytes()),
                1 => out.extend_from_slice(
                    format!("{}\r\n", "w".repeat((x % 150) as usize)).as_bytes(),
                ),
                2 if i % 500 == 2 => {
                    out.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[5;5Hfull screen\x1b[?1049l")
                }
                3 => out.extend_from_slice("wide \u{4e2d}\u{6587}\r\n".as_bytes()),
                _ => out.extend_from_slice(format!("line {i} {}\r\n", x % 1000).as_bytes()),
            }
        }
        out
    }

    #[test]
    fn the_mirror_matches_the_host_through_any_chunking_and_pacing() {
        for seed in [1u32, 7, 99, 12345] {
            let data = output(seed);
            let mut term = vterm::Term::with_history(12, 40, 500);
            let mut sender = Sender::default();
            let mut mirror = Mirror::new(12, 40);
            let mut x = seed | 1;
            let mut at = 0;
            while at < data.len() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                // Chunks of 1 byte to 8 KiB; an update after some of them,
                // so some updates cover many screens of scrolling.
                let n = (1 + (x % 8192) as usize).min(data.len() - at);
                term.feed(&data[at..at + n]);
                at += n;
                if x % 3 == 0 || at == data.len() {
                    let u = sender.update(&term).unwrap();
                    // Through the encoding, as it would travel.
                    let mut bytes = Vec::new();
                    FromHost::Update(u).write(&mut bytes).unwrap();
                    match FromHost::read(&mut bytes.as_slice()).unwrap() {
                        FromHost::Update(u) => mirror.apply(&u),
                        other => panic!("{other:?}"),
                    }
                    assert_eq!(
                        screen_rows(mirror.screen()),
                        screen_rows(term.screen()),
                        "seed {seed}"
                    );
                    assert_eq!(mirror.screen().cursor(), term.screen().cursor());
                    assert_eq!(mirror.scrolled, term.scrollback().scrolled_lines());
                    // How far back the window may ask for lines.
                    assert_eq!(mirror.held, term.scrollback().history_len() as u64);
                }
            }
        }
    }

    #[test]
    fn the_host_finds_a_line_in_history_or_on_screen_by_the_mirrors_numbering() {
        let mut term = vterm::Term::with_history(4, 10, 5);
        for i in 0..12 {
            term.feed(format!("L{i}\r\n").as_bytes());
        }
        let text = |n| {
            host_line(&term, n).map(|l| {
                l.iter()
                    .map(|c| c.ch)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
        };
        // 12 lines and the cursor's empty one: 9 scrolled off, 5 kept.
        assert_eq!(term.scrollback().scrolled_lines(), 9);
        assert_eq!(text(8).as_deref(), Some("L8"), "newest in history");
        assert_eq!(text(4).as_deref(), Some("L4"), "oldest kept");
        assert_eq!(text(3), None, "beyond the history");
        assert_eq!(text(9).as_deref(), Some("L9"), "top screen row");
        assert_eq!(text(12).as_deref(), Some(""), "the cursor's row");
        assert_eq!(text(13), None, "past the screen");
    }

    #[test]
    fn a_resize_reaches_the_mirror() {
        let mut term = vterm::Term::with_history(10, 30, 100);
        let (mut sender, mut mirror) = (Sender::default(), Mirror::new(10, 30));
        term.feed(b"one\r\ntwo\r\n");
        mirror.apply(&sender.update(&term).unwrap());
        term.resize(6, 50);
        term.feed(b"three\r\n");
        mirror.apply(&sender.update(&term).unwrap());
        assert_eq!((mirror.screen().rows(), mirror.screen().cols()), (6, 50));
        assert_eq!(screen_rows(mirror.screen()), screen_rows(term.screen()));
    }

    #[test]
    fn messages_round_trip_and_damage_is_an_error_not_a_panic() {
        let msgs = [
            FromHost::Mark {
                kind: b'D',
                line: 42,
                exit: Some(-3),
            },
            FromHost::Mark {
                kind: b'A',
                line: 7,
                exit: None,
            },
            FromHost::Exited(9),
            FromHost::History {
                first: 1000,
                lines: vec![
                    vec![],
                    vec![Cell {
                        ch: 'q',
                        ..Cell::default()
                    }],
                ],
                wrapped: vec![false, true],
            },
            FromHost::BlockReady {
                generation: 3,
                dir: "gen-1758800000000-00003".to_string(),
                block: 17,
            },
        ];
        for m in &msgs {
            let mut b = Vec::new();
            m.write(&mut b).unwrap();
            assert_eq!(&FromHost::read(&mut b.as_slice()).unwrap(), m);
            // Every truncation fails cleanly.
            for cut in 0..b.len() {
                assert!(FromHost::read(&mut &b[..cut]).is_err());
            }
        }
        for m in [
            ToHost::Input(b"dir\r".to_vec()),
            ToHost::Resize {
                rows: 40,
                cols: 120,
            },
            ToHost::Shed(true),
            ToHost::History {
                first: 12,
                count: 40,
            },
            ToHost::Signature {
                generation: 2,
                block: 9,
                sig: vec![7u8; 8192],
            },
        ] {
            let mut b = Vec::new();
            m.write(&mut b).unwrap();
            assert_eq!(ToHost::read(&mut b.as_slice()).unwrap(), m);
        }
        // Garbage: a huge length, an unknown tag, a bad char.
        assert!(FromHost::read(&mut &[1u8, 0xFF, 0xFF, 0xFF, 0x7F][..]).is_err());
        assert!(FromHost::read(&mut &[99u8, 0, 0, 0, 0][..]).is_err());
        let mut b = Vec::new();
        let mut u = Update {
            scrolled: 0,
            held: 9_999,
            alt: false,
            // Not the defaults: they must survive the trip.
            cursor_visible: false,
            bracketed_paste: true,
            mouse: vterm::MouseTracking::Drag,
            mouse_sgr: true,
            wrapped: vec![true],
            rows: vec![(
                0,
                vec![Cell {
                    ch: 'x',
                    ..Cell::default()
                }],
            )],
            cursor: (0, 0),
            size: (1, 1),
        };
        FromHost::Update(u.clone()).write(&mut b).unwrap();
        match FromHost::read(&mut b.as_slice()).unwrap() {
            FromHost::Update(back) => {
                assert!(!back.cursor_visible && back.bracketed_paste, "modes kept");
                assert_eq!(
                    (back.mouse, back.mouse_sgr),
                    (vterm::MouseTracking::Drag, true),
                    "mouse modes kept"
                );
                assert_eq!(back.held, 9_999, "history held kept");
                assert_eq!(back.wrapped, [true], "wrap marks kept");
                assert_eq!(back.rows, u.rows);
            }
            _ => panic!("not an update"),
        }
        // The row's text: its one byte, 'x' (the wrap marks come after it).
        let at = b.iter().rposition(|&c| c == b'x').expect("the row's text");
        assert_eq!(b[at], b'x');
        b[at] = 0xFF; // not UTF-8
        assert!(FromHost::read(&mut b.as_slice()).is_err());
        u.rows.clear();
    }
}
