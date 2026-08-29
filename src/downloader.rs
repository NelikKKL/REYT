use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::settings::DownloadSettings;

// --- Встроенные бинарники --------------------------------------------------
// Перед сборкой положите реальные исполняемые файлы в папку bin/:
//   Windows: bin/yt-dlp.exe и bin/ffmpeg.exe
//   Linux/macOS: bin/yt-dlp и bin/ffmpeg (с правами на исполнение не обязательны на этапе include)
//
// Если файлов нет, замените include_bytes! на include_bytes! указывающий на
// пустышку (см. README) — тогда приложение попросит пользователя указать
// путь к yt-dlp/ffmpeg вручную через переменные окружения YTDLP_PATH/FFMPEG_PATH.

#[cfg(target_os = "windows")]
const YTDLP_BIN: &[u8] = include_bytes!("../bin/yt-dlp.exe");
#[cfg(target_os = "windows")]
const FFMPEG_BIN: &[u8] = include_bytes!("../bin/ffmpeg.exe");
#[cfg(target_os = "windows")]
const YTDLP_NAME: &str = "yt-dlp.exe";
#[cfg(target_os = "windows")]
const FFMPEG_NAME: &str = "ffmpeg.exe";

#[cfg(not(target_os = "windows"))]
const YTDLP_BIN: &[u8] = include_bytes!("../bin/yt-dlp");
#[cfg(not(target_os = "windows"))]
const FFMPEG_BIN: &[u8] = include_bytes!("../bin/ffmpeg");
#[cfg(not(target_os = "windows"))]
const YTDLP_NAME: &str = "yt-dlp";
#[cfg(not(target_os = "windows"))]
const FFMPEG_NAME: &str = "ffmpeg";

/// Извлекает встроенные бинарники во внутреннюю папку приложения (один раз,
/// повторно — только если содержимое изменилось между версиями).
/// Возвращает (папка_с_бинарниками, путь_к_ytdlp).
pub fn ensure_binaries_extracted() -> anyhow::Result<(PathBuf, PathBuf)> {
    let base = dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("yt-dlp-gui")
        .join("bin");
    std::fs::create_dir_all(&base)?;

    let ytdlp_path = base.join(YTDLP_NAME);
    let ffmpeg_path = base.join(FFMPEG_NAME);

    write_if_changed(&ytdlp_path, YTDLP_BIN)?;
    write_if_changed(&ffmpeg_path, FFMPEG_BIN)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ytdlp_path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::set_permissions(&ffmpeg_path, std::fs::Permissions::from_mode(0o755))?;
    }

    Ok((base, ytdlp_path))
}

fn write_if_changed(path: &PathBuf, data: &[u8]) -> anyhow::Result<()> {
    let needs_write = match std::fs::read(path) {
        Ok(existing) => hash(&existing) != hash(data),
        Err(_) => true,
    };
    if needs_write {
        std::fs::write(path, data)?;
    }
    Ok(())
}

fn hash(data: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

// --- Запуск и парсинг вывода yt-dlp -----------------------------------------

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Log(String),
    Progress {
        percent: f32,
        speed: String,
        eta: String,
        item: String,
    },
    Finished(Result<(), String>),
}

pub struct RunningDownload {
    pub child: Child,
}

impl RunningDownload {
    pub fn cancel(&mut self) {
        let _ = self.child.kill();
    }
}

/// Запускает yt-dlp в отдельном потоке, отправляя события через channel.
/// Возвращает handle процесса, чтобы его можно было отменить.
pub fn spawn_download(
    ytdlp_path: PathBuf,
    ffmpeg_dir: PathBuf,
    settings: DownloadSettings,
    tx: Sender<DownloadEvent>,
) -> anyhow::Result<Child> {
    let args = settings.build_args(&ffmpeg_dir.to_string_lossy());

    std::fs::create_dir_all(&settings.output_dir)?;

    let mut cmd = Command::new(&ytdlp_path);
    cmd.args(&args)
        // Форсируем UTF-8 для stdout/stderr Python-процесса yt-dlp.
        // Без этого на Windows, если системная кодовая страница консоли
        // (напр. cp1251), а в названии видео/логе встречается символ,
        // не представимый в этой кодировке, Python падает с
        // "OSError: [Errno 22] Invalid argument" при попытке что-либо напечатать.
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let tx_out = tx.clone();
    std::thread::spawn(move || read_stream(stdout, tx_out));

    let tx_err = tx.clone();
    std::thread::spawn(move || read_stream(stderr, tx_err));

    Ok(child)
}

/// Неблокирующая проверка, завершился ли процесс. Вызывается из UI-потока
/// каждый кадр (egui update). Если процесс завершился — шлёт Finished и
/// возвращает true (вызывающий должен убрать running_child).
pub fn poll_finished(child: &mut Child, tx: &Sender<DownloadEvent>) -> bool {
    match child.try_wait() {
        Ok(Some(status)) => {
            let result = if status.success() {
                Ok(())
            } else {
                Err(format!("yt-dlp завершился с кодом {}", status))
            };
            let _ = tx.send(DownloadEvent::Finished(result));
            true
        }
        Ok(None) => false,
        Err(e) => {
            let _ = tx.send(DownloadEvent::Finished(Err(format!(
                "Ошибка ожидания процесса: {e}"
            ))));
            true
        }
    }
}

fn read_stream<R: std::io::Read + Send + 'static>(stream: R, tx: Sender<DownloadEvent>) {
    let progress_re =
        Regex::new(r"\[download\]\s+(\d{1,3}\.\d)%.*?of.*?at\s+([^\s]+)\s+ETA\s+([^\s]+)")
            .unwrap();
    let item_re = Regex::new(r"\[download\] Downloading item (\d+) of (\d+)").unwrap();

    let reader = BufReader::new(stream);
    let mut current_item = String::new();

    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }

        if let Some(caps) = item_re.captures(&line) {
            current_item = format!("{}/{}", &caps[1], &caps[2]);
        }

        if let Some(caps) = progress_re.captures(&line) {
            let percent: f32 = caps[1].parse().unwrap_or(0.0);
            let speed = caps[2].to_string();
            let eta = caps[3].to_string();
            let _ = tx.send(DownloadEvent::Progress {
                percent,
                speed,
                eta,
                item: current_item.clone(),
            });
        }

        let _ = tx.send(DownloadEvent::Log(line));
    }
}

/// Утилита для канала, использующаяся в app.rs
pub type EventReceiver = Receiver<DownloadEvent>;

// --- Анализ ссылки (название, превью, кол-во видео в плейлисте) ------------

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub title: String,
    pub thumbnail_url: Option<String>,
    /// Заполнено, только если ссылка ведёт на плейлист.
    pub playlist_count: Option<u64>,
    /// Реально найденные звуковые дорожки (язык + пометка), взятые из
    /// списка форматов, которые yt-dlp вернул при анализе.
    pub audio_tracks: Vec<AudioTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Код языка, как его отдал yt-dlp (напр. "ru", "en-US").
    pub code: String,
    /// Доп. пометка формата, если есть (напр. "original", "dubbed").
    pub note: Option<String>,
}

impl AudioTrack {
    /// Человекочитаемая подпись для выпадающего списка.
    pub fn display_label(&self) -> String {
        let name = language_name(&self.code).unwrap_or(&self.code);
        match &self.note {
            Some(note) if !note.trim().is_empty() => format!("{name} — {note}"),
            _ => name.to_string(),
        }
    }
}

/// Небольшой словарь распространённых языковых кодов для читаемых подписей.
/// Если код не найден — показываем его как есть.
fn language_name(code: &str) -> Option<&'static str> {
    let base = code.split(['-', '_']).next().unwrap_or(code).to_lowercase();
    Some(match base.as_str() {
        "ru" => "Русский",
        "en" => "Английский",
        "uk" => "Украинский",
        "es" => "Испанский",
        "fr" => "Французский",
        "de" => "Немецкий",
        "it" => "Итальянский",
        "pt" => "Португальский",
        "ja" => "Японский",
        "ko" => "Корейский",
        "zh" => "Китайский",
        "ar" => "Арабский",
        "hi" => "Хинди",
        "tr" => "Турецкий",
        "pl" => "Польский",
        _ => return None,
    })
}

/// Извлекает уникальные звуковые дорожки из списка форматов в JSON-ответе
/// yt-dlp (поле "formats"). Берём только форматы, где действительно есть
/// аудиодорожка (acodec != "none"), и у которых указан язык.
fn extract_audio_tracks(value: &serde_json::Value) -> Vec<AudioTrack> {
    let mut seen = std::collections::HashSet::new();
    let mut tracks = Vec::new();

    let Some(formats) = value.get("formats").and_then(|f| f.as_array()) else {
        return tracks;
    };

    for format in formats {
        let has_audio = format
            .get("acodec")
            .and_then(|v| v.as_str())
            .map(|s| s != "none")
            .unwrap_or(false);
        if !has_audio {
            continue;
        }

        let Some(lang) = format.get("language").and_then(|v| v.as_str()) else {
            continue;
        };
        if lang.trim().is_empty() || !seen.insert(lang.to_string()) {
            continue;
        }

        let note = format
            .get("format_note")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        tracks.push(AudioTrack {
            code: lang.to_string(),
            note,
        });
    }

    tracks
}

/// Быстро получает метаданные по ссылке без скачивания самого видео.
/// Используем `--playlist-items 1`, чтобы не парсить весь плейлист целиком —
/// yt-dlp всё равно кладёт в JSON первого элемента поле `playlist_count`,
/// если ссылка ведёт на плейлист.
pub fn analyze_url(ytdlp_path: &PathBuf, url: &str) -> anyhow::Result<VideoInfo> {
    let mut cmd = Command::new(ytdlp_path);
    cmd.args([
        "--skip-download",
        "--no-warnings",
        "--no-progress",
        "--playlist-items",
        "1",
        "--dump-single-json",
        url,
    ])
    .env("PYTHONIOENCODING", "utf-8")
    .env("PYTHONUTF8", "1")
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last_line = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("не удалось получить информацию о ссылке");
        anyhow::bail!("{last_line}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // На всякий случай берём первую строку, похожую на JSON-объект.
    let json_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or(stdout.as_ref());
    let value: serde_json::Value = serde_json::from_str(json_line)?;

    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Без названия")
        .to_string();

    let thumbnail_url = value
        .get("thumbnail")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            value
                .get("thumbnails")
                .and_then(|t| t.as_array())
                .and_then(|arr| arr.last())
                .and_then(|t| t.get("url"))
                .and_then(|u| u.as_str())
                .map(String::from)
        });

    let playlist_count = value.get("playlist_count").and_then(|v| v.as_u64());
    let audio_tracks = extract_audio_tracks(&value);

    Ok(VideoInfo {
        title,
        thumbnail_url,
        playlist_count,
        audio_tracks,
    })
}

/// Скачивает превью-изображение по URL и декодирует его в сырые RGBA-байты,
/// готовые к загрузке в текстуру egui.
pub fn fetch_thumbnail(url: &str) -> anyhow::Result<(Vec<u8>, [usize; 2])> {
    let response = ureq::get(url).call()?;
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;

    let image = image::load_from_memory(&bytes)?.to_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), [width as usize, height as usize]))
}
