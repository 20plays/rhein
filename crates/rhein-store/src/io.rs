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
    /// At most `WORKERS` threads: the work here is mostly waiting on sockets, and what blocks
    /// (decoding sound, opening devices, screen capture) goes to tokio's blocking threads. One
    /// per core, as tokio starts by default, was dozens of idle threads on a large machine.
    pub fn new() -> Result<Self> {
        const WORKERS: usize = 4;
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cores.clamp(2, WORKERS))
            .thread_name("rhein-io")
            .enable_all()
            .build()?;
        Ok(Self(Arc::new(runtime)))
    }

    pub fn global(cx: &App) -> Self {
        cx.global::<Self>().clone()
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.0.handle().clone()
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
