//! Persistence of the saved-feed list, the downloaded-book library, and
//! on-disk locations.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

use crate::opds::{Author, Category, Entry, Link, REL_ACQUISITION, REL_IMAGE, identifier_token};
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

/// Settings for the optional "Import to Calibre" download destination.
///
/// `command` is the calibredb executable (looked up on `PATH` when just a
/// name); `library_path` is passed as `--library-path` so the import can target
/// a specific Calibre library or a running content-server URL — pointing at the
/// content server (e.g. `http://localhost:8080/#Library`) avoids the conflict
/// that arises when the Calibre GUI holds the on-disk library open. All are
/// edited directly in `feeds.json`; there is no in-app editor yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CalibreConfig {
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub library_path: Option<String>,
    /// How calibredb merges an import whose title+author already exist (passed
    /// as `--automerge`). See [`CalibreConfig::automerge`].
    #[serde(default)]
    pub automerge: Option<String>,
}

impl CalibreConfig {
    /// The configured calibredb command, defaulting to `calibredb` on `PATH`.
    pub fn command(&self) -> String {
        self.command
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| "calibredb".to_string())
    }

    /// The `--automerge` mode for importing a book that already exists in the
    /// library (by title + author). Defaults to `ignore` — merge new formats
    /// into the existing record, discarding formats already present — so adding
    /// a different format of an existing book works instead of being silently
    /// skipped as a duplicate. Other values: `overwrite`, `new_record`. An
    /// explicitly empty string disables automerge (calibredb then skips
    /// duplicate books, which can look like a silent no-op).
    pub fn automerge(&self) -> Option<String> {
        match &self.automerge {
            None => Some("ignore".to_string()),
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(s.trim().to_string()),
        }
    }
}

/// User overrides for the on-disk locations opdsview uses.
///
/// Every field is optional; an unset (or missing) field falls back to the
/// platform default, so an absent `settings` block behaves exactly as before.
/// A leading `~/` is expanded to the user's home directory. Edited directly in
/// `feeds.json` under a `"settings"` key. See [`cache_dir`], [`library_dir`],
/// and [`downloads_dir`], whose defaults these replace once [`install_settings`]
/// has run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Where downloaded books and their metadata sidecars live (the built-in
    /// library). Default: the app data directory's `library/` subfolder.
    #[serde(default)]
    pub library_dir: Option<String>,
    /// Where cached feed XML and cover images live. Default: the app cache
    /// directory.
    #[serde(default)]
    pub cache_dir: Option<String>,
    /// Where the "~/Downloads" download destination writes files. Default: the
    /// user's Downloads directory.
    #[serde(default)]
    pub download_dir: Option<String>,
}

/// The persisted application configuration: the list of saved feeds.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub feeds: Vec<Feed>,
    /// Settings for the Calibre import destination (see [`CalibreConfig`]).
    #[serde(default)]
    pub calibre: CalibreConfig,
    /// User overrides for on-disk locations (see [`Settings`]).
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    next_id: u64,
}

/// OPDS catalogs seeded into a fresh install on first run, as `(name, url)`
/// pairs. These are openly accessible feeds requiring no authentication.
const DEFAULT_FEEDS: &[(&str, &str)] = &[(
    "Project Gutenberg",
    "https://www.gutenberg.org/ebooks.opds/",
)];

impl Config {
    /// A config pre-populated with the [`DEFAULT_FEEDS`], used on first run.
    fn seeded() -> Self {
        let mut config = Self::default();
        for &(name, url) in DEFAULT_FEEDS {
            config.add(Feed {
                id: 0,
                name: name.to_string(),
                url: url.to_string(),
                username: None,
                password: None,
            });
        }
        config
    }

    /// Load the config from disk. On first run (no config file yet), returns a
    /// config seeded with the [`DEFAULT_FEEDS`] rather than an empty one.
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::seeded());
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

/// Process-wide path overrides, installed once at startup from the loaded
/// [`Config`]. The directory accessors below consult this; before it is set (or
/// when a field is unset), they fall back to the platform defaults.
static SETTINGS: std::sync::OnceLock<Settings> = std::sync::OnceLock::new();

/// Install the user's path overrides for the rest of the process. Call once at
/// startup, after loading the [`Config`] and before any directory accessor is
/// used (the worker reads them on its own thread, so this must precede the
/// worker spawn). Later calls are ignored — the first install wins.
pub fn install_settings(settings: Settings) {
    let _ = SETTINGS.set(settings);
}

/// The override path for one [`Settings`] field, with a leading `~/` expanded,
/// or `None` when unset or empty.
fn override_dir(pick: impl Fn(&Settings) -> Option<&String>) -> Option<PathBuf> {
    let raw = SETTINGS.get().and_then(&pick)?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| expand_tilde(trimmed))
}

/// Expand a leading `~` (alone or as `~/…`) to the user's home directory. Any
/// other path is returned verbatim.
fn expand_tilde(path: &str) -> PathBuf {
    let home = || UserDirs::new().map(|d| d.home_dir().to_path_buf());
    if path == "~" {
        if let Some(home) = home() {
            return home;
        }
    } else if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = home()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn config_file() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("feeds.json"))
}

/// Directory used for cached feed and image data.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = override_dir(|s| s.cache_dir.as_ref()) {
        return Ok(dir);
    }
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// Directory where downloaded books and their metadata sidecars are saved.
pub fn library_dir() -> Result<PathBuf> {
    if let Some(dir) = override_dir(|s| s.library_dir.as_ref()) {
        return Ok(dir);
    }
    Ok(project_dirs()?.data_dir().join("library"))
}

/// The user's Downloads directory, for the "~/Downloads" save destination.
pub fn downloads_dir() -> Result<PathBuf> {
    if let Some(dir) = override_dir(|s| s.download_dir.as_ref()) {
        return Ok(dir);
    }
    let dirs = UserDirs::new().context("could not determine user directories")?;
    dirs.download_dir()
        .map(Path::to_path_buf)
        .context("no Downloads directory is configured for this user")
}

// --- Downloaded-book library ---------------------------------------------

/// A reader's saved position within a book: the spine chapter index and the
/// vertical scroll offset (in rendered rows) within that chapter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadingProgress {
    pub chapter: usize,
    pub scroll: u16,
}

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
    /// The catalog entry's Atom `<id>`, when known (often a `urn:` identifier).
    #[serde(default)]
    pub id: Option<String>,
    /// `<dc:identifier>` values (ISBNs, URNs) carried over from the catalog.
    #[serde(default)]
    pub identifiers: Vec<String>,
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
    /// The reader's last position in this book, persisted across sessions.
    #[serde(default)]
    pub progress: Option<ReadingProgress>,
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
            id: entry.id.clone(),
            identifiers: entry.identifiers.clone(),
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
            progress: None,
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
        id: meta.id.clone(),
        identifiers: meta.identifiers.clone(),
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
    // Preserve any saved reading position when re-downloading another format.
    let progress = existing.as_ref().and_then(|e| e.progress.clone());
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

    // Refresh metadata from the latest entry, keeping accumulated files/cover
    // and any reading position from a previous download.
    let mut record = meta;
    record.files = files;
    record.cover_file = cover_file;
    record.progress = progress;

    let json = serde_json::to_string_pretty(&record)?;
    write_atomic(&sidecar, json.as_bytes())?;
    Ok(ebook_path)
}

/// Persist the reader's position for a downloaded book, leaving the rest of its
/// sidecar untouched. A no-op if the book has no sidecar (e.g. just deleted).
pub fn save_progress(id: &str, progress: ReadingProgress) -> Result<()> {
    save_progress_in(&library_dir()?, id, progress)
}

fn save_progress_in(dir: &Path, id: &str, progress: ReadingProgress) -> Result<()> {
    let sidecar = dir.join(format!("{id}.json"));
    let data =
        fs::read_to_string(&sidecar).with_context(|| format!("reading {}", sidecar.display()))?;
    let mut record: LibraryEntry =
        serde_json::from_str(&data).with_context(|| format!("parsing {}", sidecar.display()))?;
    record.progress = Some(progress);
    let json = serde_json::to_string_pretty(&record)?;
    write_atomic(&sidecar, json.as_bytes())
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

/// The ids (sidecar stems) of every book currently in the local library.
///
/// Cheaper than [`load_library`] — it reads only the directory listing, not the
/// sidecar contents — so the catalog list can refresh its "downloaded" markers
/// without parsing every book's metadata. A missing library directory yields an
/// empty list.
pub fn library_ids() -> Result<Vec<String>> {
    library_ids_in(&library_dir()?)
}

fn library_ids_in(dir: &Path) -> Result<Vec<String>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return Ok(Vec::new()),
    };
    let mut ids = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            ids.push(stem.to_string());
        }
    }
    Ok(ids)
}

// --- Calibre library status ----------------------------------------------

/// One book as reported by `calibredb list`, reduced to the fields used for
/// matching a catalog entry: its title, authors, and identifier tokens.
struct CalibreBook {
    title: String,
    authors: Vec<String>,
    tokens: Vec<String>,
}

/// Build a set of match keys for the books in a Calibre library by shelling out
/// to `calibredb list`.
///
/// Each book contributes a [`book_id`] slug (first author + title) and one
/// `scheme:value` key per identifier ([`identifier_token`] — `isbn:…`, `url:…`,
/// …), so a catalog entry can be marked "in Calibre" either by a shared
/// identifier ([`in_calibre_index`]) or, when no identifier is available on both
/// sides, by a fuzzy author+title slug. The identifier path matters most for
/// Standard Ebooks, which carry no ISBN but a stable `url:` identifier.
///
/// `library_path` is passed as `--library-path`; it may be an on-disk library or
/// a running content server URL (the latter avoids the lock held by an open
/// Calibre GUI), mirroring [`import_to_calibre`].
pub fn calibre_index(command: &str, library_path: Option<&str>) -> Result<HashSet<String>> {
    let mut cmd = std::process::Command::new(command);
    cmd.arg("list")
        .arg("--for-machine")
        .arg("--fields")
        .arg("title,authors,identifiers");
    if let Some(lib) = library_path.filter(|l| !l.trim().is_empty()) {
        cmd.arg("--library-path").arg(lib);
    }
    let output = cmd
        .output()
        .with_context(|| format!("running {command} list"))?;
    if !output.status.success() {
        let detail = squish(&String::from_utf8_lossy(&output.stderr));
        anyhow::bail!("{command} list failed ({}): {detail}", output.status);
    }
    let books = parse_calibre_list(&String::from_utf8_lossy(&output.stdout))?;
    Ok(calibre_match_keys(&books))
}

/// Parse the JSON array emitted by `calibredb list --for-machine`.
///
/// Only `title`, `authors`, and `identifiers` are read. `authors` is a
/// `" & "`-joined display string; `identifiers` is either a JSON object
/// (`{"isbn": "…"}`) or, on older calibredb, a comma-separated `key:value`
/// string — both shapes are handled.
fn parse_calibre_list(json: &str) -> Result<Vec<CalibreBook>> {
    let value: serde_json::Value =
        serde_json::from_str(json).context("parsing calibredb list output as JSON")?;
    let array = value
        .as_array()
        .context("calibredb list did not return a JSON array")?;
    let books = array
        .iter()
        .map(|item| {
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let authors = item
                .get("authors")
                .and_then(|v| v.as_str())
                .map(|s| {
                    s.split(" & ")
                        .map(|a| a.trim().to_string())
                        .filter(|a| !a.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let tokens = calibre_tokens(item.get("identifiers"));
            CalibreBook {
                title,
                authors,
                tokens,
            }
        })
        .collect();
    Ok(books)
}

/// Extract canonical identifier tokens from a calibredb `identifiers` field,
/// accepting both the JSON-object (`{"isbn": "…", "url": "…"}`) and the legacy
/// comma-separated-string (`isbn:…,url:…`) encodings.
fn calibre_tokens(identifiers: Option<&serde_json::Value>) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut add = |token: Option<String>| {
        if let Some(token) = token
            && !tokens.contains(&token)
        {
            tokens.push(token);
        }
    };
    match identifiers {
        Some(serde_json::Value::Object(map)) => {
            for (scheme, value) in map {
                if let Some(s) = value.as_str() {
                    add(identifier_token(scheme, s));
                }
            }
        }
        Some(serde_json::Value::String(s)) => {
            for pair in s.split(',') {
                if let Some((scheme, value)) = pair.split_once(':') {
                    add(identifier_token(scheme, value));
                }
            }
        }
        _ => {}
    }
    tokens
}

/// Reduce parsed Calibre books to the set of match keys (see [`calibre_index`]).
fn calibre_match_keys(books: &[CalibreBook]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for book in books {
        if !book.title.is_empty() {
            keys.insert(book_id(&book.authors, &book.title));
        }
        keys.extend(book.tokens.iter().cloned());
    }
    keys
}

/// Whether a catalog book is present in the Calibre index from [`calibre_index`].
///
/// A match by a shared identifier token (`isbn:…`, `url:…`) is preferred — it is
/// reliable across metadata variations and is the only sound key for Standard
/// Ebooks (URL-identified, no ISBN). Failing that, the first-author+title slug is
/// compared, the same fuzzy key the "downloaded" markers use. An empty index
/// (Calibre unconfigured or unreadable) never matches.
pub fn in_calibre_index(
    index: &HashSet<String>,
    authors: &[String],
    title: &str,
    tokens: &[String],
) -> bool {
    if index.is_empty() {
        return false;
    }
    if tokens.iter().any(|token| index.contains(token)) {
        return true;
    }
    index.contains(&book_id(authors, title))
}

/// Save a downloaded ebook as a plain file in `dir` (no sidecar, no cover), for
/// the "~/Downloads" destination. The file is named after the book's title so
/// it lands with a human-readable name rather than a library slug.
pub fn save_loose(
    dir: &Path,
    meta: &LibraryEntry,
    mime: &str,
    ebook_url: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("creating directory {}", dir.display()))?;
    let path = dir.join(friendly_filename(meta, mime, ebook_url));
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Import a downloaded ebook into Calibre by writing it to a temporary file and
/// running `calibredb add`. Returns the temp file's path.
///
/// `automerge` (when set) is passed as `--automerge=<mode>` so importing a
/// different format of a book already in the library merges into the existing
/// record rather than being skipped as a duplicate. Failures — and the case
/// where calibredb adds nothing because the book already exists — are surfaced
/// to the caller instead of looking like a silent success.
pub fn import_to_calibre(
    command: &str,
    library_path: Option<&str>,
    automerge: Option<&str>,
    meta: &LibraryEntry,
    mime: &str,
    ebook_url: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let path = std::env::temp_dir().join(friendly_filename(meta, mime, ebook_url));
    fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;

    let mut cmd = std::process::Command::new(command);
    cmd.arg("add");
    if let Some(lib) = library_path.filter(|l| !l.trim().is_empty()) {
        cmd.arg("--library-path").arg(lib);
    }
    if let Some(mode) = automerge.filter(|m| !m.trim().is_empty()) {
        cmd.arg(format!("--automerge={mode}"));
    }
    cmd.arg(&path);
    // Capture stdout/stderr rather than letting calibredb write over the TUI;
    // its output is the useful part of any failure, so fold it into the error.
    let output = cmd
        .output()
        .with_context(|| format!("running {command} add"))?;
    if !output.status.success() {
        let detail = squish(&String::from_utf8_lossy(&output.stderr));
        if detail.is_empty() {
            anyhow::bail!("{command} add failed ({})", output.status);
        }
        anyhow::bail!("{command} add failed ({}): {detail}", output.status);
    }
    // calibredb exits 0 even when it adds nothing because the book already
    // exists (and automerge is off); treat that as an error so it isn't a
    // silent no-op.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains("already exist") {
        anyhow::bail!(
            "Not added — this book is already in Calibre. {}",
            squish(&stdout)
        );
    }
    Ok(path)
}

/// Collapse runs of whitespace (including newlines) into single spaces, so
/// multi-line subprocess output fits on one line of an error message.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A human-readable filename for a loose copy: the book's title (with filesystem-
/// unsafe characters replaced) plus the format's canonical extension.
fn friendly_filename(meta: &LibraryEntry, mime: &str, ebook_url: &str) -> String {
    let ext = ext_for_mime(mime)
        .map(str::to_string)
        .unwrap_or_else(|| ext_from_url(ebook_url));
    let base: String = meta
        .title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = base.trim();
    if base.is_empty() {
        format!("book.{ext}")
    } else {
        format!("{base}.{ext}")
    }
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
///
/// Public so the catalog list can compute a candidate entry's id and check it
/// against [`library_ids`] to mark already-downloaded books.
pub fn book_id(authors: &[String], title: &str) -> String {
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

    #[test]
    fn expand_tilde_expands_home_and_leaves_other_paths() {
        let home = UserDirs::new().unwrap().home_dir().to_path_buf();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/Books"), home.join("Books"));
        // An absolute path and a bare ~user form are passed through untouched.
        assert_eq!(expand_tilde("/srv/library"), PathBuf::from("/srv/library"));
        assert_eq!(expand_tilde("~bob/x"), PathBuf::from("~bob/x"));
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
    fn seeded_config_has_default_feeds_with_distinct_ids() {
        let config = Config::seeded();
        // One feed per default entry, names/urls preserved in order.
        assert_eq!(config.feeds.len(), DEFAULT_FEEDS.len());
        for (feed, &(name, url)) in config.feeds.iter().zip(DEFAULT_FEEDS) {
            assert_eq!(feed.name, name);
            assert_eq!(feed.url, url);
            assert!(feed.auth().is_none());
        }
        // Ids are unique and next_id keeps counting from the last seeded feed,
        // so a feed added afterwards does not collide.
        let ids: Vec<u64> = config.feeds.iter().map(|f| f.id).collect();
        assert_eq!(ids, (1..=DEFAULT_FEEDS.len() as u64).collect::<Vec<_>>());
        let mut config = config;
        config.add(Feed {
            id: 0,
            name: "Custom".to_string(),
            url: "https://example.org/opds".to_string(),
            username: None,
            password: None,
        });
        assert_eq!(
            config.feeds.last().unwrap().id,
            DEFAULT_FEEDS.len() as u64 + 1
        );
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
    fn save_progress_round_trips_and_survives_redownload() {
        let dir = temp_dir("progress");
        save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            None,
            "https://se.org/b.epub",
            b"E",
            None,
        )
        .unwrap();
        let id = load_library_from(&dir).unwrap()[0].id.clone();
        assert!(load_library_from(&dir).unwrap()[0].meta.progress.is_none());

        let pos = ReadingProgress {
            chapter: 4,
            scroll: 12,
        };
        save_progress_in(&dir, &id, pos.clone()).unwrap();
        assert_eq!(
            load_library_from(&dir).unwrap()[0].meta.progress,
            Some(pos.clone())
        );

        // Downloading another format must not clobber the saved position.
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
        assert_eq!(load_library_from(&dir).unwrap()[0].meta.progress, Some(pos));

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

    #[test]
    fn library_ids_lists_saved_book_stems() {
        let dir = temp_dir("ids");
        // No directory yet → empty.
        assert!(library_ids_in(&dir).unwrap().is_empty());
        save_book_in(
            &dir,
            sample_meta(),
            "application/epub+zip",
            None,
            "https://se.org/b.epub",
            b"E",
            None,
        )
        .unwrap();
        let authors = vec!["Willa Cather".to_string()];
        assert_eq!(
            library_ids_in(&dir).unwrap(),
            vec![book_id(&authors, "The Professor's House")]
        );
    }

    #[test]
    fn calibre_index_matches_by_identifier_and_by_author_title() {
        // Three books: an ISBN-identified one, a Standard Ebooks one identified
        // only by URL, and one with no usable identifier.
        let json = r#"[
            {"title": "The Professor's House", "authors": "Willa Cather",
             "identifiers": {"isbn": "978-0-306-40615-7", "google": "abc"}},
            {"title": "Romola", "authors": "George Eliot",
             "identifiers": {"url": "https://standardebooks.org/ebooks/george-eliot/romola"}},
            {"title": "My Antonia", "authors": "Willa Cather & Someone Else",
             "identifiers": {}}
        ]"#;
        let books = parse_calibre_list(json).unwrap();
        assert_eq!(books.len(), 3);
        assert_eq!(books[2].authors, vec!["Willa Cather", "Someone Else"]);
        let index = calibre_match_keys(&books);

        let cather = vec!["Willa Cather".to_string()];
        // Matched by shared ISBN even though the title/author differ in spelling.
        assert!(in_calibre_index(
            &index,
            &["Different Name".to_string()],
            "Totally Different Title",
            &["isbn:9780306406157".to_string()],
        ));
        // A Standard Ebooks entry matches by its URL token (it has no ISBN), even
        // with a differently-formatted author — the whole point of URL matching.
        assert!(in_calibre_index(
            &index,
            &["Eliot, George".to_string()],
            "Romola",
            &["url:https://standardebooks.org/ebooks/george-eliot/romola".to_string()],
        ));
        // Matched by first-author + title when no identifier is shared.
        assert!(in_calibre_index(&index, &cather, "My Antonia", &[]));
        // A book that's in neither set is not matched.
        assert!(!in_calibre_index(&index, &cather, "O Pioneers!", &[]));
    }

    #[test]
    fn calibre_index_handles_legacy_string_identifiers() {
        // Older calibredb encodes identifiers as a comma-separated string; a URL
        // value contains its own colons, which split_once must not mangle.
        let json = r#"[{"title": "T", "authors": "A",
            "identifiers": "isbn:0306406152,url:https://standardebooks.org/ebooks/a/b"}]"#;
        let books = parse_calibre_list(json).unwrap();
        assert_eq!(
            books[0].tokens,
            vec![
                "isbn:0306406152".to_string(),
                "url:https://standardebooks.org/ebooks/a/b".to_string(),
            ]
        );
        let index = calibre_match_keys(&books);
        assert!(in_calibre_index(
            &index,
            &["X".to_string()],
            "Y",
            &["url:https://standardebooks.org/ebooks/a/b".to_string()],
        ));
    }

    #[test]
    fn friendly_filename_uses_title_and_format_extension() {
        let meta = sample_meta();
        // Title with an apostrophe becomes a filesystem-safe name with the
        // EPUB extension from the mime type.
        assert_eq!(
            friendly_filename(&meta, "application/epub+zip", "https://se.org/b.epub"),
            "The Professor_s House.epub"
        );
        // Unknown mime falls back to the URL's extension.
        assert_eq!(
            friendly_filename(&meta, "application/unknown", "https://se.org/b.azw3"),
            "The Professor_s House.azw3"
        );
        // An empty title falls back to a generic stem.
        let blank = LibraryEntry::default();
        assert_eq!(
            friendly_filename(&blank, "application/epub+zip", "https://se.org/b.epub"),
            "book.epub"
        );
    }
}
