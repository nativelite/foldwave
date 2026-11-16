//! Linux sampling: `/proc` and `/sys` read as files, and a handful of libc
//! calls through our own `extern` block (std already links libc, so nothing
//! is added).
//!
//! The founder chose Linux as the first Unix (`docs/design.md` section 11,
//! row 24). What each function reads is named beside it, with the kernel's
//! own description in `docs/compute.md` ("Linux").
//!
//! Built for 64-bit x86 and ARM, where the system-call numbers and the
//! `statvfs` layout used here are the same. Other targets keep the stub.

use crate::cpu::Times;
use crate::delta::{Snapshot, SystemInfo};
use crate::gpu;
use crate::proc::RawProc;
use crate::volume::Volume;
use std::ffi::{c_int, c_long, c_ulong, CString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// `struct timespec` on the targets this file is built for.
#[repr(C)]
#[derive(Default)]
struct Timespec {
    sec: c_long,
    nsec: c_long,
}

/// `struct statvfs` on 64-bit Linux, glibc and musl alike: eleven words and
/// six spare ints.
#[repr(C)]
#[derive(Default)]
struct StatVfs {
    bsize: c_ulong,
    frsize: c_ulong,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    favail: u64,
    fsid: c_ulong,
    flag: c_ulong,
    namemax: c_ulong,
    spare: [c_int; 6],
}

const _: () = assert!(std::mem::size_of::<StatVfs>() == 112);

const SC_CLK_TCK: c_int = 2;
const SC_PAGESIZE: c_int = 30;
const CLOCK_PROCESS_CPUTIME_ID: c_int = 2;
const SYS_PIDFD_SEND_SIGNAL: c_long = 424;
const SYS_PIDFD_OPEN: c_long = 434;
const SIGKILL: c_int = 9;

extern "C" {
    fn sysconf(name: c_int) -> c_long;
    fn clock_gettime(clock: c_int, out: *mut Timespec) -> c_int;
    fn statvfs(path: *const std::ffi::c_char, out: *mut StatVfs) -> c_int;
    fn sched_setaffinity(pid: c_int, size: usize, mask: *const u64) -> c_int;
    fn syscall(number: c_long, ...) -> c_long;
    fn getpgid(pid: c_int) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
}

/// Nanoseconds in one clock tick (`USER_HZ`, 100 a second everywhere today).
fn tick_ns() -> u64 {
    // SAFETY: sysconf takes any name and returns -1 for one it does not know.
    let hz = unsafe { sysconf(SC_CLK_TCK) };
    1_000_000_000 / if hz > 0 { hz as u64 } else { 100 }
}

fn page_size() -> u64 {
    // SAFETY: as above.
    let n = unsafe { sysconf(SC_PAGESIZE) };
    if n > 0 {
        n as u64
    } else {
        4096
    }
}

/// The fields of `/proc/<pid>/stat` this crate uses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub pgrp: i32,
    pub utime: u64,
    pub stime: u64,
    pub start_time: u64,
}

/// Parse one `/proc/<pid>/stat` line. The name sits in parentheses and may
/// itself hold spaces and parentheses, so the fields are counted from the
/// last `)`.
pub(crate) fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let comm = line.get(open + 1..close)?.to_string();
    // After the name: state is field 3, so index 0 here.
    let f: Vec<&str> = line.get(close + 1..)?.split_ascii_whitespace().collect();
    Some(Stat {
        comm,
        state: f.first()?.chars().next()?,
        ppid: f.get(1)?.parse().ok()?,
        pgrp: f.get(2)?.parse().ok()?,
        utime: f.get(11)?.parse().ok()?,
        stime: f.get(12)?.parse().ok()?,
        start_time: f.get(19)?.parse().ok()?,
    })
}

/// `/proc/<pid>/statm`: resident and shared pages, in that order after the
/// total size.
pub(crate) fn parse_statm(line: &str) -> Option<(u64, u64)> {
    let mut f = line.split_ascii_whitespace().skip(1);
    Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
}

/// `/proc/<pid>/io`: bytes and calls through `read` and `write`, of any
/// kind, as [`RawProc`] documents (`rchar`, `wchar`, `syscr`, `syscw`).
pub(crate) fn parse_io(text: &str) -> (u64, u64, u64, u64) {
    let mut out = (0, 0, 0, 0);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().parse().unwrap_or(0);
        match key {
            "rchar" => out.0 = value,
            "wchar" => out.1 = value,
            "syscr" => out.2 = value,
            "syscw" => out.3 = value,
            _ => {}
        }
    }
    out
}

fn pids() -> io::Result<Vec<u32>> {
    let mut out = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        if let Some(pid) = entry.file_name().to_str().and_then(|s| s.parse().ok()) {
            out.push(pid);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn read_stat(pid: u32) -> Option<Stat> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// The program's file name: the executable's when it may be read, else the
/// first word of the command line, else the kernel's name for the task,
/// which is cut at 15 characters.
fn exe_name(pid: u32, comm: &str) -> String {
    let base = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    if let Some(name) = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|p| base(&p))
    {
        // A replaced binary reads as "name (deleted)".
        return name.trim_end_matches(" (deleted)").to_string();
    }
    if let Ok(raw) = fs::read(format!("/proc/{pid}/cmdline")) {
        let first = raw.split(|b| *b == 0).next().unwrap_or(&[]);
        if let Some(name) = base(Path::new(std::ffi::OsStr::from_bytes(first))) {
            if !name.is_empty() {
                return name;
            }
        }
    }
    comm.to_string()
}

/// Every process's id, parent and name, and nothing more: one small read
/// each. The name is the kernel's (15 characters at most).
pub fn process_links() -> io::Result<Vec<RawProc>> {
    Ok(pids()?
        .into_iter()
        .filter_map(|pid| {
            let s = read_stat(pid)?;
            Some(RawProc {
                pid,
                parent_pid: s.ppid,
                name: s.comm,
                ..Default::default()
            })
        })
        .collect())
}

/// Every process, with the counters this user may read. Another user's
/// process gives its CPU time and memory but not its I/O, and is marked
/// `denied`.
pub fn processes() -> io::Result<Vec<RawProc>> {
    let (tick, page) = (tick_ns(), page_size());
    let mut out = Vec::new();
    for pid in pids()? {
        // A process may exit between the listing and the read.
        let Some(s) = read_stat(pid) else { continue };
        let mut p = RawProc {
            pid,
            parent_pid: s.ppid,
            name: exe_name(pid, &s.comm),
            start_time: s.start_time,
            cpu_kernel_ns: s.stime * tick,
            cpu_user_ns: s.utime * tick,
            ..Default::default()
        };
        if let Some((resident, shared)) = fs::read_to_string(format!("/proc/{pid}/statm"))
            .ok()
            .and_then(|t| parse_statm(&t))
        {
            p.working_set = resident * page;
            p.private_bytes = resident.saturating_sub(shared) * page;
        }
        match fs::read_to_string(format!("/proc/{pid}/io")) {
            Ok(text) => {
                (
                    p.io_read_bytes,
                    p.io_write_bytes,
                    p.io_read_ops,
                    p.io_write_ops,
                ) = parse_io(&text);
            }
            Err(_) => p.denied = true,
        }
        out.push(p);
    }
    Ok(out)
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in bytes.
pub(crate) fn parse_meminfo(text: &str) -> (u64, u64) {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|v| v.split_ascii_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (field("MemTotal"), field("MemAvailable"))
}

/// Machine-wide facts.
pub fn system_info() -> io::Result<SystemInfo> {
    let (mem_total, mem_available) = parse_meminfo(&fs::read_to_string("/proc/meminfo")?);
    Ok(SystemInfo {
        logical_cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        mem_total,
        mem_available,
    })
}

/// One complete reading: processes, machine facts and the GPU.
pub fn snapshot() -> io::Result<Snapshot> {
    Ok(Snapshot {
        procs: processes()?,
        system: system_info()?,
        gpu: Some(gpu::sample()),
    })
}

/// The first line of `/proc/stat`, as (busy, total) ticks summed over every
/// CPU. Idle and waiting on I/O are the idle part; guest time is already
/// inside user time.
pub(crate) fn parse_cpu_line(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let f: Vec<u64> = line
        .split_ascii_whitespace()
        .skip(1)
        .take(8)
        .filter_map(|v| v.parse().ok())
        .collect();
    if f.len() < 4 {
        return None;
    }
    let total: u64 = f.iter().sum();
    let idle = f[3] + f.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// Machine-wide busy and total CPU time, and this process's.
pub fn cpu_times() -> io::Result<Times> {
    let (busy, total) = parse_cpu_line(&fs::read_to_string("/proc/stat")?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "/proc/stat has no cpu line"))?;
    let mut ts = Timespec::default();
    // SAFETY: a valid out-pointer to a timespec of this target's layout.
    if unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &mut ts) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let tick = tick_ns();
    Ok(Times {
        machine_busy: busy * tick,
        machine_total: total * tick,
        process: ts.sec as u64 * 1_000_000_000 + ts.nsec as u64,
    })
}

/// A list such as `0-3,8,10-11`, as the kernel writes CPU lists.
pub(crate) fn parse_cpu_list(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in text.trim().split(',') {
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        if let (Ok(lo), Ok(hi)) = (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
            out.extend(lo..=hi);
        }
    }
    out
}

/// Each CPU's class from its capacity: the slowest kind is 0, the next 1,
/// and a machine of one kind is all 0, as [`crate::cpu::Cpu`] documents.
pub(crate) fn classes(capacity: &[u64]) -> Vec<u8> {
    let mut kinds: Vec<u64> = capacity.to_vec();
    kinds.sort_unstable();
    kinds.dedup();
    capacity
        .iter()
        .map(|c| kinds.iter().position(|k| k == c).unwrap_or(0).min(255) as u8)
        .collect()
}

fn read_u64(path: String) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Every online CPU, written as the records [`crate::cpu::parse_cpu_sets`]
/// reads, so [`crate::cpu::topology`] is one function on every platform.
///
/// A CPU's kind comes from `cpu_capacity` where the kernel gives one, else
/// from the hybrid lists (`cpu_core` are the fast cores, `cpu_atom` the
/// efficient ones), else every CPU is the same kind.
pub fn cpu_sets() -> io::Result<Vec<u8>> {
    let cpus = parse_cpu_list(&fs::read_to_string("/sys/devices/system/cpu/online")?);
    let list = |pmu: &str| {
        fs::read_to_string(format!("/sys/bus/event_source/devices/{pmu}/cpus"))
            .map(|t| parse_cpu_list(&t))
            .unwrap_or_default()
    };
    let (fast, efficient) = (list("cpu_core"), list("cpu_atom"));
    let capacity: Vec<u64> = cpus
        .iter()
        .map(|&id| {
            read_u64(format!("/sys/devices/system/cpu/cpu{id}/cpu_capacity")).unwrap_or_else(|| {
                match (fast.contains(&id), efficient.contains(&id)) {
                    (true, _) => 2,
                    (_, true) => 1,
                    _ => 0,
                }
            })
        })
        .collect();
    let class = classes(&capacity);
    let mut raw = Vec::with_capacity(cpus.len() * 32);
    for (i, &id) in cpus.iter().enumerate() {
        let core =
            read_u64(format!("/sys/devices/system/cpu/cpu{id}/topology/core_id")).unwrap_or(0);
        let mut r = [0u8; 32];
        r[0..4].copy_from_slice(&32u32.to_le_bytes());
        r[8..12].copy_from_slice(&id.to_le_bytes());
        r[14] = i.min(255) as u8;
        r[15] = core.min(255) as u8;
        r[18] = class[i];
        raw.extend(r);
    }
    Ok(raw)
}

/// Let the calling thread run only on the CPUs `ids`; empty lets it run on
/// every online CPU again.
pub fn place_current_thread(ids: &[u32]) -> io::Result<()> {
    let all;
    let ids = if ids.is_empty() {
        all = parse_cpu_list(&fs::read_to_string("/sys/devices/system/cpu/online")?);
        &all[..]
    } else {
        ids
    };
    let words = ids.iter().max().map_or(1, |m| *m as usize / 64 + 1);
    let mut mask = vec![0u64; words];
    for &id in ids {
        mask[id as usize / 64] |= 1 << (id % 64);
    }
    // SAFETY: pid 0 is the calling thread; `mask` holds the bytes passed.
    if unsafe { sched_setaffinity(0, words * 8, mask.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One line of `/proc/self/mountinfo`: the mount point, the filesystem type,
/// the source and whether it is mounted read-only.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Mount {
    pub point: PathBuf,
    pub fs_type: String,
    pub source: String,
    pub read_only: bool,
}

/// The kernel writes a space, tab, newline and backslash in a path as a
/// backslash and three octal digits.
fn unescape(field: &str) -> PathBuf {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|_| bytes[i] == b'\\')
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u8::from_str_radix(d, 8).ok());
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(&out))
}

pub(crate) fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            // "36 35 98:0 /root /mnt rw,noatime shared:1 - ext4 /dev/sda1 rw"
            let (left, right) = line.split_once(" - ")?;
            let l: Vec<&str> = left.split(' ').collect();
            let mut r = right.split(' ');
            Some(Mount {
                point: unescape(l.get(4)?),
                read_only: l.get(5)?.split(',').any(|o| o == "ro"),
                fs_type: r.next()?.to_string(),
                source: r.next()?.to_string(),
            })
        })
        .collect()
}

fn mounts() -> io::Result<Vec<Mount>> {
    fs::read_to_string("/proc/self/mountinfo").map(|t| parse_mountinfo(&t))
}

/// The mount `path` is on: the one with the longest mount point that is a
/// prefix of it, the last listed when two are mounted on the same point.
pub(crate) fn mount_of<'a>(mounts: &'a [Mount], path: &Path) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.point))
        .max_by_key(|m| m.point.as_os_str().len())
}

/// The volume holding `path`, which need not exist yet: its mount point and
/// the space an unprivileged writer may still use.
pub fn volume(path: &Path) -> io::Result<Volume> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    // The nearest part of the path that exists decides the volume.
    let existing = absolute
        .ancestors()
        .find_map(|a| a.canonicalize().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no part of the path exists"))?;
    let c_path = CString::new(existing.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL"))?;
    let mut st = StatVfs::default();
    // SAFETY: a NUL-terminated path and an out-pointer of statvfs's size
    // (asserted above).
    if unsafe { statvfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mounts = mounts()?;
    let root = mount_of(&mounts, &existing)
        .map(|m| m.point.clone())
        .unwrap_or_else(|| PathBuf::from("/"));
    Ok(Volume {
        root,
        total: st.blocks * st.frsize,
        free: st.bavail * st.frsize,
    })
}

/// Whether the block device numbered `rdev` is on a disk that cannot be
/// removed. A partition's entry in `/sys/dev/block` has no `removable` file
/// of its own; its disk, one level up, has.
fn on_fixed_disk(rdev: u64) -> bool {
    // glibc's makedev, undone.
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    let dir = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
    let read = |p: PathBuf| fs::read_to_string(p).ok().map(|t| t.trim() == "0");
    read(dir.join("removable"))
        .or_else(|| read(dir.join("../removable")))
        .unwrap_or(false)
}

/// The mount points of the machine's fixed, writable volumes, one for each
/// device (its shortest mount point), in mount order. A volume is one whose
/// source is a block device on a disk that cannot be removed, so network,
/// memory and other virtual filesystems are left out.
pub fn fixed_volumes() -> Vec<PathBuf> {
    let Ok(mounts) = mounts() else {
        return Vec::new();
    };
    let mut seen: Vec<(u64, PathBuf)> = Vec::new();
    for m in mounts.iter().filter(|m| !m.read_only) {
        let Ok(meta) = fs::metadata(&m.source) else {
            continue;
        };
        let rdev = meta.rdev();
        if !m.source.starts_with("/dev/") || rdev == 0 || !on_fixed_disk(rdev) {
            continue;
        }
        match seen.iter_mut().find(|(dev, _)| *dev == rdev) {
            Some((_, point)) => {
                if m.point.as_os_str().len() < point.as_os_str().len() {
                    *point = m.point.clone();
                }
            }
            None => seen.push((rdev, m.point.clone())),
        }
    }
    seen.into_iter().map(|(_, point)| point).collect()
}

/// Another process's CPU time. The process is named by its id together with
/// its start time, which no later process with the same id can share.
pub struct ProcClock {
    pid: u32,
    start_time: u64,
    last: AtomicU64,
}

/// A clock for `child`, which is still this process's to query.
pub fn proc_clock(child: &std::process::Child) -> io::Result<ProcClock> {
    let stat = read_stat(child.id())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the child has been reaped"))?;
    Ok(ProcClock {
        pid: child.id(),
        start_time: stat.start_time,
        last: AtomicU64::new(0),
    })
}

/// Kernel plus user nanoseconds so far, and whether the process has exited.
///
/// A process that has exited but not been waited for still reports its
/// final time. Once it has been waited for the kernel keeps nothing, so the
/// time is the last one read here: final to within one reading, where
/// Windows' is exact.
pub fn proc_clock_read(clock: &ProcClock) -> io::Result<(u64, bool)> {
    match read_stat(clock.pid).filter(|s| s.start_time == clock.start_time) {
        Some(s) => {
            let ns = (s.utime + s.stime) * tick_ns();
            clock.last.fetch_max(ns, Ordering::Relaxed);
            Ok((ns, s.state == 'Z' || s.state == 'X'))
        }
        None => Ok((clock.last.load(Ordering::Relaxed), true)),
    }
}

/// A process held by a descriptor (a pidfd), so a signal can never reach a
/// later process that was given the same id.
struct Member {
    pidfd: c_int,
    pid: c_int,
}

/// The processes to end when the job is dropped.
///
/// Linux has no object that ends its processes when the last handle to it
/// closes. This one ends them when it is dropped: each process it was given,
/// and that process's whole group when it leads one (a program started with
/// `process_group(0)`, as a pane's is). **It does not cover the holder
/// being killed**, where Windows' job does. That needs the program started
/// inside a cgroup or with a parent-death signal, both set when it is
/// spawned, not after (`docs/compute.md`, "A job per pane").
pub struct Job(Mutex<Vec<Member>>);

/// A new, empty job.
pub fn job_new() -> io::Result<Job> {
    Ok(Job(Mutex::new(Vec::new())))
}

/// Put `child` in `job`. Needs Linux 5.3 for the pidfd.
pub fn job_assign(job: &Job, child: &std::process::Child) -> io::Result<()> {
    let pid = child.id() as c_int;
    // SAFETY: a system call with two integer arguments; the child has not
    // been waited for, so the id still names it.
    let pidfd = unsafe { syscall(SYS_PIDFD_OPEN, pid, 0 as c_int) };
    if pidfd < 0 {
        return Err(io::Error::last_os_error());
    }
    job.0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Member {
            pidfd: pidfd as c_int,
            pid,
        });
    Ok(())
}

impl Drop for Job {
    fn drop(&mut self) {
        let members = self.0.get_mut().unwrap_or_else(|e| e.into_inner());
        for m in members.drain(..) {
            // SAFETY: integer arguments and null pointers the call accepts.
            // Signal 0 only asks whether the descriptor's process is still
            // there, which also proves the id has not been given away, so
            // the group signal below cannot reach a stranger.
            unsafe {
                let null = std::ptr::null::<u8>();
                if syscall(SYS_PIDFD_SEND_SIGNAL, m.pidfd, 0 as c_int, null, 0 as c_int) == 0 {
                    if getpgid(m.pid) == m.pid {
                        kill(-m.pid, SIGKILL);
                    }
                    syscall(SYS_PIDFD_SEND_SIGNAL, m.pidfd, SIGKILL, null, 0 as c_int);
                }
                close(m.pidfd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_stat_line_parses_whatever_the_name_holds() {
        let line = "4242 (tmux: server (1)) S 1 4242 4242 0 -1 4194560 715 0 0 0 \
                    12 34 0 0 20 0 1 0 987654 12345678 321 18446744073709551615";
        assert_eq!(
            parse_stat(line),
            Some(Stat {
                comm: "tmux: server (1)".into(),
                state: 'S',
                ppid: 1,
                pgrp: 4242,
                utime: 12,
                stime: 34,
                start_time: 987654,
            })
        );
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn memory_io_and_meminfo_parse() {
        assert_eq!(parse_statm("5000 1200 300 10 0 900 0\n"), Some((1200, 300)));
        assert_eq!(
            parse_io("rchar: 10\nwchar: 20\nsyscr: 3\nsyscw: 4\nread_bytes: 99\n"),
            (10, 20, 3, 4)
        );
        assert_eq!(
            parse_meminfo("MemTotal:       16000 kB\nMemFree: 1 kB\nMemAvailable:    8000 kB\n"),
            (16000 * 1024, 8000 * 1024)
        );
    }

    #[test]
    fn the_cpu_line_splits_busy_from_idle_and_io_wait() {
        // user nice system idle iowait irq softirq steal guest guest_nice
        let text = "cpu  100 5 50 800 40 3 2 0 7 0\ncpu0 1 1 1 1 1 1 1 1 1 1\n";
        assert_eq!(parse_cpu_line(text), Some((160, 1000)));
    }

    #[test]
    fn cpu_lists_and_classes() {
        assert_eq!(parse_cpu_list("0-2,8,10-11\n"), [0, 1, 2, 8, 10, 11]);
        assert_eq!(parse_cpu_list(""), [] as [u32; 0]);
        // A hybrid part: fast cores are 1, efficient ones 0.
        assert_eq!(classes(&[1024, 1024, 446, 446]), [1, 1, 0, 0]);
        // One kind of core: all 0.
        assert_eq!(classes(&[1024, 1024]), [0, 0]);
    }

    #[test]
    fn mountinfo_gives_points_types_and_sources() {
        let text = "36 35 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw\n\
                    40 36 0:35 / /mnt/my\\040disk ro,nosuid - tmpfs tmpfs rw\n\
                    41 36 259:3 / /home rw - btrfs /dev/nvme0n1p3 rw,ssd\n";
        let m = parse_mountinfo(text);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].point, PathBuf::from("/mnt/my disk"));
        assert!(m[1].read_only && m[1].fs_type == "tmpfs");
        assert_eq!(m[2].source, "/dev/nvme0n1p3");
        assert_eq!(
            mount_of(&m, Path::new("/home/me/logs")).unwrap().point,
            PathBuf::from("/home")
        );
        assert_eq!(
            mount_of(&m, Path::new("/homestead")).unwrap().point,
            PathBuf::from("/")
        );
    }

    #[test]
    fn this_process_is_listed_with_its_parent_and_counters() {
        let me = std::process::id();
        let links = process_links().unwrap();
        let link = links.iter().find(|p| p.pid == me).expect("in the links");
        assert!(links.iter().any(|p| p.pid == link.parent_pid));

        let all = processes().unwrap();
        let p = all.iter().find(|p| p.pid == me).expect("in the list");
        assert_eq!(p.parent_pid, link.parent_pid);
        assert!(p.name.starts_with("resmon"), "name was {:?}", p.name);
        assert!(p.working_set > 0 && p.private_bytes <= p.working_set);
        assert!(!p.denied && p.io_read_ops > 0, "{p:?}");
        assert!(p.start_time > 0);
    }

    #[test]
    fn the_machine_reports_its_memory() {
        let s = system_info().unwrap();
        assert!(s.mem_total > 0 && s.mem_available <= s.mem_total, "{s:?}");
    }

    #[test]
    fn a_job_ends_its_process_and_that_process_group_when_dropped() {
        use std::os::unix::process::CommandExt;
        // A shell leading its own group, with a child of its own in it.
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id() as i32;
        let job = job_new().unwrap();
        job_assign(&job, &child).unwrap();
        // Wait for the shell to start its sleep.
        let in_group = || {
            pids()
                .unwrap()
                .into_iter()
                .filter_map(read_stat)
                .filter(|s| s.pgrp == group && s.state != 'Z')
                .count()
        };
        let until = Instant::now() + Duration::from_secs(5);
        while in_group() < 2 {
            assert!(Instant::now() < until, "the shell never started sleep");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_none(), "running in the job");
        drop(job);
        let until = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() || in_group() > 0 {
            assert!(Instant::now() < until, "the job's drop did not end them");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
