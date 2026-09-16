//! Process records, tree building and classification.
//!
//! Everything here is pure data manipulation: no syscalls, no clock. The
//! platform layer ([`crate::sys`]) produces [`RawProc`] values and this module
//! turns them into trees and classes. That split is what makes the interesting
//! logic testable on any platform.

use std::collections::{BTreeMap, BTreeSet};

/// One process as sampled at an instant.
///
/// Counters (`cpu_*`, `io_*`) are **cumulative since process start**; gauges
/// (`working_set`, `private_bytes`) are instantaneous. [`crate::delta`] turns
/// pairs of these into per-window usage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RawProc {
    /// Operating-system process id, unique only among live processes.
    pub pid: u32,
    /// The pid of the creating process, which may already have exited.
    pub parent_pid: u32,
    /// Executable file name as the OS reports it, e.g. `rustc.exe`.
    pub name: String,
    /// Process creation time in platform units. Only ever compared for
    /// equality, to tell a restarted or pid-reused process from a continuing
    /// one when differencing two snapshots.
    pub start_time: u64,
    /// Cumulative kernel-mode CPU time, in nanoseconds.
    pub cpu_kernel_ns: u64,
    /// Cumulative user-mode CPU time, in nanoseconds.
    pub cpu_user_ns: u64,
    /// Current working set (resident bytes).
    pub working_set: u64,
    /// Current private (non-shared) committed bytes.
    pub private_bytes: u64,
    /// Cumulative bytes read. See [`crate::Attribution`] for the caveat: the
    /// OS counts every kind of read here, not only disk.
    pub io_read_bytes: u64,
    /// Cumulative bytes written, same caveat.
    pub io_write_bytes: u64,
    /// Cumulative read operations.
    pub io_read_ops: u64,
    /// Cumulative write operations.
    pub io_write_ops: u64,
    /// True when the sampler could not open the process, so the counters above
    /// are zero because they are unknown, not because they are idle.
    pub denied: bool,
}

impl RawProc {
    /// Total cumulative CPU time in nanoseconds.
    pub fn cpu_ns(&self) -> u64 {
        self.cpu_kernel_ns.saturating_add(self.cpu_user_ns)
    }
}

/// What kind of work a process is doing, for attribution.
///
/// This is a documented heuristic over executable names, not ground truth. It
/// exists so an attribution table can answer "how much of this machine is
/// agents, and how much is us?" -- see `docs/design.md` section 8.7b, E3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// The terminal under test, and other terminals we benchmark against.
    Foldwave,
    /// Agent CLIs and local model servers.
    Agent,
    /// Compilers, linkers and build drivers.
    Compiler,
    /// Shells and console hosts.
    Shell,
    /// Browsers -- usually the loudest neighbour on a developer box.
    Browser,
    /// Everything else.
    Other,
}

impl Class {
    /// Stable lowercase label, used in output and tests.
    pub fn label(self) -> &'static str {
        match self {
            Class::Foldwave => "foldwave",
            Class::Agent => "agent",
            Class::Compiler => "compiler",
            Class::Shell => "shell",
            Class::Browser => "browser",
            Class::Other => "other",
        }
    }

    /// Every class, in report order.
    pub fn all() -> [Class; 6] {
        [
            Class::Foldwave,
            Class::Agent,
            Class::Compiler,
            Class::Shell,
            Class::Browser,
            Class::Other,
        ]
    }
}

/// Executable stems mapped to a class. Matched case-insensitively against the
/// file name with any extension removed, so `rustc.exe` and `rustc` agree.
const RULES: &[(&str, Class)] = &[
    // The terminal under test, and the terminals we benchmark against.
    ("foldwave", Class::Foldwave),
    ("resmon", Class::Foldwave),
    ("fwbench", Class::Foldwave),
    ("atrium", Class::Foldwave),
    ("windowsterminal", Class::Foldwave),
    ("alacritty", Class::Foldwave),
    ("wezterm-gui", Class::Foldwave),
    ("wezterm", Class::Foldwave),
    ("ghostty", Class::Foldwave),
    // Agent CLIs and local model servers.
    ("claude", Class::Agent),
    ("codex", Class::Agent),
    ("aider", Class::Agent),
    ("ollama", Class::Agent),
    ("llama-server", Class::Agent),
    ("node", Class::Agent),
    ("bun", Class::Agent),
    ("deno", Class::Agent),
    // Compilers, linkers, build drivers.
    ("cargo", Class::Compiler),
    ("rustc", Class::Compiler),
    ("rustfmt", Class::Compiler),
    ("cc1", Class::Compiler),
    ("cc1plus", Class::Compiler),
    ("gcc", Class::Compiler),
    ("clang", Class::Compiler),
    ("cl", Class::Compiler),
    ("link", Class::Compiler),
    ("ld", Class::Compiler),
    ("lld-link", Class::Compiler),
    ("msbuild", Class::Compiler),
    ("tsc", Class::Compiler),
    ("javac", Class::Compiler),
    ("go", Class::Compiler),
    ("make", Class::Compiler),
    ("ninja", Class::Compiler),
    // Shells and console hosts.
    ("cmd", Class::Shell),
    ("powershell", Class::Shell),
    ("pwsh", Class::Shell),
    ("bash", Class::Shell),
    ("sh", Class::Shell),
    ("zsh", Class::Shell),
    ("fish", Class::Shell),
    ("conhost", Class::Shell),
    ("openconsole", Class::Shell),
    // Browsers.
    ("chrome", Class::Browser),
    ("msedge", Class::Browser),
    ("firefox", Class::Browser),
    ("brave", Class::Browser),
];

/// Reduce an executable name to the stem the rules match on.
///
/// Strips any directory part and a single trailing extension, and lowercases
/// the result, so a full path and a bare name agree.
pub fn stem(name: &str) -> String {
    let file = name
        .rsplit(|c| c == '\\' || c == '/')
        .next()
        .unwrap_or(name);
    let base = match file.rfind('.') {
        // A leading dot is part of the name, not an extension.
        Some(i) if i > 0 => &file[..i],
        _ => file,
    };
    base.to_ascii_lowercase()
}

/// Classify a process by executable name.
pub fn classify(name: &str) -> Class {
    let stem = stem(name);
    RULES
        .iter()
        .find(|(pat, _)| *pat == stem)
        .map(|(_, class)| *class)
        .unwrap_or(Class::Other)
}

/// Parent-of relation over a set of sampled processes.
pub struct Tree {
    parent: BTreeMap<u32, u32>,
    children: BTreeMap<u32, Vec<u32>>,
}

impl Tree {
    /// Build the tree from a snapshot's processes.
    pub fn build(procs: &[RawProc]) -> Tree {
        let present: BTreeSet<u32> = procs.iter().map(|p| p.pid).collect();
        let mut parent = BTreeMap::new();
        let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
        for p in procs {
            // A parent missing from the snapshot has exited, so this process is
            // its own root. Self-parenting (seen for pid 0) is ignored too.
            if p.parent_pid != p.pid && present.contains(&p.parent_pid) {
                parent.insert(p.pid, p.parent_pid);
                children.entry(p.parent_pid).or_default().push(p.pid);
            }
        }
        Tree { parent, children }
    }

    /// The topmost ancestor of `pid` within the snapshot.
    ///
    /// Cycles, which the OS should never produce but a stale snapshot might,
    /// terminate the walk rather than hanging.
    pub fn root_of(&self, pid: u32) -> u32 {
        let mut seen = BTreeSet::new();
        let mut cur = pid;
        while let Some(&up) = self.parent.get(&cur) {
            if !seen.insert(cur) {
                break;
            }
            cur = up;
        }
        cur
    }

    /// `pid` and every descendant, in ascending pid order.
    pub fn descendants(&self, pid: u32) -> Vec<u32> {
        let mut out = BTreeSet::new();
        let mut stack = vec![pid];
        while let Some(cur) = stack.pop() {
            if !out.insert(cur) {
                continue;
            }
            if let Some(kids) = self.children.get(&cur) {
                stack.extend(kids.iter().copied());
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent_pid: u32, name: &str) -> RawProc {
        RawProc {
            pid,
            parent_pid,
            name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn stem_strips_path_and_extension_and_case() {
        assert_eq!(stem("rustc"), "rustc");
        assert_eq!(stem("Rustc.EXE"), "rustc");
        assert_eq!(stem(r"C:\bin\Rustc.exe"), "rustc");
        assert_eq!(stem("/usr/bin/rustc"), "rustc");
    }

    #[test]
    fn stem_keeps_leading_dot_names_whole() {
        assert_eq!(stem(".gitconfig"), ".gitconfig");
    }

    #[test]
    fn stem_of_empty_is_empty() {
        assert_eq!(stem(""), "");
    }

    #[test]
    fn classify_covers_each_class() {
        assert_eq!(classify("foldwave.exe"), Class::Foldwave);
        assert_eq!(classify("fwbench.exe"), Class::Foldwave);
        assert_eq!(classify("claude.exe"), Class::Agent);
        assert_eq!(classify("rustc.exe"), Class::Compiler);
        assert_eq!(classify("bash.exe"), Class::Shell);
        assert_eq!(classify("chrome.exe"), Class::Browser);
        assert_eq!(classify("explorer.exe"), Class::Other);
    }

    #[test]
    fn classify_ignores_case_and_directory() {
        assert_eq!(
            classify(r"C:\Program Files\Git\usr\bin\BASH.EXE"),
            Class::Shell
        );
    }

    #[test]
    fn class_labels_are_unique() {
        let mut seen = BTreeSet::new();
        for c in Class::all() {
            assert!(seen.insert(c.label()), "duplicate label {}", c.label());
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn cpu_ns_sums_kernel_and_user() {
        let mut proc = p(1, 0, "x");
        proc.cpu_kernel_ns = 300;
        proc.cpu_user_ns = 700;
        assert_eq!(proc.cpu_ns(), 1000);
    }

    #[test]
    fn cpu_ns_saturates_instead_of_overflowing() {
        let mut proc = p(1, 0, "x");
        proc.cpu_kernel_ns = u64::MAX;
        proc.cpu_user_ns = 5;
        assert_eq!(proc.cpu_ns(), u64::MAX);
    }

    #[test]
    fn root_of_walks_to_the_top() {
        let procs = vec![p(1, 0, "init"), p(10, 1, "shell"), p(20, 10, "agent")];
        let tree = Tree::build(&procs);
        assert_eq!(tree.root_of(20), 1);
        assert_eq!(tree.root_of(10), 1);
        assert_eq!(tree.root_of(1), 1);
    }

    #[test]
    fn a_process_whose_parent_exited_is_its_own_root() {
        // pid 99 is absent from the snapshot, so 10 is orphaned.
        let procs = vec![p(10, 99, "shell"), p(20, 10, "agent")];
        let tree = Tree::build(&procs);
        assert_eq!(tree.root_of(10), 10);
        assert_eq!(tree.root_of(20), 10);
    }

    #[test]
    fn self_parenting_does_not_hang() {
        let procs = vec![p(0, 0, "system idle")];
        let tree = Tree::build(&procs);
        assert_eq!(tree.root_of(0), 0);
    }

    #[test]
    fn a_parent_cycle_terminates() {
        let procs = vec![p(1, 2, "a"), p(2, 1, "b")];
        let tree = Tree::build(&procs);
        let _ = tree.root_of(1);
        let _ = tree.root_of(2);
    }

    #[test]
    fn descendants_include_self_and_all_children() {
        let procs = vec![
            p(1, 0, "init"),
            p(10, 1, "shell"),
            p(20, 10, "agent"),
            p(21, 10, "agent"),
            p(30, 20, "rustc"),
            p(40, 1, "other"),
        ];
        let tree = Tree::build(&procs);
        assert_eq!(tree.descendants(10), vec![10, 20, 21, 30]);
        assert_eq!(tree.descendants(30), vec![30]);
    }

    #[test]
    fn descendants_of_an_unknown_pid_is_just_that_pid() {
        let tree = Tree::build(&[p(1, 0, "init")]);
        assert_eq!(tree.descendants(999), vec![999]);
    }
}
