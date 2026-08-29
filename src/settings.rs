use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoContainer {
    Best,
    Mp4,
    Mkv,
    Webm,
}

impl VideoContainer {
    pub const ALL: [VideoContainer; 4] = [
        VideoContainer::Best,
        VideoContainer::Mp4,
        VideoContainer::Mkv,
        VideoContainer::Webm,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            VideoContainer::Best => "Автоматически (лучший)",
            VideoContainer::Mp4 => "MP4",
            VideoContainer::Mkv => "MKV",
            VideoContainer::Webm => "WebM",
        }
    }

    /// Значение для флага --merge-output-format / --remux-video
    fn ext(&self) -> Option<&'static str> {
        match self {
            VideoContainer::Best => None,
            VideoContainer::Mp4 => Some("mp4"),
            VideoContainer::Mkv => Some("mkv"),
            VideoContainer::Webm => Some("webm"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    Best,
    R2160,
    R1440,
    R1080,
    R720,
    R480,
    R360,
    Worst,
}

impl Resolution {
    pub const ALL: [Resolution; 8] = [
        Resolution::Best,
        Resolution::R2160,
        Resolution::R1440,
        Resolution::R1080,
        Resolution::R720,
        Resolution::R480,
        Resolution::R360,
        Resolution::Worst,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Resolution::Best => "Максимальное",
            Resolution::R2160 => "2160p (4K)",
            Resolution::R1440 => "1440p (2K)",
            Resolution::R1080 => "1080p",
            Resolution::R720 => "720p",
            Resolution::R480 => "480p",
            Resolution::R360 => "360p",
            Resolution::Worst => "Минимальное",
        }
    }

    fn height_filter(&self) -> Option<u32> {
        match self {
            Resolution::Best | Resolution::Worst => None,
            Resolution::R2160 => Some(2160),
            Resolution::R1440 => Some(1440),
            Resolution::R1080 => Some(1080),
            Resolution::R720 => Some(720),
            Resolution::R480 => Some(480),
            Resolution::R360 => Some(360),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Best,
    Mp3,
    M4a,
    Opus,
    Wav,
    Flac,
}

impl AudioFormat {
    pub const ALL: [AudioFormat; 6] = [
        AudioFormat::Best,
        AudioFormat::Mp3,
        AudioFormat::M4a,
        AudioFormat::Opus,
        AudioFormat::Wav,
        AudioFormat::Flac,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            AudioFormat::Best => "Оригинальный",
            AudioFormat::Mp3 => "MP3",
            AudioFormat::M4a => "M4A (AAC)",
            AudioFormat::Opus => "Opus",
            AudioFormat::Wav => "WAV",
            AudioFormat::Flac => "FLAC",
        }
    }

    fn code(&self) -> Option<&'static str> {
        match self {
            AudioFormat::Best => None,
            AudioFormat::Mp3 => Some("mp3"),
            AudioFormat::M4a => Some("m4a"),
            AudioFormat::Opus => Some("opus"),
            AudioFormat::Wav => Some("wav"),
            AudioFormat::Flac => Some("flac"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlaylistMode {
    /// Скачивать плейлист целиком, если ссылка на плейлист
    Auto,
    /// Всегда трактовать ссылку как плейлист (--yes-playlist)
    ForcePlaylist,
    /// Скачивать только конкретное видео, даже если ссылка содержит плейлист (--no-playlist)
    SingleVideoOnly,
}

impl PlaylistMode {
    pub const ALL: [PlaylistMode; 3] = [
        PlaylistMode::Auto,
        PlaylistMode::ForcePlaylist,
        PlaylistMode::SingleVideoOnly,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            PlaylistMode::Auto => "Авто (спросить yt-dlp)",
            PlaylistMode::ForcePlaylist => "Всегда скачивать весь плейлист",
            PlaylistMode::SingleVideoOnly => "Только одно видео",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadSettings {
    pub url: String,
    pub output_dir: String,

    pub audio_only: bool,
    pub video_container: VideoContainer,
    pub resolution: Resolution,
    pub audio_format: AudioFormat,

    /// Код языка звуковой дорожки, например "ru", "en". Пусто = не важно.
    pub audio_language: String,

    pub embed_thumbnail: bool,
    pub embed_subtitles: bool,
    pub subtitle_language: String,
    pub embed_metadata: bool,
    pub embed_chapters: bool,

    pub playlist_mode: PlaylistMode,
    /// Ограничение диапазона элементов плейлиста, например "1-10,15"
    pub playlist_items: String,

    /// Шаблон имени файла (без пути) в формате yt-dlp output template
    pub filename_template: String,

    /// Ограничение скорости, например "2M" (пусто = без ограничений)
    pub rate_limit: String,

    /// Доп. произвольные аргументы командной строки для тонкой настройки
    pub extra_args: String,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            url: String::new(),
            output_dir: dirs::download_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| ".".to_string()),
            audio_only: false,
            video_container: VideoContainer::Mp4,
            resolution: Resolution::Best,
            audio_format: AudioFormat::Best,
            audio_language: String::new(),
            embed_thumbnail: true,
            embed_subtitles: false,
            subtitle_language: "ru".to_string(),
            embed_metadata: true,
            embed_chapters: false,
            playlist_mode: PlaylistMode::Auto,
            playlist_items: String::new(),
            filename_template: "%(title)s.%(ext)s".to_string(),
            rate_limit: String::new(),
            extra_args: String::new(),
        }
    }
}

impl DownloadSettings {
    /// Собирает список аргументов для запуска yt-dlp на основе текущих настроек.
    /// ytdlp_dir/ffmpeg_dir — пути к папкам с встроенными бинарниками.
    pub fn build_args(&self, ffmpeg_dir: &str) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();

        // Прогресс в удобном для парсинга виде
        args.push("--newline".into());
        args.push("--no-mtime".into());
        args.push("--ffmpeg-location".into());
        args.push(ffmpeg_dir.to_string());

        // Папка + шаблон имени
        let output_template = format!(
            "{}/{}",
            self.output_dir.trim_end_matches(['/', '\\']),
            if self.filename_template.trim().is_empty() {
                "%(title)s.%(ext)s"
            } else {
                self.filename_template.trim()
            }
        );
        args.push("-o".into());
        args.push(output_template);

        // Плейлист
        match self.playlist_mode {
            PlaylistMode::Auto => {}
            PlaylistMode::ForcePlaylist => args.push("--yes-playlist".into()),
            PlaylistMode::SingleVideoOnly => args.push("--no-playlist".into()),
        }
        if !self.playlist_items.trim().is_empty() {
            args.push("--playlist-items".into());
            args.push(self.playlist_items.trim().to_string());
        }

        if self.audio_only {
            // Режим "только аудио"
            args.push("-x".into()); // --extract-audio
            if let Some(code) = self.audio_format.code() {
                args.push("--audio-format".into());
                args.push(code.to_string());
            }
            args.push("--audio-quality".into());
            args.push("0".into()); // лучшее качество перекодирования

            let mut fmt = "bestaudio/best".to_string();
            if !self.audio_language.trim().is_empty() {
                fmt = format!(
                    "bestaudio[language={0}]/bestaudio/best",
                    self.audio_language.trim()
                );
            }
            args.push("-f".into());
            args.push(fmt);
        } else {
            // Видео: собираем format selector.
            // Строим список АЛЬТЕРНАТИВ явно (а не пытаемся встроить fallback
            // внутрь одной альтернативы через "a/b+c") — в синтаксисе yt-dlp
            // "+" имеет более высокий приоритет, чем "/", поэтому
            // "bestvideo+bestaudio[language=ru]/bestaudio/best" на самом деле
            // означает "видео+ауд(ru) ИЛИ просто ауд(любой, без видео!) ИЛИ best" —
            // что могло приводить к скачиванию одного аудио без видео, если
            // нужный язык недоступен. Явный список альтернатив это исключает.
            let height = self.resolution.height_filter();
            let lang = self.audio_language.trim();
            let lang_filter = if lang.is_empty() {
                String::new()
            } else {
                format!("[language={lang}]")
            };
            let height_filter = height.map(|h| format!("[height<={h}]")).unwrap_or_default();

            let mut alts: Vec<String> = Vec::new();

            if self.resolution == Resolution::Worst {
                if !lang_filter.is_empty() {
                    alts.push(format!("worstvideo+worstaudio{lang_filter}"));
                }
                alts.push("worstvideo+worstaudio".to_string());
                alts.push("worst".to_string());
            } else {
                match self.video_container {
                    // MP4 нативно поддерживает только H.264/H.265 видео и
                    // AAC(m4a) аудио без перекодирования. Если просто взять
                    // "bestaudio" (это часто Opus в контейнере WebM) и потом
                    // силой засунуть его в mp4 через --remux-video, звук
                    // может потеряться или ffmpeg завершится с ошибкой.
                    // Поэтому для MP4 сначала просим совместимые форматы.
                    VideoContainer::Mp4 => {
                        if !lang_filter.is_empty() {
                            alts.push(format!(
                                "bestvideo[ext=mp4]{height_filter}+bestaudio[ext=m4a]{lang_filter}"
                            ));
                        }
                        alts.push(format!(
                            "bestvideo[ext=mp4]{height_filter}+bestaudio[ext=m4a]"
                        ));
                        alts.push(format!("best[ext=mp4]{height_filter}"));
                        if !lang_filter.is_empty() {
                            alts.push(format!("bestvideo{height_filter}+bestaudio{lang_filter}"));
                        }
                        alts.push(format!("bestvideo{height_filter}+bestaudio"));
                    }
                    _ => {
                        if !lang_filter.is_empty() {
                            alts.push(format!("bestvideo{height_filter}+bestaudio{lang_filter}"));
                        }
                        alts.push(format!("bestvideo{height_filter}+bestaudio"));
                    }
                }
                alts.push("best".to_string());
            }

            args.push("-f".into());
            args.push(alts.join("/"));

            if let Some(ext) = self.video_container.ext() {
                // Только merge-output-format: он просит ffmpeg смержить уже
                // подобранные (совместимые) потоки в нужный контейнер.
                // Дополнительный --remux-video здесь не нужен — раньше он
                // как раз приводил к потере звука и коду ошибки 1.
                args.push("--merge-output-format".into());
                args.push(ext.to_string());
            }
        }

        if self.embed_thumbnail {
            args.push("--embed-thumbnail".into());
        }
        if self.embed_metadata {
            args.push("--embed-metadata".into());
        }
        if self.embed_chapters {
            args.push("--embed-chapters".into());
        }
        if self.embed_subtitles {
            args.push("--embed-subs".into());
            args.push("--sub-langs".into());
            let lang = if self.subtitle_language.trim().is_empty() {
                "all".to_string()
            } else {
                self.subtitle_language.trim().to_string()
            };
            args.push(lang);
        }

        if !self.rate_limit.trim().is_empty() {
            args.push("-r".into());
            args.push(self.rate_limit.trim().to_string());
        }

        if !self.extra_args.trim().is_empty() {
            for token in self.extra_args.split_whitespace() {
                args.push(token.to_string());
            }
        }

        args.push(self.url.trim().to_string());

        args
    }
}
