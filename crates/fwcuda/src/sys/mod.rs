//! The raw Driver API surface: C types, the entry-point table, and the
//! platform loader. Only `windows.rs` can actually fill the table; everywhere
//! else [`load`] reports that CUDA is unsupported.

use std::ffi::c_void;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::load;

/// Non-Windows stub: there is no loader yet, so nothing is ever resolved.
#[cfg(not(windows))]
pub fn load() -> Result<Api, LoadError> {
    Err(LoadError::Unsupported)
}

/// `CUresult`: an enum in C, `int`-sized. Zero is `CUDA_SUCCESS`.
pub type CuResult = i32;
/// `CUdevice`: `typedef int CUdevice_v1`.
pub type CuDevice = i32;
/// `CUdeviceptr`: an unsigned integer the size of a pointer; 64-bit here.
pub type CuDevicePtr = u64;
/// `CUcontext`: `struct CUctx_st *`, opaque.
pub type CuContext = *mut c_void;
/// `CUmodule`: `struct CUmod_st *`, opaque.
pub type CuModule = *mut c_void;
/// `CUfunction`: `struct CUfunc_st *`, opaque.
pub type CuFunction = *mut c_void;
/// `CUstream`: `struct CUstream_st *`; null is the default stream.
pub type CuStream = *mut c_void;

// One alias per entry point: the documented C prototype, `CUDAAPI` being
// `__stdcall` on Windows (identical to the C ABI on x64).
pub type FnInit = unsafe extern "system" fn(flags: u32) -> CuResult;
pub type FnDeviceGetCount = unsafe extern "system" fn(count: *mut i32) -> CuResult;
pub type FnDeviceGet = unsafe extern "system" fn(device: *mut CuDevice, ordinal: i32) -> CuResult;
pub type FnDeviceGetName =
    unsafe extern "system" fn(name: *mut u8, len: i32, dev: CuDevice) -> CuResult;
pub type FnDeviceTotalMem = unsafe extern "system" fn(bytes: *mut usize, dev: CuDevice) -> CuResult;
pub type FnCtxCreate =
    unsafe extern "system" fn(pctx: *mut CuContext, flags: u32, dev: CuDevice) -> CuResult;
pub type FnCtxOp = unsafe extern "system" fn(ctx: CuContext) -> CuResult;
pub type FnCtxSynchronize = unsafe extern "system" fn() -> CuResult;
pub type FnMemAlloc = unsafe extern "system" fn(dptr: *mut CuDevicePtr, bytes: usize) -> CuResult;
pub type FnMemFree = unsafe extern "system" fn(dptr: CuDevicePtr) -> CuResult;
pub type FnMemHostAlloc =
    unsafe extern "system" fn(pp: *mut *mut c_void, bytes: usize, flags: u32) -> CuResult;
pub type FnMemFreeHost = unsafe extern "system" fn(p: *mut c_void) -> CuResult;
pub type FnMemcpyHtoD =
    unsafe extern "system" fn(dst: CuDevicePtr, src: *const c_void, bytes: usize) -> CuResult;
pub type FnMemcpyDtoH =
    unsafe extern "system" fn(dst: *mut c_void, src: CuDevicePtr, bytes: usize) -> CuResult;
pub type FnModuleLoadData =
    unsafe extern "system" fn(module: *mut CuModule, image: *const c_void) -> CuResult;
pub type FnModuleUnload = unsafe extern "system" fn(module: CuModule) -> CuResult;
pub type FnModuleGetFunction =
    unsafe extern "system" fn(func: *mut CuFunction, module: CuModule, name: *const u8) -> CuResult;
#[allow(clippy::too_many_arguments)]
pub type FnLaunchKernel = unsafe extern "system" fn(
    f: CuFunction,
    grid_x: u32,
    grid_y: u32,
    grid_z: u32,
    block_x: u32,
    block_y: u32,
    block_z: u32,
    shared_bytes: u32,
    stream: CuStream,
    params: *mut *mut c_void,
    extra: *mut *mut c_void,
) -> CuResult;
pub type FnGetErrorName = unsafe extern "system" fn(err: CuResult, s: *mut *const u8) -> CuResult;

/// The Driver API entry points this crate uses, resolved once at load.
pub struct Api {
    pub init: FnInit,
    pub device_get_count: FnDeviceGetCount,
    pub device_get: FnDeviceGet,
    pub device_get_name: FnDeviceGetName,
    pub device_total_mem: FnDeviceTotalMem,
    pub ctx_create: FnCtxCreate,
    /// `cuCtxDestroy_v2`.
    pub ctx_destroy: FnCtxOp,
    /// `cuCtxSetCurrent`.
    pub ctx_set_current: FnCtxOp,
    pub ctx_synchronize: FnCtxSynchronize,
    pub mem_alloc: FnMemAlloc,
    pub mem_free: FnMemFree,
    pub mem_host_alloc: FnMemHostAlloc,
    pub mem_free_host: FnMemFreeHost,
    pub memcpy_htod: FnMemcpyHtoD,
    pub memcpy_dtoh: FnMemcpyDtoH,
    pub module_load_data: FnModuleLoadData,
    pub module_unload: FnModuleUnload,
    pub module_get_function: FnModuleGetFunction,
    pub launch_kernel: FnLaunchKernel,
    /// Optional: only used to make errors readable.
    pub get_error_name: Option<FnGetErrorName>,
}

/// Why loading failed, before any CUDA call was made.
pub enum LoadError {
    /// This platform has no loader (only Windows is implemented).
    #[allow(dead_code)]
    Unsupported,
    /// `LoadLibraryExW` failed; the Win32 error code.
    Library(u32),
    /// The DLL loaded but lacks this export.
    Symbol(&'static str),
}

/// The driver's name for a result code, such as `CUDA_ERROR_OUT_OF_MEMORY`.
pub fn error_name(api: &Api, code: CuResult) -> Option<String> {
    let f = api.get_error_name?;
    let mut s: *const u8 = std::ptr::null();
    // SAFETY: `s` is a valid out-pointer. On success the driver points it at a
    // static NUL-terminated string; on failure it is left null and not read.
    let r = unsafe { f(code, &mut s) };
    if r != 0 || s.is_null() {
        return None;
    }
    // SAFETY: non-null and documented to be a static C string.
    let c = unsafe { std::ffi::CStr::from_ptr(s.cast()) };
    Some(c.to_string_lossy().into_owned())
}
