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
use crate::reading::{extract_reading_text, ReadingStats};
use crate::storage::{self, LibraryBook, LibraryEntry};

/// How long a cached feed response is considered fresh.
const FEED_TTL: Duration = Duration::from_secs(15 * 60);

type Auth = Option<(String, String)>;

/// A request sent from the UI thread to the worker.
pub enum Request {
    /// Fetch and parse an OPDS feed.
    Feed { url: String, auth: Auth },
    /// Fetch and decode a cover image, keyed by its URL.
    Image { url: String, auth: Auth },
    /// Download a book to the library, persisting a metadata sidecar. `url` is
    /// the chosen acquisition link (and the key the UI tracks progress under).
    Download {
        meta: Box<LibraryEntry>,
        url: String,
        mime: String,
        length: Option<u64>,
        cover_url: Option<String>,
        auth: Auth,
    },
    /// Load all downloaded books from the local library.
    Library,
    /// Run an OpenSearch query: resolve the description at `desc_url`, then
    /// fetch the resulting acquisition feed.
    Search { desc_url: String, query: String, auth: Auth },
    /// Scrape supplementary reading metrics from a publication's web page,
    /// keyed by that page's URL.
    Reading { url: String, auth: Auth },
}

/// A response delivered from the worker back to the UI thread.
pub enum Response {
    Feed { url: String, result: Result<Feed> },
    Image { url: String, result: Result<DynamicImage> },
    Download { url: String, result: Result<PathBuf> },
    Library { result: Result<Vec<LibraryBook>> },
    /// A search result: the resolved feed URL and its parsed feed.
    Search { query: String, result: Result<(String, Feed)> },
    Reading { url: String, result: Result<ReadingStats> },
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
                    Request::Download { meta, url, mime, length, cover_url, auth } => {
                        let result = download_book(
                            &http, &cache, *meta, &url, &mime, length, cover_url.as_deref(), &auth,
                        );
                        let _ = resp_tx.send(Response::Download { url, result });
                    }
                    Request::Library => {
                        let result = storage::load_library();
                        let _ = resp_tx.send(Response::Library { result });
                    }
                    Request::Search { desc_url, query, auth } => {
                        let result = search(&http, &cache, &desc_url, &query, &auth);
                        let _ = resp_tx.send(Response::Search { query, result });
                    }
                    Request::Reading { url, auth } => {
                        let result = fetch_reading(&http, &cache, &url, &auth);
                        let _ = resp_tx.send(Response::Reading { url, result });
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
    let bytes = image_bytes(http, cache, url, auth)?;
    let img = image::load_from_memory(&bytes)
        .with_context(|| format!("decoding image {url}"))?;
    Ok(img)
}

/// Whether a link points at a local file rather than a remote resource.
///
/// Library entries carry absolute filesystem paths as their cover/format
/// `href`s; everything fetched over the network is `http(s)`.
fn is_local_path(url: &str) -> bool {
    !url.starts_with("http://") && !url.starts_with("https://")
}

/// Load image bytes for a cover, from a local file, the cache, or the network.
///
/// Images are immutable content, so network fetches are cached indefinitely.
fn image_bytes(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    url: &str,
    auth: &Auth,
) -> Result<Vec<u8>> {
    if is_local_path(url) {
        return std::fs::read(url).with_context(|| format!("reading cover {url}"));
    }
    if let Some(bytes) = cache.get(url, "img", None) {
        return Ok(bytes);
    }
    let bytes = get_bytes(http, url, auth)?;
    let _ = cache.put(url, "img", &bytes);
    Ok(bytes)
}

/// Fetch a publication's web page and parse its reading metrics.
///
/// Only the extracted one-line sentence is cached, keyed by the page URL.
/// These metrics never meaningfully change, so the cache is kept indefinitely
/// (like cover images); re-parsing the cached sentence is cheap.
fn fetch_reading(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    url: &str,
    auth: &Auth,
) -> Result<ReadingStats> {
    let sentence = match cache.get(url, "read", None) {
        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        None => {
            let bytes = get_bytes(http, url, auth)?;
            let html = String::from_utf8_lossy(&bytes);
            let text = extract_reading_text(&html)
                .with_context(|| format!("no reading-ease info on {url}"))?;
            let _ = cache.put(url, "read", text.as_bytes());
            text
        }
    };
    ReadingStats::parse(&sentence)
        .with_context(|| format!("unrecognized reading-ease text from {url}"))
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

/// Download a book into the local library and return the saved ebook path.
///
/// Fetches the ebook and, the first time a book is saved, its cover (reusing
/// any cached cover bytes from browsing). [`storage::save_book`] writes the
/// files and the metadata sidecar.
#[allow(clippy::too_many_arguments)]
fn download_book(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    meta: LibraryEntry,
    url: &str,
    mime: &str,
    length: Option<u64>,
    cover_url: Option<&str>,
    auth: &Auth,
) -> Result<PathBuf> {
    let bytes = get_bytes(http, url, auth)?;
    let cover_bytes = cover_url.and_then(|u| image_bytes(http, cache, u, auth).ok());
    storage::save_book(meta, mime, length, url, &bytes, cover_bytes.as_deref())
}
