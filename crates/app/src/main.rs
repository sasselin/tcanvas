// Pas de console noire derrière la fenêtre en version publiée.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> eframe::Result {
    tcanvas::run()
}
