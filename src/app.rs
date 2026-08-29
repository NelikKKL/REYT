use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke};

use crate::downloader::{self, DownloadEvent};
use crate::settings::{AudioFormat, DownloadSettings, PlaylistMode, Resolution, VideoContainer};

/// Акцентный цвет интерфейса (#393636) — используется вместо синего.
const ACCENT_COLOR: Color32 = Color32::from_rgb(0x39, 0x36, 0x36);
const ACCENT_COLOR_HOVER: Color32 = Color32::from_rgb(0x4a, 0x46, 0x46);

pub struct YtDlpApp {
    settings: DownloadSettings,

    ytdlp_path: Option<PathBuf>,
    ffmpeg_dir: Option<PathBuf>,
    setup_error: Option<String>,

    running_child: Option<Child>,
    rx: Option<Receiver<DownloadEvent>>,
    tx: Option<Sender<DownloadEvent>>,
    log_file: Option<BufWriter<File>>,

    progress: f32,
    speed: String,
    eta: String,
    playlist_item: String,
    status: Status,

    // Анализ ссылки (название/превью/кол-во видео)
    analyze_status: AnalyzeStatus,
    analyze_rx: Option<Receiver<AnalyzeEvent>>,
    video_info: Option<VideoPreview>,
    thumbnail_texture: Option<egui::TextureHandle>,

    show_settings: bool,
    show_advanced: bool,
}

#[derive(PartialEq)]
enum Status {
    Idle,
    Running,
    Done,
    Error(String),
}

#[derive(PartialEq)]
enum AnalyzeStatus {
    Idle,
    Loading,
    Loaded,
    Error(String),
}

struct VideoPreview {
    title: String,
    playlist_count: Option<u64>,
}

struct ThumbnailData {
    rgba: Vec<u8>,
    size: [usize; 2],
}

struct AnalyzedInfo {
    title: String,
    playlist_count: Option<u64>,
    thumbnail: Option<ThumbnailData>,
}

type AnalyzeEvent = Result<AnalyzedInfo, String>;

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
            log_file: None,
            progress: 0.0,
            speed: String::new(),
            eta: String::new(),
            playlist_item: String::new(),
            status: Status::Idle,
            analyze_status: AnalyzeStatus::Idle,
            analyze_rx: None,
            video_info: None,
            thumbnail_texture: None,
            show_settings: false,
            show_advanced: false,
        }
    }

    fn log_line(&mut self, line: &str) {
        if let Some(writer) = self.log_file.as_mut() {
            let _ = writeln!(writer, "{line}");
            let _ = writer.flush();
        }
    }

    fn start_analyze(&mut self) {
        let Some(ytdlp) = self.ytdlp_path.clone() else {
            self.analyze_status = AnalyzeStatus::Error("yt-dlp не готов".into());
            return;
        };
        let url = self.settings.url.trim().to_string();
        if url.is_empty() {
            self.analyze_status = AnalyzeStatus::Error("Укажите ссылку".into());
            return;
        }

        self.analyze_status = AnalyzeStatus::Loading;
        self.video_info = None;
        self.thumbnail_texture = None;

        let (tx, rx) = channel();
        self.analyze_rx = Some(rx);

        std::thread::spawn(move || {
            let result: AnalyzeEvent = downloader::analyze_url(&ytdlp, &url)
                .map(|info| {
                    let thumbnail = info
                        .thumbnail_url
                        .as_deref()
                        .and_then(|u| downloader::fetch_thumbnail(u).ok())
                        .map(|(rgba, size)| ThumbnailData { rgba, size });
                    AnalyzedInfo {
                        title: info.title,
                        playlist_count: info.playlist_count,
                        thumbnail,
                    }
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
    }

    fn poll_analyze(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.analyze_rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.analyze_rx = None;

        match result {
            Ok(info) => {
                if let Some(thumb) = info.thumbnail {
                    let color_image =
                        egui::ColorImage::from_rgba_unmultiplied(thumb.size, &thumb.rgba);
                    let texture = ctx.load_texture(
                        "thumbnail",
                        color_image,
                        egui::TextureOptions::default(),
                    );
                    self.thumbnail_texture = Some(texture);
                }
                self.video_info = Some(VideoPreview {
                    title: info.title,
                    playlist_count: info.playlist_count,
                });
                self.analyze_status = AnalyzeStatus::Loaded;
            }
            Err(e) => {
                self.analyze_status = AnalyzeStatus::Error(e);
            }
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

        self.progress = 0.0;
        self.speed.clear();
        self.eta.clear();
        self.status = Status::Running;

        // Логи пишем не в интерфейс, а в файл рядом со скачанными видео.
        if std::fs::create_dir_all(&self.settings.output_dir).is_ok() {
            let log_path = PathBuf::from(&self.settings.output_dir)
                .join(format!("yt-dlp-gui_{}.log", timestamp()));
            self.log_file = File::create(&log_path).ok().map(BufWriter::new);
        } else {
            self.log_file = None;
        }

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
        self.log_line("⏹ Загрузка отменена пользователем");
    }

    fn poll_events(&mut self) {
        // Неблокирующая проверка: не завершился ли процесс yt-dlp.
        if let (Some(child), Some(tx)) = (self.running_child.as_mut(), self.tx.as_ref()) {
            if downloader::poll_finished(child, tx) {
                self.running_child = None;
            }
        }

        let Some(rx) = &self.rx else { return };
        let mut lines_to_log: Vec<String> = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                DownloadEvent::Log(line) => {
                    lines_to_log.push(line);
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
        for line in lines_to_log {
            self.log_line(&line);
        }
    }
}

impl eframe::App for YtDlpApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        self.poll_analyze(ctx);
        if matches!(self.status, Status::Running) || matches!(self.analyze_status, AnalyzeStatus::Loading) {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("yt-dlp GUI").strong());
                ui.label(RichText::new("портативный загрузчик видео").weak());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Настройки").clicked() {
                        self.show_settings = true;
                    }
                });
            });
            ui.add_space(6.0);
        });

        if let Some(err) = &self.setup_error {
            egui::TopBottomPanel::top("setup_error").show(ctx, |ui| {
                ui.colored_label(Color32::LIGHT_RED, err);
            });
        }

        self.ui_settings_window(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.ui_link_bar(ui);
                ui.add_space(10.0);
                self.ui_preview(ui);
                ui.add_space(14.0);
                self.ui_actions(ui);
                ui.add_space(10.0);
                self.ui_progress(ui);
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
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(50, 50, 56)))
    }

    /// Компактное поле ссылки + кнопка «Анализировать».
    fn ui_link_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.settings.url)
                    .hint_text("Ссылка на видео или плейлист")
                    .desired_width((ui.available_width() - 150.0).max(120.0)),
            );

            let analyzing = matches!(self.analyze_status, AnalyzeStatus::Loading);
            let button = egui::Button::new(if analyzing { "Анализ…" } else { "Анализировать" })
                .fill(ACCENT_COLOR);
            if ui.add_enabled(!analyzing, button).clicked() {
                self.start_analyze();
            }
        });

        if let AnalyzeStatus::Error(e) = &self.analyze_status {
            ui.add_space(4.0);
            ui.colored_label(Color32::LIGHT_RED, e);
        }
    }

    /// Превью: миниатюра + название + кол-во видео (для плейлиста).
    fn ui_preview(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                let size = egui::vec2(160.0, 90.0);

                if let Some(texture) = &self.thumbnail_texture {
                    let image = egui::Image::new(texture).fit_to_exact_size(size);
                    let response = ui.add(image);

                    if let Some(count) = self.video_info.as_ref().and_then(|i| i.playlist_count) {
                        let rect = response.rect;
                        let badge_size = egui::vec2(56.0, 20.0);
                        let badge_rect = egui::Rect::from_min_size(
                            rect.right_bottom() - badge_size - egui::vec2(4.0, 4.0),
                            badge_size,
                        );
                        ui.painter().rect_filled(
                            badge_rect,
                            Rounding::same(4.0),
                            Color32::from_rgba_unmultiplied(0, 0, 0, 190),
                        );
                        ui.painter().text(
                            badge_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{count} шт"),
                            egui::FontId::proportional(11.0),
                            Color32::WHITE,
                        );
                    }
                } else {
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, Rounding::same(6.0), Color32::from_rgb(38, 38, 44));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Превью",
                        egui::FontId::proportional(14.0),
                        Color32::GRAY,
                    );
                }

                ui.add_space(10.0);
                ui.vertical(|ui| match &self.video_info {
                    Some(info) => {
                        ui.label(RichText::new(&info.title).strong());
                        if let Some(count) = info.playlist_count {
                            ui.label(
                                RichText::new(format!("Плейлист · {count} видео")).weak(),
                            );
                        }
                    }
                    None => {
                        ui.label(
                            RichText::new("Введите ссылку и нажмите «Анализировать»").weak(),
                        );
                    }
                });
            });
        });
    }

    /// Окно настроек (видео/аудио/встраивание/плейлист/доп. параметры),
    /// открывается кнопкой «⚙ Настройки» в верхней панели.
    fn ui_settings_window(&mut self, ctx: &egui::Context) {
        let mut show_settings = self.show_settings;
        egui::Window::new("Настройки")
            .open(&mut show_settings)
            .resizable(true)
            .collapsible(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                    self.ui_output_section(ui);
                    ui.add_space(10.0);
                    self.ui_video_audio_section(ui);
                    ui.add_space(10.0);
                    self.ui_embed_section(ui);
                    ui.add_space(10.0);
                    self.ui_advanced_section(ui);
                });
            });
        self.show_settings = show_settings;
    }

    fn ui_output_section(&mut self, ui: &mut egui::Ui) {
        self.section_frame().show(ui, |ui| {
            ui.label(RichText::new("Источник и папка").strong());
            ui.add_space(4.0);
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
                        .fill(ACCENT_COLOR),
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
                    ui.label(RichText::new("⏳ Загрузка…").color(Color32::LIGHT_GRAY));
                }
                Status::Done => {
                    ui.label(RichText::new("✅ Готово").color(Color32::LIGHT_GREEN));
                }
                Status::Error(e) => {
                    ui.label(RichText::new(format!("❌ {e}")).color(Color32::LIGHT_RED));
                }
            }
        });

        if matches!(self.status, Status::Running | Status::Done) {
            ui.label(
                RichText::new("Лог сохраняется рядом со скачанными файлами (*.log)").weak(),
            );
        }
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
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn apply_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(22, 22, 26);
    visuals.window_fill = Color32::from_rgb(22, 22, 26);
    visuals.extreme_bg_color = Color32::from_rgb(18, 18, 21);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(38, 38, 44);
    visuals.widgets.hovered.bg_fill = ACCENT_COLOR_HOVER;
    visuals.widgets.active.bg_fill = ACCENT_COLOR;
    visuals.selection.bg_fill = ACCENT_COLOR;
    visuals.window_rounding = Rounding::same(10.0);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}
