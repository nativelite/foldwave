//! `libwayland-client`, bound by hand and loaded at run time.
//!
//! The library is the system's own (every Wayland desktop ships it, as
//! Windows ships `vulkan-1.dll`), found with `dlopen`; nothing is linked and
//! no crate is involved (founder decision, `docs/design.md` section 11, row
//! 25). It is needed, not merely convenient: Vulkan's Wayland surface takes
//! the library's own `wl_display` and the driver calls the library on it.
//!
//! What is transcribed here, field for field, from `wayland-client-core.h`
//! and `wayland-util.h` (libwayland 1.20 or later, for
//! `wl_proxy_marshal_array_flags`):
//!
//! * the handful of functions a client needs to connect, send requests and
//!   read events;
//! * `wl_interface` and `wl_message`, the tables that describe a protocol.
//!   The core protocol's tables are exported by the library and looked up by
//!   name. `xdg-shell`'s are not in the library (a C client generates them
//!   with `wayland-scanner`), so they are written out below from the
//!   protocol's XML, stable version 1.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

/// An opaque `struct wl_proxy` (a `wl_display` is one too).
pub type Proxy = c_void;

/// `struct wl_message`.
#[repr(C)]
pub struct Message {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub types: *const *const Interface,
}

/// `struct wl_interface`.
#[repr(C)]
pub struct Interface {
    pub name: *const c_char,
    pub version: c_int,
    pub method_count: c_int,
    pub methods: *const Message,
    pub event_count: c_int,
    pub events: *const Message,
}

// SAFETY: these tables are immutable and hold only pointers to other
// immutable statics.
unsafe impl Sync for Message {}
// SAFETY: as above.
unsafe impl Sync for Interface {}

/// `union wl_argument`: eight bytes, read by the message's signature.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Arg {
    pub i: i32,
    pub u: u32,
    pub p: *const c_void,
}

impl Arg {
    pub fn int(i: i32) -> Arg {
        Arg {
            p: std::ptr::null(),
        }
        .with(|a| a.i = i)
    }
    pub fn uint(u: u32) -> Arg {
        Arg {
            p: std::ptr::null(),
        }
        .with(|a| a.u = u)
    }
    pub fn ptr<T>(p: *const T) -> Arg {
        Arg { p: p.cast() }
    }
    /// The placeholder for a `new_id` argument: the library fills it in.
    pub fn new_id() -> Arg {
        Arg {
            p: std::ptr::null(),
        }
    }
    fn with(mut self, set: impl FnOnce(&mut Arg)) -> Arg {
        set(&mut self);
        self
    }
}

/// `struct wl_array`.
#[repr(C)]
pub struct Array {
    pub size: usize,
    pub alloc: usize,
    pub data: *mut c_void,
}

/// `WL_MARSHAL_FLAG_DESTROY`: the request destroys the proxy.
pub const MARSHAL_DESTROY: u32 = 1;

/// A type list for messages whose object arguments need no checking: the
/// library skips a null entry. Long enough for every message used here.
struct Types([*const Interface; 8]);
// SAFETY: all null, never written.
unsafe impl Sync for Types {}
static NO_TYPES: Types = Types([std::ptr::null(); 8]);

const fn message(name: &'static str, signature: &'static str) -> Message {
    Message {
        name: name.as_ptr().cast(),
        signature: signature.as_ptr().cast(),
        types: NO_TYPES.0.as_ptr(),
    }
}

const fn interface(
    name: &'static str,
    methods: &'static [Message],
    events: &'static [Message],
) -> Interface {
    Interface {
        name: name.as_ptr().cast(),
        version: 1,
        method_count: methods.len() as c_int,
        methods: methods.as_ptr(),
        event_count: events.len() as c_int,
        events: events.as_ptr(),
    }
}

// xdg-shell, stable, version 1. Every request up to the last one used must
// be listed, in order: a request is sent by its position.

static XDG_WM_BASE_REQUESTS: [Message; 4] = [
    message("destroy\0", "\0"),
    message("create_positioner\0", "n\0"),
    message("get_xdg_surface\0", "no\0"),
    message("pong\0", "u\0"),
];
static XDG_WM_BASE_EVENTS: [Message; 1] = [message("ping\0", "u\0")];
/// `xdg_wm_base`: the global that turns surfaces into windows.
pub static XDG_WM_BASE: Interface =
    interface("xdg_wm_base\0", &XDG_WM_BASE_REQUESTS, &XDG_WM_BASE_EVENTS);

static XDG_SURFACE_REQUESTS: [Message; 5] = [
    message("destroy\0", "\0"),
    message("get_toplevel\0", "n\0"),
    message("get_popup\0", "n?oo\0"),
    message("set_window_geometry\0", "iiii\0"),
    message("ack_configure\0", "u\0"),
];
static XDG_SURFACE_EVENTS: [Message; 1] = [message("configure\0", "u\0")];
/// `xdg_surface`.
pub static XDG_SURFACE: Interface =
    interface("xdg_surface\0", &XDG_SURFACE_REQUESTS, &XDG_SURFACE_EVENTS);

static XDG_TOPLEVEL_REQUESTS: [Message; 14] = [
    message("destroy\0", "\0"),
    message("set_parent\0", "?o\0"),
    message("set_title\0", "s\0"),
    message("set_app_id\0", "s\0"),
    message("show_window_menu\0", "ouii\0"),
    message("move\0", "ou\0"),
    message("resize\0", "ouu\0"),
    message("set_max_size\0", "ii\0"),
    message("set_min_size\0", "ii\0"),
    message("set_maximized\0", "\0"),
    message("unset_maximized\0", "\0"),
    message("set_fullscreen\0", "?o\0"),
    message("unset_fullscreen\0", "\0"),
    message("set_minimized\0", "\0"),
];
static XDG_TOPLEVEL_EVENTS: [Message; 2] =
    [message("configure\0", "iia\0"), message("close\0", "\0")];
/// `xdg_toplevel`: an ordinary window.
pub static XDG_TOPLEVEL: Interface = interface(
    "xdg_toplevel\0",
    &XDG_TOPLEVEL_REQUESTS,
    &XDG_TOPLEVEL_EVENTS,
);

// Request numbers, from the protocol XML (position in each interface).
pub const DISPLAY_GET_REGISTRY: u32 = 1;
pub const REGISTRY_BIND: u32 = 0;
pub const COMPOSITOR_CREATE_SURFACE: u32 = 0;
pub const SURFACE_DESTROY: u32 = 0;
pub const SURFACE_ATTACH: u32 = 1;
pub const SURFACE_DAMAGE: u32 = 2;
pub const SURFACE_COMMIT: u32 = 6;
pub const SURFACE_DAMAGE_BUFFER: u32 = 9;
pub const SHM_CREATE_POOL: u32 = 0;
pub const SHM_POOL_CREATE_BUFFER: u32 = 0;
pub const SHM_POOL_DESTROY: u32 = 1;
pub const BUFFER_DESTROY: u32 = 0;
pub const SEAT_GET_POINTER: u32 = 0;
pub const SEAT_GET_KEYBOARD: u32 = 1;
pub const XDG_WM_BASE_DESTROY: u32 = 0;
pub const XDG_WM_BASE_GET_XDG_SURFACE: u32 = 2;
pub const XDG_WM_BASE_PONG: u32 = 3;
pub const XDG_SURFACE_DESTROY: u32 = 0;
pub const XDG_SURFACE_GET_TOPLEVEL: u32 = 1;
pub const XDG_SURFACE_SET_WINDOW_GEOMETRY: u32 = 3;
pub const XDG_SURFACE_ACK_CONFIGURE: u32 = 4;
pub const XDG_TOPLEVEL_DESTROY: u32 = 0;
pub const XDG_TOPLEVEL_SET_TITLE: u32 = 2;
pub const XDG_TOPLEVEL_SET_APP_ID: u32 = 3;

/// `WL_SHM_FORMAT_XRGB8888`: bytes B, G, R, X, which is this crate's
/// `0xAARRGGBB` on a little-endian machine. Every compositor supports it.
pub const SHM_FORMAT_XRGB8888: u32 = 1;

/// The library's entry points and the core protocol's tables.
pub struct Lib {
    pub display_connect: unsafe extern "C" fn(*const c_char) -> *mut Proxy,
    pub display_disconnect: unsafe extern "C" fn(*mut Proxy),
    pub display_get_fd: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_dispatch_pending: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_flush: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_roundtrip: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_prepare_read: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_read_events: unsafe extern "C" fn(*mut Proxy) -> c_int,
    pub display_cancel_read: unsafe extern "C" fn(*mut Proxy),
    pub proxy_marshal_array_flags:
        unsafe extern "C" fn(*mut Proxy, u32, *const Interface, u32, u32, *mut Arg) -> *mut Proxy,
    pub proxy_add_listener:
        unsafe extern "C" fn(*mut Proxy, *const *const c_void, *mut c_void) -> c_int,
    pub proxy_destroy: unsafe extern "C" fn(*mut Proxy),
    pub proxy_get_version: unsafe extern "C" fn(*mut Proxy) -> u32,
    pub registry: *const Interface,
    pub compositor: *const Interface,
    pub surface: *const Interface,
    pub shm: *const Interface,
    pub shm_pool: *const Interface,
    pub buffer: *const Interface,
    pub seat: *const Interface,
    pub pointer: *const Interface,
    pub keyboard: *const Interface,
}

// SAFETY: function pointers and pointers to the library's immutable tables,
// valid for the life of the process (the library is never unloaded).
unsafe impl Sync for Lib {}
// SAFETY: as above.
unsafe impl Send for Lib {}

extern "C" {
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

/// Look `name` (NUL-terminated) up and reinterpret it as `T`, a function
/// pointer or a data pointer.
///
/// # Safety
/// `T` must be pointer-sized and match what the library exports as `name`.
unsafe fn symbol<T: Copy>(lib: *mut c_void, name: &'static str) -> Result<T, String> {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
    // SAFETY: a live handle and a NUL-terminated name.
    let p = unsafe { dlsym(lib, name.as_ptr().cast()) };
    if p.is_null() {
        return Err(format!(
            "libwayland-client has no {} (1.20 or later is needed)",
            name.trim_end_matches('\0')
        ));
    }
    // SAFETY: the caller chose `T` to match the export; sizes are asserted.
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) })
}

fn load() -> Result<Lib, String> {
    // SAFETY: a NUL-terminated name; failure returns null.
    let lib = unsafe { dlopen(b"libwayland-client.so.0\0".as_ptr().cast(), RTLD_NOW) };
    if lib.is_null() {
        return Err("libwayland-client.so.0 could not be loaded".into());
    }
    // SAFETY: each type is the prototype or table `wayland-client-core.h`
    // and `wayland-client-protocol.h` declare for that name.
    unsafe {
        Ok(Lib {
            display_connect: symbol(lib, "wl_display_connect\0")?,
            display_disconnect: symbol(lib, "wl_display_disconnect\0")?,
            display_get_fd: symbol(lib, "wl_display_get_fd\0")?,
            display_dispatch_pending: symbol(lib, "wl_display_dispatch_pending\0")?,
            display_flush: symbol(lib, "wl_display_flush\0")?,
            display_roundtrip: symbol(lib, "wl_display_roundtrip\0")?,
            display_prepare_read: symbol(lib, "wl_display_prepare_read\0")?,
            display_read_events: symbol(lib, "wl_display_read_events\0")?,
            display_cancel_read: symbol(lib, "wl_display_cancel_read\0")?,
            proxy_marshal_array_flags: symbol(lib, "wl_proxy_marshal_array_flags\0")?,
            proxy_add_listener: symbol(lib, "wl_proxy_add_listener\0")?,
            proxy_destroy: symbol(lib, "wl_proxy_destroy\0")?,
            proxy_get_version: symbol(lib, "wl_proxy_get_version\0")?,
            registry: symbol(lib, "wl_registry_interface\0")?,
            compositor: symbol(lib, "wl_compositor_interface\0")?,
            surface: symbol(lib, "wl_surface_interface\0")?,
            shm: symbol(lib, "wl_shm_interface\0")?,
            shm_pool: symbol(lib, "wl_shm_pool_interface\0")?,
            buffer: symbol(lib, "wl_buffer_interface\0")?,
            seat: symbol(lib, "wl_seat_interface\0")?,
            pointer: symbol(lib, "wl_pointer_interface\0")?,
            keyboard: symbol(lib, "wl_keyboard_interface\0")?,
        })
    }
}

/// The library, loaded once. An error says what is missing.
pub fn lib() -> Result<&'static Lib, String> {
    static LIB: OnceLock<Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(load).as_ref().map_err(Clone::clone)
}

impl Lib {
    /// Send a request that creates nothing.
    ///
    /// # Safety
    /// `proxy` is live and `args` matches the request's signature.
    pub unsafe fn request(&self, proxy: *mut Proxy, opcode: u32, args: &mut [Arg]) {
        // SAFETY: the caller's contract; the version is the proxy's own.
        unsafe {
            let version = (self.proxy_get_version)(proxy);
            (self.proxy_marshal_array_flags)(
                proxy,
                opcode,
                std::ptr::null(),
                version,
                0,
                args.as_mut_ptr(),
            );
        }
    }

    /// Send a request that creates an object of `interface`, at the parent's
    /// version, and return it (null if the connection is broken).
    ///
    /// # Safety
    /// As [`Lib::request`]; `args` holds one [`Arg::new_id`].
    pub unsafe fn create(
        &self,
        proxy: *mut Proxy,
        opcode: u32,
        interface: *const Interface,
        args: &mut [Arg],
    ) -> *mut Proxy {
        // SAFETY: the caller's contract.
        unsafe {
            let version = (self.proxy_get_version)(proxy);
            (self.proxy_marshal_array_flags)(
                proxy,
                opcode,
                interface,
                version,
                0,
                args.as_mut_ptr(),
            )
        }
    }

    /// Send a destructor request and free the proxy.
    ///
    /// # Safety
    /// `proxy` is live and not used again.
    pub unsafe fn destroy(&self, proxy: *mut Proxy, opcode: u32) {
        // SAFETY: the caller's contract; no arguments.
        unsafe {
            let version = (self.proxy_get_version)(proxy);
            (self.proxy_marshal_array_flags)(
                proxy,
                opcode,
                std::ptr::null(),
                version,
                MARSHAL_DESTROY,
                std::ptr::null_mut(),
            );
        }
    }

    /// `wl_registry.bind`: bind the global `name` as `interface` at
    /// `version`.
    ///
    /// # Safety
    /// `registry` is live; `interface` describes the global named.
    pub unsafe fn bind(
        &self,
        registry: *mut Proxy,
        name: u32,
        interface: *const Interface,
        version: u32,
    ) -> *mut Proxy {
        // SAFETY: the bind request's signature is "usun"; the interface's
        // name is a NUL-terminated string.
        unsafe {
            let mut args = [
                Arg::uint(name),
                Arg::ptr((*interface).name),
                Arg::uint(version),
                Arg::new_id(),
            ];
            (self.proxy_marshal_array_flags)(
                registry,
                REGISTRY_BIND,
                interface,
                version,
                0,
                args.as_mut_ptr(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    fn text(p: *const c_char) -> &'static str {
        // SAFETY: every name and signature above is a NUL-terminated literal.
        unsafe { CStr::from_ptr(p) }.to_str().unwrap()
    }

    #[test]
    fn the_xdg_shell_tables_are_well_formed() {
        for (iface, requests, events) in [
            (&XDG_WM_BASE, 4, 1),
            (&XDG_SURFACE, 5, 1),
            (&XDG_TOPLEVEL, 14, 2),
        ] {
            assert_eq!(iface.method_count, requests, "{}", text(iface.name));
            assert_eq!(iface.event_count, events);
            for i in 0..requests + events {
                // SAFETY: within the counts just checked.
                let m = unsafe {
                    if i < requests {
                        &*iface.methods.add(i as usize)
                    } else {
                        &*iface.events.add((i - requests) as usize)
                    }
                };
                assert!(!text(m.name).is_empty());
                let args = text(m.signature).chars().filter(|c| *c != '?').count();
                assert!(args <= NO_TYPES.0.len(), "{}", text(m.name));
            }
        }
        // The positions the window sends by.
        let name = |i: &Interface, n: u32| text(unsafe { &*i.methods.add(n as usize) }.name);
        assert_eq!(
            name(&XDG_WM_BASE, XDG_WM_BASE_GET_XDG_SURFACE),
            "get_xdg_surface"
        );
        assert_eq!(name(&XDG_WM_BASE, XDG_WM_BASE_PONG), "pong");
        assert_eq!(name(&XDG_SURFACE, XDG_SURFACE_GET_TOPLEVEL), "get_toplevel");
        assert_eq!(
            name(&XDG_SURFACE, XDG_SURFACE_ACK_CONFIGURE),
            "ack_configure"
        );
        assert_eq!(name(&XDG_TOPLEVEL, XDG_TOPLEVEL_SET_TITLE), "set_title");
        assert_eq!(name(&XDG_TOPLEVEL, XDG_TOPLEVEL_SET_APP_ID), "set_app_id");
    }

    #[test]
    fn an_argument_is_eight_bytes_and_an_integer_leaves_no_stale_half() {
        assert_eq!(std::mem::size_of::<Arg>(), 8);
        // SAFETY: reading the pointer view of an integer argument.
        assert_eq!(unsafe { Arg::uint(7).p } as usize & 0xFFFF_FFFF, 7);
    }
}
