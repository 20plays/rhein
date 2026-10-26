//! Every picture drawn from a URL, decoded once and kept under a memory budget.
//!
//! GPUI's own asset cache keeps whatever it decoded for as long as the app runs, and the window's
//! sprite atlas keeps each picture's texture just as long: an hour of scrolling through images
//! held gigabytes. Here pictures are loaded through one store instead. Each frame starts with
//! [`begin_frame`] (from the root view), and every picture drawn marks itself as used in that
//! frame.
//!
//! Views cached by GPUI replay their last paint without drawing their pictures again, so a
//! picture not marked in an ordinary frame may still be on screen. Pictures are therefore only
//! dropped after a census: the window is refreshed, which draws every view afresh, and whatever
//! that frame did not draw is off screen. The least recently used of those go until the store is
//! under its target, and any unused for a few minutes go too, with their textures.
//!
//! Pictures shrunk to the size they are drawn at ([`scaled`], [`fitted`]) are stored without the
//! full picture they came from: GPUI samples a texture down without mipmaps, so a 512 px avatar
//! drawn at 32 px comes out jagged and flickering, while the browser filters it. The decoded
//! picture is resized with a Lanczos filter to the pixels it covers on screen, rounded up to a
//! few sizes so the chat, the member list and the cards share their copies. Animated pictures
//! keep every frame and its delay.
//!
//! GPUI animates a picture of several frames by asking for a new frame of the whole window on
//! every refresh of the screen, whatever the picture's own delays: one GIF in the chat kept the
//! window drawing at 60 Hz (half a core and half the GPU here). So animations are cut into stills
//! when they load, each view is handed the still that is due, and is woken again only when the
//! next one is, the way a browser plays them.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::future::Shared;
use futures::{AsyncReadExt as _, FutureExt as _};
use gpui::{
    App, Asset as _, EntityId, Global, ImageCacheError, ImageSource, ImgResourceLoader, Pixels, RenderImage,
    Resource, SharedString, SharedUri, Size, Task, Window, px,
};
use image::imageops::FilterType;
use image::{DynamicImage, Frame, RgbaImage};

/// Above this many bytes of decoded pictures, a census drops what is off screen…
const BUDGET: usize = 192 << 20;
/// …until this many are left.
const TARGET: usize = 128 << 20;
/// Pictures off screen for this long go at the next census whatever the total.
const STALE: Duration = Duration::from_secs(180);
/// How often an idle window is checked for stale pictures.
pub const SWEEP_EVERY: Duration = Duration::from_secs(60);
/// The pixel sizes a picture is shrunk to (the smallest that covers the drawn size).
const BUCKETS: [u32; 7] = [32, 48, 64, 96, 128, 192, 256];

/// Frame delays at or under this are shown for [`SHORT_DELAY_SHOWN`] instead, as browsers do:
/// such GIFs were made for them and look wrong played faster.
const SHORT_DELAY: Duration = Duration::from_millis(10);
const SHORT_DELAY_SHOWN: Duration = Duration::from_millis(100);

type Loaded = Result<Arc<RenderImage>, ImageCacheError>;

/// A decoded picture as the store keeps it.
#[derive(Clone)]
enum Picture {
    Still(Arc<RenderImage>),
    Moving(Arc<Animation>),
}

/// An animated picture cut into stills of one frame each, so GPUI never animates it itself.
struct Animation {
    stills: Vec<Arc<RenderImage>>,
    /// When each still ends, counted from the start of a loop.
    ends: Vec<Duration>,
}

impl Animation {
    /// The still due `elapsed` after the animation started, and how long until the next is.
    fn at(&self, elapsed: Duration) -> (&Arc<RenderImage>, Duration) {
        let total = *self.ends.last().expect("an animation has stills");
        let into = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);
        let i = self.ends.iter().position(|end| into < *end).unwrap_or(0);
        (&self.stills[i], self.ends[i] - into)
    }
}

impl Picture {
    fn cut(image: Arc<RenderImage>) -> Self {
        if image.frame_count() <= 1 {
            return Picture::Still(image);
        }
        let mut stills = Vec::with_capacity(image.frame_count());
        let mut ends = Vec::with_capacity(image.frame_count());
        let mut end = Duration::ZERO;
        for i in 0..image.frame_count() {
            let size = image.size(i);
            let (w, h) = (size.width.0.max(0) as u32, size.height.0.max(0) as u32);
            let Some(pixels) = image.as_bytes(i).and_then(|b| RgbaImage::from_raw(w, h, b.to_vec())) else {
                return Picture::Still(image);
            };
            let delay = Duration::from(image.delay(i));
            end += if delay <= SHORT_DELAY { SHORT_DELAY_SHOWN } else { delay };
            stills.push(Arc::new(RenderImage::new(vec![Frame::from_parts(pixels, 0, 0, image.delay(i))])));
            ends.push(end);
        }
        Picture::Moving(Arc::new(Animation { stills, ends }))
    }

    fn stills(&self) -> Vec<Arc<RenderImage>> {
        match self {
            Picture::Still(image) => vec![image.clone()],
            Picture::Moving(animation) => animation.stills.clone(),
        }
    }
}

type Decoded = Result<Picture, ImageCacheError>;

enum State {
    Loading(Shared<Task<Decoded>>),
    Done(Decoded),
}

struct Item {
    state: State,
    bytes: usize,
    /// The last frame that drew it, and when.
    frame: u64,
    used: Instant,
    /// Views waiting for it to load, to be redrawn once it has.
    waiting: Vec<EntityId>,
    /// An animation's start, its views to wake when the next still is due, and whether a
    /// wake-up is scheduled.
    born: Instant,
    playing: Vec<EntityId>,
    timer: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Census {
    Idle,
    /// A refresh is scheduled.
    Requested,
    /// The next frame is the refreshed one.
    Armed,
    /// This frame drew every view.
    Counting(u64),
}

struct Images {
    items: HashMap<u64, Item>,
    /// Each full picture's own size, kept after the picture itself is dropped.
    dims: HashMap<SharedUri, (u32, u32)>,
    bytes: usize,
    frame: u64,
    census: Census,
}

impl Default for Images {
    fn default() -> Self {
        Self { items: HashMap::new(), dims: HashMap::new(), bytes: 0, frame: 0, census: Census::Idle }
    }
}

impl Global for Images {}

impl Images {
    /// Drops what the frame `counted` did not draw: the oldest first while over the target, and
    /// every stale one.
    fn evict(&mut self, counted: u64) -> Vec<Arc<RenderImage>> {
        let mut off: Vec<(u64, Instant)> =
            self.items.iter().filter(|(_, item)| item.frame < counted).map(|(key, item)| (*key, item.used)).collect();
        off.sort_by_key(|(_, used)| *used);
        let mut dropped = Vec::new();
        for (key, used) in off {
            if self.bytes <= TARGET && used.elapsed() < STALE {
                break;
            }
            let Some(item) = self.items.remove(&key) else { continue };
            self.bytes -= item.bytes;
            if let State::Done(Ok(picture)) = item.state {
                dropped.extend(picture.stills());
            }
        }
        dropped
    }

    fn stale(&self) -> bool {
        self.items.values().any(|item| item.used.elapsed() >= STALE)
    }
}

/// Marks the start of a frame. The root view calls it first thing in its render.
pub fn begin_frame(window: &mut Window, cx: &mut App) {
    let store = cx.default_global::<Images>();
    store.frame += 1;
    let frame = store.frame;
    match store.census {
        Census::Armed => store.census = Census::Counting(frame),
        Census::Counting(counted) if frame > counted => {
            let dropped = store.evict(counted);
            store.census = Census::Idle;
            if !dropped.is_empty() {
                log::debug!("images: dropped {}, {} MB kept", dropped.len(), cx.global::<Images>().bytes >> 20);
            }
            for image in dropped {
                cx.drop_image(image, Some(window));
            }
        }
        _ => {}
    }
    if cx.global::<Images>().bytes > BUDGET {
        request_census(window, cx);
    }
}

/// Starts a census when pictures have gone unused for a while. Called now and then while the
/// window is open, so an idle window gives its memory back too.
pub fn sweep(window: &mut Window, cx: &mut App) {
    if cx.try_global::<Images>().is_some_and(Images::stale) {
        request_census(window, cx);
    }
}

fn request_census(window: &mut Window, cx: &mut App) {
    let store = cx.default_global::<Images>();
    if store.census != Census::Idle {
        return;
    }
    store.census = Census::Requested;
    // Not while drawing: a refresh asked for during a frame is ignored.
    window.on_next_frame(|window, cx| {
        window.refresh();
        cx.global_mut::<Images>().census = Census::Armed;
    });
}

/// The picture under `key`, marked as drawn in this frame, or `None` while it loads (the view
/// drawing it is redrawn once it has). `start` begins loading it the first time.
fn get(key: u64, window: &mut Window, cx: &mut App, start: impl FnOnce(&mut App) -> Task<Loaded>) -> Option<Loaded> {
    let view = window.current_view();
    // Animations play only where GPUI would play them: in the active window, without reduced motion.
    let (active, reduced) = (window.is_window_active(), cx.reduce_motion());
    let store = cx.default_global::<Images>();
    let frame = store.frame;
    let task = match store.items.get_mut(&key) {
        Some(item) => {
            item.frame = frame;
            item.used = Instant::now();
            if let State::Loading(task) = &item.state
                && let Some(result) = task.clone().now_or_never()
            {
                item.bytes = size_of(&result);
                item.state = State::Done(result);
                item.born = Instant::now();
                store.bytes += item.bytes;
            }
            match &item.state {
                State::Done(Err(err)) => return Some(Err(err.clone())),
                State::Done(Ok(Picture::Still(image))) => return Some(Ok(image.clone())),
                State::Done(Ok(Picture::Moving(animation))) => {
                    if reduced {
                        return Some(Ok(animation.stills[0].clone()));
                    }
                    let (still, next) = animation.at(item.born.elapsed());
                    let still = still.clone();
                    // An inactive window holds the still it shows; activating it redraws it.
                    if !active {
                        return Some(Ok(still));
                    }
                    if !item.playing.contains(&view) {
                        item.playing.push(view);
                    }
                    if !item.timer {
                        item.timer = true;
                        wake_after(key, next, cx);
                    }
                    return Some(Ok(still));
                }
                State::Loading(task) => {
                    if item.waiting.contains(&view) {
                        return None;
                    }
                    item.waiting.push(view);
                    task.clone()
                }
            }
        }
        None => {
            let load = start(cx);
            let task = cx.background_executor().spawn(async move { load.await.map(Picture::cut) }).shared();
            let store = cx.global_mut::<Images>();
            let item = Item {
                state: State::Loading(task.clone()),
                bytes: 0,
                frame,
                used: Instant::now(),
                waiting: vec![view],
                born: Instant::now(),
                playing: Vec::new(),
                timer: false,
            };
            store.items.insert(key, item);
            task
        }
    };
    window
        .spawn(cx, async move |cx| {
            _ = task.await;
            cx.on_next_frame(move |_, cx| cx.notify(view));
        })
        .detach();
    None
}

/// Redraws the views playing the animation under `key` once `wait` has passed. They ask again
/// when they draw it; one that no longer does (scrolled away, closed) lets it rest.
fn wake_after(key: u64, wait: Duration, cx: &mut App) {
    cx.spawn(async move |cx| {
        // A hair late rather than early, so the next still is surely due.
        cx.background_executor().timer(wait + Duration::from_millis(1)).await;
        cx.update(|cx| {
            let views = cx.global_mut::<Images>().items.get_mut(&key).map_or_else(Vec::new, |item| {
                item.timer = false;
                std::mem::take(&mut item.playing)
            });
            for view in views {
                cx.notify(view);
            }
        });
    })
    .detach();
}

fn size_of(result: &Decoded) -> usize {
    let Ok(picture) = result else { return 0 };
    picture
        .stills()
        .iter()
        .map(|image| {
            let size = image.size(0);
            size.width.0.max(0) as usize * size.height.0.max(0) as usize * 4
        })
        .sum()
}

fn key_of(resource: &Resource, shrunk: Option<(u32, u32)>) -> u64 {
    let mut hasher = DefaultHasher::new();
    resource.hash(&mut hasher);
    shrunk.hash(&mut hasher);
    hasher.finish()
}

/// Decodes `resource` on the background executor, without GPUI's asset cache.
fn decode(resource: Resource, cx: &mut App) -> Task<Loaded> {
    let load = ImgResourceLoader::load(resource, cx);
    cx.background_executor().spawn(load)
}

fn full(resource: &Resource, window: &mut Window, cx: &mut App) -> Option<Loaded> {
    let result = get(key_of(resource, None), window, cx, |cx| decode(resource.clone(), cx));
    if let (Resource::Uri(uri), Some(Ok(image))) = (resource, &result) {
        let dims = &mut cx.global_mut::<Images>().dims;
        if !dims.contains_key(uri) {
            let size = image.size(0);
            dims.insert(uri.clone(), (size.width.0.max(0) as u32, size.height.0.max(0) as u32));
        }
    }
    result
}

fn shrunk(resource: &Resource, width: u32, height: u32, window: &mut Window, cx: &mut App) -> Option<Loaded> {
    get(key_of(resource, Some((width, height))), window, cx, |cx| {
        let load = ImgResourceLoader::load(resource.clone(), cx);
        cx.background_executor().spawn(async move { Ok(shrink(load.await?, width, height)) })
    })
}

/// A picture to draw whole, at its own size: a URL, or the path of an embedded asset.
pub fn source(url: impl Into<SharedString>) -> ImageSource {
    let url = url.into();
    whole(if url.contains("://") { Resource::Uri(url.into()) } else { Resource::Embedded(url) })
}

/// A picture in a file, drawn whole.
pub fn file(path: impl Into<std::sync::Arc<std::path::Path>>) -> ImageSource {
    whole(Resource::Path(path.into()))
}

fn whole(resource: Resource) -> ImageSource {
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| full(&resource, window, cx)))
}

/// The size of the picture at `url`, read from its header without decoding it. The HTTP client
/// keeps the bytes, so drawing it afterwards does not fetch it again.
pub fn probe(url: impl Into<SharedUri>, cx: &mut App) -> Task<Option<(u32, u32)>> {
    let url = url.into();
    if let Some(dims) = cx.try_global::<Images>().and_then(|store| store.dims.get(&url).copied()) {
        return Task::ready(Some(dims));
    }
    let client = cx.http_client();
    let fetch = {
        let url = url.clone();
        cx.background_executor().spawn(async move {
            let mut response = client.get(url.as_ref(), ().into(), true).await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let mut body = Vec::new();
            response.body_mut().read_to_end(&mut body).await.ok()?;
            image::ImageReader::new(Cursor::new(body)).with_guessed_format().ok()?.into_dimensions().ok()
        })
    };
    cx.spawn(async move |cx| {
        let dims = fetch.await?;
        cx.update(|cx| cx.default_global::<Images>().dims.insert(url, dims));
        Some(dims)
    })
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

/// `url` drawn shrunk to a `drawn`-pixel square on this window.
pub fn scaled(url: impl Into<SharedUri>, drawn: Pixels, window: &Window) -> ImageSource {
    let resource = Resource::Uri(url.into());
    let side = bucket(f32::from(drawn) * window.scale_factor());
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| shrunk(&resource, side, side, window, cx)))
}

/// A picture held in memory (a pasted one), drawn shrunk to a `drawn`-pixel square.
pub fn local(image: Arc<gpui::Image>, drawn: Pixels, window: &Window) -> ImageSource {
    let side = bucket(f32::from(drawn) * window.scale_factor());
    let mut hasher = DefaultHasher::new();
    ("local", image.id(), side).hash(&mut hasher);
    let key = hasher.finish();
    ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| {
        get(key, window, cx, |cx| {
            let (image, svg) = (image.clone(), cx.svg_renderer());
            cx.background_executor().spawn(async move {
                let full = image.to_image_data(svg).map_err(|err| ImageCacheError::Other(Arc::new(err)))?;
                Ok(shrink(full, side, side))
            })
        })
    }))
}

/// A picture fitted into a box: what to draw, and the size to draw it at once it is known.
pub struct Fitted {
    pub source: ImageSource,
    /// The picture's own size, scaled down (never up) to fit the box, keeping its shape. `None`
    /// until the picture has loaded.
    pub size: Option<Size<Pixels>>,
}

/// `url` fitted whole into `bound`, loading it to learn its size the first time. See [`fitted`].
pub fn fit(url: impl Into<SharedUri>, bound: Size<Pixels>, window: &mut Window, cx: &mut App) -> Fitted {
    let url = url.into();
    let mut natural = cx.try_global::<Images>().and_then(|store| store.dims.get(&url).copied());
    if natural.is_none()
        && let Some(Ok(image)) = full(&Resource::Uri(url.clone()), window, cx)
    {
        let size = image.size(0);
        natural = Some((size.width.0.max(0) as u32, size.height.0.max(0) as u32));
    }
    fitted(url, natural, bound, window.scale_factor())
}

/// `url`, whose own size in pixels is `natural` (if known), fitted whole into `bound`. A larger
/// picture is drawn from a copy resized with a Lanczos filter to the device pixels it covers,
/// instead of GPUI sampling it down jagged.
pub fn fitted(url: impl Into<SharedUri>, natural: Option<(u32, u32)>, bound: Size<Pixels>, scale_factor: f32) -> Fitted {
    let url = url.into();
    let Some((w, h)) = natural.filter(|(w, h)| *w > 0 && *h > 0) else {
        return Fitted { source: whole(Resource::Uri(url)), size: None };
    };
    let scale = (f32::from(bound.width) / w as f32).min(f32::from(bound.height) / h as f32).min(1.);
    let size = Size { width: px((w as f32 * scale).max(1.)), height: px((h as f32 * scale).max(1.)) };
    let (dw, dh) = ((f32::from(size.width) * scale_factor).ceil() as u32, (f32::from(size.height) * scale_factor).ceil() as u32);
    let source = if dw < w && dh < h {
        let resource = Resource::Uri(url);
        ImageSource::Custom(Arc::new(move |window: &mut Window, cx: &mut App| shrunk(&resource, dw, dh, window, cx)))
    } else {
        whole(Resource::Uri(url))
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

    fn gif(delays_ms: &[u32]) -> std::sync::Arc<gpui::RenderImage> {
        let frames = delays_ms
            .iter()
            .map(|ms| image::Frame::from_parts(image::RgbaImage::new(2, 2), 0, 0, image::Delay::from_numer_denom_ms(*ms, 1)))
            .collect::<Vec<_>>();
        std::sync::Arc::new(gpui::RenderImage::new(frames))
    }

    #[test]
    fn animations_are_cut_into_stills_shown_for_their_delays() {
        use std::time::Duration;
        let super::Picture::Moving(animation) = super::Picture::cut(gif(&[50, 0, 200])) else { panic!("not cut") };
        assert!(animation.stills.iter().all(|still| still.frame_count() == 1));
        // A delay of 0 is shown for 100 ms, as browsers do.
        assert_eq!(animation.ends, [50, 150, 350].map(Duration::from_millis));
        let at = |ms| {
            let (still, next) = animation.at(Duration::from_millis(ms));
            (animation.stills.iter().position(|s| std::sync::Arc::ptr_eq(s, still)).unwrap(), next.as_millis())
        };
        assert_eq!(at(0), (0, 50));
        assert_eq!(at(60), (1, 90));
        assert_eq!(at(349), (2, 1));
        // Loops.
        assert_eq!(at(350 + 10), (0, 40));
    }

    #[test]
    fn a_single_frame_stays_whole() {
        assert!(matches!(super::Picture::cut(gif(&[0])), super::Picture::Still(_)));
    }
}
