//! Background network worker.
//!
//! All HTTP I/O and image decoding happens on a dedicated thread so the UI
//! event loop never blocks. Requests and responses are exchanged over channels.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use epub::doc::{EpubDoc, NavPoint};
use image::DynamicImage;

use crate::cache::Cache;
use crate::opds::Feed;
use crate::reader::{BookContent, TocEntry, resolve_href};
use crate::reading::{ReadingStats, extract_reading_text};
use crate::storage::{self, LibraryBook, LibraryEntry, ReadingProgress};

/// How long a cached feed response is considered fresh.
const FEED_TTL: Duration = Duration::from_secs(15 * 60);

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
        length: Option<u64>,
        cover_url: Option<String>,
        auth: Auth,
        dest: DownloadDest,
    },
    /// Load all downloaded books from the local library.
    Library,
    /// List the ids of books in the library (for the catalog's "downloaded"
    /// markers) without parsing every sidecar.
    LibraryIds,
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
    Image {
        url: String,
        result: Result<DynamicImage>,
    },
    Download {
        url: String,
        kind: DownloadKind,
        result: Result<PathBuf>,
    },
    Library {
        result: Result<Vec<LibraryBook>>,
    },
    /// The ids of books currently in the library.
    LibraryIds {
        result: Result<Vec<String>>,
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
                    Request::Download {
                        meta,
                        url,
                        mime,
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
                    Request::LibraryIds => {
                        let result = storage::library_ids();
                        let _ = resp_tx.send(Response::LibraryIds { result });
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
                        let result = fetch_book_image(&path, chapter, &src);
                        let _ = resp_tx.send(Response::Image { url: key, result });
                    }
                    Request::SaveProgress { id, progress } => {
                        let _ = storage::save_progress(&id, progress);
                    }
                }
            }
        });

        Ok(Self {
            tx: req_tx,
            rx: resp_rx,
        })
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

fn get_bytes(http: &reqwest::blocking::Client, url: &str, auth: &Auth) -> Result<Vec<u8>> {
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
    length: Option<u64>,
    cover_url: Option<&str>,
    auth: &Auth,
    dest: DownloadDest,
) -> Result<PathBuf> {
    let bytes = get_bytes(http, url, auth)?;
    match dest {
        DownloadDest::Library => {
            let cover_bytes = cover_url.and_then(|u| image_bytes(http, cache, u, auth).ok());
            storage::save_book(meta, mime, length, url, &bytes, cover_bytes.as_deref())
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
