use futures_util::{future::try_join_all, StreamExt};
use reqwest::{header, Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex},
    time::Instant,
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter},
    sync::Semaphore,
};
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;
#[cfg(target_os = "ios")]
use std::hash::{Hash, Hasher};
#[cfg(any(target_os = "android", target_os = "ios"))]
use tauri_plugin_media::{MediaDownloadRequest, MediaExt, MediaProgress};
#[cfg(target_os = "android")]
use tauri_plugin_media::{MediaCancelRequest, MediaInspectRequest, MediaOpenRequest, MediaPublishRequest};

#[derive(Clone)]
struct DownloadManager {
    records: Arc<Mutex<HashMap<String, DownloadRecord>>>,
    slots: Arc<Semaphore>,
    persistence_file: Arc<Mutex<Option<PathBuf>>>,
    http_client: Arc<Client>,
}
impl Default for DownloadManager {
    fn default() -> Self {
        Self {
            records: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(Semaphore::new(2)),
            persistence_file: Arc::new(Mutex::new(None)),
            http_client: Arc::new(build_http_client()),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct PersistedDownload {
    id: String, url: String, file_name: String, kind: String, status: String,
    destination: String, file_path: Option<String>, downloaded_bytes: u64,
    total_bytes: Option<u64>, error: Option<String>, concurrency: usize,
    etag: Option<String>, last_modified: Option<String>,
    #[serde(default)]
    cookies_file: Option<String>,
    #[serde(default)]
    format_selector: Option<String>,
    #[serde(default)]
    media_title: Option<String>,
    #[serde(default)]
    media_thumbnail: Option<String>,
    #[serde(default)]
    cancel_requested: bool,
}

#[cfg(target_os = "ios")]
#[derive(Serialize, Deserialize, Clone)]
struct HlsResumeState {
    signature: u64,
    next_segment: usize,
    committed_bytes: u64,
}

struct DownloadRecord {
    id: String, url: String, file_name: String, kind: String, status: String,
    destination: String, file_path: Option<String>, downloaded: Arc<AtomicU64>, total: Option<u64>,
    started: Instant, error: Option<String>, cancel: CancellationToken,
    progress_override: Option<f64>, speed_bytes_sec: u64, concurrency: usize,
    etag: Option<String>, last_modified: Option<String>,
    cookies_file: Option<String>,
    format_selector: Option<String>,
    media_title: Option<String>,
    media_thumbnail: Option<String>,
    media_activity: Option<String>,
    cancel_requested: Arc<AtomicBool>,
}

fn build_http_client() -> Client {
    Client::builder()
        .user_agent("DownBest/1.0 (+https://github.com/nazim-assdi/DownBest)")
        .connect_timeout(std::time::Duration::from_secs(20))
        // Keep TLS/TCP connections warm and reusable between separate queue items.
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .pool_max_idle_per_host(32)
        .http2_adaptive_window(true)
        .tcp_keepalive(std::time::Duration::from_secs(30))
        .tcp_nodelay(true)
        .default_headers({
            let mut headers = header::HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, header::HeaderValue::from_static("identity"));
            headers
        })
        .build()
        .unwrap_or_else(|_| Client::new())
}

impl DownloadManager {
    fn restore_queue(&self, path: PathBuf) -> Vec<String> {
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(mut slot) = self.persistence_file.lock() { *slot = Some(path.clone()); }
        let fallback = path.with_extension("json.tmp");
        let bytes = std::fs::read(&path).or_else(|_| std::fs::read(&fallback));
        let Ok(bytes) = bytes else { return Vec::new(); };
        let Ok(mut items) = serde_json::from_slice::<Vec<PersistedDownload>>(&bytes) else { return Vec::new(); };
        // Prefer completed output when resolving legacy duplicate queue entries, then paused/failed
        // jobs, then interrupted jobs. HashMap persistence order is otherwise nondeterministic.
        items.sort_by_key(|item| match item.status.as_str() {
            "Completed" => 0,
            "Paused" | "Failed" => 1,
            "Queued" | "Connecting" | "Extracting media" | "Downloading" | "Retrying" => 2,
            _ => 3,
        });
        let mut seen_media = HashSet::new();
        let mut cancelled_ids = Vec::new();
        let mut auto_resume_ids = Vec::new();
        if let Ok(mut records) = self.records.lock() {
            for item in items {
                let was_cancelled = item.cancel_requested;
                let was_interrupted = matches!(item.status.as_str(),
                    "Queued" | "Connecting" | "Extracting media" | "Downloading" | "Retrying");
                // A final output can be committed just before the app exits but before
                // the queue status is persisted as Completed. Recover that file rather
                // than automatically downloading the same media again on next launch.
                let recovered_output = if was_interrupted {
                    item.file_path.as_deref().map(PathBuf::from).filter(|path| path.is_file())
                        .or_else(|| {
                            #[cfg(not(any(target_os = "android", target_os = "ios")))]
                            {
                                find_completed_media_output(&PathBuf::from(&item.destination), &item.id)
                            }
                            #[cfg(any(target_os = "android", target_os = "ios"))]
                            {
                                None
                            }
                        })
                } else {
                    None
                };
                let recovered_file = recovered_output.is_some();
                let recovered_output_path = recovered_output.as_ref()
                    .map(|path| path.to_string_lossy().into_owned());
                let recovered_bytes = recovered_output.as_ref()
                    .and_then(|path| std::fs::metadata(path).ok())
                    .map(|metadata| metadata.len())
                    .unwrap_or(item.downloaded_bytes);
                let is_completed = item.status == "Completed" || recovered_file;
                let identity = Url::parse(&item.url).ok().map(|url| download_identity_key(&url));
                let duplicate_interrupted_job = if was_cancelled {
                    false
                } else if is_completed {
                    if let Some(key) = identity { seen_media.insert(key); }
                    false
                } else if let Some(key) = identity {
                    !seen_media.insert(key)
                } else {
                    false
                };
                let should_cancel = was_cancelled || duplicate_interrupted_job;
                let status = if should_cancel {
                    "Cancelled".to_string()
                } else if is_completed {
                    "Completed".to_string()
                } else if was_interrupted {
                    "Queued".to_string()
                } else {
                    match item.status.as_str() {
                        "Completed" | "Failed" | "Cancelled" | "Paused" => item.status.clone(),
                        _ => "Paused".to_string(),
                    }
                };
                let id = item.id.clone();
                records.insert(id.clone(), DownloadRecord {
                    id: item.id, url: item.url, file_name: item.file_name, kind: item.kind,
                    status, destination: item.destination, file_path: recovered_output_path.or(item.file_path),
                    downloaded: Arc::new(AtomicU64::new(recovered_bytes)),
                    total: if recovered_file { Some(recovered_bytes) } else { item.total_bytes },
                    started: Instant::now(),
                    error: if recovered_file { None } else { item.error },
                    cancel: CancellationToken::new(),
                    progress_override: if is_completed { Some(100.0) } else { None },
                    speed_bytes_sec: 0,
                    concurrency: item.concurrency.clamp(1, 16),
                    etag: item.etag, last_modified: item.last_modified,
                    cookies_file: item.cookies_file,
                    format_selector: item.format_selector,
                    media_title: item.media_title,
                    media_thumbnail: item.media_thumbnail,
                    media_activity: None,
                    cancel_requested: Arc::new(AtomicBool::new(should_cancel)),
                });
                if should_cancel { cancelled_ids.push(id); }
                else if was_interrupted && !is_completed { auto_resume_ids.push(id); }
            }
        }
        for id in cancelled_ids {
            cleanup_cancelled_files(self, &id);
            cleanup_job_cookies(self, &id);
        }
        persist_records(self);
        auto_resume_ids
    }
}

fn persist_records(manager: &DownloadManager) {
    let path = manager.persistence_file.lock().ok().and_then(|slot| slot.clone());
    let Some(path) = path else { return; };
    let items = {
        let Ok(records) = manager.records.lock() else { return; };
        records.values().map(|record| PersistedDownload {
            id: record.id.clone(), url: record.url.clone(), file_name: record.file_name.clone(),
            kind: record.kind.clone(), status: record.status.clone(),
            destination: record.destination.clone(), file_path: record.file_path.clone(),
            downloaded_bytes: record.downloaded.load(Ordering::Relaxed),
            total_bytes: record.total, error: record.error.clone(), concurrency: record.concurrency,
            etag: record.etag.clone(), last_modified: record.last_modified.clone(),
            cookies_file: record.cookies_file.clone(),
            format_selector: record.format_selector.clone(),
            media_title: record.media_title.clone(),
            media_thumbnail: record.media_thumbnail.clone(),
            cancel_requested: record.cancel_requested.load(Ordering::Relaxed),
        }).collect::<Vec<_>>()
    };
    let Ok(bytes) = serde_json::to_vec(&items) else { return; };
    if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
    let temp = path.with_extension("json.tmp");
    if std::fs::write(&temp, bytes).is_ok() {
        // Flush the queue snapshot before the atomic rename so a sudden shutdown
        // is less likely to leave only an empty or partially written queue file.
        let synced = std::fs::OpenOptions::new().write(true).open(&temp)
            .and_then(|file| file.sync_all());
        if synced.is_err() {
            return;
        }
        if std::fs::rename(&temp, &path).is_err() {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::rename(&temp, &path);
        }
        #[cfg(unix)]
        if let Some(parent) = path.parent() {
            if let Ok(directory) = std::fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
    }
}

fn write_job_cookies(app: &AppHandle, id: &str, contents: Option<String>) -> Result<Option<String>, String> {
    let Some(contents) = contents else { return Ok(None); };
    if contents.trim().is_empty() { return Ok(None); }
    if contents.len() > 4 * 1024 * 1024 {
        return Err("The cookies.txt file is too large (maximum 4 MiB).".into());
    }
    let has_cookie_rows = contents.lines().any(|line| {
        let row = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        !row.trim().is_empty() && !row.starts_with('#') && row.split('\t').count() >= 7
    });
    if !has_cookie_rows {
        return Err("Invalid cookies.txt file. Export cookies in Netscape format and try again.".into());
    }
    let directory = app.path().app_data_dir()
        .map_err(|error| format!("Could not locate the private app data folder: {error}"))?
        .join("cookies");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create private cookies folder: {error}"))?;
    let path = directory.join(format!("{id}.txt"));
    std::fs::write(&path, contents)
        .map_err(|error| format!("Could not save the imported cookies file: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("Could not protect the imported cookies file: {error}"))?;
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

fn cleanup_job_cookies(manager: &DownloadManager, id: &str) {
    let path = if let Ok(mut records) = manager.records.lock() {
        records.get_mut(id).and_then(|record| record.cookies_file.take())
    } else { None };
    if let Some(path) = path { let _ = std::fs::remove_file(path); }
    persist_records(manager);
}

fn cleanup_cancelled_files(manager: &DownloadManager, id: &str) {
    let (destination, file_path) = match manager.records.lock() {
        Ok(records) => match records.get(id) {
            Some(record) => (PathBuf::from(&record.destination), record.file_path.clone()),
            None => return,
        },
        Err(_) => return,
    };

    if let Some(path) = file_path {
        let partial = PathBuf::from(format!("{path}.downbest-{id}.tmp"));
        let _ = std::fs::remove_file(partial);
        let checkpoint = PathBuf::from(format!("{path}.downbest-{id}.hls.json"));
        let _ = std::fs::remove_file(&checkpoint);
        let checkpoint_staging = checkpoint.with_extension("hls.tmp");
        let _ = std::fs::remove_file(checkpoint_staging);
    }
    let parts = destination.join(format!(".downbest-{id}"));
    let _ = std::fs::remove_dir_all(parts);

    let site_job_suffix = if cfg!(target_os = "android") { id.to_string() } else { id.chars().take(8).collect() };
    let marker = format!(" - {site_job_suffix}.");
    if let Ok(entries) = std::fs::read_dir(&destination) {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if let Some(name) = file_name.to_str() {
                if name.contains(&marker) {
                    let path = entry.path();
                    if path.is_file() { let _ = std::fs::remove_file(path); }
                    else if path.is_dir() { let _ = std::fs::remove_dir_all(path); }
                }
            }
        }
    }
}

fn partial_bytes_for(record: &DownloadRecord) -> u64 {
    let mut bytes = 0u64;
    let mut found_partial = false;
    if let Some(target) = record.file_path.as_ref() {
        let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target, record.id));
        if let Ok(meta) = std::fs::metadata(temp) {
            bytes = meta.len();
            found_partial = true;
        }
    }
    let parts = PathBuf::from(&record.destination).join(format!(".downbest-{}", record.id));
    if let Ok(entries) = std::fs::read_dir(parts) {
        for entry in entries.flatten() {
            bytes = bytes.saturating_add(entry.metadata().map(|meta| meta.len()).unwrap_or(0));
            found_partial = true;
        }
    }
    if found_partial { bytes } else { record.downloaded.load(Ordering::Relaxed) }
}

#[derive(Serialize, Clone)]
struct DownloadView {
    id: String, url: String, file_name: String, kind: String, status: String,
    destination: String, file_path: Option<String>, downloaded_bytes: u64, total_bytes: Option<u64>,
    speed_bytes_sec: u64, progress: f64, error: Option<String>,
    media_title: Option<String>, media_thumbnail: Option<String>, media_activity: Option<String>,
}
#[derive(Serialize)]
struct ToolStatus { name: String, available: bool, version: Option<String> }

fn view_of(record: &DownloadRecord) -> DownloadView {
    let downloaded = record.downloaded.load(Ordering::Relaxed);
    let calculated = record.total.filter(|n| *n > 0)
        .map(|n| (downloaded as f64 / n as f64 * 100.0).clamp(0.0, 100.0)).unwrap_or(0.0);
    let progress = record.progress_override.unwrap_or(calculated);
    let elapsed = record.started.elapsed().as_secs_f64().max(0.5);
    let is_media_transfer = record.media_activity.is_some() || record.media_title.is_some();
    let average_speed = (downloaded as f64 / elapsed) as u64;
    DownloadView {
        id: record.id.clone(), url: record.url.clone(), file_name: record.file_name.clone(),
        kind: record.kind.clone(), status: record.status.clone(), destination: record.destination.clone(),
        file_path: record.file_path.clone(), downloaded_bytes: downloaded, total_bytes: record.total,
        speed_bytes_sec: if matches!(record.status.as_str(), "Downloading" | "Extracting media") {
            if record.speed_bytes_sec > 0 {
                record.speed_bytes_sec
            } else if is_media_transfer {
                0
            } else {
                average_speed
            }
        } else { 0 },
        progress, error: record.error.clone(),
        media_title: record.media_title.clone(),
        media_thumbnail: record.media_thumbnail.clone(),
        media_activity: record.media_activity.clone(),
    }
}
fn set_status(manager: &DownloadManager, id: &str, status: &str, error: Option<String>) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            if record.status != status && matches!(status, "Connecting" | "Downloading" | "Extracting media") {
                record.started = Instant::now();
                record.speed_bytes_sec = 0;
            }
            if status == "Completed" {
                record.progress_override = Some(100.0);
                record.speed_bytes_sec = 0;
            } else if matches!(status, "Paused" | "Failed" | "Cancelled") {
                record.speed_bytes_sec = 0;
            }
            record.status = status.to_string();
            record.error = error;
            if status == "Extracting media" {
                record.media_activity = Some("Fetching video information".to_string());
            } else if status == "Downloading" {
                record.media_activity = Some("Downloading media".to_string());
            } else if matches!(status, "Completed" | "Paused" | "Failed" | "Cancelled") {
                record.media_activity = None;
            }
        }
    }
    persist_records(manager);
}
fn set_total(manager: &DownloadManager, id: &str, total: Option<u64>) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) { record.total = total; }
    }
}
fn set_file_name(manager: &DownloadManager, id: &str, name: &str) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) { record.file_name = name.to_string(); }
    }
}
fn set_media_title(manager: &DownloadManager, id: &str, title: &str) {
    let title = title.trim();
    if title.is_empty() { return; }
    let title = title.chars().take(512).collect::<String>();
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.media_title = Some(title);
        }
    }
}
fn media_activity_from_line(line: &str) -> Option<&'static str> {
    let message = line.to_ascii_lowercase();
    if message.contains("merging formats") || message.contains("merging video") {
        Some("Merging audio and video")
    } else if message.contains("post-process") || message.contains("postprocessing") || message.contains("fixing") {
        Some("Finalizing video file")
    } else if message.contains("downloading webpage")
        || message.contains("downloading api json")
        || message.contains("downloading player")
        || message.contains("extracting url")
        || message.contains("extracting video")
        || message.contains("fetching")
        || message.contains("downloading m3u8 information")
        || message.contains("downloading mpd manifest")
    {
        Some("Fetching video information")
    } else if message.contains("[download]") || message.contains("destination:") || message.contains("downloading video") {
        Some("Downloading media")
    } else {
        None
    }
}

fn set_media_activity_from_line(manager: &DownloadManager, id: &str, line: &str) {
    if let Some(activity) = media_activity_from_line(line) {
        if let Ok(mut records) = manager.records.lock() {
            if let Some(record) = records.get_mut(id) {
                record.media_activity = Some(activity.to_string());
            }
        }
    }
}

fn set_file_path(manager: &DownloadManager, id: &str, path: &Path) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.file_path = Some(path.to_string_lossy().into_owned());
            if let Some(name) = path.file_name().and_then(|value| value.to_str()) {
                record.file_name = name.to_string();
            }
        }
    }
    persist_records(manager);
}

#[cfg(target_os = "android")]
fn update_mobile_media_progress(manager: &DownloadManager, id: &str, progress: &MediaProgress) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.downloaded.store(progress.downloaded_bytes, Ordering::Relaxed);
            if let Some(total) = progress.total_bytes.filter(|value| *value > 0) {
                record.total = Some(total);
            }
            record.speed_bytes_sec = progress.speed_bytes_sec;
            record.progress_override = Some(progress.progress.clamp(0.0, 100.0));
            if let Some(activity) = media_activity_from_line(&progress.line) {
                record.media_activity = Some(activity.to_string());
            }
            if progress.downloaded_bytes > 0 && record.status != "Downloading" {
                record.started = Instant::now();
                record.status = "Downloading".into();
                record.media_activity = Some("Downloading media".into());
                record.error = None;
            }
        }
    }
}

fn update_media_progress(manager: &DownloadManager, id: &str, data: &str) {
    let fields: Vec<&str> = data.split('|').collect();
    let downloaded = fields.first().and_then(|value| value.parse::<u64>().ok());
    let total = fields.get(1).and_then(|value| value.parse::<u64>().ok())
        .or_else(|| fields.get(2).and_then(|value| value.parse::<u64>().ok()));
    let speed = fields.get(3).and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u64).unwrap_or(0);
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            if let Some(value) = downloaded {
                record.downloaded.store(value, Ordering::Relaxed);
            }
            if let Some(value) = total {
                record.total = Some(value);
            }
            record.speed_bytes_sec = speed;
            if downloaded.unwrap_or(0) > 0 && record.status != "Downloading" {
                record.started = Instant::now();
                record.status = "Downloading".into();
                record.media_activity = Some("Downloading media".into());
                record.error = None;
            }
        }
    }
}

fn parse_human_byte_count(raw: &str) -> Option<u64> {
    let value = raw.trim().trim_start_matches('~').replace(',', "");
    let number_end = value.find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))?;
    let number = value[..number_end].parse::<f64>().ok()?;
    if !number.is_finite() || number < 0.0 { return None; }
    let unit = value[number_end..].trim().to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "" | "b" => 1.0,
        "kib" => 1024.0,
        "mib" => 1024.0_f64.powi(2),
        "gib" => 1024.0_f64.powi(3),
        "tib" => 1024.0_f64.powi(4),
        "kb" => 1000.0,
        "mb" => 1000.0_f64.powi(2),
        "gb" => 1000.0_f64.powi(3),
        "tb" => 1000.0_f64.powi(4),
        _ => return None,
    };
    Some((number * multiplier).round().min(u64::MAX as f64) as u64)
}

// yt-dlp's custom progress template can be buffered by Windows when its output
// is captured without a console. Parse the normal --newline progress line too,
// so the desktop queue remains live even when the custom marker arrives late.
fn parse_yt_dlp_progress_line(line: &str) -> Option<(Option<u64>, Option<u64>, Option<u64>)> {
    let download = line.find("[download]")?;
    let progress_line = &line[download + "[download]".len()..];
    let percent_end = progress_line.find('%')?;
    let percent = progress_line[..percent_end].split_whitespace().last()?
        .parse::<f64>().ok()?;
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return None;
    }

    let after_percent = &progress_line[percent_end + 1..];
    let total = after_percent.split_once("of ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(parse_human_byte_count);
    let downloaded = total.map(|bytes| {
        (bytes as f64 * percent / 100.0).round().clamp(0.0, bytes as f64) as u64
    });
    let speed = progress_line.rsplit_once(" at ")
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|value| value.strip_suffix("/s"))
        .and_then(parse_human_byte_count);
    if downloaded.is_none() && total.is_none() && speed.is_none() {
        None
    } else {
        Some((downloaded, total, speed))
    }
}

fn update_media_progress_from_line(manager: &DownloadManager, id: &str, line: &str) {
    if let Some(index) = line.find("__DOWNBEST__") {
        update_media_progress(manager, id, &line[index + "__DOWNBEST__".len()..]);
        return;
    }
    if let Some((downloaded, total, speed)) = parse_yt_dlp_progress_line(line) {
        if let Ok(mut records) = manager.records.lock() {
            if let Some(record) = records.get_mut(id) {
                if let Some(value) = downloaded {
                    record.downloaded.store(value, Ordering::Relaxed);
                }
                if let Some(value) = total {
                    record.total = Some(value);
                }
                if let Some(value) = speed {
                    record.speed_bytes_sec = value;
                }
                if downloaded.unwrap_or(0) > 0 || speed.unwrap_or(0) > 0 {
                    if record.status != "Downloading" {
                        record.started = Instant::now();
                    }
                    record.status = "Downloading".into();
                    record.media_activity = Some("Downloading media".into());
                    record.error = None;
                }
            }
        }
    }
    set_media_activity_from_line(manager, id, line);
}

fn progress_counter(manager: &DownloadManager, id: &str) -> Result<Arc<AtomicU64>, String> {
    let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    records.get(id).map(|record| record.downloaded.clone())
        .ok_or_else(|| "Download record was not found.".to_string())
}
fn add_progress(progress: &AtomicU64, amount: u64) {
    progress.fetch_add(amount, Ordering::Relaxed);
}
fn reset_progress(manager: &DownloadManager, id: &str) {
    if let Ok(records) = manager.records.lock() {
        if let Some(record) = records.get(id) { record.downloaded.store(0, Ordering::Relaxed); }
    }
}
fn set_progress(manager: &DownloadManager, id: &str, progress: f64) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.progress_override = Some(progress.clamp(0.0, 100.0));
        }
    }
}


fn is_retryable_download_error(error: &str) -> bool {
    let message = error.to_ascii_lowercase();
    let permanent = [
        "unsupported url", "invalid url", "private video", "video unavailable",
        "not available in your country", "sign in to confirm", "login required",
        "authentication required", "http error 401", "http error 403",
        "http error 404", "returned http 401", "returned http 403",
        "returned http 404", "permission denied", "no space left on device",
        "disk write failed", "invalid content-range",
    ];
    if permanent.iter().any(|part| message.contains(part)) { return false; }
    let transient = [
        "timed out", "timeout", "connection", "network", "dns", "temporarily",
        "broken pipe", "connection reset", "connection closed", "failed to connect",
        "could not connect", "error sending request", "transport error",
        "network stream interrupted", "download stalled", "segment interrupted",
        "server stopped supporting parallel ranges", "incomplete download",
        "http 408", "http 425", "http 429", "http 500", "http 502",
        "http 503", "http 504", "http error 408", "http error 425",
        "http error 429", "http error 500", "http error 502",
        "http error 503", "http error 504", "temporarily unavailable",
        "remote end closed", "unexpected eof", "unexpected end of file",
    ];
    transient.iter().any(|part| message.contains(part))
}

fn auto_retry_delay(attempt: usize) -> std::time::Duration {
    let seconds = [3u64, 6, 12, 24, 45, 60];
    std::time::Duration::from_secs(seconds[attempt.saturating_sub(1).min(seconds.len() - 1)])
}

fn terminal_status_after_download_error(
    error: &str,
    transfer_was_cancelled: bool,
    user_requested_cancel: bool,
    user_requested_pause: bool,
) -> &'static str {
    if user_requested_cancel {
        "Cancelled"
    } else if user_requested_pause || transfer_was_cancelled || error == "Cancelled" {
        "Paused"
    } else {
        "Failed"
    }
}

fn spawn_download_task(
    app: AppHandle, manager: DownloadManager, id: String, url: String, kind: String,
    destination: PathBuf, concurrency: usize, cookies_file: Option<String>,
    format_selector: Option<String>, cancel: CancellationToken,
) {
    tauri::async_runtime::spawn(async move {
        let saver_manager = manager.clone();
        let saver_cancel = cancel.clone();
        let saver = tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = saver_cancel.cancelled() => break,
                    _ = interval.tick() => persist_records(&saver_manager),
                }
            }
        });
        #[cfg(target_os = "android")]
        let publish_app = app.clone();
        let mut retry_attempt = 0usize;
        let download_result = loop {
            let attempt_result = execute_download(
                app.clone(), manager.clone(), id.clone(), url.clone(), kind.clone(),
                destination.clone(), concurrency, cookies_file.clone(), format_selector.clone(),
                cancel.clone(),
            ).await;
            match attempt_result {
                Ok(()) => break Ok(()),
                Err(error) if cancel.is_cancelled() => break Err(error),
                Err(error) if is_retryable_download_error(&error) => {
                    retry_attempt = retry_attempt.saturating_add(1);
                    let delay = auto_retry_delay(retry_attempt);
                    let message = format!(
                        "Temporary connection problem · automatic retry #{} in {} seconds.",
                        retry_attempt, delay.as_secs()
                    );
                    set_status(&manager, &id, "Retrying", Some(message));
                    tokio::select! {
                        _ = cancel.cancelled() => break Err("Cancelled".into()),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
                Err(error) => break Err(error),
            }
        };
        #[cfg(target_os = "android")]
        let result = if download_result.is_ok() {
            publish_completed_download(&publish_app, &manager, &id).await
        } else {
            download_result
        };
        #[cfg(not(target_os = "android"))]
        let result = download_result;
        // Capture the user's stop intent BEFORE cancelling the token to stop the
        // periodic persistence task. Cancelling it unconditionally here must not
        // turn a real extraction/finalization error into a misleading Paused state.
        let stop_snapshot = manager.records.lock().ok()
            .and_then(|records| records.get(&id).map(|record| (
                record.cancel.is_cancelled(),
                record.cancel_requested.load(Ordering::Relaxed),
                record.status == "Pausing",
            )));
        cancel.cancel();
        let _ = saver.await;
        match result {
            Ok(()) => {
                set_status(&manager, &id, "Completed", None);
                cleanup_job_cookies(&manager, &id);
            },
            Err(error) => {
                let (transfer_was_cancelled, user_requested_cancel, user_requested_pause) =
                    stop_snapshot.unwrap_or((error == "Cancelled", false, false));
                match terminal_status_after_download_error(
                    &error,
                    transfer_was_cancelled,
                    user_requested_cancel,
                    user_requested_pause,
                ) {
                    "Cancelled" => {
                        cleanup_cancelled_files(&manager, &id);
                        set_status(&manager, &id, "Cancelled", None);
                        cleanup_job_cookies(&manager, &id);
                    }
                    "Paused" => set_status(&manager, &id, "Paused", None),
                    _ => set_status(&manager, &id, "Failed", Some(error)),
                }
            },
        }
    });
}


#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MediaEntry {
    id: String,
    title: String,
    url: String,
    thumbnail: Option<String>,
    duration: Option<f64>,
    playlist_index: Option<u64>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MediaAnalysis {
    title: String,
    url: String,
    extractor: String,
    uploader: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
    is_playlist: bool,
    entry_count: usize,
    entries: Vec<MediaEntry>,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MediaFormat {
    format_id: String,
    ext: String,
    resolution: String,
    width: Option<u64>,
    height: Option<u64>,
    fps: Option<f64>,
    filesize: Option<u64>,
    format_note: Option<String>,
    vcodec: String,
    acodec: String,
    is_video: bool,
    is_audio: bool,
}

fn json_text(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| value.get(*key).and_then(|item| {
        item.as_str().map(str::to_owned)
            .or_else(|| item.as_i64().map(|n| n.to_string()))
            .or_else(|| item.as_u64().map(|n| n.to_string()))
    })).filter(|text| !text.trim().is_empty())
}
fn json_number(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| value.get(*key).and_then(|item| item.as_f64()))
        .filter(|number| number.is_finite() && *number >= 0.0)
}
fn json_thumbnail(value: &serde_json::Value) -> Option<String> {
    json_text(value, &["thumbnail"])
        .or_else(|| value.get("thumbnails").and_then(|items| items.as_array())
            .and_then(|items| items.iter().rev().find_map(|item| json_text(item, &["url"]))))
}
fn entry_url(entry: &serde_json::Value, extractor: &str) -> Option<String> {
    for key in ["webpage_url", "original_url", "url"] {
        if let Some(raw) = entry.get(key).and_then(|item| item.as_str()) {
            if let Ok(parsed) = Url::parse(raw) {
                if matches!(parsed.scheme(), "http" | "https") { return Some(raw.to_owned()); }
            }
        }
    }
    let id = json_text(entry, &["id"])?;
    let source = extractor.to_ascii_lowercase();
    if source.contains("youtube") {
        Some(format!("https://www.youtube.com/watch?v={id}"))
    } else if source.contains("facebook") {
        Some(format!("https://www.facebook.com/watch/?v={id}"))
    } else if source.contains("vimeo") {
        Some(format!("https://vimeo.com/{id}"))
    } else {
        None
    }
}
fn parse_media_analysis(raw: &str, source_url: &str) -> Result<MediaAnalysis, String> {
    let json: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|error| format!("The media service returned invalid metadata: {error}"))?;
    let extractor = json_text(&json, &["extractor_key", "extractor"]).unwrap_or_else(|| "Media".into());
    let title = json_text(&json, &["title", "playlist_title"]).unwrap_or_else(|| "Untitled media".into());
    let url = json_text(&json, &["webpage_url", "original_url"]).unwrap_or_else(|| source_url.to_owned());
    let raw_entries = json.get("entries").and_then(|items| items.as_array());
    let is_playlist = raw_entries.is_some() || json_text(&json, &["_type"])
        .map(|kind| kind == "playlist" || kind == "multi_video").unwrap_or(false);
    let mut entries = Vec::new();
    let mut seen_entries = HashSet::new();
    if let Some(items) = raw_entries {
        entries.reserve(items.len());
        for (index, entry) in items.iter().enumerate() {
            if entry.is_null() { continue; }
            let Some(entry_url) = entry_url(entry, &extractor) else { continue; };
            let identity = Url::parse(&entry_url).map(|parsed| download_identity_key(&parsed)).unwrap_or_else(|_| entry_url.clone());
            if !seen_entries.insert(identity) { continue; }
            let id = json_text(entry, &["id"]).unwrap_or_else(|| format!("item-{}", index + 1));
            entries.push(MediaEntry {
                id,
                title: json_text(entry, &["title"]).unwrap_or_else(|| format!("Video {}", index + 1)),
                url: entry_url,
                thumbnail: json_thumbnail(entry),
                duration: json_number(entry, &["duration"]),
                playlist_index: entry.get("playlist_index").and_then(|item| item.as_u64())
                    .or(Some(index as u64 + 1)),
            });
        }
    }
    if entries.is_empty() && !is_playlist {
        entries.push(MediaEntry {
            id: json_text(&json, &["id"]).unwrap_or_else(|| "media-item".into()),
            title: title.clone(),
            url: url.clone(),
            thumbnail: json_thumbnail(&json),
            duration: json_number(&json, &["duration"]),
            playlist_index: Some(1),
        });
    }
    let entry_count = json.get("playlist_count").and_then(|item| item.as_u64())
        .map(|count| count as usize).unwrap_or(entries.len());
    Ok(MediaAnalysis {
        title, url, extractor,
        uploader: json_text(&json, &["uploader", "channel", "creator"]),
        thumbnail: json_thumbnail(&json),
        duration: json_number(&json, &["duration"]),
        is_playlist,
        entry_count,
        entries,
    })
}
fn parse_media_formats(raw: &str) -> Result<Vec<MediaFormat>, String> {
    let json: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|error| format!("The media service returned invalid format data: {error}"))?;
    let formats = json.get("formats").and_then(|items| items.as_array())
        .ok_or_else(|| "No downloadable formats were returned for this link. The video may be private, blocked, or require a newer extractor.".to_string())?;
    let mut result = Vec::new();
    for format in formats {
        let Some(format_id) = json_text(format, &["format_id"]) else { continue; };
        let width = format.get("width").and_then(|item| item.as_u64());
        let height = format.get("height").and_then(|item| item.as_u64());
        let ext = json_text(format, &["ext"]).unwrap_or_else(|| "unknown".into());
        let vcodec = json_text(format, &["vcodec"]).unwrap_or_else(|| "none".into());
        let acodec = json_text(format, &["acodec"]).unwrap_or_else(|| "none".into());
        let is_video = vcodec != "none";
        let is_audio = acodec != "none";
        let resolution = height.map(|value| format!("{value}p"))
            .or_else(|| json_text(format, &["resolution"]))
            .unwrap_or_else(|| if is_audio { "Audio only".into() } else { "Unknown".into() });
        let filesize = format.get("filesize").and_then(|item| item.as_u64())
            .or_else(|| format.get("filesize_approx").and_then(|item| item.as_u64()));
        result.push(MediaFormat {
            format_id, ext, resolution, width, height,
            fps: json_number(format, &["fps"]), filesize,
            format_note: json_text(format, &["format_note", "format"]),
            vcodec, acodec, is_video, is_audio,
        });
    }
    result.sort_by(|a, b| b.height.cmp(&a.height)
        .then_with(|| b.filesize.cmp(&a.filesize))
        .then_with(|| a.ext.cmp(&b.ext)));
    if result.is_empty() {
        return Err("No downloadable formats were found. Try another public link.".into());
    }
    Ok(result)
}

#[tauri::command]
async fn analyze_media(app: AppHandle, url: String) -> Result<MediaAnalysis, String> {
    if url.len() > 8192 { return Err("The media URL is too long.".into()); }
    let parsed = Url::parse(&url).map_err(|_| "Enter a valid http:// or https:// media link.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS media links are supported.".into());
    }
    let cancel = CancellationToken::new();
    let output = inspect_media_json(&app, &url, true, &cancel).await?;
    parse_media_analysis(&output, &url)
}

#[tauri::command]
async fn get_media_formats(app: AppHandle, url: String) -> Result<Vec<MediaFormat>, String> {
    if url.len() > 8192 { return Err("The media URL is too long.".into()); }
    let parsed = Url::parse(&url).map_err(|_| "Enter a valid http:// or https:// media link.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Only HTTP and HTTPS media links are supported.".into());
    }
    let cancel = CancellationToken::new();
    let output = inspect_media_json(&app, &url, false, &cancel).await?;
    parse_media_formats(&output)
}

async fn inspect_media_json(
    app: &AppHandle,
    url: &str,
    flat_playlist: bool,
    cancel: &CancellationToken,
) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        let handle = app.clone();
        let request = MediaInspectRequest { url: url.to_owned(), flat_playlist };
        let result = tokio::task::spawn_blocking(move || handle.media().inspect(request))
            .await.map_err(|error| format!("Android media inspection worker failed: {error}"))?
            .map_err(|error| format!("Android media inspection failed: {error}"))?;
        if result.exit_code != 0 {
            let detail = result.error_output.trim();
            return Err(if detail.is_empty() {
                format!("The media extractor exited with code {}.", result.exit_code)
            } else { format!("Media extractor: {}", detail.chars().rev().take(700).collect::<String>().chars().rev().collect::<String>()) });
        }
        return Ok(result.output);
    }
    #[cfg(target_os = "ios")]
    {
        let _ = (app, url, flat_playlist, cancel);
        return Err("YouTube/Facebook media analysis is not available in the iOS build. Use the desktop or Android build for media links.".into());
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let yt_dlp = find_tool(app, "yt-dlp")
            .ok_or_else(|| "The media extractor is not installed in this build. Reinstall DownBest using the latest desktop package.".to_string())?;
        let mut command = Command::new(&yt_dlp);
        configure_tool_environment(&mut command, &yt_dlp);
        configure_no_console(&mut command);
        command.arg("--dump-single-json").arg("--skip-download")
            .arg("--no-warnings").arg("--no-progress").arg("--no-colors")
            .arg("--no-call-home").arg("--retries").arg("2").arg("--socket-timeout").arg("20");
        if flat_playlist {
            command.arg("--flat-playlist");
        } else {
            command.arg("--no-playlist");
        }
        if let Some(deno) = find_tool(app, "deno") {
            command.arg("--js-runtimes").arg(format!("deno:{}", deno.display()));
        }
        command.arg("--remote-components").arg("ejs:github")
            .arg(url)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|error| format!("Could not start the media extractor: {error}"))?;
        let stdout = child.stdout.take().ok_or_else(|| "Could not read media metadata output.".to_string())?;
        let stderr = child.stderr.take().ok_or_else(|| "Could not read media extractor diagnostics.".to_string())?;
        let stdout_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut data = Vec::new();
            let _ = reader.read_to_end(&mut data).await;
            data
        });
        let stderr_task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut data = Vec::new();
            let _ = reader.take(64 * 1024).read_to_end(&mut data).await;
            data
        });
        let status = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                return Err("Media inspection was cancelled.".into());
            }
            result = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait()) => {
                match result {
                    Ok(Ok(status)) => status,
                    Ok(Err(error)) => return Err(format!("Media extractor process failed: {error}")),
                    Err(_) => {
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        stdout_task.abort();
                        stderr_task.abort();
                        return Err("The media extractor did not respond within 3 minutes.".into());
                    }
                }
            }
        };
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let output = String::from_utf8_lossy(&stdout).trim().to_owned();
        if status.success() && !output.is_empty() {
            return Ok(output);
        }
        let mut detail = String::from_utf8_lossy(&stderr).trim().to_owned();
        if detail.is_empty() { detail = output; }
        if detail.to_ascii_lowercase().contains("javascript runtime") || detail.to_ascii_lowercase().contains("ejs") {
            return Err("YouTube needs the JavaScript challenge runtime. Update DownBest to a build that includes the media-runtime package, then Analyze again.".into());
        }
        if detail.is_empty() {
            return Err(format!("Media extraction failed with process status {status}."));
        }
        let tail: String = detail.chars().rev().take(900).collect::<String>().chars().rev().collect();
        Err(format!("Media extraction failed: {tail}"))
    }
}

fn download_identity_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().trim_start_matches("www.")
        .trim_start_matches("m.").to_ascii_lowercase();
    let segments = url.path_segments()
        .map(|parts| parts.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();

    if host == "youtu.be" {
        if let Some(id) = segments.first() {
            return format!("youtube:{}", id);
        }
    }
    if host == "youtube.com" || host.ends_with(".youtube.com") || host == "youtube-nocookie.com" {
        if let Some(id) = url.query_pairs().find(|(key, _)| key == "v").map(|(_, value)| value.to_string()) {
            if !id.is_empty() { return format!("youtube:{id}"); }
        }
        if let Some(index) = segments.iter().position(|part| matches!(*part, "shorts" | "live" | "embed" | "v")) {
            if let Some(id) = segments.get(index + 1) {
                return format!("youtube:{}", id);
            }
        }
    }

    if host == "fb.watch" || host.ends_with(".fb.watch") || host == "facebook.watch" {
        if let Some(id) = segments.first() { return format!("facebook:{id}"); }
    }
    if host == "facebook.com" || host.ends_with(".facebook.com") || host == "fb.com" || host.ends_with(".fb.com") {
        let query: Vec<(String, String)> = url.query_pairs().map(|(key, value)| (key.to_string(), value.to_string())).collect();
        if let Some(id) = query.iter().find(|(key, _)| key == "v").map(|(_, value)| value.clone()) {
            if !id.is_empty() { return format!("facebook:{id}"); }
        }
        if let Some(story) = query.iter().find(|(key, _)| key == "story_fbid").map(|(_, value)| value.clone()) {
            let owner = query.iter().find(|(key, _)| key == "id").map(|(_, value)| value.clone()).unwrap_or_default();
            return format!("facebook-story:{owner}:{story}");
        }
        if let Some(index) = segments.iter().position(|part| matches!(*part, "reel" | "reels" | "video")) {
            if let Some(id) = segments.get(index + 1) { return format!("facebook:{id}"); }
        }
        if let Some(index) = segments.iter().position(|part| *part == "videos") {
            if let Some(id) = segments.get(index + 2) { return format!("facebook:{id}"); }
        }
        if let Some(index) = segments.iter().position(|part| *part == "v") {
            if let Some(id) = segments.get(index + 1) { return format!("facebook:{id}"); }
        }
    }

    let mut normalized = url.clone();
    normalized.set_fragment(None);
    let mut query: Vec<(String, String)> = normalized.query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !(key.starts_with("utm_") || matches!(key.as_str(),
                "fbclid" | "gclid" | "dclid" | "si" | "feature" | "ref" | "ref_src" |
                "__tn__" | "__cft__" | "igshid" | "mibextid" | "source"
            ))
        })
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    query.sort();
    normalized.set_query(None);
    if !query.is_empty() {
        let mut pairs = normalized.query_pairs_mut();
        for (key, value) in query { pairs.append_pair(&key, &value); }
    }
    normalized.to_string().trim_end_matches('/').to_string()
}

fn normalize_download_kind(kind: &str) -> &'static str {
    match kind {
        "file" => "file",
        "audio" => "audio",
        _ => "auto",
    }
}

fn selected_media_format(
    kind: &str,
    format_selector: Option<&str>,
    has_ffmpeg: bool,
) -> Result<String, String> {
    match format_selector {
        Some("audio") => Ok("bestaudio/best".to_string()),
        Some(selector) if selector.starts_with("format-") => {
            let format_id = selector.trim_start_matches("format-");
            if format_id.is_empty() || !format_id.chars().all(|ch|
                ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')
            ) {
                return Err("Invalid exact media format selection.".into());
            }
            Ok(format_id.to_string())
        }
        Some(selector) if selector.starts_with("max-height-") => {
            let max_height = selector.trim_start_matches("max-height-");
            if !max_height.chars().all(|ch| ch.is_ascii_digit()) || max_height.is_empty() {
                return Err("Invalid resolution selection.".into());
            }
            Ok(match kind {
                "audio" => "bestaudio/best".to_string(),
                "video-only" => format!("bestvideo[height<={max_height}]/bestvideo"),
                _ if has_ffmpeg => format!("bestvideo[height<={max_height}]+bestaudio/best[height<={max_height}]"),
                _ => format!("best[height<={max_height}][ext=mp4]/best[height<={max_height}]"),
            })
        }
        Some(selector) => Ok(selector.to_string()),
        None => Ok(match kind {
            "audio" => "bestaudio/best".to_string(),
            "video-only" => "bestvideo".to_string(),
            _ if has_ffmpeg => "bestvideo+bestaudio/best".to_string(),
            _ => "best[ext=mp4]/best".to_string(),
        }),
    }
}

#[tauri::command]
async fn start_download(
    app: AppHandle, manager: State<'_, DownloadManager>, url: String, kind: String,
    destination: Option<String>, concurrency: Option<usize>, cookies: Option<String>,
    format_selector: Option<String>, media_title: Option<String>, media_thumbnail: Option<String>,
) -> Result<DownloadView, String> {
    if url.len() > 8192 { return Err("The download URL is too long.".into()); }
    let parsed = Url::parse(&url).map_err(|_| "Enter a valid http:// or https:// URL.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") { return Err("Only HTTP and HTTPS links are supported.".into()); }
    let kind = normalize_download_kind(&kind).to_string();
    let media_title = media_title.and_then(|value| {
        let title = value.trim();
        (!title.is_empty()).then(|| title.chars().take(512).collect::<String>())
    });
    let media_thumbnail = media_thumbnail.and_then(|value| {
        if value.len() > 4096 { return None; }
        Url::parse(value.trim()).ok()
            .filter(|parsed| matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some())
            .map(|parsed| parsed.to_string())
    });
    if let Some(selector) = format_selector.as_ref() {
        if selector.is_empty() || selector.len() > 128
            || !selector.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '.' | '_' | '-' ))
        {
            return Err("The selected media quality is invalid. Please choose a listed resolution.".into());
        }
    }
    let default_base = {
        #[cfg(target_os = "android")]
        { app.path().app_data_dir().ok().map(|path| path.join("download-staging")) }
        #[cfg(target_os = "ios")]
        { app.path().app_data_dir().ok().map(|path| path.join("Downloads")) }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        { dirs::download_dir() }
    };
    let default_destination = default_base.unwrap_or_else(|| std::env::temp_dir().join("Downloads"));
    let destination_path = match destination {
        Some(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => default_destination,
    };
    std::fs::create_dir_all(&destination_path)
        .map_err(|error| format!("Cannot create download folder {}: {error}", destination_path.display()))?;
    let id = Uuid::new_v4().simple().to_string();
    let cookies_file = write_job_cookies(&app, &id, cookies)?;
    let requested = concurrency.unwrap_or(8).clamp(1, 16);
    let cancel = CancellationToken::new();
    let record = DownloadRecord {
        id: id.clone(), url: url.clone(),
        file_name: suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8])),
        kind: kind.clone(), status: "Queued".into(),
        destination: destination_path.display().to_string(), file_path: None,
        downloaded: Arc::new(AtomicU64::new(0)), total: None, started: Instant::now(),
        error: None, cancel: cancel.clone(), progress_override: None, speed_bytes_sec: 0,
        concurrency: requested, etag: None, last_modified: None,
        cookies_file: cookies_file.clone(),
        format_selector: format_selector.clone(),
        media_title,
        media_thumbnail,
        media_activity: None,
        cancel_requested: Arc::new(AtomicBool::new(false)),
    };
    let initial = view_of(&record);
    let manager = manager.inner().clone();
    {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let identity = download_identity_key(&parsed);
        let duplicate = records.values().find(|existing| {
            !matches!(existing.status.as_str(), "Cancelled")
                && Url::parse(&existing.url).map(|other| download_identity_key(&other) == identity).unwrap_or(false)
        }).map(|existing| (
            existing.media_title.clone().unwrap_or_else(|| existing.file_name.clone()),
            existing.status == "Completed",
        ));
        if let Some((name, already_completed)) = duplicate {
            drop(records);
            if let Some(path) = cookies_file.as_deref() { let _ = std::fs::remove_file(path); }
            return Err(if already_completed {
                format!("Already completed: {name}. This video is already saved and cannot be downloaded or resumed again.")
            } else {
                format!("Already in the queue: {name}. Resume or manage the existing queue item instead of adding it again.")
            });
        }
        records.insert(id.clone(), record);
    }
    persist_records(&manager);
    spawn_download_task(app, manager, id, url, kind, destination_path, requested, cookies_file, format_selector, cancel);
    Ok(initial)
}

fn resume_download_inner(
    app: AppHandle, manager: &DownloadManager, id: String, startup_recovery: bool,
) -> Result<DownloadView, String> {
    let (url, kind, destination, concurrency, cookies_file, format_selector, cancel, view) = {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let record = records.get_mut(&id).ok_or_else(|| "Download was not found.".to_string())?;
        if record.status == "Completed" { return Err("This download is already complete.".into()); }
        let eligible = if startup_recovery {
            record.status == "Queued"
        } else {
            matches!(record.status.as_str(), "Paused" | "Failed" | "Cancelled")
        };
        if !eligible {
            return Err("This download is already queued or running.".into());
        }
        let recovered_bytes = partial_bytes_for(record);
        record.downloaded.store(recovered_bytes, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        record.cancel = cancel.clone();
        record.cancel_requested.store(false, Ordering::Relaxed);
        record.status = "Queued".into();
        record.error = None;
        record.started = Instant::now();
        record.progress_override = None;
        let destination = PathBuf::from(&record.destination);
        let view = view_of(record);
        (record.url.clone(), record.kind.clone(), destination, record.concurrency, record.cookies_file.clone(), record.format_selector.clone(), cancel, view)
    };
    persist_records(manager);
    spawn_download_task(app, manager.clone(), id, url, kind, destination, concurrency, cookies_file, format_selector, cancel);
    Ok(view)
}

#[tauri::command]
fn resume_download(
    app: AppHandle, manager: State<'_, DownloadManager>, id: String,
) -> Result<DownloadView, String> {
    resume_download_inner(app, manager.inner(), id, false)
}

#[tauri::command]
fn get_downloads(manager: State<'_, DownloadManager>) -> Result<Vec<DownloadView>, String> {
    let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    let mut result: Vec<DownloadView> = records.values().map(view_of).collect();
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}
#[tauri::command]
async fn pause_download(app: AppHandle, manager: State<'_, DownloadManager>, id: String) -> Result<(), String> {
    {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let record = records.get_mut(&id).ok_or_else(|| "Download was not found.".to_string())?;
        if matches!(record.status.as_str(), "Completed" | "Paused" | "Failed" | "Cancelled") { return Ok(()); }
        record.status = "Pausing".into();
        record.cancel.cancel();
    }
    persist_records(manager.inner());
    // On Android the media progress callback also changes the visible status
    // to Downloading. Always ask the native plugin to stop an extractor job;
    // for a direct HTTP job it simply returns cancelled=false.
    #[cfg(target_os = "android")]
    {
        let handle = app.clone();
        let job_id = id.clone();
        tokio::task::spawn_blocking(move || handle.media().cancel(MediaCancelRequest { job_id }))
            .await.map_err(|e| format!("Android media cancellation worker failed: {e}"))?
            .map_err(|e| format!("Could not pause Android download: {e}"))?;
    }
    #[cfg(not(target_os = "android"))]
    let _ = app;
    Ok(())
}
#[tauri::command]
async fn cancel_download(app: AppHandle, manager: State<'_, DownloadManager>, id: String) -> Result<(), String> {
    let in_flight = {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let record = records.get_mut(&id).ok_or_else(|| "Download was not found.".to_string())?;
        if matches!(record.status.as_str(), "Completed" | "Failed" | "Cancelled") {
            return Ok(());
        }
        record.cancel_requested.store(true, Ordering::Relaxed);
        let in_flight = !matches!(record.status.as_str(), "Paused" | "Queued");
        record.cancel.cancel();
        record.status = if in_flight { "Cancelling".into() } else { "Cancelled".into() };
        in_flight
    };
    persist_records(manager.inner());

    #[cfg(target_os = "android")]
    {
        let handle = app.clone();
        let job_id = id.clone();
        let _ = tokio::task::spawn_blocking(move || handle.media().cancel(MediaCancelRequest { job_id }))
            .await.map_err(|error| format!("Android media cancellation worker failed: {error}"))?;
    }
    #[cfg(not(target_os = "android"))]
    let _ = app;

    if !in_flight {
        cleanup_cancelled_files(manager.inner(), &id);
        cleanup_job_cookies(manager.inner(), &id);
        set_status(manager.inner(), &id, "Cancelled", None);
    }
    Ok(())
}

#[tauri::command]
fn clear_finished(manager: State<'_, DownloadManager>) -> Result<(), String> {
    let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    for item in records.values().filter(|item| matches!(item.status.as_str(), "Completed" | "Failed" | "Cancelled")) {
        if let Some(path) = item.cookies_file.as_ref() { let _ = std::fs::remove_file(path); }
    }
    records.retain(|_, item| !matches!(item.status.as_str(), "Completed" | "Failed" | "Cancelled"));
    drop(records);
    persist_records(manager.inner());
    Ok(())
}
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
async fn check_engines(app: AppHandle) -> Vec<ToolStatus> {
    let mut result = Vec::new();
    result.push(ToolStatus { name: "Native HTTP".into(), available: true, version: Some("Built-in".into()) });
    for name in ["yt-dlp", "deno", "aria2c", "ffmpeg", "ffprobe"] {
        if let Some(path) = find_tool(&app, name) {
            let mut command = Command::new(&path);
            configure_tool_environment(&mut command, &path);
            configure_no_console(&mut command);
            let output = command.arg("--version").output().await.ok();
            let version = output.map(|o| String::from_utf8_lossy(&o.stdout).lines().next()
                .unwrap_or("Available").trim().to_string()).filter(|v| !v.is_empty());
            result.push(ToolStatus { name: name.into(), available: true, version });
        } else {
            result.push(ToolStatus { name: name.into(), available: false, version: None });
        }
    }
    result
}

#[cfg(target_os = "android")]
#[tauri::command]
async fn check_engines(_app: AppHandle) -> Vec<ToolStatus> {
    vec![
        ToolStatus { name: "Native HTTP".into(), available: true, version: Some("Built-in".into()) },
        ToolStatus { name: "yt-dlp".into(), available: true, version: Some("Embedded Python runtime".into()) },
        ToolStatus { name: "JavaScript runtime".into(), available: true, version: Some("Bundled QuickJS".into()) },
        ToolStatus { name: "aria2c".into(), available: false, version: None },
        ToolStatus { name: "ffmpeg".into(), available: true, version: Some("Embedded FFmpegKit".into()) },
        ToolStatus { name: "ffprobe".into(), available: true, version: Some("Embedded FFmpegKit".into()) },
    ]
}

#[cfg(target_os = "ios")]
#[tauri::command]
async fn check_engines(_app: AppHandle) -> Vec<ToolStatus> {
    vec![
        ToolStatus { name: "Native HTTP".into(), available: true, version: Some("Built-in".into()) },
        ToolStatus { name: "Native HLS".into(), available: true, version: Some("Open VOD playlists".into()) },
        ToolStatus { name: "yt-dlp".into(), available: false, version: None },
        ToolStatus { name: "aria2c".into(), available: false, version: None },
        ToolStatus { name: "ffmpeg".into(), available: false, version: None },
    ]
}

async fn execute_download(
    app: AppHandle, manager: DownloadManager, id: String, url: String, kind: String,
    destination: PathBuf, concurrency: usize, cookies_file: Option<String>,
    format_selector: Option<String>, cancel: CancellationToken,
) -> Result<(), String> {
    let _permit = tokio::select! {
        _ = cancel.cancelled() => return Err("Cancelled".into()),
        permit = manager.slots.clone().acquire_owned() => permit.map_err(|_| "Download queue is shutting down.".to_string())?,
    };
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    fs::create_dir_all(&destination).await.map_err(|e| format!("Cannot create save folder: {e}"))?;
    set_status(&manager, &id, "Connecting", None);
    let parsed = Url::parse(&url).map_err(|e| format!("Invalid URL: {e}"))?;
    let client = manager.http_client.as_ref().clone();

    // Known media domains/manifests go straight to yt-dlp. For other URLs,
    // inspect the response type so generic yt-dlp-supported web pages also work.
    let media_page = kind != "file"
        && (needs_media_extractor(&parsed)
            || (!has_known_direct_file_extension(&parsed) && looks_like_html_page(&client, &url).await));
    if media_page {
        set_status(&manager, &id, "Extracting media", None);
        return run_media_download(&app, &manager, &id, &parsed, &kind, &destination, concurrency, cookies_file.as_deref(), format_selector.as_deref(), &cancel).await;
    }
    let name = suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8]));
    let mut target = unique_path(&manager, &id, &destination, &name)?;
    set_file_name(&manager, &id, target.file_name().and_then(|s| s.to_str()).unwrap_or(&name));
    set_status(&manager, &id, "Downloading", None);
    download_http(&client, &url, &mut target, &destination, &id, concurrency, &manager, &cancel).await?;
    set_file_path(&manager, &id, &target);
    Ok(())
}

fn has_known_direct_file_extension(url: &Url) -> bool {
    let extension = url.path().rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(extension.as_str(),
        "aac" | "aiff" | "ape" | "avi" | "bin" | "bmp" | "csv" | "doc" | "docx" |
        "flac" | "flv" | "gif" | "gz" | "iso" | "jpeg" | "jpg" | "m4a" | "m4v" |
        "mkv" | "mov" | "mp3" | "mp4" | "mpeg" | "mpg" | "msi" | "ogg" | "opus" |
        "pdf" | "png" | "ppt" | "pptx" | "rar" | "rtf" | "tar" | "tgz" | "txt" |
        "wav" | "webm" | "webp" | "wma" | "wmv" | "xls" | "xlsx" | "zip" | "7z"
    )
}

async fn looks_like_html_page(client: &Client, url: &str) -> bool {
    let timeout = std::time::Duration::from_secs(5);
    let head = tokio::time::timeout(timeout, client.head(url).send()).await;
    if let Ok(Ok(response)) = head {
        if response.status().is_success() {
            return response.headers().get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| {
                    let mime = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
                    mime == "text/html" || mime == "application/xhtml+xml"
                }).unwrap_or(false);
        }
    }
    // Some download hosts reject HEAD; a one-byte range request is a fallback.
    match tokio::time::timeout(timeout, client.get(url).header(header::RANGE, "bytes=0-0").send()).await {
        Ok(Ok(response)) if response.status().is_success() || response.status() == StatusCode::PARTIAL_CONTENT =>
            response.headers().get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(|value| {
                    let mime = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
                    mime == "text/html" || mime == "application/xhtml+xml"
                }).unwrap_or(false),
        _ => false,
    }
}

fn needs_media_extractor(url: &Url) -> bool {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    // yt-dlp supports a broad and frequently updated extractor catalogue.
    // This fast-path list identifies common media sites without a format probe.
    let known = [
        "youtube.com", "youtu.be", "youtube-nocookie.com", "vimeo.com",
        "dailymotion.com", "tiktok.com", "instagram.com", "facebook.com", "fb.watch",
        "twitter.com", "x.com", "soundcloud.com", "twitch.tv", "reddit.com", "redd.it",
        "streamable.com", "loom.com", "bilibili.com", "b23.tv", "ok.ru", "rutube.ru",
        "bandcamp.com", "mixcloud.com", "rumble.com", "odysee.com", "pinterest.com",
        "pinterest.co.uk", "archive.org", "kick.com", "vk.com", "snapchat.com",
        "instagr.am", "threads.net", "t.me", "telegram.me", "telegram.org",
        "clips.twitch.tv", "fb.com", "fb.gg", "facebook.watch",
    ];
    if known.iter().any(|domain| host == *domain || host.ends_with(&format!(".{domain}"))) { return true; }
    let path = url.path().to_ascii_lowercase();
    path.ends_with(".m3u8") || path.ends_with(".mpd")
}
fn percent_decode(raw: &str, plus_as_space: bool) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = |value: u8| -> Option<u8> {
                match value {
                    b'0'..=b'9' => Some(value - b'0'),
                    b'a'..=b'f' => Some(value - b'a' + 10),
                    b'A'..=b'F' => Some(value - b'A' + 10),
                    _ => None,
                }
            };
            if let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        if plus_as_space && bytes[index] == b'+' {
            decoded.push(b' ');
        } else {
            decoded.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn sanitize_download_filename(raw: &str) -> Option<String> {
    let decoded = percent_decode(raw, false);
    let basename = decoded.rsplit(['/', '\\']).next()?;
    let safe: String = basename.chars()
        .filter(|character| !character.is_control()
            && !matches!(character, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .take(170)
        .collect();
    let safe = safe.trim().trim_matches('.').to_string();
    if safe.is_empty() || safe == "." || safe == ".." { None } else { Some(safe) }
}

fn parse_content_disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    for parameter in value.split(';').skip(1) {
        let Some((key, raw_value)) = parameter.trim().split_once('=') else { continue; };
        let key = key.trim();
        let raw_value = raw_value.trim().trim_matches('"')
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
        if key.eq_ignore_ascii_case("filename*") {
            let encoded = raw_value.split_once("''").map(|(_, value)| value)
                .unwrap_or(raw_value.as_str());
            if let Some(filename) = sanitize_download_filename(&percent_decode(encoded, false)) {
                return Some(filename);
            }
        } else if key.eq_ignore_ascii_case("filename") && plain.is_none() {
            plain = sanitize_download_filename(&raw_value);
        }
    }
    plain
}

fn extension_for_content_type(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match mime.as_str() {
        "application/pdf" => Some("pdf"),
        "application/zip" | "application/x-zip-compressed" => Some("zip"),
        "application/x-7z-compressed" => Some("7z"),
        "application/vnd.rar" | "application/x-rar-compressed" => Some("rar"),
        "application/gzip" | "application/x-gzip" => Some("gz"),
        "application/x-tar" => Some("tar"),
        "application/json" => Some("json"),
        "text/plain" => Some("txt"),
        "text/csv" => Some("csv"),
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        "video/mp4" => Some("mp4"),
        "video/webm" => Some("webm"),
        "audio/mpeg" => Some("mp3"),
        "audio/mp4" => Some("m4a"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        _ => None,
    }
}

fn response_suggested_name(response: &Response) -> Option<String> {
    let disposition = response.headers().get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_content_disposition_filename);
    let name = disposition.or_else(|| suggested_name(response.url()))?;
    let content_type = response.headers().get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok()).unwrap_or("");
    if Path::new(&name).extension().is_none() {
        if let Some(extension) = extension_for_content_type(content_type) {
            return Some(format!("{name}.{extension}"));
        }
    }
    Some(name)
}

fn suggested_name(url: &Url) -> Option<String> {
    for (key, value) in url.query_pairs() {
        if matches!(key.to_ascii_lowercase().as_str(), "filename" | "file_name" | "file" | "name") {
            if let Some(name) = sanitize_download_filename(&value) {
                return Some(name);
            }
        }
    }
    let raw = url.path_segments()?.filter(|segment| !segment.is_empty()).last()?;
    sanitize_download_filename(raw)
}

fn unique_path(manager: &DownloadManager, id: &str, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let original = Path::new(name);
    let stem = original.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let extension = original.extension().and_then(|s| s.to_str());
    let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    if let Some(saved) = records.get(id).and_then(|record| record.file_path.clone()) {
        let saved_path = PathBuf::from(saved);
        if !saved_path.exists() { return Ok(saved_path); }
    }
    for index in 0..10_000 {
        let candidate = if index == 0 { name.to_string() }
            else if let Some(ext) = extension { format!("{stem} ({index}).{ext}") }
            else { format!("{stem} ({index})") };
        let path = dir.join(candidate);
        let path_text = path.to_string_lossy().into_owned();
        let reserved = records.iter().any(|(record_id, record)| record_id != id &&
            record.file_path.as_ref().map(|existing| {
                if cfg!(windows) { existing.eq_ignore_ascii_case(&path_text) } else { existing == &path_text }
            }).unwrap_or(false));
        if !path.exists() && !reserved {
            let record = records.get_mut(id).ok_or_else(|| "Download record was not found.".to_string())?;
            record.file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or(name).to_string();
            record.file_path = Some(path_text);
            drop(records);
            persist_records(manager);
            return Ok(path);
        }
    }
    let path = dir.join(format!("{}-{}", Uuid::new_v4(), name));
    let path_text = path.to_string_lossy().into_owned();
    let record = records.get_mut(id).ok_or_else(|| "Download record was not found.".to_string())?;
    record.file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or(name).to_string();
    record.file_path = Some(path_text);
    drop(records);
    persist_records(manager);
    Ok(path)
}

fn reserve_response_path(
    manager: &DownloadManager, id: &str, dir: &Path, name: &str,
) -> Result<PathBuf, String> {
    let original = Path::new(name);
    let stem = original.file_stem().and_then(|value| value.to_str()).unwrap_or("download");
    let extension = original.extension().and_then(|value| value.to_str());
    let mut records = manager.records.lock()
        .map_err(|_| "Download queue is unavailable.".to_string())?;
    for index in 0..10_000 {
        let candidate = if index == 0 { name.to_string() }
            else if let Some(extension) = extension {
                format!("{stem} ({index}).{extension}")
            } else {
                format!("{stem} ({index})")
            };
        let path = dir.join(candidate);
        let path_text = path.to_string_lossy().into_owned();
        let reserved = records.iter().any(|(record_id, record)| record_id != id
            && record.file_path.as_ref().map(|existing| {
                if cfg!(windows) { existing.eq_ignore_ascii_case(&path_text) } else { existing == &path_text }
            }).unwrap_or(false));
        if !path.exists() && !reserved {
            let record = records.get_mut(id)
                .ok_or_else(|| "Download record was not found.".to_string())?;
            record.file_name = path.file_name().and_then(|value| value.to_str())
                .unwrap_or(name).to_string();
            record.file_path = Some(path_text);
            drop(records);
            persist_records(manager);
            return Ok(path);
        }
    }
    Err("Could not reserve a unique filename for this download.".into())
}

fn effective_segment_count(total_bytes: u64, requested: usize) -> usize {
    const MIN_PARALLEL_FILE: u64 = 16 * 1024 * 1024;
    const TARGET_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
    if total_bytes < MIN_PARALLEL_FILE {
        return 1;
    }
    let requested = requested.clamp(1, 16);
    requested.min(((total_bytes / TARGET_SEGMENT_BYTES).max(1)) as usize).max(1)
}

fn parse_content_range_span(
    value: Option<&header::HeaderValue>,
) -> Option<(u64, u64, Option<u64>)> {
    let raw = value?.to_str().ok()?;
    let raw = raw.strip_prefix("bytes ")?;
    let (range, total) = raw.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    if end < start { return None; }
    let total = if total == "*" { None } else { Some(total.parse::<u64>().ok()?) };
    if total.is_some_and(|value| value <= end) { return None; }
    Some((start, end, total))
}
fn parse_content_range(value: Option<&header::HeaderValue>) -> Option<u64> {
    parse_content_range_span(value).and_then(|(_, _, total)| total)
}
fn parse_content_range_start(value: Option<&header::HeaderValue>) -> Option<u64> {
    parse_content_range_span(value).map(|(start, _, _)| start)
}
fn set_validators(manager: &DownloadManager, id: &str, etag: Option<String>, last_modified: Option<String>) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.etag = etag;
            record.last_modified = last_modified;
        }
    }
    persist_records(manager);
}
fn saved_validators(manager: &DownloadManager, id: &str) -> (Option<String>, Option<String>) {
    manager.records.lock().ok().and_then(|records| records.get(id).map(|r| (r.etag.clone(), r.last_modified.clone())))
        .unwrap_or((None, None))
}
async fn response_from_offset(
    client: &Client, url: &str, offset: u64, etag: Option<&str>, last_modified: Option<&str>,
) -> Result<(Response, bool), String> {
    let mut request = client.get(url).header(header::RANGE, format!("bytes={offset}-"));
    if let Some(value) = etag.or(last_modified) {
        if let Ok(header_value) = header::HeaderValue::from_str(value) {
            request = request.header(header::IF_RANGE, header_value);
        }
    }
    let response = request.send().await.map_err(|e| format!("Resume request failed: {e}"))?;
    if response.status() == StatusCode::PARTIAL_CONTENT {
        let (start, end, total) = parse_content_range_span(response.headers().get(header::CONTENT_RANGE))
            .ok_or_else(|| "The server returned an invalid Content-Range while resuming.".to_string())?;
        if start != offset || total.is_some_and(|value| end + 1 != value) {
            return Err("The server returned a mismatched Content-Range while resuming.".into());
        }
        if response.content_length().is_some_and(|length| length != end - start + 1) {
            return Err("The server returned an inconsistent Content-Length while resuming.".into());
        }
        return Ok((response, true));
    }
    if response.status().is_success() { return Ok((response, false)); }
    Err(format!("Server returned HTTP {} while resuming.", response.status()))
}

async fn download_http(
    client: &Client, url: &str, target: &mut PathBuf, destination: &Path, id: &str,
    concurrency: usize, manager: &DownloadManager, cancel: &CancellationToken,
) -> Result<(), String> {
    let original_temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let previous_len = fs::metadata(&original_temp).await.map(|meta| meta.len()).unwrap_or(0);
    let parts_dir = destination.join(format!(".downbest-{id}"));
    let (previous_etag, previous_modified) = saved_validators(manager, id);
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client.get(url).header(header::RANGE, "bytes=0-0").send(),
    ).await.map_err(|_| "The server did not respond to the download probe within 30 seconds.".to_string())?
        .map_err(|e| format!("Connection failed: {e}"))?;

    let probe_total = if probe.status() == StatusCode::PARTIAL_CONTENT {
        let (start, end, total) = parse_content_range_span(probe.headers().get(header::CONTENT_RANGE))
            .ok_or_else(|| "The server returned an invalid Content-Range header.".to_string())?;
        if start != 0 || end != 0 {
            return Err("The server returned an unexpected range while checking the download.".into());
        }
        total.ok_or_else(|| "The server supports ranges but did not report the total file size.".to_string())?
    } else if probe.status().is_success() {
        probe.content_length().unwrap_or(0)
    } else {
        return Err(format!("Server returned HTTP {} while checking range support.", probe.status()));
    };
    let probe_total = if probe_total == 0 { None } else { Some(probe_total) };

    // Resolve redirects, Content-Disposition filenames and MIME extensions before creating
    // the partial file, so opaque links such as /download?id=... are saved correctly.
    if previous_len == 0 {
        if let Some(remote_name) = response_suggested_name(&probe) {
            let current_name = target.file_name().and_then(|value| value.to_str()).unwrap_or("");
            if remote_name != current_name {
                *target = reserve_response_path(manager, id, destination, &remote_name)?;
            }
        }
    }
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));

    let current_etag = probe.headers().get(header::ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
    let current_modified = probe.headers().get(header::LAST_MODIFIED).and_then(|v| v.to_str().ok()).map(str::to_string);
    let changed = previous_etag.as_ref().map(|old| current_etag.as_ref() != Some(old)).unwrap_or(false)
        || previous_modified.as_ref().map(|old| current_modified.as_ref() != Some(old)).unwrap_or(false);
    if changed {
        let _ = fs::remove_file(&temp).await;
        let _ = fs::remove_dir_all(&parts_dir).await;
        reset_progress(manager, id);
    }
    set_validators(manager, id, current_etag.clone(), current_modified.clone());
    let mut partial_len = if changed { 0 } else { previous_len };
    set_total(manager, id, probe_total);

    if probe.status() == StatusCode::PARTIAL_CONTENT {
        let total = probe_total.ok_or_else(|| "The server did not report the total file size.".to_string())?;
        if partial_len == total && total > 0 {
            drop(probe);
            fs::rename(&temp, target.as_path()).await
                .map_err(|e| format!("Could not finalize recovered download: {e}"))?;
            return Ok(());
        }
        let has_segment_parts = fs::metadata(&parts_dir).await.map(|m| m.is_dir()).unwrap_or(false);
        let count = if partial_len > 0 { 1 } else { effective_segment_count(total, concurrency) };
        if count > 1 || has_segment_parts {
            drop(probe);
            let progress = progress_counter(manager, id)?;
            match download_segmented(client, url, target.as_path(), destination, id, total, count, progress, cancel).await {
                Ok(()) => Ok(()),
                Err(error) if error == "Cancelled" => Err(error),
                Err(error) if error.contains("stopped supporting parallel ranges") => {
                    let _ = fs::remove_dir_all(&parts_dir).await;
                    reset_progress(manager, id);
                    let response = client.get(url).send().await
                        .map_err(|e| format!("Parallel download failed and fallback connection failed: {e}"))?;
                    if !response.status().is_success() {
                        return Err(format!("Parallel download fallback returned HTTP {}", response.status()));
                    }
                    stream_to_target(response, target.as_path(), id, manager, progress_counter(manager, id)?,
                        cancel, false, 0, Some(total)).await
                }
                Err(error) => Err(error),
            }
        } else {
            drop(probe);
            let (response, append) = if partial_len > 0 {
                let (response, append) = response_from_offset(
                    client, url, partial_len, current_etag.as_deref(), current_modified.as_deref()
                ).await?;
                if !append { partial_len = 0; }
                (response, append)
            } else {
                let response = client.get(url).send().await
                    .map_err(|e| format!("Download request failed: {e}"))?;
                if !response.status().is_success() {
                    return Err(format!("Server returned HTTP {}", response.status()));
                }
                (response, false)
            };
            stream_to_target(response, target.as_path(), id, manager, progress_counter(manager, id)?,
                cancel, append, partial_len, Some(total)).await
        }
    } else if probe.status().is_success() {
        let total = probe_total;
        let (response, append) = if partial_len > 0 {
            let (response, append) = response_from_offset(
                client, url, partial_len, current_etag.as_deref(), current_modified.as_deref()
            ).await?;
            if !append { partial_len = 0; }
            (response, append)
        } else {
            (probe, false)
        };
        stream_to_target(response, target.as_path(), id, manager, progress_counter(manager, id)?,
            cancel, append, partial_len, total).await
    } else {
        Err("Unexpected HTTP response while starting the download.".into())
    }
}

async fn stream_to_target(
    response: Response, target: &Path, id: &str, _manager: &DownloadManager,
    progress: Arc<AtomicU64>, cancel: &CancellationToken, append: bool, initial_offset: u64,
    expected_total: Option<u64>,
) -> Result<(), String> {
    if !response.status().is_success() {
        return Err(format!("Server returned HTTP {} while downloading.", response.status()));
    }
    let response_range = if response.status() == StatusCode::PARTIAL_CONTENT {
        Some(parse_content_range_span(response.headers().get(header::CONTENT_RANGE))
            .ok_or_else(|| "The server returned an invalid Content-Range while downloading.".to_string())?)
    } else {
        None
    };
    if let Some((start, end, total)) = response_range {
        if start != initial_offset || total.is_some_and(|value| end + 1 != value) {
            return Err("The download response did not match the requested byte range.".into());
        }
        if total.is_some() && expected_total.is_some() && total != expected_total {
            return Err("The file size changed while the download was in progress. Resume the download to restart safely.".into());
        }
        let range_length = end - start + 1;
        if response.content_length().is_some_and(|length| length != range_length) {
            return Err("The server returned an inconsistent Content-Length for the requested range.".into());
        }
    }
    let expected_body = response_range.map(|(start, end, _)| end - start + 1)
        .or_else(|| response.content_length());
    let final_expected_total = response_range.and_then(|(_, _, total)| total)
        .or(expected_total)
        .or_else(|| response.content_length().map(|length| initial_offset.saturating_add(length)));
    if final_expected_total.is_some_and(|total| initial_offset > total) {
        return Err("The saved partial file is larger than the current source file.".into());
    }
    if response.status() != StatusCode::PARTIAL_CONTENT
        && expected_total.is_some()
        && response.content_length().is_some_and(|length| Some(length) != expected_total)
        && initial_offset == 0
    {
        return Err("The server's file size changed between the probe and download request.".into());
    }

    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append { options.append(true); } else { options.truncate(true); }
    let file = options.open(&temp).await.map_err(|e| format!("Cannot open partial output file: {e}"))?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    progress.store(if append { initial_offset } else { 0 }, Ordering::Relaxed);
    let mut stream = response.bytes_stream();
    let mut since_sync = 0u64;
    let mut received = 0u64;

    loop {
        let item = tokio::select! {
            _ = cancel.cancelled() => {
                let _ = file.flush().await;
                let _ = file.get_ref().sync_data().await;
                return Err("Cancelled".into());
            },
            result = tokio::time::timeout(std::time::Duration::from_secs(90), stream.next()) => {
                match result {
                    Ok(item) => item,
                    Err(_) => {
                        let _ = file.flush().await;
                        let _ = file.get_ref().sync_data().await;
                        return Err("Download stalled: no data was received for 90 seconds. The partial file is saved; resume to continue.".into());
                    }
                }
            }
        };
        let Some(item) = item else { break; };
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = file.flush().await;
                let _ = file.get_ref().sync_data().await;
                return Err(format!("Network stream interrupted: {error}"));
            }
        };
        let next_received = received.saturating_add(chunk.len() as u64);
        if expected_body.is_some_and(|expected| next_received > expected)
            || final_expected_total.is_some_and(|total| initial_offset.saturating_add(next_received) > total)
        {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err("The server sent more data than the advertised file size. The partial file was retained.".into());
        }
        if let Err(error) = file.write_all(&chunk).await {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err(format!("Disk write failed: {error}"));
        }
        received = next_received;
        add_progress(&progress, chunk.len() as u64);
        since_sync += chunk.len() as u64;
        if since_sync >= 32 * 1024 * 1024 {
            file.flush().await.map_err(|e| format!("Could not flush partial file: {e}"))?;
            file.get_ref().sync_data().await.map_err(|e| format!("Could not sync partial file: {e}"))?;
            since_sync = 0;
        }
    }

    if expected_body.is_some_and(|expected| received != expected) {
        let _ = file.flush().await;
        let _ = file.get_ref().sync_data().await;
        return Err(format!(
            "Incomplete download: expected {} response bytes but received {}. The partial file was saved; resume to continue.",
            expected_body.unwrap(), received
        ));
    }
    let final_length = initial_offset.saturating_add(received);
    if final_expected_total.is_some_and(|expected| final_length != expected) {
        let _ = file.flush().await;
        let _ = file.get_ref().sync_data().await;
        return Err(format!(
            "Incomplete download: expected a {} byte file but received {} bytes. The partial file was saved; resume to continue.",
            final_expected_total.unwrap(), final_length
        ));
    }

    file.flush().await.map_err(|e| format!("Could not flush file: {e}"))?;
    file.get_ref().sync_all().await.map_err(|e| format!("Could not sync file: {e}"))?;
    drop(file);
    fs::rename(&temp, target).await.map_err(|e| format!("Could not finalize download: {e}"))?;
    Ok(())
}

async fn download_segmented(
    client: &Client, url: &str, target: &Path, destination: &Path, id: &str,
    total: u64, count: usize, progress: Arc<AtomicU64>,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let parts_dir = destination.join(format!(".downbest-{id}"));
    fs::create_dir_all(&parts_dir).await.map_err(|e| format!("Cannot create temporary folder: {e}"))?;
    let chunk_size = (total + count as u64 - 1) / count as u64;
    let mut existing_total = 0u64;
    let mut jobs = Vec::new();
    for index in 0..count {
        let start = index as u64 * chunk_size;
        if start >= total { break; }
        let end = (start + chunk_size - 1).min(total - 1);
        let part_path = parts_dir.join(format!("part-{index:03}"));
        existing_total = existing_total.saturating_add(fs::metadata(&part_path).await.map(|m| m.len()).unwrap_or(0).min(end - start + 1));
        let client = client.clone();
        let url = url.to_string();
        let id = id.to_string();
        let cancel = cancel.clone();
        let progress = progress.clone();
        jobs.push(async move {
            download_piece(client, url, start, end, part_path.clone(), &id, progress, cancel).await?;
            Ok::<PathBuf, String>(part_path)
        });
    }
    progress.store(existing_total, Ordering::Relaxed);
    let parts = try_join_all(jobs).await?;
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let output = File::create(&temp).await.map_err(|e| format!("Cannot create output file: {e}"))?;
    let mut output = BufWriter::with_capacity(1024 * 1024, output);
    for part_path in parts {
        let mut part = File::open(&part_path).await.map_err(|e| format!("A download segment is missing: {e}"))?;
        tokio::io::copy(&mut part, &mut output).await.map_err(|e| format!("Could not assemble download: {e}"))?;
    }
    output.flush().await.map_err(|e| format!("Could not flush output: {e}"))?;
    output.get_ref().sync_all().await.map_err(|e| format!("Could not sync output: {e}"))?;
    drop(output);
    fs::rename(&temp, target).await.map_err(|e| format!("Could not finalize download: {e}"))?;
    fs::remove_dir_all(&parts_dir).await.ok();
    Ok(())
}

async fn download_piece(
    client: Client, url: String, start: u64, end: u64, path: PathBuf, _id: &str,
    progress: Arc<AtomicU64>, cancel: CancellationToken,
) -> Result<(), String> {
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    let expected = end - start + 1;
    let mut existing = fs::metadata(&path).await.map(|meta| meta.len()).unwrap_or(0);
    if existing > expected {
        fs::remove_file(&path).await.map_err(|e| format!("Cannot reset oversized segment: {e}"))?;
        existing = 0;
    }
    if existing == expected { return Ok(()); }
    let offset = start + existing;
    let response = client.get(&url).header(header::RANGE, format!("bytes={offset}-{end}"))
        .send().await.map_err(|e| format!("Segment connection failed: {e}"))?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(format!("Server stopped supporting parallel ranges (HTTP {}).", response.status()));
    }
    let (range_start, range_end, range_total) = parse_content_range_span(response.headers().get(header::CONTENT_RANGE))
        .ok_or_else(|| "Server stopped supporting parallel ranges (invalid Content-Range).".to_string())?;
    if range_start != offset || range_end != end || range_total.is_some_and(|total| total <= end) {
        return Err("Server stopped supporting parallel ranges (mismatched Content-Range).".into());
    }
    if response.content_length().is_some_and(|length| length != expected - existing) {
        return Err("Server stopped supporting parallel ranges (mismatched Content-Length).".into());
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)
        .await.map_err(|e| format!("Cannot open segment file: {e}"))?;
    let mut file = BufWriter::with_capacity(1024 * 1024, file);
    let mut stream = response.bytes_stream();
    let mut received = existing;
    let mut since_sync = 0u64;
    while let Some(item) = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err("Cancelled".into());
        },
        item = stream.next() => item,
    } {
        let chunk = match item {
            Ok(chunk) => chunk,
            Err(error) => {
                let _ = file.flush().await;
                let _ = file.get_ref().sync_data().await;
                return Err(format!("Segment interrupted: {error}"));
            }
        };
        received = received.saturating_add(chunk.len() as u64);
        if received > expected {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err("Server sent more bytes than requested for a segment.".into());
        }
        if let Err(error) = file.write_all(&chunk).await {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err(format!("Segment disk write failed: {error}"));
        }
        add_progress(&progress, chunk.len() as u64);
        since_sync += chunk.len() as u64;
        if since_sync >= 32 * 1024 * 1024 {
            file.flush().await.map_err(|e| format!("Cannot flush segment: {e}"))?;
            file.get_ref().sync_data().await.map_err(|e| format!("Cannot sync segment: {e}"))?;
            since_sync = 0;
        }
    }
    file.flush().await.map_err(|e| format!("Cannot flush segment: {e}"))?;
    file.get_ref().sync_all().await.map_err(|e| format!("Cannot sync segment: {e}"))?;
    if received != expected { return Err(format!("Segment size mismatch: expected {expected} bytes, received {received}.")); }
    Ok(())
}

#[cfg(target_os = "android")]
async fn run_media_download(
    app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, kind: &str,
    destination: &Path, concurrency: usize, cookies_file: Option<&str>,
    format_selector: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    let progress_manager = manager.clone();
    let progress_id = id.to_string();
    let on_progress = tauri::ipc::Channel::new(move |body| {
        if let Ok(progress) = body.deserialize::<MediaProgress>() {
            update_mobile_media_progress(&progress_manager, &progress_id, &progress);
        }
        Ok(())
    });
    let request = MediaDownloadRequest {
        url: url.as_str().to_string(),
        destination: destination.display().to_string(),
        job_id: id.to_string(),
        kind: kind.to_string(),
        concurrency: concurrency.min(16),
        cookies_file: cookies_file.map(str::to_string),
        format_selector: format_selector.map(str::to_string),
        on_progress,
    };
    let app_handle = app.clone();
    let worker_result = tokio::task::spawn_blocking(move || app_handle.media().download(request))
        .await.map_err(|e| format!("Mobile media worker failed: {e}"))?;
    let result = match worker_result {
        Ok(result) => result,
        Err(_error) if cancel.is_cancelled() => return Err("Cancelled".into()),
        Err(error) => return Err(format!("Mobile media engine: {error}")),
    };
    set_file_name(manager, id, &result.file_name);
    set_file_path(manager, id, Path::new(&result.file_path));
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    if !Path::new(&result.file_path).is_file() {
        return Err("The Android extractor reported success but the output file is missing.".into());
    }
    Ok(())
}


#[cfg(target_os = "android")]
async fn publish_completed_download(app: &AppHandle, manager: &DownloadManager, id: &str) -> Result<(), String> {
    let source_path = {
        let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        records.get(id).and_then(|record| record.file_path.clone())
            .ok_or_else(|| "The completed download has no output file to publish.".to_string())?
    };
    if !Path::new(&source_path).is_file() {
        return Err("The finished file is missing before it could be copied to Downloads.".into());
    }
    let handle = app.clone();
    let published = tokio::task::spawn_blocking(move || {
        handle.media().publish_download(MediaPublishRequest { file_path: source_path })
    }).await.map_err(|error| format!("Android Downloads worker failed: {error}"))?
        .map_err(|error| format!("Could not publish the finished file to Downloads: {error}"))?;
    if !published.content_uri.starts_with("content://") {
        return Err("Android returned an invalid public file location.".into());
    }
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            record.file_name = published.file_name;
            record.file_path = Some(published.content_uri);
            record.destination = "Downloads".into();
        }
    }
    persist_records(manager);
    Ok(())
}

#[tauri::command]
async fn open_download(
    app: AppHandle,
    manager: State<'_, DownloadManager>,
    id: String,
) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        let content_uri = {
            let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
            records.get(&id).and_then(|record| record.file_path.clone())
                .ok_or_else(|| "The saved file location is unavailable.".to_string())?
        };
        if !content_uri.starts_with("content://") {
            return Err("This item is not saved in the public Downloads folder.".into());
        }
        let handle = app.clone();
        tokio::task::spawn_blocking(move || {
            handle.media().open_download(MediaOpenRequest { content_uri })
        }).await.map_err(|error| format!("Android file opener failed: {error}"))?
            .map_err(|error| format!("Could not open the downloaded file: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, manager, id);
        Err("Opening a public Downloads file is only supported on Android.".into())
    }
}

#[tauri::command]
fn default_save_location() -> &'static str {
    #[cfg(target_os = "android")]
    { "Finished files are saved directly in your phone's Downloads folder." }
    #[cfg(target_os = "ios")]
    { "Files stay inside the app's private storage on iOS." }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    { "Files are saved in your system Downloads folder by default." }
}

#[cfg(target_os = "ios")]
async fn run_media_download(
    _app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, kind: &str,
    destination: &Path, _concurrency: usize, _cookies_file: Option<&str>,
    _format_selector: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    if kind == "audio" {
        return Err("Audio-only extraction from remote media links is not enabled in this iOS build. Direct audio file URLs still download normally.".into());
    }
    if url.path().to_ascii_lowercase().ends_with(".m3u8") {
        return download_open_hls(url, destination, id, manager, cancel).await;
    }
    Err("This iOS build supports direct HTTP/HTTPS files and open HLS playlists. Generic third-party site extraction is not enabled in the App Store build; use a desktop build for supported yt-dlp extractors where you have download permission.".into())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn find_completed_media_output(destination: &Path, id: &str) -> Option<PathBuf> {
    let prefix = id.chars().take(8).collect::<String>();
    let marker = format!(" - {prefix}.");
    std::fs::read_dir(destination).ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else { return false; };
            let lower = name.to_ascii_lowercase();
            name.contains(&marker)
                && !lower.ends_with(".part")
                && !lower.contains(".part-frag")
                && !lower.ends_with(".ytdl")
                && !lower.ends_with(".tmp")
                && !lower.ends_with(".temp")
        })
        .max_by_key(|path| std::fs::metadata(path).and_then(|meta| meta.modified()).ok())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn run_media_download(
    app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, kind: &str,
    destination: &Path, concurrency: usize, cookies_file: Option<&str>,
    format_selector: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    let yt_dlp = find_tool(app, "yt-dlp").ok_or_else(||
        "yt-dlp was not found. Install yt-dlp or place it in the app's tools folder to download from media sites.".to_string())?;
    let has_ffmpeg = find_tool(app, "ffmpeg").is_some();
    let aria2 = find_tool(app, "aria2c");
    let mut command = Command::new(&yt_dlp);
    configure_tool_environment(&mut command, &yt_dlp);
    configure_no_console(&mut command);
    command.arg("--no-playlist").arg("--newline").arg("--progress").arg("--continue")
        .arg("--socket-timeout").arg("20")
        .arg("--retries").arg("4").arg("--fragment-retries").arg("4")
        .arg("--extractor-retries").arg("2").arg("--file-access-retries").arg("2")
        .arg("-N").arg(concurrency.to_string())
        .arg("--postprocessor-args").arg("ffmpeg:-threads 2")
        .arg("--progress-template").arg("download:__DOWNBEST__%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s")
        .arg("--progress-delta").arg("0.5")
        .arg("--print").arg("before_dl:__DOWNBEST_TITLE__%(title)s")
        .arg("--print").arg("after_move:__DOWNBEST_FILE__%(filepath)s")
        .arg("-P").arg(destination).arg("-o").arg(format!("%(title).180B [%(id)s] - {}.%(ext)s", &id[..8]));
    let selected_format = selected_media_format(kind, format_selector, has_ffmpeg)?;
    command.arg("-f").arg(selected_format);
    if kind == "audio" && has_ffmpeg {
        command.arg("--extract-audio").arg("--audio-format").arg("mp3")
            .arg("--audio-quality").arg("0");
    } else if has_ffmpeg && kind != "video-only" {
        command.arg("--merge-output-format").arg("mp4");
    }
    if let Some(deno) = find_tool(app, "deno") {
        command.arg("--js-runtimes").arg(format!("deno:{}", deno.display()));
    }
    command.arg("--remote-components").arg("ejs:github");
    if let Some(cookie_path) = cookies_file {
        command.arg("--cookies").arg(cookie_path);
    }
    if let Some(aria_path) = aria2 {
        // aria2c handles HTTP/FTP media transfers; yt-dlp's native downloader
        // handles DASH/HLS fragments so fragment scheduling remains protocol-aware.
        command.arg("--downloader").arg("aria2c")
            .arg("--downloader").arg("dash,m3u8:native")
            .arg("--downloader-args").arg(format!("aria2c:-c -x {} -s {} -k 1M", concurrency.clamp(2, 16), concurrency.clamp(2, 16)));
        if let Some(parent) = aria_path.parent() {
            let mut paths = vec![parent.to_path_buf()];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            if let Ok(joined) = std::env::join_paths(paths) { command.env("PATH", joined); }
        }
    }
    command.arg(url.as_str()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("Could not start yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or_else(|| "Could not read yt-dlp result output.".to_string())?;
    let stderr = child.stderr.take().ok_or_else(|| "Could not read yt-dlp progress output.".to_string())?;

    // yt-dlp writes progress to stderr by default; final paths are printed to stdout.
    // Drain both pipes concurrently so neither stream can block the downloader.
    let stdout_manager = manager.clone();
    let stdout_id = id.to_string();
    let stdout_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(index) = line.find("__DOWNBEST_TITLE__") {
                let title = line[index + "__DOWNBEST_TITLE__".len()..].trim();
                if !title.is_empty() {
                    set_media_title(&stdout_manager, &stdout_id, title);
                }
            }
            if let Some(index) = line.find("__DOWNBEST_FILE__") {
                let value = line[index + "__DOWNBEST_FILE__".len()..].trim();
                if !value.is_empty() {
                    set_file_path(&stdout_manager, &stdout_id, Path::new(value));
                }
            }
            update_media_progress_from_line(&stdout_manager, &stdout_id, &line);
        }
    });

    let progress_manager = manager.clone();
    let progress_id = id.to_string();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut captured = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            update_media_progress_from_line(&progress_manager, &progress_id, &line);
            captured.push_str(&line);
            captured.push('\n');
            if captured.len() > 16_000 {
                let excess = captured.len() - 12_000;
                let cut = captured.char_indices().find(|(index, _)| *index >= excess)
                    .map(|(index, _)| index).unwrap_or(captured.len());
                captured.drain(..cut);
            }
        }
        captured
    });

    let status = tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err("Cancelled".into());
        }
        result = child.wait() => result.map_err(|e| format!("yt-dlp process failed: {e}"))?,
    };
    let _ = stdout_task.await;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if status.success() {
        let recorded = manager.records.lock().ok()
            .and_then(|records| records.get(id).and_then(|record| record.file_path.clone()))
            .filter(|path| Path::new(path).is_file());
        let output = recorded.map(PathBuf::from)
            .or_else(|| find_completed_media_output(destination, id));
        match output {
            Some(path) if path.is_file() => {
                set_file_path(manager, id, &path);
                Ok(())
            }
            Some(path) => Err(format!("yt-dlp reported a result, but the output file was not found: {}", path.display())),
            None => Err("yt-dlp finished successfully but no final output file could be verified in the selected save folder.".into()),
        }
    } else {
        let tail: String = stderr_text.chars().rev().take(900).collect::<String>().chars().rev().collect();
        Err(if tail.trim().is_empty() { format!("yt-dlp exited with status {status}.") } else { format!("yt-dlp: {tail}") })
    }
}


#[cfg(target_os = "ios")]
async fn download_open_hls(
    source: &Url,
    destination: &Path,
    id: &str,
    manager: &DownloadManager,
    cancel: &CancellationToken,
) -> Result<(), String> {
    use m3u8_rs::{KeyMethod, Playlist};

    async fn playlist(
        client: &Client,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<Playlist, String> {
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("Cancelled".into()),
            result = client.get(url.clone()).send() => result.map_err(|e| format!("HLS playlist request failed: {e}"))?,
        };
        if !response.status().is_success() {
            return Err(format!("HLS server returned HTTP {}", response.status()));
        }
        let body = tokio::select! {
            _ = cancel.cancelled() => return Err("Cancelled".into()),
            result = response.bytes() => result.map_err(|e| format!("Could not read HLS playlist: {e}"))?,
        };
        m3u8_rs::parse_playlist_res(&body)
            .map_err(|e| format!("Invalid HLS playlist: {e}"))
    }

    let client = Client::builder()
        .user_agent("DownBest/1.0")
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Could not initialize HLS client: {e}"))?;

    let root = playlist(&client, source, cancel).await?;
    let (media_url, media) = match root {
        Playlist::MediaPlaylist(media) => (source.clone(), media),
        Playlist::MasterPlaylist(master) => {
            let variant = master.variants.iter()
                .filter(|variant| !variant.is_i_frame && variant.audio.is_none())
                .max_by_key(|variant| variant.bandwidth)
                .ok_or_else(|| "This HLS master playlist uses a separate audio rendition or has no compatible video variant. Use a desktop build for muxed HLS/DASH extraction.".to_string())?;
            let child_url = source.join(&variant.uri)
                .map_err(|e| format!("Invalid HLS variant URL: {e}"))?;
            match playlist(&client, &child_url, cancel).await? {
                Playlist::MediaPlaylist(media) => (child_url, media),
                Playlist::MasterPlaylist(_) => return Err("Nested HLS master playlists are not supported on iOS.".into()),
            }
        }
    };

    if !media.end_list {
        return Err("Live HLS playlists are not supported by the mobile HLS saver. Use a completed VOD playlist.".into());
    }
    if media.segments.is_empty() {
        return Err("The HLS playlist contains no media segments.".into());
    }
    if media.segments.iter().any(|segment| {
        segment.key.as_ref().map(|key| key.method != KeyMethod::None).unwrap_or(false)
    }) {
        return Err("This HLS stream is encrypted. DownBest does not decrypt DRM or encrypted media.".into());
    }
    if media.segments.iter().any(|segment| segment.byte_range.is_some()) {
        return Err("HLS byte-range segments are not supported by the mobile saver yet.".into());
    }

    let map_uri = media.segments.iter().find_map(|segment| segment.map.as_ref().map(|map| map.uri.clone()));
    if media.segments.iter().filter_map(|segment| segment.map.as_ref()).any(|map| Some(&map.uri) != map_uri.as_ref()) {
        return Err("The HLS playlist changes its initialization segment. Use a desktop build for this stream.".into());
    }

    let extension = if map_uri.is_some() { "mp4" } else { "ts" };
    let base_name = suggested_name(source).unwrap_or_else(|| format!("hls-{}", &id[..8]));
    let stem = Path::new(&base_name).file_stem().and_then(|v| v.to_str()).unwrap_or("hls");
    let target = unique_path(manager, id, destination, &format!("{stem}.{extension}"))?;
    set_file_name(manager, id, target.file_name().and_then(|v| v.to_str()).unwrap_or("stream.ts"));

    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let checkpoint = PathBuf::from(format!("{}.downbest-{}.hls.json", target.display(), id));
    let total_segments = media.segments.len();

    // The signature prevents resuming a partial output against a changed playlist.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.as_str().hash(&mut hasher);
    media_url.as_str().hash(&mut hasher);
    map_uri.hash(&mut hasher);
    for segment in &media.segments {
        segment.uri.hash(&mut hasher);
    }
    let signature = hasher.finish();

    let loaded = fs::read(&checkpoint).await.ok()
        .and_then(|bytes| serde_json::from_slice::<HlsResumeState>(&bytes).ok());
    let temp_len = fs::metadata(&temp).await.map(|meta| meta.len()).unwrap_or(0);
    let mut state = match loaded {
        Some(saved)
            if saved.signature == signature
                && saved.next_segment <= total_segments
                && temp_len >= saved.committed_bytes => saved,
        _ => {
            let _ = fs::remove_file(&temp).await;
            let _ = fs::remove_file(&checkpoint).await;
            HlsResumeState { signature, next_segment: 0, committed_bytes: 0 }
        }
    };

    let save_checkpoint = |state: HlsResumeState| {
        let checkpoint = checkpoint.clone();
        async move {
            let bytes = serde_json::to_vec(&state)
                .map_err(|e| format!("Could not encode HLS checkpoint: {e}"))?;
            let staging = checkpoint.with_extension("hls.tmp");
            let mut file = File::create(&staging).await
                .map_err(|e| format!("Could not create HLS checkpoint: {e}"))?;
            file.write_all(&bytes).await
                .map_err(|e| format!("Could not write HLS checkpoint: {e}"))?;
            file.flush().await.map_err(|e| format!("Could not flush HLS checkpoint: {e}"))?;
            file.sync_all().await.map_err(|e| format!("Could not sync HLS checkpoint: {e}"))?;
            drop(file);
            if fs::rename(&staging, &checkpoint).await.is_err() {
                let _ = fs::remove_file(&checkpoint).await;
                fs::rename(&staging, &checkpoint).await
                    .map_err(|e| format!("Could not commit HLS checkpoint: {e}"))?;
            }
            Ok::<(), String>(())
        }
    };

    // If the last segment was committed but the app closed before rename, finish without downloading again.
    if state.next_segment == total_segments && temp_len >= state.committed_bytes && state.committed_bytes > 0 {
        let mut file = OpenOptions::new().write(true).open(&temp).await
            .map_err(|e| format!("Could not reopen completed HLS output: {e}"))?;
        file.set_len(state.committed_bytes).await
            .map_err(|e| format!("Could not trim recovered HLS output: {e}"))?;
        file.sync_all().await.map_err(|e| format!("Could not sync recovered HLS output: {e}"))?;
        drop(file);
        fs::rename(&temp, &target).await
            .map_err(|e| format!("Could not finalize recovered HLS output: {e}"))?;
        let _ = fs::remove_file(&checkpoint).await;
        set_file_path(manager, id, &target);
        set_progress(manager, id, 100.0);
        return Ok(());
    }

    let mut raw_output = OpenOptions::new().create(true).write(true).open(&temp).await
        .map_err(|e| format!("Cannot open HLS partial output: {e}"))?;
    raw_output.set_len(state.committed_bytes).await
        .map_err(|e| format!("Could not restore HLS partial length: {e}"))?;
    raw_output.seek(std::io::SeekFrom::Start(state.committed_bytes)).await
        .map_err(|e| format!("Could not seek to HLS resume position: {e}"))?;
    let mut output = BufWriter::with_capacity(128 * 1024, raw_output);
    let progress = progress_counter(manager, id)?;
    progress.store(state.committed_bytes, Ordering::Relaxed);
    set_progress(manager, id, state.next_segment as f64 / total_segments as f64 * 100.0);
    save_checkpoint(state.clone()).await?;

    // Write the initialization map only once, and checkpoint it before moving to media segments.
    if state.next_segment == 0 && state.committed_bytes == 0 {
        if let Some(uri) = map_uri.as_ref() {
            let map_url = media_url.join(uri)
                .map_err(|e| format!("Invalid HLS init segment URL: {e}"))?;
            let response = tokio::select! {
                _ = cancel.cancelled() => return Err("Cancelled".into()),
                result = client.get(map_url).send() => result.map_err(|e| format!("HLS init segment request failed: {e}"))?,
            };
            if !response.status().is_success() {
                return Err(format!("HLS init segment returned HTTP {}", response.status()));
            }
            let bytes = tokio::select! {
                _ = cancel.cancelled() => return Err("Cancelled".into()),
                result = response.bytes() => result.map_err(|e| format!("Could not read HLS init segment: {e}"))?,
            };
            output.write_all(&bytes).await
                .map_err(|e| format!("Could not write HLS init segment: {e}"))?;
            output.flush().await.map_err(|e| format!("Could not flush HLS init segment: {e}"))?;
            output.get_ref().sync_data().await
                .map_err(|e| format!("Could not sync HLS init segment: {e}"))?;
            state.committed_bytes = output.get_ref().metadata().await
                .map_err(|e| format!("Could not inspect HLS partial output: {e}"))?.len();
            save_checkpoint(state.clone()).await?;
            progress.store(state.committed_bytes, Ordering::Relaxed);
        }
    }

    for index in state.next_segment..total_segments {
        if cancel.is_cancelled() { return Err("Cancelled".into()); }
        let segment = &media.segments[index];
        let segment_url = media_url.join(&segment.uri)
            .map_err(|e| format!("Invalid HLS segment URL: {e}"))?;
        let response = tokio::select! {
            _ = cancel.cancelled() => return Err("Cancelled".into()),
            result = client.get(segment_url).send() => result.map_err(|e| format!("HLS segment request failed: {e}"))?,
        };
        if !response.status().is_success() {
            return Err(format!("HLS segment {} returned HTTP {}", index + 1, response.status()));
        }
        let bytes = tokio::select! {
            _ = cancel.cancelled() => return Err("Cancelled".into()),
            result = response.bytes() => result.map_err(|e| format!("Could not read HLS segment {}: {e}", index + 1))?,
        };
        output.write_all(&bytes).await
            .map_err(|e| format!("Could not write HLS segment {}: {e}", index + 1))?;
        output.flush().await.map_err(|e| format!("Could not flush HLS segment {}: {e}", index + 1))?;
        output.get_ref().sync_data().await
            .map_err(|e| format!("Could not sync HLS segment {}: {e}", index + 1))?;

        state.next_segment = index + 1;
        state.committed_bytes = output.get_ref().metadata().await
            .map_err(|e| format!("Could not inspect HLS partial output: {e}"))?.len();
        save_checkpoint(state.clone()).await?;
        progress.store(state.committed_bytes, Ordering::Relaxed);
        set_progress(manager, id, (state.next_segment as f64 / total_segments as f64) * 100.0);
    }

    output.flush().await.map_err(|e| format!("Could not flush HLS output: {e}"))?;
    output.get_ref().sync_all().await.map_err(|e| format!("Could not sync HLS output: {e}"))?;
    drop(output);
    fs::rename(&temp, &target).await.map_err(|e| format!("Could not finalize HLS output: {e}"))?;
    let _ = fs::remove_file(&checkpoint).await;
    set_file_path(manager, id, &target);
    Ok(())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn configure_no_console(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x08000000);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = command;
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn configure_tool_environment(command: &mut Command, tool_path: &Path) {
    if let Some(parent) = tool_path.parent() {
        let mut paths: Vec<PathBuf> = std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default()
        ).collect();
        paths.insert(0, parent.to_path_buf());
        if let Ok(joined) = std::env::join_paths(paths) {
            command.env("PATH", joined);
        }
        #[cfg(target_os = "linux")]
        {
            let lib_dir = parent.join("lib");
            if lib_dir.is_dir() {
                let mut library_paths = vec![lib_dir];
                library_paths.extend(std::env::split_paths(
                    &std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default()
                ));
                if let Ok(joined) = std::env::join_paths(library_paths) {
                    command.env("LD_LIBRARY_PATH", joined);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            let lib_dir = parent.join("lib");
            if lib_dir.is_dir() {
                let mut library_paths = vec![lib_dir];
                library_paths.extend(std::env::split_paths(
                    &std::env::var_os("DYLD_LIBRARY_PATH").unwrap_or_default()
                ));
                if let Ok(joined) = std::env::join_paths(library_paths) {
                    command.env("DYLD_LIBRARY_PATH", joined);
                }
            }
        }
    }
}

fn find_tool(app: &AppHandle, name: &str) -> Option<PathBuf> {
    let executable = if cfg!(windows) && !name.ends_with(".exe") { format!("{name}.exe") } else { name.to_string() };
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join("tools").join(&executable));
        candidates.push(resource_dir.join(&executable));
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            candidates.push(parent.join("tools").join(&executable));
            candidates.push(parent.join(&executable));
        }
    }
    #[cfg(target_os = "macos")]
    for base in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
        candidates.push(PathBuf::from(base).join(&executable));
    }
    #[cfg(target_os = "linux")]
    for base in ["/usr/local/bin", "/usr/bin", "/snap/bin"] {
        candidates.push(PathBuf::from(base).join(&executable));
    }
    for path in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        candidates.push(path.join(&executable));
    }
    candidates.into_iter().find(|path| path.is_file())
}


#[cfg(test)]
mod lite_mode_tests {
    use super::{cleanup_cancelled_files, download_http, effective_segment_count, needs_media_extractor, parse_content_range_start, partial_bytes_for, persist_records, update_media_progress, unique_path, DownloadManager, DownloadRecord};
    use reqwest::{header, Client};
    use std::{io::{Read, Write}, net::TcpListener, path::PathBuf, sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc}, time::Instant};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    #[test]
    fn transient_network_errors_are_retried_but_permanent_media_errors_are_not() {
        assert!(super::is_retryable_download_error("Download request failed: error sending request for url"));
        assert!(super::is_retryable_download_error("Network stream interrupted: connection reset by peer"));
        assert!(super::is_retryable_download_error("yt-dlp: HTTP Error 503: Service Unavailable"));
        assert!(super::is_retryable_download_error("Download stalled: no data was received for 90 seconds"));
        assert!(!super::is_retryable_download_error("This video is unavailable"));
        assert!(!super::is_retryable_download_error("Server returned HTTP 403"));
        assert!(!super::is_retryable_download_error("Disk write failed: permission denied"));
    }

    #[test]
    fn completed_media_file_is_recovered_when_extractor_does_not_print_its_path() {
        let root = std::env::temp_dir().join(format!("downbest-final-output-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create media output folder");
        let final_file = root.join("Sample video [abc123] - 12345678.mp4");
        let partial_file = root.join("Sample video [abc123] - 12345678.mp4.part");
        std::fs::write(&final_file, b"completed media").expect("write final media");
        std::fs::write(&partial_file, b"incomplete fragment").expect("write partial fragment");

        let recovered = super::find_completed_media_output(&root, "12345678-job")
            .expect("recover final media file");
        assert_eq!(recovered, final_file);
        std::fs::remove_dir_all(root).expect("remove media output folder");
    }

    #[test]
    fn a_real_download_error_is_failed_not_misreported_as_paused() {
        assert_eq!(
            super::terminal_status_after_download_error("yt-dlp finished but output file path was missing", false, false, false),
            "Failed"
        );
        assert_eq!(
            super::terminal_status_after_download_error("Cancelled", true, false, true),
            "Paused"
        );
        assert_eq!(
            super::terminal_status_after_download_error("Cancelled", true, true, false),
            "Cancelled"
        );
    }

    #[test]
    fn automatic_retry_backoff_is_capped_at_sixty_seconds() {
        assert_eq!(super::auto_retry_delay(1).as_secs(), 3);
        assert_eq!(super::auto_retry_delay(2).as_secs(), 6);
        assert_eq!(super::auto_retry_delay(6).as_secs(), 60);
        assert_eq!(super::auto_retry_delay(20).as_secs(), 60);
    }

    #[test]
    fn new_downloads_default_to_auto_and_only_allow_direct_file_override() {
        assert_eq!(super::normalize_download_kind("auto"), "auto");
        assert_eq!(super::normalize_download_kind("file"), "file");
        assert_eq!(super::normalize_download_kind("audio"), "audio");
        assert_eq!(super::normalize_download_kind("video"), "auto");
        assert_eq!(super::normalize_download_kind("video-only"), "auto");
        assert_eq!(super::normalize_download_kind("unknown"), "auto");
    }

    #[test]
    fn audio_mode_requests_audio_only_and_default_requests_best_video_with_audio() {
        assert_eq!(
            super::selected_media_format("audio", None, true).unwrap(),
            "bestaudio/best"
        );
        assert_eq!(
            super::selected_media_format("auto", None, true).unwrap(),
            "bestvideo+bestaudio/best"
        );
        assert_eq!(
            super::selected_media_format("auto", Some("audio"), true).unwrap(),
            "bestaudio/best"
        );
    }

    #[test]
    fn common_social_video_urls_use_the_media_extractor() {
        for raw in [
            "https://www.facebook.com/watch/?v=123",
            "https://m.facebook.com/reel/123",
            "https://fb.watch/abc123/",
            "https://www.instagram.com/reel/ABC123/",
            "https://instagr.am/p/ABC123/",
            "https://www.threads.net/@creator/post/123",
            "https://clips.twitch.tv/example",
        ] {
            let parsed = url::Url::parse(raw).expect("valid social media URL");
            assert!(needs_media_extractor(&parsed), "{raw} should use yt-dlp");
        }
    }

    #[test]
    fn youtube_and_facebook_tracking_variants_share_one_video_identity() {
        let youtube_a = url::Url::parse("https://www.youtube.com/watch?v=abc123&utm_source=share").unwrap();
        let youtube_b = url::Url::parse("https://youtu.be/abc123?si=tracking").unwrap();
        assert_eq!(super::download_identity_key(&youtube_a), super::download_identity_key(&youtube_b));

        let facebook_a = url::Url::parse("https://www.facebook.com/watch/?v=12345&ref=share").unwrap();
        let facebook_b = url::Url::parse("https://m.facebook.com/reel/12345/").unwrap();
        assert_eq!(super::download_identity_key(&facebook_a), super::download_identity_key(&facebook_b));
    }

    #[test]
    fn media_analysis_removes_duplicate_video_entries() {
        let raw = r#"{
            "_type":"playlist",
            "extractor_key":"Youtube",
            "title":"Sample list",
            "entries":[
                {"id":"abc123","title":"One","url":"https://www.youtube.com/watch?v=abc123"},
                {"id":"abc123","title":"One duplicate","url":"https://youtu.be/abc123?si=tracking"},
                {"id":"def456","title":"Two","url":"https://www.youtube.com/watch?v=def456"}
            ]
        }"#;
        let parsed = super::parse_media_analysis(raw, "https://www.youtube.com/playlist?list=test").unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].title, "One");
    }

    #[test]
    fn media_activity_messages_explain_extractor_stages() {
        assert_eq!(super::media_activity_from_line("[youtube] abc: Downloading webpage"), Some("Fetching video information"));
        assert_eq!(super::media_activity_from_line("[download] 25.0% of 10.00MiB"), Some("Downloading media"));
        assert_eq!(super::media_activity_from_line("[Merger] Merging formats into mp4"), Some("Merging audio and video"));
    }

    #[test]
    fn unrelated_download_hosts_are_not_forced_to_media_extractor() {
        let parsed = url::Url::parse("https://downloads.example.org/file.zip").unwrap();
        assert!(!needs_media_extractor(&parsed));
        assert!(super::has_known_direct_file_extension(&parsed));
        let audio = url::Url::parse("https://cdn.example.org/music.mp3?token=abc").unwrap();
        assert!(super::has_known_direct_file_extension(&audio));
        let manifest = url::Url::parse("https://cdn.example.org/stream.m3u8").unwrap();
        assert!(!super::has_known_direct_file_extension(&manifest));
    }

    fn queued_record(id: &str) -> DownloadRecord {
        DownloadRecord {
            id: id.to_string(),
            url: "https://example.com/file.zip".into(),
            file_name: "file.zip".into(),
            kind: "file".into(),
            status: "Queued".into(),
            destination: String::new(),
            file_path: None,
            downloaded: Arc::new(AtomicU64::new(0)),
            total: None,
            started: Instant::now(),
            error: None,
            cancel: CancellationToken::new(),
            progress_override: None,
            speed_bytes_sec: 0,
            concurrency: 2,
            etag: None,
            last_modified: None,
            cookies_file: None,
            format_selector: None,
            media_title: None,
            media_thumbnail: None,
            media_activity: None,
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn yt_dlp_progress_updates_the_queue_speed_in_bytes_per_second() {
        let manager = DownloadManager::default();
        let mut record = queued_record("speed-job");
        record.status = "Extracting media".into();
        manager.records.lock().expect("lock queue")
            .insert(record.id.clone(), record);

        update_media_progress(&manager, "speed-job", "4096|16384|16384|1536.75");

        let records = manager.records.lock().expect("lock queue");
        let updated = records.get("speed-job").expect("speed job");
        assert_eq!(updated.downloaded.load(Ordering::Relaxed), 4096);
        assert_eq!(updated.total, Some(16384));
        assert_eq!(updated.speed_bytes_sec, 1537);
        assert_eq!(updated.status, "Downloading");
        assert_eq!(updated.media_activity.as_deref(), Some("Downloading media"));
    }

    #[test]
    fn parses_standard_yt_dlp_progress_lines_for_desktop_live_updates() {
        let parsed = super::parse_yt_dlp_progress_line(
            "[download]  25.0% of 100.00MiB at 2.50MiB/s ETA 00:30"
        ).expect("parse a standard yt-dlp progress line");
        assert_eq!(parsed.0, Some(25 * 1024 * 1024));
        assert_eq!(parsed.1, Some(100 * 1024 * 1024));
        assert_eq!(parsed.2, Some(2_621_440));

        let final_line = super::parse_yt_dlp_progress_line(
            "[download] 100% of 1.00MB in 00:01 at 1.00MB/s"
        ).expect("parse a final yt-dlp progress line");
        assert_eq!(final_line.0, Some(1_000_000));
        assert_eq!(final_line.1, Some(1_000_000));
        assert_eq!(final_line.2, Some(1_000_000));
    }

    #[test]
    fn live_media_speed_uses_current_rate_not_lifetime_average() {
        let mut record = queued_record("live-media-speed");
        record.status = "Downloading".into();
        record.started = Instant::now() - std::time::Duration::from_secs(10);
        record.downloaded.store(20_000_000, Ordering::Relaxed);
        record.media_activity = Some("Downloading media".into());
        record.speed_bytes_sec = 1_000;

        assert_eq!(super::view_of(&record).speed_bytes_sec, 1_000);

        record.speed_bytes_sec = 0;
        assert_eq!(super::view_of(&record).speed_bytes_sec, 0);
    }

    #[test]
    fn cancelled_job_remains_cancelled_after_queue_restore() {
        let root = std::env::temp_dir().join(format!("downbest-cancel-restore-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create cancellation restore folder");
        let queue_path = root.join("queue.json");

        let original = DownloadManager::default();
        original.restore_queue(queue_path.clone());
        let mut record = queued_record("cancelled-restore");
        record.status = "Cancelling".into();
        record.cancel_requested.store(true, Ordering::Relaxed);
        original.records.lock().expect("lock original queue")
            .insert(record.id.clone(), record);
        persist_records(&original);

        let restored = DownloadManager::default();
        restored.restore_queue(queue_path);
        let records = restored.records.lock().expect("lock restored queue");
        let item = records.get("cancelled-restore").expect("restored cancelled job");
        assert_eq!(item.status, "Cancelled");
        assert!(item.cancel_requested.load(Ordering::Relaxed));
        drop(records);
        std::fs::remove_dir_all(root).expect("remove cancellation restore folder");
    }

    #[test]
    fn restored_legacy_queue_does_not_auto_resume_duplicate_media() {
        let folder = std::env::temp_dir().join(format!("downbest-dedup-restore-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("create duplicate restore folder");
        let queue_file = folder.join("queue.json");

        let manager = DownloadManager::default();
        manager.restore_queue(queue_file.clone());
        let mut first = queued_record("duplicate-one");
        first.url = "https://www.youtube.com/watch?v=restore-duplicate&utm_source=share".into();
        first.status = "Downloading".into();
        first.destination = folder.display().to_string();
        let mut second = queued_record("duplicate-two");
        second.url = "https://youtu.be/restore-duplicate?si=tracking".into();
        second.status = "Downloading".into();
        second.destination = folder.display().to_string();
        manager.records.lock().expect("lock original queue").extend([
            (first.id.clone(), first),
            (second.id.clone(), second),
        ]);
        persist_records(&manager);

        let reopened = DownloadManager::default();
        let auto_resume = reopened.restore_queue(queue_file);
        assert_eq!(auto_resume.len(), 1, "only one copy of a video should auto-resume");

        let records = reopened.records.lock().expect("lock restored queue");
        let queued_count = records.values().filter(|item| item.status == "Queued").count();
        let cancelled_count = records.values().filter(|item| item.status == "Cancelled").count();
        assert_eq!(queued_count, 1);
        assert_eq!(cancelled_count, 1);
        drop(records);
        std::fs::remove_dir_all(folder).expect("remove duplicate restore folder");
    }

    #[test]
    fn interrupted_jobs_auto_resume_but_user_paused_jobs_stay_paused() {
        let folder = std::env::temp_dir().join(format!("downbest-auto-resume-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("create auto-resume directory");
        let queue_file = folder.join("queue.json");

        let manager = DownloadManager::default();
        manager.restore_queue(queue_file.clone());
        let mut interrupted = queued_record("interrupted-job");
        interrupted.url = "https://example.com/interrupted-file.zip".into();
        interrupted.status = "Downloading".into();
        interrupted.destination = folder.display().to_string();
        let mut manual_pause = queued_record("manual-pause-job");
        manual_pause.url = "https://example.com/manual-pause-file.zip".into();
        manual_pause.status = "Paused".into();
        manager.records.lock().expect("lock original queue").extend([
            (interrupted.id.clone(), interrupted),
            (manual_pause.id.clone(), manual_pause),
        ]);
        persist_records(&manager);

        let reopened = DownloadManager::default();
        let auto_resume = reopened.restore_queue(queue_file);
        assert!(auto_resume.iter().any(|id| id == "interrupted-job"));
        assert!(!auto_resume.iter().any(|id| id == "manual-pause-job"));

        let records = reopened.records.lock().expect("lock restored queue");
        assert_eq!(records.get("interrupted-job").unwrap().status, "Queued");
        assert_eq!(records.get("manual-pause-job").unwrap().status, "Paused");
        drop(records);
        std::fs::remove_dir_all(folder).expect("remove auto-resume directory");
    }

    #[test]
    fn interrupted_media_with_a_final_output_is_restored_as_completed_not_resumed() {
        let folder = std::env::temp_dir().join(format!("downbest-final-restore-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("create final output recovery folder");
        let queue_file = folder.join("queue.json");
        let id = "abcdef1234567890";
        let final_file = folder.join("Finished clip [video123] - abcdef12.mp4");
        std::fs::write(&final_file, b"complete video bytes").expect("write final output");

        let original = DownloadManager::default();
        original.restore_queue(queue_file.clone());
        let mut interrupted = queued_record(id);
        interrupted.url = "https://www.youtube.com/watch?v=video123".into();
        interrupted.status = "Downloading".into();
        interrupted.destination = folder.display().to_string();
        original.records.lock().expect("lock original queue")
            .insert(id.to_string(), interrupted);
        persist_records(&original);

        let reopened = DownloadManager::default();
        let auto_resume = reopened.restore_queue(queue_file);
        assert!(auto_resume.is_empty(), "a final output must never be auto-resumed");
        let records = reopened.records.lock().expect("lock restored queue");
        let restored = records.get(id).expect("restored completed media item");
        assert_eq!(restored.status, "Completed");
        assert_eq!(restored.file_path.as_deref(), Some(final_file.to_string_lossy().as_ref()));
        assert_eq!(restored.downloaded.load(Ordering::Relaxed), b"complete video bytes".len() as u64);
        assert_eq!(restored.progress_override, Some(100.0));
        drop(records);
        std::fs::remove_dir_all(folder).expect("remove final output recovery folder");
    }

    #[test]
    fn cancelling_a_job_removes_its_http_and_extractor_temporary_files() {
        let root = std::env::temp_dir().join(format!("downbest-cancel-cleanup-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create cancellation test folder");
        let id = "0123456789abcdef0123456789abcdef";
        let target = root.join("sample.mp4");
        let http_partial = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
        let parts_dir = root.join(format!(".downbest-{id}"));
        let extractor_partial = root.join(format!("sample - {}.mp4.part", &id[..8]));
        let hls_checkpoint = PathBuf::from(format!("{}.downbest-{}.hls.json", target.display(), id));
        let hls_staging = hls_checkpoint.with_extension("hls.tmp");
        std::fs::write(&http_partial, b"partial").expect("write HTTP partial");
        std::fs::create_dir_all(&parts_dir).expect("create range parts folder");
        std::fs::write(parts_dir.join("part-000"), b"range").expect("write range part");
        std::fs::write(&extractor_partial, b"extractor partial").expect("write extractor partial");
        std::fs::write(&hls_checkpoint, b"checkpoint").expect("write HLS checkpoint");
        std::fs::write(&hls_staging, b"staging").expect("write HLS checkpoint staging");

        let manager = DownloadManager::default();
        let mut record = queued_record(id);
        record.destination = root.display().to_string();
        record.file_path = Some(target.display().to_string());
        manager.records.lock().expect("lock records").insert(id.to_string(), record);

        cleanup_cancelled_files(&manager, id);
        assert!(!http_partial.exists());
        assert!(!parts_dir.exists());
        assert!(!extractor_partial.exists());
        assert!(!hls_checkpoint.exists());
        assert!(!hls_staging.exists());
        std::fs::remove_dir_all(root).expect("remove cancellation test folder");
    }

    #[test]
    fn concurrent_same_name_downloads_reserve_distinct_paths() {
        let folder = std::env::temp_dir().join(format!("downbest-path-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("create test directory");
        let manager = DownloadManager::default();
        {
            let mut records = manager.records.lock().expect("lock queue");
            records.insert("first".into(), queued_record("first"));
            records.insert("second".into(), queued_record("second"));
        }
        let first = unique_path(&manager, "first", &folder, "same.zip").expect("reserve first path");
        let second = unique_path(&manager, "second", &folder, "same.zip").expect("reserve second path");
        assert_ne!(first, second);
        assert_eq!(first.file_name().and_then(|value| value.to_str()), Some("same.zip"));
        assert_eq!(second.file_name().and_then(|value| value.to_str()), Some("same (1).zip"));
        std::fs::remove_dir_all(folder).expect("remove test directory");
    }

    #[test]
    fn interrupted_http_download_resumes_from_saved_byte_offset_after_restart() {
        let root = std::env::temp_dir().join(format!("downbest-http-resume-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create test directory");
        let queue_path = root.join("queue.json");
        let mut target = root.join("resume.bin");
        let partial = PathBuf::from(format!("{}.downbest-resume-job.tmp", target.display()));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP server");
        listener.set_nonblocking(true).expect("set bounded HTTP accept");
        let address = listener.local_addr().expect("read server address");

        let server = std::thread::spawn(move || {
            const BODY: &[u8] = b"0123456789abcdef";
            for step in 0..4 {
                let deadline = Instant::now() + std::time::Duration::from_secs(10);
                let (mut socket, _) = loop {
                    match listener.accept() {
                        Ok(connection) => break connection,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        Err(error) => panic!("timed out waiting for HTTP request {step}: {error}"),
                    }
                };
                socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let read = socket.read(&mut buffer).expect("read HTTP headers");
                    if read == 0 { break; }
                    request.extend_from_slice(&buffer[..read]);
                }
                let request_text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                match step {
                    0 | 2 => {
                        assert!(request_text.contains("range: bytes=0-0"));
                        let headers = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/{}\r\nAccept-Ranges: bytes\r\nETag: \"stable\"\r\nConnection: close\r\n\r\n",
                            BODY.len()
                        );
                        socket.write_all(headers.as_bytes()).expect("write range-probe headers");
                        socket.write_all(b"x").expect("write range-probe byte");
                    }
                    1 => {
                        assert!(!request_text.contains("range: bytes="));
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nETag: \"stable\"\r\nConnection: close\r\n\r\n",
                            BODY.len()
                        );
                        socket.write_all(headers.as_bytes()).expect("write interrupted response headers");
                        socket.write_all(&BODY[..8]).expect("write partial body");
                    }
                    3 => {
                        assert!(request_text.contains("range: bytes=8-"), "resume request did not start at byte 8: {request_text}");
                        let headers = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 8-{}/{}\r\nAccept-Ranges: bytes\r\nETag: \"stable\"\r\nConnection: close\r\n\r\n",
                            BODY.len() - 8, BODY.len() - 1, BODY.len()
                        );
                        socket.write_all(headers.as_bytes()).expect("write resumed response headers");
                        socket.write_all(&BODY[8..]).expect("write remaining body");
                    }
                    _ => unreachable!(),
                }
            }
        });

        let manager = DownloadManager::default();
        manager.restore_queue(queue_path.clone());
        let mut record = queued_record("resume-job");
        record.status = "Downloading".into();
        record.destination = root.display().to_string();
        record.file_path = Some(target.display().to_string());
        manager.records.lock().expect("lock queue").insert(record.id.clone(), record);
        persist_records(&manager);

        let client = Client::builder()
            .default_headers({
                let mut headers = header::HeaderMap::new();
                headers.insert(header::ACCEPT_ENCODING, header::HeaderValue::from_static("identity"));
                headers
            })
            .build().expect("create HTTP client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().expect("create async runtime");

        let first_attempt = runtime.block_on(download_http(
            &client, &format!("http://{address}/resume.bin"), &mut target, &root,
            "resume-job", 1, &manager, &CancellationToken::new()
        ));
        assert!(first_attempt.is_err(), "first connection should be intentionally interrupted");
        assert_eq!(std::fs::metadata(&partial).expect("partial file exists").len(), 8);
        persist_records(&manager);

        let restarted = DownloadManager::default();
        restarted.restore_queue(queue_path);
        {
            let records = restarted.records.lock().expect("lock restored queue");
            let restored = records.get("resume-job").expect("restored download");
            assert_eq!(restored.status, "Queued");
            assert_eq!(partial_bytes_for(restored), 8);
        }

        let resumed = runtime.block_on(download_http(
            &client, &format!("http://{address}/resume.bin"), &mut target, &root,
            "resume-job", 1, &restarted, &CancellationToken::new()
        ));
        assert!(resumed.is_ok(), "second attempt should resume successfully: {resumed:?}");
        server.join().expect("local HTTP server completed");
        assert_eq!(std::fs::read(&target).expect("read completed file"), b"0123456789abcdef");
        std::fs::remove_dir_all(root).expect("remove test directory");
    }

    #[test]
    fn process_restart_restores_paused_job_and_partial_file() {
        let folder = std::env::temp_dir().join(format!("downbest-recovery-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("create recovery directory");
        let queue_file = folder.join("queue.json");
        let target = folder.join("movie.mp4");
        let partial = PathBuf::from(format!("{}.downbest-recovery-job.tmp", target.display()));
        std::fs::write(&partial, vec![7u8; 4096]).expect("write recoverable partial data");

        let manager = DownloadManager::default();
        manager.restore_queue(queue_file.clone());
        let mut record = queued_record("recovery-job");
        record.status = "Downloading".into();
        record.destination = folder.display().to_string();
        record.file_path = Some(target.display().to_string());
        record.downloaded.store(4096, Ordering::Relaxed);
        manager.records.lock().expect("lock original queue").insert(record.id.clone(), record);
        persist_records(&manager);

        // A fresh manager is equivalent to a new process opening the persisted queue.
        let reopened = DownloadManager::default();
        reopened.restore_queue(queue_file);
        let records = reopened.records.lock().expect("lock restored queue");
        let restored = records.get("recovery-job").expect("restored unfinished job");
        assert_eq!(restored.status, "Queued");
        assert_eq!(partial_bytes_for(restored), 4096);
        assert_eq!(restored.downloaded.load(Ordering::Relaxed), 4096);
        drop(records);
        std::fs::remove_dir_all(folder).expect("remove recovery directory");
    }


    #[test]
    fn media_playlist_json_is_expanded_into_individual_video_entries() {
        let json = r#"{
            "title":"My playlist",
            "extractor_key":"YoutubeTab",
            "_type":"playlist",
            "playlist_count":2,
            "entries":[
                {"id":"abcdefghijk","title":"First video","url":"https://www.youtube.com/watch?v=abcdefghijk","duration":85},
                {"id":"lmnopqrstuv","title":"Second video","url":"https://www.youtube.com/watch?v=lmnopqrstuv","duration":125}
            ]
        }"#;
        let parsed = super::parse_media_analysis(json, "https://www.youtube.com/playlist?list=PL123")
            .expect("parse playlist");
        assert!(parsed.is_playlist);
        assert_eq!(parsed.entry_count, 2);
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].title, "First video");
        assert_eq!(parsed.entries[1].url, "https://www.youtube.com/watch?v=lmnopqrstuv");
    }

    #[test]
    fn flat_youtube_video_metadata_gets_a_downloadable_watch_url() {
        let json = r#"{"id":"abcdefghijk","title":"Single video","extractor_key":"Youtube","duration":42}"#;
        let parsed = super::parse_media_analysis(json, "https://youtu.be/abcdefghijk")
            .expect("parse video metadata");
        assert!(!parsed.is_playlist);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].url, "https://youtu.be/abcdefghijk");
    }

    #[test]
    fn available_media_formats_include_resolution_and_audio_video_flags() {
        let json = r#"{"formats":[
            {"format_id":"18","ext":"mp4","height":360,"width":640,"vcodec":"avc1","acodec":"mp4a"},
            {"format_id":"137","ext":"mp4","height":1080,"width":1920,"vcodec":"avc1","acodec":"none"},
            {"format_id":"140","ext":"m4a","vcodec":"none","acodec":"mp4a"}
        ]}"#;
        let formats = super::parse_media_formats(json).expect("parse formats");
        assert_eq!(formats.len(), 3);
        let high_video = formats.iter().find(|item| item.format_id == "137").unwrap();
        assert_eq!(high_video.resolution, "1080p");
        assert!(high_video.is_video);
        assert!(!high_video.is_audio);
        let progressive = formats.iter().find(|item| item.format_id == "18").unwrap();
        assert!(progressive.is_video && progressive.is_audio);
    }

    #[test]
    fn parses_extended_content_disposition_filenames_and_sanitizes_paths() {
        let name = super::parse_content_disposition_filename(
            "attachment; filename=ignored.zip; filename*=UTF-8''Quarter%20Report%20%E2%80%93%20Final.pdf"
        ).expect("extended filename");
        assert_eq!(name, "Quarter Report – Final.pdf");
        let safe = super::sanitize_download_filename("../../folder/installer%20x.exe")
            .expect("sanitized path");
        assert_eq!(safe, "installer x.exe");
    }

    #[test]
    fn direct_download_name_uses_query_filename_and_mime_extension() {
        let url = url::Url::parse("https://cdn.example.test/download?filename=release%20notes").unwrap();
        let name = super::suggested_name(&url).expect("query filename");
        assert_eq!(name, "release notes");
        assert_eq!(super::extension_for_content_type("application/pdf; charset=binary"), Some("pdf"));
    }

    #[test]
    fn content_range_parser_checks_start_end_and_total() {
        let value = header::HeaderValue::from_static("bytes 4096-8191/16384");
        assert_eq!(super::parse_content_range_span(Some(&value)), Some((4096, 8191, Some(16384))));
        let invalid = header::HeaderValue::from_static("bytes 8191-4096/16384");
        assert_eq!(super::parse_content_range_span(Some(&invalid)), None);
    }

    #[test]
    fn byte_range_resume_reads_the_exact_start_offset() {
        let value = header::HeaderValue::from_static("bytes 4096-8191/16384");
        assert_eq!(parse_content_range_start(Some(&value)), Some(4096));
    }

    #[test]
    fn lite_mode_uses_one_stream_for_small_files() {
        assert_eq!(effective_segment_count(15 * 1024 * 1024, 2), 1);
    }

    #[test]
    fn lite_mode_caps_large_downloads_at_two_segments() {
        assert_eq!(effective_segment_count(16 * 1024 * 1024, 2), 2);
        assert_eq!(effective_segment_count(128 * 1024 * 1024, 2), 2);
    }

    #[test]
    fn balanced_and_high_modes_respect_requested_limits() {
        assert_eq!(effective_segment_count(32 * 1024 * 1024, 4), 4);
        assert_eq!(effective_segment_count(64 * 1024 * 1024, 8), 8);
        assert_eq!(effective_segment_count(64 * 1024 * 1024, 32), 8);
    }

    #[test]
    fn turbo_profile_can_use_sixteen_parallel_ranges_on_large_files() {
        assert_eq!(effective_segment_count(256 * 1024 * 1024, 16), 16);
        assert_eq!(effective_segment_count(256 * 1024 * 1024, 32), 16);
    }

    #[test]
    fn lite_profile_stays_at_two_connections_for_large_files() {
        assert_eq!(effective_segment_count(256 * 1024 * 1024, 2), 2);
    }
}

#[tauri::command]
fn supports_folder_picker() -> bool {
    cfg!(not(any(target_os = "android", target_os = "ios")))
}

#[tauri::command]
async fn reveal_download(manager: State<'_, DownloadManager>, id: String) -> Result<(), String> {
    let path = {
        let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        records.get(&id).and_then(|record| record.file_path.clone())
            .ok_or_else(|| "The completed download has no saved file path.".to_string())?
    };
    let file = PathBuf::from(path);
    if !file.is_file() {
        return Err("The downloaded file is no longer available at the saved path.".into());
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("explorer.exe");
        command.arg(format!("/select,{}", file.display()));
        configure_no_console(&mut command);
        command.spawn().map_err(|error| format!("Could not open File Explorer: {error}"))?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(&file).spawn()
            .map_err(|error| format!("Could not reveal the file in Finder: {error}"))?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let folder = file.parent().ok_or_else(|| "The file has no parent folder.".to_string())?;
        Command::new("xdg-open").arg(folder).spawn()
            .map_err(|error| format!("Could not open the download folder: {error}"))?;
        Ok(())
    }
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Err("Show in folder is available in desktop builds.".into())
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(DownloadManager::default())
        .setup(|app| {
            if let Ok(directory) = app.path().app_data_dir() {
                let manager = app.state::<DownloadManager>();
                let interrupted = manager.restore_queue(directory.join("queue.json"));
                let manager = manager.inner().clone();
                let handle = app.handle().clone();
                for id in interrupted {
                    let _ = resume_download_inner(handle.clone(), &manager, id, true);
                }
            }
            Ok(())
        });
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_media::init());
    builder
        .invoke_handler(tauri::generate_handler![
            start_download, resume_download, get_downloads, pause_download, cancel_download, clear_finished,
            check_engines, supports_folder_picker, default_save_location, reveal_download, open_download,
            analyze_media, get_media_formats
        ])
        .run(tauri::generate_context!())
        .expect("error while running DownBest");
}
