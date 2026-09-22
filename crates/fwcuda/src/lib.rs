//! A minimal, runtime-loaded binding to the NVIDIA CUDA Driver API.
//!
//! Built for foldwave's M0 harness: it answers *how fast do bytes actually
//! move between host and GPU on this machine?* with the same per-sample
//! timing discipline as the rest of `fwbench`.
//!
//! `nvcuda.dll` ships with the NVIDIA display driver. It is loaded at run time
//! from System32; nothing NVIDIA is linked against or redistributed, and a
//! machine without the driver gets an [`Error`], never a panic or a failed
//! link. Zero third-party dependencies.
//!
//! Only Windows x64 is implemented. Elsewhere [`Cuda::load`] returns
//! [`Error::Unsupported`].
//!
//! # Example
//!
//! ```no_run
//! # fn main() -> Result<(), fwcuda::Error> {
//! let cuda = fwcuda::Cuda::load()?;
//! let dev = cuda.device(0)?;
//! println!("{} ({} bytes)", dev.name(), dev.total_mem());
//!
//! let ctx = fwcuda::Context::new(&cuda, &dev)?;
//! let stats = fwcuda::copy_bench(&ctx, 64 << 20, 20, true)?;
//! println!("{} H2D samples, {} D2H samples", stats.h2d.len(), stats.d2h.len());
//! # Ok(())
//! # }
//! ```
//!
//! # Threads
//!
//! A CUDA context is bound to the thread that makes it current. [`Context`]
//! and the buffers made from it hold raw pointers and are therefore neither
//! `Send` nor `Sync`: use them on the thread that created them.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::ffi::c_void;
use std::fmt;
use std::time::{Duration, Instant};

mod sys;

use sys::{Api, CuContext, CuDevice, CuDevicePtr, CuResult, LoadError};

/// Everything that can go wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// This platform has no loader (only Windows x64 is implemented).
    Unsupported,
    /// `nvcuda.dll` could not be loaded (no NVIDIA driver). Win32 error code.
    LibraryNotFound(u32),
    /// `nvcuda.dll` loaded but lacks a required export (driver too old).
    MissingSymbol(&'static str),
    /// A driver call returned a non-success `CUresult`.
    Driver {
        /// The Driver API function that failed, e.g. `cuMemAlloc_v2`.
        call: &'static str,
        /// The raw `CUresult`.
        code: i32,
        /// The driver's name for it (`cuGetErrorName`), when available.
        name: Option<String>,
    },
    /// A host slice and a device buffer differ in length.
    SizeMismatch {
        /// Length of the device buffer.
        device: usize,
        /// Length of the host slice.
        host: usize,
    },
    /// An argument this crate rejects before calling the driver.
    InvalidArgument(&'static str),
    /// The round-trip check read back different bytes from those sent.
    Corrupted {
        /// First differing byte offset.
        offset: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str("CUDA is not supported on this platform"),
            Error::LibraryNotFound(code) => {
                write!(f, "nvcuda.dll could not be loaded (Win32 error {code}); is the NVIDIA driver installed?")
            }
            Error::MissingSymbol(sym) => {
                write!(
                    f,
                    "nvcuda.dll has no export {sym}; the driver may be too old"
                )
            }
            Error::Driver { call, code, name } => match name {
                Some(n) => write!(f, "{call} failed: {n} ({code})"),
                None => write!(f, "{call} failed: CUresult {code}"),
            },
            Error::SizeMismatch { device, host } => write!(
                f,
                "size mismatch: device buffer is {device} bytes, host slice is {host} bytes"
            ),
            Error::InvalidArgument(why) => write!(f, "invalid argument: {why}"),
            Error::Corrupted { offset } => {
                write!(
                    f,
                    "round trip corrupted data: first difference at byte {offset}"
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<LoadError> for Error {
    fn from(e: LoadError) -> Error {
        match e {
            LoadError::Unsupported => Error::Unsupported,
            LoadError::Library(code) => Error::LibraryNotFound(code),
            LoadError::Symbol(s) => Error::MissingSymbol(s),
        }
    }
}

/// Require a host slice and a device buffer to be the same length.
fn check_len(device: usize, host: usize) -> Result<(), Error> {
    if device == host {
        Ok(())
    } else {
        Err(Error::SizeMismatch { device, host })
    }
}

/// Reject zero-byte allocations, which the driver refuses anyway.
fn check_nonzero(bytes: usize) -> Result<(), Error> {
    if bytes == 0 {
        Err(Error::InvalidArgument("allocation size must be non-zero"))
    } else {
        Ok(())
    }
}

/// The loaded driver library.
///
/// `cuInit` has been called successfully. The DLL is never unloaded (see
/// `sys::windows::load`), so dropping this frees nothing.
pub struct Cuda {
    api: Api,
}

impl fmt::Debug for Cuda {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cuda { .. }")
    }
}

impl Cuda {
    /// Load `nvcuda.dll`, resolve the entry points, and call `cuInit(0)`.
    pub fn load() -> Result<Cuda, Error> {
        let cuda = Cuda { api: sys::load()? };
        // SAFETY: cuInit takes flags by value; 0 is the only documented value.
        let r = unsafe { (cuda.api.init)(0) };
        cuda.check("cuInit", r)?;
        Ok(cuda)
    }

    fn check(&self, call: &'static str, code: CuResult) -> Result<(), Error> {
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Driver {
                call,
                code,
                name: sys::error_name(&self.api, code),
            })
        }
    }

    /// Number of CUDA-capable devices.
    pub fn device_count(&self) -> Result<usize, Error> {
        let mut n: i32 = 0;
        // SAFETY: `n` is a valid out-pointer for the duration of the call.
        let r = unsafe { (self.api.device_get_count)(&mut n) };
        self.check("cuDeviceGetCount", r)?;
        Ok(n.max(0) as usize)
    }

    /// Device `ordinal`, with its name and total memory read up front.
    pub fn device(&self, ordinal: usize) -> Result<Device, Error> {
        let ordinal_i32 = i32::try_from(ordinal)
            .map_err(|_| Error::InvalidArgument("device ordinal out of range"))?;
        let mut raw: CuDevice = 0;
        // SAFETY: `raw` is a valid out-pointer; the driver range-checks ordinal.
        let r = unsafe { (self.api.device_get)(&mut raw, ordinal_i32) };
        self.check("cuDeviceGet", r)?;

        let mut buf = [0u8; 256];
        // SAFETY: `buf` is writable for the length passed; the driver writes a
        // NUL-terminated string of at most that many bytes.
        let r = unsafe { (self.api.device_get_name)(buf.as_mut_ptr(), buf.len() as i32, raw) };
        self.check("cuDeviceGetName", r)?;
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).into_owned();

        let mut total: usize = 0;
        // SAFETY: `total` is a valid out-pointer for the duration of the call.
        let r = unsafe { (self.api.device_total_mem)(&mut total, raw) };
        self.check("cuDeviceTotalMem_v2", r)?;

        Ok(Device {
            raw,
            ordinal,
            name,
            total_mem: total as u64,
        })
    }
}

/// A CUDA device, as found by [`Cuda::device`].
#[derive(Debug, Clone)]
pub struct Device {
    raw: CuDevice,
    ordinal: usize,
    name: String,
    total_mem: u64,
}

impl Device {
    /// The ordinal it was opened with.
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }
    /// The driver's name for it, e.g. `NVIDIA GeForce RTX 3070`.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Total device memory in bytes.
    pub fn total_mem(&self) -> u64 {
        self.total_mem
    }
}

/// A CUDA context on one device. Destroyed on drop.
///
/// Every operation makes this context current on the calling thread first,
/// so several contexts on one thread do not interfere.
pub struct Context<'a> {
    cuda: &'a Cuda,
    raw: CuContext,
}

impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Context({:p})", self.raw)
    }
}

impl<'a> Context<'a> {
    /// Create a context with default flags (`CU_CTX_SCHED_AUTO`).
    pub fn new(cuda: &'a Cuda, device: &Device) -> Result<Context<'a>, Error> {
        let mut raw: CuContext = std::ptr::null_mut();
        // SAFETY: `raw` is a valid out-pointer; flags 0 is documented as the
        // default; `device.raw` came from cuDeviceGet.
        let r = unsafe { (cuda.api.ctx_create)(&mut raw, 0, device.raw) };
        cuda.check("cuCtxCreate_v2", r)?;
        Ok(Context { cuda, raw })
    }

    fn api(&self) -> &Api {
        &self.cuda.api
    }

    fn make_current(&self) -> Result<(), Error> {
        // SAFETY: `self.raw` is a live context created by cuCtxCreate_v2.
        let r = unsafe { (self.api().ctx_set_current)(self.raw) };
        self.cuda.check("cuCtxSetCurrent", r)
    }

    /// Block until all work in this context has finished.
    pub fn synchronize(&self) -> Result<(), Error> {
        self.make_current()?;
        self.sync_current()
    }

    fn sync_current(&self) -> Result<(), Error> {
        // SAFETY: no arguments; acts on the current context, set by the caller.
        let r = unsafe { (self.api().ctx_synchronize)() };
        self.cuda.check("cuCtxSynchronize", r)
    }

    /// Copy `src` into `dst`. Synchronous; lengths must match exactly.
    pub fn copy_h2d(&self, dst: &DeviceBuffer<'_>, src: &[u8]) -> Result<(), Error> {
        self.check_owner(dst.owner.raw)?;
        check_len(dst.len, src.len())?;
        self.make_current()?;
        self.h2d_current(dst, src)
    }

    /// Copy `src` into `dst`. Synchronous; lengths must match exactly.
    pub fn copy_d2h(&self, dst: &mut [u8], src: &DeviceBuffer<'_>) -> Result<(), Error> {
        self.check_owner(src.owner.raw)?;
        check_len(src.len, dst.len())?;
        self.make_current()?;
        self.d2h_current(dst, src)
    }

    fn check_owner(&self, owner: CuContext) -> Result<(), Error> {
        if owner == self.raw {
            Ok(())
        } else {
            Err(Error::InvalidArgument(
                "device buffer belongs to another context",
            ))
        }
    }

    // The two raw copies below assume the caller has already checked lengths
    // and made this context current, so the timed region in `copy_bench`
    // contains the driver call and nothing else.

    fn h2d_current(&self, dst: &DeviceBuffer<'_>, src: &[u8]) -> Result<(), Error> {
        debug_assert_eq!(dst.len, src.len());
        // SAFETY: `src` is readable for `src.len()` bytes and `dst.ptr` is a
        // device allocation of exactly that many bytes (checked by callers).
        let r = unsafe { (self.api().memcpy_htod)(dst.ptr, src.as_ptr().cast(), src.len()) };
        self.cuda.check("cuMemcpyHtoD_v2", r)
    }

    fn d2h_current(&self, dst: &mut [u8], src: &DeviceBuffer<'_>) -> Result<(), Error> {
        debug_assert_eq!(src.len, dst.len());
        // SAFETY: `dst` is writable for `dst.len()` bytes and `src.ptr` is a
        // device allocation of exactly that many bytes (checked by callers).
        let r = unsafe { (self.api().memcpy_dtoh)(dst.as_mut_ptr().cast(), src.ptr, dst.len()) };
        self.cuda.check("cuMemcpyDtoH_v2", r)
    }
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is a live context, destroyed exactly once. Every
        // buffer borrows this context, so none can outlive it. The result is
        // ignored: there is nothing useful to do with a failure in drop.
        let _ = unsafe { (self.api().ctx_destroy)(self.raw) };
    }
}

/// Device memory (`cuMemAlloc_v2`). Freed on drop.
pub struct DeviceBuffer<'c> {
    owner: &'c Context<'c>,
    ptr: CuDevicePtr,
    len: usize,
}

impl fmt::Debug for DeviceBuffer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DeviceBuffer({:#x}, {} bytes)", self.ptr, self.len)
    }
}

impl<'c> DeviceBuffer<'c> {
    /// Allocate `bytes` of device memory in `ctx`. Contents are undefined.
    pub fn new(ctx: &'c Context<'c>, bytes: usize) -> Result<DeviceBuffer<'c>, Error> {
        check_nonzero(bytes)?;
        ctx.make_current()?;
        let mut ptr: CuDevicePtr = 0;
        // SAFETY: `ptr` is a valid out-pointer; `bytes` is non-zero.
        let r = unsafe { (ctx.api().mem_alloc)(&mut ptr, bytes) };
        ctx.cuda.check("cuMemAlloc_v2", r)?;
        Ok(DeviceBuffer {
            owner: ctx,
            ptr,
            len: bytes,
        })
    }

    /// Size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: zero-byte buffers are rejected at allocation.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for DeviceBuffer<'_> {
    fn drop(&mut self) {
        // Free in the owning context; errors in drop are ignored.
        let _ = self.owner.make_current();
        // SAFETY: `self.ptr` came from cuMemAlloc_v2 and is freed exactly once,
        // while its context is still alive (the borrow guarantees it).
        let _ = unsafe { (self.owner.api().mem_free)(self.ptr) };
    }
}

/// Page-locked host memory (`cuMemHostAlloc`, flags 0). Freed on drop.
///
/// Zero-filled at allocation so the slice views never expose uninitialized
/// bytes.
pub struct PinnedBuffer<'c> {
    owner: &'c Context<'c>,
    ptr: *mut u8,
    len: usize,
}

impl fmt::Debug for PinnedBuffer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PinnedBuffer({:p}, {} bytes)", self.ptr, self.len)
    }
}

impl<'c> PinnedBuffer<'c> {
    /// Allocate `bytes` of pinned host memory through `ctx`.
    pub fn new(ctx: &'c Context<'c>, bytes: usize) -> Result<PinnedBuffer<'c>, Error> {
        check_nonzero(bytes)?;
        ctx.make_current()?;
        let mut p: *mut c_void = std::ptr::null_mut();
        // SAFETY: `p` is a valid out-pointer; `bytes` is non-zero; flags 0 is
        // plain page-locked memory.
        let r = unsafe { (ctx.api().mem_host_alloc)(&mut p, bytes, 0) };
        ctx.cuda.check("cuMemHostAlloc", r)?;
        if p.is_null() {
            return Err(Error::Driver {
                call: "cuMemHostAlloc",
                code: 0,
                name: Some("returned a null pointer".into()),
            });
        }
        let ptr = p.cast::<u8>();
        // SAFETY: the driver just returned `bytes` writable bytes at `ptr`.
        unsafe { std::ptr::write_bytes(ptr, 0, bytes) };
        Ok(PinnedBuffer {
            owner: ctx,
            ptr,
            len: bytes,
        })
    }

    /// Size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false: zero-byte buffers are rejected at allocation.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The memory as a byte slice.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is a live, initialized (zero-filled at creation)
        // allocation of `len` bytes, borrowed immutably for the slice's life.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// The memory as a mutable byte slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` guarantees exclusive access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for PinnedBuffer<'_> {
    fn drop(&mut self) {
        let _ = self.owner.make_current();
        // SAFETY: `self.ptr` came from cuMemHostAlloc and is freed exactly once,
        // while its context is alive (the borrow guarantees it).
        let _ = unsafe { (self.owner.api().mem_free_host)(self.ptr.cast()) };
    }
}

/// Per-copy timings from [`copy_bench`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyStats {
    /// Bytes moved by each copy.
    pub bytes_per_copy: usize,
    /// Whether the host side was pinned (`cuMemHostAlloc`) or pageable (`Vec`).
    pub pinned: bool,
    /// Wall time of each host-to-device copy, in run order.
    pub h2d: Vec<Duration>,
    /// Wall time of each device-to-host copy, in run order.
    pub d2h: Vec<Duration>,
}

/// A fill pattern with no short period, so a stale or shifted buffer cannot
/// match by accident.
fn fill_pattern(buf: &mut [u8]) {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for chunk in buf.chunks_mut(8) {
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let b = x.to_le_bytes();
        chunk.copy_from_slice(&b[..chunk.len()]);
    }
}

/// First offset where `a` and `b` differ, if any (lengths assumed equal).
fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
}

/// Host memory for one side of the benchmark: pinned or plain pageable.
enum HostMem<'c> {
    Pinned(PinnedBuffer<'c>),
    Pageable(Vec<u8>),
}

impl HostMem<'_> {
    fn new<'c>(ctx: &'c Context<'c>, bytes: usize, pinned: bool) -> Result<HostMem<'c>, Error> {
        Ok(if pinned {
            HostMem::Pinned(PinnedBuffer::new(ctx, bytes)?)
        } else {
            HostMem::Pageable(vec![0u8; bytes])
        })
    }
    fn bytes(&self) -> &[u8] {
        match self {
            HostMem::Pinned(p) => p.as_slice(),
            HostMem::Pageable(v) => v,
        }
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        match self {
            HostMem::Pinned(p) => p.as_mut_slice(),
            HostMem::Pageable(v) => v,
        }
    }
}

/// Time `iterations` host-to-device and device-to-host copies of
/// `bytes_per_copy` each, one sample per copy.
///
/// Order of work:
/// 1. Allocate one device buffer and two host buffers (pinned or pageable).
/// 2. Verify a round trip: fill the source with a pattern, H2D, D2H into the
///    second buffer, compare. Any difference is [`Error::Corrupted`].
/// 3. One unrecorded warm-up copy in each direction.
/// 4. `iterations` times: time one H2D, then one D2H.
///
/// Each sample is the wall time of the copy call **plus** a
/// `cuCtxSynchronize`. The sync matters for pageable H2D: per NVIDIA's "API
/// synchronization behavior" notes, that copy may return once the data is in
/// the driver's staging buffer, before the DMA to the device finishes.
/// Including the sync makes every sample cover the complete transfer; for the
/// other cases the sync is already a no-op and costs only its call overhead.
pub fn copy_bench<'c>(
    ctx: &'c Context<'c>,
    bytes_per_copy: usize,
    iterations: usize,
    pinned: bool,
) -> Result<CopyStats, Error> {
    check_nonzero(bytes_per_copy)?;
    if iterations == 0 {
        return Err(Error::InvalidArgument("iterations must be non-zero"));
    }

    let bytes = bytes_per_copy;
    let dev = DeviceBuffer::new(ctx, bytes)?;
    let mut src = HostMem::new(ctx, bytes, pinned)?;
    let mut dst = HostMem::new(ctx, bytes, pinned)?;

    // Round-trip correctness, once.
    fill_pattern(src.bytes_mut());
    for b in dst.bytes_mut().iter_mut() {
        *b = 0xA5;
    }
    ctx.copy_h2d(&dev, src.bytes())?;
    ctx.copy_d2h(dst.bytes_mut(), &dev)?;
    if let Some(offset) = first_difference(src.bytes(), dst.bytes()) {
        return Err(Error::Corrupted { offset });
    }

    // Everything below runs in this context with lengths already checked.
    ctx.make_current()?;

    // Warm-up, not recorded.
    ctx.h2d_current(&dev, src.bytes())?;
    ctx.sync_current()?;
    ctx.d2h_current(dst.bytes_mut(), &dev)?;
    ctx.sync_current()?;

    let mut h2d = Vec::with_capacity(iterations);
    let mut d2h = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t = Instant::now();
        ctx.h2d_current(&dev, src.bytes())?;
        ctx.sync_current()?;
        h2d.push(t.elapsed());

        let t = Instant::now();
        ctx.d2h_current(dst.bytes_mut(), &dev)?;
        ctx.sync_current()?;
        d2h.push(t.elapsed());
    }

    Ok(CopyStats {
        bytes_per_copy: bytes,
        pinned,
        h2d,
        d2h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_check_accepts_equal_and_rejects_unequal() {
        assert_eq!(check_len(16, 16), Ok(()));
        assert_eq!(
            check_len(16, 15),
            Err(Error::SizeMismatch {
                device: 16,
                host: 15
            })
        );
    }

    #[test]
    fn zero_sizes_are_rejected() {
        assert!(matches!(check_nonzero(0), Err(Error::InvalidArgument(_))));
        assert_eq!(check_nonzero(1), Ok(()));
    }

    #[test]
    fn errors_format_readably() {
        let e = Error::Driver {
            call: "cuMemAlloc_v2",
            code: 2,
            name: Some("CUDA_ERROR_OUT_OF_MEMORY".into()),
        };
        assert_eq!(
            e.to_string(),
            "cuMemAlloc_v2 failed: CUDA_ERROR_OUT_OF_MEMORY (2)"
        );
        let e = Error::Driver {
            call: "cuInit",
            code: 100,
            name: None,
        };
        assert_eq!(e.to_string(), "cuInit failed: CUresult 100");
        assert_eq!(
            Error::SizeMismatch { device: 4, host: 3 }.to_string(),
            "size mismatch: device buffer is 4 bytes, host slice is 3 bytes"
        );
        assert!(Error::LibraryNotFound(126).to_string().contains("126"));
        assert!(Error::MissingSymbol("cuInit")
            .to_string()
            .contains("cuInit"));
        assert!(Error::Corrupted { offset: 7 }
            .to_string()
            .contains("byte 7"));
        // It is a real std error.
        let _: &dyn std::error::Error = &Error::Unsupported;
    }

    #[test]
    fn load_errors_map_to_public_errors() {
        assert_eq!(Error::from(LoadError::Unsupported), Error::Unsupported);
        assert_eq!(
            Error::from(LoadError::Library(126)),
            Error::LibraryNotFound(126)
        );
        assert_eq!(
            Error::from(LoadError::Symbol("cuInit")),
            Error::MissingSymbol("cuInit")
        );
    }

    #[test]
    fn pattern_has_no_short_period_and_diff_finds_first_change() {
        let mut a = vec![0u8; 4099];
        fill_pattern(&mut a);
        assert_ne!(&a[..256], &a[256..512]);
        let mut b = a.clone();
        assert_eq!(first_difference(&a, &b), None);
        b[4098] ^= 1;
        b[1000] ^= 1;
        assert_eq!(first_difference(&a, &b), Some(1000));
    }

    /// Needs an NVIDIA driver; skips (passes with a note) without one.
    #[test]
    fn round_trip_one_mib_on_real_gpu() {
        let cuda = match Cuda::load() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("skipping GPU round trip: {e}");
                return;
            }
        };
        let dev = cuda.device(0).expect("device 0");
        let ctx = Context::new(&cuda, &dev).expect("context");
        let n = 1 << 20;
        let buf = DeviceBuffer::new(&ctx, n).expect("device alloc");
        let mut src = vec![0u8; n];
        fill_pattern(&mut src);
        ctx.copy_h2d(&buf, &src).expect("h2d");
        let mut back = PinnedBuffer::new(&ctx, n).expect("pinned alloc");
        ctx.copy_d2h(back.as_mut_slice(), &buf).expect("d2h");
        ctx.synchronize().expect("sync");
        assert_eq!(back.as_slice(), &src[..]);

        // Size mismatches are refused before reaching the driver.
        let short = vec![0u8; n - 1];
        assert_eq!(
            ctx.copy_h2d(&buf, &short),
            Err(Error::SizeMismatch {
                device: n,
                host: n - 1
            })
        );

        // The benchmark path verifies itself too.
        let stats = copy_bench(&ctx, n, 2, false).expect("bench");
        assert_eq!((stats.h2d.len(), stats.d2h.len()), (2, 2));
    }
}
