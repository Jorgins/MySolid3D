// Не открываем чёрное консольное окно рядом с программой.
#![windows_subsystem = "windows"]

mod app;
mod camera;
mod model;
mod render;
mod svg;

use eframe::egui;

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MySolid3D")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([800.0, 500.0]);
    // Иконка окна и панели задач.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/app_icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        multisampling: 4,
        depth_buffer: 24,
        ..Default::default()
    };
    eframe::run_native("MySolid3D", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)?))))
}
