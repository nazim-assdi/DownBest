use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDownloadRequest {
    pub url: String,
    pub destination: String,
    pub kind: String,
    pub concurrency: usize,
    pub on_progress: Channel<MediaProgress>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProgress {
    pub progress: f64,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub eta_seconds: Option<u64>,
    pub line: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDownloadResult {
    pub file_path: String,
    pub file_name: String,
    pub format: String,
}
