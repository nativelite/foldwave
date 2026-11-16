//! Keys: Linux key codes to the virtual-key codes and characters the rest of
//! foldwave already handles.
//!
//! Wayland hands a client raw key codes (the kernel's `input-event-codes.h`)
//! and a keymap to interpret them with. **This first cut does not read the
//! keymap**: it types a US layout. Reading it properly means either
//! `libxkbcommon` (a system library every Wayland desktop has, like
//! `libwayland-client`) or our own keymap compiler; that is an open founder
//! decision (`docs/philosophy.md` section 8), not something assumed here.
//!
//! The virtual-key codes are Windows' (`VK_*`), because [`super::Event`] is
//! the same type on both systems and fwterm's key handling is written
//! against them.

/// Modifier bits as the keyboard's `modifiers` event reports them: the first
/// four of X's eight real modifiers, whose positions are fixed.
pub const MOD_SHIFT: u32 = 1;
/// Caps Lock, in the `locked` mask.
pub const MOD_CAPS: u32 = 2;
/// Control.
pub const MOD_CTRL: u32 = 4;
/// Alt (Mod1).
pub const MOD_ALT: u32 = 8;

pub const VK_BACK: u32 = 0x08;
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_SPACE: u32 = 0x20;
pub const VK_F10: u32 = 0x79;

/// The virtual-key code for a Linux key code, or `None` for a key this
/// table does not know.
pub fn vk(key: u32) -> Option<u32> {
    Some(match key {
        1 => VK_ESCAPE,
        2..=10 => 0x31 + (key - 2), // 1..9
        11 => 0x30,                 // 0
        12 => 0xBD,                 // - _
        13 => 0xBB,                 // = +
        14 => VK_BACK,
        15 => VK_TAB,
        16..=25 => b"QWERTYUIOP"[(key - 16) as usize] as u32,
        26 => 0xDB, // [ {
        27 => 0xDD, // ] }
        28 => VK_RETURN,
        29 | 97 => VK_CONTROL,
        30..=38 => b"ASDFGHJKL"[(key - 30) as usize] as u32,
        39 => 0xBA, // ; :
        40 => 0xDE, // ' "
        41 => 0xC0, // ` ~
        42 | 54 => VK_SHIFT,
        43 => 0xDC, // \ |
        44..=50 => b"ZXCVBNM"[(key - 44) as usize] as u32,
        51 => 0xBC, // , <
        52 => 0xBE, // . >
        53 => 0xBF, // / ?
        55 => 0x6A, // keypad *
        56 | 100 => VK_MENU,
        57 => VK_SPACE,
        58 => 0x14,                   // Caps Lock
        59..=68 => 0x70 + (key - 59), // F1..F10
        69 => 0x90,                   // Num Lock
        70 => 0x91,                   // Scroll Lock
        71..=73 => 0x67 + (key - 71), // keypad 7 8 9
        74 => 0x6D,                   // keypad -
        75..=77 => 0x64 + (key - 75), // keypad 4 5 6
        78 => 0x6B,                   // keypad +
        79..=81 => 0x61 + (key - 79), // keypad 1 2 3
        82 => 0x60,                   // keypad 0
        83 => 0x6E,                   // keypad .
        87 => 0x7A,                   // F11
        88 => 0x7B,                   // F12
        96 => VK_RETURN,              // keypad Enter
        98 => 0x6F,                   // keypad /
        102 => 0x24,                  // Home
        103 => 0x26,                  // Up
        104 => 0x21,                  // Page Up
        105 => 0x25,                  // Left
        106 => 0x27,                  // Right
        107 => 0x23,                  // End
        108 => 0x28,                  // Down
        109 => 0x22,                  // Page Down
        110 => 0x2D,                  // Insert
        111 => 0x2E,                  // Delete
        125 | 126 => 0x5B,            // the logo keys
        _ => return None,
    })
}

/// Whether holding the key does not repeat it.
pub fn is_modifier(key: u32) -> bool {
    matches!(key, 29 | 42 | 54 | 56 | 58 | 69 | 70 | 97 | 100 | 125 | 126)
}

/// The two characters a US keyboard prints for a key: plain and shifted.
fn us(key: u32) -> Option<(char, char)> {
    const ROW1: &[u8] = b"1234567890-=";
    const ROW1S: &[u8] = b"!@#$%^&*()_+";
    const ROW2: &[u8] = b"qwertyuiop[]";
    const ROW2S: &[u8] = b"QWERTYUIOP{}";
    const ROW3: &[u8] = b"asdfghjkl;'`";
    const ROW3S: &[u8] = b"ASDFGHJKL:\"~";
    const ROW4: &[u8] = b"\\zxcvbnm,./";
    const ROW4S: &[u8] = b"|ZXCVBNM<>?";
    let pair = |plain: &[u8], shifted: &[u8], i: u32| {
        Some((plain[i as usize] as char, shifted[i as usize] as char))
    };
    match key {
        2..=13 => pair(ROW1, ROW1S, key - 2),
        16..=27 => pair(ROW2, ROW2S, key - 16),
        30..=41 => pair(ROW3, ROW3S, key - 30),
        43..=53 => pair(ROW4, ROW4S, key - 43),
        57 => Some((' ', ' ')),
        55 => Some(('*', '*')),
        74 => Some(('-', '-')),
        78 => Some(('+', '+')),
        98 => Some(('/', '/')),
        71..=73 => pair(b"789", b"789", key - 71),
        75..=77 => pair(b"456", b"456", key - 75),
        79..=81 => pair(b"123", b"123", key - 79),
        82 => Some(('0', '0')),
        83 => Some(('.', '.')),
        _ => None,
    }
}

/// The character a key types, as Windows' `WM_CHAR` would report it: the US
/// layout's, with Shift and Caps Lock; a control character with Ctrl (Ctrl+C
/// is `\x03`); Enter, Tab, Backspace and Escape as their control characters.
/// `None` for a key that types nothing, and for anything with Alt held.
pub fn typed(key: u32, mods: u32, locked: u32) -> Option<char> {
    if mods & MOD_ALT != 0 {
        return None;
    }
    match key {
        28 | 96 => return Some('\r'),
        15 => return Some('\t'),
        14 => return Some('\x08'),
        1 => return Some('\x1b'),
        _ => {}
    }
    let (plain, shifted) = us(key)?;
    let shift = mods & MOD_SHIFT != 0;
    let c = if plain.is_ascii_lowercase() {
        // Caps Lock turns letters over; Shift turns them back.
        if shift != (locked & MOD_CAPS != 0) {
            shifted
        } else {
            plain
        }
    } else if shift {
        shifted
    } else {
        plain
    };
    if mods & MOD_CTRL != 0 {
        // The control characters: @ A-Z [ \ ] ^ _ and their lowercase.
        return match c.to_ascii_uppercase() {
            u @ '@'..='_' => Some((u as u8 & 0x1F) as char),
            ' ' => Some('\0'),
            _ => None,
        };
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_the_keys_fwterm_binds_have_their_windows_codes() {
        assert_eq!(vk(30), Some('A' as u32));
        assert_eq!(vk(44), Some('Z' as u32));
        assert_eq!(vk(2), Some('1' as u32));
        assert_eq!(vk(11), Some('0' as u32));
        assert_eq!(vk(51), Some(0xBC), "comma, for Ctrl+Comma");
        assert_eq!(vk(59), Some(0x70), "F1");
        assert_eq!(vk(68), Some(VK_F10));
        assert_eq!(vk(103), Some(0x26), "Up");
        assert_eq!(vk(26), Some(0xDB), "[, for Ctrl+Shift+[");
        assert_eq!(vk(13), Some(0xBB), "plus, for Alt+Shift+Plus");
        assert_eq!(vk(9999), None);
        assert!(is_modifier(42) && !is_modifier(30));
    }

    #[test]
    fn typing_follows_shift_caps_and_control() {
        assert_eq!(typed(30, 0, 0), Some('a'));
        assert_eq!(typed(30, MOD_SHIFT, 0), Some('A'));
        assert_eq!(typed(30, 0, MOD_CAPS), Some('A'));
        assert_eq!(typed(30, MOD_SHIFT, MOD_CAPS), Some('a'));
        // Caps Lock does not shift a digit.
        assert_eq!(typed(2, 0, MOD_CAPS), Some('1'));
        assert_eq!(typed(2, MOD_SHIFT, 0), Some('!'));
        assert_eq!(typed(53, MOD_SHIFT, 0), Some('?'));
        assert_eq!(typed(43, 0, 0), Some('\\'));
        assert_eq!(typed(46, MOD_CTRL, 0), Some('\x03'), "Ctrl+C");
        assert_eq!(typed(26, MOD_CTRL, 0), Some('\x1b'), "Ctrl+[");
        assert_eq!(typed(28, 0, 0), Some('\r'));
        assert_eq!(typed(14, 0, 0), Some('\x08'));
        assert_eq!(typed(30, MOD_ALT, 0), None, "Alt types nothing");
        assert_eq!(typed(103, 0, 0), None, "an arrow types nothing");
    }
}
