use futures_util::{future::try_join_all, StreamExt};
use reqwest::{header, Client, Response, StatusCode};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex},
    time::Instant,
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::Semaphore,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

#[derive(Clone)]
struct DownloadManager {
    records: Arc<Mutex<HashMap<String, DownloadRecord>>>,
    slots: Arc<Semaphore>,
}
impl Default for DownloadManager {
    fn default() -> Self {
        Self { records: Arc::new(Mutex::new(HashMap::new())), slots: Arc::new(Semaphore::new(3)) }
    }
}

struct DownloadRecord {
    id: String, url: String, file_name: String, kind: String, status: String,
    destination: String, downloaded: Arc<AtomicU64>, total: Option<u64>,
    started: Instant, error: Option<String>, cancel: CancellationToken,
}

#[derive(Serialize, Clone)]
struct DownloadView {
    id: String, url: String, file_name: String, kind: String, status: String,
    destination: String, downloaded_bytes: u64, total_bytes: Option<u64>,
    speed_bytes_sec: u64, progress: f64, error: Option<String>,
}
#[derive(Serialize)]
struct ToolStatus { name: String, available: bool, version: Option<String> }

fn view_of(record: &DownloadRecord) -> DownloadView {
    let downloaded = record.downloaded.load(Ordering::Relaxed);
    let progress = record.total.filter(|n| *n > 0)
        .map(|n| (downloaded as f64 / n as f64 * 100.0).clamp(0.0, 100.0)).unwrap_or(0.0);
    let elapsed = record.started.elapsed().as_secs_f64().max(0.5);
    DownloadView {
        id: record.id.clone(), url: record.url.clone(), file_name: record.file_name.clone(),
        kind: record.kind.clone(), status: record.status.clone(), destination: record.destination.clone(),
        downloaded_bytes: downloaded, total_bytes: record.total,
        speed_bytes_sec: (downloaded as f64 / elapsed) as u64, progress, error: record.error.clone(),
    }
}
fn set_status(manager: &DownloadManager, id: &str, status: &str, error: Option<String>) {
    if let Ok(mut records) = manager.records.lock() {
        if let Some(record) = records.get_mut(id) { record.status = status.to_string(); record.error = error; }
    }
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
fn add_progress(manager: &DownloadManager, id: &str, amount: u64) {
    if let Ok(records) = manager.records.lock() {
        if let Some(record) = records.get(id) { record.downloaded.fetch_add(amount, Ordering::Relaxed); }
    }
}
fn reset_progress(manager: &DownloadManager, id: &str) {
    if let Ok(records) = manager.records.lock() {
        if let Some(record) = records.get(id) { record.downloaded.store(0, Ordering::Relaxed); }
    }
}

#[tauri::command]
async fn start_download(
    app: AppHandle, manager: State<'_, DownloadManager>, url: String, kind: String,
    destination: Option<String>, concurrency: Option<usize>,
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
    let cancel = CancellationToken::new();
    let record = DownloadRecord {
        id: id.clone(), url: url.clone(),
        file_name: suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8])),
        kind: kind.clone(), status: "Queued".into(), destination: destination_path.display().to_string(),
        downloaded: Arc::new(AtomicU64::new(0)), total: None, started: Instant::now(),
        error: None, cancel: cancel.clone(),
    };
    let initial = view_of(&record);
    let manager = manager.inner().clone();
    manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?.insert(id.clone(), record);

    let task_manager = manager.clone();
    let task_id = id.clone();
    let requested = concurrency.unwrap_or(8).clamp(1, 32);
    tauri::async_runtime::spawn(async move {
        let result = execute_download(app, task_manager.clone(), task_id.clone(), url, kind,
            destination_path, requested, cancel).await;
        match result {
            Ok(()) => set_status(&task_manager, &task_id, "Completed", None),
            Err(error) if error == "Cancelled" => set_status(&task_manager, &task_id, "Cancelled", None),
            Err(error) => set_status(&task_manager, &task_id, "Failed", Some(error)),
        }
    });
    Ok(initial)
}

#[tauri::command]
fn get_downloads(manager: State<'_, DownloadManager>) -> Result<Vec<DownloadView>, String> {
    let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    let mut result: Vec<DownloadView> = records.values().map(view_of).collect();
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}
#[tauri::command]
fn cancel_download(manager: State<'_, DownloadManager>, id: String) -> Result<(), String> {
    let records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    let record = records.get(&id).ok_or_else(|| "Download was not found.".to_string())?;
    if !matches!(record.status.as_str(), "Completed" | "Failed" | "Cancelled") { record.cancel.cancel(); }
    Ok(())
}
#[tauri::command]
fn clear_finished(manager: State<'_, DownloadManager>) -> Result<(), String> {
    let mut records = manager.records.lock().map_err(|_| "Download queue is unavailable.".to_string())?;
    records.retain(|_, item| !matches!(item.status.as_str(), "Completed" | "Failed" | "Cancelled"));
    Ok(())
}
#[tauri::command]
async fn check_engines(app: AppHandle) -> Vec<ToolStatus> {
    let mut result = Vec::new();
    for name in ["yt-dlp", "aria2c", "ffmpeg"] {
        if let Some(path) = find_tool(&app, name) {
            let output = Command::new(path).arg("--version").output().await.ok();
            let version = output.map(|o| String::from_utf8_lossy(&o.stdout).lines().next()
                .unwrap_or("Available").trim().to_string()).filter(|v| !v.is_empty());
            result.push(ToolStatus { name: name.into(), available: true, version });
        } else {
            result.push(ToolStatus { name: name.into(), available: false, version: None });
        }
    }
    result
}

async fn execute_download(
    app: AppHandle, manager: DownloadManager, id: String, url: String, kind: String,
    destination: PathBuf, concurrency: usize, cancel: CancellationToken,
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
        .pool_idle_timeout(std::time::Duration::from_secs(90)).pool_max_idle_per_host(32)
        .default_headers({
            let mut headers = header::HeaderMap::new();
            headers.insert(header::ACCEPT_ENCODING, header::HeaderValue::from_static("identity"));
            headers
        })
        .build().map_err(|e| format!("Could not initialize network client: {e}"))?;

    // Known media domains/manifests go straight to yt-dlp. For other URLs,
    // inspect the response type so generic yt-dlp-supported web pages also work.
    let media_page = needs_media_extractor(&parsed) || looks_like_html_page(&client, &url).await;
    if media_page {
        set_status(&manager, &id, "Extracting media", None);
        return run_media_download(&app, &manager, &id, &parsed, &kind, &destination, concurrency, &cancel).await;
    }
    let name = suggested_name(&parsed).unwrap_or_else(|| format!("download-{}", &id[..8]));
    let target = unique_path(&destination, &name);
    set_file_name(&manager, &id, target.file_name().and_then(|s| s.to_str()).unwrap_or(&name));
    set_status(&manager, &id, "Downloading", None);
    download_http(&client, &url, &target, &destination, &id, concurrency, &manager, &cancel).await
}

async fn looks_like_html_page(client: &Client, url: &str) -> bool {
    let head = client.head(url).send().await;
    if let Ok(response) = head {
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
    match client.get(url).header(header::RANGE, "bytes=0-0").send().await {
        Ok(response) if response.status().is_success() || response.status() == StatusCode::PARTIAL_CONTENT =>
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
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let original = Path::new(name);
    let stem = original.file_stem().and_then(|s| s.to_str()).unwrap_or("download");
    let extension = original.extension().and_then(|s| s.to_str());
    for index in 0..10_000 {
        let candidate = if index == 0 { name.to_string() }
            else if let Some(ext) = extension { format!("{stem} ({index}).{ext}") }
            else { format!("{stem} ({index})") };
        let path = dir.join(candidate);
        if !path.exists() { return path; }
    }
    dir.join(format!("{}-{}", Uuid::new_v4(), name))
}
fn parse_content_range(value: Option<&header::HeaderValue>) -> Option<u64> {
    let raw = value?.to_str().ok()?;
    let (_, total) = raw.split_once('/')?;
    if total == "*" { None } else { total.parse().ok() }
}

async fn download_http(
    client: &Client, url: &str, target: &Path, destination: &Path, id: &str,
    concurrency: usize, manager: &DownloadManager, cancel: &CancellationToken,
) -> Result<(), String> {
    let probe = client.get(url).header(header::RANGE, "bytes=0-0").send().await
        .map_err(|e| format!("Connection failed: {e}"))?;
    if probe.status() == StatusCode::PARTIAL_CONTENT {
        let total = parse_content_range(probe.headers().get(header::CONTENT_RANGE))
            .ok_or_else(|| "The server returned an invalid Content-Range header.".to_string())?;
        set_total(manager, id, Some(total));
        let count = concurrency.min(((total / (2 * 1024 * 1024)).max(1)) as usize).min(32);
        if total >= 4 * 1024 * 1024 && count > 1 {
            drop(probe);
            match download_segmented(client, url, target, destination, id, total, count, manager, cancel).await {
                Ok(()) => Ok(()),
                Err(error) if error == "Cancelled" => Err(error),
                Err(_range_error) => {
                    // Some origins advertise byte ranges but reject parallel requests.
                    // Retry once with a normal single connection instead of failing the item.
                    reset_progress(manager, id);
                    let response = client.get(url).send().await
                        .map_err(|e| format!("Parallel download failed and fallback connection failed: {e}"))?;
                    if !response.status().is_success() {
                        return Err(format!("Parallel download failed and fallback returned HTTP {}", response.status()));
                    }
                    stream_to_target(response, target, id, manager, cancel).await
                }
            }
        } else {
            drop(probe);
            let response = client.get(url).send().await.map_err(|e| format!("Download request failed: {e}"))?;
            if !response.status().is_success() { return Err(format!("Server returned HTTP {}", response.status())); }
            stream_to_target(response, target, id, manager, cancel).await
        }
    } else if probe.status().is_success() {
        let total = probe.content_length();
        set_total(manager, id, total);
        stream_to_target(probe, target, id, manager, cancel).await
    } else {
        Err(format!("Server returned HTTP {} while checking range support.", probe.status()))
    }
}

async fn stream_to_target(
    response: Response, target: &Path, id: &str, manager: &DownloadManager,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let mut file = File::create(&temp).await.map_err(|e| format!("Cannot create output file: {e}"))?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => return Err("Cancelled".into()),
        item = stream.next() => item,
    } {
        let chunk = chunk.map_err(|e| format!("Network stream interrupted: {e}"))?;
        file.write_all(&chunk).await.map_err(|e| format!("Disk write failed: {e}"))?;
        add_progress(manager, id, chunk.len() as u64);
    }
    file.flush().await.map_err(|e| format!("Could not flush file: {e}"))?;
    file.sync_all().await.map_err(|e| format!("Could not sync file: {e}"))?;
    drop(file);
    fs::rename(&temp, target).await.map_err(|e| format!("Could not finalize download: {e}"))?;
    Ok(())
}

async fn download_segmented(
    client: &Client, url: &str, target: &Path, destination: &Path, id: &str,
    total: u64, count: usize, manager: &DownloadManager, cancel: &CancellationToken,
) -> Result<(), String> {
    let parts_dir = destination.join(format!(".downbest-{id}"));
    fs::create_dir_all(&parts_dir).await.map_err(|e| format!("Cannot create temporary folder: {e}"))?;
    let chunk_size = (total + count as u64 - 1) / count as u64;
    let mut jobs = Vec::new();
    for index in 0..count {
        let start = index as u64 * chunk_size;
        if start >= total { break; }
        let end = (start + chunk_size - 1).min(total - 1);
        let client = client.clone();
        let url = url.to_string();
        let part_path = parts_dir.join(format!("part-{index:03}"));
        let manager = manager.clone();
        let id = id.to_string();
        let cancel = cancel.clone();
        jobs.push(async move {
            download_piece(client, url, start, end, part_path.clone(), &id, manager, cancel).await?;
            Ok::<PathBuf, String>(part_path)
        });
    }
    let result = try_join_all(jobs).await;
    let parts = match result {
        Ok(parts) => parts,
        Err(error) => { let _ = fs::remove_dir_all(&parts_dir).await; return Err(error); }
    };
    if cancel.is_cancelled() { let _ = fs::remove_dir_all(&parts_dir).await; return Err("Cancelled".into()); }
    let temp = PathBuf::from(format!("{}.downbest-{}.tmp", target.display(), id));
    let mut output = File::create(&temp).await.map_err(|e| format!("Cannot create output file: {e}"))?;
    for part_path in parts {
        let mut part = File::open(&part_path).await.map_err(|e| format!("A download segment is missing: {e}"))?;
        tokio::io::copy(&mut part, &mut output).await.map_err(|e| format!("Could not assemble download: {e}"))?;
    }
    output.flush().await.map_err(|e| format!("Could not flush output: {e}"))?;
    output.sync_all().await.map_err(|e| format!("Could not sync output: {e}"))?;
    drop(output);
    fs::rename(&temp, target).await.map_err(|e| format!("Could not finalize download: {e}"))?;
    fs::remove_dir_all(&parts_dir).await.ok();
    Ok(())
}

async fn download_piece(
    client: Client, url: String, start: u64, end: u64, path: PathBuf, id: &str,
    manager: DownloadManager, cancel: CancellationToken,
) -> Result<(), String> {
    if cancel.is_cancelled() { return Err("Cancelled".into()); }
    let response = client.get(&url).header(header::RANGE, format!("bytes={start}-{end}"))
        .send().await.map_err(|e| format!("Segment connection failed: {e}"))?;
    if response.status() != StatusCode::PARTIAL_CONTENT {
        return Err(format!("Server stopped supporting parallel ranges (HTTP {}). Try a lower speed profile.", response.status()));
    }
    let mut file = OpenOptions::new().create(true).truncate(true).write(true).open(&path)
        .await.map_err(|e| format!("Cannot create segment file: {e}"))?;
    let mut stream = response.bytes_stream();
    let mut received = 0u64;
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => return Err("Cancelled".into()),
        item = stream.next() => item,
    } {
        let chunk = chunk.map_err(|e| format!("Segment interrupted: {e}"))?;
        received += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(|e| format!("Segment disk write failed: {e}"))?;
        add_progress(&manager, id, chunk.len() as u64);
    }
    file.flush().await.map_err(|e| format!("Cannot flush segment: {e}"))?;
    let expected = end - start + 1;
    if received != expected { return Err(format!("Segment size mismatch: expected {expected} bytes, received {received}.")); }
    Ok(())
}

#[cfg(any(target_os = "android", target_os = "ios"))]
async fn run_media_download(
    _app: &AppHandle, _manager: &DownloadManager, _id: &str, _url: &Url, _kind: &str,
    _destination: &Path, _concurrency: usize, _cancel: &CancellationToken,
) -> Result<(), String> {
    Err("Media-site extraction (YouTube and similar) needs yt-dlp and is currently desktop-only. Direct HTTP/HTTPS file links work on mobile.".into())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn run_media_download(
    app: &AppHandle, manager: &DownloadManager, id: &str, url: &Url, kind: &str,
    destination: &Path, concurrency: usize, cancel: &CancellationToken,
) -> Result<(), String> {
    let yt_dlp = find_tool(app, "yt-dlp").ok_or_else(||
        "yt-dlp was not found. Install yt-dlp or place it in the app's tools folder to download from media sites.".to_string())?;
    let has_ffmpeg = find_tool(app, "ffmpeg").is_some();
    let aria2 = find_tool(app, "aria2c");
    let mut command = Command::new(&yt_dlp);
    command.arg("--no-playlist").arg("--newline").arg("--progress").arg("--continue")
        .arg("--retries").arg("10").arg("--fragment-retries").arg("10")
        .arg("--file-access-retries").arg("3").arg("-N").arg(concurrency.to_string())
        .arg("--progress-template").arg("download:__DOWNBEST__%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.speed)s")
        .arg("-P").arg(destination).arg("-o").arg("%(title).180B [%(id)s].%(ext)s");
    match kind {
        "audio" => { command.arg("-f").arg("bestaudio/best"); }
        "video-only" => { command.arg("-f").arg("bestvideo/best"); }
        "video" if has_ffmpeg => {
            command.arg("-f").arg("bestvideo+bestaudio/best").arg("--merge-output-format").arg("mp4");
        }
        "video" => { command.arg("-f").arg("best[ext=mp4]/best"); }
        _ if has_ffmpeg => {
            command.arg("-f").arg("bestvideo+bestaudio/best").arg("--merge-output-format").arg("mp4");
        }
        _ => { command.arg("-f").arg("best[ext=mp4]/best"); }
    }
    if let Some(aria_path) = aria2 {
        // aria2c handles HTTP/FTP media transfers; yt-dlp's native downloader
        // handles DASH/HLS fragments so fragment scheduling remains protocol-aware.
        command.arg("--downloader").arg("aria2c")
            .arg("--downloader").arg("dash,m3u8:native")
            .arg("--downloader-args").arg("aria2c:-c -x 16 -s 16 -k 1M");
        if let Some(parent) = aria_path.parent() {
            let mut paths = vec![parent.to_path_buf()];
            paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
            if let Ok(joined) = std::env::join_paths(paths) { command.env("PATH", joined); }
        }
    }
    command.arg(url.as_str()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("Could not start yt-dlp: {e}"))?;
    let stdout = child.stdout.take().ok_or_else(|| "Could not read yt-dlp output.".to_string())?;
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(mut stream) = stderr { let _ = stream.read_to_string(&mut text).await; }
        text
    });
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = tokio::select! {
            _ = cancel.cancelled() => { let _ = child.kill().await; return Err("Cancelled".into()); }
            result = lines.next_line() => result.map_err(|e| format!("Could not read yt-dlp progress: {e}"))?,
        };
        let Some(line) = line else { break };
        if let Some(data) = line.strip_prefix("__DOWNBEST__") {
            let fields: Vec<&str> = data.split('|').collect();
            if let Some(downloaded) = fields.first().and_then(|v| v.parse::<u64>().ok()) {
                if let Ok(records) = manager.records.lock() {
                    if let Some(record) = records.get(id) { record.downloaded.store(downloaded, Ordering::Relaxed); }
                }
            }
            if let Some(total) = fields.get(1).and_then(|v| v.parse::<u64>().ok()) { set_total(manager, id, Some(total)); }
        }
    }
    let status = child.wait().await.map_err(|e| format!("yt-dlp process failed: {e}"))?;
    let stderr_text = stderr_task.await.unwrap_or_default();
    if status.success() { Ok(()) } else {
        let tail: String = stderr_text.chars().rev().take(900).collect::<String>().chars().rev().collect();
        Err(if tail.trim().is_empty() { format!("yt-dlp exited with status {status}.") } else { format!("yt-dlp: {tail}") })
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
    for path in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        candidates.push(path.join(&executable));
    }
    candidates.into_iter().find(|path| path.is_file())
}

pub fn run() {
    tauri::Builder::default()
        .manage(DownloadManager::default())
        .invoke_handler(tauri::generate_handler![
            start_download, get_downloads, cancel_download, clear_finished, check_engines
        ])
        .run(tauri::generate_context!())
        .expect("error while running DownBest");
}
