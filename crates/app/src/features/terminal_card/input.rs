//! Keyboard/mouse → bytes sent to the terminal (xterm sequences).

use egui::{Key, Modifiers};

/// xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
fn xterm_mod(m: Modifiers) -> u8 {
    1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8
}

/// Bytes for a special key or a Ctrl combination. `None`: the key produces
/// text (handled by `Event::Text`) or means nothing to a terminal.
pub fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let modded = m.shift || m.alt || m.ctrl;
    let csi_letter = |c: char, ss3_in_app: bool| -> Vec<u8> {
        if modded {
            format!("\x1b[1;{}{c}", xterm_mod(m)).into_bytes()
        } else if ss3_in_app && app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> { if modded { format!("\x1b[{n};{}~", xterm_mod(m)).into_bytes() } else { format!("\x1b[{n}~").into_bytes() } };
    let alt_prefix = |bytes: &[u8]| -> Vec<u8> { if m.alt { [b"\x1b".as_slice(), bytes].concat() } else { bytes.to_vec() } };

    let bytes = match key {
        Key::ArrowUp => csi_letter('A', true),
        Key::ArrowDown => csi_letter('B', true),
        Key::ArrowRight => csi_letter('C', true),
        Key::ArrowLeft => csi_letter('D', true),
        Key::Home => csi_letter('H', true),
        Key::End => csi_letter('F', true),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => csi_or_ss3('P', m),
        Key::F2 => csi_or_ss3('Q', m),
        Key::F3 => csi_or_ss3('R', m),
        Key::F4 => csi_or_ss3('S', m),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        Key::Enter => alt_prefix(b"\r"),
        Key::Escape => b"\x1b".to_vec(),
        Key::Tab if m.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Backspace if m.ctrl => b"\x08".to_vec(),
        Key::Backspace => alt_prefix(b"\x7f"),
        _ if m.ctrl && !m.alt => return ctrl_byte(key).map(|b| vec![b]),
        _ => return None,
    };
    Some(bytes)
}

fn csi_or_ss3(c: char, m: Modifiers) -> Vec<u8> {
    if m.shift || m.alt || m.ctrl { format!("\x1b[1;{}{c}", xterm_mod(m)).into_bytes() } else { format!("\x1bO{c}").into_bytes() }
}

/// Ctrl+letter → 0x01..0x1a, plus the classic punctuation combinations.
fn ctrl_byte(key: Key) -> Option<u8> {
    let name = key.name();
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        if c.is_ascii_uppercase() {
            return Some(c - b'A' + 1);
        }
    }
    Some(match key {
        Key::Space | Key::Num2 => 0x00,
        Key::OpenBracket | Key::Num3 => 0x1b,
        Key::Backslash | Key::Num4 => 0x1c,
        Key::CloseBracket | Key::Num5 => 0x1d,
        Key::Num6 => 0x1e,
        Key::Minus | Key::Slash | Key::Num7 => 0x1f,
        Key::Num8 => 0x7f,
        _ => return None,
    })
}

/// Typed text; (left) Alt prefixes ESC, as in xterm with `metaSendsEscape`.
pub fn text_bytes(text: &str, m: Modifiers) -> Vec<u8> {
    if m.alt && !m.ctrl { [b"\x1b".as_slice(), text.as_bytes()].concat() } else { text.as_bytes().to_vec() }
}

/// Terminal-specific clipboard shortcuts (like Windows Terminal): egui only knows
/// plain Ctrl+C/V, which a terminal must keep for ^C and the usual paste.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipAction {
    /// Ctrl+Shift+V: paste.
    Paste,
    /// Ctrl+Shift+C: the tmux selection is already copied on mouse release; above all, do
    /// not send ^C (which would interrupt the running command).
    Copy,
}

pub fn clip_action(key: Key, m: Modifiers) -> Option<ClipAction> {
    if !(m.ctrl && m.shift && !m.alt) {
        return None;
    }
    match key {
        Key::V => Some(ClipAction::Paste),
        Key::C => Some(ClipAction::Copy),
        _ => None,
    }
}

pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        // Strip any markers so a paste cannot escape the mode.
        let clean = text.replace("\x1b[201~", "");
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left = 0,
    Middle = 1,
    Right = 2,
    WheelUp = 64,
    WheelDown = 65,
}

/// xterm mouse report. `col`/`line` start at 0. `drag`: motion with a button held.
pub fn mouse_report(button: MouseButton, m: Modifiers, col: usize, line: usize, pressed: bool, drag: bool, sgr: bool) -> Vec<u8> {
    let mut code = button as u8 + 4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.ctrl as u8;
    if drag {
        code += 32;
    }
    if sgr {
        let end = if pressed { 'M' } else { 'm' };
        format!("\x1b[<{code};{};{}{end}", col + 1, line + 1).into_bytes()
    } else {
        let code = if pressed { code } else { 3 + (code & !3) };
        let enc = |v: usize| (32 + 1 + v).min(255) as u8;
        vec![0x1b, b'[', b'M', 32 + code, enc(col), enc(line)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers::NONE;
    const CTRL: Modifiers = Modifiers::CTRL;
    const ALT: Modifiers = Modifiers::ALT;
    const SHIFT: Modifiers = Modifiers::SHIFT;

    fn k(key: Key, m: Modifiers) -> Vec<u8> {
        key_bytes(key, m, false).unwrap()
    }

    #[test]
    fn control_keys() {
        assert_eq!(k(Key::Enter, NONE), b"\r");
        assert_eq!(k(Key::Backspace, NONE), b"\x7f");
        assert_eq!(k(Key::Backspace, ALT), b"\x1b\x7f");
        assert_eq!(k(Key::Tab, NONE), b"\t");
        assert_eq!(k(Key::Tab, SHIFT), b"\x1b[Z");
        assert_eq!(k(Key::Escape, NONE), b"\x1b");
    }

    #[test]
    fn ctrl_combinations() {
        assert_eq!(k(Key::C, CTRL), [0x03]);
        assert_eq!(k(Key::D, CTRL), [0x04]);
        assert_eq!(k(Key::Z, CTRL), [0x1a]);
        assert_eq!(k(Key::A, CTRL), [0x01]);
        assert_eq!(k(Key::OpenBracket, CTRL), [0x1b]);
        assert_eq!(k(Key::Space, CTRL), [0x00]);
        // A lone letter, or one with AltGr (Ctrl+Alt), goes through Event::Text.
        assert_eq!(key_bytes(Key::A, NONE, false), None);
        assert_eq!(key_bytes(Key::Num0, CTRL | ALT, false), None);
    }

    #[test]
    fn arrows_follow_cursor_mode_and_modifiers() {
        assert_eq!(k(Key::ArrowUp, NONE), b"\x1b[A");
        assert_eq!(key_bytes(Key::ArrowUp, NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(k(Key::ArrowLeft, CTRL), b"\x1b[1;5D");
        assert_eq!(k(Key::ArrowRight, ALT | SHIFT), b"\x1b[1;4C");
        assert_eq!(k(Key::Home, NONE), b"\x1b[H");
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(k(Key::Delete, NONE), b"\x1b[3~");
        assert_eq!(k(Key::PageUp, SHIFT), b"\x1b[5;2~");
        assert_eq!(k(Key::F1, NONE), b"\x1bOP");
        assert_eq!(k(Key::F5, NONE), b"\x1b[15~");
        assert_eq!(k(Key::F12, CTRL), b"\x1b[24;5~");
    }

    #[test]
    fn text_with_alt_is_escaped() {
        assert_eq!(text_bytes("é", NONE), "é".as_bytes());
        assert_eq!(text_bytes("b", ALT), b"\x1bb");
        assert_eq!(text_bytes("@", NONE), b"@");
    }

    #[test]
    fn paste_normalizes_newlines_and_brackets() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn terminal_clipboard_shortcuts() {
        let cs = CTRL | SHIFT;
        assert_eq!(clip_action(Key::V, cs), Some(ClipAction::Paste));
        assert_eq!(clip_action(Key::C, cs), Some(ClipAction::Copy));
        assert_eq!(clip_action(Key::V, CTRL), None, "Ctrl+V: egui's paste (Event::Paste)");
        assert_eq!(clip_action(Key::C, CTRL), None, "Ctrl+C stays ^C");
        assert_eq!(clip_action(Key::V, cs | ALT), None);
        assert_eq!(clip_action(Key::X, cs), None);
    }

    #[test]
    fn mouse_reports() {
        assert_eq!(mouse_report(MouseButton::Left, NONE, 0, 0, true, false, true), b"\x1b[<0;1;1M");
        assert_eq!(mouse_report(MouseButton::Left, NONE, 9, 4, false, false, true), b"\x1b[<0;10;5m");
        assert_eq!(mouse_report(MouseButton::Left, NONE, 2, 3, true, true, true), b"\x1b[<32;3;4M");
        assert_eq!(mouse_report(MouseButton::WheelDown, CTRL, 0, 0, true, false, true), b"\x1b[<81;1;1M");
        assert_eq!(mouse_report(MouseButton::Right, NONE, 0, 0, true, false, false), vec![0x1b, b'[', b'M', 34, 33, 33]);
        assert_eq!(mouse_report(MouseButton::Right, NONE, 0, 0, false, false, false), vec![0x1b, b'[', b'M', 35, 33, 33]);
    }
}
