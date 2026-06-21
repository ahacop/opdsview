//! Application state, input handling, and response handling.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::opds::{Entry, Feed};
use crate::reader::{Block, BookContent};
use crate::reading::ReadingStats;
use crate::storage::{Config, Feed as FeedConfig, LibraryBook, LibraryEntry, ReadingProgress};
use crate::worker::{DownloadDest, DownloadKind, Request, Response};

type Auth = Option<(String, String)>;

/// Labels for the feed-form fields, in focus order.
pub const FORM_LABELS: [&str; 4] = ["Name", "URL", "Username", "Password"];

/// State of the add/edit feed form.
pub struct FormState {
    /// `Some(id)` when editing an existing feed, `None` when creating one.
    pub editing_id: Option<u64>,
    pub fields: [String; 4],
    pub focus: usize,
    pub error: Option<String>,
}

impl FormState {
    fn empty() -> Self {
        FormState {
            editing_id: None,
            fields: Default::default(),
            focus: 0,
            error: None,
        }
    }

    fn from_feed(feed: &FeedConfig) -> Self {
        FormState {
            editing_id: Some(feed.id),
            fields: [
                feed.name.clone(),
                feed.url.clone(),
                feed.username.clone().unwrap_or_default(),
                feed.password.clone().unwrap_or_default(),
            ],
            focus: 0,
            error: None,
        }
    }
}

/// One level in the browser's navigation back-stack.
pub struct Crumb {
    pub url: String,
    pub title: String,
    pub selected: usize,
}

/// Open detail ("show page") for a single publication entry.
pub struct DetailState {
    /// Index of the entry within the current feed.
    pub entry_index: usize,
    /// Index of the highlighted acquisition format.
    pub format: usize,
    /// When viewing a catalog publication that's already in the local library,
    /// its library id — lets the user jump to the downloaded copy. `None` for
    /// library entries and for catalog books not yet downloaded.
    pub library_id: Option<String>,
}

/// Backend-specific state for a browse session.
///
/// The shared list/detail/search rendering operates on [`BrowserState`]'s
/// `feed`/`list`/`detail` regardless of where the entries came from; this enum
/// is the swappable *back end* — a remote OPDS catalog or the local library.
pub enum Backend {
    Opds(OpdsBackend),
    Library(LibraryBackend),
}

/// Back-end state for browsing a remote OPDS catalog.
pub struct OpdsBackend {
    pub auth: Auth,
    pub stack: Vec<Crumb>,
    pub current_url: String,
    /// OpenSearch description URL advertised by a feed we've visited, if any.
    pub search_url: Option<String>,
    /// The query of an in-flight search, used to drop stale search responses.
    pub pending_search: Option<String>,
}

/// Back-end state for browsing the local downloaded-book library.
pub struct LibraryBackend {
    /// All downloaded books, unfiltered.
    pub books: Vec<LibraryBook>,
    /// Indices into `books` currently shown (after any search filter).
    pub shown: Vec<usize>,
    /// The active filter query, if the list is filtered.
    pub query: Option<String>,
    /// A book id to select and open once the library finishes loading, set when
    /// jumping here from a catalog entry's "open downloaded copy".
    pub open_target: Option<String>,
    /// The screen to restore when backing out of the library, set when jumping
    /// here from a catalog so "back" returns to the catalog rather than the
    /// feed list. `None` for the library opened normally from the feed list.
    pub return_to: Option<Box<Screen>>,
}

impl LibraryBackend {
    /// Recompute `shown` for the given filter (`None` shows everything).
    fn apply_filter(&mut self, query: Option<String>) {
        self.shown = match &query {
            None => (0..self.books.len()).collect(),
            Some(q) => {
                let q = q.to_lowercase();
                (0..self.books.len())
                    .filter(|&i| book_matches(&self.books[i], &q))
                    .collect()
            }
        };
        self.query = query;
    }

    /// Build the display feed (cloned entries) for the current `shown` set.
    fn build_feed(&self, title: &str) -> Feed {
        Feed {
            title: title.to_string(),
            entries: self
                .shown
                .iter()
                .map(|&i| self.books[i].entry.clone())
                .collect(),
            links: Vec::new(),
        }
    }
}

/// Whether a library book matches a (lowercased) search query.
fn book_matches(book: &LibraryBook, q: &str) -> bool {
    let e = &book.entry;
    e.title.to_lowercase().contains(q)
        || e.author_names().any(|a| a.to_lowercase().contains(q))
        || e.subjects().any(|s| s.to_lowercase().contains(q))
        || e.genres().any(|s| s.to_lowercase().contains(q))
}

/// State while browsing a single catalog or the local library.
pub struct BrowserState {
    /// The data source backing this session.
    pub backend: Backend,
    pub feed: Option<Feed>,
    pub list: ListState,
    pub loading: bool,
    pub error: Option<String>,
    /// Title of the current location/collection.
    pub title: String,
    /// When `Some`, the full-page detail view for a publication is open.
    pub detail: Option<DetailState>,
    /// When `Some`, the search input box is open; holds the query being typed.
    pub search_query: Option<String>,
    /// Selection index to restore once the in-flight feed finishes loading.
    restore_select: Option<usize>,
}

impl BrowserState {
    /// The entry currently highlighted in the list, if any.
    pub fn selected_entry(&self) -> Option<&Entry> {
        let feed = self.feed.as_ref()?;
        feed.entries.get(self.list.selected()?)
    }

    /// The entry whose detail view is open, if any.
    pub fn detail_entry(&self) -> Option<&Entry> {
        let detail = self.detail.as_ref()?;
        self.feed.as_ref()?.entries.get(detail.entry_index)
    }

    /// HTTP credentials for this session (none for the local library).
    fn auth(&self) -> Auth {
        match &self.backend {
            Backend::Opds(o) => o.auth.clone(),
            Backend::Library(_) => None,
        }
    }
}

/// Loaded state of a cover image, keyed by image URL (or local file path).
pub enum ImageSlot {
    Loading,
    Ready(Box<StatefulProtocol>),
    Failed,
}

/// Progress of a book download, keyed by its acquisition URL.
pub enum DownloadSlot {
    Pending,
    Done(PathBuf),
    Failed(String),
}

/// Loaded state of a publication's scraped reading metrics, keyed by its web
/// page URL.
pub enum ReadingSlot {
    Loading,
    Ready(ReadingStats),
    /// The page had no parseable metrics (e.g. a non-SE catalog).
    Unavailable,
}

/// A pending confirmation prompt.
pub enum Confirm {
    /// Delete a saved feed (by id).
    DeleteFeed(u64),
    /// Delete a downloaded book (by index into the library's `shown` list).
    DeleteBook(usize),
}

/// The destinations offered by the download menu, in display order.
pub const DOWNLOAD_DESTS: [&str; 3] = [
    "opdsview library (readable here)",
    "~/Downloads",
    "Import to Calibre",
];

/// Everything the worker needs to download one acquisition link, captured when
/// the download menu opens so confirming a destination needs no further lookup.
struct PendingDownload {
    meta: Box<LibraryEntry>,
    url: String,
    mime: String,
    length: Option<u64>,
    cover_url: Option<String>,
    auth: Auth,
}

/// The open "choose a download destination" menu.
pub struct DownloadMenu {
    /// Highlighted row, an index into [`DOWNLOAD_DESTS`].
    pub selected: usize,
    /// The download to dispatch once a destination is chosen.
    pending: PendingDownload,
}

/// In-book string search state for the reader.
#[derive(Default)]
pub struct SearchState {
    /// The query being typed; `Some` while the find input box is open and
    /// capturing keys.
    pub input: Option<String>,
    /// The query the current `matches` were found for (empty if none run yet).
    pub query: String,
    /// All matches for `query`, ordered by (chapter, row, col).
    pub matches: Vec<crate::reader::Match>,
    /// Index into `matches` of the focused match.
    pub current: usize,
    /// Terminal width `matches` were computed at, so the UI can recompute them
    /// after a resize (match rows are width-dependent).
    pub width: u16,
}

/// State of the built-in EPUB reader.
pub struct ReaderState {
    /// Library id of the book being read, for persisting progress. `None` if the
    /// book isn't a tracked library entry.
    pub book_id: Option<String>,
    /// Filesystem path of the EPUB file (also the image-request key prefix).
    pub book_path: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
    /// Raw XHTML of each spine document, in reading order.
    pub chapters: Vec<String>,
    pub toc: Vec<crate::reader::TocEntry>,
    /// Current spine index.
    pub chapter: usize,
    /// Vertical scroll offset, in rendered rows, within the current chapter.
    pub scroll: u16,
    /// Rendered blocks for the current chapter; rebuilt lazily by the UI.
    pub blocks: Vec<Block>,
    /// The (chapter, width) `blocks` were rendered for, so the UI re-wraps only
    /// when the chapter or terminal width changes.
    pub rendered_for: Option<(usize, u16)>,
    /// Total height of `blocks` in rows (for scroll clamping).
    pub content_height: u16,
    /// Height of the reader viewport, recorded each render for paging.
    pub viewport_height: u16,
    /// Width of the reader viewport, recorded each render (for search).
    pub viewport_width: u16,
    /// In-book search state.
    pub search: SearchState,
    /// Whether the table-of-contents popup is open.
    pub toc_open: bool,
    /// Selected row in the TOC popup.
    pub toc_selected: usize,
    /// Screen to restore when the reader closes (the library browser).
    pub return_to: Box<Screen>,
}

/// Which screen the UI is showing.
pub enum Screen {
    FeedList,
    Form(FormState),
    Browser(Box<BrowserState>),
    Reader(Box<ReaderState>),
}

/// Whether a MIME type is an EPUB-family container the built-in reader can open.
/// KePub is Kobo's EPUB variant — structurally an EPUB OCF zip — so the same
/// parser handles it.
fn is_readable_ebook(mime: &str) -> bool {
    matches!(mime, "application/epub+zip" | "application/kepub+zip")
}

/// The data needed to launch the reader, gathered before mutating `App`.
struct ReaderInit {
    path: String,
    book_id: Option<String>,
    progress: Option<ReadingProgress>,
    title: String,
}

/// Scroll the reader so the currently focused search match sits a little below
/// the top of the viewport. The renderer clamps the result to the real content.
fn focus_match(r: &mut ReaderState) {
    if let Some(m) = r.search.matches.get(r.search.current) {
        r.chapter = m.chapter.min(r.chapters.len().saturating_sub(1));
        let margin = r.viewport_height / 4;
        r.scroll = m.row.saturating_sub(margin);
    }
}

/// A short status-line hint for a failed Calibre query, or `None` when the
/// failure is just "calibredb isn't installed" — in that case the user isn't
/// using Calibre and shouldn't be nagged on every catalog open.
///
/// Formatted with `{err:#}` so the source chain (the OS "No such file" error, or
/// calibredb's lock message on stderr) is included; the top-level `Display`
/// alone would omit it.
fn calibre_error_hint(err: &anyhow::Error) -> Option<String> {
    let detail = format!("{err:#}");
    let lower = detail.to_lowercase();
    if lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("cannot find")
    {
        return None;
    }
    if lower.contains("another")
        || lower.contains("is running")
        || lower.contains("locked")
        || lower.contains("in use")
    {
        return Some(
            "Calibre library looks locked (desktop app open?). Point calibre.library_path at the content-server URL to read it while the GUI runs."
                .to_string(),
        );
    }
    Some(format!(
        "Couldn't read Calibre library: {}",
        squish_line(&detail)
    ))
}

/// Collapse whitespace and truncate so a hint fits on one status line.
fn squish_line(s: &str) -> String {
    let squished = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squished.chars().count() > 120 {
        let mut truncated: String = squished.chars().take(117).collect();
        truncated.push('…');
        truncated
    } else {
        squished
    }
}

pub struct App {
    pub config: Config,
    pub screen: Screen,
    pub feed_list: ListState,
    /// A pending confirmation popup (delete feed / delete book), if open.
    pub confirm: Option<Confirm>,
    /// The open download-destination menu, if any.
    pub download_menu: Option<DownloadMenu>,
    /// A dismissible message popup (e.g. a download error too long for the
    /// status line). Any key closes it.
    pub notice: Option<String>,
    /// Ids of books in the local library, for the catalog's "downloaded"
    /// markers. Refreshed when a catalog opens and after each download.
    pub downloaded_ids: HashSet<String>,
    /// Match keys for books in the user's Calibre library, for the catalog's
    /// "in Calibre" markers (see [`crate::storage::calibre_index`]). Queried
    /// lazily the first time a catalog opens, and again after a Calibre import.
    pub calibre_ids: HashSet<String>,
    /// Whether the Calibre index has been queried this session; gates the lazy
    /// (and potentially slow) `calibredb list` to one run unless an import
    /// invalidates it.
    pub calibre_loaded: bool,
    pub status: String,
    pub should_quit: bool,
    /// Outgoing network requests, drained by the main loop after each update.
    pub outbox: Vec<Request>,
    pub images: HashMap<String, ImageSlot>,
    /// Book downloads in progress or completed, keyed by acquisition URL.
    pub downloads: HashMap<String, DownloadSlot>,
    /// Scraped reading metrics, keyed by a publication's web page URL.
    pub reading: HashMap<String, ReadingSlot>,
    pub picker: Picker,
}

impl App {
    pub fn new(config: Config, picker: Picker) -> Self {
        let mut feed_list = ListState::default();
        // Row 0 is always the pinned "Downloaded books" library entry.
        feed_list.select(Some(0));
        App {
            config,
            screen: Screen::FeedList,
            feed_list,
            confirm: None,
            download_menu: None,
            notice: None,
            downloaded_ids: HashSet::new(),
            calibre_ids: HashSet::new(),
            calibre_loaded: false,
            status: String::new(),
            should_quit: false,
            outbox: Vec::new(),
            images: HashMap::new(),
            downloads: HashMap::new(),
            reading: HashMap::new(),
            picker,
        }
    }

    /// Whether a popup is currently painted over the main content. Used to
    /// force a full redraw when one closes, so any graphics-protocol image it
    /// covered is re-emitted instead of leaving the popup's cells behind.
    pub fn has_overlay(&self) -> bool {
        if self.notice.is_some() || self.confirm.is_some() || self.download_menu.is_some() {
            return true;
        }
        match &self.screen {
            Screen::Browser(b) => b.search_query.is_some(),
            Screen::Reader(r) => r.search.input.is_some() || r.toc_open,
            _ => false,
        }
    }

    // --- Input ------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // A message popup swallows the next key, which dismisses it.
        if self.notice.is_some() {
            self.notice = None;
            return;
        }
        // A popup confirmation intercepts all keys.
        if self.confirm.is_some() {
            self.handle_confirm_key(key);
            return;
        }
        // The download-destination menu likewise intercepts all keys.
        if self.download_menu.is_some() {
            self.handle_download_menu_key(key);
            return;
        }
        match &mut self.screen {
            Screen::FeedList => self.handle_feed_list_key(key),
            Screen::Form(_) => self.handle_form_key(key),
            Screen::Browser(_) => self.handle_browser_key(key),
            Screen::Reader(_) => self.handle_reader_key(key),
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) {
        let confirm = self.confirm.take();
        // Any key other than 'y' simply dismisses the prompt.
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            return;
        }
        match confirm {
            Some(Confirm::DeleteFeed(id)) => {
                self.config.remove(id);
                let _ = self.config.save();
                let len = 1 + self.config.feeds.len();
                fix_selection(&mut self.feed_list, len);
                self.status = "Feed deleted".into();
            }
            Some(Confirm::DeleteBook(i)) => self.delete_book(i),
            None => {}
        }
    }

    fn handle_feed_list_key(&mut self, key: KeyEvent) {
        let len = 1 + self.config.feeds.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => move_sel(&mut self.feed_list, len, 1),
            KeyCode::Char('k') | KeyCode::Up => move_sel(&mut self.feed_list, len, -1),
            KeyCode::Char('n') => {
                self.screen = Screen::Form(FormState::empty());
            }
            KeyCode::Char('e') => {
                if let Some(feed) = self.selected_config_feed() {
                    self.screen = Screen::Form(FormState::from_feed(feed));
                }
            }
            KeyCode::Char('d') => {
                if let Some(feed) = self.selected_config_feed() {
                    self.confirm = Some(Confirm::DeleteFeed(feed.id));
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => self.open_selected(),
            _ => {}
        }
    }

    fn handle_form_key(&mut self, key: KeyEvent) {
        let Screen::Form(form) = &mut self.screen else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.screen = Screen::FeedList,
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % 4,
            KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + 3) % 4,
            KeyCode::Enter => self.submit_form(),
            KeyCode::Backspace => {
                form.fields[form.focus].pop();
            }
            KeyCode::Char(c) => form.fields[form.focus].push(c),
            _ => {}
        }
    }

    fn handle_browser_key(&mut self, key: KeyEvent) {
        // The search input box captures all keys while open.
        if matches!(&self.screen, Screen::Browser(b) if b.search_query.is_some()) {
            self.handle_search_key(key);
            return;
        }
        // The detail ("show page") overlay captures all keys while open.
        if matches!(&self.screen, Screen::Browser(b) if b.detail.is_some()) {
            self.handle_detail_key(key);
            return;
        }
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let len = b.feed.as_ref().map_or(0, |f| f.entries.len());
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.screen = Screen::FeedList,
            KeyCode::Char('j') | KeyCode::Down => {
                move_sel(&mut b.list, len, 1);
                self.request_selected();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                move_sel(&mut b.list, len, -1);
                self.request_selected();
            }
            KeyCode::Char('g') => {
                if len > 0 {
                    b.list.select(Some(0));
                    self.request_selected();
                }
            }
            KeyCode::Char('G') => {
                if len > 0 {
                    b.list.select(Some(len - 1));
                    self.request_selected();
                }
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.follow_selected(),
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => self.go_back(),
            KeyCode::Char('n') => self.next_page(),
            KeyCode::Char('d') => self.confirm_delete_selected_book(),
            KeyCode::Char('/') => self.open_search(),
            _ => {}
        }
    }

    /// Open the search input box. OPDS catalogs must advertise search; the
    /// local library always supports it (an in-memory filter).
    fn open_search(&mut self) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        if let Backend::Opds(o) = &b.backend
            && o.search_url.is_none()
        {
            self.status = "This catalog doesn't support search".into();
            return;
        }
        b.search_query = Some(String::new());
    }

    /// Key handling while the search input box is open.
    fn handle_search_key(&mut self, key: KeyEvent) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        match key.code {
            KeyCode::Esc => b.search_query = None,
            KeyCode::Enter => self.submit_search(),
            KeyCode::Backspace => {
                if let Some(q) = b.search_query.as_mut() {
                    q.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(q) = b.search_query.as_mut() {
                    q.push(c);
                }
            }
            _ => {}
        }
    }

    /// Submit the typed query: an OPDS search (async) or a library filter.
    fn submit_search(&mut self) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let query = b.search_query.take().unwrap_or_default().trim().to_string();
        match &mut b.backend {
            Backend::Library(lib) => {
                let filter = if query.is_empty() { None } else { Some(query) };
                lib.apply_filter(filter);
                let feed = lib.build_feed(&b.title);
                let len = feed.entries.len();
                b.feed = Some(feed);
                b.list.select(if len == 0 { None } else { Some(0) });
                self.request_selected();
            }
            Backend::Opds(o) => {
                if query.is_empty() {
                    return;
                }
                let Some(desc_url) = o.search_url.clone() else {
                    self.status = "This catalog doesn't support search".into();
                    return;
                };
                let auth = o.auth.clone();
                o.stack.push(Crumb {
                    url: o.current_url.clone(),
                    title: b.title.clone(),
                    selected: b.list.selected().unwrap_or(0),
                });
                // The real feed URL is unknown until the worker resolves the
                // template; blanking it drops any in-flight plain-feed response.
                o.current_url = String::new();
                o.pending_search = Some(query.clone());
                b.title = format!("Search: {query}");
                b.loading = true;
                b.error = None;
                b.feed = None;
                b.restore_select = Some(0);
                self.outbox.push(Request::Search {
                    desc_url,
                    query,
                    auth,
                });
            }
        }
    }

    // --- Feed list actions ------------------------------------------------

    /// The saved feed under the cursor, or `None` on the pinned library row.
    fn selected_config_feed(&self) -> Option<&FeedConfig> {
        let sel = self.feed_list.selected()?;
        if sel == 0 {
            return None;
        }
        self.config.feeds.get(sel - 1)
    }

    fn submit_form(&mut self) {
        let Screen::Form(form) = &mut self.screen else {
            return;
        };
        let url = form.fields[1].trim().to_string();
        if url.is_empty() {
            form.error = Some("URL is required".into());
            return;
        }
        let name = {
            let n = form.fields[0].trim();
            if n.is_empty() {
                url.clone()
            } else {
                n.to_string()
            }
        };
        let opt = |s: &str| {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };
        let feed = FeedConfig {
            id: form.editing_id.unwrap_or(0),
            name,
            url,
            username: opt(&form.fields[2]),
            password: opt(&form.fields[3]),
        };
        match form.editing_id {
            Some(_) => self.config.update(feed),
            None => self.config.add(feed),
        }
        if let Err(e) = self.config.save() {
            self.status = format!("Failed to save: {e}");
        } else {
            self.status = "Feed saved".into();
        }
        self.screen = Screen::FeedList;
    }

    /// Open whatever the cursor is on: the library (row 0) or a saved feed.
    fn open_selected(&mut self) {
        let sel = self.feed_list.selected().unwrap_or(0);
        if sel == 0 {
            self.open_library(None, None);
            return;
        }
        let Some(feed) = self.config.feeds.get(sel - 1) else {
            return;
        };
        let auth = feed.auth();
        let url = feed.url.clone();
        let title = feed.name.clone();
        let mut list = ListState::default();
        list.select(Some(0));
        let browser = BrowserState {
            backend: Backend::Opds(OpdsBackend {
                auth: auth.clone(),
                stack: Vec::new(),
                current_url: url.clone(),
                search_url: None,
                pending_search: None,
            }),
            feed: None,
            list,
            loading: true,
            error: None,
            title,
            detail: None,
            search_query: None,
            restore_select: None,
        };
        self.screen = Screen::Browser(Box::new(browser));
        self.outbox.push(Request::Feed { url, auth });
        // Refresh the set of downloaded books so the list can mark them.
        self.outbox.push(Request::LibraryIds);
        // Query Calibre once per session so the list can mark books already in
        // the user's Calibre library; refreshed after an import.
        if !self.calibre_loaded {
            let req = self.calibre_ids_request();
            self.outbox.push(req);
        }
    }

    /// Build a request to (re)query the configured Calibre library's match keys.
    fn calibre_ids_request(&self) -> Request {
        Request::CalibreIds {
            command: self.config.calibre.command(),
            library_path: self.config.calibre.library_path.clone(),
        }
    }

    /// Open the local downloaded-book library. When `open_target` is `Some`, the
    /// book with that id is selected and its detail opened once loading finishes;
    /// `return_to` is the screen to restore when backing out (the catalog, when
    /// jumping here from one).
    fn open_library(&mut self, open_target: Option<String>, return_to: Option<Box<Screen>>) {
        let mut list = ListState::default();
        list.select(Some(0));
        let browser = BrowserState {
            backend: Backend::Library(LibraryBackend {
                books: Vec::new(),
                shown: Vec::new(),
                query: None,
                open_target,
                return_to,
            }),
            feed: None,
            list,
            loading: true,
            error: None,
            title: "Downloaded books".to_string(),
            detail: None,
            search_query: None,
            restore_select: None,
        };
        self.screen = Screen::Browser(Box::new(browser));
        self.outbox.push(Request::Library);
    }

    // --- Browser navigation ----------------------------------------------

    fn follow_selected(&mut self) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Some(entry) = b.selected_entry() else {
            return;
        };
        // Read everything off `entry` before mutating `b` (entry borrows b).
        let entry_index = b.list.selected().unwrap_or(0);
        let nav = entry
            .nav_link()
            .map(|l| (l.href.clone(), entry.title.clone()));
        let has_acquisition = entry.acquisition_links().next().is_some();
        let web_url = entry.web_link().map(|l| l.href.clone());
        // For a catalog book, note whether it's already in the local library so
        // the detail view can offer a jump to the downloaded copy.
        let library_id = match &b.backend {
            Backend::Opds(_) => {
                let authors: Vec<String> = entry.author_names().map(str::to_string).collect();
                crate::storage::downloaded_book_id(&authors, &entry.title)
            }
            Backend::Library(_) => None,
        };

        // A publication entry: open its detail / download view.
        if nav.is_none() {
            if has_acquisition {
                b.detail = Some(DetailState {
                    entry_index,
                    format: 0,
                    library_id,
                });
                // Lazily fetch reading metrics from the publication's web page
                // (unless already loaded — library books seed them from disk).
                if let Some(url) = web_url
                    && !self.reading.contains_key(&url)
                {
                    let auth = b.auth();
                    self.reading.insert(url.clone(), ReadingSlot::Loading);
                    self.outbox.push(Request::Reading { url, auth });
                }
            } else {
                self.status = "No catalog link or downloads for this entry".into();
            }
            return;
        }

        // A navigation entry: load the linked sub-catalog (OPDS only).
        let Some((next_url, next_title)) = nav else {
            return;
        };
        let Backend::Opds(o) = &mut b.backend else {
            return;
        };
        o.stack.push(Crumb {
            url: o.current_url.clone(),
            title: b.title.clone(),
            selected: b.list.selected().unwrap_or(0),
        });
        let auth = o.auth.clone();
        o.current_url = next_url.clone();
        o.pending_search = None;
        b.title = next_title;
        b.loading = true;
        b.error = None;
        b.feed = None;
        b.restore_select = Some(0);
        self.outbox.push(Request::Feed {
            url: next_url,
            auth,
        });
    }

    /// Key handling while the publication detail view is open.
    fn handle_detail_key(&mut self, key: KeyEvent) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Some(detail) = b.detail.as_mut() else {
            return;
        };
        let formats = b
            .feed
            .as_ref()
            .and_then(|f| f.entries.get(detail.entry_index))
            .map_or(0, |e| e.acquisition_links().count());
        let library = matches!(b.backend, Backend::Library(_));
        match key.code {
            KeyCode::Esc
            | KeyCode::Char('q')
            | KeyCode::Char('h')
            | KeyCode::Left
            | KeyCode::Backspace => {
                b.detail = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if formats > 0 {
                    detail.format = (detail.format + 1).min(formats - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                detail.format = detail.format.saturating_sub(1);
            }
            // In the library, Enter/o opens the built-in reader (falling back to
            // the external opener for non-EPUB formats); in a catalog they
            // download the selected format.
            KeyCode::Enter | KeyCode::Char('o') => {
                if library {
                    self.open_reader();
                } else {
                    self.activate_selected_format();
                }
            }
            // Catalog: download. Library: force-open in the external OS reader.
            KeyCode::Char('d') if !library => self.activate_selected_format(),
            KeyCode::Char('x') if library => self.open_selected_format(),
            KeyCode::Char('g') => self.jump_to_downloaded(),
            _ => {}
        }
    }

    /// Switch to the library and open the downloaded copy of the catalog book
    /// whose detail is showing, if it has one. The catalog screen is kept so
    /// backing out of the library returns to it.
    fn jump_to_downloaded(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        let Some(id) = b.detail.as_ref().and_then(|d| d.library_id.clone()) else {
            return;
        };
        // open_library overwrites self.screen, so take the catalog out first.
        let catalog = std::mem::replace(&mut self.screen, Screen::FeedList);
        self.open_library(Some(id), Some(Box::new(catalog)));
    }

    /// After a successful download of `url`, if an open catalog detail is for
    /// that very book, record its library id so the jump key lights up.
    fn mark_detail_downloaded(&mut self, url: &str) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        if !matches!(b.backend, Backend::Opds(_)) {
            return;
        }
        let Some(detail) = b.detail.as_mut() else {
            return;
        };
        let Some(entry) = b
            .feed
            .as_ref()
            .and_then(|f| f.entries.get(detail.entry_index))
        else {
            return;
        };
        if entry.acquisition_links().any(|l| l.href == url) {
            let authors: Vec<String> = entry.author_names().map(str::to_string).collect();
            detail.library_id = crate::storage::downloaded_book_id(&authors, &entry.title);
        }
    }

    /// Act on the highlighted format: download it (OPDS) or open it (library).
    fn activate_selected_format(&mut self) {
        let is_library = matches!(
            &self.screen,
            Screen::Browser(b) if matches!(b.backend, Backend::Library(_))
        );
        if is_library {
            self.open_selected_format();
        } else {
            self.download_selected_format();
        }
    }

    /// Open the destination menu for the format highlighted in the detail view,
    /// gathering the metadata the worker will need once a destination is picked.
    fn download_selected_format(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        let Some(detail) = b.detail.as_ref() else {
            return;
        };
        let Some(entry) = b
            .feed
            .as_ref()
            .and_then(|f| f.entries.get(detail.entry_index))
        else {
            return;
        };
        let Some(link) = entry.acquisition_links().nth(detail.format) else {
            return;
        };
        let url = link.href.clone();
        // Don't re-queue a download that's already running.
        if matches!(self.downloads.get(&url), Some(DownloadSlot::Pending)) {
            self.status = "Already downloading…".into();
            return;
        }
        let mime = link.mime.clone();
        let length = link.length;
        let cover_url = entry.image_link().map(|l| l.href.clone());
        let mut meta = LibraryEntry::from_entry(entry);
        // Attach scraped reading metrics if we have them on hand.
        if let Some(web) = entry.web_link()
            && let Some(ReadingSlot::Ready(stats)) = self.reading.get(&web.href)
        {
            meta.reading = Some(stats.clone());
        }
        let auth = b.auth();
        self.download_menu = Some(DownloadMenu {
            selected: 0,
            pending: PendingDownload {
                meta: Box::new(meta),
                url,
                mime,
                length,
                cover_url,
                auth,
            },
        });
    }

    /// Key handling while the download-destination menu is open.
    fn handle_download_menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = self.download_menu.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.download_menu = None,
            KeyCode::Char('j') | KeyCode::Down => {
                menu.selected = (menu.selected + 1).min(DOWNLOAD_DESTS.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                menu.selected = menu.selected.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('l') => self.confirm_download(),
            _ => {}
        }
    }

    /// Dispatch the pending download to the destination highlighted in the menu.
    fn confirm_download(&mut self) {
        let Some(menu) = self.download_menu.take() else {
            return;
        };
        let (dest, status): (DownloadDest, &str) = match menu.selected {
            1 => (DownloadDest::Downloads, "Saving to ~/Downloads…"),
            2 => (
                DownloadDest::Calibre {
                    command: self.config.calibre.command(),
                    library_path: self.config.calibre.library_path.clone(),
                    automerge: self.config.calibre.automerge(),
                },
                "Importing to Calibre…",
            ),
            _ => (DownloadDest::Library, "Downloading…"),
        };
        let PendingDownload {
            meta,
            url,
            mime,
            length,
            cover_url,
            auth,
        } = menu.pending;
        self.downloads.insert(url.clone(), DownloadSlot::Pending);
        self.status = status.into();
        self.outbox.push(Request::Download {
            meta,
            url,
            mime,
            length,
            cover_url,
            auth,
            dest,
        });
    }

    /// Open the highlighted local format in the OS default reader.
    fn open_selected_format(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        let Some(detail) = b.detail.as_ref() else {
            return;
        };
        let Some(entry) = b
            .feed
            .as_ref()
            .and_then(|f| f.entries.get(detail.entry_index))
        else {
            return;
        };
        let Some(link) = entry.acquisition_links().nth(detail.format) else {
            return;
        };
        let path = PathBuf::from(&link.href);
        match crate::storage::open_in_reader(&path) {
            Ok(()) => self.status = format!("Opened {}", path.display()),
            Err(e) => self.status = format!("Open failed: {e:#}"),
        }
    }

    /// Open the selected library book in the built-in EPUB reader. Picks the
    /// highlighted format if it's an EPUB, else the book's first EPUB; if the
    /// book has no EPUB format, falls back to the external OS reader.
    fn open_reader(&mut self) {
        // Gather what we need while only borrowing `self` immutably; `None`
        // means "no EPUB here, fall back to the external opener".
        let init = (|| -> Option<ReaderInit> {
            let Screen::Browser(b) = &self.screen else {
                return None;
            };
            let Backend::Library(lib) = &b.backend else {
                return None;
            };
            let detail = b.detail.as_ref()?;
            let entry = b.feed.as_ref()?.entries.get(detail.entry_index)?;
            let links: Vec<&crate::opds::Link> = entry.acquisition_links().collect();
            let link = links
                .get(detail.format)
                .copied()
                .filter(|l| is_readable_ebook(&l.mime))
                .or_else(|| links.iter().copied().find(|l| is_readable_ebook(&l.mime)))?;
            let book = lib
                .shown
                .get(detail.entry_index)
                .and_then(|&i| lib.books.get(i));
            Some(ReaderInit {
                path: link.href.clone(),
                book_id: book.map(|bk| bk.id.clone()),
                progress: book.and_then(|bk| bk.meta.progress.clone()),
                title: entry.title.clone(),
            })
        })();

        let Some(init) = init else {
            self.open_selected_format();
            return;
        };

        let (chapter, scroll) = init.progress.map_or((0, 0), |p| (p.chapter, p.scroll));
        let return_to = std::mem::replace(&mut self.screen, Screen::FeedList);
        let reader = ReaderState {
            book_id: init.book_id,
            book_path: init.path.clone(),
            title: init.title,
            loading: true,
            error: None,
            chapters: Vec::new(),
            toc: Vec::new(),
            chapter,
            scroll,
            blocks: Vec::new(),
            rendered_for: None,
            content_height: 0,
            viewport_height: 0,
            viewport_width: 0,
            search: SearchState::default(),
            toc_open: false,
            toc_selected: 0,
            return_to: Box::new(return_to),
        };
        self.screen = Screen::Reader(Box::new(reader));
        self.outbox.push(Request::OpenBook {
            path: PathBuf::from(init.path),
        });
    }

    /// Key handling for the built-in reader.
    fn handle_reader_key(&mut self, key: KeyEvent) {
        // The find input box captures all keys while open.
        if matches!(&self.screen, Screen::Reader(r) if r.search.input.is_some()) {
            self.handle_reader_search_key(key);
            return;
        }
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        // The table-of-contents popup captures keys while open.
        if r.toc_open {
            match key.code {
                KeyCode::Esc | KeyCode::Char('t') | KeyCode::Char('q') => r.toc_open = false,
                KeyCode::Char('j') | KeyCode::Down => {
                    if !r.toc.is_empty() {
                        r.toc_selected = (r.toc_selected + 1).min(r.toc.len() - 1);
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    r.toc_selected = r.toc_selected.saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Char('l') => {
                    if let Some(entry) = r.toc.get(r.toc_selected) {
                        r.chapter = entry.chapter.min(r.chapters.len().saturating_sub(1));
                        r.scroll = 0;
                    }
                    r.toc_open = false;
                }
                _ => {}
            }
            return;
        }

        let page = r.viewport_height.saturating_sub(2).max(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Backspace => self.close_reader(),
            KeyCode::Char('j') | KeyCode::Down => r.scroll = r.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => r.scroll = r.scroll.saturating_sub(1),
            // Page down; once already at the chapter's end, advance to the next.
            KeyCode::Char(' ') | KeyCode::PageDown => {
                let max_scroll = r.content_height.saturating_sub(r.viewport_height);
                if r.scroll >= max_scroll && r.chapter + 1 < r.chapters.len() {
                    r.chapter += 1;
                    r.scroll = 0;
                } else {
                    r.scroll = r.scroll.saturating_add(page);
                }
            }
            KeyCode::PageUp => r.scroll = r.scroll.saturating_sub(page),
            KeyCode::Char('g') => r.scroll = 0,
            // Clamped to the real content height by the renderer.
            KeyCode::Char('G') => r.scroll = u16::MAX,
            // While a search is active, n/N step through matches (vim-style);
            // otherwise n stays the next-chapter binding.
            KeyCode::Char('n') if !r.search.matches.is_empty() => self.step_match(1),
            KeyCode::Char('N') if !r.search.matches.is_empty() => self.step_match(-1),
            KeyCode::Char('/') => r.search.input = Some(String::new()),
            KeyCode::Char('n') | KeyCode::Char('l') | KeyCode::Right => {
                if r.chapter + 1 < r.chapters.len() {
                    r.chapter += 1;
                    r.scroll = 0;
                }
            }
            KeyCode::Char('p') | KeyCode::Char('h') | KeyCode::Left => {
                if r.chapter > 0 {
                    r.chapter -= 1;
                    r.scroll = 0;
                }
            }
            KeyCode::Char('t') if !r.toc.is_empty() => {
                // Start the cursor on the entry for the current chapter.
                r.toc_selected = r
                    .toc
                    .iter()
                    .rposition(|e| e.chapter <= r.chapter)
                    .unwrap_or(0);
                r.toc_open = true;
            }
            _ => {}
        }
    }

    /// Key handling while the reader's find input box is open.
    fn handle_reader_search_key(&mut self, key: KeyEvent) {
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        match key.code {
            KeyCode::Esc => r.search.input = None,
            KeyCode::Enter => self.submit_reader_search(),
            KeyCode::Backspace => {
                if let Some(q) = r.search.input.as_mut() {
                    q.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(q) = r.search.input.as_mut() {
                    q.push(c);
                }
            }
            _ => {}
        }
    }

    /// Run the typed query across the whole book and jump to the first match at
    /// or after the current position.
    fn submit_reader_search(&mut self) {
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        let query = r.search.input.take().unwrap_or_default().trim().to_string();
        r.search.query = query.clone();
        r.search.current = 0;
        r.search.width = r.viewport_width;
        if query.is_empty() {
            r.search.matches.clear();
            return;
        }
        r.search.matches =
            crate::reader::search_book(&r.chapters, r.viewport_width.max(1), &r.book_path, &query);
        if r.search.matches.is_empty() {
            return;
        }
        let pos = (r.chapter, r.scroll);
        r.search.current = r
            .search
            .matches
            .iter()
            .position(|m| (m.chapter, m.row) >= pos)
            .unwrap_or(0);
        focus_match(r);
    }

    /// Move the focused match by `delta` (wrapping) and scroll it into view.
    fn step_match(&mut self, delta: isize) {
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        let len = r.search.matches.len();
        if len == 0 {
            return;
        }
        let cur = r.search.current as isize;
        r.search.current = (cur + delta).rem_euclid(len as isize) as usize;
        focus_match(r);
    }

    /// Persist the reading position and return to the screen the reader was
    /// opened from.
    fn close_reader(&mut self) {
        let screen = std::mem::replace(&mut self.screen, Screen::FeedList);
        match screen {
            Screen::Reader(r) => {
                if let Some(id) = r.book_id {
                    self.outbox.push(Request::SaveProgress {
                        id,
                        progress: ReadingProgress {
                            chapter: r.chapter,
                            scroll: r.scroll,
                        },
                    });
                }
                self.screen = *r.return_to;
            }
            other => self.screen = other,
        }
    }

    /// Open a delete-confirmation for the highlighted library book.
    fn confirm_delete_selected_book(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        if !matches!(b.backend, Backend::Library(_)) {
            return;
        }
        if let Some(sel) = b.list.selected() {
            self.confirm = Some(Confirm::DeleteBook(sel));
        }
    }

    /// Delete the library book at `shown` index `shown_idx` (files + sidecar).
    fn delete_book(&mut self, shown_idx: usize) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Backend::Library(lib) = &mut b.backend else {
            return;
        };
        let Some(&book_idx) = lib.shown.get(shown_idx) else {
            return;
        };
        let id = lib.books[book_idx].id.clone();
        if let Err(e) = crate::storage::delete_book(&id) {
            self.status = format!("Delete failed: {e:#}");
            return;
        }
        lib.books.remove(book_idx);
        let query = lib.query.clone();
        lib.apply_filter(query);
        let feed = lib.build_feed(&b.title);
        let len = feed.entries.len();
        b.feed = Some(feed);
        fix_selection(&mut b.list, len);
        self.status = "Book deleted".into();
    }

    fn go_back(&mut self) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        // Library: clear an active filter first; then return to wherever we came
        // from (the catalog we jumped from, or the feed list).
        if let Backend::Library(lib) = &mut b.backend {
            if lib.query.is_some() {
                lib.apply_filter(None);
                let feed = lib.build_feed(&b.title);
                let len = feed.entries.len();
                b.feed = Some(feed);
                b.list.select(if len == 0 { None } else { Some(0) });
            } else if let Some(prev) = lib.return_to.take() {
                self.screen = *prev;
            } else {
                self.screen = Screen::FeedList;
            }
            return;
        }

        let Backend::Opds(o) = &mut b.backend else {
            return;
        };
        match o.stack.pop() {
            Some(crumb) => {
                let auth = o.auth.clone();
                o.current_url = crumb.url.clone();
                o.pending_search = None;
                b.title = crumb.title;
                b.loading = true;
                b.error = None;
                b.feed = None;
                b.restore_select = Some(crumb.selected);
                self.outbox.push(Request::Feed {
                    url: crumb.url,
                    auth,
                });
            }
            None => self.screen = Screen::FeedList,
        }
    }

    fn next_page(&mut self) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Backend::Opds(o) = &mut b.backend else {
            self.status = "No further pages".into();
            return;
        };
        let Some(next) = b.feed.as_ref().and_then(|f| f.next_link()) else {
            self.status = "No further pages".into();
            return;
        };
        let next_url = next.href.clone();
        o.stack.push(Crumb {
            url: o.current_url.clone(),
            title: b.title.clone(),
            selected: b.list.selected().unwrap_or(0),
        });
        let auth = o.auth.clone();
        o.current_url = next_url.clone();
        o.pending_search = None;
        b.loading = true;
        b.error = None;
        b.feed = None;
        b.restore_select = Some(0);
        self.outbox.push(Request::Feed {
            url: next_url,
            auth,
        });
    }

    /// Queue the lazy fetches for a newly highlighted entry: its cover image and
    /// its scraped reading metrics.
    fn request_selected(&mut self) {
        self.request_selected_image();
        self.request_selected_reading();
    }

    /// Queue an image fetch for the currently selected entry, if needed.
    fn request_selected_image(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        let Some(entry) = b.selected_entry() else {
            return;
        };
        let Some(link) = entry.image_link() else {
            return;
        };
        let url = link.href.clone();
        if self.images.contains_key(&url) {
            return;
        }
        let auth = b.auth();
        self.images.insert(url.clone(), ImageSlot::Loading);
        self.outbox.push(Request::Image { url, auth });
    }

    /// Queue a reading-metrics scrape for the currently selected entry's web
    /// page, if it has one and we haven't already fetched (or seeded) it. The
    /// metrics are Standard Ebooks-specific; other catalogs resolve to
    /// [`ReadingSlot::Unavailable`] and simply show nothing.
    fn request_selected_reading(&mut self) {
        let Screen::Browser(b) = &self.screen else {
            return;
        };
        let Some(entry) = b.selected_entry() else {
            return;
        };
        let Some(web) = entry.web_link() else {
            return;
        };
        let url = web.href.clone();
        if self.reading.contains_key(&url) {
            return;
        }
        let auth = b.auth();
        self.reading.insert(url.clone(), ReadingSlot::Loading);
        self.outbox.push(Request::Reading { url, auth });
    }

    // --- Worker responses -------------------------------------------------

    pub fn handle_response(&mut self, resp: Response) {
        match resp {
            Response::Feed { url, result } => self.on_feed(url, result),
            Response::Image { url, result, .. } => {
                let slot = match result {
                    Ok(img) => ImageSlot::Ready(Box::new(self.picker.new_resize_protocol(img))),
                    Err(_) => ImageSlot::Failed,
                };
                self.images.insert(url, slot);
            }
            Response::Download { url, kind, result } => {
                let slot = match result {
                    Ok(path) => {
                        self.status = match kind {
                            DownloadKind::Calibre => "Imported to Calibre".into(),
                            _ => format!("Saved to {}", path.display()),
                        };
                        DownloadSlot::Done(path)
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        // The full error can be long (e.g. calibredb's output),
                        // so show it in a dismissible popup, not the status line.
                        self.notice = Some(format!("Download failed\n\n{msg}"));
                        self.status = "Download failed".into();
                        DownloadSlot::Failed(msg)
                    }
                };
                // A book just saved to the library (while its catalog detail is
                // open) can now be jumped to, and joins the "downloaded" markers.
                if matches!(
                    (&slot, kind),
                    (DownloadSlot::Done(_), DownloadKind::Library)
                ) {
                    self.mark_detail_downloaded(&url);
                    self.outbox.push(Request::LibraryIds);
                }
                // A successful Calibre import changes what's in Calibre, so
                // refresh the "in Calibre" markers.
                if matches!(
                    (&slot, kind),
                    (DownloadSlot::Done(_), DownloadKind::Calibre)
                ) {
                    let req = self.calibre_ids_request();
                    self.outbox.push(req);
                }
                self.downloads.insert(url, slot);
            }
            Response::Search { query, result } => self.on_search(query, result),
            Response::Reading { url, result } => {
                let slot = match result {
                    Ok(stats) => ReadingSlot::Ready(stats),
                    Err(_) => ReadingSlot::Unavailable,
                };
                self.reading.insert(url, slot);
            }
            Response::Library { result } => self.on_library(result),
            Response::LibraryIds { result } => {
                if let Ok(ids) = result {
                    self.downloaded_ids = ids.into_iter().collect();
                }
            }
            Response::CalibreIds { result } => match result {
                Ok(ids) => {
                    self.calibre_loaded = true;
                    self.calibre_ids = ids.into_iter().collect();
                }
                Err(err) => {
                    // Leave `calibre_loaded` false so the next catalog open
                    // retries — a transient failure (e.g. the library briefly
                    // locked by the Calibre GUI) can then self-heal. Surface a
                    // hint for the lock case instead of failing silently; stay
                    // quiet when calibredb simply isn't installed.
                    if let Some(hint) = calibre_error_hint(&err) {
                        self.status = hint;
                    }
                }
            },
            Response::Book { result } => self.on_book(result),
        }
    }

    fn on_book(&mut self, result: anyhow::Result<BookContent>) {
        let Screen::Reader(r) = &mut self.screen else {
            return;
        };
        r.loading = false;
        match result {
            Ok(book) => {
                if !book.title.is_empty() {
                    r.title = book.title;
                }
                r.chapters = book.chapters;
                r.toc = book.toc;
                r.chapter = r.chapter.min(r.chapters.len().saturating_sub(1));
                r.rendered_for = None;
                r.error = None;
            }
            Err(e) => r.error = Some(format!("{e:#}")),
        }
    }

    fn on_library(&mut self, result: anyhow::Result<Vec<LibraryBook>>) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Backend::Library(lib) = &mut b.backend else {
            return;
        };
        b.loading = false;
        match result {
            Ok(books) => {
                // Seed cached reading metrics so the detail page needs no network.
                for book in &books {
                    if let (Some(web), Some(stats)) = (&book.meta.web_url, &book.meta.reading) {
                        self.reading
                            .entry(web.clone())
                            .or_insert_with(|| ReadingSlot::Ready(stats.clone()));
                    }
                }
                lib.books = books;
                lib.apply_filter(None);
                let feed = lib.build_feed(&b.title);
                let len = feed.entries.len();
                // If we arrived here to open a specific book, find its position
                // in the (unfiltered) shown list and jump straight to its detail.
                let target = lib
                    .open_target
                    .take()
                    .and_then(|id| lib.shown.iter().position(|&i| lib.books[i].id == id));
                let select = match (target, len) {
                    (Some(pos), _) => Some(pos),
                    (None, 0) => None,
                    (None, _) => Some(0),
                };
                b.list.select(select);
                b.detail = target.map(|pos| DetailState {
                    entry_index: pos,
                    format: 0,
                    library_id: None,
                });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected();
            }
            Err(e) => {
                b.feed = None;
                b.error = Some(format!("{e:#}"));
            }
        }
    }

    fn on_search(&mut self, query: String, result: anyhow::Result<(String, Feed)>) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Backend::Opds(o) = &mut b.backend else {
            return;
        };
        // Ignore results for a search the user has since abandoned.
        if o.pending_search.as_deref() != Some(query.as_str()) {
            return;
        }
        o.pending_search = None;
        b.loading = false;
        match result {
            Ok((url, feed)) => {
                o.current_url = url;
                if let Some(s) = feed.search_link() {
                    o.search_url = Some(s.href.clone());
                }
                let len = feed.entries.len();
                let sel = b
                    .restore_select
                    .take()
                    .unwrap_or(0)
                    .min(len.saturating_sub(1));
                b.list.select(if len == 0 { None } else { Some(sel) });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected();
            }
            Err(e) => {
                b.feed = None;
                b.error = Some(format!("{e:#}"));
            }
        }
    }

    fn on_feed(&mut self, url: String, result: anyhow::Result<Feed>) {
        let Screen::Browser(b) = &mut self.screen else {
            return;
        };
        let Backend::Opds(o) = &mut b.backend else {
            return;
        };
        // Ignore responses for feeds we've already navigated away from.
        if o.current_url != url {
            return;
        }
        b.loading = false;
        match result {
            Ok(feed) => {
                if b.title.is_empty() {
                    b.title = feed.title.clone();
                }
                // Remember the catalog's search endpoint so `/` works from here on.
                if let Some(s) = feed.search_link() {
                    o.search_url = Some(s.href.clone());
                }
                let len = feed.entries.len();
                let sel = b
                    .restore_select
                    .take()
                    .unwrap_or(0)
                    .min(len.saturating_sub(1));
                b.list.select(if len == 0 { None } else { Some(sel) });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected();
            }
            Err(e) => {
                b.feed = None;
                b.error = Some(format!("{e:#}"));
            }
        }
    }
}

/// Move a list selection by `delta`, clamping within `[0, len)`.
fn move_sel(state: &mut ListState, len: usize, delta: i32) {
    if len == 0 {
        state.select(None);
        return;
    }
    let cur = state.selected().unwrap_or(0) as i32;
    let next = (cur + delta).clamp(0, len as i32 - 1);
    state.select(Some(next as usize));
}

/// Ensure a list selection stays valid after the list shrinks.
fn fix_selection(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
    } else {
        let sel = state.selected().unwrap_or(0).min(len - 1);
        state.select(Some(sel));
    }
}

/// Modifier-aware quit check (Ctrl-C) used by the main loop.
pub fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book(id: &str, title: &str, author: &str) -> LibraryBook {
        let entry = Entry {
            title: title.to_string(),
            authors: vec![crate::opds::Author {
                name: author.to_string(),
                uri: None,
            }],
            ..Default::default()
        };
        LibraryBook {
            id: id.to_string(),
            entry,
            meta: LibraryEntry::default(),
        }
    }

    fn library(books: Vec<LibraryBook>) -> LibraryBackend {
        let mut lib = LibraryBackend {
            books,
            shown: Vec::new(),
            query: None,
            open_target: None,
            return_to: None,
        };
        lib.apply_filter(None);
        lib
    }

    #[test]
    fn filter_matches_title_and_author_case_insensitively() {
        let mut lib = library(vec![
            book("a", "The Professor's House", "Willa Cather"),
            book("b", "My Antonia", "Willa Cather"),
            book("c", "Moby Dick", "Herman Melville"),
        ]);
        assert_eq!(lib.shown.len(), 3);

        // Matches a title fragment, ignoring case.
        lib.apply_filter(Some("moby".to_string()));
        assert_eq!(lib.shown, vec![2]);

        // Matches an author across multiple books.
        lib.apply_filter(Some("cather".to_string()));
        assert_eq!(lib.shown, vec![0, 1]);

        // Clearing the filter restores everything.
        lib.apply_filter(None);
        assert_eq!(lib.shown.len(), 3);
    }

    #[test]
    fn build_feed_reflects_filtered_selection() {
        let lib = {
            let mut lib = library(vec![
                book("a", "Alpha", "Author One"),
                book("b", "Beta", "Author Two"),
            ]);
            lib.apply_filter(Some("beta".to_string()));
            lib
        };
        let feed = lib.build_feed("Downloaded books");
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].title, "Beta");
    }
}
