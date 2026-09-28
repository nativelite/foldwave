//! A pane's emulator as the window sees it: parsed here (`Local`), or parsed
//! in a pane-host process and mirrored here (`Remote`, `--isolate`, design
//! §8.8a). Drawing, scrollback, selection and blocks read either the same
//! way; only a local one is fed bytes or checkpointed.

use crate::wire::Mirror;
use ansi::Screen;
use vterm::Term;

pub enum Emu {
    Local(Term),
    Remote(Mirror),
}

impl Emu {
    /// The screen to draw.
    pub fn screen(&self) -> &Screen {
        match self {
            Emu::Local(t) => t.screen(),
            Emu::Remote(m) => m.screen(),
        }
    }

    /// Lines scrolled off the top since the pane started: the numbering
    /// selections and blocks use.
    pub fn scrolled_lines(&self) -> u64 {
        match self {
            Emu::Local(t) => t.scrollback().scrolled_lines(),
            Emu::Remote(m) => m.scrolled,
        }
    }

    /// The primary screen, with the history (none for a mirror: an isolated
    /// pane's history is its log).
    pub fn scrollback(&self) -> &Screen {
        match self {
            Emu::Local(t) => t.scrollback(),
            Emu::Remote(m) => &m.primary,
        }
    }

    /// The program's cursor is shown (`?25h`, the default) or hidden.
    pub fn cursor_visible(&self) -> bool {
        match self {
            Emu::Local(t) => t.cursor_visible(),
            Emu::Remote(m) => m.cursor_visible,
        }
    }

    /// The program wants pasted text bracketed (`?2004h`).
    pub fn bracketed_paste(&self) -> bool {
        match self {
            Emu::Local(t) => t.bracketed_paste(),
            Emu::Remote(m) => m.bracketed_paste,
        }
    }

    pub fn in_alternate_screen(&self) -> bool {
        match self {
            Emu::Local(t) => t.in_alternate_screen(),
            Emu::Remote(m) => m.in_alt,
        }
    }

    /// Whether anything changed since the last call. A mirror changes only
    /// when an update arrives, and the pane is marked for drawing then.
    pub fn take_dirty(&mut self) -> bool {
        match self {
            Emu::Local(t) => t.take_dirty(),
            Emu::Remote(_) => true,
        }
    }

    /// Parse output: only here when the emulator is local.
    pub fn feed(&mut self, bytes: &[u8]) {
        if let Emu::Local(t) = self {
            t.feed(bytes);
        }
    }

    pub fn checkpoint(&self) -> Option<Vec<u8>> {
        match self {
            Emu::Local(t) => t.checkpoint(),
            Emu::Remote(_) => None,
        }
    }

    /// A local emulator resizes now; a mirror takes its new size from the
    /// host's next update.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        if let Emu::Local(t) = self {
            t.resize(rows, cols);
        }
    }
}
