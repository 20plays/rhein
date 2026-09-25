//! Sharing a screen or window. On Wayland libwebrtc asks the desktop portal, which shows the
//! system's own picker (screen, window, with or without the cursor) and streams the choice over
//! PipeWire; the pictures go into a [`VideoOut`] at the chosen frame rate.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use libwebrtc::desktop_capturer::{CaptureError, DesktopCaptureSourceType, DesktopCapturer, DesktopCapturerOptions};

use crate::video::VideoOut;

/// A running share; dropping it stops the capture.
pub struct ScreenShare {
    stop: Arc<AtomicBool>,
    /// Set when the capture ends on its own (the portal session was closed from the desktop).
    ended: Arc<AtomicBool>,
    out: VideoOut,
}

impl ScreenShare {
    /// Asks the desktop for a screen or window and starts sending it at `fps`. Returns once the
    /// first picture arrived, or with an error when the person cancelled the picker or capture
    /// is unavailable.
    pub fn start(fps: u32, cursor: bool) -> Result<ScreenShare> {
        let stop = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        // Sized once the first picture says how big the share is.
        let (first_tx, first_rx) = mpsc::channel::<Result<VideoOut>>();
        let (running, over) = (stop.clone(), ended.clone());
        std::thread::Builder::new()
            .name("rhein-screen".into())
            .spawn(move || {
                let kind = if cfg!(target_os = "linux") { DesktopCaptureSourceType::Generic } else { DesktopCaptureSourceType::Screen };
                let mut options = DesktopCapturerOptions::new(kind);
                options.set_include_cursor(cursor);
                let Some(mut capturer) = DesktopCapturer::new(options) else {
                    let _ = first_tx.send(Err(anyhow!("screen capture is not available here")));
                    return;
                };
                let out: Arc<std::sync::Mutex<Option<VideoOut>>> = Arc::default();
                let (sink, failed, first) = (out.clone(), over.clone(), first_tx.clone());
                capturer.start_capture(None, move |result| match result {
                    Ok(frame) => {
                        let (w, h) = (frame.width() as u32, frame.height() as u32);
                        let mut sink = sink.lock().unwrap();
                        let video = sink.get_or_insert_with(|| {
                            let video = VideoOut::new(w, h, true);
                            let _ = first.send(Ok(video.clone()));
                            video
                        });
                        video.send(frame.data(), frame.stride(), w, h);
                    }
                    Err(CaptureError::Temporary) => {}
                    Err(CaptureError::Permanent) => {
                        failed.store(true, Ordering::Relaxed);
                        let _ = first.send(Err(anyhow!("the screen share was cancelled or ended")));
                    }
                });
                let every = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
                while !running.load(Ordering::Relaxed) && !over.load(Ordering::Relaxed) {
                    capturer.capture_frame();
                    std::thread::sleep(every);
                }
            })?;
        // The picker waits for the person; give them time.
        let out = first_rx.recv_timeout(Duration::from_secs(120)).map_err(|_| anyhow!("no screen was chosen"))??;
        Ok(ScreenShare { stop, ended, out })
    }

    /// The source to publish.
    pub fn video(&self) -> &VideoOut {
        &self.out
    }

    /// Whether the capture ended from the desktop's side (its own "stop sharing").
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::Relaxed)
    }
}

impl Drop for ScreenShare {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
