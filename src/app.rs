//! Application state, input handling, and response handling.

use std::collections::HashMap;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;

use crate::opds::{Entry, Feed};
use crate::storage::{Config, Feed as FeedConfig};
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

/// State while browsing a single OPDS catalog.
pub struct BrowserState {
    pub auth: Auth,
    pub stack: Vec<Crumb>,
    pub feed: Option<Feed>,
    pub list: ListState,
    pub loading: bool,
    pub error: Option<String>,
    pub current_url: String,
    pub current_title: String,
    /// When `Some`, the full-page detail view for a publication is open.
    pub detail: Option<DetailState>,
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
}

/// Loaded state of a cover image, keyed by image URL.
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

/// Which screen the UI is showing.
pub enum Screen {
    FeedList,
    Form(FormState),
    Browser(BrowserState),
}

pub struct App {
    pub config: Config,
    pub screen: Screen,
    pub feed_list: ListState,
    /// Feed id pending delete-confirmation, if the confirm popup is open.
    pub confirm_delete: Option<u64>,
    pub status: String,
    pub should_quit: bool,
    /// Outgoing network requests, drained by the main loop after each update.
    pub outbox: Vec<Request>,
    pub images: HashMap<String, ImageSlot>,
    /// Book downloads in progress or completed, keyed by acquisition URL.
    pub downloads: HashMap<String, DownloadSlot>,
    pub picker: Picker,
}

impl App {
    pub fn new(config: Config, picker: Picker) -> Self {
        let mut feed_list = ListState::default();
        if !config.feeds.is_empty() {
            feed_list.select(Some(0));
        }
        App {
            config,
            screen: Screen::FeedList,
            feed_list,
            confirm_delete: None,
            status: String::new(),
            should_quit: false,
            outbox: Vec::new(),
            images: HashMap::new(),
            downloads: HashMap::new(),
            picker,
        }
    }

    // --- Input ------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        // A popup confirmation intercepts all keys.
        if self.confirm_delete.is_some() {
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
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(id) = self.confirm_delete.take() {
                    self.config.remove(id);
                    let _ = self.config.save();
                    let len = self.config.feeds.len();
                    fix_selection(&mut self.feed_list, len);
                    self.status = "Feed deleted".into();
                }
            }
            _ => self.confirm_delete = None,
        }
    }

    fn handle_feed_list_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('j') | KeyCode::Down => move_sel(&mut self.feed_list, self.config.feeds.len(), 1),
            KeyCode::Char('k') | KeyCode::Up => move_sel(&mut self.feed_list, self.config.feeds.len(), -1),
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
                    self.confirm_delete = Some(feed.id);
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => self.open_selected_feed(),
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
            _ => {}
        }
    }

    // --- Feed list actions ------------------------------------------------

    fn selected_config_feed(&self) -> Option<&FeedConfig> {
        self.config.feeds.get(self.feed_list.selected()?)
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
        if self.feed_list.selected().is_none() && !self.config.feeds.is_empty() {
            self.feed_list.select(Some(0));
        }
        self.screen = Screen::FeedList;
    }

    fn open_selected_feed(&mut self) {
        let Some(feed) = self.selected_config_feed() else { return };
        let auth = feed.auth();
        let url = feed.url.clone();
        let title = feed.name.clone();
        let mut list = ListState::default();
        list.select(Some(0));
        let browser = BrowserState {
            auth: auth.clone(),
            stack: Vec::new(),
            feed: None,
            list,
            loading: true,
            error: None,
            current_url: url.clone(),
            current_title: title,
            detail: None,
            restore_select: None,
        };
        self.screen = Screen::Browser(browser);
        self.outbox.push(Request::Feed { url, auth });
    }

    // --- Browser navigation ----------------------------------------------

    fn follow_selected(&mut self) {
        let Screen::Browser(b) = &mut self.screen else { return };
        let Some(entry) = b.selected_entry() else { return };
        let Some(link) = entry.nav_link() else {
            // A publication entry: open its detail / download view instead.
            if entry.acquisition_links().next().is_some() {
                let entry_index = b.list.selected().unwrap_or(0);
                b.detail = Some(DetailState { entry_index, format: 0 });
            } else {
                self.status = "No catalog link or downloads for this entry".into();
            }
            return;
        };
        let next_url = link.href.clone();
        let next_title = entry.title.clone();
        b.stack.push(Crumb {
            url: b.current_url.clone(),
            title: b.current_title.clone(),
            selected: b.list.selected().unwrap_or(0),
        });
        let auth = b.auth.clone();
        b.current_url = next_url.clone();
        b.current_title = next_title;
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
            KeyCode::Enter | KeyCode::Char('d') => self.download_selected_format(),
            _ => {}
        }
    }

    /// Queue a download of the format highlighted in the detail view.
    fn download_selected_format(&mut self) {
        let Some((url, auth)) = self.selected_format_target() else {
            return;
        };
        // Don't re-queue a download that's already running.
        if matches!(self.downloads.get(&url), Some(DownloadSlot::Pending)) {
            self.status = "Already downloading…".into();
            return;
        }
        self.downloads.insert(url.clone(), DownloadSlot::Pending);
        self.status = "Downloading…".into();
        self.outbox.push(Request::Download { url, auth });
    }

    /// The `(url, auth)` of the acquisition link selected in the detail view.
    fn selected_format_target(&self) -> Option<(String, Auth)> {
        let Screen::Browser(b) = &self.screen else { return None };
        let detail = b.detail.as_ref()?;
        let entry = b.feed.as_ref()?.entries.get(detail.entry_index)?;
        let link = entry.acquisition_links().nth(detail.format)?;
        Some((link.href.clone(), b.auth.clone()))
    }

    fn go_back(&mut self) {
        let Screen::Browser(b) = &mut self.screen else { return };
        match b.stack.pop() {
            Some(crumb) => {
                let auth = b.auth.clone();
                b.current_url = crumb.url.clone();
                b.current_title = crumb.title;
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
        let Some(feed) = &b.feed else { return };
        let Some(next) = feed.next_link() else {
            self.status = "No further pages".into();
            return;
        };
        let next_url = next.href.clone();
        b.stack.push(Crumb {
            url: b.current_url.clone(),
            title: b.current_title.clone(),
            selected: b.list.selected().unwrap_or(0),
        });
        let auth = b.auth.clone();
        b.current_url = next_url.clone();
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
        let auth = b.auth.clone();
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
        }
    }

    fn on_feed(&mut self, url: String, result: anyhow::Result<Feed>) {
        let Screen::Browser(b) = &mut self.screen else { return };
        // Ignore responses for feeds we've already navigated away from.
        if b.current_url != url {
            return;
        }
        b.loading = false;
        match result {
            Ok(feed) => {
                if b.current_title.is_empty() {
                    b.current_title = feed.title.clone();
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
