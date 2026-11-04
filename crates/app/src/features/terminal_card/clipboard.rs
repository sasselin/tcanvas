//! Lecture du presse-papiers du système, pour coller ailleurs que par Ctrl+V (que egui gère seul) :
//! Ctrl+Maj+V et le clic droit. Remplaçable dans les tests.

pub trait ClipboardSource {
    fn text(&mut self) -> Option<String>;
}

/// Presse-papiers du système (Windows, X11…). Ouvert à chaque lecture : rien à garder entre deux.
pub struct System;

impl ClipboardSource for System {
    fn text(&mut self) -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }
}

/// Contenu fixe (tests, ou pas de vrai terminal).
pub struct Fixed(pub Option<String>);

impl ClipboardSource for Fixed {
    fn text(&mut self) -> Option<String> {
        self.0.clone()
    }
}
