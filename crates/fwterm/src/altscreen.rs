//! Full-screen stretches of a pane's log (founder, 2026-09-30: skipped when
//! scrolling back, reachable through search). A program on the alternate
//! screen (atrium, vim, Claude Code in atrium) draws and redraws; other
//! terminals keep none of that in scrollback, so scrolling up after it quits
//! shows the shell history before it. The log keeps every byte, so the
//! switches are marked there (`E` entering, `X` leaving), and the log view
//! counts only the lines outside those stretches. A search hit inside one
//! still opens that screen.

/// The mark kinds: entering and leaving the alternate screen.
pub const ENTER: u8 = b'E';
pub const LEAVE: u8 = b'X';

/// Finds alternate-screen switches (`CSI ? 1049/1047/47 h/l`) in output
/// given chunk by chunk, a switch split across chunks included.
#[derive(Debug, Default)]
pub struct Scanner {
    /// The start of a sequence the last chunk ended inside.
    carry: Vec<u8>,
}

impl Scanner {
    /// The switches in `chunk`: each one's offset in it (where its final
    /// byte is) and whether it enters.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<(usize, bool)> {
        let carried = self.carry.len();
        let mut bytes = std::mem::take(&mut self.carry);
        bytes.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(p) = bytes[i..].windows(3).position(|w| w == b"\x1b[?") {
            let start = i + p;
            let mut j = start + 3;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j == bytes.len() {
                // Cut off: finish it with the next chunk (a sane length only).
                if j - start < 32 {
                    self.carry = bytes[start..].to_vec();
                }
                break;
            }
            let fin = bytes[j];
            if fin == b'h' || fin == b'l' {
                let params = &bytes[start + 3..j];
                let alt = params
                    .split(|&b| b == b';')
                    .any(|p| matches!(p, b"1049" | b"1047" | b"47"));
                if alt {
                    out.push((j.saturating_sub(carried), fin == b'h'));
                }
            }
            i = j;
        }
        if self.carry.is_empty() {
            // An ESC or `ESC [` at the very end starts a sequence too.
            for tail in [&b"\x1b["[..], &b"\x1b"[..]] {
                if bytes.ends_with(tail) {
                    self.carry = tail.to_vec();
                    break;
                }
            }
        }
        out
    }
}

/// Full-screen stretches as `[first, end)` line ranges, from the switches
/// in log order (`(line, entering)`): each enter to the next leave, one
/// left open running to `total`, and a leave with no enter before it (the
/// enter was in a generation since deleted) from line 0.
pub fn spans(switches: &[(u64, bool)], total: u64) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut open: Option<u64> = None;
    let mut first = true;
    for &(line, enter) in switches {
        match (enter, open) {
            (true, None) => open = Some(line),
            (false, Some(a)) => {
                out.push((a, line.max(a)));
                open = None;
            }
            (false, None) if first => out.push((0, line)),
            _ => {}
        }
        first = false;
    }
    if let Some(a) = open {
        out.push((a, total.max(a)));
    }
    out.retain(|(a, b)| b > a);
    out
}

/// Lines outside the stretches, of `total`.
pub fn visible(total: u64, spans: &[(u64, u64)]) -> u64 {
    total
        - spans
            .iter()
            .map(|&(a, b)| b.min(total) - a.min(total))
            .sum::<u64>()
}

/// The log line a view `back` visible lines above the live end ends
/// before, skipping the stretches.
pub fn line_back(total: u64, spans: &[(u64, u64)], back: u64) -> u64 {
    let mut end = total;
    let mut left = back;
    // A stretch still open at the end (a program that never left): the
    // view starts before it.
    if let Some(&(a, b)) = spans.last() {
        if b >= total {
            end = a.min(total);
        }
    }
    for &(a, b) in spans.iter().rev() {
        if a >= end {
            continue;
        }
        let after = end - b.min(end);
        // A view ending right where the program left still shows its
        // screen (the leave is on that line): go on to before it began.
        if left < after {
            return end - left;
        }
        left -= after;
        end = a;
    }
    end.saturating_sub(left)
}

/// The other way: how many visible lines lie from `line` to the live end.
pub fn back_of(total: u64, spans: &[(u64, u64)], line: u64) -> u64 {
    let line = line.min(total);
    let hidden: u64 = spans
        .iter()
        .map(|&(a, b)| b.min(total).saturating_sub(a.max(line)))
        .sum();
    (total - line) - hidden
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_are_found_in_any_form_and_across_chunks() {
        let mut s = Scanner::default();
        // Offsets are each switch's final byte (`h` or `l`).
        assert_eq!(s.feed(b"ab\x1b[?1049hcd"), [(9, true)]);
        assert_eq!(s.feed(b"\x1b[?25l\x1b[?1049l"), [(13, false)]);
        assert_eq!(
            s.feed(b"\x1b[?47h\x1b[?1047l\x1b[?1000h"),
            [(5, true), (13, false)]
        );
        assert_eq!(s.feed(b"\x1b[?25;1049h"), [(10, true)], "among other modes");
        // Split: the sequence starts in one chunk and ends in the next.
        assert!(s.feed(b"text\x1b[?10").is_empty());
        assert_eq!(s.feed(b"49lmore"), [(2, false)]);
        assert!(s.feed(b"x\x1b").is_empty());
        assert_eq!(s.feed(b"[?1049h"), [(6, true)]);
        assert!(s.feed(b"\x1b[?2004h plain text").is_empty());
    }

    #[test]
    fn spans_pair_enters_with_leaves() {
        assert_eq!(
            spans(&[(10, true), (20, false), (30, true)], 50),
            [(10, 20), (30, 50)]
        );
        assert_eq!(
            spans(&[(5, false), (8, true), (9, false)], 20),
            [(0, 5), (8, 9)]
        );
        assert_eq!(
            spans(&[(3, true), (4, true), (6, false)], 9),
            [(3, 6)],
            "a repeat"
        );
        assert!(spans(&[], 9).is_empty());
    }

    #[test]
    fn scrolling_back_skips_the_stretches_both_ways() {
        // 100 lines; lines 40..70 and 80..90 were full-screen.
        let s = [(40, 70), (80, 90)];
        assert_eq!(visible(100, &s), 60);
        assert_eq!(line_back(100, &s, 0), 100);
        assert_eq!(
            line_back(100, &s, 9),
            91,
            "the lines after the last stretch"
        );
        assert_eq!(
            line_back(100, &s, 10),
            80,
            "at its end, straight to before it began"
        );
        assert_eq!(line_back(100, &s, 11), 79);
        assert_eq!(line_back(100, &s, 20), 40);
        assert_eq!(line_back(100, &s, 21), 39);
        assert_eq!(line_back(100, &s, 60), 0);
        assert_eq!(line_back(100, &s, 99), 0, "no further than the start");
        for back in 0..=60 {
            let line = line_back(100, &s, back);
            assert_eq!(back_of(100, &s, line), back, "round trip at {back}");
        }
        assert_eq!(line_back(100, &[], 30), 70, "no stretches: as before");
        // One still open at the end: the view starts before it.
        assert_eq!(line_back(100, &[(95, 100)], 0), 95);
    }
}
