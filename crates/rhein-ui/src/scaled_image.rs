//! Pictures shrunk to the size they are drawn at. GPUI samples a texture down without mipmaps, so
//! a 512 px avatar drawn at 32 px comes out jagged and flickering; the browser filters it. Here
//! the decoded picture (GPUI's own cache entry, fetched once) is resized with a Lanczos filter to
//! the pixels it covers on screen, rounded up to a few sizes so the chat, the member list and the
//! cards share their copies. Animated pictures keep every frame and its delay.

use std::sync::Arc;

use gpui::{App, Asset, ImageCacheError, ImageSource, ImgResourceLoader, RenderImage, Resource, SharedUri, Window};
use image::imageops::FilterType;
use image::{DynamicImage, Frame, RgbaImage};

/// The pixel sizes a picture is shrunk to (the smallest that covers the drawn size).
const BUCKETS: [u32; 7] = [32, 48, 64, 96, 128, 192, 256];

pub struct ScaledImage;

impl Asset for ScaledImage {
    /// The picture and the side, in device pixels, of the square it fills.
    type Source = (Resource, u32);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load((resource, side): Self::Source, cx: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        let (full, _) = cx.fetch_asset::<ImgResourceLoader>(&resource);
        async move { Ok(shrink(full.await?, side)) }
    }
}

/// The bucket for a square of `side` device pixels.
pub fn bucket(side: f32) -> u32 {
    let side = side.ceil().max(1.) as u32;
    BUCKETS.iter().copied().find(|b| *b >= side).unwrap_or(side)
}

/// `full` cut to a centred square and resized to `side`, unless it is no larger already.
fn shrink(full: Arc<RenderImage>, side: u32) -> Arc<RenderImage> {
    let size = full.size(0);
    let (w, h) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
    if w.min(h) <= side || full.frame_count() == 0 {
        return full;
    }
    let mut frames = Vec::with_capacity(full.frame_count());
    for i in 0..full.frame_count() {
        let size = full.size(i);
        let (fw, fh) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
        let Some(pixels) = full.as_bytes(i).and_then(|b| RgbaImage::from_raw(fw, fh, b.to_vec())) else { return full };
        // The channels are in GPUI's BGRA order; resizing does not care which is which.
        let small = DynamicImage::ImageRgba8(pixels).resize_to_fill(side, side, FilterType::Lanczos3).into_rgba8();
        frames.push(Frame::from_parts(small, 0, 0, full.delay(i)));
    }
    Arc::new(RenderImage::new(frames))
}

/// An image source that draws `url` shrunk to a `drawn`-pixel square on this window.
pub fn scaled(url: impl Into<SharedUri>, drawn: gpui::Pixels, window: &Window) -> ImageSource {
    let resource = Resource::Uri(url.into());
    let side = bucket(f32::from(drawn) * window.scale_factor());
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| window.use_asset::<ScaledImage>(&(resource.clone(), side), cx)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn buckets_cover_the_drawn_size() {
        assert_eq!(super::bucket(20.), 32);
        assert_eq!(super::bucket(40.), 48);
        assert_eq!(super::bucket(64.), 64);
        assert_eq!(super::bucket(80. * 2.), 192);
        assert_eq!(super::bucket(300.), 300);
    }
}
