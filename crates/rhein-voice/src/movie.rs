//! Playing a video from a message: H.264 in an MP4 or QuickTime file, decoded with OpenH264 (built
//! in, so no codec has to be installed) on a thread of its own, its sound through [`Playback`].
//! The pictures follow the sound's clock, or the wall clock when there is no sound; only the
//! latest is kept, for the UI to draw. It plays while the file still downloads: it starts once
//! the index and the first moments are here, and waits (clock held) when it catches up.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::I420Buffer;
use openh264::OpenH264API;
use openh264::decoder::{DecodedYUV, Decoder, DecoderConfig, Flush};
use openh264::formats::YUVSource as _;
use re_mp4::{Mp4, Sample, StsdBoxContent, TrackKind};

use crate::buffered::Buffered;
use crate::playback::Playback;
use crate::video::Picture;

/// The file extensions (and so attachments) this can play.
pub const PLAYABLE: &[&str] = &["mp4", "m4v", "mov"];

/// Pictures wider than this are scaled down: a chat never shows a video bigger.
const MAX_WIDTH: u32 = 1280;

/// A picture this late is dropped unshown, so a slow machine catches up instead of lagging on.
const LATE: f64 = 0.25;

/// Having caught up with the download, it plays on once this much more is here.
const AHEAD: f64 = 1.5;

/// Whether a file of this name and MIME type is a video this can play.
pub fn playable(filename: &str, mime: &str) -> bool {
    let ext = filename.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(mime, "video/mp4" | "video/quicktime" | "video/x-m4v") || (mime.starts_with("video/") || mime.is_empty() || mime == "application/octet-stream") && PLAYABLE.contains(&ext.as_str())
}

/// The clock pictures are shown by: set to the sound's position while the sound plays, running
/// on its own when there is none or it has ended.
struct Clock {
    /// Seconds at `since` (or for good while stopped).
    base: f64,
    since: Option<Instant>,
}

impl Clock {
    fn now(&self) -> f64 {
        self.base + self.since.map_or(0., |s| s.elapsed().as_secs_f64())
    }

    fn set(&mut self, at: f64) {
        self.base = at;
        if self.since.is_some() {
            self.since = Some(Instant::now());
        }
    }

    fn run(&mut self, running: bool) {
        self.base = self.now();
        self.since = running.then(Instant::now);
    }
}

struct Shared {
    stop: AtomicBool,
    paused: AtomicBool,
    /// Waiting for bytes still downloading.
    buffering: AtomicBool,
    /// Asked for by the UI, taken by the decoder.
    seek: Mutex<Option<f64>>,
    duration: f64,
    /// Set once the sound is open (`None`: there is none we can play). The clock waits for it.
    sound: OnceLock<Option<Playback>>,
    clock: Mutex<Clock>,
    latest: Mutex<Option<Arc<Picture>>>,
    /// Bumped with each new picture.
    seq: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    /// Where playback stands, in seconds: the sound's position while it plays, else the clock
    /// (carried on from where the sound stood).
    fn position(&self) -> f64 {
        let mut clock = self.clock.lock().unwrap();
        if let Some(sound) = self.sound().filter(|s| !s.ended() && s.error().is_none()) {
            clock.set(sound.position());
        }
        clock.now().min(self.duration)
    }

    fn sound(&self) -> Option<&Playback> {
        self.sound.get().and_then(Option::as_ref)
    }

    /// Runs or holds the sound and the clock: they run once the sound is settled, unless paused
    /// or waiting for bytes.
    fn apply(&self) {
        // The clock is brought up to date while the sound still runs.
        let at = self.position();
        let run = self.sound.get().is_some() && !self.paused.load(Ordering::Relaxed) && !self.buffering.load(Ordering::Relaxed);
        if let Some(sound) = self.sound() {
            sound.set_paused(!run);
        }
        let mut clock = self.clock.lock().unwrap();
        clock.set(at);
        clock.run(run);
    }

    fn set_buffering(&self, buffering: bool) {
        if self.buffering.swap(buffering, Ordering::Relaxed) != buffering {
            self.apply();
        }
    }

    /// A stop or a seek came: waiting for bytes ends.
    fn interrupted(&self) -> bool {
        self.stop.load(Ordering::Relaxed) || self.seek.lock().unwrap().is_some()
    }
}

/// A video playing (or paused). Dropping it stops it.
pub struct Movie {
    shared: Arc<Shared>,
    /// The picture's size in the file.
    pub width: u32,
    pub height: u32,
}

impl Movie {
    /// Starts playing `bytes`, its sound on the cpal output `device` (`None` for the default,
    /// `Some("alsa:null")` for silence). The file may still be arriving: this waits for its index,
    /// unless `cancel` says to stop.
    pub fn start(bytes: Arc<Buffered>, device: Option<String>, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Result<Movie> {
        let (stream, width, height, duration) = Stream::read(&bytes, cancel)?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            buffering: AtomicBool::new(false),
            seek: Mutex::new(None),
            duration,
            sound: OnceLock::new(),
            clock: Mutex::new(Clock { base: 0., since: None }),
            latest: Mutex::default(),
            seq: AtomicU64::new(0),
            error: Mutex::default(),
        });
        {
            let (shared, bytes) = (shared.clone(), bytes.clone());
            std::thread::Builder::new()
                .name("rhein-movie".into())
                .spawn(move || {
                    if let Err(err) = decode(&bytes, stream, &shared) {
                        *shared.error.lock().unwrap() = Some(format!("{err:#}"));
                    }
                })
                .context("cannot start the video")?;
        }
        // The sound opens beside the first pictures, not before them. No sound (or none we can
        // play) leaves the wall clock to keep time.
        {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("rhein-movie-sound".into())
                .spawn(move || {
                    let sound = Playback::start_buffered(bytes, Some("mp4"), device).inspect_err(|e| log::debug!("video sound: {e:#}")).ok();
                    // A seek made meanwhile.
                    let at = shared.clock.lock().unwrap().now();
                    if let Some(sound) = &sound
                        && at > 0.
                    {
                        sound.seek(at);
                    }
                    let _ = shared.sound.set(sound);
                    shared.apply();
                })
                .context("cannot start the video's sound")?;
        }
        Ok(Movie { shared, width, height })
    }

    /// The latest picture and its number (a new number, a new picture).
    pub fn picture(&self) -> (u64, Option<Arc<Picture>>) {
        (self.shared.seq.load(Ordering::Relaxed), self.shared.latest.lock().unwrap().clone())
    }

    /// The newest picture, taken out (so not copied), with its number; `None` until another
    /// one comes.
    pub fn take_picture(&self) -> Option<(u64, Arc<Picture>)> {
        let picture = self.shared.latest.lock().unwrap().take()?;
        Some((self.shared.seq.load(Ordering::Relaxed), picture))
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
        self.shared.apply();
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    /// Jumps to `seconds` from the start.
    pub fn seek(&self, seconds: f64) {
        let to = seconds.clamp(0., self.shared.duration);
        if let Some(sound) = self.shared.sound() {
            sound.seek(to);
        }
        self.shared.clock.lock().unwrap().set(to);
        *self.shared.seek.lock().unwrap() = Some(to);
    }

    /// How far it has played, in seconds.
    pub fn position(&self) -> f64 {
        self.shared.position()
    }

    pub fn duration(&self) -> f64 {
        self.shared.duration
    }

    /// Played to the end (or failed; see [`Movie::error`]).
    pub fn ended(&self) -> bool {
        self.error().is_some() || self.position() >= self.shared.duration - 0.05
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Held, waiting for bytes still downloading (or for its sound to open).
    pub fn buffering(&self) -> bool {
        self.shared.buffering.load(Ordering::Relaxed) || self.shared.sound.get().is_none()
    }
}

impl Drop for Movie {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(sound) = self.shared.sound() {
            sound.set_paused(true);
        }
    }
}

/// A video's first picture, for showing before it plays, and its shape as shown.
pub struct Poster {
    pub picture: Picture,
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

/// Decodes the first picture of `bytes`, asking its download for no more than the index and that
/// picture: at most `max_width` wide.
pub fn poster(bytes: &Arc<Buffered>, max_width: u32, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Result<Poster> {
    let cancel = Arc::new(cancel);
    let (stream, width, height, duration) = Stream::read(bytes, {
        let cancel = cancel.clone();
        move || cancel()
    })?;
    let mut decoder = new_decoder(&stream)?;
    let (mut data, mut nal) = (Vec::new(), Vec::new());
    // A picture can need a few samples after it (B-frames) before the decoder gives it out.
    for sample in stream.samples.iter().take(32) {
        bytes.want_until(sample.byte_range().end as u64);
        bytes.read_at(sample.byte_range(), &mut data, &|| cancel())?;
        stream.annex_b(&data, &mut nal);
        if let Ok(Some(yuv)) = decoder.decode(&nal) {
            return Ok(Poster { picture: turned(picture_of(&yuv, max_width), stream.turns), width, height, duration });
        }
    }
    let yuv = decoder.flush_remaining()?.into_iter().next().ok_or_else(|| anyhow!("this video shows no picture"))?;
    Ok(Poster { picture: turned(picture_of(&yuv, max_width), stream.turns), width, height, duration })
}

/// How many bytes from the start of a video hold its first `seconds` (its sound sits beside the
/// pictures), once its index is here.
pub fn bytes_for(bytes: &Arc<Buffered>, seconds: f64, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Result<u64> {
    let (stream, ..) = Stream::read(bytes, cancel)?;
    Ok(stream.span(0, seconds).end as u64)
}

/// The shape of a video as shown, whatever its codec (a poster needs H.264; the shape does not).
pub fn shape(bytes: &Arc<Buffered>, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Result<(u32, u32)> {
    let reader = bytes.reader(cancel);
    let len = reader.size()?;
    let mp4 = Mp4::read(reader, len).map_err(|e| anyhow!("this video cannot be read: {e}"))?;
    let track = mp4.tracks().values().find(|t| t.kind == Some(TrackKind::Video)).ok_or_else(|| anyhow!("this file has no video in it"))?;
    let tkhd = &track.trak(&mp4).tkhd;
    let (w, h) = (tkhd.width.value() as u32, tkhd.height.value() as u32);
    if w == 0 || h == 0 {
        return Err(anyhow!("this video does not say its size"));
    }
    let m = &tkhd.matrix;
    Ok(if quarter_turns(m.a, m.b, m.c, m.d) % 2 == 1 { (h, w) } else { (w, h) })
}

/// The video track as the decoder reads it.
struct Stream {
    /// The parameter sets, as Annex B.
    headers: Vec<u8>,
    /// The bytes before each NAL unit in a sample, giving its length.
    length_size: usize,
    timescale: f64,
    /// In decoding order.
    samples: Vec<Sample>,
    /// How many quarter turns clockwise the pictures are shown turned.
    turns: u8,
}

/// The quarter turns clockwise a track's matrix (its rotating part, in 16.16 fixed point) asks
/// for; a mirrored or skewed one is shown as stored.
fn quarter_turns(a: i32, b: i32, c: i32, d: i32) -> u8 {
    const ONE: i32 = 0x10000;
    match (a, b, c, d) {
        (0, ONE, n, 0) if n == -ONE => 1,
        (n, 0, 0, k) if n == -ONE && k == -ONE => 2,
        (0, n, ONE, 0) if n == -ONE => 3,
        _ => 0,
    }
}

/// `picture` turned clockwise by `turns` quarter turns.
fn turned(picture: Picture, turns: u8) -> Picture {
    let (w, h) = (picture.width as usize, picture.height as usize);
    if turns.is_multiple_of(4) {
        return picture;
    }
    let (src, _) = picture.bgra.as_chunks::<4>();
    let (tw, th) = if turns % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![[0u8; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turns % 4 {
                1 => (h - 1 - y, x),
                2 => (w - 1 - x, h - 1 - y),
                _ => (y, w - 1 - x),
            };
            out[ny * tw + nx] = src[y * w + x];
        }
    }
    Picture { width: tw as u32, height: th as u32, bgra: out.into_flattened() }
}

impl Stream {
    /// The video track of an MP4, once its index is here; with the picture's size as shown and
    /// the length in seconds.
    fn read(bytes: &Arc<Buffered>, cancel: impl Fn() -> bool + Send + Sync + 'static) -> Result<(Stream, u32, u32, f64)> {
        let reader = bytes.reader(cancel);
        let len = reader.size()?;
        let mp4 = Mp4::read(reader, len).map_err(|e| anyhow!("this video cannot be read: {e}"))?;
        let track = mp4.tracks().values().find(|t| t.kind == Some(TrackKind::Video)).ok_or_else(|| anyhow!("this file has no video in it"))?;
        let StsdBoxContent::Avc1(avc1) = &track.trak(&mp4).mdia.minf.stbl.stsd.contents else {
            let codec = track.codec_string(&mp4).unwrap_or_else(|| "this format".into());
            return Err(anyhow!("videos in {codec} cannot be played here, only H.264"));
        };
        let mut headers = Vec::new();
        for nal in avc1.avcc.sequence_parameter_sets.iter().chain(&avc1.avcc.picture_parameter_sets) {
            headers.extend_from_slice(&[0, 0, 0, 1]);
            headers.extend_from_slice(&nal.bytes);
        }
        let stream = Stream {
            headers,
            length_size: avc1.avcc.length_size_minus_one as usize % 4 + 1,
            timescale: track.timescale.max(1) as f64,
            samples: track.samples.clone(),
            turns: {
                let m = &track.trak(&mp4).tkhd.matrix;
                quarter_turns(m.a, m.b, m.c, m.d)
            },
        };
        let duration = match track.duration {
            0 => stream.samples.iter().map(|s| s.composition_timestamp as f64 + s.duration as f64).fold(0., f64::max),
            d => d as f64,
        } / stream.timescale;
        // As shown: a phone's portrait video is stored on its side and turned upright.
        let (width, height) = match stream.turns % 2 {
            0 => (avc1.width as u32, avc1.height as u32),
            _ => (avc1.height as u32, avc1.width as u32),
        };
        Ok((stream, width, height, duration))
    }

    /// A sample's NAL units (`data`) as Annex B (start codes instead of lengths), which OpenH264
    /// reads.
    fn annex_b(&self, mut data: &[u8], out: &mut Vec<u8>) {
        out.clear();
        while data.len() > self.length_size {
            let len = data[..self.length_size].iter().fold(0usize, |n, b| n << 8 | *b as usize);
            let Some(nal) = data.get(self.length_size..self.length_size + len) else { return };
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
            data = &data[self.length_size + len..];
        }
    }

    /// The bytes of the samples from `from` on that are shown in the next `seconds`.
    fn span(&self, from: usize, seconds: f64) -> std::ops::Range<usize> {
        let Some(first) = self.samples.get(from) else { return 0..0 };
        let until = first.composition_timestamp + (seconds * self.timescale) as i64;
        let ahead = self.samples[from..].iter().take_while(|s| s.composition_timestamp < until || s.id == first.id);
        ahead.fold(first.byte_range(), |r, s| r.start.min(s.byte_range().start)..r.end.max(s.byte_range().end))
    }

    /// Where to start decoding for a picture at `at` seconds: the last key frame not after it.
    fn key_before(&self, at: f64) -> usize {
        let at = (at * self.timescale) as i64;
        self.samples.iter().rposition(|s| s.is_sync && s.composition_timestamp <= at).unwrap_or(0)
    }
}

fn new_decoder(stream: &Stream) -> Result<Decoder> {
    let mut decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new().flush_after_decode(Flush::NoFlush))?;
    decoder.decode(&stream.headers)?;
    Ok(decoder)
}

/// The decoding thread: feeds the samples in order and shows each picture when its time comes.
fn decode(bytes: &Buffered, stream: Stream, shared: &Shared) -> Result<()> {
    let mut decoder = new_decoder(&stream)?;
    let mut next = 0;
    // The display times of the pictures still in the decoder. OpenH264 hands them out in display
    // order but without their times: each one out is the earliest of these.
    let mut pending = BinaryHeap::new();
    // After a seek: pictures before this are decoded but not shown, except the one at the target.
    let mut skip_until: Option<f64> = None;
    let (mut data, mut nal) = (Vec::new(), Vec::new());
    let mut flushed = false;
    // Playing starts with the first moments here, as after catching up with the download.
    let mut wait_ahead = true;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(to) = shared.seek.lock().unwrap().take() {
            decoder = new_decoder(&stream)?;
            next = stream.key_before(to);
            pending.clear();
            skip_until = Some(to);
            flushed = false;
            wait_ahead = true;
        }
        // Paused, the decoder waits, unless a seek still owes the picture at its target.
        if shared.paused.load(Ordering::Relaxed) && skip_until.is_none() {
            std::thread::sleep(Duration::from_millis(15));
            continue;
        }
        if let Some(sample) = stream.samples.get(next) {
            if wait_ahead || !bytes.has(sample.byte_range()) {
                let span = stream.span(next, AHEAD);
                if !bytes.has(span.clone()) {
                    shared.set_buffering(true);
                    match bytes.wait_for(span, &|| shared.interrupted()) {
                        Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(err) => return Err(anyhow!("the video stopped downloading: {err}")),
                        Ok(_) => {}
                    }
                }
                wait_ahead = false;
                shared.set_buffering(false);
            }
            match bytes.read_at(sample.byte_range(), &mut data, &|| shared.interrupted()) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(anyhow!("the video stopped downloading: {err}")),
            }
            next += 1;
            stream.annex_b(&data, &mut nal);
            pending.push(Reverse(sample.composition_timestamp));
            // A broken sample costs that picture, not the video.
            match decoder.decode(&nal) {
                Ok(Some(yuv)) => {
                    present(&yuv, &mut pending, &stream, shared, &mut skip_until);
                }
                Ok(None) => {}
                Err(err) => log::debug!("video sample {}: {err}", sample.id),
            }
        } else if !flushed {
            flushed = true;
            for yuv in decoder.flush_remaining().unwrap_or_default() {
                if !present(&yuv, &mut pending, &stream, shared, &mut skip_until) {
                    break;
                }
            }
            // A seek past the last picture shows the last one.
            skip_until = None;
        } else {
            std::thread::sleep(Duration::from_millis(15));
        }
    }
}

/// Times a decoded picture, waits for it and shows it, or drops it when it is late or before a
/// seek's target. False when a seek or stop came first.
fn present(yuv: &DecodedYUV<'_>, pending: &mut BinaryHeap<Reverse<i64>>, stream: &Stream, shared: &Shared, skip_until: &mut Option<f64>) -> bool {
    let Some(Reverse(pts)) = pending.pop() else { return true };
    let at = pts as f64 / stream.timescale;
    if let Some(target) = *skip_until {
        // The picture showing at the target is the last one starting before it; one a frame
        // short of it is close enough.
        if at + 1. / 24. < target {
            return true;
        }
        *skip_until = None;
        show(yuv, stream.turns, shared);
        return true;
    }
    loop {
        if shared.stop.load(Ordering::Relaxed) || shared.seek.lock().unwrap().is_some() {
            return false;
        }
        let now = shared.position();
        if now > at + LATE {
            return true;
        }
        if now >= at {
            show(yuv, stream.turns, shared);
            return true;
        }
        std::thread::sleep(Duration::from_secs_f64((at - now).min(0.01)));
    }
}

fn show(yuv: &DecodedYUV<'_>, turns: u8, shared: &Shared) {
    let picture = picture_of(yuv, MAX_WIDTH);
    *shared.latest.lock().unwrap() = Some(Arc::new(turned(picture, turns)));
    shared.seq.fetch_add(1, Ordering::Relaxed);
}

/// A decoded picture as BGRA, scaled down to `max_width` when it is wider.
fn picture_of(yuv: &DecodedYUV<'_>, max_width: u32) -> Picture {
    let (w, h) = yuv.dimensions_i32();
    let (sy, su, sv) = yuv.strides_i32();
    let (width, height) = (w as u32, h as u32);
    if width <= max_width {
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        // libyuv's "ARGB" is B, G, R, A in memory: what the UI wants.
        yuv_helper::i420_to_argb(yuv.y(), sy as u32, yuv.u(), su as u32, yuv.v(), sv as u32, &mut bgra, width * 4, w, h);
        Picture { width, height, bgra }
    } else {
        let mut buffer = I420Buffer::new(width, height);
        let (dy, du, dv) = buffer.strides();
        let (y, u, v) = buffer.data_mut();
        copy_plane(yuv.y(), sy as usize, y, dy as usize, width as usize, height as usize);
        copy_plane(yuv.u(), su as usize, u, du as usize, width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        copy_plane(yuv.v(), sv as usize, v, dv as usize, width.div_ceil(2) as usize, height.div_ceil(2) as usize);
        crate::video::to_picture(&buffer, max_width)
    }
}

fn copy_plane(src: &[u8], src_stride: usize, dst: &mut [u8], dst_stride: usize, width: usize, rows: usize) {
    for row in 0..rows {
        let (Some(from), Some(to)) = (src.get(row * src_stride..row * src_stride + width), dst.get_mut(row * dst_stride..row * dst_stride + width)) else { return };
        to.copy_from_slice(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One second of ffmpeg's test pattern at 10 fps, 96×64, High profile with B-frames and a
    /// key frame every half second, with an AAC tone.
    const HIGH: &[u8] = include_bytes!("../testdata/high-bframes-aac.mp4");
    /// The same picture, Baseline profile, no sound.
    const SILENT: &[u8] = include_bytes!("../testdata/baseline-silent.mp4");
    /// The same picture in H.265.
    const HEVC: &[u8] = include_bytes!("../testdata/hevc.mp4");

    /// Four seconds of the same picture and tone, the index first, as written for streaming.
    const STREAMED: &[u8] = include_bytes!("../testdata/streamed-faststart.mp4");
    /// The same, the index last.
    const INDEX_LAST: &[u8] = include_bytes!("../testdata/streamed-moov-last.mp4");

    /// `bytes` arriving `chunk` bytes at a time, every `every`; the end from `tail` on first, as a
    /// range request fetches an index at the end.
    fn trickle(bytes: &'static [u8], tail: Option<usize>, chunk: usize, every: Duration) -> Arc<Buffered> {
        let buffered = Arc::new(Buffered::new(Some(bytes.len() as u64)));
        let feeding = buffered.clone();
        std::thread::spawn(move || {
            if let Some(at) = tail {
                feeding.put_tail(at as u64, &bytes[at..]);
            }
            for part in bytes.chunks(chunk) {
                std::thread::sleep(every);
                feeding.push(part);
            }
            feeding.finish(None);
        });
        buffered
    }

    #[test]
    fn a_poster_needs_only_the_index_and_the_first_picture() {
        for (bytes, tail) in [(STREAMED, None), (INDEX_LAST, Some(26604))] {
            // Nothing comes unless asked for, as a look ahead fetches.
            let buffered = Arc::new(Buffered::new(Some(bytes.len() as u64)));
            let feeding = buffered.clone();
            std::thread::spawn(move || {
                if let Some(at) = tail {
                    feeding.put_tail(at as u64, &bytes[at..]);
                }
                let mut at = 0;
                while at < bytes.len() {
                    if (feeding.head() as usize) < (feeding.until() as usize).max(3300) {
                        let end = (at + 200).min(bytes.len());
                        feeding.push(&bytes[at..end]);
                        at = end;
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if feeding.failed() {
                        return;
                    }
                }
            });
            let poster = poster(&buffered, 1280, || false).unwrap();
            assert_eq!((poster.width, poster.height, poster.picture.width, poster.picture.height), (96, 64, 96, 64));
            assert!((poster.duration - 4.).abs() < 0.1, "{}", poster.duration);
            assert!(buffered.head() < bytes.len() as u64 / 2, "only the start was fetched ({} bytes)", buffered.head());
            assert_eq!(shape(&buffered, || false).unwrap(), (96, 64));
            let two = bytes_for(&buffered, 2., || false).unwrap();
            assert!(two > 3300 && two < bytes.len() as u64, "{two}");
            buffered.give_up();
        }
        let hevc = full(HEVC);
        assert!(poster(&hevc, 1280, || false).is_err());
        assert!(shape(&hevc, || false).is_ok_and(|(w, h)| w > 0 && h > 0));
    }

    #[test]
    fn plays_while_it_downloads() {
        let bytes = trickle(STREAMED, None, 1000, Duration::from_millis(40));
        let movie = Movie::start(bytes.clone(), Some("alsa:null".into()), || false).unwrap();
        wait_for("a picture", || movie.picture().1.is_some());
        assert!(!bytes.complete(), "the first picture comes before the last byte");
        wait_for("the end", || movie.ended());
        assert!(movie.error().is_none(), "{:?}", movie.error());
    }

    #[test]
    fn waits_when_it_catches_up_with_the_download() {
        // Slower than it plays, the index fetched from the end first.
        let bytes = trickle(INDEX_LAST, Some(26604), 500, Duration::from_millis(100));
        let movie = Movie::start(bytes.clone(), Some("alsa:null".into()), || false).unwrap();
        wait_for("a picture", || movie.picture().1.is_some());
        assert!(!bytes.complete());
        let until = Instant::now() + Duration::from_secs(10);
        let mut held = false;
        while !movie.ended() {
            assert!(Instant::now() < until, "timed out waiting for the end");
            held |= movie.picture().1.is_some() && movie.buffering();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(held, "it waited for the download");
        assert!(movie.error().is_none(), "{:?}", movie.error());
    }

    fn full(bytes: &[u8]) -> Arc<Buffered> {
        Arc::new(Buffered::full(bytes.to_vec()))
    }

    fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
        let until = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn plays_pauses_seeks_and_ends() {
        // Silence on ALSA's null device where there is one; without it the clock keeps time.
        let movie = Movie::start(full(HIGH), Some("alsa:null".into()), || false).unwrap();
        assert_eq!((movie.width, movie.height), (96, 64));
        assert!((movie.duration() - 1.).abs() < 0.05, "{}", movie.duration());
        wait_for("a picture", || movie.picture().1.is_some());
        let picture = movie.picture().1.unwrap();
        assert_eq!((picture.width, picture.height, picture.bgra.len()), (96, 64, 96 * 64 * 4));

        movie.set_paused(true);
        let (seq, held) = (movie.picture().0, movie.position());
        std::thread::sleep(Duration::from_millis(200));
        assert!((movie.position() - held).abs() < 0.02, "the clock stops while paused");
        assert!(movie.picture().0 <= seq + 1, "no pictures while paused");

        // Paused, a seek still shows the picture it lands on.
        let seq = movie.picture().0;
        movie.seek(0.75);
        wait_for("the picture at the seek", || movie.picture().0 > seq);
        assert!((movie.position() - 0.75).abs() < 0.05, "{}", movie.position());

        movie.set_paused(false);
        wait_for("the end", || movie.ended());
        assert!(movie.error().is_none(), "{:?}", movie.error());
    }

    #[test]
    fn plays_without_sound() {
        let movie = Movie::start(full(SILENT), Some("alsa:null".into()), || false).unwrap();
        wait_for("a few pictures", || movie.picture().0 >= 3);
        wait_for("the end", || movie.ended());
        assert!(movie.error().is_none(), "{:?}", movie.error());
    }

    #[test]
    fn other_codecs_and_non_videos_are_refused() {
        let err = Movie::start(full(HEVC), None, || false).err().expect("H.265 is not played");
        assert!(format!("{err}").contains("H.264"), "{err}");
        assert!(Movie::start(full(b"not a video at all"), None, || false).is_err());
    }

    #[test]
    fn pictures_turn_upright() {
        // 2×1: blue, red (B, G, R, A).
        let picture = || Picture { width: 2, height: 1, bgra: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let right = turned(picture(), 1);
        assert_eq!((right.width, right.height), (1, 2));
        assert_eq!(right.bgra, [255, 0, 0, 255, 0, 0, 255, 255], "blue on top");
        let left = turned(picture(), 3);
        assert_eq!(left.bgra, [0, 0, 255, 255, 255, 0, 0, 255], "red on top");
        assert_eq!(turned(picture(), 2).bgra, [0, 0, 255, 255, 255, 0, 0, 255]);
        const ONE: i32 = 0x10000;
        assert_eq!(quarter_turns(0, ONE, -ONE, 0), 1);
        assert_eq!(quarter_turns(-ONE, 0, 0, -ONE), 2);
        assert_eq!(quarter_turns(0, -ONE, ONE, 0), 3);
        assert_eq!(quarter_turns(ONE, 0, 0, ONE), 0);
    }

    #[test]
    fn videos_are_told_from_other_files() {
        assert!(playable("clip.mp4", "video/mp4"));
        assert!(playable("Clip.MOV", "application/octet-stream"));
        assert!(playable("clip", "video/quicktime"));
        assert!(!playable("song.mp4", "audio/mp4"));
        assert!(!playable("clip.webm", "video/webm"));
    }
}
