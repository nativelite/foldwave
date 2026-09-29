//! `fwbench logcheck`: after fwterm is killed mid-flood, is every pane's log
//! an exact prefix of what its program wrote?
//!
//! `replay` (E6) kills one log writer of its own. This checks fwterm itself:
//! run panes whose program is `fwbench --emit` (pipe lane, so the log holds
//! exactly the program's bytes), kill fwterm, then point this at the session
//! folder. Each pane's generations are read in order and compared byte for
//! byte with the seeded stream `--emit` writes; a torn, reordered or missing
//! record shows up as a mismatch (design §4, crash target).

use crate::emit::Generator;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The seed `fwbench --emit` writes with.
pub const EMIT_SEED: u64 = 0xF01D_2A7E;

/// What one pane's log holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCheck {
    /// The pane's folder name (`pane-3`).
    pub pane: String,
    /// Generations found and read.
    pub generations: usize,
    /// Bytes in the log (every generation).
    pub bytes: u64,
    /// Where the log first differs from the stream; `None` when it is an
    /// exact prefix.
    pub mismatch_at: Option<u64>,
}

/// Every `pane-*` folder of `session`, checked, by name.
pub fn check_session(session: &Path) -> io::Result<Vec<PaneCheck>> {
    let mut panes: Vec<PathBuf> = subfolders(session, "pane-")?;
    panes.sort();
    panes.iter().map(|p| check_pane(p)).collect()
}

/// One pane's generations, in order, against the stream.
pub fn check_pane(pane: &Path) -> io::Result<PaneCheck> {
    let mut gens = subfolders(pane, "gen-")?;
    gens.sort();
    let mut expect = Expected::new();
    let mut bytes = 0u64;
    let mut mismatch_at = None;
    let mut buf = vec![0u8; 4 << 20];
    'gens: for g in &gens {
        let log = panelog::ByteReader::open(g)?;
        let len = log.len();
        let mut at = 0u64;
        while at < len {
            let n = ((len - at) as usize).min(buf.len());
            log.read_exact_at(at, &mut buf[..n])?;
            if let Some(k) = expect.first_difference(&buf[..n]) {
                mismatch_at = Some(bytes + k as u64);
                bytes += n as u64;
                break 'gens;
            }
            at += n as u64;
            bytes += n as u64;
        }
    }
    let name = pane
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(PaneCheck {
        pane: name,
        generations: gens.len(),
        bytes,
        mismatch_at,
    })
}

fn subfolders(dir: &Path, prefix: &str) -> io::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect())
}

/// The emit stream, generated as it is compared.
struct Expected {
    generator: Generator,
    pending: Vec<u8>,
    at: usize,
}

impl Expected {
    fn new() -> Expected {
        Expected {
            generator: Generator::new(EMIT_SEED),
            pending: Vec::new(),
            at: 0,
        }
    }

    /// Compare `got` with the stream's next bytes; the offset within `got`
    /// of the first that differs.
    fn first_difference(&mut self, got: &[u8]) -> Option<usize> {
        let mut k = 0;
        while k < got.len() {
            if self.at == self.pending.len() {
                self.pending.clear();
                self.at = 0;
                self.generator.fill(&mut self.pending, 1 << 20);
            }
            let n = (self.pending.len() - self.at).min(got.len() - k);
            let want = &self.pending[self.at..self.at + n];
            if let Some(d) = want.iter().zip(&got[k..k + n]).position(|(a, b)| a != b) {
                return Some(k + d);
            }
            self.at += n;
            k += n;
        }
        None
    }
}

/// The report: a table row per pane and a verdict.
pub fn render(checks: &[PaneCheck]) -> String {
    let mut out = String::from(
        "| Pane | Generations | Durable | Exact prefix |\n| --- | --- | --- | --- |\n",
    );
    for c in checks {
        let verdict = match c.mismatch_at {
            None => "yes".to_string(),
            Some(at) => format!("**no**: differs at byte {at}"),
        };
        out.push_str(&format!(
            "| {} | {} | {:.1} MiB | {} |\n",
            c.pane,
            c.generations,
            c.bytes as f64 / (1 << 20) as f64,
            verdict
        ));
    }
    let bad = checks.iter().filter(|c| c.mismatch_at.is_some()).count();
    out.push_str(&format!(
        "\n{} panes, {} exact prefixes, {:.1} MiB durable in all\n",
        checks.len(),
        checks.len() - bad,
        checks.iter().map(|c| c.bytes).sum::<u64>() as f64 / (1 << 20) as f64
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        Generator::new(EMIT_SEED).fill(&mut v, n);
        v
    }

    #[test]
    fn a_prefix_in_any_chunks_matches_and_a_changed_byte_does_not() {
        let s = stream(3 << 20);
        let mut e = Expected::new();
        for chunk in s[..(3 << 20) - 5].chunks(777_777) {
            assert_eq!(e.first_difference(chunk), None);
        }
        let mut bad = s[..1000].to_vec();
        bad[600] ^= 1;
        assert_eq!(Expected::new().first_difference(&bad), Some(600));
    }

    #[test]
    fn a_log_split_across_generations_is_checked_as_one_stream() {
        let dir = std::env::temp_dir().join(format!("fwbench-logcheck-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let pane = dir.join("pane-0");
        let s = stream(5 << 20);
        let cfg = panelog::Config::default();
        for (i, part) in [&s[..2 << 20], &s[2 << 20..(5 << 20) - 3]]
            .iter()
            .enumerate()
        {
            let mut w =
                panelog::Writer::create(&pane.join(format!("gen-{:013}-{i:05}", i + 1)), cfg)
                    .unwrap();
            w.append(part, 0).unwrap();
            w.flush().unwrap();
        }
        let got = check_session(&dir).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].generations, 2);
        assert_eq!(got[0].bytes, (5 << 20) - 3);
        assert_eq!(got[0].mismatch_at, None);
        let _ = fs::remove_dir_all(&dir);
    }
}
