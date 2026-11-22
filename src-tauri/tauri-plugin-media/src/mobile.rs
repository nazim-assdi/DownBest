use serde::de::DeserializeOwned;
use tauri::{
    AppHandle, Runtime,
    plugin::{PluginApi, PluginHandle},
};

use crate::{MediaCancelRequest, MediaCancelResult, MediaDownloadRequest, MediaDownloadResult};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.downbest.media";

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_downbest_media);

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    api: PluginApi<R, C>,
) -> crate::Result<Media<R>> {
    #[cfg(target_os = "android")]
    let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "MediaPlugin")?;
    #[cfg(target_os = "ios")]
    let handle = api.register_ios_plugin(init_plugin_downbest_media)?;
    Ok(Media(handle))
}

pub struct Media<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Media<R> {
    pub fn download(&self, payload: MediaDownloadRequest) -> crate::Result<MediaDownloadResult> {
        self.0.run_mobile_plugin("download", payload).map_err(Into::into)
    }

    #[cfg(target_os = "android")]
    pub fn cancel(&self, payload: MediaCancelRequest) -> crate::Result<MediaCancelResult> {
        self.0.run_mobile_plugin("cancelDownload", payload).map_err(Into::into)
    }
}
