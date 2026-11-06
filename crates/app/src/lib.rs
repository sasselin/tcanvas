//! tcanvas: all your WSL terminals on an infinite canvas.

pub mod features {
    pub mod agent_link;
    pub mod canvas;
    pub mod changes;
    pub mod filters;
    pub mod jump;
    pub mod notifications;
    pub mod persistence;
    pub mod projects;
    pub mod spaces;
    pub mod terminal_card;
    pub mod wires;
}
pub mod shell;

pub use shell::{App, Config};

pub fn run() -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon-256.png")).expect("assets/icon-256.png is invalid");
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("tcanvas")
            // Wayland and X11 match it with `tcanvas.desktop` (taskbar icon, grouping).
            .with_app_id("tcanvas")
            .with_icon(icon)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native("tcanvas", options, Box::new(|cc| Ok(Box::new(App::new(&cc.egui_ctx, Config::production())))))
}
