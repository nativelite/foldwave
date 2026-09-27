//! Folded commands (design §8.7): a folded command's output shows as one
//! line, `▸ N lines folded`, until it is unfolded. Folding changes only what
//! a pane draws: the output stays in memory and the log, and copying and
//! search see all of it.
//!
//! Lines are output lines, numbered as blocks and selections number them.

use std::ops::Range;

/// One row of a pane's view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// An output line.
    Line(u64),
    /// A folded command's output: its first line and how many lines.
    Folded { first: u64, lines: u64 },
}

/// A pane's folds: output ranges, sorted and disjoint.
#[derive(Debug, Default)]
pub struct Folds {
    list: Vec<Range<u64>>,
}

impl Folds {
    /// Fold `lines`. Folds it overlaps are replaced; an empty range is not
    /// folded.
    pub fn fold(&mut self, lines: Range<u64>) {
        if lines.is_empty() {
            return;
        }
        self.list
            .retain(|f| f.end <= lines.start || f.start >= lines.end);
        let at = self.list.partition_point(|f| f.start < lines.start);
        self.list.insert(at, lines);
    }

    /// Unfold the fold at the top of a view whose top line is `top` (the
    /// first that ends after it), or, at the live view (`None`), the latest.
    /// Returns whether one was.
    pub fn unfold(&mut self, top: Option<u64>) -> bool {
        let at = match top {
            None => self.list.len().checked_sub(1),
            Some(top) => {
                let i = self.list.partition_point(|f| f.end <= top);
                (i < self.list.len()).then_some(i)
            }
        };
        at.map(|i| self.list.remove(i)).is_some()
    }

    /// Forget folds that end before `first`, the oldest line still held.
    pub fn forget_before(&mut self, first: u64) {
        let keep = self.list.partition_point(|f| f.end <= first);
        self.list.drain(..keep);
    }

    /// The rows of a view `rows` tall whose top is line `top`, over the lines
    /// before `end` (one past the live bottom); `None` when no fold reaches
    /// into it, so the view is plain lines.
    pub fn view(&self, top: u64, rows: usize, end: u64) -> Option<Vec<Row>> {
        let i = self.list.partition_point(|f| f.end <= top);
        let near = self.list.get(i)?;
        if near.start >= top + rows as u64 {
            return None;
        }
        let mut out = Vec::with_capacity(rows);
        let mut line = top;
        let mut folds = self.list[i..].iter().peekable();
        while out.len() < rows && line < end {
            match folds.peek() {
                Some(f) if f.start <= line => {
                    // A fold the view starts inside still shows as its line.
                    out.push(Row::Folded {
                        first: f.start,
                        lines: f.end - f.start,
                    });
                    line = f.end;
                    folds.next();
                }
                _ => {
                    out.push(Row::Line(line));
                    line += 1;
                }
            }
        }
        Some(out)
    }

    /// The live view: `rows` rows ending with line `end - 1`, going up no
    /// further than line `oldest`, so a fold lets older lines show above
    /// rather than leaving blank rows below. `None` when no fold reaches into
    /// it.
    pub fn live_view(&self, end: u64, rows: usize, oldest: u64) -> Option<Vec<Row>> {
        let mut i = self.list.partition_point(|f| f.start < end);
        if i == 0 || self.list[i - 1].end + (rows as u64) <= end {
            return None;
        }
        let mut out = Vec::with_capacity(rows);
        let mut line = end; // the rows so far start here
        while out.len() < rows && line > oldest {
            let above = line - 1;
            while i > 0 && self.list[i - 1].start > above {
                i -= 1;
            }
            match self.list[..i].last() {
                Some(f) if f.end > above => {
                    out.push(Row::Folded {
                        first: f.start,
                        lines: f.end - f.start,
                    });
                    line = f.start;
                }
                _ => {
                    out.push(Row::Line(above));
                    line = above;
                }
            }
        }
        out.reverse();
        Some(out)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.len()
    }
}

/// Where a selection from `start` to `end` (line, column; reading order)
/// falls on a view's `rows`, `cols` wide: the (row, column) of its first
/// and last cells. A folded row it reaches is covered whole.
#[allow(clippy::type_complexity)]
pub fn highlight(
    rows: &[Row],
    cols: usize,
    start: (u64, usize),
    end: (u64, usize),
) -> Option<((usize, usize), (usize, usize))> {
    let span = |r: &Row| match *r {
        Row::Line(l) => (l, l),
        Row::Folded { first, lines } => (first, first + lines - 1),
    };
    let first = rows.iter().position(|r| span(r).1 >= start.0)?;
    let last = rows.iter().rposition(|r| span(r).0 <= end.0)?;
    if cols == 0 || first > last {
        return None;
    }
    let from = match rows[first] {
        Row::Line(l) if l == start.0 => start.1.min(cols - 1),
        _ => 0,
    };
    let to = match rows[last] {
        Row::Line(l) if l == end.0 => end.1.min(cols - 1),
        _ => cols - 1,
    };
    Some(((first, from), (last, to)))
}

/// The text a folded row shows.
pub fn summary(lines: u64) -> String {
    let digits = lines.to_string();
    let mut grouped = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(d);
    }
    let noun = if lines == 1 { "line" } else { "lines" };
    format!("\u{25B8} {grouped} {noun} folded")
}

#[cfg(test)]
mod tests {
    use super::*;
    use Row::*;

    #[test]
    fn a_fold_is_one_row_and_the_view_fills_past_it() {
        let mut f = Folds::default();
        f.fold(3..8);
        // Lines 0..20 exist; a 6-row view from line 1.
        assert_eq!(
            f.view(1, 6, 20).unwrap(),
            [
                Line(1),
                Line(2),
                Folded { first: 3, lines: 5 },
                Line(8),
                Line(9),
                Line(10)
            ]
        );
        // A view starting inside the fold shows it first.
        assert_eq!(
            f.view(5, 2, 20).unwrap(),
            [Folded { first: 3, lines: 5 }, Line(8)]
        );
        // Folds outside the view leave it plain.
        assert_eq!(f.view(10, 5, 20), None);
        assert_eq!(f.view(0, 3, 20), None, "the fold starts below the view");
        // Near the live bottom the view ends with the lines.
        assert_eq!(
            f.view(2, 6, 10).unwrap(),
            [Line(2), Folded { first: 3, lines: 5 }, Line(8), Line(9)]
        );
    }

    #[test]
    fn folds_replace_overlaps_and_unfold_by_position() {
        let mut f = Folds::default();
        f.fold(10..20);
        f.fold(30..40);
        f.fold(15..25); // overlaps the first: replaces it
        f.fold(5..5); // empty: ignored
        assert_eq!(f.len(), 2);
        assert_eq!(
            f.view(14, 3, 50).unwrap(),
            [
                Line(14),
                Folded {
                    first: 15,
                    lines: 10
                },
                Line(25)
            ]
        );
        assert!(f.unfold(None), "the live view unfolds the latest");
        assert_eq!(f.view(29, 3, 50), None);
        assert!(f.unfold(Some(0)), "the first fold ending after the top");
        assert!(!f.unfold(Some(0)));
        f.fold(1..3);
        f.forget_before(3);
        assert_eq!(f.len(), 0, "a fold of lines no longer held goes");
    }

    #[test]
    fn the_live_view_keeps_its_bottom_and_shows_older_lines_above() {
        let mut f = Folds::default();
        f.fold(12..17);
        // Live bottom line 19 (end 20), 5 rows: the fold frees 4 rows.
        assert_eq!(
            f.live_view(20, 5, 0).unwrap(),
            [
                Line(11),
                Folded {
                    first: 12,
                    lines: 5
                },
                Line(17),
                Line(18),
                Line(19)
            ]
        );
        // Not past the oldest line held.
        assert_eq!(
            f.live_view(20, 8, 11).unwrap(),
            [
                Line(11),
                Folded {
                    first: 12,
                    lines: 5
                },
                Line(17),
                Line(18),
                Line(19)
            ]
        );
        // A fold too far above the view leaves it plain.
        assert_eq!(f.live_view(30, 5, 0), None);
        assert_eq!(f.live_view(12, 5, 0), None, "the fold is below");
    }

    #[test]
    fn a_selection_maps_onto_folded_rows() {
        let rows = [Line(1), Line(2), Folded { first: 3, lines: 5 }, Line(8)];
        // From the middle of line 2 into the middle of line 8.
        assert_eq!(highlight(&rows, 10, (2, 4), (8, 6)), Some(((1, 4), (3, 6))));
        // Starting inside the fold: from its row's start.
        assert_eq!(highlight(&rows, 10, (5, 4), (8, 2)), Some(((2, 0), (3, 2))));
        // Ending inside it: to its row's end.
        assert_eq!(highlight(&rows, 10, (1, 0), (4, 3)), Some(((0, 0), (2, 9))));
        assert_eq!(
            highlight(&rows, 10, (20, 0), (30, 0)),
            None,
            "below the view"
        );
    }

    #[test]
    fn the_summary_counts_lines_with_separators() {
        assert_eq!(summary(1), "\u{25B8} 1 line folded");
        assert_eq!(summary(1234), "\u{25B8} 1,234 lines folded");
        assert_eq!(summary(1_000_000), "\u{25B8} 1,000,000 lines folded");
    }
}
