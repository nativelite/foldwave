//! The Win32 the probe needs, hand-bound: finding a window by title, bringing
//! it forward, synthetic keystrokes, and reading screen pixels. x64 only.
//!
//! Struct layouts and constants are from the Windows SDK headers (winuser.h,
//! wingdi.h); the sizes are checked at compile time.

#![allow(non_snake_case, clippy::upper_case_acronyms)]

use std::ffi::c_void;
use std::io;

pub type Hwnd = *mut c_void;
type Hdc = *mut c_void;
type Hbitmap = *mut c_void;
type Handle = *mut c_void;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

/// `KEYBDINPUT`.
#[repr(C)]
#[derive(Clone, Copy)]
struct KeybdInput {
    vk: u16,
    scan: u16,
    flags: u32,
    time: u32,
    extra: usize,
}

/// `INPUT` for keyboard input: the type, then the union, whose largest member
/// (MOUSEINPUT) makes it 32 bytes; 40 in all on x64.
#[repr(C)]
#[derive(Clone, Copy)]
struct Input {
    kind: u32,
    ki: KeybdInput,
    _pad: [u8; 8],
}

#[repr(C)]
struct BitmapInfoHeader {
    size: u32,
    width: i32,
    height: i32,
    planes: u16,
    bit_count: u16,
    compression: u32,
    size_image: u32,
    x_ppm: i32,
    y_ppm: i32,
    clr_used: u32,
    clr_important: u32,
}

const _: () = {
    assert!(std::mem::size_of::<Input>() == 40);
    assert!(std::mem::size_of::<KeybdInput>() == 24);
    assert!(std::mem::size_of::<BitmapInfoHeader>() == 40);
    assert!(std::mem::size_of::<Rect>() == 16);
};

const INPUT_KEYBOARD: u32 = 1;
const KEYEVENTF_KEYUP: u32 = 0x0002;
const KEYEVENTF_UNICODE: u32 = 0x0004;
const VK_MENU: u16 = 0x12;
pub const VK_BACK: u16 = 0x08;
const SRCCOPY: u32 = 0x00CC_0020;
const SW_RESTORE: i32 = 9;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_NOACTIVATE: u32 = 0x0010;
const HWND_TOPMOST: isize = -1;
const HWND_NOTOPMOST: isize = -2;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOSIZE: u32 = 0x0001;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;

type EnumProc = unsafe extern "system" fn(Hwnd, isize) -> i32;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(f: EnumProc, l: isize) -> i32;
    fn GetWindowTextW(h: Hwnd, buf: *mut u16, n: i32) -> i32;
    fn IsWindowVisible(h: Hwnd) -> i32;
    fn SetForegroundWindow(h: Hwnd) -> i32;
    fn GetForegroundWindow() -> Hwnd;
    fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
    fn SetWindowPos(h: Hwnd, after: isize, x: i32, y: i32, cx: i32, cy: i32, f: u32) -> i32;
    fn GetClientRect(h: Hwnd, r: *mut Rect) -> i32;
    fn ClientToScreen(h: Hwnd, p: *mut Point) -> i32;
    fn SendInput(n: u32, inputs: *const Input, size: i32) -> u32;
    fn GetDC(h: Hwnd) -> Hdc;
    fn ReleaseDC(h: Hwnd, dc: Hdc) -> i32;
    fn PostMessageW(h: Hwnd, msg: u32, w: usize, l: isize) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(dc: Hdc) -> Hdc;
    fn DeleteDC(dc: Hdc) -> i32;
    fn CreateDIBSection(
        dc: Hdc,
        info: *const BitmapInfoHeader,
        usage: u32,
        bits: *mut *mut c_void,
        section: Handle,
        offset: u32,
    ) -> Hbitmap;
    fn SelectObject(dc: Hdc, obj: *mut c_void) -> *mut c_void;
    fn DeleteObject(obj: *mut c_void) -> i32;
    fn BitBlt(
        dst: Hdc,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        src: Hdc,
        sx: i32,
        sy: i32,
        rop: u32,
    ) -> i32;
    fn GdiFlush() -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetStdHandle(which: u32) -> Handle;
    fn WriteFile(h: Handle, buf: *const u8, n: u32, written: *mut u32, ov: *mut c_void) -> i32;
}

fn last_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// Visible top-level windows whose title contains `needle`.
pub fn find_windows(needle: &str) -> Vec<(Hwnd, String)> {
    struct Found {
        needle: String,
        out: Vec<(Hwnd, String)>,
    }
    unsafe extern "system" fn visit(h: Hwnd, l: isize) -> i32 {
        // SAFETY: `l` is the `Found` passed to EnumWindows below, alive for
        // the whole enumeration; `buf` is a writable buffer of its length.
        unsafe {
            let found = &mut *(l as *mut Found);
            if IsWindowVisible(h) == 0 {
                return 1;
            }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
            let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
            if title.contains(&found.needle) {
                found.out.push((h, title));
            }
        }
        1
    }
    let mut found = Found {
        needle: needle.to_string(),
        out: Vec::new(),
    };
    // SAFETY: the callback only runs during this call; `found` outlives it.
    unsafe { EnumWindows(visit, &mut found as *mut Found as isize) };
    found.out
}

/// Ask a window to close (`WM_CLOSE`).
pub fn close(h: Hwnd) {
    // SAFETY: posting to a stale handle fails harmlessly.
    unsafe { PostMessageW(h, 0x0010, 0, 0) };
}

/// The client area, in screen pixels.
pub fn client_rect_on_screen(h: Hwnd) -> Rect {
    let mut r = Rect::default();
    let mut p = Point::default();
    // SAFETY: valid out pointers; a stale handle yields zeros.
    unsafe {
        GetClientRect(h, &mut r);
        ClientToScreen(h, &mut p);
    }
    Rect {
        left: p.x,
        top: p.y,
        right: p.x + r.right,
        bottom: p.y + r.bottom,
    }
}

/// Move and size the window's outer frame.
pub fn place(h: Hwnd, x: i32, y: i32, w: i32, hgt: i32) {
    // SAFETY: plain call on a window handle.
    unsafe {
        ShowWindow(h, SW_RESTORE);
        SetWindowPos(h, 0, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

/// Keep the window above others (or stop).
pub fn topmost(h: Hwnd, on: bool) {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    // SAFETY: plain call on a window handle.
    unsafe {
        SetWindowPos(
            h,
            after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
    };
}

fn key(vk: u16, scan: u16, flags: u32) -> Input {
    Input {
        kind: INPUT_KEYBOARD,
        ki: KeybdInput {
            vk,
            scan,
            flags,
            time: 0,
            extra: 0,
        },
        _pad: [0; 8],
    }
}

fn send(inputs: &[Input]) -> io::Result<()> {
    // SAFETY: `inputs` is a valid array of INPUT of the given size.
    let n = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<Input>() as i32,
        )
    };
    if n as usize == inputs.len() {
        Ok(())
    } else {
        Err(last_error("SendInput"))
    }
}

/// Bring `h` to the foreground. Windows only lets the process that last had
/// input do this, so an Alt tap is sent first, the usual workaround.
pub fn focus(h: Hwnd) -> bool {
    let _ = send(&[key(VK_MENU, 0, 0), key(VK_MENU, 0, KEYEVENTF_KEYUP)]);
    // SAFETY: plain calls on a window handle.
    unsafe {
        ShowWindow(h, SW_RESTORE);
        SetForegroundWindow(h);
        GetForegroundWindow() == h
    }
}

/// The title of `h`.
pub fn title(h: Hwnd) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: `buf` is writable for its length; a stale handle gives "".
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// The window that has keyboard focus now.
pub fn foreground() -> Hwnd {
    // SAFETY: no arguments.
    unsafe { GetForegroundWindow() }
}

/// Type one character (as a Unicode key press and release).
pub fn type_char(c: char) -> io::Result<()> {
    let mut units = [0u16; 2];
    let mut inputs = Vec::new();
    for &u in c.encode_utf16(&mut units).iter() {
        inputs.push(key(0, u, KEYEVENTF_UNICODE));
        inputs.push(key(0, u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    send(&inputs)
}

/// Press and release a virtual key.
pub fn press(vk: u16) -> io::Result<()> {
    send(&[key(vk, 0, 0), key(vk, 0, KEYEVENTF_KEYUP)])
}

/// Reads a fixed screen rectangle into memory, repeatedly.
pub struct Capture {
    screen: Hdc,
    mem: Hdc,
    bitmap: Hbitmap,
    old: *mut c_void,
    bits: *const u32,
    pub rect: Rect,
}

impl Capture {
    /// A capture of `rect` (screen pixels).
    pub fn new(rect: Rect) -> io::Result<Capture> {
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w <= 0 || h <= 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty capture"));
        }
        let info = BitmapInfoHeader {
            size: 40,
            width: w,
            height: -h, // top-down
            planes: 1,
            bit_count: 32,
            compression: 0,
            size_image: 0,
            x_ppm: 0,
            y_ppm: 0,
            clr_used: 0,
            clr_important: 0,
        };
        // SAFETY: standard GDI setup; every handle is checked and released
        // in Drop (or here on failure).
        unsafe {
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return Err(last_error("GetDC"));
            }
            let mem = CreateCompatibleDC(screen);
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(mem, &info, 0, &mut bits, std::ptr::null_mut(), 0);
            if mem.is_null() || bitmap.is_null() || bits.is_null() {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                if !mem.is_null() {
                    DeleteDC(mem);
                }
                ReleaseDC(std::ptr::null_mut(), screen);
                return Err(last_error("CreateDIBSection"));
            }
            let old = SelectObject(mem, bitmap);
            Ok(Capture {
                screen,
                mem,
                bitmap,
                old,
                bits: bits as *const u32,
                rect,
            })
        }
    }

    /// Width and height.
    pub fn size(&self) -> (usize, usize) {
        (
            (self.rect.right - self.rect.left) as usize,
            (self.rect.bottom - self.rect.top) as usize,
        )
    }

    /// Copy the rectangle's current pixels into `out` (0x00RRGGBB).
    pub fn grab(&self, out: &mut Vec<u32>) -> io::Result<()> {
        let (w, h) = self.size();
        // SAFETY: both DCs are live; the DIB is w*h 32-bit pixels, read only
        // after GdiFlush has completed the blit.
        unsafe {
            if BitBlt(
                self.mem,
                0,
                0,
                w as i32,
                h as i32,
                self.screen,
                self.rect.left,
                self.rect.top,
                SRCCOPY,
            ) == 0
            {
                return Err(last_error("BitBlt"));
            }
            GdiFlush();
            out.clear();
            out.extend_from_slice(std::slice::from_raw_parts(self.bits, w * h));
        }
        for p in out.iter_mut() {
            *p &= 0x00FF_FFFF;
        }
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: the objects this struct created, released once.
        unsafe {
            SelectObject(self.mem, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.mem);
            ReleaseDC(std::ptr::null_mut(), self.screen);
        }
    }
}

/// Write all of `bytes` to standard output with WriteFile (no console
/// re-encoding on the way).
pub fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    // SAFETY: GetStdHandle has no preconditions; WriteFile gets a valid
    // buffer and length and a valid out pointer.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut at = 0;
        while at < bytes.len() {
            let chunk = &bytes[at..(at + (1 << 20)).min(bytes.len())];
            let mut n = 0u32;
            if WriteFile(
                h,
                chunk.as_ptr(),
                chunk.len() as u32,
                &mut n,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(last_error("WriteFile"));
            }
            at += n as usize;
        }
    }
    Ok(())
}
