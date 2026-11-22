use futures_util::{future::try_join_all, StreamExt};
use reqwest::{header, Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex},
    time::Instant,
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncSeekExt, AsyncWriteExt, BufWriter},
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
use tauri_plugin_media::MediaCancelRequest;

#[derive(Clone)]
struct DownloadManager {
    records: Arc<Mutex<HashMap<String, DownloadRecord>>>,
    slots: Arc<Semaphore>,
    persistence_file: Arc<Mutex<Option<PathBuf>>>,
}
impl Default for DownloadManager {
    fn default() -> Self {
        Self {
            records: Arc::new(Mutex::new(HashMap::new())),
            slots: Arc::new(Semaphore::new(2)),
            persistence_file: Arc::new(Mutex::new(None)),
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
    progress_override: Option<f64>, concurrency: usize,
    etag: Option<String>, last_modified: Option<String>,
    cookies_file: Option<String>,
}

impl DownloadManager {
    fn restore_queue(&self, path: PathBuf) {
        if let Some(parent) = path.parent() { let _ = std::fs::create_dir_all(parent); }
        if let Ok(mut slot) = self.persistence_file.lock() { *slot = Some(path.clone()); }
        let fallback = path.with_extension("json.tmp");
        let bytes = std::fs::read(&path).or_else(|_| std::fs::read(&fallback));
        let Ok(bytes) = bytes else { return; };
        let Ok(items) = serde_json::from_slice::<Vec<PersistedDownload>>(&bytes) else { return; };
        if let Ok(mut records) = self.records.lock() {
            for item in items {
                let status = match item.status.as_str() {
                    "Completed" | "Failed" => item.status.clone(),
                    _ => "Paused".to_string(),
                };
                records.insert(item.id.clone(), DownloadRecord {
                    id: item.id, url: item.url, file_name: item.file_name, kind: item.kind,
                    status, destination: item.destination, file_path: item.file_path,
                    downloaded: Arc::new(AtomicU64::new(item.downloaded_bytes)),
                    total: item.total_bytes, started: Instant::now(), error: item.error,
                    cancel: CancellationToken::new(), progress_override: None,
                    concurrency: item.concurrency.clamp(1, 8),
                    etag: item.etag, last_modified: item.last_modified,
                    cookies_file: item.cookies_file,
                });
            }
        }
        persist_records(self);
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
        !line.trim().is_empty() && !line.starts_with('#') && line.split('\t').count() >= 7
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
}
#[derive(Serialize)]
struct ToolStatus { name: String, available: bool, version: Option<String> }

fn view_of(record: &DownloadRecord) -> DownloadView {
    let downloaded = record.downloaded.load(Ordering::Relaxed);
    let calculated = record.total.filter(|n| *n > 0)
        .map(|n| (downloaded as f64 / n as f64 * 100.0).clamp(0.0, 100.0)).unwrap_or(0.0);
    let progress = record.progress_override.unwrap_or(calculated);
    let elapsed = record.started.elapsed().as_secs_f64().max(0.5);
    DownloadView {
        id: record.id.clone(), url: record.url.clone(), file_name: record.file_name.clone(),
        kind: record.kind.clone(), status: record.status.clone(), destination: record.destination.clone(),
        file_path: record.file_path.clone(), downloaded_bytes: downloaded, total_bytes: record.total,
        speed_bytes_sec: if matches!(record.status.as_str(), "Downloading" | "Extracting media") {
            (downloaded as f64 / elapsed) as u64
        } else { 0 },
        progress, error: record.error.clone(),
    }
}
fn set_status(manager: &DownloadManager, id: &str, status: &str, error: Option<String>) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            if record.status != status && matches!(status, "Downloading" | "Extracting media") {
                record.started = Instant::now();
            }
            if status == "Completed" { record.progress_override = Some(100.0); }
            record.status = status.to_string();
            record.error = error;
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
            record.progress_override = Some(progress.progress.clamp(0.0, 100.0));
            if progress.downloaded_bytes > 0 && record.status != "Downloading" {
                record.started = Instant::now();
                record.status = "Downloading".into();
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
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) {
            if let Some(value) = downloaded {
                record.downloaded.store(value, Ordering::Relaxed);
            }
            if let Some(value) = total {
                record.total = Some(value);
            }
            if downloaded.unwrap_or(0) > 0 && record.status != "Downloading" {
                record.started = Instant::now();
                record.status = "Downloading".into();
                record.error = None;
            }
        }
    }
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


fn spawn_download_task(
    app: AppHandle, manager: DownloadManager, id: String, url: String, kind: String,
    destination: PathBuf, concurrency: usize, cookies_file: Option<String>, cancel: CancellationToken,
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
        let result = execute_download(app, manager.clone(), id.clone(), url, kind,
            destination, concurrency, cookies_file, cancel.clone()).await;
        cancel.cancel();
        let _ = saver.await;
        match result {
            Ok(()) => {
                set_status(&manager, &id, "Completed", None);
                cleanup_job_cookies(&manager, &id);
            },
            Err(error) if error == "Cancelled" => set_status(&manager, &id, "Paused", None),
            Err(error) => set_status(&manager, &id, "Failed", Some(error)),
        }
    });
}

#[tauri::command]
async fn start_download(
    app: AppHandle, manager: State<'_, DownloadManager>, url: String, kind: String,
    destination: Option<String>, concurrency: Option<usize>, cookies: Option<String>,
) -> Result<DownloadView, String> {
    if url.len() > 8192 { return Err("The download URL is too long.".into()); }
    let parsed = Url::parse(&url).map_err(|_| "Enter a valid http:// or https:// URL.".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") { return Err("Only HTTP and HTTPS links are supported.".into()); }
    let kind = match kind.as_str() {
        "auto" | "file" | "audio" | "video" | "video-only" => kind,
        _ => "auto".to_string(),
    };
    let mobile_default = app.path().app_data_dir().ok().map(|path| path.join("Downloads"));
    let default_destination = if cfg!(any(target_os = "android", target_os = "ios")) {
        mobile_default.or_else(|| dirs::document_dir()).unwrap_or_else(std::env::temp_dir)
    } else {
        dirs::download_dir().or_else(|| dirs::document_dir()).unwrap_or_else(std::env::temp_dir)
    };
    let destination_path = match destination {
        Some(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
        _ => default_destination,
    };
    let id = Uuid::new_v4().simple().to_string();
    let cookies_file = write_job_cookies(&app, &id, cookies)?;
    let requested = concurrency.unwrap_or(2).clamp(1, 8);
    let cancel = CancellationToken::new();
    let record = DownloadRecord {
        id: id.clone(), url: url.clone(),
        file_name: suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8])),
        kind: kind.clone(), status: "Queued".into(),
        destination: destination_path.display().to_string(), file_path: None,
        downloaded: Arc::new(AtomicU64::new(0)), total: None, started: Instant::now(),
        error: None, cancel: cancel.clone(), progress_override: None,
        concurrency: requested, etag: None, last_modified: None,
        cookies_file: cookies_file.clone(),
    };
    let initial = view_of(&record);
    let manager = manager.inner().clone();
    manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?.insert(id.clone(), record);
    persist_records(&manager);
    spawn_download_task(app, manager, id, url, kind, destination_path, requested, cookies_file, cancel);
    Ok(initial)
}

#[tauri::command]
fn resume_download(
    app: AppHandle, manager: State<'_, DownloadManager>, id: String,
) -> Result<DownloadView, String> {
    let (url, kind, destination, concurrency, cookies_file, cancel, view) = {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let record = records.get_mut(&id).ok_or_else(|| "Download was not found.".to_string())?;
        if record.status == "Completed" { return Err("This download is already complete.".into()); }
        if !matches!(record.status.as_str(), "Paused" | "Failed" | "Cancelled") {
            return Err("Pause the running download before resuming it.".into());
        }
        let recovered_bytes = partial_bytes_for(record);
        record.downloaded.store(recovered_bytes, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        record.cancel = cancel.clone();
        record.status = "Queued".into();
        record.error = None;
        record.started = Instant::now();
        record.progress_override = None;
        let destination = PathBuf::from(&record.destination);
        let view = view_of(record);
        (record.url.clone(), record.kind.clone(), destination, record.concurrency, record.cookies_file.clone(), cancel, view)
    };
    persist_records(manager.inner());
    spawn_download_task(app, manager.inner().clone(), id, url, kind, destination, concurrency, cookies_file, cancel);
    Ok(view)
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
    let was_media = {
        let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
        let record = records.get_mut(&id).ok_or_else(|| "Download was not found.".to_string())?;
        if matches!(record.status.as_str(), "Completed" | "Paused" | "Failed") { return Ok(()); }
        let media = record.status == "Extracting media";
        record.status = "Pausing".into();
        record.cancel.cancel();
        media
    };
    persist_records(manager.inner());
    #[cfg(target_os = "android")]
    if was_media {
        let handle = app.clone();
        let job_id = id.clone();
        tokio::task::spawn_blocking(move || handle.media().cancel(MediaCancelRequest { job_id }))
            .await.map_err(|e| format!("Android media cancellation worker failed: {e}"))?
            .map_err(|e| format!("Could not pause Android media download: {e}"))?;
    }
    #[cfg(not(target_os = "android"))]
    let _ = (app, was_media);
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
    for name in ["yt-dlp", "aria2c", "ffmpeg", "ffprobe"] {
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
    destination: PathBuf, concurrency: usize, cookies_file: Option<String>, cancel: CancellationToken,
) -> Result<(), String> {
    let _permit = tokio::select! {
        _ = cancel.cancelled() => return Err("Cancelled".into()),
        permit = manager.slots.clone().acquire_owned() => permit.map_err(|_| "Download queue is shutting down.".to_string())?,
    };
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    fs::create_dir_all(&destination).await.map_err(|e| format!("Cannot create save folder: {e}"))?;
    set_status(&manager, &id, "Connecting", None);
    let parsed = Url::parse(&url).map_err(|e| format!("Invalid URL: {e}"))?;
    let client = Client::builder().user_agent("DownBest/1.0 (+https://github.com/nazim-assdi/DownBest)")
        .connect_timeout(std::time::Duration::from_secs(20))
        .pool_idle_timeout(std::time::Duration::from_secs(45)).pool_max_idle_per_host(8)
        .default_headers({
            let mut headers = header::HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, header::HeaderValue::from_static("identity"));
            headers
        })
        .build().map_err(|e| format!("Could not initialize network client: {e}"))?;

    // Known media domains/manifests go straight to yt-dlp. For other URLs,
    // inspect the response type so generic yt-dlp-supported web pages also work.
    let media_page = kind != "file"
        && (needs_media_extractor(&parsed) || looks_like_html_page(&client, &url).await);
    if media_page {
        set_status(&manager, &id, "Extracting media", None);
        return run_media_download(&app, &manager, &id, &parsed, &kind, &destination, concurrency, cookies_file.as_deref(), &cancel).await;
    }
    let name = suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8]));
    let target = unique_path(&manager, &id, &destination, &name)?;
    set_file_name(&manager, &id, target.file_name().and_then(|s| s.to_str()).unwrap_or(&name));
    set_status(&manager, &id, "Downloading", None);
    download_http(&client, &url, &target, &destination, &id, concurrency, &manager, &cancel).await?;
    set_file_path(&manager, &id, &target);
    Ok(())
}

async fn looks_like_html_page(client: &Client, url: &str) -> bool {
    let timeout = std::time::Duration::from_secs(8);
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
fn suggested_name(url: &Url) -> Option<String> {
    let raw = url.path_segments()?.filter(|s| !s.is_empty()).last()?;
    let decoded = raw.replace("%20", " ").replace("%2E", ".");
    let safe: String = decoded.chars().filter(|c| !c.is_control()
        && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')).take(170).collect();
    let safe = safe.trim().trim_matches('.').to_string();
    if safe.is_empty() { None } else { Some(safe) }
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
fn effective_segment_count(total_bytes: u64, requested: usize) -> usize {
    const MIN_PARALLEL_FILE: u64 = 16 * 1024 * 1024;
    const TARGET_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;
    if total_bytes < MIN_PARALLEL_FILE {
        return 1;
    }
    let requested = requested.clamp(1, 8);
    requested.min(((total_bytes / TARGET_SEGMENT_BYTES).max(1)) as usize).max(1)
}

fn parse_content_range(value: Option<&header::HeaderValue>) -> Option<u64> {
    let raw = value?.to_str().ok()?;
    let (_, total) = raw.split_once('/')?;
    if total == "*" { None } else { total.parse().ok() }
}
fn parse_content_range_start(value: Option<&header::HeaderValue>) -> Option<u64> {
    let raw = value?.to_str().ok()?;
    raw.strip_prefix("bytes ")?.split('-').next()?.parse().ok()
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
        if parse_content_range_start(response.headers().get(header::CONTENT_RANGE)) == Some(offset) {
            return Ok((response, true));
        }
        return Err("The server returned a mismatched Content-Range while resuming.".into());
    }
    if response.status().is_success() { return Ok((response, false)); }
    Err(format!("Server returned HTTP {} while resuming.", response.status()))
}

async fn download_http(
    client: &Client, url: &str, target: &Path, destination: &Path, id: &str,
    concurrency: usize, manager: &DownloadManager, cancel: &CancellationToken,
) -> Result<(), String> {
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let parts_dir = destination.join(format!(".downbest-{id}"));
    let previous_len = fs::metadata(&temp).await.map(|meta| meta.len()).unwrap_or(0);
    let (previous_etag, previous_modified) = saved_validators(manager, id);
    let probe = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client.get(url).header(header::RANGE, "bytes=0-0").send(),
    ).await.map_err(|_| "The server did not respond to the download probe within 30 seconds.".to_string())?
        .map_err(|e| format!("Connection failed: {e}"))?;
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

    if probe.status() == StatusCode::PARTIAL_CONTENT {
        let total = parse_content_range(probe.headers().get(header::CONTENT_RANGE))
            .ok_or_else(|| "The server returned an invalid Content-Range header.".to_string())?;
        set_total(manager, id, Some(total));
        if partial_len == total && total > 0 {
            drop(probe);
            fs::rename(&temp, target).await.map_err(|e| format!("Could not finalize recovered download: {e}"))?;
            return Ok(());
        }
        let has_segment_parts = fs::metadata(&parts_dir).await.map(|m| m.is_dir()).unwrap_or(false);
        let count = if partial_len > 0 { 1 } else { effective_segment_count(total, concurrency) };
        if count > 1 || has_segment_parts {
            drop(probe);
            let progress = progress_counter(manager, id)?;
            match download_segmented(client, url, target, destination, id, total, count, progress, cancel).await {
                Ok(()) => Ok(()),
                Err(error) if error == "Cancelled" => Err(error),
                Err(error) if error.contains("stopped supporting parallel ranges") => {
                    let _ = fs::remove_dir_all(&parts_dir).await;
                    reset_progress(manager, id);
                    let response = client.get(url).send().await.map_err(|e| format!("Parallel download failed and fallback connection failed: {e}"))?;
                    if !response.status().is_success() { return Err(format!("Parallel download fallback returned HTTP {}", response.status())); }
                    stream_to_target(response, target, id, manager, progress_counter(manager, id)?, cancel, false, 0).await
                }
                Err(error) => Err(error),
            }
        } else {
            drop(probe);
            let (response, append) = if partial_len > 0 {
                let (response, append) = response_from_offset(client, url, partial_len, current_etag.as_deref(), current_modified.as_deref()).await?;
                if !append { partial_len = 0; }
                (response, append)
            } else {
                let response = client.get(url).send().await.map_err(|e| format!("Download request failed: {e}"))?;
                if !response.status().is_success() { return Err(format!("Server returned HTTP {}", response.status())); }
                (response, false)
            };
            stream_to_target(response, target, id, manager, progress_counter(manager, id)?, cancel, append, partial_len).await
        }
    } else if probe.status().is_success() {
        let total = probe.content_length();
        set_total(manager, id, total);
        let (response, append) = if partial_len > 0 {
            let (response, append) = response_from_offset(client, url, partial_len, current_etag.as_deref(), current_modified.as_deref()).await?;
            if !append { partial_len = 0; }
            (response, append)
        } else {
            (probe, false)
        };
        stream_to_target(response, target, id, manager, progress_counter(manager, id)?, cancel, append, partial_len).await
    } else {
        Err(format!("Server returned HTTP {} while checking range support.", probe.status()))
    }
}

async fn stream_to_target(
    response: Response, target: &Path, id: &str, _manager: &DownloadManager,
    progress: Arc<AtomicU64>, cancel: &CancellationToken, append: bool, initial_offset: u64,
) -> Result<(), String> {
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if append { options.append(true); } else { options.truncate(true); }
    let file = options.open(&temp).await.map_err(|e| format!("Cannot open partial output file: {e}"))?;
    let mut file = BufWriter::with_capacity(256 * 1024, file);
    progress.store(if append { initial_offset } else { 0 }, Ordering::Relaxed);
    let mut stream = response.bytes_stream();
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
                return Err(format!("Network stream interrupted: {error}"));
            }
        };
        if let Err(error) = file.write_all(&chunk).await {
            let _ = file.flush().await;
            let _ = file.get_ref().sync_data().await;
            return Err(format!("Disk write failed: {error}"));
        }
        add_progress(&progress, chunk.len() as u64);
        since_sync += chunk.len() as u64;
        if since_sync >= 4 * 1024 * 1024 {
            file.flush().await.map_err(|e| format!("Could not flush partial file: {e}"))?;
            file.get_ref().sync_data().await.map_err(|e| format!("Could not sync partial file: {e}"))?;
            since_sync = 0;
        }
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
    let mut output = BufWriter::with_capacity(256 * 1024, output);
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
    if parse_content_range_start(response.headers().get(header::CONTENT_RANGE)) != Some(offset) {
        return Err("Server returned an unexpected range for a saved segment.".into());
    }
    let file = OpenOptions::new().create(true).append(true).open(&path)
        .await.map_err(|e| format!("Cannot open segment file: {e}"))?;
    let mut file = BufWriter::with_capacity(256 * 1024, file);
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
        if since_sync >= 4 * 1024 * 1024 {
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
    destination: &Path, concurrency: usize, cookies_file: Option<&str>, cancel: &CancellationToken,
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

#[cfg(target_os = "ios")]
async fn run_media_download(
    _app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, _kind: &str,
    destination: &Path, _concurrency: usize, _cookies_file: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    if url.path().to_ascii_lowercase().ends_with(".m3u8") {
        return download_open_hls(url, destination, id, manager, cancel).await;
    }
    Err("This iOS build supports direct HTTP/HTTPS files and open HLS playlists. Generic third-party site extraction is not enabled in the App Store build; use a desktop build for supported yt-dlp extractors where you have download permission.".into())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn run_media_download(
    app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, kind: &str,
    destination: &Path, concurrency: usize, cookies_file: Option<&str>, cancel: &CancellationToken,
) -> Result<(), String> {
    let yt_dlp = find_tool(app, "yt-dlp").ok_or_else(||
        "yt-dlp was not found. Install yt-dlp or place it in the app's tools folder to download from media sites.".to_string())?;
    let has_ffmpeg = find_tool(app, "ffmpeg").is_some();
    let aria2 = find_tool(app, "aria2c");
    let mut command = Command::new(&yt_dlp);
    configure_tool_environment(&mut command, &yt_dlp);
    configure_no_console(&mut command);
    command.arg("--no-playlist").arg("--newline").arg("--progress").arg("--continue")
        .arg("--retries").arg("10").arg("--fragment-retries").arg("10")
        .arg("--file-access-retries").arg("3").arg("-N").arg(concurrency.to_string())
        .arg("--postprocessor-args").arg("ffmpeg:-threads 2")
        .arg("--progress-template").arg("download:__DOWNBEST__%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s")
        .arg("--print").arg("after_move:__DOWNBEST_FILE__%(filepath)s")
        .arg("-P").arg(destination).arg("-o").arg(format!("%(title).180B [%(id)s] - {}.%(ext)s", &id[..8]));
    match kind {
        "audio" => { command.arg("-f").arg("bestaudio/best"); }
        "video-only" => { command.arg("-f").arg("bestvideo"); }
        "video" if has_ffmpeg => {
            command.arg("-f").arg("bestvideo+bestaudio/best").arg("--merge-output-format").arg("mp4");
        }
        "video" => { command.arg("-f").arg("best[ext=mp4]/best"); }
        _ if has_ffmpeg => {
            command.arg("-f").arg("bestvideo+bestaudio/best").arg("--merge-output-format").arg("mp4");
        }
        _ => { command.arg("-f").arg("best[ext=mp4]/best"); }
    }
    if let Some(cookie_path) = cookies_file {
        command.arg("--cookies").arg(cookie_path);
    }
    if let Some(aria_path) = aria2 {
        // aria2c handles HTTP/FTP media transfers; yt-dlp's native downloader
        // handles DASH/HLS fragments so fragment scheduling remains protocol-aware.
        command.arg("--downloader").arg("aria2c")
            .arg("--downloader").arg("dash,m3u8:native")
            .arg("--downloader-args").arg(format!("aria2c:-c -x {} -s {} -k 2M", concurrency.clamp(2, 8), concurrency.clamp(2, 8)));
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
            if let Some(index) = line.find("__DOWNBEST_FILE__") {
                let value = line[index + "__DOWNBEST_FILE__".len()..].trim();
                if !value.is_empty() {
                    set_file_path(&stdout_manager, &stdout_id, Path::new(value));
                }
            }
        }
    });

    let progress_manager = manager.clone();
    let progress_id = id.to_string();
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut captured = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(index) = line.find("__DOWNBEST__") {
                let data = &line[index + "__DOWNBEST__".len()..];
                update_media_progress(&progress_manager, &progress_id, data);
            }
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
        let output = manager.records.lock().ok()
            .and_then(|records| records.get(id).and_then(|record| record.file_path.clone()));
        match output {
            Some(path) if Path::new(&path).is_file() => Ok(()),
            Some(path) => Err(format!("yt-dlp reported a result, but the output file was not found: {path}")),
            None => Err("yt-dlp finished but did not report the resulting file path. Check the selected save folder.".into()),
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
    use super::{download_http, effective_segment_count, parse_content_range_start, partial_bytes_for, persist_records, unique_path, DownloadManager, DownloadRecord};
    use reqwest::{header, Client};
    use std::{io::{Read, Write}, net::TcpListener, path::PathBuf, sync::{atomic::{AtomicU64, Ordering}, Arc}, time::Instant};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

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
            concurrency: 2,
            etag: None,
            last_modified: None,
            cookies_file: None,
        }
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
        let target = root.join("resume.bin");
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
            &client, &format!("http://{address}/asset.bin"), &target, &root,
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
            assert_eq!(restored.status, "Paused");
            assert_eq!(partial_bytes_for(restored), 8);
        }

        let resumed = runtime.block_on(download_http(
            &client, &format!("http://{address}/asset.bin"), &target, &root,
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
        assert_eq!(restored.status, "Paused");
        assert_eq!(partial_bytes_for(restored), 4096);
        assert_eq!(restored.downloaded.load(Ordering::Relaxed), 4096);
        drop(records);
        std::fs::remove_dir_all(folder).expect("remove recovery directory");
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
                app.state::<DownloadManager>().restore_queue(directory.join("queue.json"));
            }
            Ok(())
        });
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_media::init());
    builder
        .invoke_handler(tauri::generate_handler![
            start_download, resume_download, get_downloads, pause_download, clear_finished, check_engines, supports_folder_picker, reveal_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running DownBest");
}
