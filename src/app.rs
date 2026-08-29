use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::{channel, Receiver, Sender};

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke};

use crate::downloader::{self, DownloadEvent};
use crate::settings::{AudioFormat, DownloadSettings, PlaylistMode, Resolution, VideoContainer};

pub struct YtDlpApp {
    settings: DownloadSettings,

    ytdlp_path: Option<PathBuf>,
    ffmpeg_dir: Option<PathBuf>,
    setup_error: Option<String>,

    running_child: Option<Child>,
    rx: Option<Receiver<DownloadEvent>>,
    tx: Option<Sender<DownloadEvent>>,

    log: Vec<String>,
    progress: f32,
    speed: String,
    eta: String,
    playlist_item: String,
    status: Status,

    show_advanced: bool,
}

#[derive(PartialEq)]
enum Status {
    Idle,
    Running,
    Done,
    Error(String),
}

impl YtDlpApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_dark_theme(&cc.egui_ctx);

        let (ytdlp_path, ffmpeg_dir, setup_error) = match downloader::ensure_binaries_extracted()
        {
            Ok((dir, ytdlp)) => (Some(ytdlp), Some(dir), None),
            Err(e) => (None, None, Some(format!("Не удалось подготовить yt-dlp/ffmpeg: {e}"))),
        };

        Self {
            settings: DownloadSettings::default(),
            ytdlp_path,
            ffmpeg_dir,
            setup_error,
            running_child: None,
            rx: None,
            tx: None,
            log: Vec::new(),
            progress: 0.0,
            speed: String::new(),
            eta: String::new(),
            playlist_item: String::new(),
            status: Status::Idle,
            show_advanced: false,
        }
    }

    fn start_download(&mut self) {
        let (Some(ytdlp), Some(ffmpeg_dir)) = (self.ytdlp_path.clone(), self.ffmpeg_dir.clone())
        else {
            self.status = Status::Error("yt-dlp/ffmpeg не готовы".into());
            return;
        };
        if self.settings.url.trim().is_empty() {
            self.status = Status::Error("Укажите ссылку".into());
            return;
        }

        self.log.clear();
        self.progress = 0.0;
        self.speed.clear();
        self.eta.clear();
        self.status = Status::Running;

        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.tx = Some(tx.clone());

        match downloader::spawn_download(ytdlp, ffmpeg_dir, self.settings.clone(), tx) {
            Ok(child) => {
                self.running_child = Some(child);
            }
            Err(e) => {
                self.status = Status::Error(format!("Не удалось запустить yt-dlp: {e}"));
            }
        }
    }

    fn cancel_download(&mut self) {
        if let Some(child) = self.running_child.as_mut() {
            let _ = child.kill();
        }
        self.running_child = None;
        self.status = Status::Idle;
        self.log.push("⏹ Загрузка отменена пользователем".into());
    }

    fn poll_events(&mut self) {
        // Неблокирующая проверка: не завершился ли процесс yt-dlp.
        if let (Some(child), Some(tx)) = (self.running_child.as_mut(), self.tx.as_ref()) {
            if downloader::poll_finished(child, tx) {
                self.running_child = None;
            }
        }

        let Some(rx) = &self.rx else { return };
        while let Ok(event) = rx.try_recv() {
            match event {
                DownloadEvent::Log(line) => {
                    self.log.push(line);
                    if self.log.len() > 2000 {
                        self.log.drain(0..500);
                    }
                }
                DownloadEvent::Progress {
                    percent,
                    speed,
                    eta,
                    item,
                } => {
                    self.progress = percent / 100.0;
                    self.speed = speed;
                    self.eta = eta;
                    self.playlist_item = item;
                }
                DownloadEvent::Finished(result) => {
                    self.running_child = None;
                    self.status = match result {
                        Ok(()) => Status::Done,
                        Err(e) => Status::Error(e),
                    };
                }
            }
        }
    }
}

impl eframe::App for YtDlpApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        if matches!(self.status, Status::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("yt-dlp GUI").strong());
                ui.label(RichText::new("портативный загрузчик видео").weak());
            });
            ui.add_space(6.0);
        });

        if let Some(err) = &self.setup_error {
            egui::TopBottomPanel::top("setup_error").show(ctx, |ui| {
                ui.colored_label(Color32::LIGHT_RED, err);
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.ui_source_section(ui);
                ui.add_space(10.0);
                self.ui_video_audio_section(ui);
                ui.add_space(10.0);
                self.ui_embed_section(ui);
                ui.add_space(10.0);
                self.ui_advanced_section(ui);
                ui.add_space(14.0);
                self.ui_actions(ui);
                ui.add_space(10.0);
                self.ui_progress(ui);
                ui.add_space(10.0);
                self.ui_log(ui);
            });
        });
    }
}

impl YtDlpApp {
    fn section_frame(&self) -> egui::Frame {
        egui::Frame::group(&egui::Style::default())
            .fill(Color32::from_rgb(30, 30, 34))
            .rounding(Rounding::same(10.0))
            .inner_margin(egui::Margin::same(12.0))
            .stroke(Stroke::new(1.0, Color32::from_rgb(50, 50, 56)))
    }

    fn ui_source_section(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.label(RichText::new("Источник").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Ссылка (видео или плейлист):");
            });
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.url)
                    .hint_text("https://www.youtube.com/watch?v=... или ссылка на плейлист")
                    .desired_width(f32::INFINITY),
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Папка загрузки:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.output_dir)
                        .desired_width(ui.available_width() - 90.0),
                );
                if ui.button("Обзор…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.settings.output_dir = dir.to_string_lossy().to_string();
                    }
                }
            });

            ui.add_space(8.0);
            egui::ComboBox::from_label("Режим плейлиста")
                .selected_text(self.settings.playlist_mode.label())
                .show_ui(ui, |ui| {
                    for mode in PlaylistMode::ALL {
                        let label = mode.label();
                        ui.selectable_value(&mut self.settings.playlist_mode, mode, label);
                    }
                });
            ui.horizontal(|ui| {
                ui.label("Элементы плейлиста (напр. 1-10,15):");
                ui.text_edit_singleline(&mut self.settings.playlist_items);
            });
        });
    }

    fn ui_video_audio_section(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.label(RichText::new("Видео и аудио").strong());
            ui.add_space(4.0);

            ui.checkbox(&mut self.settings.audio_only, "Скачивать только аудио");

            ui.add_space(6.0);
            ui.add_enabled_ui(!self.settings.audio_only, |ui| {
                egui::ComboBox::from_label("Разрешение видео")
                    .selected_text(self.settings.resolution.label())
                    .show_ui(ui, |ui| {
                        for res in Resolution::ALL {
                            let label = res.label();
                            ui.selectable_value(&mut self.settings.resolution, res, label);
                        }
                    });
                egui::ComboBox::from_label("Контейнер видео")
                    .selected_text(self.settings.video_container.label())
                    .show_ui(ui, |ui| {
                        for c in VideoContainer::ALL {
                            let label = c.label();
                            ui.selectable_value(&mut self.settings.video_container, c, label);
                        }
                    });
            });

            ui.add_space(6.0);
            egui::ComboBox::from_label("Формат аудио")
                .selected_text(self.settings.audio_format.label())
                .show_ui(ui, |ui| {
                    for a in AudioFormat::ALL {
                        let label = a.label();
                        ui.selectable_value(&mut self.settings.audio_format, a, label);
                    }
                });

            ui.horizontal(|ui| {
                ui.label("Язык звуковой дорожки (код, напр. ru, en; пусто = любой):");
                ui.text_edit_singleline(&mut self.settings.audio_language);
            });
        });
    }

    fn ui_embed_section(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.label(RichText::new("Встраивание").strong());
            ui.add_space(4.0);
            ui.checkbox(&mut self.settings.embed_thumbnail, "Встроить обложку");
            ui.checkbox(&mut self.settings.embed_metadata, "Встроить метаданные (название, автор и т.д.)");
            ui.checkbox(&mut self.settings.embed_chapters, "Встроить главы (если есть)");
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.settings.embed_subtitles, "Встроить субтитры, язык(и):");
                ui.add_enabled(
                    self.settings.embed_subtitles,
                    egui::TextEdit::singleline(&mut self.settings.subtitle_language)
                        .desired_width(80.0),
                );
            });
        });
    }

    fn ui_advanced_section(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Дополнительно").strong());
                if ui
                    .button(if self.show_advanced { "Скрыть" } else { "Показать" })
                    .clicked()
                {
                    self.show_advanced = !self.show_advanced;
                }
            });
            if self.show_advanced {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Шаблон имени файла:");
                    ui.text_edit_singleline(&mut self.settings.filename_template);
                });
                ui.horizontal(|ui| {
                    ui.label("Ограничение скорости (напр. 2M, пусто = без лимита):");
                    ui.text_edit_singleline(&mut self.settings.rate_limit);
                });
                ui.label("Произвольные аргументы yt-dlp (через пробел):");
                ui.text_edit_singleline(&mut self.settings.extra_args);
            }
        });
    }

    fn ui_actions(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let running = matches!(self.status, Status::Running);
            if ui
                .add_enabled(
                    !running,
                    egui::Button::new(RichText::new("  Скачать  ").strong())
                        .fill(Color32::from_rgb(80, 140, 255)),
                )
                .clicked()
            {
                self.start_download();
            }
            if ui
                .add_enabled(running, egui::Button::new("Отменить"))
                .clicked()
            {
                self.cancel_download();
            }

            match &self.status {
                Status::Idle => {}
                Status::Running => {
                    ui.label(RichText::new("⏳ Загрузка…").color(Color32::LIGHT_BLUE));
                }
                Status::Done => {
                    ui.label(RichText::new("✅ Готово").color(Color32::LIGHT_GREEN));
                }
                Status::Error(e) => {
                    ui.label(RichText::new(format!("❌ {e}")).color(Color32::LIGHT_RED));
                }
            }
        });
    }

    fn ui_progress(&mut self, ui: &mut egui::Ui) {
        if matches!(self.status, Status::Running) || self.progress > 0.0 {
            ui.add(egui::ProgressBar::new(self.progress).show_percentage());
            ui.horizontal(|ui| {
                if !self.playlist_item.is_empty() {
                    ui.label(format!("Элемент: {}", self.playlist_item));
                }
                if !self.speed.is_empty() {
                    ui.label(format!("Скорость: {}", self.speed));
                }
                if !self.eta.is_empty() {
                    ui.label(format!("Осталось: {}", self.eta));
                }
            });
        }
    }

    fn ui_log(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Лог").strong());
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace().size(12.0));
                }
            });
    }
}

fn apply_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(22, 22, 26);
    visuals.window_fill = Color32::from_rgb(22, 22, 26);
    visuals.extreme_bg_color = Color32::from_rgb(18, 18, 21);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 38, 44);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(52, 52, 60);
    visuals.widgets.active.bg_fill = Color32::from_rgb(80, 140, 255);
    visuals.selection.bg_fill = Color32::from_rgb(80, 140, 255);
    visuals.window_rounding = Rounding::same(10.0);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}
