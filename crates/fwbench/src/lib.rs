//! foldwave M0 harness: measure what the machine actually does under an
//! agent-like load.
//!
//! `docs/design.md` section 10 names M0 as the first milestone, and section 4
//! sets the targets it has to measure. This crate covers two of them today:
//!
//! - **Throughput** — N pty panes flooding at once ([`flood`]).
//! - **Input latency** — key echo timed on arrival ([`echo`]).
//! - **Storage** — the log drive measured the way the log writer will use
//!   it: unbuffered write-through, queued ([`disk`]); and read back, with and
//!   without NTFS compression ([`read`]).
//! - **Parsing** — the parse budget and the GPU fit, experiment E4 ([`parse`],
//!   [`fsm`], [`scan`]), and the host-to-GPU copy cost behind it ([`gpu`]).
//! - **The composed pipeline** — log + parse + scan on N panes at once,
//!   experiment E5 ([`compose`]).
//! - **Crash and replay** — kill a writer mid-append, check the log against
//!   the stream that made it, rebuild a screen from it ([`replay`]).
//! - **Resource attribution** — where the machine went during the run, across
//!   CPU, RAM, disk and GPU, via `resmon` (experiment E3, section 8.7b).
//!
//! Every run ends with the noisy-run guard ([`noise`]): CPU used by other
//! processes during the run, and whether it exceeded the limit.
//!
//! Frame timing is deliberately absent: it needs a renderer, which is M3.
//!
//! Zero third-party dependencies; nativelite org crates and std only.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod compose;
pub mod disk;
pub mod echo;
pub mod emit;
pub mod flood;
pub mod fsm;
pub mod glyphs;
pub mod gpu;
pub mod history;
pub mod noise;
pub mod parse;
pub mod read;
pub mod render;
pub mod replay;
pub mod report;
pub mod scan;
pub mod stats;
pub mod terminal;

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
