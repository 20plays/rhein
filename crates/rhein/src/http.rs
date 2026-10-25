//! GPUI's HTTP client, backed by reqwest on the tokio runtime.
//!
//! GPUI fetches `img(url)` sources through whatever `HttpClient` the app installs. Requests run on
//! the `Io` runtime (`handle.spawn`) because reqwest needs tokio, while GPUI's executor only
//! awaits the join handle. Bodies are buffered: images are small and GPUI decodes whole files.
//!
//! The bodies of pictures fetched lately are kept, still encoded: decoded pictures are dropped
//! once off screen for a while (`rhein_ui::images`), and scrolling back to them should only
//! decode them again, not fetch them.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io::Read as _;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use gpui::http_client::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use gpui::http_client::{AsyncBody, HttpClient, Inner, Request, Response, Url};
use tokio::runtime::Handle;

type Sent = Pin<Box<dyn Future<Output = Result<Response<AsyncBody>>> + Send>>;

/// How many bytes of encoded pictures are kept, and the largest one kept.
const KEEP: usize = 64 << 20;
const KEEP_ONE: usize = 8 << 20;

pub struct Client {
    inner: reqwest::Client,
    handle: Handle,
    user_agent: HeaderValue,
    kept: Arc<Mutex<Kept>>,
}

/// Picture bodies by URL, the oldest dropped first.
#[derive(Default)]
struct Kept {
    bodies: HashMap<String, (HeaderMap, bytes::Bytes)>,
    order: VecDeque<String>,
    bytes: usize,
}

impl Kept {
    fn get(&mut self, url: &str) -> Option<(HeaderMap, bytes::Bytes)> {
        let hit = self.bodies.get(url).cloned()?;
        // Used again: the newest.
        if let Some(at) = self.order.iter().position(|u| u == url) {
            let url = self.order.remove(at).unwrap_or_default();
            self.order.push_back(url);
        }
        Some(hit)
    }

    fn put(&mut self, url: String, headers: HeaderMap, body: bytes::Bytes) {
        if body.len() > KEEP_ONE || self.bodies.contains_key(&url) {
            return;
        }
        self.bytes += body.len();
        self.bodies.insert(url.clone(), (headers, body));
        self.order.push_back(url);
        while self.bytes > KEEP {
            let Some(old) = self.order.pop_front() else { break };
            if let Some((_, body)) = self.bodies.remove(&old) {
                self.bytes -= body.len();
            }
        }
    }
}

fn is_picture(headers: &HeaderMap) -> bool {
    headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("image/"))
}

impl Client {
    pub fn new(handle: Handle) -> Self {
        let _guard = handle.enter();
        let agent = concat!("Rhein/", env!("CARGO_PKG_VERSION"));
        let inner = reqwest::Client::builder().user_agent(agent).build().expect("http client");
        Self { inner, handle, user_agent: HeaderValue::from_static(agent), kept: Arc::default() }
    }
}

impl HttpClient for Client {
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(&self, request: Request<AsyncBody>) -> Sent {
        let client = self.inner.clone();
        let handle = self.handle.clone();
        let kept = self.kept.clone();
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = read(body)?;
            let url = (parts.method == Method::GET).then(|| parts.uri.to_string());
            if let Some((headers, bytes)) = url.as_deref().and_then(|u| kept.lock().ok()?.get(u)) {
                let mut response = Response::builder().status(StatusCode::OK);
                if let Some(map) = response.headers_mut() {
                    *map = headers;
                }
                return Ok(response.body(AsyncBody::from(bytes.to_vec()))?);
            }
            let fetch = handle.spawn(async move {
                let mut outgoing = client.request(parts.method, parts.uri.to_string()).headers(parts.headers);
                if let Some(body) = body {
                    outgoing = outgoing.body(body);
                }
                let incoming = outgoing.send().await?;
                let status = incoming.status();
                let headers = incoming.headers().clone();
                let bytes = incoming.bytes().await?;
                Ok::<_, anyhow::Error>((status, headers, bytes))
            });
            let (status, headers, bytes) = fetch.await??;
            if let Some(url) = url.filter(|_| status.is_success() && is_picture(&headers))
                && let Ok(mut kept) = kept.lock()
            {
                kept.put(url, headers.clone(), bytes.clone());
            }
            let mut response = Response::builder().status(status);
            if let Some(map) = response.headers_mut() {
                *map = headers;
            }
            Ok(response.body(AsyncBody::from(bytes.to_vec()))?)
        })
    }
}

/// A buffered body's bytes (this client buffers every response).
pub(crate) fn read(body: AsyncBody) -> Result<Option<Vec<u8>>> {
    match body.0 {
        Inner::Empty => Ok(None),
        Inner::Bytes(mut cursor) => {
            let mut bytes = Vec::new();
            cursor.read_to_end(&mut bytes)?;
            Ok(Some(bytes))
        }
        Inner::AsyncReader(_) => Err(anyhow!("streaming request bodies are not supported")),
    }
}
