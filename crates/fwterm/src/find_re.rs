//! Regular expressions for search (design §8.5): a query written `/pattern/`.
//!
//! A small engine, since fwterm takes no dependencies: the pattern is parsed
//! to a tree, compiled to a program, and run as a Pike VM (a Thompson NFA
//! simulated breadth-first), so matching is linear in the text and no
//! pattern can make it backtrack exponentially. Matching folds ASCII case,
//! as the plain search does, and finds the leftmost non-empty match, with
//! greedy repeats preferred longest unless marked lazy (`*?`).
//!
//! Syntax: literals and `\` escapes; `.` (any but a newline); classes `[a-z]`,
//! `[^...]`, with `\d \w \s` and their negations inside or out; `* + ?` and
//! `{m}`, `{m,}`, `{m,n}`, each lazy with a trailing `?`; groups `( )` and
//! `(?: )`; `|`; and `^` / `$` for the start and end of a line.

/// Parse errors say what is wrong, for the search bar.
pub type Error = String;

/// The most a `{m,n}` may count to, and the most instructions a pattern may
/// compile to: a bound on memory and on the work per character.
const MAX_REPEAT: u32 = 1000;
const MAX_PROGRAM: usize = 20_000;

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Empty,
    Lit(char),
    Any,
    Class(Class),
    Start,
    End,
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: u32,
        max: Option<u32>,
        greedy: bool,
    },
}

/// A set of characters: ranges, maybe negated.
#[derive(Debug, Clone, PartialEq)]
struct Class {
    ranges: Vec<(char, char)>,
    negated: bool,
}

impl Class {
    fn digit() -> Class {
        Class {
            ranges: vec![('0', '9')],
            negated: false,
        }
    }
    fn word() -> Class {
        Class {
            ranges: vec![('0', '9'), ('A', 'Z'), ('_', '_'), ('a', 'z')],
            negated: false,
        }
    }
    fn space() -> Class {
        Class {
            ranges: vec![('\t', '\r'), (' ', ' ')],
            negated: false,
        }
    }

    fn has(&self, c: char) -> bool {
        let inside = |c: char| self.ranges.iter().any(|&(a, b)| a <= c && c <= b);
        let hit = inside(c) || inside(c.to_ascii_lowercase()) || inside(c.to_ascii_uppercase());
        hit != self.negated
    }
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl Parser<'_> {
    fn alt(&mut self) -> Result<Node, Error> {
        let mut arms = vec![self.concat()?];
        while self.chars.peek() == Some(&'|') {
            self.chars.next();
            arms.push(self.concat()?);
        }
        Ok(if arms.len() == 1 {
            arms.pop().unwrap()
        } else {
            Node::Alt(arms)
        })
    }

    fn concat(&mut self) -> Result<Node, Error> {
        let mut items = Vec::new();
        while let Some(&c) = self.chars.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            items.push(self.repeat(atom)?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn atom(&mut self) -> Result<Node, Error> {
        let c = self.chars.next().ok_or("unexpected end")?;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '[' => Node::Class(self.class()?),
            '(' => {
                if self.chars.peek() == Some(&'?') {
                    self.chars.next();
                    if self.chars.next() != Some(':') {
                        return Err("only (?: ) groups are supported".into());
                    }
                }
                let inner = self.alt()?;
                if self.chars.next() != Some(')') {
                    return Err("missing )".into());
                }
                inner
            }
            '*' | '+' | '?' => return Err(format!("nothing to repeat before {c}")),
            // Not a valid `{m,n}` after an atom (`repeat` took those): a
            // literal brace.
            '{' => Node::Lit('{'),
            '\\' => self.escape()?,
            c => Node::Lit(c),
        })
    }

    fn escape(&mut self) -> Result<Node, Error> {
        let c = self.chars.next().ok_or("pattern ends in \\")?;
        Ok(match c {
            'd' => Node::Class(Class::digit()),
            'w' => Node::Class(Class::word()),
            's' => Node::Class(Class::space()),
            'D' | 'W' | 'S' => {
                let mut k = match c {
                    'D' => Class::digit(),
                    'W' => Class::word(),
                    _ => Class::space(),
                };
                k.negated = true;
                Node::Class(k)
            }
            't' => Node::Lit('\t'),
            'n' => Node::Lit('\n'),
            c if c.is_ascii_alphanumeric() => return Err(format!("unknown escape \\{c}")),
            c => Node::Lit(c),
        })
    }

    /// After `[`: the members up to `]`.
    fn class(&mut self) -> Result<Class, Error> {
        let mut k = Class {
            ranges: Vec::new(),
            negated: false,
        };
        if self.chars.peek() == Some(&'^') {
            self.chars.next();
            k.negated = true;
        }
        let mut first = true;
        loop {
            let c = self.chars.next().ok_or("missing ]")?;
            if c == ']' && !first {
                break;
            }
            first = false;
            let lo = if c == '\\' {
                let e = self.chars.next().ok_or("missing ]")?;
                let set = match e {
                    'd' => Some(Class::digit()),
                    'w' => Some(Class::word()),
                    's' => Some(Class::space()),
                    _ => None,
                };
                if let Some(set) = set {
                    k.ranges.extend(set.ranges);
                    continue;
                }
                match e {
                    't' => '\t',
                    'n' => '\n',
                    e => e,
                }
            } else {
                c
            };
            // A range `a-z`, unless the `-` is last.
            let mut ahead = self.chars.clone();
            if ahead.next() == Some('-') && ahead.peek().is_some_and(|&n| n != ']') {
                self.chars.next();
                let mut hi = self.chars.next().ok_or("missing ]")?;
                if hi == '\\' {
                    hi = self.chars.next().ok_or("missing ]")?;
                }
                if hi < lo {
                    return Err(format!("bad range {lo}-{hi}"));
                }
                k.ranges.push((lo, hi));
            } else {
                k.ranges.push((lo, lo));
            }
        }
        Ok(k)
    }

    fn repeat(&mut self, atom: Node) -> Result<Node, Error> {
        let (min, max) = match self.chars.peek() {
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('?') => (0, Some(1)),
            Some('{') => {
                // `{m}`, `{m,}`, `{m,n}`; anything else is a literal `{`.
                let mut ahead = self.chars.clone();
                ahead.next();
                let text: String = ahead.by_ref().take_while(|&c| c != '}').collect();
                let bounds = parse_bounds(&text);
                match bounds {
                    Some(b) => {
                        for _ in 0..text.chars().count() + 2 {
                            self.chars.next();
                        }
                        let node = self.lazy(atom, b.0, b.1);
                        return self.repeat_more(node);
                    }
                    None => return Ok(atom),
                }
            }
            _ => return Ok(atom),
        };
        self.chars.next();
        let node = self.lazy(atom, min, max);
        self.repeat_more(node)
    }

    fn lazy(&mut self, atom: Node, min: u32, max: Option<u32>) -> Node {
        let greedy = if self.chars.peek() == Some(&'?') {
            self.chars.next();
            false
        } else {
            true
        };
        Node::Repeat {
            node: Box::new(atom),
            min,
            max,
            greedy,
        }
    }

    fn repeat_more(&mut self, node: Node) -> Result<Node, Error> {
        match self.chars.peek() {
            Some('*') | Some('+') => Err("a repeat cannot repeat".into()),
            _ => Ok(node),
        }
    }
}

/// `m`, `m,` or `m,n` from inside `{ }`, within bounds.
fn parse_bounds(text: &str) -> Option<(u32, Option<u32>)> {
    let (lo, hi) = match text.split_once(',') {
        None => (text, Some(text)),
        Some((lo, "")) => (lo, None),
        Some((lo, hi)) => (lo, Some(hi)),
    };
    let lo: u32 = lo.parse().ok()?;
    let hi: Option<u32> = match hi {
        Some(h) => Some(h.parse().ok()?),
        None => None,
    };
    let ok = lo <= MAX_REPEAT && hi.map_or(true, |h| h >= lo && h <= MAX_REPEAT);
    ok.then_some((lo, hi))
}

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,
    Class(Class),
    Start,
    End,
    Split(usize, usize),
    Jmp(usize),
    Match,
}

fn compile(n: &Node, prog: &mut Vec<Inst>) -> Result<(), Error> {
    if prog.len() > MAX_PROGRAM {
        return Err("pattern too large".into());
    }
    match n {
        Node::Empty => {}
        Node::Lit(c) => prog.push(Inst::Char(*c)),
        Node::Any => prog.push(Inst::Any),
        Node::Class(k) => prog.push(Inst::Class(k.clone())),
        Node::Start => prog.push(Inst::Start),
        Node::End => prog.push(Inst::End),
        Node::Concat(v) => {
            for x in v {
                compile(x, prog)?;
            }
        }
        Node::Alt(arms) => {
            // Split to each arm in order; every arm jumps to the end.
            let mut jumps = Vec::new();
            for (i, arm) in arms.iter().enumerate() {
                if i + 1 < arms.len() {
                    let split = prog.len();
                    prog.push(Inst::Split(split + 1, 0));
                    compile(arm, prog)?;
                    jumps.push(prog.len());
                    prog.push(Inst::Jmp(0));
                    let next = prog.len();
                    prog[split] = Inst::Split(split + 1, next);
                } else {
                    compile(arm, prog)?;
                }
            }
            let end = prog.len();
            for j in jumps {
                prog[j] = Inst::Jmp(end);
            }
        }
        Node::Repeat {
            node,
            min,
            max,
            greedy,
        } => {
            for _ in 0..*min {
                compile(node, prog)?;
            }
            let split = |body: usize, out: usize| {
                if *greedy {
                    Inst::Split(body, out)
                } else {
                    Inst::Split(out, body)
                }
            };
            match max {
                None => {
                    let top = prog.len();
                    prog.push(Inst::Jmp(0));
                    compile(node, prog)?;
                    prog.push(Inst::Jmp(top));
                    let out = prog.len();
                    prog[top] = split(top + 1, out);
                }
                Some(max) => {
                    let mut holes = Vec::new();
                    for _ in *min..*max {
                        let at = prog.len();
                        prog.push(Inst::Jmp(0));
                        holes.push(at);
                        compile(node, prog)?;
                        if prog.len() > MAX_PROGRAM {
                            return Err("pattern too large".into());
                        }
                    }
                    let out = prog.len();
                    for at in holes {
                        prog[at] = split(at + 1, out);
                    }
                }
            }
        }
    }
    Ok(())
}

/// A literal every match must contain (the longest found), which the log's
/// index can look for first.
fn required(n: &Node) -> Option<String> {
    match n {
        Node::Lit(c) => Some(c.to_string()),
        Node::Repeat { node, min, .. } if *min >= 1 => required(node),
        Node::Concat(v) => {
            let mut best: Option<String> = None;
            let mut run = String::new();
            let keep = |s: String, best: &mut Option<String>| {
                if best
                    .as_ref()
                    .map_or(true, |b| s.chars().count() > b.chars().count())
                {
                    *best = Some(s);
                }
            };
            for x in v {
                match x {
                    Node::Lit(c) => run.push(*c),
                    _ => {
                        if !run.is_empty() {
                            keep(std::mem::take(&mut run), &mut best);
                        }
                        if let Some(s) = required(x) {
                            keep(s, &mut best);
                        }
                    }
                }
            }
            if !run.is_empty() {
                keep(run, &mut best);
            }
            best
        }
        _ => None,
    }
}

/// A compiled pattern.
#[derive(Debug, Clone)]
pub struct Regex {
    prog: Vec<Inst>,
    literal: Option<String>,
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex, Error> {
        let mut p = Parser {
            chars: pattern.chars().peekable(),
        };
        let tree = p.alt()?;
        if p.chars.next().is_some() {
            return Err("unmatched )".into());
        }
        let mut prog = Vec::new();
        compile(&tree, &mut prog)?;
        prog.push(Inst::Match);
        Ok(Regex {
            prog,
            literal: required(&tree),
        })
    }

    /// A literal every match contains, if the pattern has one.
    pub fn literal(&self) -> Option<&str> {
        self.literal.as_deref()
    }

    /// The leftmost non-empty match in `text`: its first and last index.
    pub fn find(&self, text: &[char]) -> Option<(usize, usize)> {
        let n = self.prog.len();
        // Threads in priority order: (pc, where its match started).
        let mut clist: Vec<(usize, usize)> = Vec::new();
        let mut nlist: Vec<(usize, usize)> = Vec::new();
        let mut seen = vec![usize::MAX; n];
        let mut found = None;
        for sp in 0..=text.len() {
            if found.is_none() {
                // A match may start here, after every thread already going.
                self.add(&mut clist, &mut seen, 0, sp, sp, text.len());
            }
            if clist.is_empty() {
                break;
            }
            let c = text.get(sp).copied();
            for i in 0..clist.len() {
                let (pc, start) = clist[i];
                let step = match (&self.prog[pc], c) {
                    (Inst::Match, _) => {
                        if sp > start {
                            // Lower-priority threads lose to this one.
                            found = Some((start, sp - 1));
                            break;
                        }
                        false
                    }
                    (Inst::Char(want), Some(c)) => c.eq_ignore_ascii_case(want),
                    (Inst::Any, Some(c)) => c != '\n',
                    (Inst::Class(k), Some(c)) => k.has(c),
                    _ => false,
                };
                if step {
                    self.add(&mut nlist, &mut seen, pc + 1, start, sp + 1, text.len());
                }
            }
            std::mem::swap(&mut clist, &mut nlist);
            nlist.clear();
        }
        found
    }

    /// Add a thread at `pc`, following jumps, splits and anchors, at text
    /// position `sp`; `seen` keeps one thread per instruction per position.
    fn add(
        &self,
        list: &mut Vec<(usize, usize)>,
        seen: &mut [usize],
        pc: usize,
        start: usize,
        sp: usize,
        len: usize,
    ) {
        // Marks are per position: position sp+1 uses a fresh stamp.
        let stamp = sp;
        let mut stack = vec![pc];
        while let Some(pc) = stack.pop() {
            if seen[pc] == stamp {
                continue;
            }
            seen[pc] = stamp;
            match self.prog[pc] {
                Inst::Jmp(to) => stack.push(to),
                // Pushed in reverse so the preferred branch is taken first.
                Inst::Split(a, b) => {
                    stack.push(b);
                    stack.push(a);
                }
                Inst::Start if sp == 0 => stack.push(pc + 1),
                Inst::End if sp == len => stack.push(pc + 1),
                Inst::Start | Inst::End => {}
                _ => list.push((pc, start)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(pattern: &str, text: &str) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        Regex::new(pattern)
            .unwrap()
            .find(&chars)
            .map(|(a, b)| chars[a..=b].iter().collect())
    }

    #[test]
    fn literals_classes_and_repeats_match_leftmost_longest() {
        assert_eq!(find("error", "an ERROR here").as_deref(), Some("ERROR"));
        assert_eq!(
            find(r"err(or)?\s+\d+", "err  42 and error 7").as_deref(),
            Some("err  42")
        );
        assert_eq!(find("a.c", "xabc").as_deref(), Some("abc"));
        assert_eq!(find("[a-c]+", "zzbcaz").as_deref(), Some("bca"));
        assert_eq!(find("[^0-9 ]+", "12 ab3").as_deref(), Some("ab"));
        assert_eq!(find(r"\w+", "  foo_1!").as_deref(), Some("foo_1"));
        assert_eq!(find(r"x{2,3}", "xxxxx").as_deref(), Some("xxx"));
        assert_eq!(find(r"x{2}", "x xx").as_deref(), Some("xx"));
        assert_eq!(find(r"a{2,}", "aaaa").as_deref(), Some("aaaa"));
        assert_eq!(find("cat|dog", "hotdog cat").as_deref(), Some("dog"));
        assert_eq!(find("<.+?>", "<a><b>").as_deref(), Some("<a>"), "lazy");
        assert_eq!(find("<.+>", "<a><b>").as_deref(), Some("<a><b>"), "greedy");
        assert_eq!(
            find("a{x", "a{x").as_deref(),
            Some("a{x"),
            "a literal brace"
        );
    }

    #[test]
    fn anchors_and_empty_matches() {
        assert_eq!(find("^warn", "warning").as_deref(), Some("warn"));
        assert_eq!(find("^warn", "a warning"), None);
        assert_eq!(find(r"\d+$", "v1 2 34").as_deref(), Some("34"));
        // A pattern that can match nothing reports the first real match.
        assert_eq!(find("a*", "bbaab").as_deref(), Some("aa"));
        assert_eq!(find("x*", "bbb"), None);
    }

    #[test]
    fn a_pathological_pattern_stays_linear() {
        let text: String = "a".repeat(5000);
        let t = std::time::Instant::now();
        assert_eq!(find("(a*)*b", &text), None);
        assert!(
            t.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn bad_patterns_are_errors_not_panics() {
        for p in ["(ab", "ab)", "[ab", "*a", "a**", r"\q", "[z-a]", r"a\"] {
            assert!(Regex::new(p).is_err(), "{p}");
        }
        assert!(
            Regex::new(&format!("a{{{}}}", MAX_REPEAT + 1)).is_ok(),
            "an over-bound {{}} is a literal"
        );
        assert!(Regex::new("(a{1000}){1000}").is_err(), "too large");
    }

    #[test]
    fn the_required_literal_feeds_the_index() {
        let lit = |p: &str| Regex::new(p).unwrap().literal().map(str::to_string);
        assert_eq!(lit(r"error\s+\d+").as_deref(), Some("error"));
        assert_eq!(lit(r"\d+ failed tests").as_deref(), Some(" failed tests"));
        assert_eq!(lit("(build)+ ok").as_deref(), Some("build"), "the longest");
        assert_eq!(lit("cat|dog"), None, "either arm");
        assert_eq!(lit(r"x?\d*"), None);
    }
}
