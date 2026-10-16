//! Blocks: a pane's output cut into commands, from shell-integration marks
//! (OSC 133, design §8.7). Each block is a prompt, the command typed at it,
//! the command's output, and its exit code when the shell sends one.
//!
//! Lines are output lines, counted from the first line the pane ever showed
//! (the same numbering as selections: lines scrolled into history plus the
//! screen row). A shell that sends only some marks still gives blocks: cmd
//! sends prompt start (`A`), command start (`B`) and end (`D`, no exit code),
//! so a command's output is taken to start on the line after its command.

use ansi::MarkKind;

/// One command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Line the prompt starts on (`A`).
    pub prompt: u64,
    /// Line the command is typed on (`B`).
    pub command: Option<u64>,
    /// Line its output starts on (`C`), when the shell says.
    pub output: Option<u64>,
    /// Line the command ended on (`D`), and its exit code if sent.
    pub end: Option<u64>,
    pub exit: Option<i32>,
}

impl Block {
    /// The lines of the command's output, `start..end`: from `C` (or the line
    /// after the command) to `D` (or `next_prompt`). `None` before it ran.
    pub fn output_lines(&self, next_prompt: Option<u64>) -> Option<std::ops::Range<u64>> {
        let start = self.output.or(self.command.map(|c| c + 1))?;
        let end = self.end.or(next_prompt)?;
        (start < end).then_some(start..end)
    }
}

/// A pane's blocks, oldest first.
#[derive(Debug, Default)]
pub struct Blocks {
    list: Vec<Block>,
}

impl Blocks {
    /// Record a mark at `line`.
    pub fn mark(&mut self, kind: MarkKind, line: u64, exit: Option<i32>) {
        match kind {
            MarkKind::PromptStart => self.list.push(Block {
                prompt: line,
                command: None,
                output: None,
                end: None,
                exit: None,
            }),
            // Marks that belong to the newest block; one without a block is
            // a stray (a `D` before the first prompt) and is ignored.
            MarkKind::CommandStart => {
                if let Some(b) = self.list.last_mut() {
                    b.command.get_or_insert(line);
                }
            }
            MarkKind::OutputStart => {
                if let Some(b) = self.list.last_mut() {
                    b.output.get_or_insert(line);
                }
            }
            MarkKind::CommandEnd => {
                if let Some(b) = self.list.last_mut().filter(|b| b.end.is_none()) {
                    // cmd sends `D` just before its next prompt: an end on
                    // the very line of the command, with no output mark, is
                    // an empty command line.
                    b.end = Some(line);
                    b.exit = exit;
                }
            }
        }
    }

    /// Every command, oldest first: the line it started on (its command
    /// line, else its prompt) and its exit code if the shell sent one.
    pub fn commands(&self) -> impl Iterator<Item = (u64, Option<i32>)> + '_ {
        self.list
            .iter()
            .map(|b| (b.command.unwrap_or(b.prompt), b.exit))
    }

    /// Forget blocks whose prompt is before `first`, the oldest line the
    /// pane still holds.
    pub fn forget_before(&mut self, first: u64) {
        let keep = self.list.partition_point(|b| b.prompt < first);
        self.list.drain(..keep);
    }

    /// The last prompt strictly before `line`.
    pub fn prev_prompt(&self, line: u64) -> Option<u64> {
        let i = self.list.partition_point(|b| b.prompt < line);
        i.checked_sub(1).map(|i| self.list[i].prompt)
    }

    /// The first prompt strictly after `line`.
    pub fn next_prompt(&self, line: u64) -> Option<u64> {
        let i = self.list.partition_point(|b| b.prompt <= line);
        self.list.get(i).map(|b| b.prompt)
    }

    /// The output of the newest command at or before `line` that has any.
    pub fn output_at(&self, line: u64) -> Option<std::ops::Range<u64>> {
        let upto = self.list.partition_point(|b| b.prompt <= line);
        (0..upto).rev().find_map(|i| {
            let next = self.list.get(i + 1).map(|b| b.prompt);
            self.list[i].output_lines(next)
        })
    }

    /// Commands that failed (a nonzero exit code) whose command line falls in
    /// `lines`: that line and the code. A shell that sends no exit code (cmd)
    /// has none.
    pub fn failed_in(&self, lines: std::ops::Range<u64>) -> impl Iterator<Item = (u64, i32)> + '_ {
        // A command line comes before the next prompt, so of the blocks that
        // start before `lines` only the last can reach into it.
        let from = self
            .list
            .partition_point(|b| b.prompt < lines.start)
            .saturating_sub(1);
        self.list[from..]
            .iter()
            .take_while(move |b| b.prompt < lines.end)
            .filter_map(move |b| {
                let line = b.command.unwrap_or(b.prompt);
                let code = b.exit.filter(|&c| c != 0)?;
                lines.contains(&line).then_some((line, code))
            })
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[cfg(test)]
    pub fn get(&self, i: usize) -> Option<&Block> {
        self.list.get(i)
    }
}

/// Where a failed command's `exit N` label goes on its command row, `cols`
/// wide: right-aligned with a blank cell either side, and only where every
/// cell it needs is `blank`, so it never covers output. The starting column
/// and the label; `None` when it does not fit.
pub fn exit_label(
    code: i32,
    cols: usize,
    blank: impl Fn(usize) -> bool,
) -> Option<(usize, String)> {
    let label = format!("exit {code}");
    let n = label.len();
    if cols < n + 2 {
        return None;
    }
    let start = cols - n - 1;
    (start - 1..cols).all(blank).then_some((start, label))
}

#[cfg(test)]
mod tests {
    use super::*;
    use MarkKind::*;

    #[test]
    fn the_exit_label_sits_right_on_blank_cells_only() {
        let row = |text: &str| {
            let chars: Vec<char> = text.chars().collect();
            move |c: usize| chars.get(c).map_or(true, |&ch| ch == ' ')
        };
        // "C:\> type nofile" on a 30-column row: "exit 1" in cols 23..29.
        assert_eq!(
            exit_label(1, 30, row("C:\\> type nofile")),
            Some((23, "exit 1".into()))
        );
        // A command reaching into the label's cells: no label.
        assert_eq!(exit_label(1, 30, row(&"x".repeat(23))), None);
        assert_eq!(exit_label(-1073741819, 12, row("")), None, "too narrow");
    }

    /// cmd with fwterm's prompt: `D A <prompt> B`, the user's command on the
    /// B line, its output on the lines after, then `D A B` again.
    fn cmd_session() -> Blocks {
        let mut b = Blocks::default();
        b.mark(CommandEnd, 0, None); // before any prompt: ignored
        b.mark(PromptStart, 0, None);
        b.mark(CommandStart, 0, None); // "dir" typed on line 0
        b.mark(CommandEnd, 5, None); // output on lines 1..5
        b.mark(PromptStart, 5, None);
        b.mark(CommandStart, 5, None); // an empty Enter
        b.mark(CommandEnd, 6, None);
        b.mark(PromptStart, 6, None);
        b.mark(CommandStart, 6, None); // still typing
        b
    }

    #[test]
    fn cmd_marks_make_blocks_with_output_after_the_command_line() {
        let b = cmd_session();
        assert_eq!(b.len(), 3);
        assert_eq!(b.get(0).unwrap().output_lines(Some(5)), Some(1..5));
        // The empty Enter printed nothing: 6..6 is no output.
        assert_eq!(b.get(1).unwrap().output_lines(Some(6)), None);
        // The command still being typed has not ended.
        assert_eq!(b.get(2).unwrap().output_lines(None), None);
    }

    #[test]
    fn jumps_go_prompt_to_prompt() {
        let b = cmd_session();
        assert_eq!(b.prev_prompt(6), Some(5));
        assert_eq!(b.prev_prompt(5), Some(0));
        assert_eq!(b.prev_prompt(0), None);
        assert_eq!(b.next_prompt(0), Some(5));
        assert_eq!(b.next_prompt(3), Some(5));
        assert_eq!(b.next_prompt(6), None);
    }

    #[test]
    fn the_output_near_a_line_skips_commands_that_printed_nothing() {
        let b = cmd_session();
        // At the live prompt: the newest command with output is the first.
        assert_eq!(b.output_at(6), Some(1..5));
        assert_eq!(b.output_at(2), Some(1..5));
    }

    #[test]
    fn full_marks_use_output_start_and_exit_code() {
        let mut b = Blocks::default();
        b.mark(PromptStart, 10, None);
        b.mark(CommandStart, 10, None);
        b.mark(OutputStart, 11, None);
        b.mark(CommandEnd, 20, Some(2));
        let blk = b.get(0).unwrap();
        assert_eq!(blk.output_lines(None), Some(11..20));
        assert_eq!(blk.exit, Some(2));
    }

    #[test]
    fn only_failed_commands_in_view_are_reported_on_their_command_line() {
        let mut b = Blocks::default();
        // Two-line prompts: the command is typed on the line after `A`.
        for (prompt, exit) in [(0, Some(0)), (10, Some(2)), (20, None), (30, Some(-1))] {
            b.mark(PromptStart, prompt, None);
            b.mark(CommandStart, prompt + 1, None);
            b.mark(CommandEnd, prompt + 9, exit);
        }
        let all: Vec<_> = b.failed_in(0..100).collect();
        assert_eq!(all, [(11, 2), (31, -1)], "0 and no code are not failures");
        // A view starting between a prompt and its command line still shows it.
        assert_eq!(b.failed_in(11..12).collect::<Vec<_>>(), [(11, 2)]);
        assert_eq!(b.failed_in(12..31).count(), 0);
        assert_eq!(
            cmd_session().failed_in(0..100).count(),
            0,
            "cmd sends no codes"
        );
    }

    #[test]
    fn blocks_scrolled_out_of_memory_are_forgotten() {
        let mut b = cmd_session();
        b.forget_before(3);
        assert_eq!(b.len(), 2);
        assert_eq!(b.prev_prompt(6), Some(5));
        assert_eq!(b.prev_prompt(5), None);
    }
}
