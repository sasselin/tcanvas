//! Fonts: egui's own (including monochrome emoji), Hack as a fallback for the
//! proportional font, and the system's symbol fonts when they exist.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Symbols used by the UI; a test checks that they exist in the embedded fonts.
pub const UI_SYMBOLS: &str = "+●○■×◆⇄·—…⏸⌨✖✔⚠🔔";

/// System fonts tried as a last resort (symbols and rare scripts in terminals).
const SYSTEM_FALLBACKS: &[&str] = &[
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\msgothic.ttc",
    // Linux (Debian/Ubuntu paths, then Fedora/Arch ones): symbols Claude Code draws (`⎿`, `⏵`).
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansMath-Regular.ttf",
    "/usr/share/fonts/google-noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/freefont/FreeMono.ttf",
];

pub fn install(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    if let Some(prop) = defs.families.get_mut(&FontFamily::Proportional) {
        prop.insert(1, "Hack".to_owned());
    }
    for path in SYSTEM_FALLBACKS {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = format!("system:{path}");
        defs.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families.entry(family).or_default().push(name.clone());
        }
    }
    ctx.set_fonts(defs);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontId;

    #[test]
    fn ui_symbols_are_covered_by_bundled_fonts() {
        let ctx = egui::Context::default();
        // Embedded fonts only: the result must not depend on the machine.
        let mut defs = FontDefinitions::default();
        defs.families.get_mut(&FontFamily::Proportional).unwrap().insert(1, "Hack".to_owned());
        ctx.set_fonts(defs);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for family in [FontId::proportional(14.0), FontId::monospace(14.0)] {
                for c in UI_SYMBOLS.chars() {
                    assert!(ui.ctx().fonts_mut(|f| f.has_glyph(&family, c)), "{c:?} missing from {family:?}");
                }
            }
        });
        out.textures_delta.clear();
    }
}
