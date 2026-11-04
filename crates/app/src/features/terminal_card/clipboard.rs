//! Reads the system clipboard, for pasting other than via Ctrl+V (which egui handles itself):
//! Ctrl+Shift+V and right-click. Replaceable in tests.

pub trait ClipboardSource {
    fn text(&mut self) -> Option<String>;
}

/// System clipboard (Windows, X11…). Opened on each read: nothing to keep in between.
pub struct System;

impl ClipboardSource for System {
    fn text(&mut self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }
}

/// Fixed content (tests, or no real terminal).
pub struct Fixed(pub Option<String>);

impl ClipboardSource for Fixed {
    fn text(&mut self) -> Option<String> {
        self.0.clone()
    }
}
