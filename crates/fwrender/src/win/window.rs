//! [`Window`]: a top-level Win32 window with a non-blocking event queue, and
//! [`CpuPresenter`], which blits a [`CpuRenderer`] framebuffer into it.

use super::sys::*;
use crate::cpu::CpuRenderer;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::io;
use std::ops::Range;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

/// What happened to a [`Window`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The client area is now `width x height` physical pixels (`WM_SIZE`;
    /// `0 x 0` when minimized).
    Resized(u32, u32),
    /// A typed character (`WM_CHAR`), surrogate pairs already joined.
    Char(char),
    /// A key went down or up (`WM_KEYDOWN` / `WM_KEYUP`); `vk` is the
    /// virtual-key code.
    Key {
        /// Virtual-key code.
        vk: u32,
        /// `true` for down (including auto-repeat), `false` for up.
        down: bool,
    },
    /// The mouse wheel turned (`WM_MOUSEWHEEL`): `delta` in Windows units,
    /// 120 per notch, positive away from the user.
    Wheel(i32),
    /// A key went down or up with Alt held (`WM_SYSKEYDOWN` / `WM_SYSKEYUP`),
    /// or F10. Windows still acts on these (Alt+F4 closes the window); the
    /// characters they would type (`WM_SYSCHAR`) are dropped, except Alt+Space
    /// for the window menu, so they do not beep.
    SysKey {
        /// Virtual-key code.
        vk: u32,
        /// `true` for down (including auto-repeat), `false` for up.
        down: bool,
    },
    /// The left mouse button went down at client pixel `(x, y)`
    /// (`WM_LBUTTONDOWN`). The window captures the mouse until it is
    /// released, so a drag keeps reporting outside the window.
    Click(i32, i32),
    /// The left button went down again quickly at the same place
    /// (`WM_LBUTTONDBLCLK`); it comes instead of the second [`Event::Click`].
    DoubleClick(i32, i32),
    /// The mouse moved to `(x, y)` with the left button down
    /// (`WM_MOUSEMOVE`); coordinates can be outside the client area.
    Drag(i32, i32),
    /// The left button came up at `(x, y)` (`WM_LBUTTONUP`).
    Release(i32, i32),
    /// The user asked to close the window (`WM_CLOSE`). The window stays
    /// open; drop the [`Window`] to close it.
    CloseRequested,
    /// Some of the window needs repainting (`WM_PAINT`). The update region is
    /// already validated; present the current frame.
    Paint,
    /// A [`Waker`] woke the event loop.
    Woken,
}

/// Per-window state the window procedure writes to. Boxed so its address is
/// stable; the procedure finds it through `GWLP_USERDATA`.
struct Shared {
    events: RefCell<Vec<Event>>,
    size: Cell<(u32, u32)>,
    /// A high surrogate from a `WM_CHAR` waiting for its low half.
    surrogate: Cell<Option<u16>>,
}

/// `WM_APP`: the message a [`Waker`] posts.
const WM_WAKE: u32 = WM_APP;

const CLASS_NAME: &str = "nativelite.fwrender.Window";

/// A top-level window.
///
/// Events are gathered by [`Window::poll`] (never blocks) or
/// [`Window::wait`] (sleeps in `MsgWaitForMultipleObjectsEx` until a message
/// or a [`Waker`] arrives, so an idle window costs no CPU). Both pump every
/// message of the calling thread, so events for other windows on the thread
/// are queued for those windows, not lost.
///
/// The process is made per-monitor DPI aware (V2) the first time a window is
/// created, so sizes are physical pixels and GDI output is never bitmap
/// scaled. If the process already chose an awareness (e.g. in its manifest)
/// that choice stands.
///
/// A window belongs to the thread that created it and is neither `Send` nor
/// `Sync`; dropping it destroys it.
pub struct Window {
    hwnd: Hwnd,
    hinstance: Hinstance,
    shared: Box<Shared>,
    /// The hwnd as seen by wakers; zeroed (under the lock) before the window
    /// is destroyed, so a waker never posts to a dead or reused handle.
    wake_target: Arc<Mutex<usize>>,
    /// A high-resolution waitable timer for [`Window::wait`]'s deadline, so a
    /// frame can be paced to well under the ~15.6 ms default timer tick that
    /// a plain wait timeout rounds to. Null when the system has none (before
    /// Windows 10 1803); `wait` then falls back to the millisecond timeout.
    timer: Handle,
    /// Icons set with [`Window::set_icon`], destroyed with the window.
    icons: Vec<Handle>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window")
            .field("hwnd", &self.hwnd)
            .field("size", &self.shared.size.get())
            .finish()
    }
}

/// Wakes a [`Window`] blocked in [`Window::wait`] from any thread; the window
/// then reports [`Event::Woken`]. Waking a dropped window does nothing.
#[derive(Debug, Clone)]
pub struct Waker {
    target: Arc<Mutex<usize>>,
}

impl Waker {
    /// Post the wake message. Cheap and non-blocking.
    pub fn wake(&self) {
        let hwnd = match self.target.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if *hwnd != 0 {
            // SAFETY: the handle is live: `Window::drop` zeroes it under this
            // same lock before destroying the window. PostMessageW may be
            // called from any thread and does not wait for the target.
            unsafe { PostMessageW(*hwnd as Hwnd, WM_WAKE, 0, 0) };
        }
    }
}

/// An `HICON` from a straight-alpha RGBA image `size` pixels square.
fn make_icon(size: u32, rgba: &[u8]) -> io::Result<Handle> {
    let info = BitmapInfo::top_down_32(size as i32, size as i32);
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: `info` is a valid 32bpp header; DIB_RGB_COLORS (0) needs no DC.
    let color = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            0,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if color.is_null() || bits.is_null() {
        return Err(last_error("CreateDIBSection for an icon"));
    }
    let n = (size * size * 4) as usize;
    // SAFETY: the section holds `size * size` 4-byte pixels at `bits`.
    let dst = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), n) };
    for (d, s) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    // The mask is all zeros: the colour bitmap's alpha decides. Rows of a
    // monochrome bitmap are padded to 16 bits.
    let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
    // SAFETY: `mask_bits` holds a whole 1bpp `size x size` bitmap.
    let mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, mask_bits.as_ptr().cast()) };
    let icon_info = IconInfo {
        icon: 1,
        x_hotspot: 0,
        y_hotspot: 0,
        mask,
        color,
    };
    // SAFETY: both bitmaps are live; CreateIconIndirect copies them.
    let icon = unsafe { CreateIconIndirect(&icon_info) };
    // SAFETY: our bitmaps, no longer needed once copied into the icon.
    unsafe {
        DeleteObject(color);
        DeleteObject(mask);
    }
    if icon.is_null() {
        return Err(last_error("CreateIconIndirect"));
    }
    Ok(icon)
}

fn last_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

fn register_class(hinstance: Hinstance) -> io::Result<()> {
    static DPI: Once = Once::new();
    DPI.call_once(|| {
        // SAFETY: a plain value argument. It fails harmlessly (returns FALSE)
        // if the awareness was already set; the result is ignored on purpose.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    });
    // Registering again after success fails with ERROR_CLASS_ALREADY_EXISTS,
    // which is treated as success, so no process-wide flag is needed and a
    // failed registration is retried by the next window.
    let name = wide(CLASS_NAME);
    let class = WndClassExW {
        cb_size: std::mem::size_of::<WndClassExW>() as u32,
        // CS_OWNDC: the window keeps one DC for its lifetime, so the
        // presenter's GetDC/ReleaseDC per present is cheap.
        // CS_DBLCLKS: double-clicks arrive as WM_LBUTTONDBLCLK.
        style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC | CS_DBLCLKS,
        wnd_proc: Some(wnd_proc),
        cls_extra: 0,
        wnd_extra: 0,
        instance: hinstance,
        icon: std::ptr::null_mut(),
        // SAFETY: loads a predefined system cursor (null instance).
        cursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW as *const u16) },
        // No background brush: the renderer paints every pixel.
        background: std::ptr::null_mut(),
        menu_name: std::ptr::null(),
        class_name: name.as_ptr(),
        icon_sm: std::ptr::null_mut(),
    };
    // SAFETY: `class` is fully initialised and `name` outlives the call (the
    // system copies the class name).
    if unsafe { RegisterClassExW(&class) } == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_CLASS_ALREADY_EXISTS) {
            return Err(io::Error::new(e.kind(), format!("RegisterClassExW: {e}")));
        }
    }
    Ok(())
}

impl Window {
    /// Create and show a window whose client area is `width x height`
    /// physical pixels.
    pub fn new(title: &str, width: u32, height: u32) -> io::Result<Window> {
        // SAFETY: a null name returns the executable's module handle.
        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        register_class(hinstance)?;

        let shared = Box::new(Shared {
            events: RefCell::new(Vec::new()),
            size: Cell::new((0, 0)),
            surrogate: Cell::new(None),
        });
        let class = wide(CLASS_NAME);
        let title = wide(title);
        let style = WS_OVERLAPPEDWINDOW;
        // SAFETY: strings are NUL-terminated and outlive the call; the class
        // is registered; `shared` is passed as the create parameter and the
        // window procedure stores it in GWLP_USERDATA on WM_NCCREATE. The box
        // is owned by the returned `Window`, which destroys the window
        // (clearing the pointer) before the box is freed.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                style,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                &*shared as *const Shared as *mut c_void,
            )
        };
        if hwnd.is_null() {
            return Err(last_error("CreateWindowExW"));
        }
        let window = Window {
            hwnd,
            hinstance,
            shared,
            wake_target: Arc::new(Mutex::new(hwnd as usize)),
            // SAFETY: no attributes, no name; a null result is handled.
            timer: unsafe {
                CreateWaitableTimerExW(
                    std::ptr::null(),
                    std::ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_MODIFY_AND_SYNCHRONIZE,
                )
            },
            icons: Vec::new(),
        };

        window.set_client_size(width, height)?;
        // SAFETY: `hwnd` is live. The return value is the previous visibility.
        unsafe { ShowWindow(hwnd, SW_SHOW) };
        Ok(window)
    }

    /// Resize the window so its client area is `width x height` physical
    /// pixels, at the DPI of the monitor it is on. The resulting
    /// [`Event::Resized`] arrives with the next `poll` / `wait`.
    pub fn set_client_size(&self, width: u32, height: u32) -> io::Result<()> {
        let mut r = Rect {
            left: 0,
            top: 0,
            right: width.min(i32::MAX as u32) as i32,
            bottom: height.min(i32::MAX as u32) as i32,
        };
        // SAFETY: `hwnd` is live; `r` is a valid in/out RECT.
        let ok = unsafe {
            let dpi = GetDpiForWindow(self.hwnd);
            AdjustWindowRectExForDpi(&mut r, WS_OVERLAPPEDWINDOW, 0, 0, dpi) != 0
                && SetWindowPos(
                    self.hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                ) != 0
        };
        if ok {
            Ok(())
        } else {
            Err(last_error("sizing the window"))
        }
    }

    /// Set the window's icon from square images, each its size and its
    /// pixels as straight-alpha RGBA rows, top first: the one nearest 16 px
    /// at the window's DPI for the title bar, nearest 32 px for the taskbar
    /// and Alt+Tab (a larger one is scaled down, a smaller one never up).
    pub fn set_icon(&mut self, images: &[(u32, Vec<u8>)]) -> io::Result<()> {
        // SAFETY: `hwnd` is live.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) }.max(96);
        let pick = |want: u32| {
            images
                .iter()
                .filter(|(s, px)| px.len() == (*s * *s * 4) as usize)
                .min_by_key(|(s, _)| if *s >= want { *s - want } else { u32::MAX - *s })
        };
        for (which, base) in [(ICON_SMALL, 16), (ICON_BIG, 32)] {
            let Some((size, rgba)) = pick(base * dpi / 96) else {
                continue;
            };
            let icon = make_icon(*size, rgba)?;
            // SAFETY: `hwnd` is live; the window keeps using the icon, which
            // this window owns until it is destroyed.
            unsafe { SendMessageW(self.hwnd, WM_SETICON, which, icon as isize) };
            self.icons.push(icon);
        }
        Ok(())
    }

    /// Colour the title bar and frame: `caption` behind the title, `text`
    /// for it, `border` around the window (ARGB). The frame is switched to
    /// dark mode first. Windows before 11 ignore the colours; false then.
    pub fn set_frame_colors(&self, caption: u32, text: u32, border: u32) -> bool {
        let colorref = |c: u32| ((c >> 16) & 0xFF) | (c & 0xFF00) | ((c & 0xFF) << 16);
        let set = |attribute: u32, value: u32| {
            // SAFETY: `hwnd` is live; `value` is a 4-byte BOOL or COLORREF
            // that outlives the call.
            unsafe {
                DwmSetWindowAttribute(
                    self.hwnd,
                    attribute,
                    (&value as *const u32).cast(),
                    std::mem::size_of::<u32>() as u32,
                ) == 0
            }
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
        set(DWMWA_CAPTION_COLOR, colorref(caption))
            & set(DWMWA_TEXT_COLOR, colorref(text))
            & set(DWMWA_BORDER_COLOR, colorref(border))
    }

    /// The window's DPI (96 at 100 % scaling).
    pub fn dpi(&self) -> u32 {
        // SAFETY: `hwnd` is live.
        unsafe { GetDpiForWindow(self.hwnd) }
    }

    /// The `HWND`, for creating a GPU surface.
    pub fn hwnd(&self) -> *mut c_void {
        self.hwnd
    }

    /// The `HINSTANCE` the window class was registered with.
    pub fn hinstance(&self) -> *mut c_void {
        self.hinstance
    }

    /// The current client-area size in physical pixels.
    pub fn client_size(&self) -> (u32, u32) {
        let mut r = Rect::default();
        // SAFETY: `hwnd` is live; `r` is a valid out RECT.
        if unsafe { GetClientRect(self.hwnd, &mut r) } != 0 {
            (
                (r.right - r.left).max(0) as u32,
                (r.bottom - r.top).max(0) as u32,
            )
        } else {
            self.shared.size.get()
        }
    }

    /// A handle that wakes [`Window::wait`] from another thread.
    /// Put `text` on the clipboard as Unicode text.
    pub fn set_clipboard(&self, text: &str) -> io::Result<()> {
        let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let _open = Clipboard::open(self.hwnd)?;
        // SAFETY: the clipboard is open (and owned by this window) for the
        // guard's lifetime. The block is `units.len()` u16s; it is written
        // only between GlobalLock and GlobalUnlock, and freed only if the
        // system did not take it.
        unsafe {
            if EmptyClipboard() == 0 {
                return Err(io::Error::last_os_error());
            }
            let mem = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
            if mem.is_null() {
                return Err(io::Error::last_os_error());
            }
            let dst = GlobalLock(mem) as *mut u16;
            if dst.is_null() {
                let e = io::Error::last_os_error();
                GlobalFree(mem);
                return Err(e);
            }
            std::ptr::copy_nonoverlapping(units.as_ptr(), dst, units.len());
            GlobalUnlock(mem);
            if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
                let e = io::Error::last_os_error();
                GlobalFree(mem);
                return Err(e);
            }
        }
        Ok(())
    }

    /// The clipboard's text, if it holds any.
    pub fn clipboard(&self) -> io::Result<Option<String>> {
        let _open = Clipboard::open(self.hwnd)?;
        // SAFETY: the clipboard is open; the handle stays valid until it
        // closes. The locked block is read within GlobalSize bytes, up to
        // its terminating NUL, then unlocked.
        unsafe {
            let mem = GetClipboardData(CF_UNICODETEXT);
            if mem.is_null() {
                return Ok(None);
            }
            let src = GlobalLock(mem) as *const u16;
            if src.is_null() {
                return Ok(None);
            }
            let units = std::slice::from_raw_parts(src, GlobalSize(mem) / 2);
            let len = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            let text = String::from_utf16_lossy(&units[..len]);
            GlobalUnlock(mem);
            Ok(Some(text))
        }
    }

    pub fn waker(&self) -> Waker {
        Waker {
            target: Arc::clone(&self.wake_target),
        }
    }

    /// Handle every pending message without blocking and append the
    /// resulting events.
    pub fn poll(&mut self, events: &mut Vec<Event>) {
        pump();
        self.drain(events);
    }

    /// Block until at least one event arrives or `timeout` passes (`None`
    /// waits forever), then append the events. Sleeps in the kernel; never
    /// spins.
    pub fn wait(&mut self, timeout: Option<Duration>, events: &mut Vec<Event>) {
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        loop {
            pump();
            if self.drain(events) {
                return;
            }
            let mut millis = INFINITE;
            let mut handles = 0;
            if let Some(d) = deadline {
                let left = d.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                // Negative: relative, in 100 ns units, rounded up.
                let due = -(((left.as_nanos() + 99) / 100).min(i64::MAX as u128) as i64);
                // SAFETY: `self.timer` is our live timer handle (checked
                // non-null); `due` outlives the call; no completion routine.
                let armed = !self.timer.is_null()
                    && unsafe {
                        SetWaitableTimer(self.timer, &due, 0, std::ptr::null(), std::ptr::null(), 0)
                    } != 0;
                if armed {
                    handles = 1;
                } else {
                    // Round up so a sub-millisecond remainder still sleeps.
                    let ms = (left.as_nanos() + 999_999) / 1_000_000;
                    millis = ms.min(u128::from(INFINITE - 1)) as u32;
                }
            } else if timeout.is_some() {
                // A timeout too large to represent: wait without one.
                millis = INFINITE;
            }
            // SAFETY: `handles` is 0 (null array) or 1 (our armed timer).
            let r = unsafe {
                MsgWaitForMultipleObjectsEx(
                    handles,
                    &self.timer,
                    millis,
                    QS_ALLINPUT,
                    MWMO_INPUTAVAILABLE,
                )
            };
            if r == WAIT_FAILED {
                // Cannot happen with these arguments; never spin if it does.
                std::thread::sleep(Duration::from_millis(1));
            }
            // Loop: a message that produced no event (mouse move, timer, ...)
            // goes back to sleep until the deadline.
        }
    }

    fn drain(&self, events: &mut Vec<Event>) -> bool {
        let mut q = self.shared.events.borrow_mut();
        let any = !q.is_empty();
        events.append(&mut q);
        any
    }
}

/// Dispatch every queued message of this thread.
fn pump() {
    // SAFETY: `Msg` is plain data, all-zero is a valid value.
    let mut msg: Msg = unsafe { std::mem::zeroed() };
    // SAFETY: `msg` is a valid out MSG; a null hwnd takes every message of
    // the thread. Dispatch runs window procedures, which only touch their
    // own `Shared` through short-lived borrows.
    unsafe {
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        match self.wake_target.lock() {
            Ok(mut g) => *g = 0,
            Err(p) => *p.into_inner() = 0,
        }
        // SAFETY: `hwnd` is live and owned by this thread. WM_NCDESTROY clears
        // GWLP_USERDATA before `shared` is freed after this block.
        unsafe { DestroyWindow(self.hwnd) };
        for &icon in &self.icons {
            // SAFETY: icons this window made, no longer in use now that it
            // is destroyed.
            unsafe { DestroyIcon(icon) };
        }
        if !self.timer.is_null() {
            // SAFETY: our timer handle, closed once.
            unsafe { CloseHandle(self.timer) };
        }
    }
}

/// `WM_SYSCOMMAND` for the menu that Alt alone opens: `SC_KEYMENU` with no
/// character (Alt+Space carries `' '` and still opens the window menu).
pub(crate) fn alt_alone_menu(wparam: usize, lparam: isize) -> bool {
    wparam & 0xFFF0 == SC_KEYMENU && lparam == 0
}

unsafe extern "system" fn wnd_proc(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize {
    if msg == WM_NCCREATE {
        // SAFETY: for WM_NCCREATE, lparam is a CREATESTRUCTW whose first
        // field is the `param` given to CreateWindowExW: our `Shared`.
        unsafe {
            let cs = lparam as *const CreateStructWHead;
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).create_params as isize);
        }
    }
    // SAFETY: GWLP_USERDATA is 0 or the `Shared` set above, which outlives
    // the window (see `Window::drop`).
    let shared = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared).as_ref() };
    let Some(shared) = shared else {
        // SAFETY: forwarding the message unchanged.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    // Never panic here: unwinding out of an `extern "system"` fn aborts.
    let push = |e: Event| {
        if let Ok(mut q) = shared.events.try_borrow_mut() {
            q.push(e);
        }
    };
    match msg {
        WM_SIZE => {
            let (w, h) = ((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32);
            shared.size.set((w, h));
            push(Event::Resized(w, h));
            0
        }
        WM_CHAR => {
            let unit = wparam as u16;
            match unit {
                0xD800..=0xDBFF => shared.surrogate.set(Some(unit)),
                0xDC00..=0xDFFF => {
                    if let Some(hi) = shared.surrogate.take() {
                        if let Some(Ok(c)) = char::decode_utf16([hi, unit]).next() {
                            push(Event::Char(c));
                        }
                    }
                }
                _ => {
                    shared.surrogate.set(None);
                    if let Some(c) = char::from_u32(u32::from(unit)) {
                        push(Event::Char(c));
                    }
                }
            }
            0
        }
        WM_KEYDOWN | WM_KEYUP => {
            push(Event::Key {
                vk: wparam as u32,
                down: msg == WM_KEYDOWN,
            });
            0
        }
        WM_SYSKEYDOWN | WM_SYSKEYUP => {
            push(Event::SysKey {
                vk: wparam as u32,
                down: msg == WM_SYSKEYDOWN,
            });
            // SAFETY: forwarding the message unchanged (Alt+F4, menu keys).
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // Alt+Space opens the window menu; any other Alt+character would
        // only beep.
        WM_SYSCHAR if wparam != usize::from(b' ') => 0,
        // Alt pressed and released on its own would enter menu mode, where
        // the next key selects from an invisible menu instead of reaching the
        // terminal. A terminal wants that key.
        WM_SYSCOMMAND if alt_alone_menu(wparam, lparam) => 0,
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_MOUSEMOVE | WM_LBUTTONUP => {
            // Client coordinates, signed 16-bit words of lparam.
            let x = (lparam & 0xFFFF) as u16 as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32;
            match msg {
                WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                    // SAFETY: `hwnd` is our live window, on its own thread.
                    unsafe { SetCapture(hwnd) };
                    push(if msg == WM_LBUTTONDOWN {
                        Event::Click(x, y)
                    } else {
                        Event::DoubleClick(x, y)
                    });
                }
                WM_MOUSEMOVE if wparam & MK_LBUTTON != 0 => push(Event::Drag(x, y)),
                WM_MOUSEMOVE => {}
                _ => {
                    // SAFETY: no preconditions; a no-op without capture.
                    unsafe { ReleaseCapture() };
                    push(Event::Release(x, y));
                }
            }
            0
        }
        WM_CLOSE => {
            push(Event::CloseRequested);
            0
        }
        WM_MOUSEWHEEL => {
            // The high word of wparam is the signed wheel delta.
            push(Event::Wheel(((wparam >> 16) & 0xFFFF) as u16 as i16 as i32));
            0
        }
        WM_PAINT => {
            // SAFETY: a null rect validates the whole update region, which
            // stops WM_PAINT repeating; the caller repaints by presenting.
            unsafe { ValidateRect(hwnd, std::ptr::null()) };
            push(Event::Paint);
            0
        }
        WM_WAKE => {
            push(Event::Woken);
            0
        }
        WM_NCDESTROY => {
            // SAFETY: detach `Shared` before the owning `Window` frees it.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        // SAFETY: forwarding the message unchanged.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Blits [`CpuRenderer`] rows into a [`Window`] with `SetDIBitsToDevice`.
///
/// The framebuffer is already `0xAARRGGBB`, the byte order of a 32bpp DIB, so
/// no conversion happens. The window's class DC is used (`CS_OWNDC`):
/// `GetDC`/`ReleaseDC` per present just hand back the same DC.
#[derive(Debug, Default)]
pub struct CpuPresenter {
    _private: (),
}

impl CpuPresenter {
    /// A presenter.
    pub fn new() -> CpuPresenter {
        CpuPresenter::default()
    }

    /// Copy framebuffer rows `rows` (clamped to its height) to the same rows
    /// of the window's client area.
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
        let n = end - start;
        let px = &cpu.pixels()[(start * w) as usize..((start + n) * w) as usize];
        // The DIB handed to GDI is exactly the band being drawn: `n` rows,
        // top-down, starting at the first requested row.
        let info = BitmapInfo::top_down_32(w as i32, n as i32);
        // SAFETY: `window.hwnd` is live on this thread.
        let dc = unsafe { GetDC(window.hwnd) };
        if dc.is_null() {
            return Err(io::Error::new(io::ErrorKind::Other, "GetDC failed"));
        }
        // SAFETY: `px` holds `w * n` pixels, 4 bytes each (rows are therefore
        // DWORD-aligned), matching `info`; both outlive the call.
        let lines = unsafe {
            let lines = SetDIBitsToDevice(
                dc,
                0,
                start as i32,
                w,
                n,
                0,
                0,
                0,
                n,
                px.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            );
            ReleaseDC(window.hwnd, dc);
            lines
        };
        if lines <= 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "SetDIBitsToDevice failed",
            ));
        }
        Ok(())
    }
}

/// The primary display's refresh rate in Hz, from its current mode
/// (`EnumDisplaySettingsW`), or `None` if Windows reports none (0 or 1 mean
/// "hardware default").
pub fn display_refresh_hz() -> Option<u32> {
    // DEVMODEW is 220 bytes: dmSize (u16) at offset 68, dmPelsWidth (u32) at
    // 172 and dmDisplayFrequency (u32) at 184 (wingdi.h, x64). A u32 array
    // keeps it aligned.
    let mut mode = [0u32; 55];
    let bytes = 220u16.to_le_bytes();
    mode[17] = u32::from_le_bytes([bytes[0], bytes[1], 0, 0]); // dmSize at 68
                                                               // SAFETY: `mode` is a writable 220-byte DEVMODEW with dmSize set; a null
                                                               // device name means the display the calling thread is on.
    let ok =
        unsafe { EnumDisplaySettingsW(std::ptr::null(), 0xFFFF_FFFF, mode.as_mut_ptr().cast()) };
    let (width, hz) = (mode[172 / 4], mode[184 / 4]);
    (ok != 0 && width > 0 && hz > 1).then_some(hz)
}

/// Run the calling thread at above-normal priority (THREAD_PRIORITY_ABOVE_NORMAL).
///
/// For a render thread: it sleeps nearly all the time, but when a frame is
/// due it must get a core at once. With every core busy moving output (16
/// flooding panes), a normal-priority render thread waited one or two
/// scheduler slices (~15.6 ms each) after waking, which held each pane to 30
/// fps. Returns false if Windows refused.
pub fn raise_thread_priority() -> bool {
    // SAFETY: the pseudo-handle for the current thread is always valid.
    unsafe { SetThreadPriority(GetCurrentThread(), 1) != 0 }
}

/// Whether a Shift key is down now (`GetKeyState`, as of the message being
/// handled), for key combinations such as Shift+PageUp.
pub fn shift_down() -> bool {
    // SAFETY: GetKeyState takes a virtual-key code and has no preconditions.
    unsafe { GetKeyState(0x10) < 0 }
}

/// Write `text` to the console of the process that started this one, for a
/// GUI program (no console of its own) run from a terminal: attach to it,
/// write, and detach again at once, so the terminal's Ctrl+C or closing
/// never reaches this process. Returns false if the parent has no console
/// (started from Explorer, say) or the write failed.
pub fn write_parent_console(text: &str) -> bool {
    // SAFETY: no preconditions; fails harmlessly if already attached or if
    // the parent has no console.
    if unsafe { AttachConsole(u32::MAX) } == 0 {
        return false;
    }
    let ok = std::fs::OpenOptions::new()
        .write(true)
        .open("CONOUT$")
        .and_then(|mut out| io::Write::write_all(&mut out, text.as_bytes()))
        .is_ok();
    // SAFETY: attached just above; the CONOUT$ handle is already closed.
    unsafe { FreeConsole() };
    ok
}

/// Whether an Alt key is down now (`GetKeyState`, as [`shift_down`]).
pub fn alt_down() -> bool {
    // SAFETY: as in `shift_down`.
    unsafe { GetKeyState(0x12) < 0 }
}

/// Whether a Ctrl key is down now (`GetKeyState`, as [`shift_down`]). Note
/// that AltGr reports as Ctrl+Alt.
pub fn ctrl_down() -> bool {
    // SAFETY: as in `shift_down`.
    unsafe { GetKeyState(0x11) < 0 }
}

/// The clipboard, open for this window until dropped. Another program may
/// hold it for a moment, so opening retries briefly.
struct Clipboard;

impl Clipboard {
    fn open(hwnd: Hwnd) -> io::Result<Clipboard> {
        for _ in 0..10 {
            // SAFETY: `hwnd` is a live window; closed again in `drop`.
            if unsafe { OpenClipboard(hwnd) } != 0 {
                return Ok(Clipboard);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(io::Error::last_os_error())
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // SAFETY: this guard exists only while the clipboard is open.
        unsafe { CloseClipboard() };
    }
}
