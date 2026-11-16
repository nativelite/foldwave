//! [`Window`]: a top-level Wayland window with a non-blocking event queue,
//! and [`CpuPresenter`], which shows a [`CpuRenderer`] framebuffer in it.
//!
//! The method names follow `win::Window`, so a caller is written once. What
//! Wayland has no counterpart for says so and does nothing, as documented on
//! each method.
//!
//! One thread owns the window and everything on its connection. Requests go
//! out when [`Window::poll`], [`Window::wait`] or a present flushes them;
//! events are read there and turned into [`Event`]s by the listeners below.

use super::keys;
use super::sys::*;
use crate::cpu::CpuRenderer;
pub use crate::event::{Cursor, Event, TaskbarProgress};
use std::ffi::{c_char, c_int, c_long, c_void, CStr, CString};
use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

extern "C" {
    fn eventfd(initial: u32, flags: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn ppoll(fds: *mut PollFd, count: u64, timeout: *const Timespec, mask: *const c_void) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
    fn ftruncate(fd: c_int, length: i64) -> c_int;
    fn mmap(
        addr: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, length: usize) -> c_int;
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

#[repr(C)]
struct Timespec {
    sec: c_long,
    nsec: c_long,
}

const POLLIN: i16 = 1;
const EFD_CLOEXEC: c_int = 0o2000000;
const EFD_NONBLOCK: c_int = 0o4000;
const MFD_CLOEXEC: u32 = 1;
const PROT_READ_WRITE: c_int = 3;
const MAP_SHARED: c_int = 1;
const BTN_LEFT: u32 = 0x110;
/// Two clicks this close in time and place are a double click, as Windows'
/// defaults have it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_PX: i32 = 4;

/// Shift, Ctrl and Alt as the keyboard last reported them, for
/// [`shift_down`] and the others, which take no window.
static MODS: AtomicU32 = AtomicU32::new(0);

/// A file descriptor closed on drop.
struct Fd(c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: ours, closed once.
        unsafe { close(self.0) };
    }
}

/// What [`Waker`]s write to and [`Window::wait`] sleeps on: an eventfd.
struct WakeFd(Fd);

/// Wakes a [`Window`] blocked in [`Window::wait`] from any thread; the window
/// then reports [`Event::Woken`]. Waking a dropped window does nothing.
#[derive(Clone)]
pub struct Waker {
    target: Weak<WakeFd>,
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waker").finish_non_exhaustive()
    }
}

impl Waker {
    /// Queue [`Event::Woken`]. Cheap and non-blocking.
    pub fn wake(&self) {
        if let Some(fd) = self.target.upgrade() {
            let one = 1u64;
            // SAFETY: eight bytes from a live u64 to a live eventfd. A full
            // counter (it never is) would only mean the wake is already
            // pending.
            unsafe { write(fd.0 .0, (&one as *const u64).cast(), 8) };
        }
    }
}

/// A global the registry announced: its name and version.
#[derive(Clone, Copy, Default)]
struct Global {
    name: u32,
    version: u32,
}

/// A key being held, for auto-repeat, which a Wayland client does itself.
#[derive(Clone, Copy)]
struct Held {
    key: u32,
    next: Instant,
}

/// Everything the listeners write. It lives in its own allocation, reached
/// only through a raw pointer, so a listener running inside
/// [`Window::poll`] never aliases a borrow of the [`Window`].
struct State {
    lib: &'static Lib,
    events: Vec<Event>,
    compositor: Global,
    shm: Global,
    seat: Global,
    wm_base: Global,
    seat_proxy: *mut Proxy,
    pointer: *mut Proxy,
    keyboard: *mut Proxy,
    xdg_surface: *mut Proxy,
    /// The size the next `xdg_surface.configure` applies; 0 leaves it to us.
    pending: (i32, i32),
    size: (u32, u32),
    configured: bool,
    /// Where the pointer is while it is over the window.
    pointer_at: Option<(i32, i32)>,
    button_down: bool,
    last_click: Option<(Instant, i32, i32)>,
    mods: u32,
    locked: u32,
    held: Option<Held>,
    /// Keys a second, and the wait before the first repeat.
    repeat: (u32, Duration),
}

impl State {
    fn push(&mut self, event: Event) {
        // A pointer move behind another replaces it, as on Windows.
        if let (Event::Move(..), Some(last @ Event::Move(..))) = (event, self.events.last_mut()) {
            *last = event;
        } else {
            self.events.push(event);
        }
    }

    /// The events a key going down produces, also used for each repeat.
    fn key_down(&mut self, key: u32) {
        let Some(vk) = keys::vk(key) else { return };
        let alt = self.mods & keys::MOD_ALT != 0;
        // Windows reports a key with Alt held, and F10, as a system key.
        if alt || vk == keys::VK_F10 {
            self.push(Event::SysKey { vk, down: true });
        } else {
            self.push(Event::Key { vk, down: true });
        }
        if let Some(c) = keys::typed(key, self.mods, self.locked) {
            self.push(Event::Char(c));
        }
    }
}

/// A 24.8 fixed-point number to whole pixels.
fn fixed(v: i32) -> i32 {
    v >> 8
}

// ---------------------------------------------------------------- listeners
//
// Each function's parameters are the event's arguments as the protocol XML
// gives them, after the listener's data pointer and the object itself. The
// library calls them from inside `wl_display_dispatch_pending`, on the
// window's thread. `data` is always the window's `*mut State`.

unsafe extern "C" fn registry_global(
    data: *mut c_void,
    _registry: *mut Proxy,
    name: u32,
    interface: *const c_char,
    version: u32,
) {
    // SAFETY: `data` is the live State; `interface` is a C string.
    let (state, interface) = unsafe { (&mut *data.cast::<State>(), CStr::from_ptr(interface)) };
    let global = Global { name, version };
    match interface.to_bytes() {
        b"wl_compositor" => state.compositor = global,
        b"wl_shm" => state.shm = global,
        b"wl_seat" if state.seat.version == 0 => state.seat = global,
        b"xdg_wm_base" => state.wm_base = global,
        _ => {}
    }
}

unsafe extern "C" fn registry_global_remove(_: *mut c_void, _: *mut Proxy, _name: u32) {}

unsafe extern "C" fn wm_base_ping(data: *mut c_void, wm_base: *mut Proxy, serial: u32) {
    // SAFETY: the live State; `pong` takes the serial.
    unsafe {
        let state = &mut *data.cast::<State>();
        state
            .lib
            .request(wm_base, XDG_WM_BASE_PONG, &mut [Arg::uint(serial)]);
    }
}

unsafe extern "C" fn toplevel_configure(
    data: *mut c_void,
    _toplevel: *mut Proxy,
    width: i32,
    height: i32,
    _states: *mut Array,
) {
    // SAFETY: the live State.
    unsafe { (*data.cast::<State>()).pending = (width, height) };
}

unsafe extern "C" fn toplevel_close(data: *mut c_void, _toplevel: *mut Proxy) {
    // SAFETY: the live State.
    unsafe { (*data.cast::<State>()).push(Event::CloseRequested) };
}

unsafe extern "C" fn surface_configure(data: *mut c_void, xdg_surface: *mut Proxy, serial: u32) {
    // SAFETY: the live State; `ack_configure` takes the serial.
    unsafe {
        let state = &mut *data.cast::<State>();
        state.lib.request(
            xdg_surface,
            XDG_SURFACE_ACK_CONFIGURE,
            &mut [Arg::uint(serial)],
        );
        let (w, h) = state.pending;
        if w > 0 && h > 0 && (w as u32, h as u32) != state.size {
            state.size = (w as u32, h as u32);
            state.push(Event::Resized(w as u32, h as u32));
        }
        state.configured = true;
        // The compositor wants a frame for this state.
        state.push(Event::Paint);
    }
}

unsafe extern "C" fn seat_capabilities(data: *mut c_void, seat: *mut Proxy, caps: u32) {
    // SAFETY: the live State; each request creates one object of the
    // interface named, and each listener table matches that interface.
    unsafe {
        let state = &mut *data.cast::<State>();
        let lib = state.lib;
        if caps & 1 != 0 && state.pointer.is_null() {
            state.pointer = lib.create(seat, SEAT_GET_POINTER, lib.pointer, &mut [Arg::new_id()]);
            if !state.pointer.is_null() {
                (lib.proxy_add_listener)(state.pointer, POINTER_LISTENER.0.as_ptr(), data);
            }
        }
        if caps & 2 != 0 && state.keyboard.is_null() {
            state.keyboard =
                lib.create(seat, SEAT_GET_KEYBOARD, lib.keyboard, &mut [Arg::new_id()]);
            if !state.keyboard.is_null() {
                (lib.proxy_add_listener)(state.keyboard, KEYBOARD_LISTENER.0.as_ptr(), data);
            }
        }
    }
}

unsafe extern "C" fn seat_name(_: *mut c_void, _: *mut Proxy, _name: *const c_char) {}

unsafe extern "C" fn pointer_enter(
    data: *mut c_void,
    _pointer: *mut Proxy,
    _serial: u32,
    _surface: *mut Proxy,
    x: i32,
    y: i32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    state.pointer_at = Some((fixed(x), fixed(y)));
    state.push(Event::Move(fixed(x), fixed(y)));
}

unsafe extern "C" fn pointer_leave(
    data: *mut c_void,
    _pointer: *mut Proxy,
    _serial: u32,
    _surface: *mut Proxy,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    state.pointer_at = None;
    state.push(Event::Leave);
}

unsafe extern "C" fn pointer_motion(
    data: *mut c_void,
    _pointer: *mut Proxy,
    _time: u32,
    x: i32,
    y: i32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    let (x, y) = (fixed(x), fixed(y));
    state.pointer_at = Some((x, y));
    if state.button_down {
        state.push(Event::Drag(x, y));
    } else {
        state.push(Event::Move(x, y));
    }
}

unsafe extern "C" fn pointer_button(
    data: *mut c_void,
    _pointer: *mut Proxy,
    _serial: u32,
    _time: u32,
    button: u32,
    pressed: u32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    if button != BTN_LEFT {
        return;
    }
    let (x, y) = state.pointer_at.unwrap_or((0, 0));
    if pressed == 0 {
        state.button_down = false;
        state.push(Event::Release(x, y));
        return;
    }
    state.button_down = true;
    let now = Instant::now();
    let double = state.last_click.is_some_and(|(at, lx, ly)| {
        now.duration_since(at) <= DOUBLE_CLICK
            && (x - lx).abs() <= DOUBLE_CLICK_PX
            && (y - ly).abs() <= DOUBLE_CLICK_PX
    });
    if double {
        // It comes instead of the second click, and a third starts over.
        state.last_click = None;
        state.push(Event::DoubleClick(x, y));
    } else {
        state.last_click = Some((now, x, y));
        state.push(Event::Click(x, y));
    }
}

unsafe extern "C" fn pointer_axis(
    data: *mut c_void,
    _pointer: *mut Proxy,
    _time: u32,
    axis: u32,
    value: i32,
) {
    // Vertical only. A wheel notch is 10 units here, positive toward the
    // user; Windows' is 120, positive away.
    if axis == 0 {
        let delta = -(value as i64 * 12 / 256) as i32;
        if delta != 0 {
            // SAFETY: the live State.
            unsafe { (*data.cast::<State>()).push(Event::Wheel(delta)) };
        }
    }
}

unsafe extern "C" fn pointer_frame(_: *mut c_void, _: *mut Proxy) {}
unsafe extern "C" fn pointer_axis_source(_: *mut c_void, _: *mut Proxy, _source: u32) {}
unsafe extern "C" fn pointer_axis_stop(_: *mut c_void, _: *mut Proxy, _time: u32, _axis: u32) {}
unsafe extern "C" fn pointer_axis_discrete(_: *mut c_void, _: *mut Proxy, _axis: u32, _n: i32) {}

unsafe extern "C" fn keyboard_keymap(
    _: *mut c_void,
    _keyboard: *mut Proxy,
    _format: u32,
    fd: c_int,
    _size: u32,
) {
    // The keymap is not read yet (`keys.rs`); the descriptor is ours to close.
    // SAFETY: a descriptor the compositor handed us.
    unsafe { close(fd) };
}

unsafe extern "C" fn keyboard_enter(
    _: *mut c_void,
    _keyboard: *mut Proxy,
    _serial: u32,
    _surface: *mut Proxy,
    _keys: *mut Array,
) {
}

unsafe extern "C" fn keyboard_leave(
    data: *mut c_void,
    _keyboard: *mut Proxy,
    _serial: u32,
    _surface: *mut Proxy,
) {
    // A key held while focus leaves is released elsewhere: stop repeating.
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    state.held = None;
    state.mods = 0;
    MODS.store(0, Ordering::Relaxed);
}

unsafe extern "C" fn keyboard_key(
    data: *mut c_void,
    _keyboard: *mut Proxy,
    _serial: u32,
    _time: u32,
    key: u32,
    pressed: u32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    if pressed == 1 {
        state.key_down(key);
        let (rate, delay) = state.repeat;
        state.held = (rate > 0 && !keys::is_modifier(key)).then(|| Held {
            key,
            next: Instant::now() + delay,
        });
    } else {
        if state.held.is_some_and(|h| h.key == key) {
            state.held = None;
        }
        if let Some(vk) = keys::vk(key) {
            // A system key if Alt is still held, or is the key coming up.
            let alt = state.mods & keys::MOD_ALT != 0 || vk == keys::VK_MENU;
            if alt || vk == keys::VK_F10 {
                state.push(Event::SysKey { vk, down: false });
            } else {
                state.push(Event::Key { vk, down: false });
            }
        }
    }
}

unsafe extern "C" fn keyboard_modifiers(
    data: *mut c_void,
    _keyboard: *mut Proxy,
    _serial: u32,
    depressed: u32,
    latched: u32,
    locked: u32,
    _group: u32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    state.mods = depressed | latched;
    state.locked = locked;
    MODS.store(state.mods, Ordering::Relaxed);
}

unsafe extern "C" fn keyboard_repeat_info(
    data: *mut c_void,
    _keyboard: *mut Proxy,
    rate: i32,
    delay: i32,
) {
    // SAFETY: the live State.
    let state = unsafe { &mut *data.cast::<State>() };
    state.repeat = (
        rate.max(0) as u32,
        Duration::from_millis(delay.max(0) as u64),
    );
}

unsafe extern "C" fn buffer_release(data: *mut c_void, _buffer: *mut Proxy) {
    // SAFETY: `data` is the buffer's own flag, alive as long as the buffer.
    unsafe { (*data.cast::<AtomicBool>()).store(false, Ordering::Relaxed) };
}

/// A listener: one function for each event of an interface, in order.
struct Listener<const N: usize>([*const c_void; N]);
// SAFETY: function addresses, never written.
unsafe impl<const N: usize> Sync for Listener<N> {}

static REGISTRY_LISTENER: Listener<2> = Listener([
    registry_global as *const c_void,
    registry_global_remove as *const c_void,
]);
static WM_BASE_LISTENER: Listener<1> = Listener([wm_base_ping as *const c_void]);
static XDG_SURFACE_LISTENER: Listener<1> = Listener([surface_configure as *const c_void]);
static TOPLEVEL_LISTENER: Listener<2> = Listener([
    toplevel_configure as *const c_void,
    toplevel_close as *const c_void,
]);
static SEAT_LISTENER: Listener<2> = Listener([
    seat_capabilities as *const c_void,
    seat_name as *const c_void,
]);
/// The pointer's events through version 5, the version the seat is bound at.
static POINTER_LISTENER: Listener<9> = Listener([
    pointer_enter as *const c_void,
    pointer_leave as *const c_void,
    pointer_motion as *const c_void,
    pointer_button as *const c_void,
    pointer_axis as *const c_void,
    pointer_frame as *const c_void,
    pointer_axis_source as *const c_void,
    pointer_axis_stop as *const c_void,
    pointer_axis_discrete as *const c_void,
]);
/// The keyboard's events through version 5.
static KEYBOARD_LISTENER: Listener<6> = Listener([
    keyboard_keymap as *const c_void,
    keyboard_enter as *const c_void,
    keyboard_leave as *const c_void,
    keyboard_key as *const c_void,
    keyboard_modifiers as *const c_void,
    keyboard_repeat_info as *const c_void,
]);
static BUFFER_LISTENER: Listener<1> = Listener([buffer_release as *const c_void]);

/// The seat is bound no higher than this: the listeners above cover it.
const SEAT_VERSION: u32 = 5;
/// For `wl_surface.damage_buffer`.
const COMPOSITOR_VERSION: u32 = 4;

fn other(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Other, message.into())
}

/// A top-level Wayland window (`xdg_toplevel`).
///
/// The window is drawn by whoever holds it: [`CpuPresenter`], or a Vulkan
/// swapchain on [`Window::display`] and [`Window::surface`]. It appears on
/// screen with its first frame, as Wayland windows do.
///
/// **Not built yet:** a title bar where the desktop draws none (GNOME; the
/// window is then a bare rectangle), sizes on a scaled display (the surface
/// is one pixel a point, so a desktop scaled to 200 % enlarges it), the
/// clipboard, dropped files, pointer shapes, and any keyboard layout but US
/// (`keys.rs`).
///
/// A window is used from the thread that created it; dropping it closes it.
pub struct Window {
    lib: &'static Lib,
    display: *mut Proxy,
    registry: *mut Proxy,
    compositor: *mut Proxy,
    shm: *mut Proxy,
    wm_base: *mut Proxy,
    surface: *mut Proxy,
    toplevel: *mut Proxy,
    state: *mut State,
    wake: Arc<WakeFd>,
    /// Dropped with the window: a presenter holds a weak one, to know
    /// whether its buffers' window is still there.
    alive: Arc<()>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("size", &self.client_size())
            .finish()
    }
}

impl Window {
    /// Open a window `width x height` pixels, titled `title`, on the
    /// compositor `WAYLAND_DISPLAY` names. Fails when the library or a
    /// compositor is missing, or the compositor lacks `xdg_wm_base`.
    pub fn new(title: &str, width: u32, height: u32) -> io::Result<Window> {
        let lib = lib().map_err(other)?;
        // SAFETY: a null name means "the display the environment names".
        let display = unsafe { (lib.display_connect)(std::ptr::null()) };
        if display.is_null() {
            return Err(other(
                "no Wayland compositor to connect to (is WAYLAND_DISPLAY set?)",
            ));
        }
        // SAFETY: plain eventfd creation; -1 is checked.
        let wake = unsafe { eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK) };
        if wake < 0 {
            let e = io::Error::last_os_error();
            // SAFETY: the display just connected, with nothing on it.
            unsafe { (lib.display_disconnect)(display) };
            return Err(e);
        }
        let state = Box::into_raw(Box::new(State {
            lib,
            events: Vec::new(),
            compositor: Global::default(),
            shm: Global::default(),
            seat: Global::default(),
            wm_base: Global::default(),
            seat_proxy: std::ptr::null_mut(),
            pointer: std::ptr::null_mut(),
            keyboard: std::ptr::null_mut(),
            xdg_surface: std::ptr::null_mut(),
            pending: (0, 0),
            size: (width.max(1), height.max(1)),
            configured: false,
            pointer_at: None,
            button_down: false,
            last_click: None,
            mods: 0,
            locked: 0,
            held: None,
            repeat: (25, Duration::from_millis(600)),
        }));
        // Built up field by field, so `Drop` releases whatever exists if a
        // later step fails.
        let mut window = Window {
            lib,
            display,
            registry: std::ptr::null_mut(),
            compositor: std::ptr::null_mut(),
            shm: std::ptr::null_mut(),
            wm_base: std::ptr::null_mut(),
            surface: std::ptr::null_mut(),
            toplevel: std::ptr::null_mut(),
            state,
            wake: Arc::new(WakeFd(Fd(wake))),
            alive: Arc::new(()),
        };
        window.open(title)?;
        Ok(window)
    }

    fn open(&mut self, title: &str) -> io::Result<()> {
        let lib = self.lib;
        let data = self.state.cast::<c_void>();
        // SAFETY: every proxy passed is live, each request's arguments
        // follow its signature in the protocol XML, each listener table
        // matches its interface, and `data` outlives every proxy (`Drop`).
        unsafe {
            self.registry = lib.create(
                self.display,
                DISPLAY_GET_REGISTRY,
                lib.registry,
                &mut [Arg::new_id()],
            );
            if self.registry.is_null() {
                return Err(other("the Wayland connection failed at the registry"));
            }
            (lib.proxy_add_listener)(self.registry, REGISTRY_LISTENER.0.as_ptr(), data);
            self.roundtrip()?;

            let globals = &*self.state;
            let (compositor, shm, seat, wm_base) = (
                globals.compositor,
                globals.shm,
                globals.seat,
                globals.wm_base,
            );
            for (global, name) in [
                (compositor, "wl_compositor"),
                (shm, "wl_shm"),
                (wm_base, "xdg_wm_base"),
            ] {
                if global.version == 0 {
                    return Err(other(format!("the compositor has no {name}")));
                }
            }
            self.compositor = lib.bind(
                self.registry,
                compositor.name,
                lib.compositor,
                compositor.version.min(COMPOSITOR_VERSION),
            );
            self.shm = lib.bind(self.registry, shm.name, lib.shm, 1);
            self.wm_base = lib.bind(self.registry, wm_base.name, &XDG_WM_BASE, 1);
            (lib.proxy_add_listener)(self.wm_base, WM_BASE_LISTENER.0.as_ptr(), data);
            if seat.version > 0 {
                let proxy = lib.bind(
                    self.registry,
                    seat.name,
                    lib.seat,
                    seat.version.min(SEAT_VERSION),
                );
                (lib.proxy_add_listener)(proxy, SEAT_LISTENER.0.as_ptr(), data);
                (*self.state).seat_proxy = proxy;
            }

            self.surface = lib.create(
                self.compositor,
                COMPOSITOR_CREATE_SURFACE,
                lib.surface,
                &mut [Arg::new_id()],
            );
            let xdg_surface = lib.create(
                self.wm_base,
                XDG_WM_BASE_GET_XDG_SURFACE,
                &XDG_SURFACE,
                &mut [Arg::new_id(), Arg::ptr(self.surface)],
            );
            if self.surface.is_null() || xdg_surface.is_null() {
                return Err(other("the Wayland connection failed creating the surface"));
            }
            (*self.state).xdg_surface = xdg_surface;
            (lib.proxy_add_listener)(xdg_surface, XDG_SURFACE_LISTENER.0.as_ptr(), data);
            self.toplevel = lib.create(
                xdg_surface,
                XDG_SURFACE_GET_TOPLEVEL,
                &XDG_TOPLEVEL,
                &mut [Arg::new_id()],
            );
            if self.toplevel.is_null() {
                return Err(other("the Wayland connection failed creating the window"));
            }
            (lib.proxy_add_listener)(self.toplevel, TOPLEVEL_LISTENER.0.as_ptr(), data);
        }
        self.set_title(title);
        self.set_app_id("foldwave");
        // A commit with no buffer asks for the first configure, which must
        // be acknowledged before anything is drawn.
        self.commit();
        while !self.state().configured {
            self.roundtrip()?;
        }
        Ok(())
    }

    fn state(&self) -> &State {
        // SAFETY: allocated in `new`, freed in `drop`; no listener runs
        // while this borrow is used (they run only inside dispatch calls).
        unsafe { &*self.state }
    }

    fn roundtrip(&self) -> io::Result<()> {
        // SAFETY: a live display.
        if unsafe { (self.lib.display_roundtrip)(self.display) } < 0 {
            return Err(other("the Wayland connection was lost"));
        }
        Ok(())
    }

    fn commit(&self) {
        // SAFETY: a live surface; `commit` takes no arguments.
        unsafe { self.lib.request(self.surface, SURFACE_COMMIT, &mut []) };
    }

    fn set_string(&self, opcode: u32, text: &str) {
        // A NUL inside the text would end it early: drop any.
        let text: String = text.chars().filter(|c| *c != '\0').collect();
        let Ok(text) = CString::new(text) else { return };
        // SAFETY: a live toplevel; both requests take one string.
        unsafe {
            self.lib
                .request(self.toplevel, opcode, &mut [Arg::ptr(text.as_ptr())])
        };
    }

    /// Set the window's title.
    pub fn set_title(&self, title: &str) {
        self.set_string(XDG_TOPLEVEL_SET_TITLE, title);
    }

    /// The name a desktop groups the window under and finds its icon by.
    fn set_app_id(&self, id: &str) {
        self.set_string(XDG_TOPLEVEL_SET_APP_ID, id);
    }

    /// The window's size in pixels.
    pub fn client_size(&self) -> (u32, u32) {
        self.state().size
    }

    /// Make the window `width x height`. A Wayland window is as large as
    /// what it draws, so this takes effect with the next frame; a maximized
    /// or tiled window is put back to the compositor's size by its next
    /// configure.
    pub fn set_client_size(&self, width: u32, height: u32) -> io::Result<()> {
        if width == 0 || height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a window cannot be empty",
            ));
        }
        // SAFETY: the State is live and no listener is running; the request
        // takes four integers.
        unsafe {
            let state = &mut *self.state;
            if state.size != (width, height) {
                state.size = (width, height);
                state.push(Event::Resized(width, height));
            }
            self.lib.request(
                state.xdg_surface,
                XDG_SURFACE_SET_WINDOW_GEOMETRY,
                &mut [
                    Arg::int(0),
                    Arg::int(0),
                    Arg::int(width as i32),
                    Arg::int(height as i32),
                ],
            );
        }
        Ok(())
    }

    /// The pointer's place in the window, while it is over it.
    pub fn pointer(&self) -> Option<(i32, i32)> {
        self.state().pointer_at
    }

    /// Dots per inch. Always 96: scaled displays are not handled yet.
    pub fn dpi(&self) -> u32 {
        96
    }

    /// The `wl_display`, for a Vulkan surface.
    pub fn display(&self) -> *mut c_void {
        self.display
    }

    /// The `wl_surface`, for a Vulkan surface.
    pub fn surface(&self) -> *mut c_void {
        self.surface
    }

    /// Does nothing yet: pointer shapes need the cursor-shape protocol or a
    /// cursor theme.
    pub fn set_cursor(&self, _cursor: Cursor) {}

    /// Does nothing: a Wayland window takes its icon from the desktop entry
    /// its application id names, not from the window.
    pub fn set_icon(&mut self, _images: &[(u32, Vec<u8>)]) -> io::Result<()> {
        Ok(())
    }

    /// Returns `false`: there is no taskbar button to flash.
    pub fn flash_taskbar(&self) -> bool {
        false
    }

    /// Returns `false`: there is no taskbar progress.
    pub fn set_progress(&mut self, _progress: Option<TaskbarProgress>) -> bool {
        false
    }

    /// Returns `false`: desktop notifications are not built on Linux yet.
    pub fn notify(&mut self, _title: &str, _text: &str) -> bool {
        false
    }

    /// Returns `false`: the frame, where there is one, is the desktop's.
    pub fn set_frame_colors(&self, _caption: u32, _text: u32, _border: u32) -> bool {
        false
    }

    /// Does nothing, as [`Window::set_frame_colors`] set nothing.
    pub fn reset_frame_colors(&self) {}

    /// Fails as unsupported: the clipboard is not built on Linux yet.
    pub fn set_clipboard(&self, _text: &str) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the clipboard is not built on Wayland yet",
        ))
    }

    /// `None`: the clipboard is not built on Linux yet.
    pub fn clipboard(&self) -> io::Result<Option<String>> {
        Ok(None)
    }

    /// Empty, as [`Window::clipboard`].
    pub fn clipboard_files(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// `false`, as [`Window::clipboard`].
    pub fn clipboard_has_image(&self) -> bool {
        false
    }

    /// Empty: dropped files are not built on Linux yet.
    pub fn take_dropped(&self) -> Vec<String> {
        Vec::new()
    }

    /// A handle that wakes [`Window::wait`] from any thread.
    pub fn waker(&self) -> Waker {
        Waker {
            target: Arc::downgrade(&self.wake),
        }
    }

    /// Send what is queued, read what has arrived, and append the events to
    /// `events`. Never blocks.
    pub fn poll(&mut self, events: &mut Vec<Event>) {
        self.pump(Some(Duration::ZERO));
        self.take(events);
    }

    /// As [`Window::poll`], but sleep until something happens or `timeout`
    /// passes (`None`: until something happens). A held key's repeat and a
    /// [`Waker`] both end the sleep.
    pub fn wait(&mut self, timeout: Option<Duration>, events: &mut Vec<Event>) {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            let now = Instant::now();
            let left = deadline.map(|d| d.saturating_duration_since(now));
            // Wake for the next repeat of a held key, if that comes first.
            let repeat = self
                .state()
                .held
                .map(|h| h.next.saturating_duration_since(now));
            let sleep = match (left, repeat) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            self.pump(sleep);
            if !self.state().events.is_empty() || left.is_some_and(|l| l.is_zero()) {
                break;
            }
        }
        self.take(events);
    }

    fn take(&mut self, events: &mut Vec<Event>) {
        // SAFETY: the State is live and no listener is running.
        events.append(unsafe { &mut (*self.state).events });
    }

    /// One turn of the loop: flush, sleep at most `sleep` for the
    /// compositor or a waker, read and dispatch, and repeat a held key.
    fn pump(&mut self, sleep: Option<Duration>) {
        let lib = self.lib;
        // SAFETY: the display is live; the read is announced and then either
        // made or cancelled, as the library requires of every reader; the
        // poll set and the timeout outlive the call.
        unsafe {
            // Events already read must be dispatched before a read begins.
            while (lib.display_prepare_read)(self.display) != 0 {
                (lib.display_dispatch_pending)(self.display);
            }
            (lib.display_flush)(self.display);
            // Something already queued (a resize this call made, say) must
            // not wait.
            let sleep = if (*self.state).events.is_empty() {
                sleep
            } else {
                Some(Duration::ZERO)
            };
            let timeout = sleep.map(|d| Timespec {
                sec: d.as_secs() as c_long,
                nsec: d.subsec_nanos() as c_long,
            });
            let mut fds = [
                PollFd {
                    fd: (lib.display_get_fd)(self.display),
                    events: POLLIN,
                    revents: 0,
                },
                PollFd {
                    fd: self.wake.0 .0,
                    events: POLLIN,
                    revents: 0,
                },
            ];
            let ready = ppoll(
                fds.as_mut_ptr(),
                2,
                timeout
                    .as_ref()
                    .map_or(std::ptr::null(), |t| t as *const Timespec),
                std::ptr::null(),
            );
            if ready > 0 && fds[0].revents != 0 {
                (lib.display_read_events)(self.display);
            } else {
                (lib.display_cancel_read)(self.display);
            }
            (lib.display_dispatch_pending)(self.display);
            if ready > 0 && fds[1].revents != 0 {
                let mut count = 0u64;
                read(fds[1].fd, (&mut count as *mut u64).cast(), 8);
                (*self.state).push(Event::Woken);
            }
            // A held key repeats on our clock.
            let state = &mut *self.state;
            let now = Instant::now();
            while let Some(held) = state.held.filter(|h| h.next <= now) {
                state.key_down(held.key);
                let step = Duration::from_millis(1000 / state.repeat.0.max(1) as u64);
                state.held = Some(Held {
                    key: held.key,
                    next: (held.next + step).max(now),
                });
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let lib = self.lib;
        // SAFETY: each proxy is destroyed once, children before parents, and
        // the State is freed only after the display, so no listener can run
        // on freed memory.
        unsafe {
            let state = &mut *self.state;
            for proxy in [state.keyboard, state.pointer, state.seat_proxy] {
                if !proxy.is_null() {
                    (lib.proxy_destroy)(proxy);
                }
            }
            if !self.toplevel.is_null() {
                lib.destroy(self.toplevel, XDG_TOPLEVEL_DESTROY);
            }
            if !state.xdg_surface.is_null() {
                lib.destroy(state.xdg_surface, XDG_SURFACE_DESTROY);
            }
            if !self.surface.is_null() {
                lib.destroy(self.surface, SURFACE_DESTROY);
            }
            if !self.wm_base.is_null() {
                lib.destroy(self.wm_base, XDG_WM_BASE_DESTROY);
            }
            for proxy in [self.shm, self.compositor, self.registry] {
                if !proxy.is_null() {
                    (lib.proxy_destroy)(proxy);
                }
            }
            (lib.display_flush)(self.display);
            (lib.display_disconnect)(self.display);
            drop(Box::from_raw(self.state));
        }
    }
}

/// One shared-memory buffer the compositor reads a frame from.
struct Buffer {
    proxy: *mut Proxy,
    pixels: *mut u32,
    bytes: usize,
    width: u32,
    height: u32,
    /// Set when the buffer is handed over, cleared by its `release` event.
    busy: *mut AtomicBool,
}

/// Shows [`CpuRenderer`] frames in a [`Window`] through shared memory
/// (`wl_shm`).
///
/// The framebuffer is already `0xAARRGGBB`, which is the bytes of
/// `XRGB8888`, so pixels are copied as they are. The compositor keeps a
/// buffer until it has finished with it, so frames alternate between a few;
/// each present copies the whole frame into a free one and tells the
/// compositor which rows changed.
///
/// A presenter belongs to the window it first presents to. It may outlive
/// that window: its buffers then go with the connection.
#[derive(Default)]
pub struct CpuPresenter {
    /// The library and the window the buffers were made on.
    owner: Option<(&'static Lib, Weak<()>)>,
    buffers: Vec<Buffer>,
}

impl std::fmt::Debug for CpuPresenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPresenter")
            .field("buffers", &self.buffers.len())
            .finish()
    }
}

impl CpuPresenter {
    /// A presenter.
    pub fn new() -> CpuPresenter {
        CpuPresenter::default()
    }

    fn make(window: &Window, width: u32, height: u32) -> io::Result<Buffer> {
        let lib = window.lib;
        let bytes = width as usize * height as usize * 4;
        // SAFETY: a NUL-terminated name; the descriptor is checked, sized
        // and mapped, and closed by `Fd` once the compositor has its copy;
        // each request's arguments follow its signature.
        unsafe {
            let fd = memfd_create(b"foldwave-frame\0".as_ptr().cast(), MFD_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = Fd(fd);
            if ftruncate(fd.0, bytes as i64) != 0 {
                return Err(io::Error::last_os_error());
            }
            let map = mmap(
                std::ptr::null_mut(),
                bytes,
                PROT_READ_WRITE,
                MAP_SHARED,
                fd.0,
                0,
            );
            if map as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            let pool = lib.create(
                window.shm,
                SHM_CREATE_POOL,
                lib.shm_pool,
                &mut [Arg::new_id(), Arg::int(fd.0), Arg::int(bytes as i32)],
            );
            let proxy = if pool.is_null() {
                pool
            } else {
                let buffer = lib.create(
                    pool,
                    SHM_POOL_CREATE_BUFFER,
                    lib.buffer,
                    &mut [
                        Arg::new_id(),
                        Arg::int(0),
                        Arg::int(width as i32),
                        Arg::int(height as i32),
                        Arg::int(width as i32 * 4),
                        Arg::uint(SHM_FORMAT_XRGB8888),
                    ],
                );
                // The buffer keeps the pool's memory alive.
                lib.destroy(pool, SHM_POOL_DESTROY);
                buffer
            };
            if proxy.is_null() {
                munmap(map, bytes);
                return Err(other("the Wayland connection failed creating a buffer"));
            }
            let busy = Box::into_raw(Box::new(AtomicBool::new(false)));
            (lib.proxy_add_listener)(proxy, BUFFER_LISTENER.0.as_ptr(), busy.cast());
            Ok(Buffer {
                proxy,
                pixels: map.cast(),
                bytes,
                width,
                height,
                busy,
            })
        }
    }

    /// How many buffers the presenter holds, for tests.
    #[cfg(test)]
    pub(crate) fn buffers_in_use(&self) -> usize {
        self.buffers.len()
    }

    /// Release a buffer: the compositor keeps its own reference to the
    /// memory for as long as it still shows it. With `lib` `None` the
    /// window has closed: its connection took the buffer object with it,
    /// and only our own memory is left to free.
    ///
    /// # Safety
    /// `buffer` came from [`CpuPresenter::make`]; `lib` is `Some` only
    /// while that window is open.
    unsafe fn free(lib: Option<&Lib>, buffer: Buffer) {
        // SAFETY: the caller's contract; each resource is released once. No
        // listener can run for a closed window, so the flag is ours alone.
        unsafe {
            if let Some(lib) = lib {
                lib.destroy(buffer.proxy, BUFFER_DESTROY);
            }
            munmap(buffer.pixels.cast(), buffer.bytes);
            drop(Box::from_raw(buffer.busy));
        }
    }

    /// The library, while the window the buffers were made on is open.
    fn open_on(&self) -> Option<&'static Lib> {
        let (lib, alive) = self.owner.as_ref()?;
        alive.upgrade().map(|_| *lib)
    }

    /// Show the framebuffer in the window, telling the compositor that rows
    /// `rows` (clamped to its height) changed.
    pub fn present(
        &mut self,
        window: &Window,
        cpu: &CpuRenderer,
        rows: Range<u32>,
    ) -> io::Result<()> {
        let (w, h) = (cpu.width(), cpu.height());
        let start = rows.start.min(h);
        let end = rows.end.min(h);
        if start >= end || w == 0 {
            return Ok(());
        }
        let lib = window.lib;
        // Buffers made on another window, or one since closed, are no use
        // on this one.
        let same = self
            .owner
            .as_ref()
            .is_some_and(|(_, alive)| alive.ptr_eq(&Arc::downgrade(&window.alive)));
        if !same {
            let old = self.open_on();
            for buffer in self.buffers.drain(..) {
                // SAFETY: made by this presenter; `old` is `Some` only
                // while their window is open.
                unsafe { CpuPresenter::free(old, buffer) };
            }
            self.owner = Some((lib, Arc::downgrade(&window.alive)));
        }
        // Buffers of an old size go as soon as the compositor lets go.
        let mut i = 0;
        while i < self.buffers.len() {
            let b = &self.buffers[i];
            // SAFETY: the flag lives as long as its buffer.
            let busy = unsafe { (*b.busy).load(Ordering::Relaxed) };
            if (b.width, b.height) != (w, h) && !busy {
                // SAFETY: made on this window, which is open.
                unsafe { CpuPresenter::free(Some(lib), self.buffers.swap_remove(i)) };
            } else {
                i += 1;
            }
        }
        let free = self.buffers.iter().position(|b| {
            // SAFETY: as above.
            (b.width, b.height) == (w, h) && !unsafe { (*b.busy).load(Ordering::Relaxed) }
        });
        let index = match free {
            Some(index) => index,
            None => {
                self.buffers.push(CpuPresenter::make(window, w, h)?);
                self.buffers.len() - 1
            }
        };
        let buffer = &self.buffers[index];
        // SAFETY: the mapping holds `w * h` pixels, as does the framebuffer;
        // the compositor is not reading a buffer it has released; each
        // request's arguments follow its signature.
        unsafe {
            std::ptr::copy_nonoverlapping(cpu.pixels().as_ptr(), buffer.pixels, (w * h) as usize);
            (*buffer.busy).store(true, Ordering::Relaxed);
            lib.request(
                window.surface,
                SURFACE_ATTACH,
                &mut [Arg::ptr(buffer.proxy), Arg::int(0), Arg::int(0)],
            );
            let damage = if (lib.proxy_get_version)(window.surface) >= 4 {
                SURFACE_DAMAGE_BUFFER
            } else {
                SURFACE_DAMAGE
            };
            lib.request(
                window.surface,
                damage,
                &mut [
                    Arg::int(0),
                    Arg::int(start as i32),
                    Arg::int(w as i32),
                    Arg::int((end - start) as i32),
                ],
            );
            window.commit();
            (lib.display_flush)(window.display);
        }
        Ok(())
    }
}

impl Drop for CpuPresenter {
    fn drop(&mut self) {
        let lib = self.open_on();
        for buffer in self.buffers.drain(..) {
            // SAFETY: made by this presenter; `lib` is `Some` only while
            // their window is open.
            unsafe { CpuPresenter::free(lib, buffer) };
        }
    }
}

/// `None`: Wayland tells a client when to draw, not the display's rate.
pub fn display_refresh_hz() -> Option<u32> {
    None
}

/// Returns `false`: an unprivileged Linux thread cannot raise its priority.
pub fn raise_thread_priority() -> bool {
    false
}

/// Whether Shift is down, as the keyboard last reported it to a window.
pub fn shift_down() -> bool {
    MODS.load(Ordering::Relaxed) & keys::MOD_SHIFT != 0
}

/// Whether Ctrl is down, as [`shift_down`].
pub fn ctrl_down() -> bool {
    MODS.load(Ordering::Relaxed) & keys::MOD_CTRL != 0
}

/// Whether Alt is down, as [`shift_down`].
pub fn alt_down() -> bool {
    MODS.load(Ordering::Relaxed) & keys::MOD_ALT != 0
}

/// Write `text` to standard error, which a terminal that started foldwave
/// shows. Always `true`.
pub fn write_parent_console(text: &str) -> bool {
    eprint!("{text}");
    true
}

/// Returns `false`: there is no system-wide environment to announce to.
pub fn announce_environment_change() -> bool {
    false
}
