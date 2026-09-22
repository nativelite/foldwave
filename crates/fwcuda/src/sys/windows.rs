//! Loading `nvcuda.dll` and resolving the Driver API entry points.
//!
//! `LoadLibraryExW`, `GetProcAddress` and `GetLastError` live in `kernel32`,
//! which std already links, so this adds no library and no crate. The DLL is
//! searched for in System32 only (`LOAD_LIBRARY_SEARCH_SYSTEM32`), where the
//! display driver installs it, so a stray `nvcuda.dll` in the working
//! directory is never picked up.
//!
//! Signatures follow the CUDA Driver API reference
//! (<https://docs.nvidia.com/cuda/cuda-driver-api/>). Where `cuda.h` maps a
//! name to a versioned export (`cuMemAlloc` to `cuMemAlloc_v2`, and so on),
//! the versioned export is resolved, because that is the symbol the header
//! would have called. `cuCtxCreate_v2` keeps the pre-13.0 three-argument
//! signature; the driver still exports it for older binaries.

use super::*;
use std::ffi::c_void;

// `CuDevicePtr` is fixed at 64 bits; a 32-bit build would mismatch the ABI.
#[cfg(not(target_pointer_width = "64"))]
compile_error!("fwcuda supports 64-bit Windows only (CUdeviceptr is pointer-sized)");

type Hmodule = *mut c_void;

const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryExW(name: *const u16, file: *mut c_void, flags: u32) -> Hmodule;
    fn GetProcAddress(module: Hmodule, name: *const u8) -> *mut c_void;
    fn GetLastError() -> u32;
}

/// Load `nvcuda.dll` from System32 and resolve every entry point.
///
/// The module is deliberately never freed: the driver starts threads of its
/// own, and unloading it under them is a known way to crash a process. A
/// process that loads CUDA keeps it until exit, as the CUDA runtime does.
pub fn load() -> Result<Api, LoadError> {
    let name: Vec<u16> = "nvcuda.dll\0".encode_utf16().collect();
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the call;
    // the reserved file handle is null as documented. Failure returns null.
    let module = unsafe {
        LoadLibraryExW(
            name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if module.is_null() {
        // SAFETY: a plain thread-local query, read straight after the failure.
        return Err(LoadError::Library(unsafe { GetLastError() }));
    }

    let find = |sym: &'static str| -> Result<*mut c_void, LoadError> {
        debug_assert!(sym.ends_with('\0'));
        // SAFETY: `module` is a live module handle and `sym` is NUL-terminated
        // ASCII. A missing export returns null, which is checked.
        let p = unsafe { GetProcAddress(module, sym.as_ptr()) };
        if p.is_null() {
            Err(LoadError::Symbol(sym.trim_end_matches('\0')))
        } else {
            Ok(p)
        }
    };

    // Each arm casts a non-null export address to the fn-pointer type of its
    // field. The aliases in `mod.rs` are the documented C prototypes, so the
    // transmute is exactly what a C caller of the header would do.
    macro_rules! sym {
        ($name:literal, $ty:ty) => {{
            let p = find(concat!($name, "\0"))?;
            // SAFETY: `p` is a non-null export of nvcuda.dll named $name, whose
            // documented prototype is the target field's type; data and code
            // pointers are the same size on every Windows target.
            unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
        }};
    }

    let get_error_name = match find("cuGetErrorName\0") {
        // SAFETY: as for `sym!`: a non-null export whose documented prototype is
        // `CUresult cuGetErrorName(CUresult, const char **)`.
        Ok(p) => Some(unsafe { std::mem::transmute::<*mut c_void, FnGetErrorName>(p) }),
        Err(_) => None,
    };

    Ok(Api {
        init: sym!("cuInit", FnInit),
        device_get_count: sym!("cuDeviceGetCount", FnDeviceGetCount),
        device_get: sym!("cuDeviceGet", FnDeviceGet),
        device_get_name: sym!("cuDeviceGetName", FnDeviceGetName),
        device_total_mem: sym!("cuDeviceTotalMem_v2", FnDeviceTotalMem),
        ctx_create: sym!("cuCtxCreate_v2", FnCtxCreate),
        ctx_destroy: sym!("cuCtxDestroy_v2", FnCtxOp),
        ctx_set_current: sym!("cuCtxSetCurrent", FnCtxOp),
        ctx_synchronize: sym!("cuCtxSynchronize", FnCtxSynchronize),
        mem_alloc: sym!("cuMemAlloc_v2", FnMemAlloc),
        mem_free: sym!("cuMemFree_v2", FnMemFree),
        mem_host_alloc: sym!("cuMemHostAlloc", FnMemHostAlloc),
        mem_free_host: sym!("cuMemFreeHost", FnMemFreeHost),
        memcpy_htod: sym!("cuMemcpyHtoD_v2", FnMemcpyHtoD),
        memcpy_dtoh: sym!("cuMemcpyDtoH_v2", FnMemcpyDtoH),
        get_error_name,
    })
}
