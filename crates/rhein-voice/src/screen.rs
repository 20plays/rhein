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

/// Why a share did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareError {
    /// The desktop has no screen capture portal: worth telling the person how to get one.
    NoPortal,
    /// The person closed the picker (or the desktop said no).
    Cancelled,
    Other(String),
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::NoPortal => f.write_str(
                "Screen sharing needs a desktop portal that can capture the screen, and none is running. \
                 On sway and other wlroots compositors install xdg-desktop-portal-wlr, then restart xdg-desktop-portal \
                 (or log in again); elsewhere, the portal package of your desktop.",
            ),
            ShareError::Cancelled => f.write_str("The screen share was cancelled."),
            ShareError::Other(err) => f.write_str(err),
        }
    }
}

impl std::error::Error for ShareError {}

/// Whether the session's desktop portal offers screen capture at all (asking it for the
/// ScreenCast interface's version; a portal without a backend for it has no such interface).
pub fn portal_ready() -> Result<(), ShareError> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    let connection = zbus::blocking::Connection::session().map_err(|_| ShareError::NoPortal)?;
    let proxy = zbus::blocking::Proxy::new(&connection, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.ScreenCast")
        .map_err(|_| ShareError::NoPortal)?;
    proxy.get_property::<u32>("version").map(|_| ()).map_err(|_| ShareError::NoPortal)
}

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
    pub fn start(fps: u32, cursor: bool) -> Result<ScreenShare, ShareError> {
        portal_ready()?;
        Self::capture(fps, cursor).map_err(|err| match err.downcast_ref::<ShareError>() {
            Some(share) => share.clone(),
            None => ShareError::Other(format!("{err:#}")),
        })
    }

    fn capture(fps: u32, cursor: bool) -> Result<ScreenShare> {
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
                        // The portal was there (checked above): this is the picker closed.
                        let _ = first.send(Err(ShareError::Cancelled.into()));
                    }
                });
                let every = Duration::from_millis(1000 / fps.clamp(1, 60) as u64);
                while !running.load(Ordering::Relaxed) && !over.load(Ordering::Relaxed) {
                    capturer.capture_frame();
                    std::thread::sleep(every);
                }
            })?;
        // The picker waits for the person; give them time.
        let out = first_rx.recv_timeout(Duration::from_secs(120)).map_err(|_| ShareError::Cancelled)??;
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
