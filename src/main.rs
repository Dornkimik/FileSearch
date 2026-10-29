// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icons;
mod index;
mod search;
mod theme;
mod util;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("File Search")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([760.0, 440.0])
            .with_icon(util::app_icon()),
        ..Default::default()
    };
    eframe::run_native("File Search", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
