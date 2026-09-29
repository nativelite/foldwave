//! An isolated pane's history lines that a scrolled-back view shows (design
//! §8.8a). The pane host holds the history; the window asks for just the
//! lines in view, without waiting, and draws them when they come. Lines keep
//! their numbers (the mirror's: lines scrolled off, then screen rows) and
//! never change once they have scrolled off, so a line fetched once is good
//! until it leaves the view's neighbourhood or the host lets it go.

use ansi::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::time::{Duration, Instant};

/// How long an ask may go unanswered before the lines are asked for again.
const RETRY: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub struct HostLines {
    lines: HashMap<u64, Vec<Cell>>,
    /// The lines last asked for and when; one ask is out at a time.
    asked: Option<(Range<u64>, Instant)>,
}

impl HostLines {
    /// The line's cells, if fetched.
    pub fn get(&self, line: u64) -> Option<&Vec<Cell>> {
        self.lines.get(&line)
    }

    /// Of the lines `need`, the span from the first to the last not yet
    /// fetched, to ask the host for; `None` when all are here or an ask is
    /// still out (sent less than [`RETRY`] ago).
    pub fn to_ask(
        &mut self,
        need: impl IntoIterator<Item = u64>,
        now: Instant,
    ) -> Option<Range<u64>> {
        if self
            .asked
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) < RETRY)
        {
            return None;
        }
        let mut span: Option<Range<u64>> = None;
        for l in need {
            if !self.lines.contains_key(&l) {
                span = Some(match span {
                    Some(s) => s.start.min(l)..s.end.max(l + 1),
                    None => l..l + 1,
                });
            }
        }
        if let Some(s) = &span {
            self.asked = Some((s.clone(), now));
        }
        span
    }

    /// The host's answer: lines `first..`. It clears the ask it answers.
    pub fn store(&mut self, first: u64, lines: Vec<Vec<Cell>>) {
        if self.asked.as_ref().is_some_and(|(s, _)| s.start == first) {
            self.asked = None;
        }
        for (i, l) in lines.into_iter().enumerate() {
            self.lines.insert(first + i as u64, l);
        }
    }

    /// Forget every line outside `keep`: those far from the view, and those
    /// the host no longer holds.
    pub fn keep(&mut self, keep: Range<u64>) {
        self.lines.retain(|l, _| keep.contains(l));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(ch: char) -> Vec<Cell> {
        vec![Cell {
            ch,
            ..Cell::default()
        }]
    }

    #[test]
    fn asks_once_for_the_missing_span_then_uses_what_came() {
        let t = Instant::now();
        let mut h = HostLines::default();
        assert_eq!(h.to_ask(10..20, t), Some(10..20), "nothing fetched yet");
        assert_eq!(h.to_ask(10..20, t), None, "the ask is still out");
        h.store(10, (0..10).map(|i| line((b'a' + i) as char)).collect());
        assert_eq!(h.get(12).unwrap()[0].ch, 'c');
        assert_eq!(h.to_ask(10..20, t), None, "all here");
        // Scrolled two lines further back: only those two are asked for.
        assert_eq!(h.to_ask(8..18, t), Some(8..10));
    }

    #[test]
    fn an_unanswered_ask_is_repeated_after_a_while() {
        let t = Instant::now();
        let mut h = HostLines::default();
        assert!(h.to_ask(0..5, t).is_some());
        assert!(h.to_ask(0..5, t + RETRY / 2).is_none());
        assert_eq!(h.to_ask(0..5, t + RETRY), Some(0..5));
    }

    #[test]
    fn a_reply_to_someone_else_keeps_the_ask_out() {
        let t = Instant::now();
        let mut h = HostLines::default();
        assert_eq!(h.to_ask(30..40, t), Some(30..40));
        // A copy's own request answered: stored, but ours is still out.
        h.store(0, vec![line('x')]);
        assert_eq!(h.to_ask(30..40, t), None);
        h.store(30, (0..10).map(|_| line('y')).collect());
        assert_eq!(h.to_ask(30..40, t), None, "all here");
    }

    #[test]
    fn lines_outside_the_neighbourhood_are_forgotten() {
        let mut h = HostLines::default();
        h.store(0, (0..100).map(|_| line('z')).collect());
        h.keep(40..60);
        assert_eq!(h.len(), 20);
        assert!(h.get(39).is_none() && h.get(40).is_some() && h.get(60).is_none());
    }
}
