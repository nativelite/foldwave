//! [`Window`]: a top-level Wayland window with a non-blocking event queue,
//! and [`CpuPresenter`], which shows a [`CpuRenderer`] framebuffer in it.
//!
//! The method names follow `win::Window`, so a caller is written once. What
//! Wayland has no counterpart for says so and does nothing, as documented on
//! each method.
//!
//! The protocol is spoken by `nativelite-wayland` and the keyboard read by
//! `nativelite-xkb`: no system library is loaded. One thread owns the window
//! and its connection. Requests go out when [`Window::poll`],
//! [`Window::wait`] or a present flushes them; events are read there, one at
//! a time, and turned into [`Event`]s.

use super::keys;
use super::linux_dmabuf::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_v1, ZwpLinuxDmabufV1};
use crate::cpu::CpuRenderer;
pub use crate::event::{Cursor, Event, TaskbarProgress};
use crate::vk::{Dmabuf, FrameSink};
use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_long, c_void};
use std::fs::File;
use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
use wayland::protocol::wayland::{
    wl_buffer, wl_data_device, wl_data_offer, wl_data_source, wl_keyboard, wl_pointer, wl_registry,
    wl_seat, wl_shm, WlBuffer, WlCompositor, WlDataDevice, WlDataDeviceManager, WlDataOffer,
    WlDataSource, WlKeyboard, WlPointer, WlRegistry, WlSeat, WlShm, WlSurface,
};
use wayland::protocol::xdg_shell::{
    xdg_surface, xdg_toplevel, xdg_wm_base, XdgSurface, XdgToplevel, XdgWmBase,
};
use wayland::{Connection, Proxy};
use xkb::compose;

extern "C" {
    fn ppoll(fds: *mut PollFd, count: u64, timeout: *const Timespec, mask: *const c_void) -> c_int;
    fn memfd_create(name: *const c_char, flags: u32) -> c_int;
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
const MFD_CLOEXEC: u32 = 1;
const BTN_LEFT: u32 = 0x110;
/// The keysyms of Delete and the keypad's: they have a character (0x7F),
/// but Windows reports the key alone, and fwterm handles it as a key.
const DELETE_SYMS: [u32; 2] = [0xffff, 0xff9f];
/// Two clicks this close in time and place are a double click, as Windows'
/// defaults have it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_PX: i32 = 4;
/// The versions bound, no higher than this code handles.
const COMPOSITOR_VERSION: u32 = 4;
const SEAT_VERSION: u32 = 5;
const DATA_DEVICE_VERSION: u32 = 3;
/// The names text goes by on the clipboard, best first: UTF-8 by its
/// proper name, then what older programs ask for.
const TEXT_TYPES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];
/// The most clipboard text read, and the longest wait for it.
const CLIPBOARD_LIMIT: u64 = 64 << 20;
const CLIPBOARD_WAIT: Duration = Duration::from_secs(2);

/// Shift, Ctrl and Alt as the keyboard last reported them, for
/// [`shift_down`] and the others, which take no window.
static MODS: AtomicU32 = AtomicU32::new(0);

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::Other, e.to_string())
}

/// Wakes a [`Window`] blocked in [`Window::wait`] from any thread; the window
/// then reports [`Event::Woken`]. Waking a dropped window does nothing.
#[derive(Clone)]
pub struct Waker {
    target: Weak<UnixStream>,
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Waker").finish_non_exhaustive()
    }
}

impl Waker {
    /// Queue [`Event::Woken`]. Cheap and non-blocking.
    pub fn wake(&self) {
        if let Some(stream) = self.target.upgrade() {
            // A full socket means a wake is already waiting to be read.
            let _ = (&*stream).write(&[1]);
        }
    }
}

/// A key being held, for auto-repeat, which a Wayland client does itself.
#[derive(Clone, Copy)]
struct Held {
    key: u32,
    next: Instant,
}

/// The keyboard: the compositor's keymap and the state it reports.
struct Keyboard {
    /// `None` until the compositor sends a keymap this code can read; keys
    /// then type a US layout (`keys::typed`).
    state: Option<xkb::State>,
    /// Dead keys and Compose sequences, from the locale's Compose file.
    compose: Option<compose::State>,
    /// Modifiers as [`keys`] names them, whichever way they were learned.
    mods: u32,
    locked: u32,
    held: Option<Held>,
    /// Keys a second, and the wait before the first repeat.
    repeat: (u32, Duration),
}

impl Keyboard {
    fn new() -> Keyboard {
        Keyboard {
            state: None,
            compose: None,
            mods: 0,
            locked: 0,
            held: None,
            repeat: (25, Duration::from_millis(600)),
        }
    }

    /// Take the keymap the compositor sent: `size` bytes of text in `fd`.
    fn load(&mut self, fd: OwnedFd, size: u32) {
        let mut text = vec![0u8; size as usize];
        let file = File::from(fd);
        // The descriptor is a shared-memory file; one that will not read at
        // an offset is read from where it stands.
        let read = file
            .read_exact_at(&mut text, 0)
            .or_else(|_| (&file).read_exact(&mut text));
        self.state = read
            .ok()
            .and_then(|()| xkb::Keymap::from_bytes(&text).ok())
            .map(xkb::State::new);
        if self.compose.is_none() {
            let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
                .unwrap_or_else(|| "C".into());
            self.compose = compose::Table::from_locale(&locale, &compose::Paths::from_env())
                .ok()
                .map(compose::State::new);
        }
    }

    /// `wl_keyboard.modifiers`.
    fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        match &mut self.state {
            Some(state) => {
                state.update_mask(depressed, latched, locked, 0, 0, group);
                let on = |name, part| state.mod_name_is_active(name, part);
                let bit = |name, bit| {
                    if on(name, xkb::Component::Effective) {
                        bit
                    } else {
                        0
                    }
                };
                self.mods = bit("Shift", keys::MOD_SHIFT)
                    | bit("Control", keys::MOD_CTRL)
                    | bit("Mod1", keys::MOD_ALT);
                self.locked = if on("Lock", xkb::Component::Locked) {
                    keys::MOD_CAPS
                } else {
                    0
                };
            }
            None => {
                self.mods = depressed | latched;
                self.locked = locked;
            }
        }
        MODS.store(self.mods, Ordering::Relaxed);
    }

    /// The virtual-key code of a Linux key code.
    fn vk(&self, key: u32) -> Option<u32> {
        let first = self.state.as_ref().and_then(|state| {
            let code = key + 8;
            let layout = state.key_layout(code)?;
            let sym = state.keymap().key_syms_by_level(code, layout, 0).first()?;
            Some(sym.0)
        });
        keys::vk_in_layout(key, first)
    }

    /// What a key press types, as Windows' `WM_CHAR` would report it:
    /// nothing with Alt held, nothing in the middle of a Compose sequence,
    /// the sequence's result when it completes.
    fn typed(&mut self, key: u32) -> String {
        if self.mods & keys::MOD_ALT != 0 {
            return String::new();
        }
        let Some(state) = &self.state else {
            return keys::typed(key, self.mods, self.locked)
                .map(String::from)
                .unwrap_or_default();
        };
        let code = key + 8;
        let sym = state.key_one_sym(code);
        if let Some(compose) = &mut self.compose {
            match compose.feed(sym) {
                compose::Status::Composing | compose::Status::Cancelled => return String::new(),
                compose::Status::Composed => return compose.utf8(),
                compose::Status::Nothing => {}
            }
        }
        if DELETE_SYMS.contains(&sym.0) {
            return String::new();
        }
        state.key_utf8(code)
    }

    fn repeats(&self, key: u32) -> bool {
        match &self.state {
            Some(state) => state.key_repeats(key + 8),
            None => !keys::is_modifier(key),
        }
    }
}

/// `DRM_FORMAT_XRGB8888`: the bytes of this crate's `0xAARRGGBB`.
const FOURCC_XRGB8888: u32 = 0x3432_5258;
/// `DRM_FORMAT_MOD_LINEAR`: rows one after another, nothing else.
const MOD_LINEAR: u64 = 0;
/// `DRM_FORMAT_MOD_INVALID`: "the layout the driver gives by default".
const MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;
/// The dmabuf global is bound no higher than this: from version 4 a client
/// must read the formats from a shared table instead of plain events.
const DMABUF_VERSION: u32 = 3;

/// The connection and everything learned from it.
struct Inner {
    conn: Connection,
    registry: WlRegistry,
    /// The compositor's dmabuf global, if it has one: its name and version.
    dmabuf: Option<(u32, u32)>,
    /// The window has been dropped: nothing more is sent for it.
    closed: bool,
    shm: WlShm,
    wm_base: XdgWmBase,
    surface: WlSurface,
    xdg: XdgSurface,
    toplevel: XdgToplevel,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    keys: Keyboard,
    events: Vec<Event>,
    /// The size the next `xdg_surface.configure` applies; 0 leaves it to us.
    pending: (i32, i32),
    size: (u32, u32),
    configured: bool,
    /// Where the pointer is while it is over the window.
    pointer_at: Option<(i32, i32)>,
    button_down: bool,
    last_click: Option<(Instant, i32, i32)>,
    /// Buffers the compositor has finished with, for the presenter.
    released: Vec<u32>,
    /// The clipboard: this seat's data device, when the compositor has one.
    data_manager: Option<WlDataDeviceManager>,
    data_device: Option<WlDataDevice>,
    /// The serial of the last key or click, which setting the clipboard
    /// must name: only a program the user is using may set it.
    serial: u32,
    /// What this window put on the clipboard, while it is still there.
    copied: Option<(WlDataSource, Arc<str>)>,
    /// Offers the compositor has introduced, each with the types it has.
    offers: Vec<(WlDataOffer, Vec<String>)>,
    /// The offer that is the clipboard now, if another program set it.
    selection: Option<WlDataOffer>,
    /// The connection failed; said once, as a close request.
    lost: bool,
}

impl Inner {
    fn push(&mut self, event: Event) {
        // A pointer move behind another replaces it, as on Windows.
        if let (Event::Move(..), Some(last @ Event::Move(..))) = (event, self.events.last_mut()) {
            *last = event;
        } else {
            self.events.push(event);
        }
    }

    /// The compositor is gone or refused something: there is no window to
    /// keep, so the caller is asked to close.
    fn lose(&mut self) {
        if !self.lost {
            self.lost = true;
            self.push(Event::CloseRequested);
        }
    }

    /// The events a key going down produces, also used for each repeat.
    fn key_down(&mut self, key: u32) {
        let Some(vk) = self.keys.vk(key) else { return };
        // Windows reports a key with Alt held, and F10, as a system key.
        if self.keys.mods & keys::MOD_ALT != 0 || vk == keys::VK_F10 {
            self.push(Event::SysKey { vk, down: true });
        } else {
            self.push(Event::Key { vk, down: true });
        }
        for c in self.keys.typed(key).chars() {
            self.push(Event::Char(c));
        }
    }

    fn key_up(&mut self, key: u32) {
        if self.keys.held.is_some_and(|h| h.key == key) {
            self.keys.held = None;
        }
        if let Some(vk) = self.keys.vk(key) {
            // A system key if Alt is still held, or is the key coming up.
            let alt = self.keys.mods & keys::MOD_ALT != 0 || vk == keys::VK_MENU;
            if alt || vk == keys::VK_F10 {
                self.push(Event::SysKey { vk, down: false });
            } else {
                self.push(Event::Key { vk, down: false });
            }
        }
    }

    /// Turn one protocol event into window events and replies.
    fn handle(&mut self, event: wayland::Event) -> Result<(), wayland::Error> {
        let object = event.object;
        let is = |id: Option<u32>| id == Some(object);
        if event.is_for(&self.wm_base) {
            let xdg_wm_base::Event::Ping { serial } = xdg_wm_base::Event::decode(event)?;
            self.wm_base.pong(&mut self.conn, serial)?;
        } else if event.is_for(&self.toplevel) {
            match xdg_toplevel::Event::decode(event)? {
                xdg_toplevel::Event::Configure { width, height, .. } => {
                    self.pending = (width, height);
                }
                xdg_toplevel::Event::Close => self.push(Event::CloseRequested),
                _ => {}
            }
        } else if event.is_for(&self.xdg) {
            let xdg_surface::Event::Configure { serial } = xdg_surface::Event::decode(event)?;
            self.xdg.ack_configure(&mut self.conn, serial)?;
            let (w, h) = self.pending;
            if w > 0 && h > 0 && (w as u32, h as u32) != self.size {
                self.size = (w as u32, h as u32);
                self.push(Event::Resized(w as u32, h as u32));
            }
            self.configured = true;
            // The compositor wants a frame for this state.
            self.push(Event::Paint);
        } else if is(self.seat.map(|s| s.id())) {
            if let wl_seat::Event::Capabilities { capabilities } = wl_seat::Event::decode(event)? {
                let seat = self.seat.expect("matched above");
                // A device can go and another come (a keyboard unplugged
                // and plugged in, a virtual one for each burst of typing):
                // the object for one that has gone is dead, and must be let
                // go so the next one is asked for.
                let has = |bit| capabilities & bit != 0;
                // An old seat's devices have no release request.
                let version = self.conn.object(seat.id()).map_or(1, |(_, v)| v);
                match (has(wl_seat::capability::POINTER), self.pointer) {
                    (true, None) => self.pointer = Some(seat.get_pointer(&mut self.conn)?),
                    (false, Some(pointer)) => {
                        if version >= wl_pointer::REQ_RELEASE_SINCE {
                            pointer.release(&mut self.conn)?;
                        }
                        self.pointer = None;
                        self.button_down = false;
                        if self.pointer_at.take().is_some() {
                            self.push(Event::Leave);
                        }
                    }
                    _ => {}
                }
                match (has(wl_seat::capability::KEYBOARD), self.keyboard) {
                    (true, None) => self.keyboard = Some(seat.get_keyboard(&mut self.conn)?),
                    (false, Some(keyboard)) => {
                        if version >= wl_keyboard::REQ_RELEASE_SINCE {
                            keyboard.release(&mut self.conn)?;
                        }
                        self.keyboard = None;
                        self.keys.held = None;
                        self.keys.mods = 0;
                        MODS.store(0, Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
        } else if is(self.pointer.map(|p| p.id())) {
            self.pointer_event(wl_pointer::Event::decode(event)?);
        } else if is(self.keyboard.map(|k| k.id())) {
            self.keyboard_event(wl_keyboard::Event::decode(event)?);
        } else if is(self.data_device.map(|d| d.id())) {
            match wl_data_device::Event::decode(event)? {
                wl_data_device::Event::DataOffer { id } => self.offers.push((id, Vec::new())),
                wl_data_device::Event::Selection { id } => {
                    // The clipboard changed hands: every other offer is over.
                    for (offer, types) in std::mem::take(&mut self.offers) {
                        if Some(offer.id()) == id.map(|o| o.id()) {
                            self.offers.push((offer, types));
                        } else {
                            offer.destroy(&mut self.conn)?;
                        }
                    }
                    self.selection = id;
                }
                // Dragging things onto the window is not handled.
                _ => {}
            }
        } else if is(self.copied.as_ref().map(|(source, _)| source.id())) {
            match wl_data_source::Event::decode(event)? {
                wl_data_source::Event::Send { fd, .. } => {
                    // Another program is pasting. It reads at its own pace,
                    // so the text is written on a thread of its own.
                    let text = self.copied.as_ref().expect("matched above").1.clone();
                    std::thread::spawn(move || {
                        let _ = File::from(fd).write_all(text.as_bytes());
                    });
                }
                wl_data_source::Event::Cancelled => {
                    // Something else is on the clipboard now.
                    if let Some((source, _)) = self.copied.take() {
                        source.destroy(&mut self.conn)?;
                    }
                }
                _ => {}
            }
        } else if event.interface.name == wl_data_offer::INTERFACE.name {
            if let wl_data_offer::Event::Offer { mime_type } = wl_data_offer::Event::decode(event)?
            {
                // Recorded against its offer; one for an offer already gone
                // is dropped.
                if let Some((_, types)) = self.offers.iter_mut().find(|(o, _)| o.id() == object) {
                    types.push(mime_type);
                }
            }
        } else if event.interface.name == wl_buffer::INTERFACE.name {
            self.released.push(event.object);
        }
        Ok(())
    }

    fn pointer_event(&mut self, event: wl_pointer::Event) {
        use wl_pointer::Event as P;
        let px = |v: wayland::wire::Fixed| v.to_f64().floor() as i32;
        match event {
            P::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                self.pointer_at = Some((px(surface_x), px(surface_y)));
                self.push(Event::Move(px(surface_x), px(surface_y)));
            }
            P::Leave { .. } => {
                self.pointer_at = None;
                self.push(Event::Leave);
            }
            P::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                let (x, y) = (px(surface_x), px(surface_y));
                self.pointer_at = Some((x, y));
                if self.button_down {
                    self.push(Event::Drag(x, y));
                } else {
                    self.push(Event::Move(x, y));
                }
            }
            P::Button {
                button,
                state,
                serial,
                ..
            } if button == BTN_LEFT => {
                self.serial = serial;
                let (x, y) = self.pointer_at.unwrap_or((0, 0));
                if state != wl_pointer::button_state::PRESSED {
                    self.button_down = false;
                    self.push(Event::Release(x, y));
                    return;
                }
                self.button_down = true;
                let now = Instant::now();
                let double = self.last_click.is_some_and(|(at, lx, ly)| {
                    now.duration_since(at) <= DOUBLE_CLICK
                        && (x - lx).abs() <= DOUBLE_CLICK_PX
                        && (y - ly).abs() <= DOUBLE_CLICK_PX
                });
                if double {
                    // It comes instead of the second click, and a third
                    // starts over.
                    self.last_click = None;
                    self.push(Event::DoubleClick(x, y));
                } else {
                    self.last_click = Some((now, x, y));
                    self.push(Event::Click(x, y));
                }
            }
            P::Axis { axis, value, .. } if axis == wl_pointer::axis::VERTICAL_SCROLL => {
                // A wheel notch is 10 units here, positive toward the user;
                // Windows' is 120, positive away.
                let delta = (-value.to_f64() * 12.0).round() as i32;
                if delta != 0 {
                    self.push(Event::Wheel(delta));
                }
            }
            _ => {}
        }
    }

    fn keyboard_event(&mut self, event: wl_keyboard::Event) {
        use wl_keyboard::Event as K;
        if std::env::var_os("FWRENDER_TRACE_KEYS").is_some() {
            eprintln!("wl_keyboard: {event:?}");
        }
        match event {
            K::Keymap { format, fd, size } => {
                if format == wl_keyboard::keymap_format::XKB_V1 {
                    self.keys.load(fd, size);
                }
            }
            K::Leave { .. } => {
                // A key held while focus leaves is released elsewhere.
                self.keys.held = None;
                self.keys.mods = 0;
                MODS.store(0, Ordering::Relaxed);
            }
            K::Key {
                key, state, serial, ..
            } => {
                self.serial = serial;
                if state == wl_keyboard::key_state::PRESSED {
                    self.key_down(key);
                    let (rate, delay) = self.keys.repeat;
                    self.keys.held = (rate > 0 && self.keys.repeats(key)).then(|| Held {
                        key,
                        next: Instant::now() + delay,
                    });
                } else if state == wl_keyboard::key_state::RELEASED {
                    self.key_up(key);
                }
            }
            K::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => self
                .keys
                .modifiers(mods_depressed, mods_latched, mods_locked, group),
            K::RepeatInfo { rate, delay } => {
                self.keys.repeat = (
                    rate.max(0) as u32,
                    Duration::from_millis(delay.max(0) as u64),
                );
            }
            K::Enter { serial, .. } => self.serial = serial,
        }
    }

    /// Handle every event already read.
    fn drain(&mut self) -> Result<(), wayland::Error> {
        while let Some(event) = self.conn.next_event()? {
            self.handle(event)?;
        }
        Ok(())
    }

    /// A held key repeats on our clock.
    fn repeat(&mut self) {
        let now = Instant::now();
        while let Some(held) = self.keys.held.filter(|h| h.next <= now) {
            self.key_down(held.key);
            let step = Duration::from_millis(1000 / self.keys.repeat.0.max(1) as u64);
            self.keys.held = Some(Held {
                key: held.key,
                next: (held.next + step).max(now),
            });
        }
    }
}

/// A top-level Wayland window (`xdg_toplevel`).
///
/// The window is drawn by [`CpuPresenter`]. It appears on screen with its
/// first frame, as Wayland windows do.
///
/// **Not built yet:** a Vulkan frame in the window (Vulkan's own Wayland
/// surface needs the C `libwayland-client`, which foldwave does not load),
/// a title bar where the desktop draws none (GNOME; the window is then a
/// bare rectangle), sizes on a scaled display (the surface is one pixel a
/// point, so a desktop scaled to 200 % enlarges it), dropped files and
/// pointer shapes.
///
/// A window is used from the thread that created it; dropping it closes it.
pub struct Window {
    /// Shared with whoever shows GPU frames in the window ([`GpuFrames`]).
    inner: Rc<RefCell<Inner>>,
    /// What [`Waker`]s write to, and the end [`Window::wait`] sleeps on.
    wake_tx: Arc<UnixStream>,
    wake_rx: UnixStream,
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
    /// compositor `WAYLAND_DISPLAY` names. Fails when there is no
    /// compositor, or it lacks `xdg_wm_base`.
    pub fn new(title: &str, width: u32, height: u32) -> io::Result<Window> {
        let (wake_tx, wake_rx) = UnixStream::pair()?;
        wake_tx.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let inner = Window::open(title, width.max(1), height.max(1)).map_err(other)?;
        Ok(Window {
            inner: Rc::new(RefCell::new(inner)),
            wake_tx: Arc::new(wake_tx),
            wake_rx,
            alive: Arc::new(()),
        })
    }

    fn open(title: &str, width: u32, height: u32) -> Result<Inner, wayland::Error> {
        let mut conn = Connection::connect()?;
        let registry = conn.display().get_registry(&mut conn)?;
        conn.roundtrip()?;
        let mut globals = Vec::new();
        while let Some(event) = conn.next_event()? {
            if event.is_for(&registry) {
                if let wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } = wl_registry::Event::decode(event)?
                {
                    globals.push((name, interface, version));
                }
            }
        }
        let find = |wanted: &str| {
            globals
                .iter()
                .find(|(_, interface, _)| interface == wanted)
                .map(|(name, _, version)| (*name, *version))
        };
        let need = |wanted: &'static str| {
            find(wanted).ok_or_else(|| wayland::Error::NoCompositor(format!("it has no {wanted}")))
        };
        let (name, version) = need("wl_compositor")?;
        let compositor: WlCompositor =
            registry.bind(&mut conn, name, version.min(COMPOSITOR_VERSION))?;
        let (name, _) = need("wl_shm")?;
        let shm: WlShm = registry.bind(&mut conn, name, 1)?;
        let (name, _) = need("xdg_wm_base")?;
        let wm_base: XdgWmBase = registry.bind(&mut conn, name, 1)?;
        let seat: Option<WlSeat> = match find("wl_seat") {
            Some((name, version)) => {
                Some(registry.bind(&mut conn, name, version.min(SEAT_VERSION))?)
            }
            None => None,
        };

        let data_manager: Option<WlDataDeviceManager> = match find("wl_data_device_manager") {
            Some((name, version)) => {
                Some(registry.bind(&mut conn, name, version.min(DATA_DEVICE_VERSION))?)
            }
            None => None,
        };
        let data_device = match (data_manager, seat) {
            (Some(manager), Some(seat)) => Some(manager.get_data_device(&mut conn, seat)?),
            _ => None,
        };

        let surface = compositor.create_surface(&mut conn)?;
        let xdg = wm_base.get_xdg_surface(&mut conn, surface)?;
        let toplevel = xdg.get_toplevel(&mut conn)?;
        // A NUL inside the text would be refused: drop any.
        let title: String = title.chars().filter(|c| *c != '\0').collect();
        toplevel.set_title(&mut conn, &title)?;
        // The name a desktop groups the window under and finds its icon by.
        toplevel.set_app_id(&mut conn, "foldwave")?;
        // A commit with no buffer asks for the first configure, which must
        // be acknowledged before anything is drawn.
        surface.commit(&mut conn)?;

        let dmabuf = find("zwp_linux_dmabuf_v1");
        let mut inner = Inner {
            conn,
            registry,
            dmabuf,
            closed: false,
            shm,
            wm_base,
            surface,
            xdg,
            toplevel,
            seat,
            pointer: None,
            keyboard: None,
            keys: Keyboard::new(),
            events: Vec::new(),
            pending: (0, 0),
            size: (width, height),
            configured: false,
            pointer_at: None,
            button_down: false,
            last_click: None,
            released: Vec::new(),
            data_manager,
            data_device,
            serial: 0,
            copied: None,
            offers: Vec::new(),
            selection: None,
            lost: false,
        };
        while !inner.configured {
            inner.conn.roundtrip()?;
            inner.drain()?;
        }
        Ok(inner)
    }

    /// Set the window's title.
    pub fn set_title(&self, title: &str) {
        let title: String = title.chars().filter(|c| *c != '\0').collect();
        let inner = &mut *self.inner.borrow_mut();
        if inner.toplevel.set_title(&mut inner.conn, &title).is_err() {
            inner.lose();
        }
    }

    /// The window's size in pixels.
    pub fn client_size(&self) -> (u32, u32) {
        self.inner.borrow().size
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
        let inner = &mut *self.inner.borrow_mut();
        if inner.size != (width, height) {
            inner.size = (width, height);
            inner.push(Event::Resized(width, height));
        }
        inner
            .xdg
            .set_window_geometry(&mut inner.conn, 0, 0, width as i32, height as i32)
            .map_err(other)
    }

    /// The pointer's place in the window, while it is over it.
    pub fn pointer(&self) -> Option<(i32, i32)> {
        self.inner.borrow().pointer_at
    }

    /// Dots per inch. Always 96: scaled displays are not handled yet.
    pub fn dpi(&self) -> u32 {
        96
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

    /// Put `text` on the clipboard. It stays this window's to hand out: the
    /// text goes to another program only when that program pastes, so it is
    /// gone if foldwave closes first, unless the desktop runs a clipboard
    /// manager, as most do.
    ///
    /// A compositor honours this only from the program the user is using:
    /// with no key or click yet, or no keyboard focus, it is ignored.
    pub fn set_clipboard(&self, text: &str) -> io::Result<()> {
        let inner = &mut *self.inner.borrow_mut();
        let (Some(manager), Some(device)) = (inner.data_manager, inner.data_device) else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the compositor has no clipboard for this seat",
            ));
        };
        let conn = &mut inner.conn;
        let sent = (|| {
            let source = manager.create_data_source(conn)?;
            for kind in TEXT_TYPES {
                source.offer(conn, kind)?;
            }
            device.set_selection(conn, Some(source), inner.serial)?;
            conn.flush()?;
            Ok::<_, wayland::Error>(source)
        })();
        match sent {
            Ok(source) => {
                if let Some((old, _)) = inner.copied.replace((source, text.into())) {
                    let _ = old.destroy(&mut inner.conn);
                }
                Ok(())
            }
            Err(e) => {
                inner.lose();
                Err(other(e))
            }
        }
    }

    /// The clipboard's text, or `None` when it holds none: what this window
    /// copied, or what another program offers, read from it now. A program
    /// that does not answer within two seconds is given up on.
    pub fn clipboard(&self) -> io::Result<Option<String>> {
        let inner = &mut *self.inner.borrow_mut();
        if let Some((_, text)) = &inner.copied {
            return Ok(Some(text.to_string()));
        }
        let Some(offer) = inner.selection else {
            return Ok(None);
        };
        let types = inner
            .offers
            .iter()
            .find(|(o, _)| o.id() == offer.id())
            .map_or(&[][..], |(_, types)| &types[..]);
        let Some(kind) = TEXT_TYPES.iter().find(|k| types.iter().any(|t| t == *k)) else {
            return Ok(None);
        };
        // The other program writes into one end of this pair; ours is read
        // until it closes its end.
        let (mut ours, theirs) = UnixStream::pair()?;
        let asked = offer
            .receive(&mut inner.conn, kind, theirs.as_fd())
            .and_then(|()| inner.conn.flush());
        drop(theirs);
        if let Err(e) = asked {
            inner.lose();
            return Err(other(e));
        }
        ours.set_read_timeout(Some(CLIPBOARD_WAIT))?;
        let mut bytes = Vec::new();
        match (&mut ours).take(CLIPBOARD_LIMIT).read_to_end(&mut bytes) {
            Ok(_) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            // Silence for too long: whatever arrived is not the whole text.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Empty: copied files are not read on Linux yet.
    pub fn clipboard_files(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }

    /// `false`: copied images are not read on Linux yet.
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
            target: Arc::downgrade(&self.wake_tx),
        }
    }

    /// Send what is queued, read what has arrived, and append the events to
    /// `events`. Never blocks.
    pub fn poll(&mut self, events: &mut Vec<Event>) {
        self.pump(Some(Duration::ZERO));
        events.append(&mut self.inner.borrow_mut().events);
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
            let held = self.inner.borrow().keys.held;
            let repeat = held.map(|h| h.next.saturating_duration_since(now));
            let sleep = match (left, repeat) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            self.pump(sleep);
            if !self.inner.borrow().events.is_empty() || left.is_some_and(|l| l.is_zero()) {
                break;
            }
        }
        events.append(&mut self.inner.borrow_mut().events);
    }

    /// One turn of the loop: flush, sleep at most `sleep` for the
    /// compositor or a waker, read and handle, and repeat a held key.
    fn pump(&mut self, sleep: Option<Duration>) {
        let inner = &mut *self.inner.borrow_mut();
        if inner.lost {
            // Nothing more will come; do not spin on a dead socket.
            if let Some(sleep) = sleep.filter(|_| inner.events.is_empty()) {
                std::thread::sleep(sleep.min(Duration::from_millis(50)));
            }
            return;
        }
        // Events already read (during a roundtrip, say) come first, and
        // anything queued must not wait.
        if inner.drain().is_err() || inner.conn.flush().is_err() {
            inner.lose();
            return;
        }
        let sleep = if inner.events.is_empty() {
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
                fd: inner.conn.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            },
            PollFd {
                fd: self.wake_rx.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: two valid entries and a timeout that outlive the call; a
        // null mask leaves the signal mask alone.
        let ready = unsafe {
            ppoll(
                fds.as_mut_ptr(),
                2,
                timeout
                    .as_ref()
                    .map_or(std::ptr::null(), |t| t as *const Timespec),
                std::ptr::null(),
            )
        };
        if ready > 0 && fds[0].revents != 0 {
            // The end of the stream reads as nothing here and is reported
            // by the drain below.
            if inner.conn.read().is_err() || inner.drain().is_err() {
                inner.lose();
            }
        }
        if ready > 0 && fds[1].revents != 0 {
            let mut sink = [0u8; 64];
            while matches!((&self.wake_rx).read(&mut sink), Ok(n) if n > 0) {}
            inner.push(Event::Woken);
        }
        inner.repeat();
        // Replies made while handling (a pong, an acknowledgement) go now.
        if !inner.lost && inner.conn.flush().is_err() {
            inner.lose();
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let inner = &mut *self.inner.borrow_mut();
        inner.closed = true;
        if inner.lost {
            return;
        }
        // Children before parents; the socket closes with the connection.
        let conn = &mut inner.conn;
        let _ = inner.toplevel.destroy(conn);
        let _ = inner.xdg.destroy(conn);
        let _ = inner.surface.destroy(conn);
        let _ = inner.wm_base.destroy(conn);
        let _ = conn.flush();
    }
}

/// Shows an exporting Vulkan renderer's frames in a [`Window`]: each of the
/// renderer's images, handed to the compositor once as a dmabuf
/// (`zwp_linux_dmabuf_v1`), then attached whenever it holds a new frame.
/// This is how the GPU draws a Wayland window without Vulkan's own surface.
///
/// **Written, and not yet run with a real buffer**: the machine it was
/// written on has no graphics device, so nothing there can make a dmabuf.
/// Everything the compositor can refuse is asked for before a frame is
/// drawn, and a refusal is an error from [`Window::gpu_frames`], so a caller
/// falls back to [`CpuPresenter`].
pub struct GpuFrames {
    inner: Rc<RefCell<Inner>>,
    dmabuf: ZwpLinuxDmabufV1,
    /// The modifier the compositor is told: how the rows are laid out.
    modifier: u64,
    /// One for each of the renderer's images, and whether the compositor
    /// still holds it.
    buffers: Vec<(WlBuffer, bool)>,
    size: (u32, u32),
}

impl std::fmt::Debug for GpuFrames {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuFrames")
            .field("buffers", &self.buffers.len())
            .field("size", &self.size)
            .finish()
    }
}

/// Which modifier to describe row-by-row frames with, from what the
/// compositor listed for the format: plain rows if it names them, else the
/// driver's default if it takes that; `None` if it takes neither. A
/// compositor too old to list modifiers (`listed` is `None`) takes the
/// default.
fn pick_modifier(listed: Option<&[u64]>) -> Option<u64> {
    match listed {
        None => Some(MOD_INVALID),
        Some(m) if m.contains(&MOD_LINEAR) => Some(MOD_LINEAR),
        Some(m) if m.contains(&MOD_INVALID) => Some(MOD_INVALID),
        Some(_) => None,
    }
}

impl Window {
    /// Hand an exporting renderer's images to the compositor, to show its
    /// frames in this window. Fails, changing nothing the window shows,
    /// when the compositor has no dmabuf support, does not take 32-bit
    /// row-by-row frames, or refuses a buffer.
    pub fn gpu_frames(&self, frames: Vec<Dmabuf>) -> io::Result<GpuFrames> {
        let inner = &mut *self.inner.borrow_mut();
        let (name, version) = inner
            .dmabuf
            .ok_or_else(|| other("the compositor has no zwp_linux_dmabuf_v1"))?;
        let version = version.min(DMABUF_VERSION);
        let dmabuf: ZwpLinuxDmabufV1 = inner
            .registry
            .bind(&mut inner.conn, name, version)
            .map_err(other)?;
        // The compositor lists what it takes as soon as the global is bound.
        let (mut format, mut modifiers) = (false, Vec::new());
        inner.conn.roundtrip().map_err(other)?;
        while let Some(event) = inner.conn.next_event().map_err(other)? {
            if !event.is_for(&dmabuf) {
                inner.handle(event).map_err(other)?;
                continue;
            }
            match zwp_linux_dmabuf_v1::Event::decode(event).map_err(other)? {
                zwp_linux_dmabuf_v1::Event::Format { format: f } => {
                    format |= f == FOURCC_XRGB8888;
                }
                zwp_linux_dmabuf_v1::Event::Modifier {
                    format: f,
                    modifier_hi,
                    modifier_lo,
                } if f == FOURCC_XRGB8888 => {
                    format = true;
                    modifiers.push(u64::from(modifier_hi) << 32 | u64::from(modifier_lo));
                }
                _ => {}
            }
        }
        let listed = (version >= 3).then_some(&modifiers[..]);
        let modifier = match (format, pick_modifier(listed)) {
            (true, Some(m)) => m,
            _ => {
                let _ = dmabuf.destroy(&mut inner.conn);
                return Err(other(
                    "the compositor does not take 32-bit row-by-row dmabufs",
                ));
            }
        };
        let mut out = GpuFrames {
            inner: self.inner.clone(),
            dmabuf,
            modifier,
            buffers: Vec::new(),
            size: (0, 0),
        };
        out.create(inner, frames)?;
        Ok(out)
    }
}

impl GpuFrames {
    /// Make a `wl_buffer` of each frame, waiting for the compositor to
    /// accept or refuse each one.
    fn create(&mut self, inner: &mut Inner, frames: Vec<Dmabuf>) -> io::Result<()> {
        let mut made = Vec::with_capacity(frames.len());
        let mut size = self.size;
        for frame in &frames {
            let conn = &mut inner.conn;
            let params = self.dmabuf.create_params(conn).map_err(other)?;
            params
                .add(
                    conn,
                    frame.fd.as_fd(),
                    0,
                    frame.offset,
                    frame.stride,
                    (self.modifier >> 32) as u32,
                    self.modifier as u32,
                )
                .map_err(other)?;
            params
                .create(
                    conn,
                    frame.width as i32,
                    frame.height as i32,
                    FOURCC_XRGB8888,
                    0,
                )
                .map_err(other)?;
            // `created` or `failed` answers; anything else is the window's.
            let mut answer = None;
            inner.conn.roundtrip().map_err(other)?;
            while let Some(event) = inner.conn.next_event().map_err(other)? {
                if event.is_for(&params) {
                    answer = Some(zwp_linux_buffer_params_v1::Event::decode(event).map_err(other)?);
                } else {
                    inner.handle(event).map_err(other)?;
                }
            }
            let _ = params.destroy(&mut inner.conn);
            match answer {
                Some(zwp_linux_buffer_params_v1::Event::Created { buffer }) => {
                    made.push((buffer, false));
                }
                _ => {
                    for (buffer, _) in made {
                        let _ = buffer.destroy(&mut inner.conn);
                    }
                    return Err(other("the compositor refused a Vulkan frame's dmabuf"));
                }
            }
            size = (frame.width, frame.height);
        }
        self.buffers = made;
        self.size = size;
        Ok(())
    }

    fn lost() -> crate::vk::Error {
        crate::vk::Error::Unsupported("the Wayland connection was lost")
    }
}

impl FrameSink for GpuFrames {
    fn acquire(&mut self) -> Option<usize> {
        let inner = &mut *self.inner.borrow_mut();
        if inner.closed || inner.lost {
            return None;
        }
        for id in inner.released.drain(..) {
            if let Some(b) = self.buffers.iter_mut().find(|(b, _)| b.id() == id) {
                b.1 = false;
            }
        }
        self.buffers.iter().position(|(_, busy)| !busy)
    }

    fn present(&mut self, index: usize) -> Result<(), crate::vk::Error> {
        let inner = &mut *self.inner.borrow_mut();
        let Some((buffer, busy)) = self.buffers.get_mut(index) else {
            return Ok(());
        };
        if inner.closed || inner.lost {
            return Ok(());
        }
        let surface = inner.surface;
        let conn = &mut inner.conn;
        let version = conn.object(surface.id()).map_or(1, |(_, v)| v);
        let (w, h) = (self.size.0 as i32, self.size.1 as i32);
        let sent = (|| {
            surface.attach(conn, Some(*buffer), 0, 0)?;
            if version >= 4 {
                surface.damage_buffer(conn, 0, 0, w, h)?;
            } else {
                surface.damage(conn, 0, 0, w, h)?;
            }
            surface.commit(conn)?;
            conn.flush()
        })();
        match sent {
            Ok(()) => {
                *busy = true;
                Ok(())
            }
            Err(_) => {
                inner.lose();
                Err(GpuFrames::lost())
            }
        }
    }

    fn rebuild(&mut self, frames: Vec<Dmabuf>) -> Result<(), crate::vk::Error> {
        let inner_rc = self.inner.clone();
        let inner = &mut *inner_rc.borrow_mut();
        if inner.closed || inner.lost {
            return Ok(());
        }
        // The compositor keeps what it still shows; the objects can go.
        for (buffer, _) in std::mem::take(&mut self.buffers) {
            let _ = buffer.destroy(&mut inner.conn);
        }
        self.create(inner, frames).map_err(|_| {
            crate::vk::Error::Unsupported("the compositor refused a Vulkan frame's dmabuf")
        })
    }
}

/// One shared-memory buffer the compositor reads a frame from.
struct Buffer {
    proxy: WlBuffer,
    /// The memory behind it, written with ordinary file writes.
    file: File,
    width: u32,
    height: u32,
    /// From when the buffer is handed over until its `release` event.
    busy: bool,
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
    /// The window the buffers were made on.
    owner: Weak<()>,
    buffers: Vec<Buffer>,
}

impl std::fmt::Debug for CpuPresenter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuPresenter")
            .field("buffers", &self.buffers.len())
            .finish()
    }
}

/// A framebuffer's pixels as the bytes a shared-memory buffer holds.
fn bytes(pixels: &[u32]) -> &[u8] {
    // SAFETY: a `u32` is four initialized bytes with no padding, `u8` has
    // no alignment to meet, and the length is the same memory in bytes.
    unsafe { std::slice::from_raw_parts(pixels.as_ptr().cast(), pixels.len() * 4) }
}

impl CpuPresenter {
    /// A presenter.
    pub fn new() -> CpuPresenter {
        CpuPresenter::default()
    }

    /// How many buffers the presenter holds, for tests.
    #[cfg(test)]
    pub(crate) fn buffers_in_use(&self) -> usize {
        self.buffers.len()
    }

    fn make(inner: &mut Inner, width: u32, height: u32) -> io::Result<Buffer> {
        let size = width as u64 * height as u64 * 4;
        let stride = i32::try_from(width as u64 * 4).map_err(other)?;
        let pool_size = i32::try_from(size).map_err(other)?;
        // SAFETY: a NUL-terminated name; a descriptor on success, which
        // `File` then owns, and -1 on failure, which is checked.
        let file = unsafe {
            let fd = memfd_create(b"foldwave-frame\0".as_ptr().cast(), MFD_CLOEXEC);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            File::from_raw_fd(fd)
        };
        file.set_len(size)?;
        let conn = &mut inner.conn;
        let pool = inner
            .shm
            .create_pool(conn, file.as_fd(), pool_size)
            .map_err(other)?;
        let proxy = pool
            .create_buffer(
                conn,
                0,
                width as i32,
                height as i32,
                stride,
                wl_shm::format::XRGB8888,
            )
            .map_err(other)?;
        // The buffer keeps the pool's memory alive.
        pool.destroy(conn).map_err(other)?;
        Ok(Buffer {
            proxy,
            file,
            width,
            height,
            busy: false,
        })
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
        let inner = &mut *window.inner.borrow_mut();
        if inner.lost {
            return Err(other("the Wayland connection was lost"));
        }
        // Buffers made on another window, or one since closed, went with
        // that connection.
        if !self.owner.ptr_eq(&Arc::downgrade(&window.alive)) {
            self.buffers.clear();
            self.owner = Arc::downgrade(&window.alive);
        }
        for id in inner.released.drain(..) {
            if let Some(b) = self.buffers.iter_mut().find(|b| b.proxy.id() == id) {
                b.busy = false;
            }
        }
        // Buffers of an old size go as soon as the compositor lets go.
        let mut i = 0;
        while i < self.buffers.len() {
            let b = &self.buffers[i];
            if (b.width, b.height) != (w, h) && !b.busy {
                let old = self.buffers.swap_remove(i);
                old.proxy.destroy(&mut inner.conn).map_err(other)?;
            } else {
                i += 1;
            }
        }
        let free = self
            .buffers
            .iter()
            .position(|b| (b.width, b.height) == (w, h) && !b.busy);
        let index = match free {
            Some(index) => index,
            None => {
                self.buffers.push(CpuPresenter::make(inner, w, h)?);
                self.buffers.len() - 1
            }
        };
        let buffer = &mut self.buffers[index];
        buffer.file.write_all_at(bytes(cpu.pixels()), 0)?;
        buffer.busy = true;
        let surface = inner.surface;
        let conn = &mut inner.conn;
        let version = conn.object(surface.id()).map_or(1, |(_, v)| v);
        let sent = (|| {
            surface.attach(conn, Some(buffer.proxy), 0, 0)?;
            let (x, y, dw, dh) = (0, start as i32, w as i32, (end - start) as i32);
            if version >= 4 {
                surface.damage_buffer(conn, x, y, dw, dh)?;
            } else {
                surface.damage(conn, x, y, dw, dh)?;
            }
            surface.commit(conn)?;
            conn.flush()
        })();
        sent.map_err(|e| {
            inner.lose();
            other(e)
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A small keymap in the text a compositor sends: a French-style top
    /// left (the key at Q's place types `a`, the key at 1's place `&`),
    /// with Shift, Control, Alt, Return and Delete.
    const KEYMAP: &str = r#"
xkb_keymap {
    xkb_keycodes {
        <AE01> = 10; <AD01> = 24; <RTRN> = 36; <LCTL> = 37; <LFSH> = 50;
        <LALT> = 64; <DELE> = 119;
    };
    xkb_types {
        type "ONE_LEVEL" { modifiers = none; };
        type "TWO_LEVEL" { modifiers = Shift; map[Shift] = Level2; };
        type "ALPHABETIC" {
            modifiers = Shift+Lock;
            map[Shift] = Level2;
            map[Lock] = Level2;
        };
    };
    xkb_compatibility { };
    xkb_symbols {
        key <AE01> { type = "TWO_LEVEL", [ ampersand, 1 ] };
        key <AD01> { type = "ALPHABETIC", [ a, A ] };
        key <RTRN> { type = "ONE_LEVEL", [ Return ] };
        key <LCTL> { type = "ONE_LEVEL", [ Control_L ] };
        key <LFSH> { type = "ONE_LEVEL", [ Shift_L ] };
        key <LALT> { type = "ONE_LEVEL", [ Alt_L ] };
        key <DELE> { type = "ONE_LEVEL", [ Delete ] };
        modifier_map Shift { <LFSH> };
        modifier_map Control { <LCTL> };
        modifier_map Mod1 { <LALT> };
    };
};"#;

    /// A keyboard that has been sent [`KEYMAP`] the way a compositor sends
    /// it: as a file descriptor and a size.
    fn keyboard() -> Keyboard {
        // Tests run side by side: each keyboard gets a file of its own.
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "fwrender-keymap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(&path).unwrap();
        file.write_all(KEYMAP.as_bytes()).unwrap();
        file.write_all(&[0]).unwrap();
        let mut keys = Keyboard::new();
        // No Compose file: the sequences are the system's, not this test's.
        keys.compose = Some(compose::State::new(compose::Table::parse("")));
        keys.load(OwnedFd::from(file), KEYMAP.len() as u32 + 1);
        assert!(keys.state.is_some(), "the keymap was read");
        keys
    }

    // Linux key codes: the keymap's keycodes less 8.
    const KEY_1: u32 = 2;
    const KEY_Q: u32 = 16;
    const KEY_ENTER: u32 = 28;
    const KEY_DELETE: u32 = 111;

    #[test]
    fn a_key_types_what_the_compositors_keymap_says() {
        let mut keys = keyboard();
        // The key at Q's place types `a`, and is the A key to a shortcut.
        assert_eq!(keys.typed(KEY_Q), "a");
        assert_eq!(keys.vk(KEY_Q), Some('A' as u32));
        // The key at 1's place types `&`; it keeps its place's code.
        assert_eq!(keys.typed(KEY_1), "&");
        assert_eq!(keys.vk(KEY_1), Some('1' as u32));
        assert_eq!(keys.typed(KEY_ENTER), "\r");
        // Delete is a key, not a character.
        assert_eq!(keys.typed(KEY_DELETE), "");
        assert_eq!(keys.vk(KEY_DELETE), Some(0x2E));
        assert!(keys.repeats(KEY_Q));
    }

    #[test]
    fn modifiers_change_what_is_typed_and_what_is_reported_down() {
        let mut keys = keyboard();
        // wl_keyboard.modifiers: Shift is bit 0, Control bit 2, Mod1 bit 3.
        keys.modifiers(1, 0, 0, 0);
        assert_eq!(keys.typed(KEY_Q), "A");
        assert_eq!(keys.typed(KEY_1), "1");
        assert!(shift_down() && !ctrl_down() && !alt_down());
        keys.modifiers(4, 0, 0, 0);
        assert_eq!(keys.typed(KEY_Q), "\x01", "Ctrl+A");
        assert!(ctrl_down() && !shift_down());
        keys.modifiers(8, 0, 0, 0);
        assert_eq!(keys.typed(KEY_Q), "", "Alt types nothing");
        assert!(alt_down());
        keys.modifiers(0, 0, 0, 0);
        assert_eq!(keys.typed(KEY_Q), "a");
        assert!(!shift_down() && !ctrl_down() && !alt_down());
    }

    #[test]
    fn a_compose_sequence_types_its_result_and_nothing_on_the_way() {
        let mut keys = keyboard();
        keys.compose = Some(compose::State::new(compose::Table::parse(
            "<ampersand> <a> : \"ä\"\n",
        )));
        assert_eq!(keys.typed(KEY_1), "", "the sequence has begun");
        assert_eq!(keys.typed(KEY_Q), "ä");
        assert_eq!(keys.typed(KEY_Q), "a", "and is over");
    }

    #[test]
    fn row_by_row_frames_are_described_by_a_modifier_the_compositor_listed() {
        assert_eq!(
            pick_modifier(Some(&[7, MOD_LINEAR, MOD_INVALID])),
            Some(MOD_LINEAR)
        );
        assert_eq!(pick_modifier(Some(&[7, MOD_INVALID])), Some(MOD_INVALID));
        assert_eq!(
            pick_modifier(Some(&[7])),
            None,
            "only a tiled layout: refused"
        );
        assert_eq!(pick_modifier(Some(&[])), None);
        // Before version 3 a compositor lists no modifiers at all.
        assert_eq!(pick_modifier(None), Some(MOD_INVALID));
    }

    #[test]
    fn without_a_keymap_keys_type_a_us_layout() {
        let mut keys = Keyboard::new();
        assert_eq!(keys.typed(KEY_Q), "q");
        assert_eq!(keys.vk(KEY_Q), Some('Q' as u32));
        // Set directly: `modifiers` also sets what `shift_down` reports,
        // which another test is checking.
        keys.mods = keys::MOD_SHIFT;
        assert_eq!(keys.typed(KEY_1), "!");
    }
}
