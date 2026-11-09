//! Keys travel as PC scan codes (set 1), the form Windows injects directly.
//! Keys are matched by their position on the keyboard, so the host's own
//! keyboard layout decides which character a key types.

/// Set on the scan codes of extended keys (arrows, right Ctrl, the Windows key).
pub const EXTENDED: u16 = 0xE000;

pub const LEFT_CTRL: u16 = 0x1D;
pub const LEFT_WIN: u16 = EXTENDED | 0x5B;

/// Shift, Ctrl, Alt and the Windows key, left or right.
pub fn is_modifier(scancode: u16) -> bool {
    const E: u16 = EXTENDED;
    [0x1D, 0x2A, 0x36, 0x38, E | 0x1D, E | 0x38, E | 0x5B, E | 0x5C].contains(&scancode)
}

/// The scan code of a key of the viewer's keyboard. With `mac_layout` the
/// left Command key acts as Ctrl (so Command-C copies on the Windows computer)
/// and the right Command key as the Windows key.
#[cfg(any(target_os = "macos", windows))]
pub fn scancode(key: tao::keyboard::KeyCode, mac_layout: bool) -> Option<u16> {
    use tao::keyboard::KeyCode as K;
    const E: u16 = EXTENDED;
    Some(match key {
        K::SuperLeft if mac_layout => LEFT_CTRL,
        K::SuperRight if mac_layout => LEFT_WIN,
        K::Escape => 0x01,
        K::Digit1 => 0x02,
        K::Digit2 => 0x03,
        K::Digit3 => 0x04,
        K::Digit4 => 0x05,
        K::Digit5 => 0x06,
        K::Digit6 => 0x07,
        K::Digit7 => 0x08,
        K::Digit8 => 0x09,
        K::Digit9 => 0x0A,
        K::Digit0 => 0x0B,
        K::Minus => 0x0C,
        K::Equal => 0x0D,
        K::Backspace => 0x0E,
        K::Tab => 0x0F,
        K::KeyQ => 0x10,
        K::KeyW => 0x11,
        K::KeyE => 0x12,
        K::KeyR => 0x13,
        K::KeyT => 0x14,
        K::KeyY => 0x15,
        K::KeyU => 0x16,
        K::KeyI => 0x17,
        K::KeyO => 0x18,
        K::KeyP => 0x19,
        K::BracketLeft => 0x1A,
        K::BracketRight => 0x1B,
        K::Enter => 0x1C,
        K::ControlLeft => LEFT_CTRL,
        K::KeyA => 0x1E,
        K::KeyS => 0x1F,
        K::KeyD => 0x20,
        K::KeyF => 0x21,
        K::KeyG => 0x22,
        K::KeyH => 0x23,
        K::KeyJ => 0x24,
        K::KeyK => 0x25,
        K::KeyL => 0x26,
        K::Semicolon => 0x27,
        K::Quote => 0x28,
        K::Backquote => 0x29,
        K::ShiftLeft => 0x2A,
        K::Backslash => 0x2B,
        K::KeyZ => 0x2C,
        K::KeyX => 0x2D,
        K::KeyC => 0x2E,
        K::KeyV => 0x2F,
        K::KeyB => 0x30,
        K::KeyN => 0x31,
        K::KeyM => 0x32,
        K::Comma => 0x33,
        K::Period => 0x34,
        K::Slash => 0x35,
        K::ShiftRight => 0x36,
        K::NumpadMultiply => 0x37,
        K::AltLeft => 0x38,
        K::Space => 0x39,
        K::CapsLock => 0x3A,
        K::F1 => 0x3B,
        K::F2 => 0x3C,
        K::F3 => 0x3D,
        K::F4 => 0x3E,
        K::F5 => 0x3F,
        K::F6 => 0x40,
        K::F7 => 0x41,
        K::F8 => 0x42,
        K::F9 => 0x43,
        K::F10 => 0x44,
        K::NumLock => 0x45,
        K::ScrollLock => 0x46,
        K::Numpad7 => 0x47,
        K::Numpad8 => 0x48,
        K::Numpad9 => 0x49,
        K::NumpadSubtract => 0x4A,
        K::Numpad4 => 0x4B,
        K::Numpad5 => 0x4C,
        K::Numpad6 => 0x4D,
        K::NumpadAdd => 0x4E,
        K::Numpad1 => 0x4F,
        K::Numpad2 => 0x50,
        K::Numpad3 => 0x51,
        K::Numpad0 => 0x52,
        K::NumpadDecimal => 0x53,
        K::IntlBackslash => 0x56,
        K::F11 => 0x57,
        K::F12 => 0x58,
        K::NumpadEnter => E | 0x1C,
        K::ControlRight => E | 0x1D,
        K::NumpadDivide => E | 0x35,
        K::PrintScreen => E | 0x37,
        K::AltRight => E | 0x38,
        K::Home => E | 0x47,
        K::ArrowUp => E | 0x48,
        K::PageUp => E | 0x49,
        K::ArrowLeft => E | 0x4B,
        K::ArrowRight => E | 0x4D,
        K::End => E | 0x4F,
        K::ArrowDown => E | 0x50,
        K::PageDown => E | 0x51,
        K::Insert => E | 0x52,
        K::Delete => E | 0x53,
        K::SuperLeft => LEFT_WIN,
        K::SuperRight => E | 0x5C,
        K::ContextMenu => E | 0x5D,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_are_told_apart_from_other_keys() {
        for scancode in [
            0x1D,
            0x2A,
            0x36,
            0x38,
            EXTENDED | 0x1D,
            EXTENDED | 0x38,
            LEFT_WIN,
            EXTENDED | 0x5C,
        ] {
            assert!(is_modifier(scancode), "{scancode:#x}");
        }
        // C, Enter, keypad Enter, the arrow that shares 0x38's row, the menu key.
        for scancode in [0x2E, 0x1C, EXTENDED | 0x1C, EXTENDED | 0x48, EXTENDED | 0x5D, 0x5B] {
            assert!(!is_modifier(scancode), "{scancode:#x}");
        }
    }
}
