#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(mobile)]
    #[error(transparent)]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
    #[error("The mobile media extractor is not available on this target.")]
    Unsupported,
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;
