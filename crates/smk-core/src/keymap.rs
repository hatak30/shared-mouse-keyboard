//! Key code translation.
//!
//! The canonical wire representation is the Linux evdev key code
//! (`linux/input-event-codes.h`). Each platform converts from its native
//! code on capture and back to its native code on injection.

#![allow(dead_code)]

pub mod ev {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    pub const ENTER: u16 = 28;
    pub const LEFTCTRL: u16 = 29;
    pub const LEFTSHIFT: u16 = 42;
    pub const RIGHTSHIFT: u16 = 54;
    pub const LEFTALT: u16 = 56;
    pub const CAPSLOCK: u16 = 58;
    pub const NUMLOCK: u16 = 69;
    pub const KPENTER: u16 = 96;
    pub const RIGHTCTRL: u16 = 97;
    pub const RIGHTALT: u16 = 100;
    pub const HOME: u16 = 102;
    pub const UP: u16 = 103;
    pub const LEFT: u16 = 105;
    pub const RIGHT: u16 = 106;
    pub const DOWN: u16 = 108;
    pub const PAUSE: u16 = 119;
    pub const LEFTMETA: u16 = 125;
    pub const RIGHTMETA: u16 = 126;
    pub const FN: u16 = 0x1d0;
    pub const V: u16 = 47;
}

/// The modifier that pastes on this OS: Command on macOS, Ctrl elsewhere.
pub fn is_paste_modifier(code: u16) -> bool {
    if cfg!(target_os = "macos") {
        is_meta(code)
    } else {
        is_ctrl(code)
    }
}

/// True if exactly the paste modifier is held (so `V` would paste, not e.g.
/// "paste and match style" or some other shortcut).
pub fn is_paste_chord(held: &std::collections::HashSet<u16>) -> bool {
    held.iter().any(|&c| is_paste_modifier(c))
        && held.iter().all(|&c| is_paste_modifier(c) || !(is_ctrl(c) || is_alt(c) || is_shift(c) || is_meta(c)))
}

pub fn is_ctrl(code: u16) -> bool {
    code == ev::LEFTCTRL || code == ev::RIGHTCTRL
}
pub fn is_alt(code: u16) -> bool {
    code == ev::LEFTALT || code == ev::RIGHTALT
}
pub fn is_shift(code: u16) -> bool {
    code == ev::LEFTSHIFT || code == ev::RIGHTSHIFT
}
pub fn is_meta(code: u16) -> bool {
    code == ev::LEFTMETA || code == ev::RIGHTMETA
}

/// Swap Command (meta) and Control. Used when a Mac controls a PC or the
/// other way around, so that Cmd+C on the Mac keyboard becomes Ctrl+C.
pub fn swap_cmd_ctrl(code: u16) -> u16 {
    match code {
        ev::LEFTMETA => ev::LEFTCTRL,
        ev::RIGHTMETA => ev::RIGHTCTRL,
        ev::LEFTCTRL => ev::LEFTMETA,
        ev::RIGHTCTRL => ev::RIGHTMETA,
        c => c,
    }
}

/// (macOS virtual key code, evdev code)
pub const MAC_TABLE: &[(u16, u16)] = &[
    (0x00, 30),  // A
    (0x01, 31),  // S
    (0x02, 32),  // D
    (0x03, 33),  // F
    (0x04, 35),  // H
    (0x05, 34),  // G
    (0x06, 44),  // Z
    (0x07, 45),  // X
    (0x08, 46),  // C
    (0x09, 47),  // V
    (0x0A, 86),  // ISO section -> 102ND
    (0x0B, 48),  // B
    (0x0C, 16),  // Q
    (0x0D, 17),  // W
    (0x0E, 18),  // E
    (0x0F, 19),  // R
    (0x10, 21),  // Y
    (0x11, 20),  // T
    (0x12, 2),   // 1
    (0x13, 3),   // 2
    (0x14, 4),   // 3
    (0x15, 5),   // 4
    (0x16, 7),   // 6
    (0x17, 6),   // 5
    (0x18, 13),  // =
    (0x19, 10),  // 9
    (0x1A, 8),   // 7
    (0x1B, 12),  // -
    (0x1C, 9),   // 8
    (0x1D, 11),  // 0
    (0x1E, 27),  // ]
    (0x1F, 24),  // O
    (0x20, 22),  // U
    (0x21, 26),  // [
    (0x22, 23),  // I
    (0x23, 25),  // P
    (0x24, 28),  // Return
    (0x25, 38),  // L
    (0x26, 36),  // J
    (0x27, 40),  // '
    (0x28, 37),  // K
    (0x29, 39),  // ;
    (0x2A, 43),  // backslash
    (0x2B, 51),  // ,
    (0x2C, 53),  // /
    (0x2D, 49),  // N
    (0x2E, 50),  // M
    (0x2F, 52),  // .
    (0x30, 15),  // Tab
    (0x31, 57),  // Space
    (0x32, 41),  // `
    (0x33, 14),  // Delete (backspace)
    (0x35, 1),   // Escape
    (0x36, 126), // Right Command
    (0x37, 125), // Command
    (0x38, 42),  // Shift
    (0x39, 58),  // Caps Lock
    (0x3A, 56),  // Option
    (0x3B, 29),  // Control
    (0x3C, 54),  // Right Shift
    (0x3D, 100), // Right Option
    (0x3E, 97),  // Right Control
    (0x3F, ev::FN),
    (0x40, 187), // F17
    (0x41, 83),  // Keypad .
    (0x43, 55),  // Keypad *
    (0x45, 78),  // Keypad +
    (0x47, 69),  // Keypad Clear -> NumLock
    (0x48, 115), // Volume up
    (0x49, 114), // Volume down
    (0x4A, 113), // Mute
    (0x4B, 98),  // Keypad /
    (0x4C, 96),  // Keypad Enter
    (0x4E, 74),  // Keypad -
    (0x4F, 188), // F18
    (0x50, 189), // F19
    (0x51, 117), // Keypad =
    (0x52, 82),  // Keypad 0
    (0x53, 79),  // Keypad 1
    (0x54, 80),  // Keypad 2
    (0x55, 81),  // Keypad 3
    (0x56, 75),  // Keypad 4
    (0x57, 76),  // Keypad 5
    (0x58, 77),  // Keypad 6
    (0x59, 71),  // Keypad 7
    (0x5A, 190), // F20
    (0x5B, 72),  // Keypad 8
    (0x5C, 73),  // Keypad 9
    (0x5D, 124), // JIS Yen
    (0x5E, 89),  // JIS underscore -> RO
    (0x5F, 95),  // JIS keypad comma
    (0x60, 63),  // F5
    (0x61, 64),  // F6
    (0x62, 65),  // F7
    (0x63, 61),  // F3
    (0x64, 66),  // F8
    (0x65, 67),  // F9
    (0x66, 94),  // JIS Eisu -> MUHENKAN
    (0x67, 87),  // F11
    (0x68, 92),  // JIS Kana -> HENKAN
    (0x69, 183), // F13
    (0x6A, 186), // F16
    (0x6B, 184), // F14
    (0x6D, 68),  // F10
    (0x6E, 127), // Context menu -> COMPOSE
    (0x6F, 88),  // F12
    (0x71, 185), // F15
    (0x72, 110), // Help -> Insert
    (0x73, 102), // Home
    (0x74, 104), // Page up
    (0x75, 111), // Forward delete
    (0x76, 62),  // F4
    (0x77, 107), // End
    (0x78, 60),  // F2
    (0x79, 109), // Page down
    (0x7A, 59),  // F1
    (0x7B, 105), // Left
    (0x7C, 106), // Right
    (0x7D, 108), // Down
    (0x7E, 103), // Up
];

pub fn mac_to_ev(mac: u16) -> Option<u16> {
    MAC_TABLE.iter().find(|(m, _)| *m == mac).map(|(_, e)| *e)
}

pub fn ev_to_mac(code: u16) -> Option<u16> {
    MAC_TABLE.iter().find(|(_, e)| *e == code).map(|(m, _)| *m)
}

/// Extended (0xE0-prefixed) PC set-1 scan codes -> evdev.
const WIN_EXTENDED: &[(u16, u16)] = &[
    (0x1C, 96),  // Keypad Enter
    (0x1D, 97),  // Right Ctrl
    (0x20, 113), // Mute
    (0x2E, 114), // Volume down
    (0x30, 115), // Volume up
    (0x35, 98),  // Keypad /
    (0x37, 99),  // Print Screen / SysRq
    (0x38, 100), // Right Alt
    (0x45, 69),  // Num Lock (reported extended by some layouts)
    (0x47, 102), // Home
    (0x48, 103), // Up
    (0x49, 104), // Page up
    (0x4B, 105), // Left
    (0x4D, 106), // Right
    (0x4F, 107), // End
    (0x50, 108), // Down
    (0x51, 109), // Page down
    (0x52, 110), // Insert
    (0x53, 111), // Delete
    (0x5B, 125), // Left Win
    (0x5C, 126), // Right Win
    (0x5D, 127), // Menu
];

/// Non-extended scan codes that do not map 1:1 to evdev.
const WIN_SPECIAL: &[(u16, u16)] = &[
    (0x64, 183), // F13
    (0x65, 184),
    (0x66, 185),
    (0x67, 186),
    (0x68, 187),
    (0x69, 188),
    (0x6A, 189),
    (0x6B, 190), // F20
    (0x70, 93),  // Katakana/Hiragana
    (0x73, 89),  // RO
    (0x79, 92),  // Henkan
    (0x7B, 94),  // Muhenkan
    (0x7D, 124), // Yen
];

/// Windows scan code (+ extended flag) -> evdev.
pub fn win_to_ev(scan: u16, extended: bool) -> Option<u16> {
    if extended {
        return WIN_EXTENDED.iter().find(|(s, _)| *s == scan).map(|(_, e)| *e);
    }
    if let Some((_, e)) = WIN_SPECIAL.iter().find(|(s, _)| *s == scan) {
        return Some(*e);
    }
    // For the base block, set-1 scan codes equal evdev codes (ESC=1 .. F12=88).
    if (1..=88).contains(&scan) {
        return Some(scan);
    }
    None
}

/// evdev -> Windows scan code + extended flag.
pub fn ev_to_win(code: u16) -> Option<(u16, bool)> {
    if let Some((s, _)) = WIN_EXTENDED.iter().find(|(_, e)| *e == code && *e != 69) {
        return Some((*s, true));
    }
    if let Some((s, _)) = WIN_SPECIAL.iter().find(|(_, e)| *e == code) {
        return Some((*s, false));
    }
    if (1..=88).contains(&code) {
        return Some((code, false));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_roundtrip() {
        for (mac, e) in MAC_TABLE {
            assert_eq!(mac_to_ev(*mac), Some(*e));
            assert_eq!(ev_to_mac(*e), Some(*mac), "evdev {e} not unique");
        }
    }

    #[test]
    fn win_roundtrip() {
        for code in 1..=200u16 {
            if let Some((s, ext)) = ev_to_win(code) {
                assert_eq!(win_to_ev(s, ext), Some(code));
            }
        }
        assert_eq!(win_to_ev(0x1E, false), Some(30)); // A
        assert_eq!(win_to_ev(0x48, true), Some(ev::UP));
        assert_eq!(ev_to_win(ev::RIGHTCTRL), Some((0x1D, true)));
    }
}
