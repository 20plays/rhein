//! The crop dialog for a new avatar or banner, as the web client has it: the picture behind a
//! dimmed veil with the crop cut out of it (a circle for an avatar, a wide slot for a banner),
//! dragged to move, zoomed with the wheel (around the pointer), the buttons or the bar below.
//! Saving crops, resizes and encodes it (`rhein_store::crop`); an animated GIF can also go up as
//! it is.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, ObjectFit, ParentElement as _, Pixels, Point,
    Render, RenderImage, ScrollDelta, Styled as _, StyledImage as _, Window, canvas, div, img, prelude::*, px, rgba,
};
use image::DynamicImage;
use rhein_store::crop::{self, Crop, Encoded, MAX_ZOOM, Preset};
use rhein_ui::theme::{radius, text};
use rhein_ui::{ActiveTheme as _, Button, Icon, IconButton, Modal};

/// The crop's size on screen, and the margin of picture shown around it.
const AVATAR_FRAME: f32 = 280.;
const BANNER_FRAME: f32 = 480.;
const MARGIN: f32 = 40.;

pub enum CropEvent {
    Done(Encoded),
    Cancel,
}

pub struct CropDialog {
    picture: Arc<DynamicImage>,
    /// What is drawn: the picture, made smaller if it is huge.
    shown: Arc<RenderImage>,
    preset: Preset,
    crop: Crop,
    /// Where the pointer was at the last step of a drag.
    drag: Option<Point<Pixels>>,
    stage: Rc<Cell<Bounds<Pixels>>>,
    /// The file as picked, for "Use original" (an animated GIF would lose its motion otherwise).
    original: Option<Encoded>,
    busy: bool,
    error: Option<String>,
    focus: FocusHandle,
}

impl EventEmitter<CropEvent> for CropDialog {}

impl Focusable for CropDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A picture ready to crop: decoded, and a copy for the screen.
pub struct Prepared {
    picture: Arc<DynamicImage>,
    shown: Arc<RenderImage>,
    original: Option<Encoded>,
}

/// Decodes a picked file (off the main thread: this is the slow part).
pub fn prepare(bytes: Vec<u8>, filename: &str) -> anyhow::Result<Prepared> {
    let picture = image::load_from_memory(&bytes)?;
    let small = if picture.width().max(picture.height()) > 1600 { picture.thumbnail(1600, 1600) } else { picture.clone() };
    let mut pixels = small.to_rgba8();
    for p in pixels.pixels_mut() {
        p.0.swap(0, 2);
    }
    let shown = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
    let gif = filename.to_ascii_lowercase().ends_with(".gif") || bytes.starts_with(b"GIF8");
    let original = gif.then(|| Encoded { bytes, filename: filename.to_owned(), mime: "image/gif" });
    Ok(Prepared { picture: Arc::new(picture), shown, original })
}

impl CropDialog {
    pub fn new(prepared: Prepared, preset: Preset, cx: &mut Context<Self>) -> Self {
        CropDialog {
            picture: prepared.picture,
            shown: prepared.shown,
            preset,
            crop: Crop::default(),
            drag: None,
            stage: Rc::default(),
            original: prepared.original,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
        }
    }

    fn frame(&self) -> (f32, f32) {
        let w = if self.preset.round { AVATAR_FRAME } else { BANNER_FRAME };
        (w, w / self.preset.aspect() as f32)
    }

    fn size(&self) -> (f64, f64) {
        (self.picture.width() as f64, self.picture.height() as f64)
    }

    fn zoom_to(&mut self, zoom: f64, fx: f64, fy: f64, cx: &mut Context<Self>) {
        let (w, h) = self.size();
        self.crop = self.crop.zoomed(zoom, fx, fy, w, h, self.preset.aspect());
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        let (picture, crop, preset) = (self.picture.clone(), self.crop, self.preset);
        let task = cx.background_spawn(async move { crop::render(&picture, crop, preset) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(encoded) => cx.emit(CropEvent::Done(encoded)),
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn use_original(&mut self, cx: &mut Context<Self>) {
        let Some(original) = self.original.clone() else { return };
        if original.bytes.len() > self.preset.max_bytes {
            self.error = Some(format!("As it is, this file is over {} KB; crop it instead.", self.preset.max_bytes / 1024));
            cx.notify();
            return;
        }
        cx.emit(CropEvent::Done(original));
    }
}

impl Render for CropDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().clone();
        let (fw, fh) = self.frame();
        let (w, h) = self.size();
        let aspect = self.preset.aspect();
        let r = self.crop.rect(w, h, aspect);
        // Screen pixels per source pixel, and where the picture's corner lands on the stage.
        let scale = fw as f64 / r.w;
        let (left, top) = (MARGIN as f64 - r.x * scale, MARGIN as f64 - r.y * scale);
        let (stage_w, stage_h) = (fw + 2. * MARGIN, fh + 2. * MARGIN);
        let veil = rgba(0x0e0a1aad);
        let band = |x: f32, y: f32, bw: f32, bh: f32| div().absolute().left(px(x)).top(px(y)).w(px(bw)).h(px(bh)).bg(veil);

        let stage_bounds = self.stage.clone();
        let stage = div()
            .id("crop-stage")
            .relative()
            .w(px(stage_w))
            .h(px(stage_h))
            .flex_none()
            .overflow_hidden()
            .rounded(radius::MD)
            .bg(t.bg_base)
            .cursor_grab()
            .child(canvas(move |bounds, _, _| stage_bounds.set(bounds), |_, _, _, _| {}).absolute().size_full())
            .child(
                img(gpui::ImageSource::Render(self.shown.clone()))
                    .absolute()
                    .left(px(left as f32))
                    .top(px(top as f32))
                    .w(px((w * scale) as f32))
                    .h(px((h * scale) as f32))
                    .object_fit(ObjectFit::Fill),
            )
            // The veil around the crop, and its edge.
            .child(band(0., 0., stage_w, MARGIN))
            .child(band(0., MARGIN + fh, stage_w, MARGIN))
            .child(band(0., MARGIN, MARGIN, fh))
            .child(band(MARGIN + fw, MARGIN, MARGIN, fh))
            .child(
                div()
                    .absolute()
                    .left(px(MARGIN))
                    .top(px(MARGIN))
                    .w(px(fw))
                    .h(px(fh))
                    .border_1()
                    .border_color(rgba(0xffffff8c))
                    .when(self.preset.round, |d| d.rounded_full().border_2()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &gpui::MouseDownEvent, _, cx| {
                    this.drag = Some(e.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, e: &gpui::MouseMoveEvent, _, cx| {
                let Some(from) = this.drag else { return };
                if e.pressed_button != Some(MouseButton::Left) {
                    this.drag = None;
                    return;
                }
                let d = e.position - from;
                this.drag = Some(e.position);
                let (w, h) = this.size();
                this.crop = this.crop.panned(f32::from(d.x) as f64, f32::from(d.y) as f64, fw as f64, w, h, aspect);
                cx.notify();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.drag = None;
                cx.notify();
            }))
            .on_scroll_wheel(cx.listener(move |this, e: &gpui::ScrollWheelEvent, _, cx| {
                cx.stop_propagation();
                let travel = match e.delta {
                    ScrollDelta::Lines(l) => l.y as f64 * 0.06,
                    ScrollDelta::Pixels(p) => f32::from(p.y) as f64 / 300.,
                };
                let origin = this.stage.get().origin;
                let fx = ((f32::from(e.position.x - origin.x) - MARGIN) / fw).clamp(0., 1.) as f64;
                let fy = ((f32::from(e.position.y - origin.y) - MARGIN) / fh).clamp(0., 1.) as f64;
                let zoom = this.crop.zoom * travel.exp();
                this.zoom_to(zoom, fx, fy, cx);
            }));

        // Zoom: out, a bar to click, in.
        let fill = ((self.crop.zoom - 1.) / (MAX_ZOOM - 1.)).clamp(0., 1.) as f32;
        let track_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::default();
        let track_read = track_bounds.clone();
        let zoom_row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(IconButton::new("crop-zoom-out", Icon::Minus).tooltip("Zoom out").on_click(cx.listener(|this, _, _, cx| {
                let zoom = this.crop.zoom / 1.25;
                this.zoom_to(zoom, 0.5, 0.5, cx);
            })))
            .child(
                div()
                    .id("crop-zoom-bar")
                    .flex_1()
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(canvas(move |bounds, _, _| track_bounds.set(bounds), |_, _, _, _| {}).absolute().size_full())
                    .child(
                        div().relative().w_full().h(px(4.)).rounded_full().bg(t.bg_selected).child(
                            div().absolute().left_0().top_0().h_full().w(gpui::relative(fill)).rounded_full().bg(t.accent),
                        ),
                    )
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &gpui::MouseDownEvent, _, cx| {
                        let b = track_read.get();
                        let f = (f32::from(e.position.x - b.origin.x) / f32::from(b.size.width).max(1.)).clamp(0., 1.) as f64;
                        this.zoom_to(1. + f * (MAX_ZOOM - 1.), 0.5, 0.5, cx);
                    })),
            )
            .child(IconButton::new("crop-zoom-in", Icon::Plus).tooltip("Zoom in").on_click(cx.listener(|this, _, _, cx| {
                let zoom = this.crop.zoom * 1.25;
                this.zoom_to(zoom, 0.5, 0.5, cx);
            })));

        let title = if self.preset.round { "Crop your picture" } else { "Crop your banner" };
        let mut modal = Modal::new("crop-dialog")
            .title(title)
            .width(px(stage_w + 40.))
            .focus(&self.focus)
            .on_dismiss({
                let this = cx.weak_entity();
                move |_, cx| {
                    this.update(cx, |_, cx| cx.emit(CropEvent::Cancel)).ok();
                }
            })
            .child(div().flex().justify_center().child(stage))
            .child(zoom_row)
            .children(self.error.clone().map(|e| div().text_size(text::SM).text_color(t.danger).child(e)))
            .footer(Button::new("crop-cancel", "Cancel").ghost().on_click(cx.listener(|_, _, _, cx| cx.emit(CropEvent::Cancel))));
        if self.original.is_some() {
            modal = modal.footer(Button::new("crop-original", "Use original").on_click(cx.listener(|this, _, _, cx| this.use_original(cx))));
        }
        modal.footer(
            Button::new("crop-save", if self.busy { "Saving…" } else { "Apply" })
                .primary()
                .disabled(self.busy)
                .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
        )
    }
}
