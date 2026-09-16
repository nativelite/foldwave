//! The synthetic workload: agent-like output, generated deterministically.
//!
//! The harness needs a flood it can reproduce exactly, on any machine, with no
//! recorded fixture to ship. So output is generated from a seeded PRNG: the
//! same seed always yields the same bytes, which keeps a benchmark comparable
//! between runs and between machines.
//!
//! The shape matters. Agent output is not random noise: it is mostly printable
//! ASCII in lines of a few dozen characters, sprinkled with SGR colour escapes,
//! which is precisely the mix a VT parser's fast path is built for
//! (`docs/design.md` section 8.2). A flood of random bytes would measure the
//! wrong thing.

/// A small xorshift64* generator. Deterministic, and enough for shaping a
/// workload; nothing here is cryptographic.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Seed the generator. Zero is remapped, since xorshift cannot leave it.
    pub fn new(seed: u64) -> Rng {
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// The next 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..n`. Returns 0 when `n` is 0.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }
}

/// Line templates, chosen to look like what agents actually print.
const TEMPLATES: &[&str] = &[
    "   Compiling nativelite-{} v0.{}.{}",
    "    Finished `release` profile [optimized] target(s) in {}.{}s",
    "test {}::tests::case_{} ... ok",
    "[INFO ] {} worker {} processed {} records in {} ms",
    "[WARN ] retrying {} after transient failure (attempt {})",
    "[ERROR] {}: expected {} fields, found {}",
    "+++ b/crates/{}/src/lib.rs",
    "+    let value = compute({})?;",
    "-    let value = compute_old({})?;",
    "     at crates/{}/src/main.rs:{}:{}",
];

const WORDS: &[&str] = &[
    "resmon",
    "panelog",
    "vterm",
    "governor",
    "indexer",
    "parser",
    "renderer",
    "atlas",
    "segment",
    "checkpoint",
    "throughput",
    "latency",
];

/// SGR colour escapes an agent CLI typically emits.
const COLOURS: &[&str] = &[
    "\x1b[0m",
    "\x1b[1m",
    "\x1b[31m",
    "\x1b[32m",
    "\x1b[33m",
    "\x1b[36m",
    "\x1b[1;32m",
];

/// Generates agent-like lines into a caller-owned buffer.
#[derive(Debug, Clone)]
pub struct Generator {
    rng: Rng,
}

impl Generator {
    /// A generator with the given seed.
    pub fn new(seed: u64) -> Generator {
        Generator {
            rng: Rng::new(seed),
        }
    }

    /// Append one line, including its trailing newline.
    ///
    /// Roughly one line in four is wrapped in an SGR colour pair, which keeps
    /// the escape density in the range a VT fast path has to cope with.
    pub fn write_line(&mut self, out: &mut Vec<u8>) {
        let coloured = self.rng.below(4) == 0;
        if coloured {
            let c = COLOURS[self.rng.below(COLOURS.len() as u64) as usize];
            out.extend_from_slice(c.as_bytes());
        }

        let template = TEMPLATES[self.rng.below(TEMPLATES.len() as u64) as usize];
        for part in split_placeholders(template) {
            match part {
                Part::Literal(text) => out.extend_from_slice(text.as_bytes()),
                Part::Placeholder => {
                    // Half words, half numbers, so lines vary in width.
                    if self.rng.below(2) == 0 {
                        let w = WORDS[self.rng.below(WORDS.len() as u64) as usize];
                        out.extend_from_slice(w.as_bytes());
                    } else {
                        let n = self.rng.below(10_000);
                        out.extend_from_slice(n.to_string().as_bytes());
                    }
                }
            }
        }

        if coloured {
            out.extend_from_slice(b"\x1b[0m");
        }
        out.push(b'\n');
    }

    /// Fill `out` with whole lines until it holds at least `target` bytes.
    ///
    /// Never splits a line, so the result is always valid output; it therefore
    /// slightly overshoots `target`.
    pub fn fill(&mut self, out: &mut Vec<u8>, target: usize) {
        while out.len() < target {
            self.write_line(out);
        }
    }
}

enum Part<'a> {
    Literal(&'a str),
    Placeholder,
}

/// Split a template on `{}` into literals and placeholders.
fn split_placeholders(template: &str) -> Vec<Part<'_>> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        if i > 0 {
            parts.push(Part::Literal(&rest[..i]));
        }
        parts.push(Part::Placeholder);
        rest = &rest[i + 2..];
    }
    if !rest.is_empty() {
        parts.push(Part::Literal(rest));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_seed_still_generates() {
        let mut rng = Rng::new(0);
        assert_ne!(rng.next_u64(), 0);
    }

    #[test]
    fn the_same_seed_gives_the_same_stream() {
        let (mut a, mut b) = (Rng::new(7), Rng::new(7));
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let (mut a, mut b) = (Rng::new(1), Rng::new(2));
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn below_stays_in_range_and_handles_zero() {
        let mut rng = Rng::new(99);
        assert_eq!(rng.below(0), 0);
        for _ in 0..1000 {
            assert!(rng.below(10) < 10);
        }
    }

    #[test]
    fn split_placeholders_separates_literals_and_holes() {
        let parts = split_placeholders("a{}b{}");
        assert_eq!(parts.len(), 4);
        assert!(matches!(parts[0], Part::Literal("a")));
        assert!(matches!(parts[1], Part::Placeholder));
        assert!(matches!(parts[2], Part::Literal("b")));
        assert!(matches!(parts[3], Part::Placeholder));
    }

    #[test]
    fn split_placeholders_handles_a_template_with_none() {
        let parts = split_placeholders("plain");
        assert_eq!(parts.len(), 1);
        assert!(matches!(parts[0], Part::Literal("plain")));
    }

    #[test]
    fn every_line_ends_with_exactly_one_newline() {
        let mut g = Generator::new(1);
        for _ in 0..200 {
            let mut buf = Vec::new();
            g.write_line(&mut buf);
            assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 1);
            assert_eq!(*buf.last().unwrap(), b'\n');
        }
    }

    #[test]
    fn no_placeholder_survives_into_the_output() {
        let mut g = Generator::new(4);
        let mut buf = Vec::new();
        g.fill(&mut buf, 64 * 1024);
        let text = String::from_utf8(buf).expect("output is valid UTF-8");
        assert!(!text.contains("{}"), "an unsubstituted placeholder escaped");
    }

    #[test]
    fn fill_reaches_the_target_without_splitting_a_line() {
        let mut g = Generator::new(2);
        let mut buf = Vec::new();
        g.fill(&mut buf, 10_000);
        assert!(buf.len() >= 10_000);
        assert_eq!(*buf.last().unwrap(), b'\n', "fill left a partial line");
    }

    #[test]
    fn fill_to_zero_writes_nothing() {
        let mut buf = Vec::new();
        Generator::new(3).fill(&mut buf, 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn the_workload_is_reproducible_for_a_seed() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        Generator::new(1234).fill(&mut a, 32 * 1024);
        Generator::new(1234).fill(&mut b, 32 * 1024);
        assert_eq!(a, b, "the same seed must produce identical bytes");
    }

    #[test]
    fn the_workload_contains_escape_sequences_like_real_agent_output() {
        let mut buf = Vec::new();
        Generator::new(5).fill(&mut buf, 64 * 1024);
        let escapes = buf.iter().filter(|&&b| b == 0x1b).count();
        assert!(escapes > 0, "a VT fast path needs escapes to exercise it");
        // But it must stay mostly printable text, not an escape storm.
        let printable = buf.iter().filter(|&&b| (0x20..0x7f).contains(&b)).count();
        assert!(
            printable > buf.len() / 2,
            "workload should be mostly printable text"
        );
    }
}
