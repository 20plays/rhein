//! Cropping an avatar or a banner before it is uploaded, as the web client's crop dialog does
//! (`web/src/views/dialogs/CropImageDialog.jsx`, presets in `web/src/lib/image.js`): the crop is a
//! zoom and a centre on the source picture, always inside it, and the result is resized to the
//! preset and encoded small enough for the server's profile-asset ceiling.

use std::io::Cursor;

use anyhow::{Result, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};

/// What a crop produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub width: u32,
    pub height: u32,
    pub max_bytes: usize,
    /// Drawn as a circle (the crop is still square).
    pub round: bool,
}

pub const AVATAR: Preset = Preset { width: 256, height: 256, max_bytes: 256 * 1024, round: true };
pub const BANNER: Preset = Preset { width: 600, height: 200, max_bytes: 256 * 1024, round: false };

pub const MAX_ZOOM: f64 = 4.;

impl Preset {
    pub fn aspect(&self) -> f64 {
        self.width as f64 / self.height as f64
    }
}

/// A crop: how far in (1 shows as much of the picture as the shape allows) and where its centre
/// is, as fractions of the picture's width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub zoom: f64,
    pub cx: f64,
    pub cy: f64,
}

impl Default for Crop {
    fn default() -> Self {
        Crop { zoom: 1., cx: 0.5, cy: 0.5 }
    }
}

/// The rectangle a crop takes from a `w`×`h` picture, in its pixels: left, top, width, height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Crop {
    /// Width of the crop in source pixels.
    fn span(&self, w: f64, h: f64, aspect: f64) -> f64 {
        w.min(h * aspect) / self.zoom
    }

    pub fn rect(&self, w: f64, h: f64, aspect: f64) -> Rect {
        let cw = self.span(w, h, aspect);
        let ch = cw / aspect;
        Rect { x: self.cx * w - cw / 2., y: self.cy * h - ch / 2., w: cw, h: ch }
    }

    /// The zoom kept between 1 and `MAX_ZOOM`, and the centre moved so the crop stays inside the
    /// picture.
    pub fn clamped(self, w: f64, h: f64, aspect: f64) -> Crop {
        let zoom = self.zoom.clamp(1., MAX_ZOOM);
        let crop = Crop { zoom, ..self };
        let r = crop.rect(w, h, aspect);
        let cx = (self.cx * w).clamp(r.w / 2., w - r.w / 2.) / w;
        let cy = (self.cy * h).clamp(r.h / 2., h - r.h / 2.) / h;
        Crop { zoom, cx, cy }
    }

    /// Moves the picture by a drag of `dx`, `dy` on a frame `frame_w` wide (the picture follows the
    /// pointer, so the crop moves the other way).
    pub fn panned(self, dx: f64, dy: f64, frame_w: f64, w: f64, h: f64, aspect: f64) -> Crop {
        let per_px = self.span(w, h, aspect) / frame_w;
        Crop { cx: self.cx - dx * per_px / w, cy: self.cy - dy * per_px / h, ..self }.clamped(w, h, aspect)
    }

    /// Zooms to `zoom` keeping the picture's point under (`fx`, `fy`) — fractions across the
    /// frame — where it is.
    pub fn zoomed(self, zoom: f64, fx: f64, fy: f64, w: f64, h: f64, aspect: f64) -> Crop {
        let before = self.rect(w, h, aspect);
        let (px, py) = (before.x + fx * before.w, before.y + fy * before.h);
        let next = Crop { zoom: zoom.clamp(1., MAX_ZOOM), ..self };
        let after = next.rect(w, h, aspect);
        let (x, y) = (px - fx * after.w, py - fy * after.h);
        Crop { cx: (x + after.w / 2.) / w, cy: (y + after.h / 2.) / h, ..next }.clamped(w, h, aspect)
    }
}

/// An encoded result: bytes, file name and type.
#[derive(Debug, Clone)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub mime: &'static str,
}

/// Crops, resizes to the preset and encodes: PNG for an avatar with transparency when it fits,
/// JPEG otherwise, lowering the quality until it is under the preset's ceiling.
pub fn render(picture: &DynamicImage, crop: Crop, preset: Preset) -> Result<Encoded> {
    let (w, h) = picture.dimensions();
    let r = crop.clamped(w as f64, h as f64, preset.aspect()).rect(w as f64, h as f64, preset.aspect());
    let (x, y) = (r.x.round().max(0.) as u32, r.y.round().max(0.) as u32);
    let cw = (r.w.round() as u32).clamp(1, w - x.min(w - 1));
    let ch = (r.h.round() as u32).clamp(1, h - y.min(h - 1));
    let out = picture.crop_imm(x, y, cw, ch).resize_exact(preset.width, preset.height, FilterType::Lanczos3);

    if preset.round && out.color().has_alpha() && out.to_rgba8().pixels().any(|p| p.0[3] < 255) {
        let mut png = Vec::new();
        out.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)?;
        if png.len() <= preset.max_bytes {
            return Ok(Encoded { bytes: png, filename: "avatar.png".into(), mime: "image/png" });
        }
    }
    let rgb = DynamicImage::ImageRgb8(out.to_rgb8());
    for quality in [92u8, 85, 78, 70, 60, 50, 40] {
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, quality).encode_image(&rgb)?;
        if jpeg.len() <= preset.max_bytes {
            let name = if preset.round { "avatar.jpg" } else { "banner.jpg" };
            return Ok(Encoded { bytes: jpeg, filename: name.into(), mime: "image/jpeg" });
        }
    }
    bail!("this picture can't be made smaller than {} KB", preset.max_bytes / 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 1000.;
    const H: f64 = 500.;

    #[test]
    fn a_crop_stays_inside_the_picture() {
        // A banner (3:1) of a 2:1 picture: full width at zoom 1, and pinned to the edges.
        let r = Crop::default().rect(W, H, 3.);
        assert_eq!((r.w, r.h), (1000., 1000. / 3.));
        let far = Crop { zoom: 1., cx: 0.9, cy: 0.0 }.clamped(W, H, 3.);
        assert_eq!(far.cx, 0.5);
        let r = far.rect(W, H, 3.);
        assert!(r.y.abs() < 1e-9, "pulled down to the top edge: {r:?}");
        // Zoom is limited both ways.
        assert_eq!(Crop { zoom: 9., ..Crop::default() }.clamped(W, H, 1.).zoom, MAX_ZOOM);
        assert_eq!(Crop { zoom: 0.2, ..Crop::default() }.clamped(W, H, 1.).zoom, 1.);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let start = Crop::default();
        let (fx, fy) = (0.25, 0.75);
        let before = start.rect(W, H, 1.);
        let point = (before.x + fx * before.w, before.y + fy * before.h);
        let after = start.zoomed(2., fx, fy, W, H, 1.).rect(W, H, 1.);
        let moved = (after.x + fx * after.w, after.y + fy * after.h);
        assert!((point.0 - moved.0).abs() < 1e-6 && (point.1 - moved.1).abs() < 1e-6, "{point:?} vs {moved:?}");
    }

    #[test]
    fn dragging_moves_the_picture_with_the_pointer() {
        let zoomed = Crop { zoom: 2., ..Crop::default() };
        // Dragging right by a tenth of the frame shows more of the left.
        let dragged = zoomed.panned(30., 0., 300., W, H, 1.);
        assert!(dragged.cx < zoomed.cx);
    }

    #[test]
    fn output_fits_the_preset() {
        let noise = image::RgbImage::from_fn(1600, 900, |x, y| image::Rgb([((x * 7) ^ (y * 13)) as u8, (x * y) as u8, (x + y * 3) as u8]));
        let picture = DynamicImage::ImageRgb8(noise);
        let banner = render(&picture, Crop::default(), BANNER).unwrap();
        assert_eq!(banner.mime, "image/jpeg");
        assert!(banner.bytes.len() <= BANNER.max_bytes);
        let back = image::load_from_memory(&banner.bytes).unwrap();
        assert_eq!(back.dimensions(), (600, 200));
        let avatar = render(&picture, Crop { zoom: 3., cx: 0.2, cy: 0.8 }, AVATAR).unwrap();
        assert_eq!(image::load_from_memory(&avatar.bytes).unwrap().dimensions(), (256, 256));
    }
}
