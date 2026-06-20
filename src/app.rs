//! Application state, input handling, and response handling.

use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::opds::{Entry, Feed};
use crate::reading::ReadingStats;
use crate::storage::{Config, Feed as FeedConfig, LibraryBook, LibraryEntry};
use crate::worker::{Request, Response};

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
            entries: self.shown.iter().map(|&i| self.books[i].entry.clone()).collect(),
            links: Vec::new(),
        }
    }
}

/// Whether a library book matches a (lowercased) search query.
fn book_matches(book: &LibraryBook, q: &str) -> bool {
    let e = &book.entry;
    e.title.to_lowercase().contains(q)
        || e.authors.iter().any(|a| a.to_lowercase().contains(q))
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

/// Which screen the UI is showing.
pub enum Screen {
    FeedList,
    Form(FormState),
    Browser(Box<BrowserState>),
}

pub struct App {
    pub config: Config,
    pub screen: Screen,
    pub feed_list: ListState,
    /// A pending confirmation popup (delete feed / delete book), if open.
    pub confirm: Option<Confirm>,
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
            status: String::new(),
            should_quit: false,
            outbox: Vec::new(),
            images: HashMap::new(),
            downloads: HashMap::new(),
            reading: HashMap::new(),
            picker,
        }
    }

    // --- Input ------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // A popup confirmation intercepts all keys.
        if self.confirm.is_some() {
            self.handle_confirm_key(key);
            return;
        }
        match &mut self.screen {
            Screen::FeedList => self.handle_feed_list_key(key),
            Screen::Form(_) => self.handle_form_key(key),
            Screen::Browser(_) => self.handle_browser_key(key),
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
        let Screen::Form(form) = &mut self.screen else { return };
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
        let Screen::Browser(b) = &mut self.screen else { return };
        let len = b.feed.as_ref().map_or(0, |f| f.entries.len());
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.screen = Screen::FeedList,
            KeyCode::Char('j') | KeyCode::Down => {
                move_sel(&mut b.list, len, 1);
                self.request_selected_image();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                move_sel(&mut b.list, len, -1);
                self.request_selected_image();
            }
            KeyCode::Char('g') => {
                if len > 0 {
                    b.list.select(Some(0));
                    self.request_selected_image();
                }
            }
            KeyCode::Char('G') => {
                if len > 0 {
                    b.list.select(Some(len - 1));
                    self.request_selected_image();
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
        let Screen::Browser(b) = &mut self.screen else { return };
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
        let Screen::Browser(b) = &mut self.screen else { return };
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
        let Screen::Browser(b) = &mut self.screen else { return };
        let query = b.search_query.take().unwrap_or_default().trim().to_string();
        match &mut b.backend {
            Backend::Library(lib) => {
                let filter = if query.is_empty() { None } else { Some(query) };
                lib.apply_filter(filter);
                let feed = lib.build_feed(&b.title);
                let len = feed.entries.len();
                b.feed = Some(feed);
                b.list.select(if len == 0 { None } else { Some(0) });
                self.request_selected_image();
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
                self.outbox.push(Request::Search { desc_url, query, auth });
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
        let Screen::Form(form) = &mut self.screen else { return };
        let url = form.fields[1].trim().to_string();
        if url.is_empty() {
            form.error = Some("URL is required".into());
            return;
        }
        let name = {
            let n = form.fields[0].trim();
            if n.is_empty() { url.clone() } else { n.to_string() }
        };
        let opt = |s: &str| {
            let s = s.trim();
            if s.is_empty() { None } else { Some(s.to_string()) }
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
            self.open_library();
            return;
        }
        let Some(feed) = self.config.feeds.get(sel - 1) else { return };
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
    }

    /// Open the local downloaded-book library.
    fn open_library(&mut self) {
        let mut list = ListState::default();
        list.select(Some(0));
        let browser = BrowserState {
            backend: Backend::Library(LibraryBackend {
                books: Vec::new(),
                shown: Vec::new(),
                query: None,
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
        let Screen::Browser(b) = &mut self.screen else { return };
        let Some(entry) = b.selected_entry() else { return };
        // Read everything off `entry` before mutating `b` (entry borrows b).
        let entry_index = b.list.selected().unwrap_or(0);
        let nav = entry.nav_link().map(|l| (l.href.clone(), entry.title.clone()));
        let has_acquisition = entry.acquisition_links().next().is_some();
        let web_url = entry.web_link().map(|l| l.href.clone());

        // A publication entry: open its detail / download view.
        if nav.is_none() {
            if has_acquisition {
                b.detail = Some(DetailState { entry_index, format: 0 });
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
        let Some((next_url, next_title)) = nav else { return };
        let Backend::Opds(o) = &mut b.backend else { return };
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
        self.outbox.push(Request::Feed { url: next_url, auth });
    }

    /// Key handling while the publication detail view is open.
    fn handle_detail_key(&mut self, key: KeyEvent) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Some(detail) = b.detail.as_mut() else { return };
        let formats = b
            .feed
            .as_ref()
            .and_then(|f| f.entries.get(detail.entry_index))
            .map_or(0, |e| e.acquisition_links().count());
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
            KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('o') => {
                self.activate_selected_format()
            }
            _ => {}
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

    /// Queue a download of the format highlighted in the detail view, carrying
    /// the metadata the worker needs to persist a library sidecar.
    fn download_selected_format(&mut self) {
        let Screen::Browser(b) = &self.screen else { return };
        let Some(detail) = b.detail.as_ref() else { return };
        let Some(entry) = b.feed.as_ref().and_then(|f| f.entries.get(detail.entry_index)) else {
            return;
        };
        let Some(link) = entry.acquisition_links().nth(detail.format) else { return };
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
        self.downloads.insert(url.clone(), DownloadSlot::Pending);
        self.status = "Downloading…".into();
        self.outbox.push(Request::Download {
            meta: Box::new(meta),
            url,
            mime,
            length,
            cover_url,
            auth,
        });
    }

    /// Open the highlighted local format in the OS default reader.
    fn open_selected_format(&mut self) {
        let Screen::Browser(b) = &self.screen else { return };
        let Some(detail) = b.detail.as_ref() else { return };
        let Some(entry) = b.feed.as_ref().and_then(|f| f.entries.get(detail.entry_index)) else {
            return;
        };
        let Some(link) = entry.acquisition_links().nth(detail.format) else { return };
        let path = PathBuf::from(&link.href);
        match crate::storage::open_in_reader(&path) {
            Ok(()) => self.status = format!("Opened {}", path.display()),
            Err(e) => self.status = format!("Open failed: {e:#}"),
        }
    }

    /// Open a delete-confirmation for the highlighted library book.
    fn confirm_delete_selected_book(&mut self) {
        let Screen::Browser(b) = &self.screen else { return };
        if !matches!(b.backend, Backend::Library(_)) {
            return;
        }
        if let Some(sel) = b.list.selected() {
            self.confirm = Some(Confirm::DeleteBook(sel));
        }
    }

    /// Delete the library book at `shown` index `shown_idx` (files + sidecar).
    fn delete_book(&mut self, shown_idx: usize) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Backend::Library(lib) = &mut b.backend else { return };
        let Some(&book_idx) = lib.shown.get(shown_idx) else { return };
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
        let Screen::Browser(b) = &mut self.screen else { return };
        // Library: clear an active filter first; otherwise leave to the feeds.
        if let Backend::Library(lib) = &mut b.backend {
            if lib.query.is_some() {
                lib.apply_filter(None);
                let feed = lib.build_feed(&b.title);
                let len = feed.entries.len();
                b.feed = Some(feed);
                b.list.select(if len == 0 { None } else { Some(0) });
            } else {
                self.screen = Screen::FeedList;
            }
            return;
        }

        let Backend::Opds(o) = &mut b.backend else { return };
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
                self.outbox.push(Request::Feed { url: crumb.url, auth });
            }
            None => self.screen = Screen::FeedList,
        }
    }

    fn next_page(&mut self) {
        let Screen::Browser(b) = &mut self.screen else { return };
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
        self.outbox.push(Request::Feed { url: next_url, auth });
    }

    /// Queue an image fetch for the currently selected entry, if needed.
    fn request_selected_image(&mut self) {
        let Screen::Browser(b) = &self.screen else { return };
        let Some(entry) = b.selected_entry() else { return };
        let Some(link) = entry.image_link() else { return };
        let url = link.href.clone();
        if self.images.contains_key(&url) {
            return;
        }
        let auth = b.auth();
        self.images.insert(url.clone(), ImageSlot::Loading);
        self.outbox.push(Request::Image { url, auth });
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
            Response::Download { url, result } => {
                let slot = match result {
                    Ok(path) => {
                        self.status = format!("Saved to {}", path.display());
                        DownloadSlot::Done(path)
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        self.status = format!("Download failed: {msg}");
                        DownloadSlot::Failed(msg)
                    }
                };
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
        }
    }

    fn on_library(&mut self, result: anyhow::Result<Vec<LibraryBook>>) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Backend::Library(lib) = &mut b.backend else { return };
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
                b.list.select(if len == 0 { None } else { Some(0) });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected_image();
            }
            Err(e) => {
                b.feed = None;
                b.error = Some(format!("{e:#}"));
            }
        }
    }

    fn on_search(&mut self, query: String, result: anyhow::Result<(String, Feed)>) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Backend::Opds(o) = &mut b.backend else { return };
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
                let sel = b.restore_select.take().unwrap_or(0).min(len.saturating_sub(1));
                b.list.select(if len == 0 { None } else { Some(sel) });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected_image();
            }
            Err(e) => {
                b.feed = None;
                b.error = Some(format!("{e:#}"));
            }
        }
    }

    fn on_feed(&mut self, url: String, result: anyhow::Result<Feed>) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Backend::Opds(o) = &mut b.backend else { return };
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
                let sel = b.restore_select.take().unwrap_or(0).min(len.saturating_sub(1));
                b.list.select(if len == 0 { None } else { Some(sel) });
                b.feed = Some(feed);
                b.error = None;
                self.request_selected_image();
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
            authors: vec![author.to_string()],
            ..Default::default()
        };
        LibraryBook {
            id: id.to_string(),
            entry,
            meta: LibraryEntry::default(),
        }
    }

    fn library(books: Vec<LibraryBook>) -> LibraryBackend {
        let mut lib = LibraryBackend { books, shown: Vec::new(), query: None };
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
