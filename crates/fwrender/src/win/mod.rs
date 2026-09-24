//! The Windows platform layer: a window with an event loop, a CPU presenter
//! and a GDI glyph rasterizer.
//!
//! Win32 is bound by hand (`sys.rs`) against the OS libraries `user32`,
//! `gdi32` and `kernel32`; no crate is involved. Only 64-bit Windows 10 1703
//! or later is supported (per-monitor DPI awareness V2).
//!
//! * [`Window`] opens a top-level window and turns its messages into
//!   [`Event`]s, with [`Window::wait`] sleeping until something happens and a
//!   [`Waker`] to interrupt it from another thread.
//! * [`CpuPresenter`] copies rows of a [`crate::CpuRenderer`] framebuffer to
//!   the window.
//! * [`GdiRasterizer`] is a [`crate::Rasterizer`] over an installed font.

mod font;
mod sys;
mod window;

pub use font::GdiRasterizer;
pub use window::{
    alt_down, ctrl_down, display_refresh_hz, raise_thread_priority, shift_down, CpuPresenter,
    Event, Waker, Window,
};

#[cfg(test)]
mod tests;
