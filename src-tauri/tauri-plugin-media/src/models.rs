use serde::{Deserialize, Serialize};
use tauri::ipc::Channel;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaDownloadRequest {
    pub url: String,
    pub destination: String,
    pub job_id: String,
    pub kind: String,
    pub concurrency: usize,
    pub cookies_file: Option<String>,
    pub format_selector: Option<String>,
    pub on_progress: Channel<MediaProgress>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCancelRequest {
    pub job_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaCancelResult {
    pub cancelled: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProgress {
    pub progress: f64,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
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


#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPublishRequest {
    pub file_path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaPublishResult {
    pub content_uri: String,
    pub file_name: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaOpenRequest {
    pub content_uri: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaOpenResult {
    pub opened: bool,
}


#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInspectRequest {
    pub url: String,
    pub flat_playlist: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInspectResult {
    pub output: String,
    pub error_output: String,
    pub exit_code: i32,
}
