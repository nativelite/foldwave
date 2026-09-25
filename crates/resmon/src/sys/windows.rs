//! Windows process sampling, through our own extern blocks.
//!
//! Every symbol here lives in `kernel32`, which is already linked by std, so
//! this adds no library and no crate. `K32GetProcessMemoryInfo` is used in
//! place of the `psapi` export of the same function precisely so that stays
//! true.

use crate::delta::{Snapshot, SystemInfo};
use crate::gpu;
use crate::proc::RawProc;
use crate::volume::Volume;
use std::io;
use std::path::{Path, PathBuf};

type Handle = *mut std::ffi::c_void;
type Bool = i32;

const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_VM_READ: u32 = 0x0010;
const MAX_PATH: usize = 260;

/// 100-nanosecond units, the resolution of every Win32 time value here.
const FILETIME_NS: u64 = 100;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    fn ticks(self) -> u64 {
        ((self.high as u64) << 32) | self.low as u64
    }
    fn nanos(self) -> u64 {
        self.ticks().saturating_mul(FILETIME_NS)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; MAX_PATH],
}

impl Default for ProcessEntry32W {
    fn default() -> Self {
        ProcessEntry32W {
            dw_size: std::mem::size_of::<ProcessEntry32W>() as u32,
            cnt_usage: 0,
            th32_process_id: 0,
            th32_default_heap_id: 0,
            th32_module_id: 0,
            cnt_threads: 0,
            th32_parent_process_id: 0,
            pc_pri_class_base: 0,
            dw_flags: 0,
            sz_exe_file: [0; MAX_PATH],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessMemoryCountersEx {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
    private_usage: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
    fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
    fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> Bool;
    fn CloseHandle(handle: Handle) -> Bool;
    fn OpenProcess(access: u32, inherit: Bool, pid: u32) -> Handle;
    fn GetProcessTimes(
        handle: Handle,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> Bool;
    fn GetProcessIoCounters(handle: Handle, counters: *mut IoCounters) -> Bool;
    fn K32GetProcessMemoryInfo(
        handle: Handle,
        counters: *mut ProcessMemoryCountersEx,
        cb: u32,
    ) -> Bool;
    fn GlobalMemoryStatusEx(status: *mut MemoryStatusEx) -> Bool;
    fn GetVolumePathNameW(path: *const u16, root: *mut u16, len: u32) -> Bool;
    fn GetDiskFreeSpaceExW(
        root: *const u16,
        free_to_caller: *mut u64,
        total: *mut u64,
        total_free: *mut u64,
    ) -> Bool;
    fn GetLogicalDrives() -> u32;
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetSystemCpuSetInformation(
        info: *mut u8,
        len: u32,
        returned: *mut u32,
        process: Handle,
        flags: u32,
    ) -> Bool;
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> Bool;
    fn GetCurrentProcess() -> Handle;
    fn GetCurrentThread() -> Handle;
    fn SetThreadSelectedCpuSets(thread: Handle, ids: *const u32, count: u32) -> Bool;
    fn CreateJobObjectW(attributes: *mut std::ffi::c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(
        job: Handle,
        class: u32,
        info: *const std::ffi::c_void,
        len: u32,
    ) -> Bool;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> Bool;
}

/// `JOBOBJECT_BASIC_LIMIT_INFORMATION`.
#[repr(C)]
#[derive(Default)]
struct JobBasicLimits {
    per_process_user_time: i64,
    per_job_user_time: i64,
    limit_flags: u32,
    min_working_set: usize,
    max_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

/// `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
#[repr(C)]
#[derive(Default)]
struct JobExtendedLimits {
    basic: JobBasicLimits,
    io: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;

/// A job object handle, closed on drop (which ends its processes).
pub struct Job(OwnedHandle);

// SAFETY: a job object handle is a kernel handle, valid from any thread; the
// job is only assigned to and closed, both thread-safe calls.
unsafe impl Send for Job {}
// SAFETY: as above; `&Job` only allows `job_assign`.
unsafe impl Sync for Job {}

/// A new kill-on-close job.
pub fn job_new() -> io::Result<Job> {
    // SAFETY: no attributes and no name are both allowed.
    let h = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
    if h.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = Job(OwnedHandle(h));
    let mut info = JobExtendedLimits::default();
    info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `info` is the structure the class names, with its exact size.
    let ok = unsafe {
        SetInformationJobObject(
            job.0 .0,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            (&info as *const JobExtendedLimits).cast(),
            std::mem::size_of::<JobExtendedLimits>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// Put `child` in `job`.
pub fn job_assign(job: &Job, child: &std::process::Child) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    // SAFETY: both handles are live for the call.
    if unsafe { AssignProcessToJobObject(job.0 .0, child.as_raw_handle().cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The raw `SYSTEM_CPU_SET_INFORMATION` records for every logical processor.
pub fn cpu_sets() -> io::Result<Vec<u8>> {
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 asks only for the size needed.
    unsafe {
        GetSystemCpuSetInformation(std::ptr::null_mut(), 0, &mut len, std::ptr::null_mut(), 0)
    };
    let mut buf = vec![0u8; len as usize];
    // SAFETY: `buf` holds `len` bytes, as passed.
    if unsafe {
        GetSystemCpuSetInformation(buf.as_mut_ptr(), len, &mut len, std::ptr::null_mut(), 0)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(buf)
}

/// Machine-wide busy and total CPU time, and this process's.
pub fn cpu_times() -> io::Result<crate::cpu::Times> {
    let (mut idle, mut kernel, mut user) = Default::default();
    // SAFETY: three valid out-pointers.
    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut c, mut e, mut pk, mut pu) = Default::default();
    // SAFETY: the current-process pseudo-handle is always valid; valid
    // out-pointers.
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut pk, &mut pu) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _: (FileTime, FileTime) = (c, e);
    // Kernel time includes idle time.
    let total = kernel.nanos() + user.nanos();
    Ok(crate::cpu::Times {
        machine_busy: total.saturating_sub(idle.nanos()),
        machine_total: total,
        process: pk.nanos() + pu.nanos(),
    })
}

/// Restrict the calling thread to the CPU sets `ids`; empty clears it.
pub fn place_current_thread(ids: &[u32]) -> io::Result<()> {
    let ptr = if ids.is_empty() {
        std::ptr::null()
    } else {
        ids.as_ptr()
    };
    // SAFETY: the current-thread pseudo-handle is always valid; `ptr` points
    // at `ids.len()` ids, or is null with a count of 0, which clears.
    if unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), ptr, ids.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `GetDriveTypeW`: a fixed disk (not removable, network, optical or RAM).
const DRIVE_FIXED: u32 = 3;

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide(raw: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    PathBuf::from(std::ffi::OsString::from_wide(&raw[..len]))
}

/// The volume holding `path` (which need not exist yet), and its space.
pub fn volume(path: &Path) -> io::Result<Volume> {
    let mut root = [0u16; MAX_PATH + 1];
    let p = wide(path.as_os_str());
    // SAFETY: `p` is NUL-terminated and `root` holds the length we pass.
    if unsafe { GetVolumePathNameW(p.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut free, mut total, mut total_free) = (0u64, 0u64, 0u64);
    // SAFETY: `root` was NUL-terminated by the call above; the out-pointers
    // are valid locals.
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, &mut total_free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Volume {
        root: from_wide(&root),
        total,
        free,
    })
}

/// The roots of the machine's fixed drives (`C:\`, `E:\`, ...), in letter
/// order: where a log could live.
pub fn fixed_volumes() -> Vec<PathBuf> {
    // SAFETY: no arguments; returns a bitmask of drive letters.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i) as char)))
        .filter(|root| {
            let w = wide(root.as_os_str());
            // SAFETY: `w` is a NUL-terminated root path.
            let kind = unsafe { GetDriveTypeW(w.as_ptr()) };
            kind == DRIVE_FIXED
        })
        .collect()
}

/// An owned process handle, closed on drop.
struct OwnedHandle(Handle);

impl OwnedHandle {
    /// Open a process for querying, trying the widest access we need first and
    /// falling back to the narrower right, which some protected processes still
    /// grant.
    fn open(pid: u32) -> Option<OwnedHandle> {
        for access in [
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            PROCESS_QUERY_LIMITED_INFORMATION,
        ] {
            // SAFETY: a plain query call; a failure returns null, which we check.
            let h = unsafe { OpenProcess(access, 0, pid) };
            if !h.is_null() {
                return Some(OwnedHandle(h));
            }
        }
        None
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

/// A toolhelp snapshot handle, closed on drop.
struct Snap(Handle);

impl Drop for Snap {
    fn drop(&mut self) {
        // SAFETY: self.0 is a valid snapshot handle, closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

fn exe_name(raw: &[u16; MAX_PATH]) -> String {
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..len])
}

/// Fill in the counters that need an open handle. Leaves them zero and marks
/// the process denied when it cannot be opened, which is normal for protected
/// and higher-integrity processes.
fn enrich(p: &mut RawProc) {
    let Some(handle) = OwnedHandle::open(p.pid) else {
        p.denied = true;
        return;
    };

    let (mut creation, mut exit, mut kernel, mut user) = Default::default();
    // SAFETY: handle is live for this scope and every out-pointer is a valid,
    // initialized local of the right type.
    if unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) } != 0 {
        p.start_time = creation.ticks();
        p.cpu_kernel_ns = kernel.nanos();
        p.cpu_user_ns = user.nanos();
    }

    let mut io = IoCounters::default();
    // SAFETY: as above.
    if unsafe { GetProcessIoCounters(handle.0, &mut io) } != 0 {
        p.io_read_bytes = io.read_transfer_count;
        p.io_write_bytes = io.write_transfer_count;
        p.io_read_ops = io.read_operation_count;
        p.io_write_ops = io.write_operation_count;
    }

    let mut mem = ProcessMemoryCountersEx {
        cb: std::mem::size_of::<ProcessMemoryCountersEx>() as u32,
        ..Default::default()
    };
    // SAFETY: cb matches the struct we pass, as the API requires.
    if unsafe { K32GetProcessMemoryInfo(handle.0, &mut mem, mem.cb) } != 0 {
        p.working_set = mem.working_set_size as u64;
        p.private_bytes = mem.private_usage as u64;
    }
}

/// Every process the caller can see, with the counters it is allowed to read.
pub fn processes() -> io::Result<Vec<RawProc>> {
    // SAFETY: documented call; the invalid value is checked immediately.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let snap = Snap(raw);

    let mut entry = ProcessEntry32W::default();
    let mut out = Vec::new();
    // SAFETY: entry.dw_size is set by Default, as Process32FirstW requires.
    let mut ok = unsafe { Process32FirstW(snap.0, &mut entry) } != 0;
    while ok {
        let mut p = RawProc {
            pid: entry.th32_process_id,
            parent_pid: entry.th32_parent_process_id,
            name: exe_name(&entry.sz_exe_file),
            ..Default::default()
        };
        enrich(&mut p);
        out.push(p);
        // SAFETY: same snapshot and entry, iterated as documented.
        ok = unsafe { Process32NextW(snap.0, &mut entry) } != 0;
    }
    Ok(out)
}

/// Machine-wide facts.
pub fn system_info() -> io::Result<SystemInfo> {
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    // SAFETY: length is set as the API requires.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SystemInfo {
        logical_cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1),
        mem_total: status.total_phys,
        mem_available: status.avail_phys,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_combines_its_halves() {
        let ft = FileTime {
            low: 0x0000_0002,
            high: 0x0000_0001,
        };
        assert_eq!(ft.ticks(), (1u64 << 32) | 2);
    }

    #[test]
    fn filetime_converts_ticks_to_nanoseconds() {
        let ft = FileTime { low: 10, high: 0 };
        assert_eq!(ft.nanos(), 1_000);
    }

    #[test]
    fn filetime_nanos_saturate_instead_of_overflowing() {
        let ft = FileTime {
            low: u32::MAX,
            high: u32::MAX,
        };
        assert_eq!(ft.nanos(), u64::MAX);
    }

    #[test]
    fn exe_name_stops_at_the_nul() {
        let mut raw = [0u16; MAX_PATH];
        for (i, c) in "cargo.exe".encode_utf16().enumerate() {
            raw[i] = c;
        }
        assert_eq!(exe_name(&raw), "cargo.exe");
    }

    #[test]
    fn exe_name_of_an_empty_buffer_is_empty() {
        assert_eq!(exe_name(&[0u16; MAX_PATH]), "");
    }

    #[test]
    fn process_entry_declares_its_own_size() {
        let e = ProcessEntry32W::default();
        assert_eq!(e.dw_size as usize, std::mem::size_of::<ProcessEntry32W>());
    }

    #[test]
    fn enumerating_processes_finds_this_one() {
        let procs = processes().expect("snapshot the process list");
        let me = std::process::id();
        let found = procs
            .iter()
            .find(|p| p.pid == me)
            .expect("the test process itself must appear");
        assert!(!found.name.is_empty());
        assert!(!found.denied, "we can always open our own process");
        assert!(found.cpu_ns() > 0, "our own CPU time must be non-zero");
        assert!(
            found.working_set > 0,
            "our own working set must be non-zero"
        );
        assert!(found.start_time > 0, "our own start time must be known");
    }

    #[test]
    fn system_info_is_plausible() {
        let s = system_info().expect("read machine facts");
        assert!(s.logical_cpus >= 1);
        assert!(s.mem_total > 0);
        assert!(s.mem_available <= s.mem_total);
    }

    #[test]
    fn a_snapshot_has_processes_and_a_labelled_gpu_reading() {
        let s = snapshot().expect("take a snapshot");
        assert!(!s.procs.is_empty());
        assert!(
            s.gpu.is_some(),
            "the GPU field is always populated, even as none"
        );
    }
}
