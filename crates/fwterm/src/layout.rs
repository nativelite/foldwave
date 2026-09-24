//! Where each pane sits in the window: a tree of splits over the cell grid.
//!
//! Every split divides its part of the grid in two with a one-cell divider
//! between them — a column of `│` for panes side by side, a row of `─` for
//! panes stacked — so the whole window stays one grid of cells and one frame.
//! The tree only knows pane ids; the app owns the panes.

/// A pane's identity, unique for the window's lifetime.
pub type PaneId = usize;

/// A block of cells: its top-left cell and size. A pane squeezed out by a
/// small window has zero rows or columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub top: usize,
    pub left: usize,
    pub rows: usize,
    pub cols: usize,
}

impl Rect {
    fn contains(&self, row: usize, col: usize) -> bool {
        row >= self.top
            && row < self.top + self.rows
            && col >= self.left
            && col < self.left + self.cols
    }
}

/// How a split lays its two sides out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Side by side, a column divider between.
    Across,
    /// One above the other, a row divider between.
    Down,
}

/// A direction to move the focus in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toward {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone)]
enum Node {
    Leaf(PaneId),
    Split {
        dir: Dir,
        /// The first side's share of the room left after the divider.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn first(&self) -> PaneId {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { a, .. } => a.first(),
        }
    }

    #[cfg(test)]
    fn panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.panes(out);
                b.panes(out);
            }
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    fn place(&self, at: Rect, panes: &mut Vec<(PaneId, Rect)>, lines: &mut Vec<(usize, usize)>) {
        let Node::Split { dir, ratio, a, b } = self else {
            if let Node::Leaf(id) = self {
                panes.push((*id, at));
            }
            return;
        };
        let size = match dir {
            Dir::Across => at.cols,
            Dir::Down => at.rows,
        };
        // Too small for two sides and a divider: the second side gets none.
        let (first, line) = if size >= 3 {
            let room = size - 1;
            let n = (room as f32 * ratio).round() as usize;
            (n.clamp(1, room - 1), true)
        } else {
            (size, false)
        };
        let second = size - first - usize::from(line);
        let (ra, rb) = match dir {
            Dir::Across => (
                Rect { cols: first, ..at },
                Rect {
                    left: at.left + first + usize::from(line),
                    cols: second,
                    ..at
                },
            ),
            Dir::Down => (
                Rect { rows: first, ..at },
                Rect {
                    top: at.top + first + usize::from(line),
                    rows: second,
                    ..at
                },
            ),
        };
        if line {
            match dir {
                Dir::Across => lines.extend((0..at.rows).map(|r| (at.top + r, at.left + first))),
                Dir::Down => lines.extend((0..at.cols).map(|c| (at.top + first, at.left + c))),
            }
        }
        a.place(ra, panes, lines);
        b.place(rb, panes, lines);
    }

    /// Replace leaf `target` with a split of it and `new`; false if absent.
    fn split(&mut self, target: PaneId, dir: Dir, new: PaneId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(Node::Leaf(target)),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, dir, new) || b.split(target, dir, new),
        }
    }

    /// Remove leaf `id` below this split: its sibling takes the split's
    /// place. Returns the pane that should take the focus, if `id` was found.
    fn remove(&mut self, id: PaneId) -> Option<PaneId> {
        let Node::Split { a, b, .. } = self else {
            return None;
        };
        let keep = if matches!(**a, Node::Leaf(p) if p == id) {
            Some(std::mem::replace(&mut **b, Node::Leaf(0)))
        } else if matches!(**b, Node::Leaf(p) if p == id) {
            Some(std::mem::replace(&mut **a, Node::Leaf(0)))
        } else {
            None
        };
        if let Some(keep) = keep {
            let focus = keep.first();
            *self = keep;
            return Some(focus);
        }
        if a.contains(id) {
            a.remove(id)
        } else {
            b.remove(id)
        }
    }
}

/// The window's panes and how they are split.
#[derive(Debug, Clone)]
pub struct Layout {
    root: Option<Node>,
}

/// Where everything goes for one grid size: each pane's cells, and the
/// divider cells with the line character each draws.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Placement {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<(usize, usize, char)>,
}

impl Layout {
    #[cfg(test)]
    /// One pane filling the window.
    pub fn single(id: PaneId) -> Layout {
        Layout {
            root: Some(Node::Leaf(id)),
        }
    }

    /// `ids` in a near-square grid: `ceil(sqrt(n))` columns of equal width,
    /// each a stack of equal-height panes, filled column by column with any
    /// extra panes in the first columns (16 panes: 4 x 4).
    pub fn grid(ids: &[PaneId]) -> Layout {
        fn chain(items: Vec<Node>, dir: Dir) -> Option<Node> {
            let n = items.len();
            let mut it = items.into_iter();
            let first = it.next()?;
            Some(match chain(it.collect(), dir) {
                None => first,
                Some(rest) => Node::Split {
                    dir,
                    ratio: 1.0 / n as f32,
                    a: Box::new(first),
                    b: Box::new(rest),
                },
            })
        }
        let n = ids.len();
        let columns = (1..=n).find(|c| c * c >= n).unwrap_or(0);
        let mut left = ids;
        let mut stacks = Vec::new();
        for c in 0..columns {
            let take = (left.len() + (columns - c) - 1) / (columns - c);
            let (col, rest) = left.split_at(take);
            left = rest;
            stacks.extend(chain(
                col.iter().map(|&id| Node::Leaf(id)).collect(),
                Dir::Down,
            ));
        }
        Layout {
            root: chain(stacks, Dir::Across),
        }
    }

    /// No panes left.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    #[cfg(test)]
    /// Every pane, in reading order of the tree.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            r.panes(&mut out);
        }
        out
    }

    /// Split pane `target` in two, `new` taking the right (`Across`) or lower
    /// (`Down`) half. False if `target` is not in the layout.
    pub fn split(&mut self, target: PaneId, dir: Dir, new: PaneId) -> bool {
        self.root
            .as_mut()
            .map_or(false, |r| r.split(target, dir, new))
    }

    /// Take pane `id` out; its sibling grows into the room. Returns the pane
    /// to focus next (`None` if `id` was absent or was the last pane).
    pub fn remove(&mut self, id: PaneId) -> Option<PaneId> {
        match &mut self.root {
            Some(Node::Leaf(p)) if *p == id => {
                self.root = None;
                None
            }
            Some(r) => r.remove(id),
            None => None,
        }
    }

    /// Lay the tree out, or with `zoom` naming one of its panes, that pane
    /// alone over the whole grid: the others keep their places in the tree
    /// but get no cells.
    pub fn place_zoomed(&self, zoom: Option<PaneId>, rows: usize, cols: usize) -> Placement {
        let full = self.place(rows, cols);
        match zoom {
            Some(z) if full.panes.iter().any(|(id, _)| *id == z) => Placement {
                panes: vec![(
                    z,
                    Rect {
                        top: 0,
                        left: 0,
                        rows,
                        cols,
                    },
                )],
                dividers: Vec::new(),
            },
            _ => full,
        }
    }

    /// Lay the tree out over a `rows x cols` grid.
    pub fn place(&self, rows: usize, cols: usize) -> Placement {
        let mut panes = Vec::new();
        let mut lines = Vec::new();
        if let Some(r) = &self.root {
            r.place(
                Rect {
                    top: 0,
                    left: 0,
                    rows,
                    cols,
                },
                &mut panes,
                &mut lines,
            );
        }
        let is_line = |r: Option<usize>, c: Option<usize>| match (r, c) {
            (Some(r), Some(c)) => lines.contains(&(r, c)),
            _ => false,
        };
        let dividers = lines
            .iter()
            .map(|&(r, c)| {
                let up = is_line(r.checked_sub(1), Some(c));
                let down = is_line(Some(r + 1), Some(c));
                let left = is_line(Some(r), c.checked_sub(1));
                let right = is_line(Some(r), Some(c + 1));
                (r, c, line_char(up, down, left, right))
            })
            .collect();
        Placement { panes, dividers }
    }
}

/// The box-drawing character joining a divider cell to its divider
/// neighbours.
fn line_char(up: bool, down: bool, left: bool, right: bool) -> char {
    match (up, down, left, right) {
        (true, true, true, true) => '┼',
        (true, true, false, true) => '├',
        (true, true, true, false) => '┤',
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (true, false, false, true) => '└',
        (true, false, true, false) => '┘',
        (false, true, false, true) => '┌',
        (false, true, true, false) => '┐',
        (_, _, false, false) => '│',
        _ => '─',
    }
}

/// The pane covering cell `(row, col)`.
pub fn pane_at(panes: &[(PaneId, Rect)], row: usize, col: usize) -> Option<PaneId> {
    panes
        .iter()
        .find(|(_, r)| r.contains(row, col))
        .map(|(id, _)| *id)
}

/// The pane next to `from` in direction `to`: of the panes just past the
/// divider on that side, the one sharing the most of its edge (the later on
/// a tie).
pub fn neighbor(panes: &[(PaneId, Rect)], from: PaneId, to: Toward) -> Option<PaneId> {
    let (_, f) = *panes.iter().find(|(id, _)| *id == from)?;
    let span = |r: &Rect| match to {
        Toward::Left | Toward::Right => (r.top, r.top + r.rows),
        Toward::Up | Toward::Down => (r.left, r.left + r.cols),
    };
    let (lo, hi) = span(&f);
    panes
        .iter()
        .filter(|(id, r)| {
            *id != from
                && r.rows > 0
                && r.cols > 0
                && match to {
                    Toward::Right => r.left == f.left + f.cols + 1,
                    Toward::Left => r.left + r.cols + 1 == f.left,
                    Toward::Down => r.top == f.top + f.rows + 1,
                    Toward::Up => r.top + r.rows + 1 == f.top,
                }
        })
        .filter_map(|(id, r)| {
            let (a, b) = span(r);
            let shared = hi.min(b).saturating_sub(lo.max(a));
            (shared > 0).then_some((*id, shared))
        })
        .max_by_key(|&(_, key)| key)
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(top: usize, left: usize, rows: usize, cols: usize) -> Rect {
        Rect {
            top,
            left,
            rows,
            cols,
        }
    }

    #[test]
    fn one_pane_fills_the_grid_with_no_dividers() {
        let p = Layout::single(7).place(24, 80);
        assert_eq!(p.panes, [(7, rect(0, 0, 24, 80))]);
        assert!(p.dividers.is_empty());
    }

    #[test]
    fn splits_share_the_room_around_a_one_cell_divider() {
        let mut l = Layout::single(0);
        assert!(l.split(0, Dir::Across, 1));
        assert!(l.split(1, Dir::Down, 2));
        assert!(!l.split(9, Dir::Down, 3), "no such pane");
        let p = l.place(11, 21);
        assert_eq!(
            p.panes,
            [
                (0, rect(0, 0, 11, 10)),
                (1, rect(0, 11, 5, 10)),
                (2, rect(6, 11, 5, 10)),
            ]
        );
        let at = |r: usize, c: usize| {
            p.dividers
                .iter()
                .find(|d| (d.0, d.1) == (r, c))
                .map(|d| d.2)
        };
        assert_eq!(at(0, 10), Some('│'));
        assert_eq!(at(5, 10), Some('├'), "the row divider meets the column");
        assert_eq!(at(5, 11), Some('─'));
        assert_eq!(at(5, 20), Some('─'));
        assert_eq!(at(3, 3), None);
        assert_eq!(p.dividers.len(), 11 + 10);
        // Every cell is a pane's or a divider's, exactly once.
        for r in 0..11 {
            for c in 0..21 {
                let owners = p.panes.iter().filter(|(_, x)| x.contains(r, c)).count()
                    + usize::from(at(r, c).is_some());
                assert_eq!(owners, 1, "cell {r},{c}");
            }
        }
    }

    #[test]
    fn a_grid_of_sixteen_is_four_by_four_and_covers_everything() {
        let ids: Vec<PaneId> = (0..16).collect();
        let l = Layout::grid(&ids);
        assert_eq!(l.panes(), ids);
        let p = l.place(47, 163);
        let mut widths: Vec<usize> = p.panes.iter().map(|(_, r)| r.cols).collect();
        let mut heights: Vec<usize> = p.panes.iter().map(|(_, r)| r.rows).collect();
        widths.dedup();
        heights.sort_unstable();
        heights.dedup();
        assert!(widths.iter().all(|&w| (39..=41).contains(&w)), "{widths:?}");
        assert!(
            heights.iter().all(|&h| (10..=12).contains(&h)),
            "{heights:?}"
        );
        assert_eq!(p.panes[4].1.left, p.panes[0].1.cols + 1, "second column");
        let covered: usize = p.panes.iter().map(|(_, r)| r.rows * r.cols).sum();
        assert_eq!(covered + p.dividers.len(), 47 * 163);
        assert!(p.dividers.iter().any(|d| d.2 == '┼'));
        // Odd counts: extra panes go in the first columns.
        let five = Layout::grid(&[0, 1, 2, 3, 4]).place(30, 90);
        let lefts: Vec<usize> = five.panes.iter().map(|(_, r)| r.left).collect();
        assert_eq!(lefts[0], lefts[1]);
        assert_eq!(lefts[2], lefts[3]);
        assert!(lefts[4] > lefts[2]);
        assert!(Layout::grid(&[]).is_empty());
    }

    #[test]
    fn removing_a_pane_gives_its_room_to_its_sibling() {
        let mut l = Layout::single(0);
        l.split(0, Dir::Across, 1);
        l.split(1, Dir::Down, 2);
        assert_eq!(l.remove(1), Some(2), "focus goes to the sibling");
        assert_eq!(
            l.place(10, 21).panes,
            [(0, rect(0, 0, 10, 10)), (2, rect(0, 11, 10, 10))]
        );
        assert_eq!(l.remove(5), None);
        assert_eq!(l.remove(0), Some(2));
        assert_eq!(l.place(10, 21).panes, [(2, rect(0, 0, 10, 21))]);
        assert_eq!(l.remove(2), None);
        assert!(l.is_empty());
        assert!(l.place(10, 21).panes.is_empty());
    }

    #[test]
    fn a_grid_too_small_to_split_squeezes_panes_out() {
        let mut l = Layout::single(0);
        l.split(0, Dir::Across, 1);
        let p = l.place(4, 2);
        assert_eq!(p.panes, [(0, rect(0, 0, 4, 2)), (1, rect(0, 2, 4, 0))]);
        assert!(p.dividers.is_empty());
    }

    #[test]
    fn focus_moves_to_the_pane_across_the_divider() {
        // 0 | 1
        //   |---
        //   | 2
        let mut l = Layout::single(0);
        l.split(0, Dir::Across, 1);
        l.split(1, Dir::Down, 2);
        let p = l.place(11, 21).panes;
        // 1 and 2 share 5 rows each of 0's edge: the tie goes to the later.
        assert_eq!(neighbor(&p, 0, Toward::Right), Some(2));
        let mut wide = Layout::single(0);
        wide.split(0, Dir::Down, 1);
        wide.split(1, Dir::Across, 2);
        wide.split(2, Dir::Across, 3);
        // 0 on top; below it 1 (15 columns) | 2 (7) | 3 (7).
        let w = wide.place(11, 31).panes;
        assert_eq!(neighbor(&w, 0, Toward::Down), Some(1), "most of the edge");
        assert_eq!(neighbor(&w, 3, Toward::Up), Some(0));
        assert_eq!(neighbor(&p, 1, Toward::Left), Some(0));
        assert_eq!(neighbor(&p, 1, Toward::Down), Some(2));
        assert_eq!(neighbor(&p, 2, Toward::Up), Some(1));
        assert_eq!(neighbor(&p, 0, Toward::Left), None);
        assert_eq!(neighbor(&p, 2, Toward::Right), None);
        assert_eq!(pane_at(&p, 7, 15), Some(2));
        assert_eq!(pane_at(&p, 5, 15), None, "a divider");
    }

    #[test]
    fn a_zoomed_pane_takes_the_whole_grid_and_the_rest_wait() {
        let l = Layout::grid(&[0, 1, 2, 3]);
        let z = l.place_zoomed(Some(2), 30, 100);
        assert_eq!(
            z.panes,
            vec![(
                2,
                Rect {
                    top: 0,
                    left: 0,
                    rows: 30,
                    cols: 100
                }
            )]
        );
        assert!(z.dividers.is_empty());
        assert_eq!(l.place_zoomed(None, 30, 100), l.place(30, 100));
        // A pane no longer in the layout zooms nothing.
        assert_eq!(l.place_zoomed(Some(9), 30, 100), l.place(30, 100));
    }
}
