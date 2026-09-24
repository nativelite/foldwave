//! Hand-written Win32 declarations: just what the window, presenter and GDI
//! rasterizer call, nothing more.
//!
//! `user32`, `gdi32` and `kernel32` are OS libraries every Windows process can
//! link; nothing third-party is involved. Struct layouts, constants and
//! prototypes follow the Windows SDK headers (`winuser.h`, `wingdi.h`,
//! `windef.h`) as documented on learn.microsoft.com; the page for each is
//! named where a value is not obvious. x64 layout is asserted at compile time
//! at the bottom of this file.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;

// Only the x64 layouts were checked; a 32-bit build would need different
// struct sizes (and `SetWindowLongPtrW` is a macro there).
#[cfg(not(target_pointer_width = "64"))]
compile_error!("fwrender::win supports 64-bit Windows only");

pub type Hwnd = *mut c_void;
pub type Hinstance = *mut c_void;
pub type Hdc = *mut c_void;
pub type Hgdiobj = *mut c_void;
pub type Handle = *mut c_void;
pub type Bool = i32;

/// `WNDPROC`.
pub type WndProc = unsafe extern "system" fn(Hwnd, u32, usize, isize) -> isize;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Size {
    pub cx: i32,
    pub cy: i32,
}

/// `WNDCLASSEXW` (winuser.h).
#[repr(C)]
pub struct WndClassExW {
    pub cb_size: u32,
    pub style: u32,
    pub wnd_proc: Option<WndProc>,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub instance: Hinstance,
    pub icon: Handle,
    pub cursor: Handle,
    pub background: Handle,
    pub menu_name: *const u16,
    pub class_name: *const u16,
    pub icon_sm: Handle,
}

/// `MSG` (winuser.h). The trailing `lPrivate` is part of the SDK definition.
#[repr(C)]
pub struct Msg {
    pub hwnd: Hwnd,
    pub message: u32,
    pub wparam: usize,
    pub lparam: isize,
    pub time: u32,
    pub pt: Point,
    pub private: u32,
}

/// `CREATESTRUCTW` (winuser.h); only the first field is read, so only that
/// one is declared and the struct is only ever used behind a pointer.
#[repr(C)]
pub struct CreateStructWHead {
    pub create_params: *mut c_void,
}

/// `TEXTMETRICW` (wingdi.h): eleven `LONG`s, four `WCHAR`s, five `BYTE`s.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct TextMetricW {
    pub height: i32,
    pub ascent: i32,
    pub descent: i32,
    pub internal_leading: i32,
    pub external_leading: i32,
    pub ave_char_width: i32,
    pub max_char_width: i32,
    pub weight: i32,
    pub overhang: i32,
    pub digitized_aspect_x: i32,
    pub digitized_aspect_y: i32,
    pub first_char: u16,
    pub last_char: u16,
    pub default_char: u16,
    pub break_char: u16,
    pub italic: u8,
    pub underlined: u8,
    pub struck_out: u8,
    pub pitch_and_family: u8,
    pub char_set: u8,
}

/// `BITMAPINFOHEADER` (wingdi.h).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BitmapInfoHeader {
    pub size: u32,
    pub width: i32,
    /// Negative for a top-down DIB (origin at the upper-left corner).
    pub height: i32,
    pub planes: u16,
    pub bit_count: u16,
    pub compression: u32,
    pub size_image: u32,
    pub x_pels_per_meter: i32,
    pub y_pels_per_meter: i32,
    pub clr_used: u32,
    pub clr_important: u32,
}

/// `BITMAPINFO` with its one-entry colour table (unused for 32bpp `BI_RGB`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct BitmapInfo {
    pub header: BitmapInfoHeader,
    pub colors: [u32; 1],
}

impl BitmapInfo {
    /// A 32bpp `BI_RGB` top-down DIB header of `width x height`.
    pub fn top_down_32(width: i32, height: i32) -> BitmapInfo {
        BitmapInfo {
            header: BitmapInfoHeader {
                size: std::mem::size_of::<BitmapInfoHeader>() as u32,
                width,
                height: -height,
                planes: 1,
                bit_count: 32,
                compression: BI_RGB,
                ..BitmapInfoHeader::default()
            },
            colors: [0],
        }
    }
}

// --- winuser.h -------------------------------------------------------------

pub const CS_VREDRAW: u32 = 0x0001;
pub const CS_HREDRAW: u32 = 0x0002;
pub const CS_OWNDC: u32 = 0x0020;
pub const CS_DBLCLKS: u32 = 0x0008;

pub const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
/// `CW_USEDEFAULT` is `((int)0x80000000)`.
pub const CW_USEDEFAULT: i32 = i32::MIN;
pub const SW_SHOW: i32 = 5;
/// `GWLP_USERDATA` is `(-21)`.
pub const GWLP_USERDATA: i32 = -21;
/// `IDC_ARROW` is `MAKEINTRESOURCE(32512)`.
pub const IDC_ARROW: usize = 32512;

pub const SWP_NOMOVE: u32 = 0x0002;
pub const SWP_NOZORDER: u32 = 0x0004;
pub const SWP_NOACTIVATE: u32 = 0x0010;

pub const PM_REMOVE: u32 = 0x0001;

pub const WM_SIZE: u32 = 0x0005;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCDESTROY: u32 = 0x0082;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_SYSCHAR: u32 = 0x0106;
pub const WM_SYSCOMMAND: u32 = 0x0112;
pub const SC_KEYMENU: usize = 0xF100;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_LBUTTONDBLCLK: u32 = 0x0203;
/// `wparam` of mouse messages: the left button is down.
pub const MK_LBUTTON: usize = 0x0001;
/// Clipboard format: UTF-16 text, NUL-terminated (winuser.h).
pub const CF_UNICODETEXT: u32 = 13;
pub const GMEM_MOVEABLE: u32 = 0x0002;
/// winuser.h.
pub const WM_MOUSEWHEEL: u32 = 0x020A;
pub const WM_APP: u32 = 0x8000;

/// `QS_ALLINPUT` for Windows 8 and later: `QS_INPUT` (mouse, key, raw input,
/// touch, pointer = 0x1C07) | `QS_POSTMESSAGE` | `QS_TIMER` | `QS_PAINT` |
/// `QS_HOTKEY` | `QS_SENDMESSAGE` = 0x1CFF. Values from the `GetQueueStatus`
/// page on learn.microsoft.com.
pub const QS_ALLINPUT: u32 = 0x1CFF;
/// Return even when input was already seen by `PeekMessage` but not removed.
pub const MWMO_INPUTAVAILABLE: u32 = 0x0004;
pub const INFINITE: u32 = 0xFFFF_FFFF;
pub const WAIT_FAILED: u32 = 0xFFFF_FFFF;
/// `CreateWaitableTimerExW` flag, Windows 10 1803+ (learn.microsoft.com,
/// CreateWaitableTimerExW, dwFlags).
pub const CREATE_WAITABLE_TIMER_HIGH_RESOLUTION: u32 = 0x0000_0002;
/// `TIMER_MODIFY_STATE` (0x0002) | `SYNCHRONIZE` (0x0010_0000): enough to
/// set the timer and wait on it.
pub const TIMER_MODIFY_AND_SYNCHRONIZE: u32 = 0x0010_0002;

/// `DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2` is `((DPI_AWARENESS_CONTEXT)-4)`.
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;

pub const ERROR_CLASS_ALREADY_EXISTS: i32 = 1410;

// --- wingdi.h --------------------------------------------------------------

pub const BI_RGB: u32 = 0;
pub const DIB_RGB_COLORS: u32 = 0;
pub const TRANSPARENT: i32 = 1;
pub const ETO_CLIPPED: u32 = 0x0004;

pub const FW_NORMAL: i32 = 400;
pub const FW_BOLD: i32 = 700;
pub const DEFAULT_CHARSET: u32 = 1;
pub const OUT_TT_PRECIS: u32 = 4;
pub const CLIP_DEFAULT_PRECIS: u32 = 0;
/// Grayscale antialiasing; `CLEARTYPE_QUALITY` (5) would give per-channel
/// coverage, which a single-channel atlas cannot hold.
pub const ANTIALIASED_QUALITY: u32 = 4;
pub const FIXED_PITCH: u32 = 1;
/// `FF_MODERN` is `(3 << 4)`.
pub const FF_MODERN: u32 = 0x30;
/// `LF_FACESIZE`: a face name is at most 31 UTF-16 units plus the NUL.
pub const LF_FACESIZE: usize = 32;

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(class: *const WndClassExW) -> u16;
    pub fn CreateWindowExW(
        ex_style: u32,
        class_name: *const u16,
        window_name: *const u16,
        style: u32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: Hwnd,
        menu: Handle,
        instance: Hinstance,
        param: *mut c_void,
    ) -> Hwnd;
    pub fn DestroyWindow(hwnd: Hwnd) -> Bool;
    pub fn DefWindowProcW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> isize;
    pub fn ShowWindow(hwnd: Hwnd, cmd: i32) -> Bool;
    pub fn SetWindowPos(
        hwnd: Hwnd,
        insert_after: Hwnd,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> Bool;
    pub fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> Bool;
    pub fn ValidateRect(hwnd: Hwnd, rect: *const Rect) -> Bool;
    pub fn SetWindowLongPtrW(hwnd: Hwnd, index: i32, value: isize) -> isize;
    pub fn GetWindowLongPtrW(hwnd: Hwnd, index: i32) -> isize;
    pub fn LoadCursorW(instance: Hinstance, name: *const u16) -> Handle;
    pub fn PeekMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32, remove: u32) -> Bool;
    pub fn TranslateMessage(msg: *const Msg) -> Bool;
    pub fn DispatchMessageW(msg: *const Msg) -> isize;
    pub fn PostMessageW(hwnd: Hwnd, msg: u32, wparam: usize, lparam: isize) -> Bool;
    /// High bit set (negative) while the key is down.
    pub fn GetKeyState(vk: i32) -> i16;
    pub fn SetCapture(hwnd: Hwnd) -> Hwnd;
    pub fn ReleaseCapture() -> Bool;
    pub fn OpenClipboard(owner: Hwnd) -> Bool;
    pub fn CloseClipboard() -> Bool;
    pub fn EmptyClipboard() -> Bool;
    /// On success the system owns `data`; on failure the caller still does.
    pub fn SetClipboardData(format: u32, data: Handle) -> Handle;
    pub fn GetClipboardData(format: u32) -> Handle;
    pub fn MsgWaitForMultipleObjectsEx(
        count: u32,
        handles: *const Handle,
        millis: u32,
        wake_mask: u32,
        flags: u32,
    ) -> u32;
    pub fn GetDC(hwnd: Hwnd) -> Hdc;
    pub fn ReleaseDC(hwnd: Hwnd, hdc: Hdc) -> i32;
    // Windows 10 1607+ (1703+ for the V2 context); foldwave targets Windows 11.
    pub fn SetProcessDpiAwarenessContext(context: isize) -> Bool;
    pub fn GetDpiForWindow(hwnd: Hwnd) -> u32;
    /// `mode` is a DEVMODEW (220 bytes); `index` ENUM_CURRENT_SETTINGS
    /// (0xFFFFFFFF) reads the current mode.
    pub fn EnumDisplaySettingsW(device: *const u16, index: u32, mode: *mut c_void) -> Bool;
    pub fn AdjustWindowRectExForDpi(
        rect: *mut Rect,
        style: u32,
        menu: Bool,
        ex_style: u32,
        dpi: u32,
    ) -> Bool;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn CreateCompatibleDC(hdc: Hdc) -> Hdc;
    pub fn DeleteDC(hdc: Hdc) -> Bool;
    pub fn CreateDIBSection(
        hdc: Hdc,
        info: *const BitmapInfo,
        usage: u32,
        bits: *mut *mut c_void,
        section: Handle,
        offset: u32,
    ) -> Hgdiobj;
    pub fn SelectObject(hdc: Hdc, obj: Hgdiobj) -> Hgdiobj;
    pub fn DeleteObject(obj: Hgdiobj) -> Bool;
    pub fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: u32,
        underline: u32,
        strike_out: u32,
        char_set: u32,
        out_precision: u32,
        clip_precision: u32,
        quality: u32,
        pitch_and_family: u32,
        face_name: *const u16,
    ) -> Hgdiobj;
    pub fn GetTextMetricsW(hdc: Hdc, tm: *mut TextMetricW) -> Bool;
    pub fn GetTextExtentPoint32W(hdc: Hdc, s: *const u16, len: i32, size: *mut Size) -> Bool;
    pub fn SetTextColor(hdc: Hdc, color: u32) -> u32;
    pub fn SetBkMode(hdc: Hdc, mode: i32) -> i32;
    pub fn ExtTextOutW(
        hdc: Hdc,
        x: i32,
        y: i32,
        options: u32,
        rect: *const Rect,
        s: *const u16,
        len: u32,
        dx: *const i32,
    ) -> Bool;
    pub fn GdiFlush() -> Bool;
    pub fn SetDIBitsToDevice(
        hdc: Hdc,
        x_dest: i32,
        y_dest: i32,
        width: u32,
        height: u32,
        x_src: i32,
        y_src: i32,
        start_scan: u32,
        lines: u32,
        bits: *const c_void,
        info: *const BitmapInfo,
        color_use: u32,
    ) -> i32;
    /// Test read-back only. Returns a `COLORREF` (`0x00BBGGRR`).
    #[cfg(test)]
    pub fn GetPixel(hdc: Hdc, x: i32, y: i32) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> Hinstance;
    /// Returns a pseudo-handle for the calling thread (no need to close).
    pub fn GetCurrentThread() -> Handle;
    /// `priority`: THREAD_PRIORITY_ABOVE_NORMAL is 1, NORMAL 0 (processthreadsapi.h).
    pub fn SetThreadPriority(thread: Handle, priority: i32) -> i32;
    pub fn CreateWaitableTimerExW(
        attributes: *const c_void,
        name: *const u16,
        flags: u32,
        access: u32,
    ) -> Handle;
    /// `due` is a LARGE_INTEGER: negative is relative, in 100 ns units.
    pub fn SetWaitableTimer(
        timer: Handle,
        due: *const i64,
        period_ms: i32,
        completion: *const c_void,
        arg: *const c_void,
        resume: i32,
    ) -> i32;
    pub fn CloseHandle(handle: Handle) -> i32;
    pub fn GlobalAlloc(flags: u32, bytes: usize) -> Handle;
    pub fn GlobalLock(mem: Handle) -> *mut c_void;
    pub fn GlobalUnlock(mem: Handle) -> Bool;
    pub fn GlobalSize(mem: Handle) -> usize;
    pub fn GlobalFree(mem: Handle) -> Handle;
    /// `ATTACH_PARENT_PROCESS` (u32::MAX) attaches to the parent's console.
    pub fn AttachConsole(process: u32) -> Bool;
    pub fn FreeConsole() -> Bool;
}

/// `s` as NUL-terminated UTF-16.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// x64 sizes from the SDK headers; a mismatch fails the build, not a call.
const _: () = {
    use std::mem::size_of;
    assert!(size_of::<WndClassExW>() == 80);
    assert!(size_of::<Msg>() == 48);
    assert!(size_of::<Rect>() == 16);
    assert!(size_of::<TextMetricW>() == 60);
    assert!(size_of::<BitmapInfoHeader>() == 40);
    assert!(size_of::<BitmapInfo>() == 44);
};
