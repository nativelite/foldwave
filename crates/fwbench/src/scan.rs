//! The hidden-pane path: a structural scan, not a parse (design 8.0, E4).
//!
//! A pane nobody is looking at keeps no live screen. It needs only what lets
//! it be jumped into and rebuilt later: where each line starts, and where the
//! shell-integration marks (`ESC ] 133 ; ...`, OSC 133) fall. This finds both
//! without running the VT state machine, eight bytes at a time using SWAR
//! (SIMD within a register) on plain `u64`s, so it stays std-only and safe.

const LF: u8 = b'\n';
const ESC: u8 = 0x1B;
const MARK: &[u8] = b"\x1b]133;";

/// What a structural scan found in a slice.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Structure {
    /// Offset of the first byte of every line after the first (one past each
    /// `\n`).
    pub line_starts: Vec<u64>,
    /// Offset of the `ESC` that opens each OSC 133 mark.
    pub marks: Vec<u64>,
}

#[inline(always)]
fn splat(b: u8) -> u64 {
    u64::from_ne_bytes([b; 8])
}

/// Non-zero iff some byte of `x` is zero (the classic haszero test).
#[inline(always)]
fn has_zero(x: u64) -> u64 {
    x.wrapping_sub(splat(0x01)) & !x & splat(0x80)
}

/// Scan `bytes`, whose first byte sits at stream offset `base`.
///
/// A mark split across two calls is not seen; callers scanning a stream in
/// chunks carry the last `MARK.len() - 1` bytes over, as a real indexer does.
pub fn scan(bytes: &[u8], base: u64, out: &mut Structure) {
    let lf = splat(LF);
    let esc = splat(ESC);
    let mut chunks = bytes.chunks_exact(8);
    let mut at = 0usize;
    for c in &mut chunks {
        let w = u64::from_ne_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
        if has_zero(w ^ lf) | has_zero(w ^ esc) != 0 {
            for (i, &b) in c.iter().enumerate() {
                visit(bytes, at + i, b, base, out);
            }
        }
        at += 8;
    }
    for (i, &b) in chunks.remainder().iter().enumerate() {
        visit(bytes, at + i, b, base, out);
    }
}

#[inline(always)]
fn visit(bytes: &[u8], i: usize, b: u8, base: u64, out: &mut Structure) {
    if b == LF {
        out.line_starts.push(base + i as u64 + 1);
    } else if b == ESC && bytes[i..].starts_with(MARK) {
        out.marks.push(base + i as u64);
    }
}

/// Byte-at-a-time reference implementation, for tests.
pub fn scan_naive(bytes: &[u8], base: u64) -> Structure {
    let mut out = Structure::default();
    for i in 0..bytes.len() {
        visit(bytes, i, bytes[i], base, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast(bytes: &[u8], base: u64) -> Structure {
        let mut s = Structure::default();
        scan(bytes, base, &mut s);
        s
    }

    #[test]
    fn finds_lines_and_marks() {
        let s = fast(b"ab\ncd\n\x1b]133;A\x07ls\n\x1b]0;t\x07", 10);
        assert_eq!(s.line_starts, vec![13, 16, 27]);
        assert_eq!(s.marks, vec![16]);
    }

    #[test]
    fn other_escapes_are_not_marks() {
        let s = fast(b"\x1b[1m\x1b]13;x\x07\x1b]1330\x07", 0);
        assert!(s.marks.is_empty());
    }

    #[test]
    fn matches_naive_at_every_alignment() {
        let mut data = Vec::new();
        crate::emit::Generator::new(7).fill(&mut data, 20_000);
        // Sprinkle marks at awkward offsets.
        for (k, at) in [3usize, 4097, 9999, 15_001].iter().enumerate() {
            let tag = [b"\x1b]133;A\x07".as_slice(), b"\x1b]133;D;0\x1b\\"][k % 2];
            data.splice(at..at, tag.iter().copied());
        }
        for shift in 0..9 {
            let slice = &data[shift..];
            assert_eq!(fast(slice, 5), scan_naive(slice, 5), "shift {shift}");
        }
    }

    #[test]
    fn empty_and_tiny_inputs() {
        assert_eq!(fast(b"", 0), Structure::default());
        assert_eq!(fast(b"\n", 0).line_starts, vec![1]);
        assert_eq!(fast(b"\x1b]133;", 0).marks, vec![0]);
    }
}
