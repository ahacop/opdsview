//! Background network worker.
//!
//! All HTTP I/O and image decoding happens on a dedicated thread so the UI
//! event loop never blocks. Requests and responses are exchanged over channels.

use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use image::DynamicImage;

use crate::cache::Cache;
use crate::opds::Feed;

/// How long a cached feed response is considered fresh.
const FEED_TTL: Duration = Duration::from_secs(15 * 60);

type Auth = Option<(String, String)>;

/// A request sent from the UI thread to the worker.
pub enum Request {
    /// Fetch and parse an OPDS feed.
    Feed { url: String, auth: Auth },
    /// Fetch and decode a cover image, keyed by its URL.
    Image { url: String, auth: Auth },
}

/// A response delivered from the worker back to the UI thread.
pub enum Response {
    Feed { url: String, result: Result<Feed> },
    Image { url: String, result: Result<DynamicImage> },
}

/// Handle to the worker thread.
pub struct Worker {
    tx: Sender<Request>,
    pub rx: Receiver<Response>,
}

impl Worker {
    pub fn spawn(cache: Cache) -> Result<Self> {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<Response>();

        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("opdsview/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .context("building HTTP client")?;

        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                match req {
                    Request::Feed { url, auth } => {
                        let result = fetch_feed(&http, &cache, &url, &auth);
                        let _ = resp_tx.send(Response::Feed { url, result });
                    }
                    Request::Image { url, auth } => {
                        let result = fetch_image(&http, &cache, &url, &auth);
                        let _ = resp_tx.send(Response::Image { url, result });
                    }
                }
            }
        });

        Ok(Self { tx: req_tx, rx: resp_rx })
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

fn get_bytes(
    http: &reqwest::blocking::Client,
    url: &str,
    auth: &Auth,
) -> Result<Vec<u8>> {
    let mut req = http.get(url);
    if let Some((user, pass)) = auth {
        req = req.basic_auth(user, Some(pass));
    }
    let resp = req.send().with_context(|| format!("requesting {url}"))?;
    let resp = resp
        .error_for_status()
        .with_context(|| format!("server returned an error for {url}"))?;
    Ok(resp.bytes()?.to_vec())
}

fn fetch_feed(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    url: &str,
    auth: &Auth,
) -> Result<Feed> {
    if let Some(bytes) = cache.get(url, "xml", Some(FEED_TTL))
        && let Ok(text) = String::from_utf8(bytes)
            && let Ok(feed) = Feed::parse(&text, url) {
                return Ok(feed);
            }
    let bytes = get_bytes(http, url, auth)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let feed = Feed::parse(&text, url)?;
    let _ = cache.put(url, "xml", text.as_bytes());
    Ok(feed)
}

fn fetch_image(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    url: &str,
    auth: &Auth,
) -> Result<DynamicImage> {
    // Images are immutable content; cache them indefinitely.
    let bytes = match cache.get(url, "img", None) {
        Some(bytes) => bytes,
        None => {
            let bytes = get_bytes(http, url, auth)?;
            let _ = cache.put(url, "img", &bytes);
            bytes
        }
    };
    let img = image::load_from_memory(&bytes)
        .with_context(|| format!("decoding image {url}"))?;
    Ok(img)
}
