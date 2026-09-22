//! The VT tokenizer's state machine alone, as a transition table, to measure
//! whether it can be parsed in parallel (experiment E4, the GPU fit).
//!
//! A byte stream can be tokenized in parallel chunks if a chunk that starts in
//! an *unknown* state can still be parsed: run it from every possible start
//! state at once (Mytkowicz, Musuvathi and Schulte, "Data-parallel
//! finite-state machines", ASPLOS 2014) and pick the right trajectory once the
//! previous chunk's end state is known. It is cheap when the trajectories
//! **converge**: once every start state has reached the same state, the chunk
//! is parsed once, not once per state. This module measures how fast they
//! converge on agent-like output.
//!
//! [`State`] models `nativelite-ansi`'s `Parser` (0.3.1, `src/parse.rs`) state
//! for state: its six states, the pending-ESC flag inside OSC and DCS/SOS/PM/APC
//! strings, and the number of UTF-8 continuation bytes still expected in
//! ground. Payload length caps (`MAX_SEQ`) are not modelled: they only change
//! behaviour on sequences longer than 4 KiB, which agent output does not emit.

/// A tokenizer state. Every byte moves one state to exactly one other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Printable text and C0 controls.
    Ground,
    /// Ground, one UTF-8 continuation byte still expected.
    Utf8One,
    /// Ground, two UTF-8 continuation bytes still expected.
    Utf8Two,
    /// Ground, three UTF-8 continuation bytes still expected.
    Utf8Three,
    /// After `ESC`.
    Escape,
    /// After `ESC` and an intermediate byte.
    EscapeIntermediate,
    /// Inside `ESC [ ...`.
    Csi,
    /// Inside an OSC string (`ESC ] ...`).
    Osc,
    /// Inside an OSC string, just after an `ESC` (a possible `ESC \`).
    OscEsc,
    /// Inside a DCS/SOS/PM/APC string.
    Str,
    /// Inside a DCS/SOS/PM/APC string, just after an `ESC`.
    StrEsc,
}

/// Number of states.
pub const STATES: usize = 11;

/// Every state, in `repr` order.
pub const ALL: [State; STATES] = [
    State::Ground,
    State::Utf8One,
    State::Utf8Two,
    State::Utf8Three,
    State::Escape,
    State::EscapeIntermediate,
    State::Csi,
    State::Osc,
    State::OscEsc,
    State::Str,
    State::StrEsc,
];

impl State {
    /// The state after byte `b`, exactly as `ansi::Parser` moves.
    pub fn next(self, b: u8) -> State {
        use State::*;
        match self {
            Ground => ground(b),
            Utf8One | Utf8Two | Utf8Three => {
                if (0x80..0xC0).contains(&b) {
                    match self {
                        Utf8Three => Utf8Two,
                        Utf8Two => Utf8One,
                        _ => Ground,
                    }
                } else {
                    // Truncated sequence: the parser emits U+FFFD and
                    // reprocesses `b` in ground.
                    ground(b)
                }
            }
            Escape => match b {
                b'[' => Csi,
                b']' => Osc,
                b'P' | b'X' | b'^' | b'_' => Str,
                0x20..=0x2F => EscapeIntermediate,
                0x30..=0x7E => Ground,
                0x1B => Escape,
                0x18 | 0x1A => Ground,
                _ => Escape, // C0 executed inside the sequence
            },
            EscapeIntermediate => match b {
                0x30..=0x7E => Ground,
                0x1B => Escape,
                0x18 | 0x1A => Ground,
                _ => EscapeIntermediate,
            },
            Csi => match b {
                0x40..=0x7E => Ground,
                0x1B => Escape,
                0x18 | 0x1A => Ground,
                _ => Csi,
            },
            Osc => string(b, Osc, OscEsc),
            OscEsc => {
                if b == b'\\' {
                    Ground
                } else {
                    string(b, Osc, OscEsc)
                }
            }
            Str => string(b, Str, StrEsc),
            StrEsc => {
                if b == b'\\' {
                    Ground
                } else {
                    string(b, Str, StrEsc)
                }
            }
        }
    }

    /// True for the string states, which absorb text until a terminator.
    pub fn in_string(self) -> bool {
        matches!(
            self,
            State::Osc | State::OscEsc | State::Str | State::StrEsc
        )
    }
}

fn ground(b: u8) -> State {
    match b {
        0x1B => State::Escape,
        0xC0..=0xDF => State::Utf8One,
        0xE0..=0xEF => State::Utf8Two,
        0xF0..=0xF7 => State::Utf8Three,
        _ => State::Ground,
    }
}

fn string(b: u8, body: State, esc: State) -> State {
    match b {
        0x07 => State::Ground, // BEL terminator
        0x1B => esc,
        _ => body,
    }
}

/// The transition table: `TABLE[state][byte]` is the next state's index.
pub struct Table([[u8; 256]; STATES]);

impl Table {
    /// Build the table from [`State::next`].
    pub fn new() -> Table {
        let mut t = [[0u8; 256]; STATES];
        for (s, row) in ALL.iter().zip(t.iter_mut()) {
            for (b, cell) in row.iter_mut().enumerate() {
                *cell = s.next(b as u8) as u8;
            }
        }
        Table(t)
    }

    /// The state index after `b` from state index `s`.
    #[inline(always)]
    pub fn step(&self, s: u8, b: u8) -> u8 {
        self.0[s as usize][b as usize]
    }

    /// Run one trajectory over `bytes`; returns the end state index.
    pub fn run(&self, start: u8, bytes: &[u8]) -> u8 {
        bytes.iter().fold(start, |s, &b| self.step(s, b))
    }

    /// Run every start state over `bytes` in lockstep (the enumerative
    /// method without the convergence shortcut); returns the end state of
    /// each start state.
    pub fn run_all(&self, bytes: &[u8]) -> [u8; STATES] {
        let mut v: [u8; STATES] = std::array::from_fn(|i| i as u8);
        for &b in bytes {
            for s in v.iter_mut() {
                *s = self.step(*s, b);
            }
        }
        v
    }
}

impl Default for Table {
    fn default() -> Self {
        Table::new()
    }
}

/// How many bytes after `bytes[0]` it takes for start state `from` to reach
/// the same state as the trajectory that starts in [`State::Ground`], or
/// `None` if they have not met by the end of `bytes`.
pub fn converge_with_ground(table: &Table, from: State, bytes: &[u8]) -> Option<usize> {
    let mut a = from as u8;
    let mut g = State::Ground as u8;
    if a == g {
        return Some(0);
    }
    for (i, &b) in bytes.iter().enumerate() {
        a = table.step(a, b);
        g = table.step(g, b);
        if a == g {
            return Some(i + 1);
        }
    }
    None
}

/// The true state at every byte offset of `bytes`, parsed from ground: how
/// often a chunk boundary would land in each state.
pub fn state_histogram(table: &Table, bytes: &[u8]) -> [u64; STATES] {
    let mut h = [0u64; STATES];
    let mut s = State::Ground as u8;
    for &b in bytes {
        h[s as usize] += 1;
        s = table.step(s, b);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(bytes: &[u8]) -> State {
        let t = Table::new();
        ALL[t.run(State::Ground as u8, bytes) as usize]
    }

    #[test]
    fn repr_order_matches_all() {
        for (i, s) in ALL.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
    }

    #[test]
    fn sequences_return_to_ground() {
        assert_eq!(walk(b"plain text\r\n"), State::Ground);
        assert_eq!(walk(b"\x1b[1;32mgreen\x1b[0m"), State::Ground);
        assert_eq!(walk(b"\x1b[?25l"), State::Ground);
        assert_eq!(walk(b"\x1b7"), State::Ground);
        assert_eq!(walk(b"\x1b(B"), State::Ground);
        assert_eq!(walk(b"\x1b]0;title\x07"), State::Ground);
        assert_eq!(walk(b"\x1b]133;A\x1b\\"), State::Ground);
        assert_eq!(walk(b"\x1bPq#0;2;0;0;0\x1b\\"), State::Ground);
        assert_eq!(walk("caf\u{e9} \u{1F600}".as_bytes()), State::Ground);
    }

    #[test]
    fn partial_sequences_hold_their_state() {
        assert_eq!(walk(b"\x1b"), State::Escape);
        assert_eq!(walk(b"\x1b("), State::EscapeIntermediate);
        assert_eq!(walk(b"\x1b[1;3"), State::Csi);
        assert_eq!(walk(b"\x1b]0;ti"), State::Osc);
        assert_eq!(walk(b"\x1b]0;ti\x1b"), State::OscEsc);
        assert_eq!(walk(b"\x1bPdata"), State::Str);
        assert_eq!(walk(b"\x1bPdata\x1b"), State::StrEsc);
        assert_eq!(walk(&[0xE2]), State::Utf8Two);
        assert_eq!(walk(&[0xE2, 0x82]), State::Utf8One);
        assert_eq!(walk(&[0xF0]), State::Utf8Three);
    }

    #[test]
    fn parser_edge_cases_match_ansi() {
        // CAN/SUB abort a CSI; a stray ESC restarts one.
        assert_eq!(walk(b"\x1b[12\x18"), State::Ground);
        assert_eq!(walk(b"\x1b[12\x1b"), State::Escape);
        // C0 inside a CSI is executed and the CSI continues.
        assert_eq!(walk(b"\x1b[1\n"), State::Csi);
        // CAN does not abort a string; only BEL or ST ends one.
        assert_eq!(walk(b"\x1b]0;x\x18y"), State::Osc);
        // ESC not followed by '\' inside a string is payload.
        assert_eq!(walk(b"\x1b]0;x\x1bz"), State::Osc);
        // A truncated UTF-8 sequence falls back to ground and reprocesses.
        assert_eq!(walk(&[0xE2, b'a']), State::Ground);
        assert_eq!(walk(&[0xE2, 0x1B]), State::Escape);
    }

    #[test]
    fn table_matches_next_everywhere() {
        let t = Table::new();
        for s in ALL {
            for b in 0..=255u8 {
                assert_eq!(t.step(s as u8, b), s.next(b) as u8);
            }
        }
    }

    #[test]
    fn run_all_equals_independent_runs() {
        let t = Table::new();
        let bytes = b"ab\x1b[1mc\x1b]0;t\x07d\xE2\x82\xAC";
        let all = t.run_all(bytes);
        for s in ALL {
            assert_eq!(all[s as usize], t.run(s as u8, bytes));
        }
    }

    #[test]
    fn convergence_is_measured_in_bytes() {
        let t = Table::new();
        assert_eq!(converge_with_ground(&t, State::Ground, b"x"), Some(0));
        // From Escape, 'm' is a final byte: back to ground after one byte.
        assert_eq!(converge_with_ground(&t, State::Escape, b"mabc"), Some(1));
        // From Csi, text runs until a final byte (0x40..=0x7E): 'a' is one.
        assert_eq!(converge_with_ground(&t, State::Csi, b"1;2a"), Some(4));
        // A string state absorbs plain text and never meets ground.
        assert_eq!(converge_with_ground(&t, State::Osc, b"plain text\n"), None);
        // ...until a terminator.
        assert_eq!(converge_with_ground(&t, State::Osc, b"ab\x07c"), Some(3));
    }

    #[test]
    fn histogram_counts_every_offset() {
        let t = Table::new();
        let h = state_histogram(&t, b"a\x1b[1mb");
        assert_eq!(h.iter().sum::<u64>(), 6);
        assert_eq!(h[State::Ground as usize], 3); // before 'a', ESC, 'b'
        assert_eq!(h[State::Escape as usize], 1);
        assert_eq!(h[State::Csi as usize], 2);
    }
}
