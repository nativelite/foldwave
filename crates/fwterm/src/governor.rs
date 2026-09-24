//! The resource governor, first slice (design §8.8): foldwave should add
//! near-zero CPU to a machine the agents have already taken.
//!
//! - **Placement** (`--place fast`, off by default). Threads that move output
//!   (pane readers, which also parse) and the UI thread can be kept on the
//!   fast cores of a hybrid CPU. E4 measured one pinned thread at 125.5 MiB/s
//!   on a P-core, 100.4 on CPU 0 (which takes the machine's interrupts) and
//!   55.4 on an E-core, but inside fwterm the OS already schedules them well:
//!   placed, one pane gained ~1.5 % and 16 panes lost 10-20 % of their frame
//!   rate (design §8.8).
//! - **Pressure.** Every half second the governor reads how busy the machine
//!   is apart from fwterm. When other programs hold nearly all of it, frames
//!   are drawn at most [`BUSY_FPS`] times a second, except just after input:
//!   the focused pane's echo is never slowed.

use resmon::cpu::{Cpu, Load};
use std::time::Duration;

/// Where pane readers and the UI thread run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Anywhere the OS schedules them.
    Off,
    /// On the fastest class of core, CPU 0 left out.
    Fast,
}

/// The CPU set ids for `place`; empty means no restriction (placement off,
/// or every core the same kind, so there is nothing to prefer).
pub fn placement(cpus: &[Cpu], place: Place) -> Vec<u32> {
    if place == Place::Off {
        return Vec::new();
    }
    let (Some(top), Some(low)) = (
        cpus.iter().map(|c| c.efficiency_class).max(),
        cpus.iter().map(|c| c.efficiency_class).min(),
    ) else {
        return Vec::new();
    };
    if top == low {
        return Vec::new();
    }
    let fast: Vec<&Cpu> = cpus.iter().filter(|c| c.efficiency_class == top).collect();
    let without_zero: Vec<u32> = fast
        .iter()
        .filter(|c| c.logical != 0)
        .map(|c| c.id)
        .collect();
    if without_zero.is_empty() {
        fast.iter().map(|c| c.id).collect()
    } else {
        without_zero
    }
}

/// How hard other programs are pressing on the CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Calm,
    Busy,
}

/// Busy: the machine is nearly full and other programs hold at least half
/// of it. Calm again once either eases past the lower mark; the gaps keep it
/// from flapping.
pub const BUSY_MACHINE: f64 = 0.90;
pub const BUSY_OTHERS: f64 = 0.50;
pub const CALM_MACHINE: f64 = 0.80;
pub const CALM_OTHERS: f64 = 0.35;

/// The level after a reading. A machine full of fwterm's own work (a flood
/// through its panes) is fwterm doing its job, not pressure on it; but a
/// machine that is full because others took most of it is, even while
/// fwterm's share is large (16 flooding panes hold ~3 of 16 cores).
pub fn next(level: Level, load: Load) -> Level {
    let others = (load.machine - load.process).max(0.0);
    match level {
        Level::Calm if load.machine >= BUSY_MACHINE && others >= BUSY_OTHERS => Level::Busy,
        Level::Busy if load.machine < CALM_MACHINE || others < CALM_OTHERS => Level::Calm,
        l => l,
    }
}

/// Frames per second while the machine is busy.
pub const BUSY_FPS: u32 = 30;

/// How long after input frames stay at the full rate.
pub const INPUT_GRACE: Duration = Duration::from_millis(250);

/// The shortest time between frames, given the display's `base` interval.
pub fn frame_interval(base: Duration, level: Level, since_input: Duration) -> Duration {
    if level == Level::Busy && since_input >= INPUT_GRACE {
        base.max(Duration::from_secs(1) / BUSY_FPS)
    } else {
        base
    }
}

/// When a hidden pane (another is zoomed) stops parsing and only logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hidden {
    /// While the machine is busy: the first thing §8.8 sheds.
    Busy,
    Always,
    Never,
}

/// Whether a pane keeps its screen live. Only a hidden pane with a log may
/// stop: the log is how it catches up when shown.
pub fn keep_parsing(hidden: bool, logged: bool, policy: Hidden, level: Level) -> bool {
    if !hidden || !logged {
        return true;
    }
    match policy {
        Hidden::Never => true,
        Hidden::Always => false,
        Hidden::Busy => level == Level::Calm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(id: u32, logical: u8, class: u8) -> Cpu {
        Cpu {
            id,
            logical,
            core: logical,
            efficiency_class: class,
        }
    }

    #[test]
    fn placement_picks_the_fast_cores_without_cpu_zero() {
        // This machine: 12 P-core threads (class 1), 4 E-cores (class 0).
        let hybrid: Vec<Cpu> = (0..16)
            .map(|i| cpu(256 + i, i as u8, u8::from(i < 12)))
            .collect();
        assert_eq!(
            placement(&hybrid, Place::Fast),
            (257..268).collect::<Vec<u32>>()
        );
        assert!(placement(&hybrid, Place::Off).is_empty());
        let uniform: Vec<Cpu> = (0..8).map(|i| cpu(i, i as u8, 0)).collect();
        assert!(placement(&uniform, Place::Fast).is_empty());
        assert!(placement(&[], Place::Fast).is_empty());
        // One fast CPU, and it is CPU 0: keep it rather than nothing.
        assert_eq!(
            placement(&[cpu(5, 0, 1), cpu(6, 1, 0)], Place::Fast),
            vec![5]
        );
    }

    #[test]
    fn only_other_programs_load_makes_it_busy_and_it_does_not_flap() {
        let load = |machine, process| Load { machine, process };
        assert_eq!(
            next(Level::Calm, load(0.95, 0.5)),
            Level::Calm,
            "fwterm's own flood"
        );
        assert_eq!(next(Level::Calm, load(0.95, 0.05)), Level::Busy);
        // Full, with fwterm holding a fifth of it (16 flooding panes).
        assert_eq!(next(Level::Calm, load(0.99, 0.19)), Level::Busy);
        assert_eq!(next(Level::Calm, load(0.85, 0.05)), Level::Calm, "not full");
        assert_eq!(
            next(Level::Busy, load(0.85, 0.05)),
            Level::Busy,
            "inside the gap"
        );
        assert_eq!(next(Level::Busy, load(0.60, 0.05)), Level::Calm);
        assert_eq!(
            next(Level::Busy, load(0.99, 0.70)),
            Level::Calm,
            "fwterm's own"
        );
    }

    #[test]
    fn only_hidden_logged_panes_stop_parsing_and_by_policy() {
        use Level::*;
        assert!(keep_parsing(false, true, Hidden::Always, Busy), "shown");
        assert!(keep_parsing(true, false, Hidden::Always, Busy), "no log");
        assert!(!keep_parsing(true, true, Hidden::Always, Calm));
        assert!(keep_parsing(true, true, Hidden::Never, Busy));
        assert!(keep_parsing(true, true, Hidden::Busy, Calm));
        assert!(!keep_parsing(true, true, Hidden::Busy, Busy));
    }

    #[test]
    fn a_busy_machine_slows_frames_except_just_after_input() {
        let base = Duration::from_secs(1) / 165;
        let slow = Duration::from_secs(1) / BUSY_FPS;
        let long = Duration::from_secs(5);
        assert_eq!(frame_interval(base, Level::Calm, long), base);
        assert_eq!(frame_interval(base, Level::Busy, long), slow);
        assert_eq!(
            frame_interval(base, Level::Busy, Duration::from_millis(10)),
            base
        );
        // A display already slower than the busy rate is left alone.
        let rdp = Duration::from_secs(1) / 31;
        assert_eq!(frame_interval(rdp, Level::Busy, long), slow.max(rdp));
    }
}
