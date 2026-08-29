use std::io::{BufRead, BufReader};
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
