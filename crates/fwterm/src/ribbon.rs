//! The activity ribbon (design §8.7): one column at a pane's right edge that
//! maps everything the pane still holds in memory onto the pane's rows, so
//! sixteen panes can be read at a glance without reading any of them. Each
//! cell stands for a run of output lines: it shows where commands started,
//! in red where one failed, and where the view is. Lines are output lines,
//! numbered as blocks and selections number them.

/// What one ribbon cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    /// A command started in this cell's lines.
    pub prompt: bool,
    /// A command in this cell's lines failed.
    pub failed: bool,
    /// The view shows some of this cell's lines.
    pub in_view: bool,
}

/// The ribbon for a pane of `rows` rows whose memory holds lines
/// `oldest..end` (`end` the line after the last on screen) and whose view
/// shows lines `view_top..view_top + rows`. `commands` are the pane's
/// commands, oldest first: the line each started on and its exit code if
/// the shell sent one. Fewer lines than rows and each cell is one line, from
/// the top.
pub fn cells(
    rows: usize,
    oldest: u64,
    end: u64,
    view_top: u64,
    commands: impl Iterator<Item = (u64, Option<i32>)>,
) -> Vec<Cell> {
    let mut out = vec![Cell::default(); rows];
    if rows == 0 || end <= oldest {
        return out;
    }
    let total = end - oldest;
    // Lines per cell, rounded up, so the whole span fits.
    let per = total.div_ceil(rows as u64).max(1);
    let cell_of = |line: u64| -> Option<usize> {
        (line >= oldest && line < end).then(|| ((line - oldest) / per) as usize)
    };
    for (line, exit) in commands {
        if let Some(c) = cell_of(line).filter(|&c| c < rows) {
            out[c].prompt = true;
            if exit.is_some_and(|e| e != 0) {
                out[c].failed = true;
            }
        }
    }
    let view_end = view_top.saturating_add(rows as u64).min(end);
    let first = cell_of(view_top.max(oldest));
    let last = view_end
        .checked_sub(1)
        .filter(|&l| l >= view_top.max(oldest))
        .and_then(cell_of);
    if let (Some(a), Some(b)) = (first, last) {
        for cell in out.iter_mut().take(b.min(rows - 1) + 1).skip(a) {
            cell.in_view = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(cells: &[Cell]) -> String {
        cells
            .iter()
            .map(|c| match (c.failed, c.prompt, c.in_view) {
                (true, _, true) => 'X',
                (true, _, false) => 'x',
                (false, true, true) => 'P',
                (false, true, false) => 'p',
                (false, false, true) => '=',
                (false, false, false) => '.',
            })
            .collect()
    }

    #[test]
    fn commands_and_the_view_land_in_the_cells_their_lines_map_to() {
        // 1,000 lines held (0..1000) on 10 rows: 100 lines a cell. The view
        // is live, the last 10 lines: the bottom cell.
        let cmds = [(5, Some(0)), (250, Some(1)), (251, None), (999, None)];
        let c = cells(10, 0, 1000, 990, cmds.into_iter());
        assert_eq!(flags(&c), "p.x......P");
        // Scrolled back to lines 300..310: the fourth cell.
        let c = cells(10, 0, 1000, 300, cmds.into_iter());
        assert_eq!(flags(&c), "p.x=.....p");
        // History trimmed: lines before 200 are gone and are not mapped.
        let c = cells(10, 200, 1000, 990, cmds.into_iter());
        assert_eq!(flags(&c), "x........P", "80 lines a cell, 10 cells");
        assert_eq!(c.len(), 10);
    }

    #[test]
    fn fewer_lines_than_rows_take_one_line_each_from_the_top() {
        let c = cells(8, 0, 3, 0, [(1, Some(2))].into_iter());
        assert_eq!(flags(&c), "=X=.....");
        // A view spanning more cells than it has lines is clipped to them.
        let c = cells(4, 10, 12, 10, std::iter::empty());
        assert_eq!(flags(&c), "==..");
    }

    #[test]
    fn empty_or_absurd_inputs_give_blank_ribbons() {
        assert!(cells(0, 0, 10, 0, std::iter::empty()).is_empty());
        assert_eq!(flags(&cells(3, 10, 10, 10, std::iter::empty())), "...");
        assert_eq!(flags(&cells(3, 10, 5, 0, std::iter::empty())), "...");
        // A view past the end shows nothing in view.
        assert_eq!(flags(&cells(3, 0, 30, 100, [(0, None)].into_iter())), "p..");
    }
}
