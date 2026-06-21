//! Background network worker.
//!
//! All HTTP I/O and image decoding happens on a dedicated thread so the UI
//! event loop never blocks. Requests and responses are exchanged over channels.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};
use image::DynamicImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::cache::Cache;
use crate::opds::Feed;
use crate::reader::{BookContent, TocEntry, resolve_href};
use crate::reading::{ReadingStats, extract_reading_text};
use crate::storage::{self, LibraryBook, LibraryEntry, ReadingProgress};

/// How long a cached feed response is considered fresh.
const FEED_TTL: Duration = Duration::from_secs(15 * 60);

/// Number of cover images fetched and decoded concurrently. Covers are fetched
/// lazily — the selected entry's in the browser, a chapter's inline images in
/// the reader — so only a handful are ever in flight at once; a small pool
/// overlaps their network latency while keeping the shared HTTP client's
/// connections alive and reused.
const IMAGE_WORKERS: usize = 6;

type Auth = Option<(String, String)>;

/// Where a download should be written.
pub enum DownloadDest {
    /// The opdsview library (with cover + metadata sidecar); the readable copy.
    Library,
    /// A plain file in the user's Downloads directory.
    Downloads,
    /// Imported into Calibre via `command add`, optionally `--library-path` and
    /// `--automerge`.
    Calibre {
        command: String,
        library_path: Option<String>,
        automerge: Option<String>,
    },
}

/// A destination tag echoed back with a download response, so the UI can phrase
/// completion (and refresh "downloaded" markers only for library saves) without
/// re-deriving where the file went.
#[derive(Clone, Copy)]
pub enum DownloadKind {
    Library,
    Downloads,
    Calibre,
}

impl DownloadDest {
    fn kind(&self) -> DownloadKind {
        match self {
            DownloadDest::Library => DownloadKind::Library,
            DownloadDest::Downloads => DownloadKind::Downloads,
            DownloadDest::Calibre { .. } => DownloadKind::Calibre,
        }
    }
}

/// A request sent from the UI thread to the worker.
pub enum Request {
    /// Fetch and parse an OPDS feed.
    Feed { url: String, auth: Auth },
    /// Fetch and decode a cover image, keyed by its URL.
    Image { url: String, auth: Auth },
    /// Download a book to the chosen destination. `url` is the acquisition link
    /// (and the key the UI tracks progress under). Only [`DownloadDest::Library`]
    /// writes a metadata sidecar and cover.
    Download {
        meta: Box<LibraryEntry>,
        url: String,
        mime: String,
        /// The feed's title for this acquisition link, kept with a library save
        /// so two same-format variants of a book can be told apart.
        title: String,
        length: Option<u64>,
        cover_url: Option<String>,
        auth: Auth,
        dest: DownloadDest,
    },
    /// Load all downloaded books from the local library.
    Library,
    /// Index the library's books by id → their formats' source URLs, for the
    /// catalog's book-level and per-format "downloaded" markers.
    LibraryFormats,
    /// Query a Calibre library (via `calibredb list`) for the set of match keys
    /// used to mark catalog books already present in Calibre.
    CalibreIds {
        command: String,
        library_path: Option<String>,
    },
    /// Run an OpenSearch query: resolve the description at `desc_url`, then
    /// fetch the resulting acquisition feed.
    Search {
        desc_url: String,
        query: String,
        auth: Auth,
    },
    /// Scrape supplementary reading metrics from a publication's web page,
    /// keyed by that page's URL.
    Reading { url: String, auth: Auth },
    /// Open and extract a downloaded EPUB for the built-in reader.
    OpenBook { path: PathBuf },
    /// Decode an image embedded in a book chapter, keyed by `key` (the value the
    /// UI tracks it under in its image cache). Resolved relative to `chapter`.
    BookImage {
        path: PathBuf,
        chapter: usize,
        src: String,
        key: String,
    },
    /// Persist the reader's position for a downloaded book (fire-and-forget).
    SaveProgress {
        id: String,
        progress: ReadingProgress,
    },
}

/// A response delivered from the worker back to the UI thread.
pub enum Response {
    Feed {
        url: String,
        result: Result<Feed>,
    },
    /// A fetched+decoded cover, already wrapped in a resize protocol (built on
    /// the worker, not the UI thread). Keyed by image URL (or local file path).
    Image {
        url: String,
        result: Result<StatefulProtocol>,
    },
    Download {
        url: String,
        kind: DownloadKind,
        result: Result<PathBuf>,
    },
    Library {
        result: Result<Vec<LibraryBook>>,
    },
    /// The library indexed by book id → its formats' source acquisition URLs.
    LibraryFormats {
        result: Result<HashMap<String, HashSet<String>>>,
    },
    /// The Calibre-library match keys (see [`Request::CalibreIds`]).
    CalibreIds {
        result: Result<HashSet<String>>,
    },
    /// A search result: the resolved feed URL and its parsed feed.
    Search {
        query: String,
        result: Result<(String, Feed)>,
    },
    Reading {
        url: String,
        result: Result<ReadingStats>,
    },
    /// An extracted EPUB ready for the reader.
    Book {
        result: Result<BookContent>,
    },
}

/// Handle to the worker thread.
pub struct Worker {
    tx: Sender<Request>,
}

impl Worker {
    /// Spawn the worker threads and return the handle alongside the channel on
    /// which responses arrive. The caller owns the [`Receiver`] so it can fold
    /// responses into its own event source (the UI forwards them into a unified
    /// channel it blocks on, so a completed fetch/encode wakes the loop at once).
    pub fn spawn(cache: Cache, picker: Picker) -> Result<(Self, Receiver<Response>)> {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (resp_tx, resp_rx) = std::sync::mpsc::channel::<Response>();

        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("opdsview/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()
            .context("building HTTP client")?;

        // The cover-fetch pool runs off the UI thread and reports on `resp_tx`:
        // it fetches+decodes covers and builds their resize protocols
        // (network-bound). The dispatcher below forwards every `Request::Image`
        // to it. The `picker` builds protocols; cloned where needed.
        let img_tx = spawn_image_pool(http.clone(), cache.clone(), picker.clone(), resp_tx.clone());

        thread::spawn(move || {
            while let Ok(req) = req_rx.recv() {
                match req {
                    Request::Feed { url, auth } => {
                        let result = fetch_feed(&http, &cache, &url, &auth);
                        let _ = resp_tx.send(Response::Feed { url, result });
                    }
                    Request::Image { url, auth } => {
                        // Hand off to the image pool so a burst of prefetch
                        // requests downloads concurrently instead of blocking
                        // the dispatcher (and every other request) one by one.
                        let _ = img_tx.send((url, auth));
                    }
                    Request::Download {
                        meta,
                        url,
                        mime,
                        title,
                        length,
                        cover_url,
                        auth,
                        dest,
                    } => {
                        let kind = dest.kind();
                        let result = download_book(
                            &http,
                            &cache,
                            *meta,
                            &url,
                            &mime,
                            &title,
                            length,
                            cover_url.as_deref(),
                            &auth,
                            dest,
                        );
                        let _ = resp_tx.send(Response::Download { url, kind, result });
                    }
                    Request::Library => {
                        let result = storage::load_library();
                        let _ = resp_tx.send(Response::Library { result });
                    }
                    Request::LibraryFormats => {
                        let result = storage::library_format_sources();
                        let _ = resp_tx.send(Response::LibraryFormats { result });
                    }
                    Request::CalibreIds {
                        command,
                        library_path,
                    } => {
                        let result = storage::calibre_index(&command, library_path.as_deref());
                        let _ = resp_tx.send(Response::CalibreIds { result });
                    }
                    Request::Search {
                        desc_url,
                        query,
                        auth,
                    } => {
                        let result = search(&http, &cache, &desc_url, &query, &auth);
                        let _ = resp_tx.send(Response::Search { query, result });
                    }
                    Request::Reading { url, auth } => {
                        let result = fetch_reading(&http, &cache, &url, &auth);
                        let _ = resp_tx.send(Response::Reading { url, result });
                    }
                    Request::OpenBook { path } => {
                        let result = open_book(&path);
                        let _ = resp_tx.send(Response::Book { result });
                    }
                    Request::BookImage {
                        path,
                        chapter,
                        src,
                        key,
                    } => {
                        // Decode and build the protocol here; the UI thread only
                        // wraps and renders it, and the resize pool encodes it.
                        let result = fetch_book_image(&path, chapter, &src)
                            .map(|img| picker.new_resize_protocol(img));
                        let _ = resp_tx.send(Response::Image { url: key, result });
                    }
                    Request::SaveProgress { id, progress } => {
                        let _ = storage::save_progress(&id, progress);
                    }
                }
            }
        });

        Ok((Self { tx: req_tx }, resp_rx))
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

/// Spawn the [`IMAGE_WORKERS`]-strong cover-fetch pool and return the sender the
/// dispatcher uses to feed it `(url, auth)` jobs.
///
/// The workers share one job receiver behind a mutex; each holds the lock only
/// long enough to take the next job, then releases it before the slow
/// fetch+decode, so the others keep working in parallel. Each fetched cover is
/// decoded and wrapped in a resize protocol (via the cloned `picker`) right
/// here, off the UI thread, and sent back on `resp_tx` as a [`Response::Image`].
fn spawn_image_pool(
    http: reqwest::blocking::Client,
    cache: Cache,
    picker: Picker,
    resp_tx: Sender<Response>,
) -> Sender<(String, Auth)> {
    let (job_tx, job_rx) = std::sync::mpsc::channel::<(String, Auth)>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    for _ in 0..IMAGE_WORKERS {
        let http = http.clone();
        let cache = cache.clone();
        let picker = picker.clone();
        let resp_tx = resp_tx.clone();
        let job_rx = Arc::clone(&job_rx);
        thread::spawn(move || {
            // The lock guard is dropped at the end of this block expression, so
            // the fetch below runs without holding it.
            while let Ok((url, auth)) = {
                let rx = job_rx.lock().unwrap();
                rx.recv()
            } {
                let result = fetch_image(&http, &cache, &url, &auth)
                    .map(|img| picker.new_resize_protocol(img));
                let _ = resp_tx.send(Response::Image { url, result });
            }
        });
    }
    job_tx
}

/// How many times a rate-limited request is retried before giving up.
const MAX_RETRIES: u32 = 4;

fn get_bytes(http: &reqwest::blocking::Client, url: &str, auth: &Auth) -> Result<Vec<u8>> {
    let mut attempt = 0;
    loop {
        let mut req = http.get(url);
        if let Some((user, pass)) = auth {
            req = req.basic_auth(user, Some(pass));
        }
        let resp = req.send().with_context(|| format!("requesting {url}"))?;
        // Back off and retry on rate-limit / temporary-unavailable responses.
        // Prefetching a page of covers fires a burst of concurrent requests,
        // which a catalog may answer with 429; honoring Retry-After (and
        // backing off otherwise) keeps us a well-behaved client instead of
        // hammering the server and getting every cover rejected.
        if is_retryable(resp.status()) && attempt < MAX_RETRIES {
            let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt, url));
            attempt += 1;
            thread::sleep(wait);
            continue;
        }
        let resp = resp
            .error_for_status()
            .with_context(|| format!("server returned an error for {url}"))?;
        return Ok(resp.bytes()?.to_vec());
    }
}

/// Whether a status means "slow down / try again shortly" rather than a hard
/// failure: `429 Too Many Requests` or `503 Service Unavailable`.
fn is_retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
}

/// The server-requested wait from a `Retry-After` header, when given as an
/// integer number of seconds (clamped to 30s). The HTTP-date form is ignored —
/// we fall back to [`backoff`] — which keeps this dependency-free; rate limiters
/// almost always use the seconds form.
fn retry_after(resp: &reqwest::blocking::Response) -> Option<Duration> {
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs.min(30)))
}

/// Exponential backoff before a retry (500ms, 1s, 2s, …, capped at 8s) plus a
/// small deterministic per-URL jitter, so a pool that all trips the limit at the
/// same instant doesn't retry in lockstep.
fn backoff(attempt: u32, url: &str) -> Duration {
    let base = Duration::from_millis(500)
        .saturating_mul(1u32 << attempt.min(4))
        .min(Duration::from_secs(8));
    let jitter = Duration::from_millis((url_hash(url) % 250) as u64);
    base + jitter
}

/// A cheap, stable hash of a URL, used only to spread out retry jitter.
fn url_hash(url: &str) -> u32 {
    url.bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
}

fn fetch_feed(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    url: &str,
    auth: &Auth,
) -> Result<Feed> {
    if let Some(bytes) = cache.get(url, "xml", Some(FEED_TTL))
        && let Ok(text) = String::from_utf8(bytes)
        && let Ok(feed) = Feed::parse(&text, url)
    {
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
    let img = image::load_from_memory(&bytes).with_context(|| format!("decoding image {url}"))?;
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

/// Open an EPUB and extract every spine document's XHTML plus the title and a
/// flattened table of contents, for the built-in reader.
fn open_book(path: &Path) -> Result<BookContent> {
    let mut doc = EpubDoc::new(path).with_context(|| format!("opening epub {}", path.display()))?;
    let title = doc.get_title().unwrap_or_else(|| "Untitled".to_string());

    let count = doc.get_num_chapters();
    let mut chapters = Vec::with_capacity(count);
    for i in 0..count {
        doc.set_current_chapter(i);
        let html = doc
            .get_current_str()
            .map(|(html, _)| html)
            .unwrap_or_default();
        chapters.push(html);
    }

    let nav = doc.toc.clone();
    let mut toc = Vec::new();
    flatten_toc(&doc, &nav, &mut toc);

    Ok(BookContent {
        title,
        chapters,
        toc,
    })
}

/// Walk the TOC tree depth-first, resolving each navigation point's target
/// document to its spine index. Points whose target isn't in the spine (or that
/// can't be resolved) are skipped.
fn flatten_toc<R: std::io::Read + std::io::Seek>(
    doc: &EpubDoc<R>,
    points: &[NavPoint],
    out: &mut Vec<TocEntry>,
) {
    for point in points {
        // `content` may carry a `#fragment`; match on the document path alone.
        let content = point
            .content
            .to_str()
            .map(|s| PathBuf::from(s.split('#').next().unwrap_or(s)))
            .unwrap_or_else(|| point.content.clone());
        if let Some(chapter) = doc.resource_uri_to_chapter(&content) {
            out.push(TocEntry {
                label: point.label.clone(),
                chapter,
            });
        }
        flatten_toc(doc, &point.children, out);
    }
}

/// Resolve and decode an image referenced from a book chapter.
fn fetch_book_image(path: &Path, chapter: usize, src: &str) -> Result<DynamicImage> {
    let mut doc = EpubDoc::new(path).with_context(|| format!("opening epub {}", path.display()))?;
    doc.set_current_chapter(chapter);
    let chapter_path = doc
        .get_current_path()
        .ok_or_else(|| anyhow::anyhow!("no current chapter path"))?;
    let resolved = resolve_href(&chapter_path, src);
    let bytes = doc
        .get_resource_by_path(&resolved)
        .with_context(|| format!("image {} not found in epub", resolved.display()))?;
    image::load_from_memory(&bytes).with_context(|| format!("decoding image {src}"))
}

/// Download a book to the chosen destination and return the saved file's path.
///
/// [`DownloadDest::Library`] saves into the opdsview library with its cover
/// (reusing any cached cover bytes from browsing) and a metadata sidecar;
/// [`DownloadDest::Downloads`] writes a plain copy to the user's Downloads
/// directory; [`DownloadDest::Calibre`] imports it via `calibredb add`.
#[allow(clippy::too_many_arguments)]
fn download_book(
    http: &reqwest::blocking::Client,
    cache: &Cache,
    meta: LibraryEntry,
    url: &str,
    mime: &str,
    title: &str,
    length: Option<u64>,
    cover_url: Option<&str>,
    auth: &Auth,
    dest: DownloadDest,
) -> Result<PathBuf> {
    let bytes = get_bytes(http, url, auth)?;
    match dest {
        DownloadDest::Library => {
            let cover_bytes = cover_url.and_then(|u| image_bytes(http, cache, u, auth).ok());
            storage::save_book(meta, mime, title, length, url, &bytes, cover_bytes.as_deref())
        }
        DownloadDest::Downloads => {
            storage::save_loose(&storage::downloads_dir()?, &meta, mime, url, &bytes)
        }
        DownloadDest::Calibre {
            command,
            library_path,
            automerge,
        } => storage::import_to_calibre(
            &command,
            library_path.as_deref(),
            automerge.as_deref(),
            &meta,
            mime,
            url,
            &bytes,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_statuses_are_only_429_and_503() {
        use reqwest::StatusCode;
        assert!(is_retryable(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable(StatusCode::OK));
        assert!(!is_retryable(StatusCode::NOT_FOUND));
        assert!(!is_retryable(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn backoff_grows_then_caps_and_stays_bounded() {
        let url = "https://example.org/cover.jpg";
        // Jitter is under 250ms, so compare against the exponential base.
        let ms = |a| backoff(a, url).as_millis();
        assert!(ms(0) >= 500 && ms(0) < 750);
        assert!(ms(1) >= 1000 && ms(1) < 1250);
        assert!(ms(2) >= 2000 && ms(2) < 2250);
        // Caps at 8s + jitter and never grows past it, however high the attempt.
        assert!(ms(4) >= 8000 && ms(4) < 8250);
        assert!(ms(99) < 8250);
    }

    #[test]
    fn url_hash_is_stable_and_varies_by_url() {
        assert_eq!(url_hash("https://a/x.jpg"), url_hash("https://a/x.jpg"));
        assert_ne!(url_hash("https://a/x.jpg"), url_hash("https://a/y.jpg"));
    }
}
