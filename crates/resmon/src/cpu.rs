//! Processors: what kind each logical CPU is, how busy the machine and this
//! process are, and where the current thread may run. The resource governor
//! (design §8.8) reads these and places its threads with them.

use std::io;

/// One logical processor, as the OS schedules it (a Windows CPU set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    /// The id thread placement names it by.
    pub id: u32,
    /// Index of the logical processor within its group.
    pub logical: u8,
    /// The physical core it is on: hyper-threads share one.
    pub core: u8,
    /// Higher is faster. On a hybrid Intel part, P-cores are 1 and E-cores 0;
    /// on a uniform part, every core is 0.
    pub efficiency_class: u8,
}

/// Every logical processor, in the OS's order.
pub fn topology() -> io::Result<Vec<Cpu>> {
    crate::sys::cpu_sets().map(|raw| parse_cpu_sets(&raw))
}

/// `SYSTEM_CPU_SET_INFORMATION` records, read by offset so no `unsafe` cast
/// is needed. Each starts with its own size, so newer, longer records parse.
pub fn parse_cpu_sets(raw: &[u8]) -> Vec<Cpu> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= raw.len() {
        let size = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
        let kind = u32::from_le_bytes(raw[at + 4..at + 8].try_into().unwrap());
        if size < 20 || at + size > raw.len() {
            break;
        }
        // Type 0 is CpuSetInformation; skip any other.
        if kind == 0 {
            let r = &raw[at..at + size];
            out.push(Cpu {
                id: u32::from_le_bytes(r[8..12].try_into().unwrap()),
                logical: r[14],
                core: r[15],
                efficiency_class: r[18],
            });
        }
        at += size;
    }
    out
}

/// Cumulative CPU time, in nanoseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Times {
    /// Machine-wide: busy time summed over every logical CPU.
    pub machine_busy: u64,
    /// Machine-wide: busy plus idle, summed over every logical CPU.
    pub machine_total: u64,
    /// This process: kernel plus user time, over all its threads.
    pub process: u64,
}

/// Read the counters now.
pub fn times() -> io::Result<Times> {
    crate::sys::cpu_times()
}

/// How busy things were between two readings, as fractions of the whole
/// machine (1.0 = every logical CPU busy).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Load {
    /// Everything on the machine, this process included.
    pub machine: f64,
    /// This process alone.
    pub process: f64,
}

/// The load between `before` and `after`. Zero when no time passed.
pub fn load(before: &Times, after: &Times) -> Load {
    let total = after.machine_total.saturating_sub(before.machine_total);
    if total == 0 {
        return Load {
            machine: 0.0,
            process: 0.0,
        };
    }
    let frac = |n: u64| (n as f64 / total as f64).clamp(0.0, 1.0);
    Load {
        machine: frac(after.machine_busy.saturating_sub(before.machine_busy)),
        process: frac(after.process.saturating_sub(before.process)),
    }
}

/// Let the calling thread run only on `ids` (from [`topology`]); an empty
/// list lets it run anywhere again.
pub fn place_current_thread(ids: &[u32]) -> io::Result<()> {
    crate::sys::place_current_thread(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u32, logical: u8, core: u8, class: u8) -> Vec<u8> {
        let mut r = vec![0u8; 32];
        r[0..4].copy_from_slice(&32u32.to_le_bytes());
        r[8..12].copy_from_slice(&id.to_le_bytes());
        r[14] = logical;
        r[15] = core;
        r[18] = class;
        r
    }

    #[test]
    fn cpu_set_records_parse_by_their_own_size() {
        let mut raw = record(256, 0, 0, 1);
        raw.extend(record(257, 1, 0, 1));
        // A longer record from a newer OS still parses; its tail is skipped.
        let mut long = record(270, 14, 9, 0);
        long[0..4].copy_from_slice(&40u32.to_le_bytes());
        long.extend([0xAA; 8]);
        raw.extend(long);
        raw.extend([1, 2, 3]); // a torn tail is ignored
        let cpus = parse_cpu_sets(&raw);
        assert_eq!(cpus.len(), 3);
        assert_eq!(
            cpus[2],
            Cpu {
                id: 270,
                logical: 14,
                core: 9,
                efficiency_class: 0
            }
        );
    }

    #[test]
    fn load_is_a_share_of_the_machine() {
        let a = Times {
            machine_busy: 100,
            machine_total: 1000,
            process: 10,
        };
        let b = Times {
            machine_busy: 600,
            machine_total: 2000,
            process: 110,
        };
        assert_eq!(
            load(&a, &b),
            Load {
                machine: 0.5,
                process: 0.1
            }
        );
        assert_eq!(load(&b, &b).machine, 0.0);
    }

    #[test]
    #[cfg(windows)]
    fn this_machine_reports_its_processors_and_times() {
        let cpus = topology().unwrap();
        assert_eq!(
            cpus.len(),
            std::thread::available_parallelism().unwrap().get()
        );
        let t0 = times().unwrap();
        let spin = std::time::Instant::now();
        while spin.elapsed() < std::time::Duration::from_millis(50) {}
        let l = load(&t0, &times().unwrap());
        assert!(l.process > 0.0 && l.machine >= l.process * 0.5, "{l:?}");
        // Placing on one CPU and back succeeds.
        place_current_thread(&[cpus[1].id]).unwrap();
        place_current_thread(&[]).unwrap();
    }
}
