use std::sync::Arc;

use anyhow::Result;
use gpui::{App, Global};
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

/// The tokio runtime all network work runs on. GPUI has its own executor; sockets and `reqwest`
/// need tokio, so views and entities hand futures to `Io::spawn` and await the handle from
/// `cx.spawn`.
#[derive(Clone)]
pub struct Io(Arc<Runtime>);

impl Global for Io {}

impl Io {
    pub fn new() -> Result<Self> {
        Ok(Self(Arc::new(Runtime::new()?)))
    }

    pub fn global(cx: &App) -> Self {
        cx.global::<Self>().clone()
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.0.spawn(future)
    }
}

/// Flattens a spawned fallible task into one `Result`.
pub async fn join<T>(handle: JoinHandle<Result<T>>) -> Result<T> {
    handle.await?
}
