//! Polices : celles d'egui (dont les emoji monochromes), Hack en secours de la police
//! proportionnelle, et les polices de symboles du système quand elles existent.

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// Symboles utilisés par l'interface ; un test vérifie qu'ils existent dans les polices embarquées.
pub const UI_SYMBOLS: &str = "+●○■×◆⇄·—…⏸⌨✖✔⚠🔔";

/// Polices système essayées en dernier recours (symboles et écritures rares des terminaux).
const SYSTEM_FALLBACKS: &[&str] = &[
    r"C:\Windows\Fonts\seguisym.ttf",
    r"C:\Windows\Fonts\msgothic.ttc",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
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
        // Seulement les polices embarquées : le résultat ne doit pas dépendre de la machine.
        let mut defs = FontDefinitions::default();
        defs.families.get_mut(&FontFamily::Proportional).unwrap().insert(1, "Hack".to_owned());
        ctx.set_fonts(defs);
        let mut out = ctx.run_ui(Default::default(), |ui| {
            for family in [FontId::proportional(14.0), FontId::monospace(14.0)] {
                for c in UI_SYMBOLS.chars() {
                    assert!(ui.ctx().fonts_mut(|f| f.has_glyph(&family, c)), "{c:?} absent de {family:?}");
                }
            }
        });
        out.textures_delta.clear();
    }
}
