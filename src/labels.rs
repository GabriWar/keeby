use evdev::Key;

const MODIFIERS: &[Key] = &[
    Key::KEY_LEFTCTRL,
    Key::KEY_RIGHTCTRL,
    Key::KEY_LEFTSHIFT,
    Key::KEY_RIGHTSHIFT,
    Key::KEY_LEFTALT,
    Key::KEY_RIGHTALT,
    Key::KEY_LEFTMETA,
    Key::KEY_RIGHTMETA,
];

pub fn is_modifier(k: Key) -> bool {
    MODIFIERS.iter().any(|m| *m == k)
}

const LABELS: &[(Key, &str)] = &[
    (Key::KEY_LEFTCTRL, "Ctrl"),
    (Key::KEY_RIGHTCTRL, "Ctrl"),
    (Key::KEY_LEFTSHIFT, "Shift"),
    (Key::KEY_RIGHTSHIFT, "Shift"),
    (Key::KEY_LEFTALT, "Alt"),
    (Key::KEY_RIGHTALT, "AltGr"),
    (Key::KEY_LEFTMETA, "Super"),
    (Key::KEY_RIGHTMETA, "Super"),
    (Key::KEY_ESC, "Esc"),
    (Key::KEY_TAB, "Tab"),
    (Key::KEY_CAPSLOCK, "Caps"),
    (Key::KEY_ENTER, "Enter"),
    (Key::KEY_KPENTER, "Enter"),
    (Key::KEY_SPACE, "Space"),
    (Key::KEY_BACKSPACE, "Backspace"),
    (Key::KEY_DELETE, "Del"),
    (Key::KEY_INSERT, "Ins"),
    (Key::KEY_HOME, "Home"),
    (Key::KEY_END, "End"),
    (Key::KEY_PAGEUP, "PgUp"),
    (Key::KEY_PAGEDOWN, "PgDn"),
    (Key::KEY_UP, "Up"),
    (Key::KEY_DOWN, "Down"),
    (Key::KEY_LEFT, "Left"),
    (Key::KEY_RIGHT, "Right"),
    (Key::KEY_F1, "F1"),
    (Key::KEY_F2, "F2"),
    (Key::KEY_F3, "F3"),
    (Key::KEY_F4, "F4"),
    (Key::KEY_F5, "F5"),
    (Key::KEY_F6, "F6"),
    (Key::KEY_F7, "F7"),
    (Key::KEY_F8, "F8"),
    (Key::KEY_F9, "F9"),
    (Key::KEY_F10, "F10"),
    (Key::KEY_F11, "F11"),
    (Key::KEY_F12, "F12"),
    (Key::KEY_PRINT, "PrtSc"),
    (Key::KEY_SYSRQ, "PrtSc"),
    (Key::KEY_SCROLLLOCK, "ScrLk"),
    (Key::KEY_PAUSE, "Pause"),
    (Key::KEY_NUMLOCK, "NumLk"),
    (Key::KEY_A, "A"),
    (Key::KEY_B, "B"),
    (Key::KEY_C, "C"),
    (Key::KEY_D, "D"),
    (Key::KEY_E, "E"),
    (Key::KEY_F, "F"),
    (Key::KEY_G, "G"),
    (Key::KEY_H, "H"),
    (Key::KEY_I, "I"),
    (Key::KEY_J, "J"),
    (Key::KEY_K, "K"),
    (Key::KEY_L, "L"),
    (Key::KEY_M, "M"),
    (Key::KEY_N, "N"),
    (Key::KEY_O, "O"),
    (Key::KEY_P, "P"),
    (Key::KEY_Q, "Q"),
    (Key::KEY_R, "R"),
    (Key::KEY_S, "S"),
    (Key::KEY_T, "T"),
    (Key::KEY_U, "U"),
    (Key::KEY_V, "V"),
    (Key::KEY_W, "W"),
    (Key::KEY_X, "X"),
    (Key::KEY_Y, "Y"),
    (Key::KEY_Z, "Z"),
    (Key::KEY_0, "0"),
    (Key::KEY_1, "1"),
    (Key::KEY_2, "2"),
    (Key::KEY_3, "3"),
    (Key::KEY_4, "4"),
    (Key::KEY_5, "5"),
    (Key::KEY_6, "6"),
    (Key::KEY_7, "7"),
    (Key::KEY_8, "8"),
    (Key::KEY_9, "9"),
    (Key::KEY_KP0, "0"),
    (Key::KEY_KP1, "1"),
    (Key::KEY_KP2, "2"),
    (Key::KEY_KP3, "3"),
    (Key::KEY_KP4, "4"),
    (Key::KEY_KP5, "5"),
    (Key::KEY_KP6, "6"),
    (Key::KEY_KP7, "7"),
    (Key::KEY_KP8, "8"),
    (Key::KEY_KP9, "9"),
    (Key::KEY_MINUS, "-"),
    (Key::KEY_KPMINUS, "-"),
    (Key::KEY_EQUAL, "="),
    (Key::KEY_KPEQUAL, "="),
    (Key::KEY_KPPLUS, "+"),
    (Key::KEY_KPASTERISK, "*"),
    (Key::KEY_KPSLASH, "/"),
    (Key::KEY_SLASH, "/"),
    (Key::KEY_LEFTBRACE, "["),
    (Key::KEY_RIGHTBRACE, "]"),
    (Key::KEY_SEMICOLON, ";"),
    (Key::KEY_APOSTROPHE, "'"),
    (Key::KEY_GRAVE, "`"),
    (Key::KEY_BACKSLASH, "\\"),
    (Key::KEY_COMMA, ","),
    (Key::KEY_DOT, "."),
    (Key::KEY_KPDOT, "."),
    (Key::KEY_VOLUMEUP, "Vol+"),
    (Key::KEY_VOLUMEDOWN, "Vol-"),
    (Key::KEY_MUTE, "Mute"),
    (Key::KEY_BRIGHTNESSUP, "Br+"),
    (Key::KEY_BRIGHTNESSDOWN, "Br-"),
    (Key::KEY_PLAYPAUSE, "Play"),
    (Key::KEY_NEXTSONG, "Next"),
    (Key::KEY_PREVIOUSSONG, "Prev"),
    (Key::KEY_STOPCD, "Stop"),
];

pub fn label(k: Key) -> &'static str {
    for (key, lbl) in LABELS {
        if *key == k {
            return lbl;
        }
    }
    "?"
}

pub fn modifier_order(k: Key) -> u8 {
    if k == Key::KEY_LEFTCTRL || k == Key::KEY_RIGHTCTRL {
        0
    } else if k == Key::KEY_LEFTSHIFT || k == Key::KEY_RIGHTSHIFT {
        1
    } else if k == Key::KEY_LEFTALT || k == Key::KEY_RIGHTALT {
        2
    } else if k == Key::KEY_LEFTMETA || k == Key::KEY_RIGHTMETA {
        3
    } else {
        99
    }
}
