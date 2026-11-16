//! The Linux platform layer: a Wayland window with an event loop and a CPU
//! presenter.
//!
//! `libwayland-client` is the system's own library, loaded at run time and
//! bound by hand (`sys.rs`); no crate is involved, and a machine without it
//! or without a compositor gets an error, never a failed start (founder
//! decision, `docs/design.md` section 11, row 25).
//!
//! * [`Window`] opens a top-level window and turns what the compositor
//!   sends into [`Event`]s, the same type the Windows window reports, with
//!   [`Window::wait`] sleeping until something happens and a [`Waker`] to
//!   interrupt it from another thread.
//! * [`CpuPresenter`] shows a [`crate::CpuRenderer`] framebuffer in the
//!   window through shared memory.
//! * The Vulkan backend draws into the same window through
//!   `VK_KHR_wayland_surface` ([`crate::vk::VkWindow`]).

mod keys;
mod sys;
mod window;

pub use window::{
    alt_down, announce_environment_change, ctrl_down, display_refresh_hz, raise_thread_priority,
    shift_down, write_parent_console, CpuPresenter, Cursor, Event, TaskbarProgress, Waker, Window,
};

#[cfg(test)]
mod tests;
