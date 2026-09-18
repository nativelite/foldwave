//! foldwave M0 harness: measure what the machine actually does under an
//! agent-like load.
//!
//! `docs/design.md` section 10 names M0 as the first milestone, and section 4
//! sets the targets it has to measure. This crate covers two of them today:
//!
//! - **Throughput** — N pty panes flooding at once ([`flood`]).
//! - **Input latency** — key echo timed on arrival ([`echo`]).
//! - **Storage** — the log drive measured the way the log writer will use
//!   it: unbuffered write-through, queued ([`disk`]).
//! - **Resource attribution** — where the machine went during the run, across
//!   CPU, RAM, disk and GPU, via `resmon` (experiment E3, section 8.7b).
//!
//! Frame timing is deliberately absent: it needs a renderer, which is M3.
//!
//! Zero third-party dependencies; nativelite org crates and std only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod disk;
pub mod echo;
pub mod emit;
pub mod flood;
pub mod report;
pub mod stats;

pub use echo::EchoResult;
pub use flood::{FloodResult, PaneResult};
pub use stats::Summary;

/// A complete M0 run: the load, the latency under it, and what it cost.
#[derive(Debug)]
pub struct Run {
    /// The flood result, when panes were run.
    pub flood: Option<FloodResult>,
    /// The key-echo result, when latency was measured.
    pub echo: Option<EchoResult>,
    /// Resource attribution across the whole run.
    pub attribution: Option<resmon::Attribution>,
}
