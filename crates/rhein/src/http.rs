//! GPUI's HTTP client, backed by reqwest on the tokio runtime.
//!
//! GPUI fetches `img(url)` sources through whatever `HttpClient` the app installs. Requests run on
//! the `Io` runtime (`handle.spawn`) because reqwest needs tokio, while GPUI's executor only
//! awaits the join handle. Bodies are buffered: images are small and GPUI decodes whole files.

use std::future::Future;
use std::io::Read as _;
use std::pin::Pin;

use anyhow::{Result, anyhow};
use gpui::http_client::http::HeaderValue;
use gpui::http_client::{AsyncBody, HttpClient, Inner, Request, Response, Url};
use tokio::runtime::Handle;

type Sent = Pin<Box<dyn Future<Output = Result<Response<AsyncBody>>> + Send>>;

pub struct Client {
    inner: reqwest::Client,
    handle: Handle,
    user_agent: HeaderValue,
}

impl Client {
    pub fn new(handle: Handle) -> Self {
        let _guard = handle.enter();
        let agent = concat!("Rhein/", env!("CARGO_PKG_VERSION"));
        let inner = reqwest::Client::builder().user_agent(agent).build().expect("http client");
        Self { inner, handle, user_agent: HeaderValue::from_static(agent) }
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
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            let body = read(body)?;
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
            let mut response = Response::builder().status(status);
            if let Some(map) = response.headers_mut() {
                *map = headers;
            }
            Ok(response.body(AsyncBody::from(bytes.to_vec()))?)
        })
    }
}

fn read(body: AsyncBody) -> Result<Option<Vec<u8>>> {
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
