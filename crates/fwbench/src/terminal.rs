//! The two queries a console host may put to its terminal, and the answers.
//!
//! A real terminal emulator answers these; until now the harness never did.
//! That was harmless with Windows' built-in `conhost.exe`, which never asks,
//! but Windows Terminal's OpenConsole (experiment E2) opens every session with
//! a Primary Device Attributes query (`ESC [ c`) and **draws nothing until it
//! is answered**. A harness that stays silent measures that stall, not the
//! console host.
//!
//! [`Watcher`] scans output for the queries — including one split across two
//! reads — and returns the reply to write back, exactly as foldwave itself
//! will have to.

/// Primary Device Attributes reply: a VT220-class terminal with ANSI colour.
/// Any well-formed answer releases a host waiting on the query.
pub const DA1_REPLY: &[u8] = b"\x1b[?62;22c";
/// Cursor Position Report for a harness that never tracks the cursor: the
/// top-left cell. Only the fact of an answer matters here.
pub const CPR_REPLY: &[u8] = b"\x1b[1;1R";

const DA1: &[u8] = b"\x1b[c";
const DA1_ZERO: &[u8] = b"\x1b[0c";
const DSR_CPR: &[u8] = b"\x1b[6n";
/// Longest query minus one: the bytes to carry into the next read.
const CARRY: usize = 3;

/// Scans a stream of output for terminal queries.
#[derive(Debug, Default)]
pub struct Watcher {
    tail: Vec<u8>,
}

impl Watcher {
    /// A watcher with nothing carried over.
    pub fn new() -> Self {
        Self::default()
    }

    /// The replies owed for the queries in `chunk`, in order. A query split
    /// across this chunk and the previous one is found.
    pub fn replies(&mut self, chunk: &[u8]) -> Vec<&'static [u8]> {
        // Only the carried tail plus the head of the chunk can hide a split
        // query; the rest of the chunk is scanned in place.
        let mut out = Vec::new();
        let carried = self.tail.len();
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(&chunk[..chunk.len().min(CARRY)]);
        scan(&joined, carried, &mut out);
        scan_from(chunk, &mut out);

        // Carry the last bytes of everything seen so far, not just of this
        // chunk: reads shorter than a query must still add up to one.
        self.tail = if chunk.len() >= CARRY {
            chunk[chunk.len() - CARRY..].to_vec()
        } else {
            joined[joined.len().saturating_sub(CARRY)..].to_vec()
        };
        out
    }
}

/// Queries that *start* inside the carried bytes (before `boundary`) and end
/// in the new chunk.
fn scan(joined: &[u8], boundary: usize, out: &mut Vec<&'static [u8]>) {
    for start in 0..boundary {
        for (query, reply) in [
            (DA1, DA1_REPLY),
            (DA1_ZERO, DA1_REPLY),
            (DSR_CPR, CPR_REPLY),
        ] {
            if joined[start..].starts_with(query) && start + query.len() > boundary {
                out.push(reply);
            }
        }
    }
}

/// Queries wholly inside `chunk`.
fn scan_from(chunk: &[u8], out: &mut Vec<&'static [u8]>) {
    let mut i = 0;
    while i < chunk.len() {
        if chunk[i] == 0x1B {
            for (query, reply) in [
                (DA1, DA1_REPLY),
                (DA1_ZERO, DA1_REPLY),
                (DSR_CPR, CPR_REPLY),
            ] {
                if chunk[i..].starts_with(query) {
                    out.push(reply);
                }
            }
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_openconsoles_opening_query() {
        // OpenConsole's first bytes on the reference machine, verbatim.
        let mut w = Watcher::new();
        assert_eq!(
            w.replies(b"\x1b[1t\x1b[c\x1b[?1004h\x1b[?9001h"),
            vec![DA1_REPLY]
        );
    }

    #[test]
    fn answers_both_queries_and_the_explicit_zero_form() {
        let mut w = Watcher::new();
        assert_eq!(w.replies(b"a\x1b[0cb\x1b[6nc"), vec![DA1_REPLY, CPR_REPLY]);
    }

    #[test]
    fn ignores_look_alikes() {
        let mut w = Watcher::new();
        // A private DA (ESC [ > c) and SGR sequences are not queries we answer.
        assert!(w.replies(b"\x1b[>c\x1b[1;31m\x1b[?25l text").is_empty());
    }

    #[test]
    fn finds_a_query_split_at_every_point() {
        for query in [DA1, DA1_ZERO, DSR_CPR] {
            let mut stream = b"prompt> ".to_vec();
            stream.extend_from_slice(query);
            stream.extend_from_slice(b" more");
            for split in 1..stream.len() {
                let mut w = Watcher::new();
                let mut got = w.replies(&stream[..split]);
                got.extend(w.replies(&stream[split..]));
                assert_eq!(got.len(), 1, "query {query:?} split at {split}: {got:?}");
            }
        }
    }

    #[test]
    fn finds_a_query_arriving_one_byte_at_a_time() {
        for query in [DA1, DA1_ZERO, DSR_CPR] {
            let mut stream = b"ab".to_vec();
            stream.extend_from_slice(query);
            stream.extend_from_slice(b"cd");
            let mut w = Watcher::new();
            let got: Vec<_> = stream.iter().flat_map(|b| w.replies(&[*b])).collect();
            assert_eq!(got.len(), 1, "query {query:?}: {got:?}");
        }
    }

    #[test]
    fn a_query_is_answered_once_not_twice_across_reads() {
        let mut w = Watcher::new();
        let mut got = w.replies(b"xx\x1b[c");
        got.extend(w.replies(b"yy"));
        assert_eq!(got, vec![DA1_REPLY]);
    }
}
