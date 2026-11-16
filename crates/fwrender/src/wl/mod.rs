//! The Linux platform layer: a Wayland window with an event loop and a CPU
//! presenter.
//!
//! Wayland is spoken by `nativelite-wayland` and the keyboard's keymap read
//! by `nativelite-xkb`, nativelite's own, on the standard library alone.
//! Neither the system's `libwayland-client` nor `libxkbcommon` is linked or
//! loaded (founder decision, `docs/design.md` section 11, row 25). A machine
//! without a compositor gets an error, never a failed start.
//!
//! * [`Window`] opens a top-level window and turns what the compositor
//!   sends into [`Event`]s, the same type the Windows window reports, with
//!   [`Window::wait`] sleeping until something happens and a [`Waker`] to
//!   interrupt it from another thread.
//! * [`CpuPresenter`] shows a [`crate::CpuRenderer`] framebuffer in the
//!   window through shared memory.
//!
//! A Vulkan frame in the window is not built: Vulkan's own Wayland surface
//! needs the C library. [`crate::vk::VkWindow`] refuses and hands the window
//! back, so a caller falls back to the CPU renderer.

mod keys;
mod window;

pub use window::{
    alt_down, announce_environment_change, ctrl_down, display_refresh_hz, raise_thread_priority,
    shift_down, write_parent_console, CpuPresenter, Cursor, Event, TaskbarProgress, Waker, Window,
};

#[cfg(test)]
mod tests;
