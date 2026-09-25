//! Video for calls: remote tracks turned into frames the UI can draw, and local sources (a
//! screen through the desktop portal, a test pattern) to publish.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use livekit::webrtc::native::yuv_helper;
use livekit::webrtc::prelude::{I420Buffer, RtcVideoTrack, VideoBuffer, VideoFrame, VideoResolution, VideoRotation};
use livekit::webrtc::video_source::native::NativeVideoSource;
use livekit::webrtc::video_stream::native::NativeVideoStream;

/// One decoded picture, BGRA, top row first.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// The latest picture of a remote video track, updated by a background task.
#[derive(Clone)]
pub struct VideoFeed {
    latest: Arc<Mutex<Option<Arc<Picture>>>>,
    seq: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    /// Held by the handles only: the last one dropped stops the task.
    _guard: Arc<StopOnDrop>,
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl VideoFeed {
    /// Starts reading `track` (inside a tokio runtime). Pictures wider than `max_width` are
    /// scaled down, and at most `fps` are converted per second.
    pub fn start(track: RtcVideoTrack, max_width: u32, fps: u32) -> VideoFeed {
        let stopped = Arc::new(AtomicBool::new(false));
        let feed = VideoFeed { latest: Arc::default(), seq: Arc::default(), stopped: stopped.clone(), _guard: Arc::new(StopOnDrop(stopped)) };
        let (latest, seq, stopped) = (feed.latest.clone(), feed.seq.clone(), feed.stopped.clone());
        tokio::spawn(async move {
            let mut stream = NativeVideoStream::new(track);
            let every = Duration::from_millis(1000 / fps.max(1) as u64);
            let mut last: Option<Instant> = None;
            while let Some(frame) = stream.next().await {
                if stopped.load(Ordering::Relaxed) {
                    break;
                }
                if last.is_some_and(|at| at.elapsed() < every) {
                    continue;
                }
                last = Some(Instant::now());
                let picture = to_picture(&*frame.buffer, max_width);
                *latest.lock().unwrap() = Some(Arc::new(picture));
                seq.fetch_add(1, Ordering::Relaxed);
            }
            stream.close();
        });
        feed
    }

    /// The latest picture and its number (which changes with every new one).
    pub fn latest(&self) -> Option<(u64, Arc<Picture>)> {
        let picture = self.latest.lock().unwrap().clone()?;
        Some((self.seq.load(Ordering::Relaxed), picture))
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

fn to_picture(buffer: &dyn VideoBuffer, max_width: u32) -> Picture {
    let mut i420 = buffer.to_i420();
    let (w, h) = (i420.width(), i420.height());
    if w > max_width && max_width > 0 {
        let scaled_h = (h as u64 * max_width as u64 / w as u64).max(2) as i32 & !1;
        i420 = i420.scale(max_width as i32 & !1, scaled_h);
    }
    let (width, height) = (i420.width(), i420.height());
    let (sy, su, sv) = i420.strides();
    let (y, u, v) = i420.data();
    let mut bgra = vec![0u8; (width * height * 4) as usize];
    // libyuv's "ARGB" is B, G, R, A in memory: what the UI wants.
    yuv_helper::i420_to_argb(y, sy, u, su, v, sv, &mut bgra, width * 4, width as i32, height as i32);
    Picture { width, height, bgra }
}

/// A video source fed with BGRA pictures (screens, cameras, test patterns).
#[derive(Clone)]
pub struct VideoOut {
    source: NativeVideoSource,
}

impl VideoOut {
    pub fn new(width: u32, height: u32, screencast: bool) -> VideoOut {
        VideoOut { source: NativeVideoSource::new(VideoResolution { width, height }, screencast) }
    }

    pub fn source(&self) -> NativeVideoSource {
        self.source.clone()
    }

    /// Sends one picture (BGRA with the given row stride).
    pub fn send(&self, bgra: &[u8], stride: u32, width: u32, height: u32) {
        let (width, height) = (width & !1, height & !1);
        let mut i420 = I420Buffer::new(width, height);
        let (sy, su, sv) = i420.strides();
        let (y, u, v) = i420.data_mut();
        yuv_helper::argb_to_i420(bgra, stride, y, sy, u, su, v, sv, width as i32, height as i32);
        let frame = VideoFrame { rotation: VideoRotation::VideoRotation0, timestamp_us: 0, frame_metadata: None, buffer: i420 };
        self.source.capture_frame(&frame);
    }
}

/// A moving test pattern at 15 fps, until dropped. For tests and the voice bot.
pub struct TestPattern {
    stop: Arc<AtomicBool>,
}

impl TestPattern {
    pub fn start(out: VideoOut, width: u32, height: u32) -> TestPattern {
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        std::thread::spawn(move || {
            let mut t = 0u32;
            let mut bgra = vec![0u8; (width * height * 4) as usize];
            while !running.load(Ordering::Relaxed) {
                for yy in 0..height {
                    for xx in 0..width {
                        let i = ((yy * width + xx) * 4) as usize;
                        // A bar sweeping across a blue-to-green field.
                        let bar = (xx + t * 8) % width < width / 8;
                        bgra[i] = if bar { 255 } else { (255 * xx / width) as u8 };
                        bgra[i + 1] = if bar { 255 } else { (255 * yy / height) as u8 };
                        bgra[i + 2] = if bar { 255 } else { 40 };
                        bgra[i + 3] = 255;
                    }
                }
                out.send(&bgra, width * 4, width, height);
                t += 1;
                std::thread::sleep(Duration::from_millis(66));
            }
        });
        TestPattern { stop }
    }
}

impl Drop for TestPattern {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
