//! Saving a picture from the chat to the downloads folder, or copying it to the clipboard. Both
//! fetch the file itself (not GPUI's decoded copy), so a GIF keeps its animation.

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, ensure};
use gpui::http_client::AsyncBody;
use gpui::{App, AppContext as _, ClipboardItem, Entity, Image, ImageFormat};
use rhein_store::Client;

/// A picture in the chat: its link, and the name it is saved under (a file name, or a link whose
/// last part is used).
#[derive(Clone)]
pub struct Media {
    pub url: String,
    pub name: String,
}

impl Media {
    pub fn new(url: impl Into<String>, name: impl Into<String>) -> Self {
        Media { url: url.into(), name: name.into() }
    }
}

/// Saves `media` into the downloads folder, under a name not taken yet.
pub fn download(client: &Entity<Client>, media: Media, cx: &mut App) {
    let client = client.clone();
    let fetch = fetch(&media.url, cx);
    cx.spawn(async move |cx| {
        let result = async {
            let bytes = fetch.await?;
            let path = free_path(&file_name(&media, &bytes));
            std::fs::write(&path, &bytes).with_context(|| format!("couldn't write {}", path.display()))?;
            Ok::<_, anyhow::Error>(path)
        }
        .await;
        client
            .update(cx, |c, cx| match result {
                Ok(path) => c.flash_notice(format!("Saved to {}", path.display()), cx),
                Err(err) => {
                    c.error = Some(format!("Couldn't download the picture: {err:#}").into());
                    cx.notify();
                }
            });
    })
    .detach();
}

/// Puts the picture itself on the clipboard.
pub fn copy(client: &Entity<Client>, url: String, cx: &mut App) {
    let client = client.clone();
    let fetch = fetch(&url, cx);
    cx.spawn(async move |cx| {
        let result = async {
            let bytes = fetch.await?;
            let format = format_of(&bytes).ok_or_else(|| anyhow!("that isn't a picture"))?;
            Ok::<_, anyhow::Error>(Image::from_bytes(format, bytes))
        }
        .await;
        cx.update(|cx| match result {
            Ok(image) => {
                cx.write_to_clipboard(ClipboardItem::new_image(&image));
                client.update(cx, |c, cx| c.flash_notice("Picture copied", cx));
            }
            Err(err) => client.update(cx, |c, cx| {
                c.error = Some(format!("Couldn't copy the picture: {err:#}").into());
                cx.notify();
            }),
        });
    })
    .detach();
}

fn fetch(url: &str, cx: &App) -> gpui::Task<Result<Vec<u8>>> {
    let http = cx.http_client();
    let url = url.to_owned();
    cx.background_spawn(async move {
        let response = http.get(&url, AsyncBody::empty(), true).await?;
        ensure!(response.status().is_success(), "the server answered {}", response.status());
        crate::http::read(response.into_body())?.ok_or_else(|| anyhow!("the server sent nothing"))
    })
}

fn format_of(bytes: &[u8]) -> Option<ImageFormat> {
    Some(match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::WebP => ImageFormat::Webp,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        image::ImageFormat::Tiff => ImageFormat::Tiff,
        _ => return None,
    })
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Svg => "svg",
        _ => "img",
    }
}

/// The name to save under: the file name, or the last part of the link without its query, with
/// the extension the bytes call for when it has none.
fn file_name(media: &Media, bytes: &[u8]) -> String {
    let name = media.name.split(['?', '#']).next().unwrap_or_default();
    let name = name.trim_end_matches('/').rsplit('/').next().unwrap_or_default();
    let name = urlencoding::decode(name).map(|n| n.into_owned()).unwrap_or_else(|_| name.to_owned());
    let mut name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
    if name.trim().is_empty() || name.starts_with('.') {
        name = format!("picture{name}");
    }
    let has_extension = name.rsplit_once('.').is_some_and(|(stem, ext)| !stem.is_empty() && (1..=5).contains(&ext.len()));
    if !has_extension && let Some(format) = format_of(bytes) {
        name = format!("{name}.{}", extension(format));
    }
    name
}

/// `name` in the downloads folder, numbered when that file exists already.
fn free_path(name: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
    let mut path = dir.join(name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("{n}-{name}"));
        n += 1;
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    #[test]
    fn names_come_from_the_link() {
        let media = Media::new("", "https://media.tenor.com/abc/cat%20dance.gif?x=1");
        assert_eq!(file_name(&media, b""), "cat dance.gif");
        let media = Media::new("", "https://cdn.example/pictures/1234");
        assert_eq!(file_name(&media, PNG), "1234.png");
        let media = Media::new("", "holiday.jpeg");
        assert_eq!(file_name(&media, PNG), "holiday.jpeg");
    }
}
