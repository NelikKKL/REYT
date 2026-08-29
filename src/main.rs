// На Windows не открываем консольное окно вместе с GUI
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod downloader;
mod settings;

use app::YtDlpApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([880.0, 760.0])
            .with_min_inner_size([640.0, 500.0])
            .with_title("yt-dlp GUI"),
        ..Default::default()
    };

    eframe::run_native(
        "yt-dlp GUI",
        options,
        Box::new(|cc| Box::new(YtDlpApp::new(cc)) as Box<dyn eframe::App>),
    )
}
