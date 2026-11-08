//! Reads the system clipboard (text, else image), for pasting other than via Ctrl+V (which egui
//! handles itself, images included):
//! Ctrl+Shift+V and right-click. Replaceable in tests.

use egui::ColorImage;

pub trait ClipboardSource {
    fn text(&mut self) -> Option<String>;
    /// Image on the clipboard (screenshot, copied photo), read when there is no text.
    fn image(&mut self) -> Option<ColorImage> {
        None
    }
}

/// System clipboard (Windows, X11…). Opened on each read: nothing to keep in between.
pub struct System;

impl ClipboardSource for System {
    fn text(&mut self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }

    fn image(&mut self) -> Option<ColorImage> {
        let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
        Some(ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.bytes))
    }
}

/// Fixed content (tests, or no real terminal).
pub struct Fixed(pub Option<String>);

impl ClipboardSource for Fixed {
    fn text(&mut self) -> Option<String> {
        self.0.clone()
    }
}

/// Fixed image (tests).
pub struct FixedImage(pub ColorImage);

impl ClipboardSource for FixedImage {
    fn text(&mut self) -> Option<String> {
        None
    }

    fn image(&mut self) -> Option<ColorImage> {
        Some(self.0.clone())
    }
}
