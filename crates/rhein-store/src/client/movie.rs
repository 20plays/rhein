//! Videos in messages: one plays at a time (and stops a sound file playing), fetched (and in DMs
//! decrypted) when first played. What it shows lives in [`MoviePlayer`], an entity of its own
//! the player view watches: thirty pictures a second redraw the player and nothing else.

use std::sync::Arc;

use rhein_voice::movie::Movie;

use super::*;

/// What the player of a video shows.
#[derive(Clone, Debug, PartialEq)]
pub struct MovieView {
    pub loading: bool,
    pub playing: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub error: Option<String>,
    /// The picture's size in the file, once it is loaded.
    pub size: Option<(u32, u32)>,
}

/// The video loaded from a message (at most one), and its latest picture.
#[derive(Default)]
pub struct MoviePlayer {
    key: Option<String>,
    loading: bool,
    error: Option<String>,
    movie: Option<Arc<Movie>>,
    picture: Option<(u64, Arc<gpui::RenderImage>)>,
    /// What the view last showed, to redraw only when it changes.
    shown: Option<MovieView>,
    _ticker: Option<Task<()>>,
}

impl MoviePlayer {
    /// The video loaded, by message id and attachment index.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn view(&self) -> Option<MovieView> {
        self.key.as_ref()?;
        let movie = self.movie.as_ref();
        Some(MovieView {
            loading: self.loading,
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
        *self = MoviePlayer::default();
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
        let loading = cx.spawn(async move |_, cx| {
            let started = join(io.spawn(async move {
                let bytes = fetcher.fetch(source).await?;
                Ok(Arc::new(tokio::task::spawn_blocking(move || Movie::start(bytes.into(), device)).await??))
            }))
            .await;
            player.update(cx, |p, cx| {
                if p.key.as_deref() != Some(wanted.as_str()) {
                    return;
                }
                p.loading = false;
                match started {
                    Ok(movie) => p.movie = Some(movie),
                    Err(err) => p.error = Some(friendly(&err)),
                }
                p.refresh(cx);
                cx.notify();
            });
            tick.update(cx, |_, cx| cx.notify());
            // Up to sixty pictures a second while it plays, a slower look while it is paused (a
            // seek still brings a picture); nothing is redrawn unless something changed.
            let mut every = Duration::from_millis(16);
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
                    p.movie.as_ref().map(|_| Duration::from_millis(if playing { 16 } else { 100 }))
                });
                match next {
                    Some(next) => every = next,
                    None => return,
                }
            }
        });
        self.movie_player.update(cx, |p, cx| {
            p.clear(cx);
            p.key = Some(key);
            p.loading = true;
            p._ticker = Some(loading);
            p.refresh(cx);
            cx.notify();
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
