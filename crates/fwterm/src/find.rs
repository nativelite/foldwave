//! Search across panes (M4): the parts that need no window.
//!
//! Every pane's log carries a search index (panelog's signatures, built on
//! the GPU, `indexer`), so a query reads only the blocks that can match. The
//! app asks each pane's log for its hits and shows them one at a time, newest
//! first; these helpers order the hits, clean a log line for the search bar,
//! and find the match on a rebuilt screen row to highlight it.

/// One match in one pane's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub pane: usize,
    /// The pane's log folder: a closed pane's hits are shown from it.
    pub dir: std::path::PathBuf,
    /// Line in the pane's logs, counted across its generations (as the log
    /// view counts them).
    pub line: u64,
    /// The line, cleaned for display.
    pub text: String,
    /// When its generation started (ms) and its offset there: newest first.
    pub generation: u64,
    pub offset: u64,
}

/// Newest first across panes: by generation start, then offset.
pub fn order(hits: &mut [Hit]) {
    hits.sort_by(|a, b| (b.generation, b.offset).cmp(&(a.generation, a.offset)));
}

/// A raw log line as text for the search bar: escape sequences (CSI, OSC,
/// two-byte ESC) and other control bytes removed, invalid UTF-8 replaced,
/// at most `max` characters.
pub fn clean(raw: &[u8], max: usize) -> String {
    let mut out = Vec::with_capacity(raw.len().min(max * 4));
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            0x1B => {
                i += 1;
                match raw.get(i) {
                    Some(b'[') => {
                        // CSI: parameters and intermediates, then a final byte.
                        i += 1;
                        while i < raw.len() && !(0x40..=0x7E).contains(&raw[i]) {
                            i += 1;
                        }
                        i += 1;
                    }
                    Some(b']') | Some(b'P') | Some(b'_') | Some(b'^') | Some(b'X') => {
                        // A string: up to BEL or ESC \.
                        i += 1;
                        while i < raw.len() && raw[i] != 0x07 && raw[i] != 0x1B {
                            i += 1;
                        }
                        if raw.get(i) == Some(&0x1B) {
                            i += 1;
                        }
                        i += 1;
                    }
                    Some(_) => i += 1,
                    None => {}
                }
            }
            b'\t' => {
                out.push(b' ');
                i += 1;
            }
            b if b < 0x20 || b == 0x7F => i += 1,
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).chars().take(max).collect()
}

/// Where `needle` sits in a screen row, ignoring ASCII case: the first and
/// last columns it covers. `row` has one char per column.
pub fn match_in_row(row: &[char], needle: &str) -> Option<(usize, usize)> {
    let n: Vec<char> = needle.chars().collect();
    if n.is_empty() || n.len() > row.len() {
        return None;
    }
    (0..=row.len() - n.len())
        .find(|&c| {
            row[c..c + n.len()]
                .iter()
                .zip(&n)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
        .map(|c| (c, c + n.len() - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(pane: usize, generation: u64, offset: u64) -> Hit {
        Hit {
            pane,
            dir: Default::default(),
            line: 0,
            text: String::new(),
            generation,
            offset,
        }
    }

    #[test]
    fn hits_from_every_pane_come_newest_first() {
        let mut h = vec![
            hit(0, 100, 5),
            hit(1, 200, 1),
            hit(0, 100, 90),
            hit(2, 50, 7),
        ];
        order(&mut h);
        let got: Vec<(usize, u64)> = h.iter().map(|h| (h.pane, h.offset)).collect();
        assert_eq!(got, vec![(1, 1), (0, 90), (0, 5), (2, 7)]);
    }

    #[test]
    fn log_lines_are_cleaned_of_escapes_and_controls() {
        let raw = b"\x1b[31merror\x1b[0m: \x1b]133;D\x1b\\build\tfailed\x07\r\x1bM!";
        assert_eq!(clean(raw, 100), "error: build failed!");
        assert_eq!(clean(b"abcdef", 3), "abc");
        assert_eq!(clean("h\u{e9}llo".as_bytes(), 10), "h\u{e9}llo");
        // A sequence cut off at the end of the line.
        assert_eq!(clean(b"ok\x1b[3", 10), "ok");
    }

    #[test]
    fn a_match_is_found_in_a_row_ignoring_case() {
        let row: Vec<char> = "  Build FAILED at step 3 ".chars().collect();
        assert_eq!(match_in_row(&row, "failed"), Some((8, 13)));
        assert_eq!(match_in_row(&row, "missing"), None);
        assert_eq!(match_in_row(&row, ""), None);
        assert_eq!(match_in_row(&['a'], "ab"), None);
    }
}
