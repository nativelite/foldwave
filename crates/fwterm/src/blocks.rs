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

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[cfg(test)]
    pub fn get(&self, i: usize) -> Option<&Block> {
        self.list.get(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MarkKind::*;

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
    fn blocks_scrolled_out_of_memory_are_forgotten() {
        let mut b = cmd_session();
        b.forget_before(3);
        assert_eq!(b.len(), 2);
        assert_eq!(b.prev_prompt(6), Some(5));
        assert_eq!(b.prev_prompt(5), None);
    }
}
