//! Reporting the mouse to a program that asked for it (the xterm protocol):
//! atrium's Ctrl+A m, vim with `mouse=a`, htop. While a pane's program has
//! mouse tracking on, clicks, drags and the wheel go to it as reports
//! instead of selecting and scrolling in fwterm; Shift held keeps them
//! fwterm's. On the alternate screen with no tracking, the wheel becomes
//! Up and Down arrows, as in other terminals, since that screen has no
//! history to scroll.

use vterm::MouseTracking;

/// Which button an event is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    WheelUp,
    WheelDown,
}

/// What happened to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Press,
    /// Moved while held.
    Drag,
    Release,
}

/// Modifier keys held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// The report for an event at cell `(row, col)` of the pane (from 0), in the
/// encoding the program asked for; `None` when its tracking does not cover
/// the event, or the original encoding cannot carry the position (past 223).
pub fn report(
    tracking: MouseTracking,
    sgr: bool,
    button: Button,
    action: Action,
    row: usize,
    col: usize,
    mods: Mods,
) -> Option<Vec<u8>> {
    let wheel = matches!(button, Button::WheelUp | Button::WheelDown);
    match (tracking, action) {
        (MouseTracking::Off, _) => return None,
        (MouseTracking::Click, Action::Drag) => return None,
        (_, Action::Drag | Action::Release) if wheel => return None,
        _ => {}
    }
    let mut code: u32 = match button {
        Button::Left => 0,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
    };
    if action == Action::Drag {
        code += 32;
    }
    code += u32::from(mods.shift) * 4 + u32::from(mods.alt) * 8 + u32::from(mods.ctrl) * 16;
    let (x, y) = (col + 1, row + 1);
    if sgr {
        let end = if action == Action::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    if x > 223 || y > 223 {
        return None;
    }
    // The original encoding cannot say which button came up: 3 is "released".
    if action == Action::Release {
        code = (code & !3) | 3;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code) as u8,
        (32 + x) as u8,
        (32 + y) as u8,
    ])
}

/// The wheel as arrow keys, `lines` of them, for a full-screen program that
/// did not ask for the mouse.
pub fn wheel_as_arrows(up: bool, lines: usize) -> Vec<u8> {
    let key: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
    key.repeat(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Mods = Mods {
        shift: false,
        alt: false,
        ctrl: false,
    };

    fn r(
        t: MouseTracking,
        sgr: bool,
        b: Button,
        a: Action,
        row: usize,
        col: usize,
    ) -> Option<String> {
        report(t, sgr, b, a, row, col, NONE).map(|v| String::from_utf8_lossy(&v).into_owned())
    }

    #[test]
    fn sgr_reports_press_drag_release_and_wheel_one_based() {
        use Action::*;
        use Button::*;
        let t = MouseTracking::Drag;
        assert_eq!(
            r(t, true, Left, Press, 0, 0).as_deref(),
            Some("\x1b[<0;1;1M")
        );
        assert_eq!(
            r(t, true, Left, Drag, 4, 9).as_deref(),
            Some("\x1b[<32;10;5M")
        );
        assert_eq!(
            r(t, true, Left, Release, 4, 9).as_deref(),
            Some("\x1b[<0;10;5m")
        );
        assert_eq!(
            r(t, true, WheelUp, Press, 2, 3).as_deref(),
            Some("\x1b[<64;4;3M")
        );
        assert_eq!(
            r(t, true, WheelDown, Press, 2, 3).as_deref(),
            Some("\x1b[<65;4;3M")
        );
        // Wide panes are fine in SGR.
        assert_eq!(
            r(t, true, Left, Press, 66, 283).as_deref(),
            Some("\x1b[<0;284;67M")
        );
        let ctrl = Mods { ctrl: true, ..NONE };
        assert_eq!(
            report(t, true, Left, Press, 0, 0, ctrl).as_deref(),
            Some(&b"\x1b[<16;1;1M"[..])
        );
    }

    #[test]
    fn tracking_decides_what_is_reported() {
        use Action::*;
        use Button::*;
        assert_eq!(r(MouseTracking::Off, true, Left, Press, 0, 0), None);
        assert_eq!(
            r(MouseTracking::Click, true, Left, Drag, 0, 0),
            None,
            "clicks only"
        );
        assert!(r(MouseTracking::Click, true, Left, Release, 0, 0).is_some());
        assert!(r(MouseTracking::Motion, true, Left, Drag, 0, 0).is_some());
        assert_eq!(r(MouseTracking::Drag, true, WheelUp, Release, 0, 0), None);
    }

    #[test]
    fn the_original_encoding_offsets_by_32_and_stops_at_223() {
        use Action::*;
        use Button::*;
        let t = MouseTracking::Click;
        assert_eq!(
            report(t, false, Left, Press, 0, 0, NONE),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        assert_eq!(
            report(t, false, Left, Release, 1, 2, NONE),
            Some(vec![0x1b, b'[', b'M', 35, 35, 34]),
            "release is button 3"
        );
        assert_eq!(report(t, false, Left, Press, 0, 223, NONE), None);
        assert_eq!(
            report(t, false, WheelDown, Press, 0, 0, NONE),
            Some(vec![0x1b, b'[', b'M', 32 + 65, 33, 33])
        );
    }

    #[test]
    fn the_wheel_as_arrows() {
        assert_eq!(wheel_as_arrows(true, 3), b"\x1b[A\x1b[A\x1b[A");
        assert_eq!(wheel_as_arrows(false, 1), b"\x1b[B");
    }
}
