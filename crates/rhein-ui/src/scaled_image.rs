//! Pictures shrunk to the size they are drawn at. GPUI samples a texture down without mipmaps, so
//! a 512 px avatar drawn at 32 px comes out jagged and flickering; the browser filters it. Here
//! the decoded picture (GPUI's own cache entry, fetched once) is resized with a Lanczos filter to
//! the pixels it covers on screen, rounded up to a few sizes so the chat, the member list and the
//! cards share their copies. Animated pictures keep every frame and its delay. [`fit`] does the
//! same for a whole picture that has to fit a box, keeping its shape.

use std::sync::Arc;

use gpui::{App, Asset, ImageCacheError, ImageSource, ImgResourceLoader, Pixels, RenderImage, Resource, SharedUri, Size, Window, px};
use image::imageops::FilterType;
use image::{DynamicImage, Frame, RgbaImage};

/// The pixel sizes a picture is shrunk to (the smallest that covers the drawn size).
const BUCKETS: [u32; 7] = [32, 48, 64, 96, 128, 192, 256];

pub struct ScaledImage;

impl Asset for ScaledImage {
    /// The picture and the width and height, in device pixels, of the box it fills.
    type Source = (Resource, u32, u32);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load((resource, width, height): Self::Source, cx: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        let (full, _) = cx.fetch_asset::<ImgResourceLoader>(&resource);
        async move { Ok(shrink(full.await?, width, height)) }
    }
}

/// The bucket for a square of `side` device pixels.
pub fn bucket(side: f32) -> u32 {
    let side = side.ceil().max(1.) as u32;
    BUCKETS.iter().copied().find(|b| *b >= side).unwrap_or(side)
}

/// `full` cut to the shape of the box (centred) and resized to fill it, unless it is no larger
/// already. A box of the picture's own shape keeps all of it.
fn shrink(full: Arc<RenderImage>, width: u32, height: u32) -> Arc<RenderImage> {
    let size = full.size(0);
    let (w, h) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
    if w <= width || h <= height || full.frame_count() == 0 {
        return full;
    }
    let mut frames = Vec::with_capacity(full.frame_count());
    for i in 0..full.frame_count() {
        let size = full.size(i);
        let (fw, fh) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
        let Some(pixels) = full.as_bytes(i).and_then(|b| RgbaImage::from_raw(fw, fh, b.to_vec())) else { return full };
        // The channels are in GPUI's BGRA order; resizing does not care which is which.
        let small = DynamicImage::ImageRgba8(pixels).resize_to_fill(width, height, FilterType::Lanczos3).into_rgba8();
        frames.push(Frame::from_parts(small, 0, 0, full.delay(i)));
    }
    Arc::new(RenderImage::new(frames))
}

/// An image source that draws `url` shrunk to a `drawn`-pixel square on this window.
pub fn scaled(url: impl Into<SharedUri>, drawn: gpui::Pixels, window: &Window) -> ImageSource {
    let resource = Resource::Uri(url.into());
    let side = bucket(f32::from(drawn) * window.scale_factor());
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| window.use_asset::<ScaledImage>(&(resource.clone(), side, side), cx)))
}

/// A picture fitted into a box: what to draw, and the size to draw it at once it is known.
pub struct Fitted {
    pub source: ImageSource,
    /// The picture's own size, scaled down (never up) to fit the box, keeping its shape. `None`
    /// until the picture has loaded.
    pub size: Option<Size<Pixels>>,
}

/// `url` fitted whole into `bound`, loading it to learn its size. See [`fitted`].
pub fn fit(url: impl Into<SharedUri>, bound: Size<Pixels>, window: &mut Window, cx: &mut App) -> Fitted {
    let url = url.into();
    let natural = match window.use_asset::<ImgResourceLoader>(&Resource::Uri(url.clone()), cx) {
        Some(Ok(full)) => {
            let size = full.size(0);
            Some((size.width.0.max(0) as u32, size.height.0.max(0) as u32))
        }
        _ => None,
    };
    fitted(url, natural, bound, window.scale_factor())
}

/// `url`, whose own size in pixels is `natural` (if known), fitted whole into `bound`. A larger
/// picture is drawn from a copy resized with a Lanczos filter to the device pixels it covers,
/// instead of GPUI sampling it down jagged.
pub fn fitted(url: impl Into<SharedUri>, natural: Option<(u32, u32)>, bound: Size<Pixels>, scale_factor: f32) -> Fitted {
    let resource = Resource::Uri(url.into());
    let Some((w, h)) = natural.filter(|(w, h)| *w > 0 && *h > 0) else {
        return Fitted { source: ImageSource::Resource(resource), size: None };
    };
    let scale = (f32::from(bound.width) / w as f32).min(f32::from(bound.height) / h as f32).min(1.);
    let size = Size { width: px((w as f32 * scale).max(1.)), height: px((h as f32 * scale).max(1.)) };
    let (dw, dh) = ((f32::from(size.width) * scale_factor).ceil() as u32, (f32::from(size.height) * scale_factor).ceil() as u32);
    let source = if dw < w && dh < h {
        ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| window.use_asset::<ScaledImage>(&(resource.clone(), dw, dh), cx)))
    } else {
        ImageSource::Resource(resource)
    };
    Fitted { source, size: Some(size) }
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
