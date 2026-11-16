//! What a window reports and the few choices a caller makes about it: one
//! set of types for every window system, so the code that handles a key or a
//! click is written once.
//!
//! The words are Windows' (the first platform): a key is a virtual-key code,
//! the wheel turns in units of 120 a notch. The Wayland window translates
//! into them (`wl/keys.rs`).

/// What happened to a window.
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
    /// The pointer moved to `(x, y)` with no button down (`WM_MOUSEMOVE`).
    /// Coalesced: a `Move` queued behind another `Move` replaces it, so a
    /// fast pointer leaves one event, its latest place, and never floods
    /// the queue.
    Move(i32, i32),
    /// The pointer left the client area (`WM_MOUSELEAVE`).
    Leave,
    /// Files were dropped at client pixel `(x, y)` (`WM_DROPFILES`); their
    /// paths wait in `Window::take_dropped`.
    Drop(i32, i32),
    /// The user asked to close the window (`WM_CLOSE`). The window stays
    /// open; drop the window to close it.
    CloseRequested,
    /// Some of the window needs repainting (`WM_PAINT`). The update region is
    /// already validated; present the current frame.
    Paint,
    /// A `Waker` woke the event loop.
    Woken,
}

/// Progress on the taskbar button (`Window::set_progress`): a percent for
/// the first three, none for an indeterminate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarProgress {
    Normal(u8),
    Error(u8),
    Paused(u8),
    Indeterminate,
}

/// The pointer's shape over the client area (`Window::set_cursor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// The usual arrow.
    Arrow,
    /// A pointing hand: something under the pointer can be clicked.
    Hand,
}
