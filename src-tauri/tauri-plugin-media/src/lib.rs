use tauri::{
    Manager, Runtime, State,
    plugin::{Builder, TauriPlugin},
};

mod error;
mod models;
#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

#[cfg(desktop)]
use desktop::Media;
#[cfg(mobile)]
use mobile::Media;

pub use error::*;
pub use models::*;

pub trait MediaExt<R: Runtime> {
    fn media(&self) -> State<'_, Media<R>>;
}

impl<R: Runtime, T: Manager<R>> MediaExt<R> for T {
    fn media(&self) -> State<'_, Media<R>> {
        self.state::<Media<R>>()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("media")
        .setup(|app, api| {
            #[cfg(mobile)]
            let media = mobile::init(app, api)?;
            #[cfg(desktop)]
            let media = desktop::init(app, api)?;
            app.manage(media);
            Ok(())
        })
        .build()
}
