//! Videos in messages: one plays at a time (and stops a sound file playing), fetched (and in DMs
//! decrypted) when first played, or its start when the pointer rests on it. It plays while the
//! rest arrives, and the last few stay for playing again. What it shows lives in
//! [`MoviePlayer`], an entity of its own the player view watches: thirty pictures a second
//! redraw the player and nothing else.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rhein_voice::buffered::Buffered;
use rhein_voice::movie::Movie;

use super::media::Fetcher;

use super::*;

/// What the player of a video shows.
#[derive(Clone, Debug, PartialEq)]
pub struct MovieView {
    pub loading: bool,
    /// Waiting for more of the file, having caught up with its download.
    pub buffering: bool,
    /// How much of the file is here, in whole percent, while it downloads (when its size is known).
    pub downloaded: Option<u8>,
    pub playing: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub error: Option<String>,
    /// The picture's size in the file, once it is loaded.
    pub size: Option<(u32, u32)>,
}

/// The videos' bytes kept (and still downloading) take at most this much, the one loaded aside.
const KEPT: u64 = 256 << 20;

/// The pointer resting on a video this long fetches its start.
const REST: Duration = Duration::from_millis(250);

/// How much of its start the pointer resting on a video fetches.
const LOOK_AHEAD: u64 = 2 << 20;

/// How much of a video near the view is fetched before it is played, so it plays at once.
const PRELOAD: f64 = 15.;

/// The video loaded from a message (at most one), and its latest picture.
#[derive(Default)]
pub struct MoviePlayer {
    key: Option<String>,
    loading: bool,
    error: Option<String>,
    movie: Option<Arc<Movie>>,
    bytes: Option<Arc<Buffered>>,
    /// Set when it is unloaded: starting it stops waiting for the file.
    unloaded: Arc<AtomicBool>,
    picture: Option<(u64, Arc<gpui::RenderImage>)>,
    /// What the view last showed, to redraw only when it changes.
    shown: Option<MovieView>,
    _loading: Option<Task<()>>,
    _ticker: Option<Task<()>>,
    /// Videos' bytes by key, the latest used last; they outlive loading one.
    kept: VecDeque<(String, Arc<Buffered>)>,
    /// The video the pointer rests on, and the wait before its start is fetched.
    resting: Option<(String, Task<()>)>,
    /// Videos whose first moments were asked for (see `Client::preload_movie`).
    preloaded: HashSet<String>,
}

impl MoviePlayer {
    /// The video loaded, by message id and attachment index.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn view(&self) -> Option<MovieView> {
        self.key.as_ref()?;
        let movie = self.movie.as_ref();
        let downloaded = self.bytes.as_ref().filter(|b| !b.complete()).and_then(|b| match b.progress() {
            (got, Some(total)) if total > 0 => Some((got * 100 / total).min(99) as u8),
            _ => None,
        });
        Some(MovieView {
            loading: self.loading,
            buffering: movie.is_some_and(|m| m.buffering()),
            downloaded,
            playing: movie.is_some_and(|m| !m.paused() && !m.ended()),
            position: movie.map_or(0., |m| m.position()),
            duration: movie.map(|m| m.duration()),
            error: self.error.clone().or_else(|| movie.and_then(|m| m.error())),
            size: movie.map(|m| (m.width, m.height)),
        })
    }

    pub fn picture(&self) -> Option<Arc<gpui::RenderImage>> {
        self.picture.as_ref().map(|(_, image)| image.clone())
    }

    fn clear(&mut self, cx: &mut App) {
        if let Some((_, image)) = self.picture.take() {
            cx.drop_image(image, None);
        }
        self.unloaded.store(true, Ordering::Relaxed);
        let (kept, resting, preloaded) = (std::mem::take(&mut self.kept), self.resting.take(), std::mem::take(&mut self.preloaded));
        *self = MoviePlayer { kept, resting, preloaded, ..MoviePlayer::default() };
    }

    /// The bytes of the video `key`, kept or a new download of them; asking for its first
    /// `until` bytes, or all of them (`None`).
    pub(super) fn bytes(&mut self, key: &str, source: MediaSource, until: Option<u64>, io: &Io, fetcher: Fetcher) -> Arc<Buffered> {
        let bytes = match self.kept.iter().position(|(k, _)| k == key) {
            Some(i) if !self.kept[i].1.failed() => self.kept.remove(i).expect("found").1,
            found => {
                if let Some(i) = found {
                    self.kept.remove(i);
                }
                let bytes = Arc::new(Buffered::default());
                io.spawn({
                    let bytes = bytes.clone();
                    async move { fetcher.stream(source, bytes).await }
                });
                bytes
            }
        };
        match until {
            None => bytes.want(),
            Some(n) => bytes.want_until(n),
        }
        self.kept.push_back((key.to_owned(), bytes.clone()));
        // The oldest go (their downloads stop), never the one loaded or just asked for.
        let size = |b: &Buffered| b.progress().0;
        let mut total: u64 = self.kept.iter().map(|(_, b)| size(b)).sum();
        let mut i = 0;
        while total > KEPT && i + 1 < self.kept.len() {
            if self.key.as_deref() == Some(self.kept[i].0.as_str()) {
                i += 1;
                continue;
            }
            let (key, gone) = self.kept.remove(i).expect("in range");
            self.preloaded.remove(&key);
            total -= size(&gone);
            gone.give_up();
        }
        bytes
    }

    /// Takes the movie's newest picture when there is one, and says whether anything shown
    /// changed.
    fn refresh(&mut self, cx: &mut App) -> bool {
        let mut changed = false;
        if let Some((seq, picture)) = self.movie.as_ref().and_then(|m| m.take_picture())
            && let Some(image) = render_image(picture)
        {
            if let Some((_, old)) = self.picture.replace((seq, image)) {
                cx.drop_image(old, None);
            }
            changed = true;
        }
        // The time shown moves in tenths of a second.
        let view = self.view().map(|v| MovieView { position: (v.position * 10.).floor() / 10., ..v });
        if self.shown != view {
            self.shown = view;
            changed = true;
        }
        changed
    }
}

fn render_image(picture: Arc<rhein_voice::video::Picture>) -> Option<Arc<gpui::RenderImage>> {
    let (width, height) = (picture.width, picture.height);
    let bgra = Arc::try_unwrap(picture).map_or_else(|p| p.bgra.clone(), |p| p.bgra);
    let buffer = image::RgbaImage::from_raw(width, height, bgra)?;
    Some(Arc::new(gpui::RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)])))
}

impl Client {
    /// Plays or pauses the video `key` (message id and attachment index); another one loaded
    /// stops.
    pub fn toggle_movie(&mut self, key: String, source: MediaSource, cx: &mut Context<Self>) {
        let player = self.movie_player.clone();
        if player.read(cx).key() == Some(key.as_str()) {
            self.play_pause_movie(cx);
            return;
        }
        // One thing plays at a time.
        self.sound = None;
        cx.notify();
        let device = self.voice_prefs().null_devices.then(|| "alsa:null".to_owned()).or_else(|| self.prefs.voice.output.clone());
        let (io, fetcher) = (self.io.clone(), self.fetcher());
        let wanted = key.clone();
        // The rows learn the picture's size once it is known.
        let tick = self.media_tick.clone();
        let (bytes, unloaded) = self.movie_player.update(cx, |p, cx| {
            p.clear(cx);
            p.key = Some(key.clone());
            p.loading = true;
            let bytes = p.bytes(&key, source, None, &io, fetcher);
            p.bytes = Some(bytes.clone());
            p.refresh(cx);
            cx.notify();
            (bytes, p.unloaded.clone())
        });
        let since = std::time::Instant::now();
        // It starts once the file's index and its first moments are here.
        let started = io.spawn(async move { tokio::task::spawn_blocking(move || Movie::start(bytes, device, move || unloaded.load(Ordering::Relaxed))).await? });
        let loading = cx.spawn({
            let (player, wanted) = (player.clone(), wanted.clone());
            async move |_, cx| {
                let started = join(started).await;
                log::debug!("video {wanted} started in {:?}", since.elapsed());
                player.update(cx, |p, cx| {
                    if p.key.as_deref() != Some(wanted.as_str()) {
                        return;
                    }
                    p.loading = false;
                    match started {
                        Ok(movie) => p.movie = Some(Arc::new(movie)),
                        Err(err) => p.error = Some(friendly(&err)),
                    }
                    p.refresh(cx);
                    cx.notify();
                });
                tick.update(cx, |_, cx| cx.notify());
            }
        });
        // Up to sixty pictures a second while it plays, a slower look while it loads or is paused
        // (a seek still brings a picture); nothing is redrawn unless something changed.
        let ticker = cx.spawn(async move |_, cx| {
            let mut every = Duration::from_millis(100);
            loop {
                cx.background_executor().timer(every).await;
                let next = player.update(cx, |p, cx| {
                    if p.key.as_deref() != Some(wanted.as_str()) {
                        return None;
                    }
                    if p.refresh(cx) {
                        cx.notify();
                    }
                    let playing = p.shown.as_ref().is_some_and(|v| v.playing);
                    (p.loading || p.movie.is_some()).then(|| Duration::from_millis(if playing { 16 } else { 100 }))
                });
                match next {
                    Some(next) => every = next,
                    None => return,
                }
            }
        });
        self.movie_player.update(cx, |p, _| {
            p._loading = Some(loading);
            p._ticker = Some(ticker);
        });
    }

    /// Fetches the first moments of the video `key`, near the view, so playing it starts at
    /// once; the rest comes when it is played. Only guild files: an encrypted one comes whole.
    pub fn preload_movie(&mut self, key: String, source: MediaSource, cx: &mut Context<Self>) {
        if !matches!(source, MediaSource::Url(_)) || self.movie_player.read(cx).preloaded.contains(&key) {
            return;
        }
        let (io, fetcher) = (self.io.clone(), self.fetcher());
        let bytes = self.movie_player.update(cx, |p, _| {
            p.preloaded.insert(key.clone());
            p.bytes(&key, source, Some(0), &io, fetcher)
        });
        io.spawn(async move {
            // Its index says where its first moments end.
            let ahead = tokio::task::spawn_blocking({
                let bytes = bytes.clone();
                move || rhein_voice::movie::bytes_for(&bytes, PRELOAD, || false)
            })
            .await;
            if let Ok(Ok(n)) = ahead {
                bytes.want_until(n + (64 << 10));
            }
        });
    }

    /// The pointer rests on the video `key` (or has left it, `None`): after a moment its start
    /// is fetched, so playing it starts at once. Nothing is redrawn. Only guild files: an
    /// encrypted one would come whole.
    pub fn rest_on_movie(&mut self, key: String, source: Option<MediaSource>, cx: &mut Context<Self>) {
        let (io, fetcher) = (self.io.clone(), self.fetcher());
        let player = self.movie_player.clone();
        player.update(cx, |p, cx| {
            let Some(source @ MediaSource::Url(_)) = source else {
                if p.resting.as_ref().is_some_and(|(k, _)| *k == key) {
                    p.resting = None;
                }
                return;
            };
            if p.key.as_deref() == Some(key.as_str()) || p.kept.iter().any(|(k, b)| *k == key && !b.failed()) {
                return;
            }
            let wait = cx.spawn({
                let key = key.clone();
                async move |player, cx| {
                    cx.background_executor().timer(REST).await;
                    player
                        .update(cx, |p, _| {
                            p.resting = None;
                            p.bytes(&key, source, Some(LOOK_AHEAD), &io, fetcher);
                        })
                        .ok();
                }
            });
            p.resting = Some((key, wait));
        });
    }

    /// Plays or pauses the loaded video; one played to the end starts again.
    pub fn play_pause_movie(&mut self, cx: &mut Context<Self>) {
        self.movie_player.update(cx, |p, cx| {
            if let Some(movie) = &p.movie {
                if movie.ended() && movie.error().is_none() {
                    movie.seek(0.);
                    movie.set_paused(false);
                } else {
                    movie.set_paused(!movie.paused());
                }
                p.refresh(cx);
                cx.notify();
            }
        });
    }

    /// Jumps to `fraction` (0..1) of the video `key`.
    pub fn seek_movie(&mut self, key: &str, fraction: f64, cx: &mut Context<Self>) {
        self.movie_player.update(cx, |p, cx| {
            if p.key.as_deref() != Some(key) {
                return;
            }
            if let Some(movie) = &p.movie {
                movie.seek(movie.duration() * fraction.clamp(0., 1.));
                p.refresh(cx);
                cx.notify();
            }
        });
    }

    /// Stops and unloads the video, if one is loaded.
    pub fn stop_movie(&mut self, cx: &mut Context<Self>) {
        if self.movie_player.read(cx).key().is_some() {
            self.movie_player.update(cx, |p, cx| {
                p.clear(cx);
                cx.notify();
            });
            cx.notify();
        }
    }
}
