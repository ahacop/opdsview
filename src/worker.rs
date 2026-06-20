//! Background network worker.
//!
//! All HTTP I/O and image decoding happens on a dedicated thread so the UI
//! event loop never blocks. Requests and responses are exchanged over channels.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use image::DynamicImage;

use crate::cache::Cache;
use crate::opds::Feed;
use crate::storage::download_dir;

/// How long a cached feed response is considered fresh.
const FEED_TTL: Duration = Duration::from_secs(15 * 60);

type Auth = Option<(String, String)>;

/// A request sent from the UI thread to the worker.
pub enum Request {
    /// Fetch and parse an OPDS feed.
    Feed { url: String, auth: Auth },
    /// Fetch and decode a cover image, keyed by its URL.
    Image { url: String, auth: Auth },
    /// Download a book to disk, keyed by its acquisition URL.
    Download { url: String, auth: Auth },
    /// Run an OpenSearch query: resolve the description at `desc_url`, then
    /// fetch the resulting acquisition feed.
    Search { desc_url: String, query: String, auth: Auth },
}

/// A response delivered from the worker back to the UI thread.
pub enum Response {
    Feed { url: String, result: Result<Feed> },
    Image { url: String, result: Result<DynamicImage> },
    Download { url: String, result: Result<PathBuf> },
    /// A search result: the resolved feed URL and its parsed feed.
    Search { query: String, result: Result<(String, Feed)> },
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
                    Request::Download { url, auth } => {
                        let result = download_book(&http, &url, &auth);
                        let _ = resp_tx.send(Response::Download { url, result });
                    }
                    Request::Search { desc_url, query, auth } => {
                        let result = search(&http, &cache, &desc_url, &query, &auth);
                        let _ = resp_tx.send(Response::Search { query, result });
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

/// Resolve an OpenSearch description and fetch the matching acquisition feed.
///
/// Returns the resolved feed URL alongside the parsed feed so the UI can use it
/// as the current location (for pagination and back-navigation).
fn search(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    desc_url: &str,
    query: &str,
    auth: &Auth,
) -> Result<(String, Feed)> {
    let bytes = get_bytes(http, desc_url, auth)?;
    let desc = String::from_utf8_lossy(&bytes);
    let template = crate::opds::opensearch_template(&desc)
        .with_context(|| format!("no usable search template in {desc_url}"))?;
    let url = crate::opds::build_search_url(&template, query);
    let feed = fetch_feed(http, cache, &url, auth)?;
    Ok((url, feed))
}

/// Download a book to the user's downloads directory and return its path.
fn download_book(http: &reqwest::blocking::Client, url: &str, auth: &Auth) -> Result<PathBuf> {
    let dir = download_dir().context("resolving downloads directory")?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating downloads directory {}", dir.display()))?;
    let bytes = get_bytes(http, url, auth)?;
    let path = dir.join(filename_from_url(url));
    std::fs::write(&path, &bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Derive a sensible filename from a download URL's last path segment.
fn filename_from_url(url: &str) -> String {
    let name = url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .map(|s| s.to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".to_string());
    // Strip any leftover query fragment and percent-decode nothing fancy.
    name.split(['?', '#']).next().unwrap_or(&name).to_string()
}

#[cfg(test)]
mod tests {
    use super::filename_from_url;

    #[test]
    fn derives_filename_from_url() {
        assert_eq!(
            filename_from_url("https://se.org/a/b/cather_house.epub?source=feed"),
            "cather_house.epub"
        );
        assert_eq!(filename_from_url("https://se.org/x.azw3"), "x.azw3");
        // A URL with no usable last segment falls back to a default.
        assert_eq!(filename_from_url("https://se.org/"), "download");
    }
}
