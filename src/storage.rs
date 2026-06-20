//! Persistence of the saved-feed list, the downloaded-book library, and
//! on-disk locations.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::opds::{Author, Category, Entry, Link, REL_ACQUISITION, REL_IMAGE};
use crate::reading::ReadingStats;

/// A saved OPDS catalog, including optional HTTP Basic Auth credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    pub id: u64,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl Feed {
    /// Basic-auth credentials as a `(user, pass)` pair, if a username is set.
    pub fn auth(&self) -> Option<(String, String)> {
        let user = self.username.clone().filter(|u| !u.is_empty())?;
        Some((user, self.password.clone().unwrap_or_default()))
    }
}

/// The persisted application configuration: the list of saved feeds.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub feeds: Vec<Feed>,
    #[serde(default)]
    next_id: u64,
}

impl Config {
    /// Load the config from disk, returning a default (empty) config if none exists.
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg = serde_json::from_str(&data)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    /// Atomically write the config to disk.
    pub fn save(&self) -> Result<()> {
        let path = config_file()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Add a new feed, assigning it a fresh id.
    pub fn add(&mut self, mut feed: Feed) {
        self.next_id += 1;
        feed.id = self.next_id;
        self.feeds.push(feed);
    }

    /// Replace an existing feed (matched by id) in place.
    pub fn update(&mut self, feed: Feed) {
        if let Some(slot) = self.feeds.iter_mut().find(|f| f.id == feed.id) {
            *slot = feed;
        }
    }

    /// Remove the feed with the given id.
    pub fn remove(&mut self, id: u64) {
        self.feeds.retain(|f| f.id != id);
    }
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "opdsview", "opdsview")
        .context("could not determine application directories")
}

fn config_file() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("feeds.json"))
}

/// Directory used for cached feed and image data.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// Directory where downloaded books and their metadata sidecars are saved.
pub fn library_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().join("library"))
}

// --- Downloaded-book library ---------------------------------------------

/// One downloaded format file backing a library book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryFile {
    pub mime: String,
    pub filename: String,
    #[serde(default)]
    pub length: Option<u64>,
}

/// A persisted record of one downloaded book: its catalog metadata plus the
/// local files (one or more formats, an optional cover) it was saved with.
///
/// Stored as a `<id>.json` sidecar in [`library_dir`]; the ebook and cover
/// files sit beside it under the same `<id>` stem.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    /// Author catalog-page URIs, parallel to `authors` (empty string where a
    /// URI was unknown). Absent in sidecars written before author links existed.
    #[serde(default)]
    pub author_uris: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    /// `(term, scheme)` pairs mirroring [`crate::opds::Category`].
    #[serde(default)]
    pub categories: Vec<(String, Option<String>)>,
    /// The publication's web page, when known (used for reading metrics).
    #[serde(default)]
    pub web_url: Option<String>,
    #[serde(default)]
    pub reading: Option<ReadingStats>,
    /// Filename of the saved cover image, relative to [`library_dir`].
    #[serde(default)]
    pub cover_file: Option<String>,
    #[serde(default)]
    pub files: Vec<LibraryFile>,
}

impl LibraryEntry {
    /// Build a metadata record from a catalog entry. `cover_file` and `files`
    /// are filled in later by [`save_book`]; `reading` is set by the caller
    /// when scraped metrics are on hand.
    pub fn from_entry(entry: &Entry) -> Self {
        LibraryEntry {
            title: entry.title.clone(),
            authors: entry.author_names().map(str::to_string).collect(),
            author_uris: entry
                .authors
                .iter()
                .map(|a| a.uri.clone().unwrap_or_default())
                .collect(),
            summary: entry.summary.clone(),
            content: entry.content.clone(),
            language: entry.language.clone(),
            publisher: entry.publisher.clone(),
            published: entry.published.clone(),
            updated: entry.updated.clone(),
            categories: entry
                .categories
                .iter()
                .map(|c| (c.term.clone(), c.scheme.clone()))
                .collect(),
            web_url: entry.web_link().map(|l| l.href.clone()),
            reading: None,
            cover_file: None,
            files: Vec::new(),
        }
    }
}

/// A library book as loaded from disk: its id (sidecar stem), a synthetic
/// [`Entry`] for the shared rendering path, and the raw metadata record.
pub struct LibraryBook {
    pub id: String,
    pub entry: Entry,
    pub meta: LibraryEntry,
}

/// Load every downloaded book, sorted by author then title.
///
/// A missing library directory (nothing downloaded yet) yields an empty list;
/// individual sidecars that fail to read or parse are skipped rather than
/// failing the whole load.
pub fn load_library() -> Result<Vec<LibraryBook>> {
    load_library_from(&library_dir()?)
}

fn load_library_from(dir: &Path) -> Result<Vec<LibraryBook>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(Vec::new()),
    };
    let mut books = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<LibraryEntry>(&data) else {
            continue;
        };
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let entry = library_to_entry(dir, &meta);
        books.push(LibraryBook { id, entry, meta });
    }
    books.sort_by_key(|b| sort_key(&b.meta));
    Ok(books)
}

/// Sort books by lowercased first-author then title.
fn sort_key(meta: &LibraryEntry) -> (String, String) {
    let author = meta.authors.first().cloned().unwrap_or_default();
    (author.to_lowercase(), meta.title.to_lowercase())
}

/// Synthesize an OPDS [`Entry`] from a library record so the existing browse
/// and detail renderers work against local books unchanged. Cover and format
/// links carry absolute local file paths as their `href`.
fn library_to_entry(dir: &Path, meta: &LibraryEntry) -> Entry {
    let local = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let mut links = Vec::new();
    if let Some(cover) = &meta.cover_file {
        links.push(Link {
            rel: REL_IMAGE.to_string(),
            href: local(cover),
            mime: String::new(),
            title: String::new(),
            length: None,
        });
    }
    for f in &meta.files {
        links.push(Link {
            rel: REL_ACQUISITION.to_string(),
            href: local(&f.filename),
            mime: f.mime.clone(),
            title: String::new(),
            length: f.length,
        });
    }
    if let Some(web) = &meta.web_url {
        links.push(Link {
            rel: "alternate".to_string(),
            href: web.clone(),
            mime: "text/html".to_string(),
            title: String::new(),
            length: None,
        });
    }
    Entry {
        title: meta.title.clone(),
        authors: meta
            .authors
            .iter()
            .enumerate()
            .map(|(i, name)| Author {
                name: name.clone(),
                uri: meta.author_uris.get(i).filter(|u| !u.is_empty()).cloned(),
            })
            .collect(),
        summary: meta.summary.clone(),
        content: meta.content.clone(),
        language: meta.language.clone(),
        publisher: meta.publisher.clone(),
        published: meta.published.clone(),
        updated: meta.updated.clone(),
        rights: None,
        categories: meta
            .categories
            .iter()
            .map(|(term, scheme)| Category {
                term: term.clone(),
                scheme: scheme.clone(),
            })
            .collect(),
        links,
    }
}

/// Persist a downloaded book: write the ebook (and, the first time, the cover),
/// then write or merge the metadata sidecar. Re-downloading another format of
/// the same book appends to the existing record rather than overwriting it.
/// Returns the path of the saved ebook file.
pub fn save_book(
    meta: LibraryEntry,
    mime: &str,
    length: Option<u64>,
    ebook_url: &str,
    ebook_bytes: &[u8],
    cover_bytes: Option<&[u8]>,
) -> Result<PathBuf> {
    save_book_in(
        &library_dir()?,
        meta,
        mime,
        length,
        ebook_url,
        ebook_bytes,
        cover_bytes,
    )
}

#[allow(clippy::too_many_arguments)]
fn save_book_in(
    dir: &Path,
    meta: LibraryEntry,
    mime: &str,
    length: Option<u64>,
    ebook_url: &str,
    ebook_bytes: &[u8],
    cover_bytes: Option<&[u8]>,
) -> Result<PathBuf> {
    fs::create_dir_all(dir)
        .with_context(|| format!("creating library directory {}", dir.display()))?;

    let id = book_id(&meta.authors, &meta.title);
    let sidecar = dir.join(format!("{id}.json"));

    // Start from any existing record so accumulated files/cover survive.
    let existing: Option<LibraryEntry> = fs::read_to_string(&sidecar)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok());
    let mut files = existing
        .as_ref()
        .map(|e| e.files.clone())
        .unwrap_or_default();
    let mut cover_file = existing.and_then(|e| e.cover_file);

    // Write the ebook file.
    let ext = ext_for_mime(mime)
        .map(str::to_string)
        .unwrap_or_else(|| ext_from_url(ebook_url));
    let ebook_name = format!("{id}.{ext}");
    let ebook_path = dir.join(&ebook_name);
    fs::write(&ebook_path, ebook_bytes)
        .with_context(|| format!("writing {}", ebook_path.display()))?;
    if !files.iter().any(|f| f.filename == ebook_name) {
        files.push(LibraryFile {
            mime: mime.to_string(),
            filename: ebook_name,
            length: length.or(Some(ebook_bytes.len() as u64)),
        });
    }

    // Save the cover once, on the first download of this book.
    if cover_file.is_none()
        && let Some(bytes) = cover_bytes
    {
        let name = format!("{id}-cover.jpg");
        if fs::write(dir.join(&name), bytes).is_ok() {
            cover_file = Some(name);
        }
    }

    // Refresh metadata from the latest entry, keeping accumulated files/cover.
    let mut record = meta;
    record.files = files;
    record.cover_file = cover_file;

    let json = serde_json::to_string_pretty(&record)?;
    write_atomic(&sidecar, json.as_bytes())?;
    Ok(ebook_path)
}

/// Delete a book's sidecar and every file it references.
pub fn delete_book(id: &str) -> Result<()> {
    delete_book_in(&library_dir()?, id)
}

fn delete_book_in(dir: &Path, id: &str) -> Result<()> {
    let sidecar = dir.join(format!("{id}.json"));
    if let Ok(data) = fs::read_to_string(&sidecar)
        && let Ok(meta) = serde_json::from_str::<LibraryEntry>(&data)
    {
        for f in &meta.files {
            let _ = fs::remove_file(dir.join(&f.filename));
        }
        if let Some(cover) = &meta.cover_file {
            let _ = fs::remove_file(dir.join(cover));
        }
    }
    let _ = fs::remove_file(&sidecar);
    Ok(())
}

/// The library id for a book if it has already been downloaded, else `None`.
///
/// A book counts as downloaded when its metadata sidecar exists in the library
/// directory. Lets a catalog detail page offer a jump to the saved copy.
pub fn downloaded_book_id(authors: &[String], title: &str) -> Option<String> {
    downloaded_book_id_in(&library_dir().ok()?, authors, title)
}

fn downloaded_book_id_in(dir: &Path, authors: &[String], title: &str) -> Option<String> {
    let id = book_id(authors, title);
    dir.join(format!("{id}.json")).exists().then_some(id)
}

/// Open a downloaded file in the operating system's default application.
pub fn open_in_reader(path: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    const OPENER: &str = "xdg-open";
    #[cfg(target_os = "macos")]
    const OPENER: &str = "open";
    #[cfg(target_os = "windows")]
    const OPENER: &str = "explorer";

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    {
        std::process::Command::new(OPENER)
            .arg(path)
            .spawn()
            .with_context(|| format!("launching {OPENER} for {}", path.display()))?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        anyhow::bail!("opening files is not supported on this platform")
    }
}

/// A stable, readable id for a book, used as the `<id>` filename stem so every
/// format of one book shares a sidecar and cover. A short content hash is
/// appended so distinct books that slug identically don't collide.
fn book_id(authors: &[String], title: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut base = String::new();
    if let Some(a) = authors.first() {
        base.push_str(a);
        base.push('-');
    }
    base.push_str(title);

    let slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(60).collect();

    let hash = Sha256::digest(base.as_bytes());
    let suffix: String = hash.iter().take(4).map(|b| format!("{b:02x}")).collect();

    if slug.is_empty() {
        format!("book-{suffix}")
    } else {
        format!("{slug}-{suffix}")
    }
}

/// Canonical file extension for a known ebook MIME type.
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    match mime {
        "application/epub+zip" => Some("epub"),
        "application/kepub+zip" => Some("kepub"),
        "application/x-mobipocket-ebook" => Some("azw3"),
        "application/pdf" => Some("pdf"),
        "application/x-cbz" => Some("cbz"),
        _ => None,
    }
}

/// Fall back to the extension on a download URL's last path segment.
fn ext_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path_segments()
                .and_then(|mut s| s.next_back())
                .and_then(|seg| seg.rsplit_once('.').map(|(_, ext)| ext.to_string()))
        })
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| "bin".to_string())
}

/// Write bytes to `path` atomically via a temporary file and rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fresh, empty temp directory unique to this test run.
    fn temp_dir(tag: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("opdsview-test-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn sample_meta() -> LibraryEntry {
        LibraryEntry {
            title: "The Professor's House".to_string(),
            authors: vec!["Willa Cather".to_string()],
            summary: Some("A blurb.".to_string()),
            web_url: Some(
                "https://standardebooks.org/ebooks/willa-cather/the-professors-house".to_string(),
            ),
            categories: vec![("Fiction".to_string(), Some("lcsh".to_string()))],
            ..Default::default()
        }
    }

    #[test]
    fn book_id_is_stable_readable_and_distinct() {
        let authors = vec!["Willa Cather".to_string()];
        let a = book_id(&authors, "The Professor's House");
        // Deterministic for the same input.
        assert_eq!(a, book_id(&authors, "The Professor's House"));
        // Readable slug prefix, lowercased, no punctuation.
        assert!(a.starts_with("willa-cather-the-professor-s-house"));
        // A different title yields a different id.
        assert_ne!(a, book_id(&authors, "My Antonia"));
    }

    #[test]
    fn downloaded_book_id_detects_saved_books() {
        let dir = temp_dir("downloaded");
        let authors = vec!["Willa Cather".to_string()];
        // Nothing saved yet.
        assert!(downloaded_book_id_in(&dir, &authors, "The Professor's House").is_none());
        save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            Some(123),
            "https://se.org/book.epub",
            b"EPUBDATA",
            None,
        )
        .unwrap();
        // The saved book is now found by the same author/title…
        assert_eq!(
            downloaded_book_id_in(&dir, &authors, "The Professor's House"),
            Some(book_id(&authors, "The Professor's House"))
        );
        // …but a different book is not.
        assert!(downloaded_book_id_in(&dir, &authors, "My Antonia").is_none());
    }

    #[test]
    fn extension_from_mime_then_url() {
        assert_eq!(ext_for_mime("application/epub+zip"), Some("epub"));
        assert_eq!(ext_for_mime("application/x-mobipocket-ebook"), Some("azw3"));
        assert_eq!(ext_for_mime("application/unknown"), None);
        assert_eq!(ext_from_url("https://se.org/a/b/book.kepub?x=1"), "kepub");
        // No usable extension falls back to a generic one.
        assert_eq!(ext_from_url("https://se.org/download"), "bin");
    }

    #[test]
    fn save_then_load_round_trips_metadata_and_links() {
        let dir = temp_dir("roundtrip");
        let path = save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            Some(123),
            "https://se.org/book.epub",
            b"EPUBDATA",
            Some(b"COVERBYTES"),
        )
        .unwrap();
        assert!(path.exists());
        assert_eq!(fs::read(&path).unwrap(), b"EPUBDATA");

        let books = load_library_from(&dir).unwrap();
        assert_eq!(books.len(), 1);
        let book = &books[0];
        assert_eq!(book.entry.title, "The Professor's House");
        assert_eq!(
            book.entry.author_names().collect::<Vec<_>>(),
            vec!["Willa Cather"]
        );

        // The cover link resolves to a local file that exists.
        let cover = book.entry.image_link().expect("cover link");
        assert!(Path::new(&cover.href).exists());
        assert_eq!(fs::read(&cover.href).unwrap(), b"COVERBYTES");

        // The acquisition link points at the saved ebook with the right mime.
        let acq: Vec<_> = book.entry.acquisition_links().collect();
        assert_eq!(acq.len(), 1);
        assert_eq!(acq[0].mime, "application/epub+zip");
        assert!(Path::new(&acq[0].href).exists());

        // The web link round-trips.
        assert_eq!(
            book.entry.web_link().unwrap().href,
            "https://standardebooks.org/ebooks/willa-cather/the-professors-house"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_format_merges_into_one_record() {
        let dir = temp_dir("merge");
        save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            None,
            "https://se.org/b.epub",
            b"E",
            Some(b"C"),
        )
        .unwrap();
        save_book_in(
            &dir,
            sample_meta(),
            "application/x-mobipocket-ebook",
            None,
            "https://se.org/b.azw3",
            b"A",
            None,
        )
        .unwrap();

        let books = load_library_from(&dir).unwrap();
        // Still one book, now with two formats and the original cover.
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].entry.acquisition_links().count(), 2);
        assert!(books[0].entry.image_link().is_some());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_removes_files_and_sidecar() {
        let dir = temp_dir("delete");
        save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            None,
            "https://se.org/b.epub",
            b"E",
            Some(b"C"),
        )
        .unwrap();
        let id = load_library_from(&dir).unwrap()[0].id.clone();

        delete_book_in(&dir, &id).unwrap();
        assert!(load_library_from(&dir).unwrap().is_empty());
        // Directory has no leftover files for this book.
        let leftovers = fs::read_dir(&dir).unwrap().flatten().count();
        assert_eq!(leftovers, 0);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_library_dir_loads_empty() {
        let dir = temp_dir("missing");
        assert!(load_library_from(&dir).unwrap().is_empty());
    }
}
