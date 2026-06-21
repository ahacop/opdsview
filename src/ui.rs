//! Terminal rendering for all screens.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui_image::StatefulImage;

use crate::app::{
    App, Backend, BrowserState, Confirm, DOWNLOAD_DESTS, DownloadMenu, DownloadSlot, FORM_LABELS,
    FormState, ImageSlot, ReaderState, ReadingSlot, Screen,
};
use crate::opds::Entry;
use crate::reader::{Block as ContentBlock, block_height, render_chapter, search_book};
use crate::reading::ReadingStats;
use crate::worker::Request;

const ACCENT: Color = Color::Cyan;

/// Rows of context kept between the list cursor and the top/bottom edge, so the
/// viewport scrolls a few rows early instead of pinning the selection to the
/// border (degrades gracefully when the list is shorter than the viewport).
const LIST_SCROLL_PADDING: usize = 5;

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    render_title_bar(frame, chunks[0], &app.screen);

    match &app.screen {
        Screen::FeedList => {
            render_feed_list(frame, chunks[1], app);
            render_help(
                frame,
                chunks[2],
                "↑↓ move   n new   e edit   d delete   Enter open   q quit",
            );
        }
        Screen::Form(form) => {
            render_form(frame, chunks[1], form);
            render_help(
                frame,
                chunks[2],
                "Tab/↑↓ field   type to edit   Enter save   Esc cancel",
            );
        }
        Screen::Browser(b) => {
            let library = matches!(b.backend, Backend::Library(_));
            let help = if b.search_query.is_some() {
                "type to search   Enter run   Esc cancel"
            } else if b.detail.is_some() {
                if library {
                    "Enter/o read   x external   ↑↓ format   ⌫/h/Esc back"
                } else if b
                    .detail
                    .as_ref()
                    .and_then(|d| d.library_id.as_ref())
                    .is_some()
                {
                    "↑↓ format   Enter/d download…   g open copy   ⌫/h/Esc back"
                } else {
                    "↑↓ format   Enter/d download…   ⌫/h/Esc back"
                }
            } else if library {
                "↑↓ move   Enter open   / search   d delete   ⌫/h clear   q feeds"
            } else {
                "↑↓ move   Enter open   / search   ⌫/h back   n next   q feeds"
            };
            render_browser(
                frame,
                chunks[1],
                &app.screen,
                &mut app.images,
                &app.downloads,
                &app.reading,
                &app.downloaded_ids,
                &app.calibre_ids,
            );
            render_help(frame, chunks[2], help);
        }
        // The reader needs mutable access to cache rendered blocks and queue
        // image loads, so it is rendered below this (immutable) match.
        Screen::Reader(_) => {}
    }

    if matches!(app.screen, Screen::Reader(_)) {
        render_reader_screen(frame, chunks[1], chunks[2], app);
    }

    if !app.status.is_empty() {
        // Status text overrides the help line briefly.
        render_help(frame, chunks[2], &app.status);
    }

    if let Screen::Browser(b) = &app.screen
        && let Some(query) = &b.search_query
    {
        render_search_input(frame, area, query, " Search catalog ");
    }

    if let Screen::Reader(r) = &app.screen
        && let Some(query) = &r.search.input
    {
        render_search_input(frame, area, query, " Find in book ");
    }

    if let Some(confirm) = &app.confirm {
        render_confirm_delete(frame, area, app, confirm);
    }

    if let Some(menu) = &app.download_menu {
        render_download_menu(frame, area, menu);
        render_help(
            frame,
            chunks[2],
            "↑↓ choose destination   Enter confirm   Esc cancel",
        );
    }

    if let Some(msg) = &app.notice {
        render_notice(frame, area, msg);
        render_help(frame, chunks[2], "press any key to dismiss");
    }
}

/// A centered one-line text input box for entering a search query.
fn render_search_input(frame: &mut Frame, area: Rect, query: &str, title: &str) {
    let popup = centered_rect(60, 3, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(title.to_string());
    let p = Paragraph::new(Line::from(format!(" {query}█"))).block(block);
    frame.render_widget(p, popup);
}

fn render_title_bar(frame: &mut Frame, area: Rect, screen: &Screen) {
    let title = match screen {
        Screen::FeedList => "  opdsview — Catalogs".to_string(),
        Screen::Form(f) if f.editing_id.is_some() => "  opdsview — Edit feed".to_string(),
        Screen::Form(_) => "  opdsview — New feed".to_string(),
        Screen::Browser(b) => format!("  opdsview — {}", b.title),
        Screen::Reader(r) => format!("  opdsview — {}", r.title),
    };
    let bar = Paragraph::new(Line::from(Span::styled(
        title,
        Style::default()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )))
    .style(Style::default().bg(ACCENT));
    frame.render_widget(bar, area);
}

fn render_help(frame: &mut Frame, area: Rect, text: &str) {
    let help = Paragraph::new(Line::from(Span::styled(
        format!(" {text}"),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(help, area);
}

// --- Feed list -----------------------------------------------------------

fn render_feed_list(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Saved feeds ");

    // Row 0 is always the pinned local library; saved feeds follow.
    let mut items: Vec<ListItem> = vec![ListItem::new(vec![
        Line::from(Span::styled(
            "📚 Downloaded books",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  Your local library",
            Style::default().fg(Color::DarkGray),
        )),
    ])];

    items.extend(app.config.feeds.iter().map(|f| {
        let lock = if f.username.is_some() { " 🔒" } else { "" };
        ListItem::new(vec![
            Line::from(Span::styled(
                format!("{}{lock}", f.name),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("  {}", f.url),
                Style::default().fg(Color::DarkGray),
            )),
        ])
    }));

    if app.config.feeds.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "  Press 'n' to add an OPDS catalog, e.g. https://standardebooks.org/opds",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))));
    }

    let list = List::new(items)
        .block(block)
        .scroll_padding(LIST_SCROLL_PADDING)
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, area, &mut app.feed_list);
}

// --- Form ----------------------------------------------------------------

fn render_form(frame: &mut Frame, area: Rect, form: &FormState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Feed details ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut constraints = vec![Constraint::Length(3); 4];
    constraints.push(Constraint::Min(1));
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .margin(1)
        .split(inner);

    for (i, label) in FORM_LABELS.iter().enumerate() {
        let focused = form.focus == i;
        let value = if i == 3 {
            "•".repeat(form.fields[i].chars().count())
        } else {
            form.fields[i].clone()
        };
        let cursor = if focused { "█" } else { "" };
        let border_style = if focused {
            Style::default().fg(ACCENT)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let field = Paragraph::new(Line::from(format!("{value}{cursor}"))).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(border_style)
                .title(format!(" {label} ")),
        );
        frame.render_widget(field, rows[i]);
    }

    if let Some(err) = &form.error {
        let err = Paragraph::new(Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        )));
        frame.render_widget(err, rows[4]);
    }
}

// --- Browser -------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn render_browser(
    frame: &mut Frame,
    area: Rect,
    screen: &Screen,
    images: &mut HashMap<String, ImageSlot>,
    downloads: &HashMap<String, DownloadSlot>,
    reading: &HashMap<String, ReadingSlot>,
    downloaded: &HashSet<String>,
    calibre: &HashSet<String>,
) {
    let Screen::Browser(b) = screen else { return };

    // The detail "show page" takes over the whole browser area when open.
    if b.detail.is_some() {
        render_detail_page(frame, area, b, images, downloads, reading, calibre);
        return;
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    render_entry_list(frame, panes[0], b, downloaded, calibre);
    render_detail(frame, panes[1], b, images, reading);
}

fn render_entry_list(
    frame: &mut Frame,
    area: Rect,
    b: &BrowserState,
    downloaded: &HashSet<String>,
    calibre: &HashSet<String>,
) {
    let title = format!(" {} ", crumb_path(b));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title);

    if b.loading {
        let p = Paragraph::new("\n  Loading…").block(block);
        frame.render_widget(p, area);
        return;
    }
    if let Some(err) = &b.error {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Failed to load feed",
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(format!("  {err}")),
        ])
        .wrap(Wrap { trim: true })
        .block(block);
        frame.render_widget(p, area);
        return;
    }

    let entries = b.feed.as_ref().map(|f| f.entries.as_slice()).unwrap_or(&[]);
    if entries.is_empty() {
        let msg = match &b.backend {
            Backend::Library(lib) if lib.query.is_some() => "\n  No books match your search.",
            Backend::Library(_) => {
                "\n  No downloaded books yet.\n\n  Browse a catalog and press Enter on a book to download it."
            }
            Backend::Opds(_) => "\n  (empty feed)",
        };
        let p = Paragraph::new(msg).wrap(Wrap { trim: false }).block(block);
        frame.render_widget(p, area);
        return;
    }

    // Only a remote catalog gets library/Calibre status markers; in the local
    // library every book is by definition already downloaded.
    let mark_status = matches!(b.backend, Backend::Opds(_));
    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            ListItem::new(Line::from(entry_row_spans(
                e,
                mark_status,
                downloaded,
                calibre,
            )))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .scroll_padding(LIST_SCROLL_PADDING)
        .highlight_style(
            Style::default()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");
    let mut state = b.list;
    frame.render_stateful_widget(list, area, &mut state);
}

/// Build the spans for one entry's list row: a status gutter then the title.
///
/// For catalog entries (`status`) the gutter is two independent slots — the
/// local opdsview library (green ✓) and Calibre (cyan ◆) — so a book in both
/// shows both, rather than one state hiding the other. Navigation entries get an
/// accent ▸; local-library rows (no status, every book is already downloaded)
/// get a plain bullet.
fn entry_row_spans(
    entry: &Entry,
    status: bool,
    downloaded: &HashSet<String>,
    calibre: &HashSet<String>,
) -> Vec<Span<'static>> {
    let title = Span::raw(entry.title.clone());
    if entry.is_navigation() {
        return vec![Span::styled("▸  ", Style::default().fg(ACCENT)), title];
    }
    if !status {
        return vec![Span::styled("• ", Style::default().fg(Color::Reset)), title];
    }
    let lib = if is_downloaded(entry, downloaded) {
        Span::styled("✓", Style::default().fg(Color::Green))
    } else {
        Span::raw(" ")
    };
    let cal = if is_in_calibre(entry, calibre) {
        Span::styled("◆", Style::default().fg(Color::Cyan))
    } else {
        Span::raw(" ")
    };
    vec![lib, cal, Span::raw(" "), title]
}

/// Whether a catalog entry's book is already in the local library, found by
/// matching its computed id against the set of downloaded ids.
fn is_downloaded(entry: &Entry, downloaded: &HashSet<String>) -> bool {
    if downloaded.is_empty() {
        return false;
    }
    let authors: Vec<String> = entry.author_names().map(str::to_string).collect();
    downloaded.contains(&crate::storage::book_id(&authors, &entry.title))
}

/// Whether a catalog entry's book is present in the user's Calibre library,
/// matched by a shared identifier (ISBN or URL) or, failing that, author+title
/// (see [`crate::storage::in_calibre_index`]).
fn is_in_calibre(entry: &Entry, calibre: &HashSet<String>) -> bool {
    if calibre.is_empty() {
        return false;
    }
    let authors: Vec<String> = entry.author_names().map(str::to_string).collect();
    crate::storage::in_calibre_index(calibre, &authors, &entry.title, &entry.identifier_tokens())
}

fn render_detail(
    frame: &mut Frame,
    area: Rect,
    b: &BrowserState,
    images: &mut HashMap<String, ImageSlot>,
    reading: &HashMap<String, ReadingSlot>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Details ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(entry) = b.selected_entry() else {
        return;
    };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Min(3)])
        .split(inner);

    let reading_slot = entry.web_link().and_then(|l| reading.get(&l.href));
    render_cover(frame, split[0], entry, images);
    render_entry_text(frame, split[1], entry, reading_slot);
}

fn render_cover(
    frame: &mut Frame,
    area: Rect,
    entry: &Entry,
    images: &mut HashMap<String, ImageSlot>,
) {
    let Some(link) = entry.image_link() else {
        let p = Paragraph::new(Span::styled(
            "no cover",
            Style::default().fg(Color::DarkGray),
        ))
        .alignment(Alignment::Center);
        frame.render_widget(p, center_v(area));
        return;
    };

    match images.get_mut(&link.href) {
        Some(ImageSlot::Ready(proto)) => {
            frame.render_stateful_widget(StatefulImage::new(), area, proto.as_mut());
        }
        Some(ImageSlot::Loading) | None => {
            let p = Paragraph::new(Span::styled(
                "loading cover…",
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Center);
            frame.render_widget(p, center_v(area));
        }
        Some(ImageSlot::Failed) => {
            let p = Paragraph::new(Span::styled(
                "cover unavailable",
                Style::default().fg(Color::DarkGray),
            ))
            .alignment(Alignment::Center);
            frame.render_widget(p, center_v(area));
        }
    }
}

fn render_entry_text(frame: &mut Frame, area: Rect, entry: &Entry, reading: Option<&ReadingSlot>) {
    let mut lines = vec![Line::from(Span::styled(
        entry.title.clone(),
        Style::default().add_modifier(Modifier::BOLD).fg(ACCENT),
    ))];

    if !entry.authors.is_empty() {
        lines.push(Line::from(Span::styled(
            entry.author_names().collect::<Vec<_>>().join(", "),
            Style::default().add_modifier(Modifier::ITALIC),
        )));
    }

    push_meta_lines(&mut lines, entry);
    push_reading_lines(&mut lines, reading);
    lines.push(Line::from(""));

    if let Some(summary) = &entry.summary {
        lines.push(Line::from(summary.clone()));
        lines.push(Line::from(""));
    }

    let downloads: Vec<&crate::opds::Link> = entry.acquisition_links().collect();
    if !downloads.is_empty() {
        lines.push(Line::from(Span::styled(
            "Available formats:",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for link in downloads {
            lines.push(Line::from(format!("  • {}", format_label(link))));
        }
        lines.push(Line::from(Span::styled(
            "Press Enter to view & download",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

/// Append the compact metadata block (date, language, publisher, subjects)
/// shared by the browse detail pane.
fn push_meta_lines(lines: &mut Vec<Line>, entry: &Entry) {
    let mut meta: Vec<(&str, String)> = Vec::new();
    if let Some(date) = entry.published.as_deref() {
        meta.push(("Published", date_only(date).to_string()));
    }
    if let Some(lang) = &entry.language {
        meta.push(("Language", lang.clone()));
    }
    if let Some(pubr) = &entry.publisher {
        meta.push(("Publisher", pubr.clone()));
    }
    for (label, value) in meta {
        lines.push(Line::from(vec![
            Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
            Span::raw(value),
        ]));
    }
    let genres: Vec<&str> = entry.genres().collect();
    if !genres.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Genre: ", Style::default().fg(Color::DarkGray)),
            Span::raw(genres.join(", ")),
        ]));
    }
    let subjects: Vec<&str> = entry.subjects().collect();
    if !subjects.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Subjects: ", Style::default().fg(Color::DarkGray)),
            Span::raw(subjects.join(", ")),
        ]));
    }
}

/// Append the scraped reading-length and difficulty lines, if available.
///
/// Shown only on the full detail page, where the metrics have been fetched.
/// While loading, two placeholder lines reserve the same space the values
/// will occupy, so the layout doesn't shift when they arrive.
fn push_reading_lines(lines: &mut Vec<Line>, reading: Option<&ReadingSlot>) {
    let (length, ease) = match reading {
        Some(ReadingSlot::Ready(stats)) => (reading_length(stats), reading_ease(stats)),
        Some(ReadingSlot::Loading) => (None, None),
        // No web page to fetch from, or the page carried no metrics: show nothing.
        Some(ReadingSlot::Unavailable) | None => return,
    };
    lines.push(reading_field_line("Length", length));
    lines.push(reading_field_line("Reading ease", ease));
}

/// A reading-metric line: the value if known, else a dimmed "…" placeholder.
fn reading_field_line(label: &str, value: Option<String>) -> Line<'static> {
    match value {
        Some(v) => meta_line(label, v),
        None => Line::from(Span::styled(
            format!("{label}: …"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
    }
}

/// "60,463 words (3 hours 40 minutes)", as far as the available fields allow.
fn reading_length(stats: &ReadingStats) -> Option<String> {
    let words = stats
        .word_count
        .map(|n| format!("{} words", group_thousands(n)));
    match (words, &stats.reading_time) {
        (Some(w), Some(t)) => Some(format!("{w} ({t})")),
        (Some(w), None) => Some(w),
        (None, Some(t)) => Some(t.clone()),
        (None, None) => None,
    }
}

/// "72.56 (fairly easy)", as far as the available fields allow.
fn reading_ease(stats: &ReadingStats) -> Option<String> {
    let ease = stats.reading_ease.as_deref()?;
    Some(match &stats.difficulty {
        Some(d) => format!("{ease} ({d})"),
        None => ease.to_string(),
    })
}

/// A "Label: value" metadata line with a dimmed label.
fn meta_line(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray)),
        Span::raw(value),
    ])
}

// --- Detail "show page" --------------------------------------------------

fn render_detail_page(
    frame: &mut Frame,
    area: Rect,
    b: &BrowserState,
    images: &mut HashMap<String, ImageSlot>,
    downloads: &HashMap<String, DownloadSlot>,
    reading: &HashMap<String, ReadingSlot>,
    calibre: &HashSet<String>,
) {
    let Some(entry) = b.detail_entry() else {
        return;
    };
    let Some(detail) = b.detail.as_ref() else {
        return;
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {} ", entry.title));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .margin(1)
        .split(inner);

    // Inset the cover a little so it doesn't crowd the panel border or the
    // metadata column.
    let cover_area = panes[0].inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    render_cover(frame, cover_area, entry, images);

    // Right column: metadata/description on top, download formats below.
    let acquisitions: Vec<&crate::opds::Link> = entry.acquisition_links().collect();
    let formats_height = (acquisitions.len() as u16).saturating_add(2);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(formats_height)])
        .split(panes[1]);

    let reading_slot = entry.web_link().and_then(|l| reading.get(&l.href));
    render_detail_info(
        frame,
        right[0],
        entry,
        reading_slot,
        detail.library_id.is_some(),
        is_in_calibre(entry, calibre),
    );
    render_detail_formats(frame, right[1], &acquisitions, detail.format, downloads);
}

fn render_detail_info(
    frame: &mut Frame,
    area: Rect,
    entry: &Entry,
    reading: Option<&ReadingSlot>,
    in_library: bool,
    in_calibre: bool,
) {
    let mut lines = vec![Line::from(Span::styled(
        entry.title.clone(),
        Style::default().add_modifier(Modifier::BOLD).fg(ACCENT),
    ))];
    if !entry.authors.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("by {}", entry.author_names().collect::<Vec<_>>().join(", ")),
            Style::default().add_modifier(Modifier::ITALIC),
        )));
    }
    push_meta_lines(&mut lines, entry);
    push_reading_lines(&mut lines, reading);
    // The author's catalog page, when the feed supplies it. Labelled with the
    // author's name (distinct from the italic "by …" line) and shown as a raw
    // URL so terminals that auto-link will make it clickable, like "Web:" below.
    for author in &entry.authors {
        if let Some(uri) = &author.uri {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{}: ", author.name),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(uri.clone(), Style::default().fg(Color::Blue)),
            ]));
        }
    }
    if let Some(web) = entry.web_link() {
        lines.push(Line::from(vec![
            Span::styled("Web: ", Style::default().fg(Color::DarkGray)),
            Span::styled(web.href.clone(), Style::default().fg(Color::Blue)),
        ]));
    }
    if in_library {
        lines.push(Line::from(Span::styled(
            "✓ In your library — press g to open your downloaded copy",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if in_calibre {
        lines.push(Line::from(Span::styled(
            "◆ In your Calibre library",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(""));

    // Prefer the long description; fall back to the short summary.
    let body = entry.content.as_deref().or(entry.summary.as_deref());
    if let Some(text) = body {
        for para in text.split('\n') {
            lines.push(Line::from(para.to_string()));
        }
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_detail_formats(
    frame: &mut Frame,
    area: Rect,
    links: &[&crate::opds::Link],
    selected: usize,
    downloads: &HashMap<String, DownloadSlot>,
) {
    let block = Block::default().borders(Borders::TOP).title(Span::styled(
        " Download ",
        Style::default().add_modifier(Modifier::BOLD),
    ));

    if links.is_empty() {
        let p = Paragraph::new("  No downloadable formats").block(block);
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = links
        .iter()
        .enumerate()
        .map(|(i, link)| {
            let marker = if i == selected { "▸ " } else { "  " };
            let status = match downloads.get(&link.href) {
                Some(DownloadSlot::Pending) => {
                    Span::styled("  ↓ downloading…", Style::default().fg(Color::Yellow))
                }
                Some(DownloadSlot::Done(_)) => {
                    Span::styled("  ✓ saved", Style::default().fg(Color::Green))
                }
                Some(DownloadSlot::Failed(_)) => {
                    Span::styled("  ✗ failed", Style::default().fg(Color::Red))
                }
                None => Span::raw(""),
            };
            let style = if i == selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}{}", format_label(link)), style),
                status,
            ]))
        })
        .collect();

    frame.render_widget(List::new(items).block(block), area);
}

// --- Reader --------------------------------------------------------------

/// Render the reader's content and help line, with the disjoint `&mut` access
/// the reader needs (block cache + image queue).
fn render_reader_screen(frame: &mut Frame, content: Rect, help_area: Rect, app: &mut App) {
    let App {
        screen,
        images,
        outbox,
        ..
    } = app;
    let Screen::Reader(reader) = screen else {
        return;
    };
    let help = if reader.search.input.is_some() {
        "type to search   Enter run   Esc cancel".to_string()
    } else if reader.toc_open {
        "↑↓ move   Enter open   t/Esc close".to_string()
    } else if !reader.search.query.is_empty() {
        if reader.search.matches.is_empty() {
            format!(
                "no match for \"{}\"   / find   q close",
                reader.search.query
            )
        } else {
            format!(
                "match {}/{}   n/N next/prev   / find   q close",
                reader.search.current + 1,
                reader.search.matches.len()
            )
        }
    } else {
        "↑↓ scroll   n/p chapter   t contents   / find   x external   q close".to_string()
    };
    render_reader(frame, content, reader, images, outbox);
    render_help(frame, help_area, &help);
}

fn render_reader(
    frame: &mut Frame,
    area: Rect,
    reader: &mut ReaderState,
    images: &mut HashMap<String, ImageSlot>,
    outbox: &mut Vec<Request>,
) {
    let total = reader.chapters.len().max(1);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(
            " {} — {}/{} ",
            reader.title,
            reader.chapter + 1,
            total
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    reader.viewport_height = inner.height;
    reader.viewport_width = inner.width;

    if reader.loading {
        let p = Paragraph::new("\n  Opening book…").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(p, inner);
        return;
    }
    if let Some(err) = &reader.error {
        let p = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Failed to open book",
                Style::default().fg(Color::Red),
            )),
            Line::from(""),
            Line::from(format!("  {err}")),
        ])
        .wrap(Wrap { trim: true });
        frame.render_widget(p, inner);
        return;
    }

    // Re-wrap the current chapter when the chapter or width changes.
    if reader.rendered_for != Some((reader.chapter, inner.width)) {
        let html = reader
            .chapters
            .get(reader.chapter)
            .map(String::as_str)
            .unwrap_or("");
        reader.blocks = render_chapter(html, inner.width, reader.chapter, &reader.book_path);
        let h: u32 = reader.blocks.iter().map(block_height).sum();
        reader.content_height = h.min(u16::MAX as u32) as u16;
        reader.rendered_for = Some((reader.chapter, inner.width));
    }
    // Clamp the scroll to the real content height.
    let max_scroll = reader.content_height.saturating_sub(inner.height);
    reader.scroll = reader.scroll.min(max_scroll);

    // Match rows depend on width; recompute them if the terminal was resized
    // since the search was run, so highlights stay aligned.
    if !reader.search.query.is_empty() && reader.search.width != inner.width {
        let query = reader.search.query.clone();
        reader.search.matches = search_book(
            &reader.chapters,
            inner.width.max(1),
            &reader.book_path,
            &query,
        );
        reader.search.width = inner.width;
        if reader.search.current >= reader.search.matches.len() {
            reader.search.current = 0;
        }
    }

    // Highlight ranges for matches in the current chapter, grouped by row.
    let mut highlights: HashMap<u16, Vec<(usize, usize, bool)>> = HashMap::new();
    for (i, m) in reader.search.matches.iter().enumerate() {
        if m.chapter == reader.chapter {
            highlights
                .entry(m.row)
                .or_default()
                .push((m.col, m.len, i == reader.search.current));
        }
    }

    let scroll = reader.scroll;
    let view_h = inner.height;
    let view_bottom = scroll.saturating_add(view_h);
    let book_path = reader.book_path.clone();
    let chapter = reader.chapter;

    // Walk blocks in content space, drawing each block's visible slice.
    let mut y: u16 = 0;
    for blk in &reader.blocks {
        let top = y;
        let bottom = y.saturating_add(block_height(blk) as u16);
        y = bottom;
        if top >= view_bottom {
            break;
        }
        let vstart = top.max(scroll);
        let vend = bottom.min(view_bottom);
        if vstart >= vend {
            continue;
        }
        let rect = Rect::new(
            inner.x,
            inner.y + (vstart - scroll),
            inner.width,
            vend - vstart,
        );
        match blk {
            ContentBlock::Text(lines) => {
                let lines: Vec<Line> = if highlights.is_empty() {
                    lines.clone()
                } else {
                    lines
                        .iter()
                        .enumerate()
                        .map(
                            |(j, line)| match highlights.get(&top.saturating_add(j as u16)) {
                                Some(ranges) => highlight_line(line, ranges),
                                None => line.clone(),
                            },
                        )
                        .collect()
                };
                let p = Paragraph::new(lines).scroll((vstart - top, 0));
                frame.render_widget(p, rect);
            }
            ContentBlock::Image { key, src } => {
                // Queue a decode the first time we see this image.
                if !images.contains_key(key) {
                    images.insert(key.clone(), ImageSlot::Loading);
                    outbox.push(Request::BookImage {
                        path: PathBuf::from(&book_path),
                        chapter,
                        src: src.clone(),
                        key: key.clone(),
                    });
                }
                match images.get_mut(key) {
                    Some(ImageSlot::Ready(proto)) => {
                        frame.render_stateful_widget(StatefulImage::new(), rect, proto.as_mut());
                    }
                    Some(ImageSlot::Failed) => {
                        let p = Paragraph::new(Span::styled(
                            "[image unavailable]",
                            Style::default().fg(Color::DarkGray),
                        ))
                        .alignment(Alignment::Center);
                        frame.render_widget(p, center_v(rect));
                    }
                    _ => {
                        let p = Paragraph::new(Span::styled(
                            "[loading image…]",
                            Style::default().fg(Color::DarkGray),
                        ))
                        .alignment(Alignment::Center);
                        frame.render_widget(p, center_v(rect));
                    }
                }
            }
        }
    }

    if reader.toc_open {
        render_toc_popup(frame, area, reader);
    }
}

/// Rebuild a line with the given character `ranges` `(start, len, is_current)`
/// restyled as search highlights, splitting spans as needed.
fn highlight_line(line: &Line<'static>, ranges: &[(usize, usize, bool)]) -> Line<'static> {
    // Expand to one styled cell per character, overlay the highlight style on
    // matched ranges, then coalesce runs of equal style back into spans.
    let mut cells: Vec<(char, Style)> = Vec::new();
    for span in &line.spans {
        for c in span.content.chars() {
            cells.push((c, span.style));
        }
    }
    for &(start, len, is_current) in ranges {
        let style = highlight_style(is_current);
        for cell in cells.iter_mut().skip(start).take(len) {
            cell.1 = style;
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style: Option<Style> = None;
    for (c, st) in cells {
        if Some(st) != style {
            if let Some(prev) = style {
                spans.push(Span::styled(std::mem::take(&mut text), prev));
            }
            style = Some(st);
        }
        text.push(c);
    }
    if let Some(prev) = style {
        spans.push(Span::styled(text, prev));
    }
    Line::from(spans)
}

/// Style for a highlighted search match: the focused match stands out from the
/// rest.
fn highlight_style(is_current: bool) -> Style {
    if is_current {
        Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    }
}

fn render_toc_popup(frame: &mut Frame, area: Rect, reader: &ReaderState) {
    let rows = (reader.toc.len() as u16 + 2).clamp(3, area.height.saturating_sub(2).max(3));
    let popup = centered_rect(60, rows, area);
    frame.render_widget(Clear, popup);

    let items: Vec<ListItem> = reader
        .toc
        .iter()
        .map(|e| {
            // Mark the entry for the chapter currently being read.
            let style = if e.chapter == reader.chapter {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(e.label.clone(), style)))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Contents ");
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(reader.toc_selected));
    frame.render_stateful_widget(list, popup, &mut state);
}

// --- Popups --------------------------------------------------------------

fn render_confirm_delete(frame: &mut Frame, area: Rect, app: &App, confirm: &Confirm) {
    let (title, name) = match confirm {
        Confirm::DeleteFeed(id) => {
            let name = app
                .config
                .feeds
                .iter()
                .find(|f| f.id == *id)
                .map(|f| f.name.clone())
                .unwrap_or_default();
            (" Delete feed ", name)
        }
        Confirm::DeleteBook(i) => {
            let name = library_book_title(app, *i).unwrap_or_default();
            (" Delete book ", name)
        }
    };

    let popup = centered_rect(50, 7, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(title);
    let text = Paragraph::new(vec![
        Line::from(""),
        Line::from(format!("  Delete \"{name}\"?")),
        Line::from(""),
        Line::from(Span::styled(
            "  y = yes      any other key = cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(block);
    frame.render_widget(text, popup);
}

/// A centered menu for choosing where the highlighted format is saved.
fn render_download_menu(frame: &mut Frame, area: Rect, menu: &DownloadMenu) {
    let rows = DOWNLOAD_DESTS.len() as u16 + 2;
    let popup = centered_rect(50, rows, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Download to ");
    let items: Vec<ListItem> = DOWNLOAD_DESTS
        .iter()
        .map(|d| ListItem::new(Line::from(d.to_string())))
        .collect();
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(ACCENT)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    state.select(Some(menu.selected));
    frame.render_stateful_widget(list, popup, &mut state);
}

/// A centered, dismissible message popup for errors too long for the status
/// line (e.g. calibredb's failure output). Sized to the message, capped to the
/// screen, with the text wrapped.
fn render_notice(frame: &mut Frame, area: Rect, msg: &str) {
    // Estimate wrapped height: the body's rows plus borders, margin, and footer.
    let inner_w = (area.width * 70 / 100).saturating_sub(4).max(1) as usize;
    let body_rows: usize = msg
        .split('\n')
        .map(|para| para.chars().count().div_ceil(inner_w).max(1))
        .sum();
    let height = (body_rows as u16 + 4).min(area.height.max(3));
    let popup = centered_rect(70, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" Error ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .margin(1)
        .split(inner);
    frame.render_widget(
        Paragraph::new(msg.to_string()).wrap(Wrap { trim: true }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "press any key to dismiss",
            Style::default().fg(Color::DarkGray),
        )),
        rows[1],
    );
}

// --- Helpers -------------------------------------------------------------

/// Title of the library book at `shown` index `i` in the open browser, if any.
fn library_book_title(app: &App, i: usize) -> Option<String> {
    let Screen::Browser(b) = &app.screen else {
        return None;
    };
    let Backend::Library(lib) = &b.backend else {
        return None;
    };
    let &book_idx = lib.shown.get(i)?;
    Some(lib.books.get(book_idx)?.meta.title.clone())
}

fn crumb_path(b: &BrowserState) -> String {
    match &b.backend {
        Backend::Opds(o) => {
            let depth = o.stack.len();
            if depth == 0 {
                b.title.clone()
            } else {
                format!("{}  (depth {depth})", b.title)
            }
        }
        Backend::Library(lib) => match &lib.query {
            Some(q) => format!("{} (filter: {q})", b.title),
            None => b.title.clone(),
        },
    }
}

/// Human label for a download link: format, optional title, and size.
fn format_label(link: &crate::opds::Link) -> String {
    let fmt = pretty_mime(&link.mime);
    let mut label = if link.title.is_empty() {
        fmt.to_string()
    } else {
        format!("{fmt} — {}", link.title)
    };
    if let Some(size) = link.length {
        label.push_str(&format!(" ({})", human_size(size)));
    }
    label
}

/// Just the `YYYY-MM-DD` portion of an ISO-8601 timestamp.
fn date_only(ts: &str) -> &str {
    match ts.find('T') {
        Some(10) => &ts[..10],
        _ => ts,
    }
}

/// Insert thousands separators into a number, e.g. `60463` → `60,463`.
fn group_thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Format a byte count as a compact human-readable size.
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn pretty_mime(mime: &str) -> &str {
    match mime {
        "application/epub+zip" => "EPUB",
        "application/kepub+zip" => "KEPUB",
        "application/x-mobipocket-ebook" => "AZW3",
        "application/pdf" => "PDF",
        "application/x-cbz" => "CBZ",
        "application/xhtml+xml" => "XHTML",
        "text/html" => "HTML",
        other => other,
    }
}

/// Vertically center a one-line region within `area`.
fn center_v(area: Rect) -> Rect {
    if area.height < 1 {
        return area;
    }
    let y = area.y + area.height / 2;
    Rect::new(area.x, y, area.width, 1)
}

/// A rectangle of fixed width-percentage and height (rows), centered in `area`.
fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let width = area.width * percent_x / 100;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height.min(area.height))
}
