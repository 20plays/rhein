//! Posters of videos in messages: the first picture and the shape, made from as little of the
//! file as there is to read (its index and that picture) and kept on disk, so a video shows at
//! its size with a picture before it is played, and again at once after a chat switch or a
//! restart. Their own entity: a poster arriving redraws only the rows showing videos.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rhein_voice::buffered::Buffered;

use super::*;

/// Posters are at most this wide (the chat shows videos up to 560 px).
const POSTER_WIDTH: u32 = 640;

/// Posters made at once: each fetches the start of a file.
const AT_ONCE: usize = 3;

/// An encrypted video is opened whole for its poster only up to this size.
const DM_POSTER_MAX: i64 = 8 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Poster {
    Loading,
    /// The first picture, on disk, and the video's shape.
    Ready { path: Arc<Path>, size: (u32, u32) },
    /// Only the shape: there is no picture we can decode (not H.264).
    Shape((u32, u32)),
    Failed,
}

impl Poster {
    pub fn size(&self) -> Option<(u32, u32)> {
        match self {
            Poster::Ready { size, .. } | Poster::Shape(size) => Some(*size),
            _ => None,
        }
    }
}

pub struct MoviePosters {
    posters: HashMap<String, Poster>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Default for MoviePosters {
    fn default() -> Self {
        Self { posters: HashMap::new(), slots: Arc::new(tokio::sync::Semaphore::new(AT_ONCE)) }
    }
}

impl MoviePosters {
    /// The poster of the video `key` (message id and attachment index), once asked for.
    pub fn get(&self, key: &str) -> Option<&Poster> {
        self.posters.get(key)
    }
}

/// Where the poster of a file with content `hash` is kept.
fn poster_path(hash: &str) -> PathBuf {
    let name: String = hash.chars().filter(char::is_ascii_alphanumeric).take(64).collect();
    crate::persist::cache_dir().join("posters").join(format!("{name}.jpg"))
}

/// Decodes the first picture of `bytes` into a JPEG at `path`; its size, or only the video's
/// shape when the picture cannot be decoded.
fn make_poster(bytes: &Arc<Buffered>, path: &Path) -> Result<Poster> {
    let poster = match rhein_voice::movie::poster(bytes, POSTER_WIDTH, || false) {
        Ok(poster) => poster,
        Err(err) => {
            log::debug!("video poster: {err:#}");
            return Ok(Poster::Shape(rhein_voice::movie::shape(bytes, || false)?));
        }
    };
    let picture = poster.picture;
    let rgb: Vec<u8> = picture.bgra.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0]]).collect();
    let image = image::RgbImage::from_raw(picture.width, picture.height, rgb).ok_or_else(|| anyhow!("the poster has no picture"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = path.with_extension("part");
    image.save_with_format(&part, image::ImageFormat::Jpeg)?;
    std::fs::rename(&part, path)?;
    Ok(Poster::Ready { path: path.into(), size: (picture.width, picture.height) })
}

impl Client {
    /// Makes the poster of the video `key`, a file of `size` bytes whose content `hash` names it
    /// on disk, unless it is made or under way. Nothing is redrawn until it is there.
    pub fn request_poster(&mut self, key: String, hash: &str, size: i64, source: MediaSource, cx: &mut Context<Self>) {
        let posters = self.movie_posters.clone();
        if posters.read(cx).posters.contains_key(&key) {
            return;
        }
        let dm = matches!(source, MediaSource::Dm { .. });
        let path = poster_path(if hash.is_empty() { &key } else { hash });
        let slots = posters.read(cx).slots.clone();
        posters.update(cx, |p, _| p.posters.insert(key.clone(), Poster::Loading));
        let (io, fetcher, player) = (self.io.clone(), self.fetcher(), self.movie_player.clone());
        cx.spawn(async move |_, cx| {
            let kept = {
                let path = path.clone();
                join(io.spawn(async move { Ok(tokio::task::spawn_blocking(move || image::image_dimensions(&path).ok()).await?) })).await.ok().flatten()
            };
            let poster = match kept {
                Some(size) => Poster::Ready { path: path.into(), size },
                None if dm && size > DM_POSTER_MAX => Poster::Failed,
                None => {
                    let _slot = slots.acquire_owned().await;
                    let bytes = player.update(cx, |p, _| p.bytes(&key, source, Some(0), &io, fetcher));
                    let made = join(io.spawn(async move { tokio::task::spawn_blocking(move || make_poster(&bytes, &path)).await? })).await;
                    made.unwrap_or_else(|err| {
                        log::debug!("video poster for {key}: {err:#}");
                        Poster::Failed
                    })
                }
            };
            posters.update(cx, |p, cx| {
                p.posters.insert(key, poster);
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_poster_is_kept_as_a_picture_of_the_videos_shape() {
        let video = include_bytes!("../../../rhein-voice/testdata/streamed-faststart.mp4");
        let dir = std::env::temp_dir().join(format!("rhein-poster-{}", std::process::id()));
        let path = dir.join("poster.jpg");
        let made = make_poster(&Arc::new(Buffered::full(video.to_vec())), &path).unwrap();
        assert_eq!(made, Poster::Ready { path: path.clone().into(), size: (96, 64) });
        assert_eq!(image::image_dimensions(&path).unwrap(), (96, 64));
        let hevc = include_bytes!("../../../rhein-voice/testdata/hevc.mp4");
        assert!(matches!(make_poster(&Arc::new(Buffered::full(hevc.to_vec())), &dir.join("hevc.jpg")).unwrap(), Poster::Shape(_)));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
