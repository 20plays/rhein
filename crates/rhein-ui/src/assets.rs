use std::borrow::Cow;

use anyhow::Result;
use gpui::{App, AssetSource, SharedString};

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

/// Serves the embedded fonts and icons to GPUI (`svg().path("icons/x.svg")`).
pub struct Assets;

impl Assets {
    pub fn load_fonts(cx: &App) -> Result<()> {
        let fonts = ASSETS
            .iter()
            .filter(|(path, _)| path.starts_with("fonts/"))
            .map(|(_, bytes)| Cow::Borrowed(*bytes))
            .collect();
        cx.text_system().add_fonts(fonts)
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ASSETS.iter().find(|(name, _)| *name == path) {
            Some((_, bytes)) => Ok(Some(Cow::Borrowed(*bytes))),
            None => {
                log::warn!("asset {path} is not embedded");
                Ok(None)
            }
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::new_static(name))
            .collect())
    }
}
