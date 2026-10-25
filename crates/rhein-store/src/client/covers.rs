//! Local covers in shared activities. A player that offers its cover only as a file on this
//! machine (Sonora over MPRIS, for one) would show none to anybody else, so the cover is shrunk,
//! uploaded to the account's home server as short-lived profile media, and that copy is shared
//! for six hours, after which it is uploaded again if still playing. A local path never leaves
//! the device: until the copy is up, the activity goes out without a picture.

use std::collections::{HashMap, HashSet};
use std::io::Cursor;

use image::ImageFormat;
use image::imageops::FilterType;

use super::*;

/// The side covers are shrunk to (plenty for a 64 px card on a sharp screen).
const COVER_SIDE: u32 = 512;
/// How long an uploaded cover is shared before it is sent again (activity artwork is
/// short-lived profile media).
const COVER_LIFE_MS: i64 = 6 * 60 * 60 * 1000;

/// What is known about local covers: uploaded copies, and the ones being uploaded or failed.
#[derive(Default)]
pub struct Covers {
    /// Local path to the shared copy's address and when it was uploaded.
    uploaded: HashMap<String, (String, i64)>,
    pending: HashSet<String>,
}

fn stored_path() -> std::path::PathBuf {
    crate::persist::cache_dir().join("covers.json")
}

impl Covers {
    pub fn load() -> Covers {
        let uploaded = std::fs::read(stored_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Covers { uploaded, pending: HashSet::new() }
    }

    fn store(&self) {
        let path = stored_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec(&self.uploaded) {
            let _ = std::fs::write(path, bytes);
        }
    }
}

/// A cover file made small and plain: a JPEG no larger than `COVER_SIDE`, without the file's
/// own metadata.
fn shrink(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let picture = image::load_from_memory(bytes)?;
    let small = if picture.width().max(picture.height()) > COVER_SIDE { picture.resize(COVER_SIDE, COVER_SIDE, FilterType::Lanczos3) } else { picture };
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(small.to_rgb8()).write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)?;
    Ok(out)
}

impl Client {
    /// The activities with every local cover replaced by its uploaded copy, or dropped while
    /// there is none yet (its upload is started here).
    pub(super) fn with_shared_covers(&mut self, activities: Vec<rhein_proto::Activity>, cx: &mut Context<Self>) -> Vec<rhein_proto::Activity> {
        activities
            .into_iter()
            .map(|mut activity| {
                if let Some(assets) = activity.assets.as_mut() {
                    for image in [&mut assets.large_image, &mut assets.small_image] {
                        let Some(local) = image.as_deref().and_then(|u| u.strip_prefix("file://")).map(str::to_owned) else { continue };
                        *image = self.covers.uploaded.get(&local).filter(|(_, at)| now_ms() - at < COVER_LIFE_MS).map(|(url, _)| url.clone());
                        if image.is_none() {
                            self.upload_cover(local, cx);
                        }
                    }
                }
                activity
            })
            .collect()
    }

    fn upload_cover(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.covers.pending.insert(path.clone()) {
            return;
        }
        let Some(api) = self.home.as_ref().map(|h| h.api.clone()) else { return };
        let io = self.io.clone();
        let file = path.clone();
        cx.spawn(async move |this, cx| {
            let uploaded = join(io.spawn(async move {
                let decoded = urlencoding::decode(&file).map(|p| p.into_owned()).unwrap_or(file);
                let bytes = tokio::fs::read(&decoded).await?;
                let small = tokio::task::spawn_blocking(move || shrink(&bytes)).await??;
                let media = api.upload_profile_media("cover.jpg", "image/jpeg", small).await?;
                anyhow::Ok(format!("{}?cap={}", media.hash, media.capability.unwrap_or_default()))
            }))
            .await;
            this.update(cx, |this, cx| match uploaded {
                Ok(reference) => {
                    let Some(url) = this.home_media_url(&reference) else { return };
                    this.covers.pending.remove(&path);
                    this.covers.uploaded.insert(path, (url, now_ms()));
                    this.covers.store();
                    // Shared again, now with the picture.
                    this.share_local(cx);
                }
                // Left pending, so a cover that can't be read or sent isn't tried on every poll.
                Err(err) => log::info!("cover {path}: {err:#}"),
            })
            .ok();
        })
        .detach();
    }
}
