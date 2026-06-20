//! Terminal rendering for all screens.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap,
};
use ratatui_image::StatefulImage;

use crate::app::{
    App, BrowserState, DownloadSlot, FormState, ImageSlot, ReadingSlot, Screen, FORM_LABELS,
};
use crate::opds::Entry;
use crate::reading::ReadingStats;

const ACCENT: Color = Color::Cyan;

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
            render_help(frame, chunks[2], "↑↓ move   n new   e edit   d delete   Enter open   q quit");
        }
        Screen::Form(form) => {
            render_form(frame, chunks[1], form);
            render_help(frame, chunks[2], "Tab/↑↓ field   type to edit   Enter save   Esc cancel");
        }
        Screen::Browser(b) => {
            let help = if b.search_query.is_some() {
                "type to search   Enter run   Esc cancel"
            } else if b.detail.is_some() {
                "↑↓ format   Enter/d download   ⌫/h/Esc back"
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
            );
            render_help(frame, chunks[2], help);
        }
    }

    if !app.status.is_empty() {
        // Status text overrides the help line briefly.
        render_help(frame, chunks[2], &app.status);
    }

    if let Screen::Browser(b) = &app.screen
        && let Some(query) = &b.search_query
    {
        render_search_input(frame, area, query);
    }

    if let Some(id) = app.confirm_delete {
        render_confirm_delete(frame, area, app, id);
    }
}

/// A centered one-line text input box for entering a search query.
fn render_search_input(frame: &mut Frame, area: Rect, query: &str) {
    let popup = centered_rect(60, 3, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" Search catalog ");
    let p = Paragraph::new(Line::from(format!(" {query}█"))).block(block);
    frame.render_widget(p, popup);
}

fn render_title_bar(frame: &mut Frame, area: Rect, screen: &Screen) {
    let title = match screen {
        Screen::FeedList => "  opdsview — Catalogs".to_string(),
        Screen::Form(f) if f.editing_id.is_some() => "  opdsview — Edit feed".to_string(),
        Screen::Form(_) => "  opdsview — New feed".to_string(),
        Screen::Browser(b) => format!("  opdsview — {}", b.current_title),
    };
    let bar = Paragraph::new(Line::from(Span::styled(
        title,
        Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
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

    if app.config.feeds.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from("  No feeds yet.").style(Style::default().add_modifier(Modifier::BOLD)),
            Line::from(""),
            Line::from("  Press 'n' to add an OPDS catalog URL."),
            Line::from("  Example: https://standardebooks.org/opds"),
        ])
        .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .config
        .feeds
        .iter()
        .map(|f| {
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
        })
        .collect();

    let list = List::new(items)
        .block(block)
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

fn render_browser(
    frame: &mut Frame,
    area: Rect,
    screen: &Screen,
    images: &mut HashMap<String, ImageSlot>,
    downloads: &HashMap<String, DownloadSlot>,
    reading: &HashMap<String, ReadingSlot>,
) {
    let Screen::Browser(b) = screen else { return };

    // The detail "show page" takes over the whole browser area when open.
    if b.detail.is_some() {
        render_detail_page(frame, area, b, images, downloads, reading);
        return;
    }

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    render_entry_list(frame, panes[0], b);
    render_detail(frame, panes[1], b, images);
}

fn render_entry_list(frame: &mut Frame, area: Rect, b: &BrowserState) {
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
            Line::from(Span::styled("  Failed to load feed", Style::default().fg(Color::Red))),
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
        let p = Paragraph::new("\n  (empty feed)").block(block);
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<ListItem> = entries
        .iter()
        .map(|e| {
            let (marker, color) = if e.is_navigation() {
                ("▸ ", ACCENT)
            } else {
                ("• ", Color::Reset)
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, Style::default().fg(color)),
                Span::raw(e.title.clone()),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
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

fn render_detail(
    frame: &mut Frame,
    area: Rect,
    b: &BrowserState,
    images: &mut HashMap<String, ImageSlot>,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Details ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(entry) = b.selected_entry() else { return };

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Min(3)])
        .split(inner);

    render_cover(frame, split[0], entry, images);
    render_entry_text(frame, split[1], entry);
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

fn render_entry_text(frame: &mut Frame, area: Rect, entry: &Entry) {
    let mut lines = vec![Line::from(Span::styled(
        entry.title.clone(),
        Style::default().add_modifier(Modifier::BOLD).fg(ACCENT),
    ))];

    if !entry.authors.is_empty() {
        lines.push(Line::from(Span::styled(
            entry.authors.join(", "),
            Style::default().add_modifier(Modifier::ITALIC),
        )));
    }

    push_meta_lines(&mut lines, entry);
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
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
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
            Span::styled(
                format!("{label}: "),
                Style::default().fg(Color::DarkGray),
            ),
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
fn push_reading_lines(lines: &mut Vec<Line>, reading: Option<&ReadingSlot>) {
    match reading {
        Some(ReadingSlot::Ready(stats)) => {
            if let Some(length) = reading_length(stats) {
                lines.push(meta_line("Length", length));
            }
            if let Some(ease) = reading_ease(stats) {
                lines.push(meta_line("Reading ease", ease));
            }
        }
        Some(ReadingSlot::Loading) => lines.push(Line::from(Span::styled(
            "Reading info…",
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        ))),
        Some(ReadingSlot::Unavailable) | None => {}
    }
}

/// "60,463 words (3 hours 40 minutes)", as far as the available fields allow.
fn reading_length(stats: &ReadingStats) -> Option<String> {
    let words = stats.word_count.map(|n| format!("{} words", group_thousands(n)));
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
) {
    let Some(entry) = b.detail_entry() else { return };
    let Some(detail) = b.detail.as_ref() else { return };

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
    render_detail_info(frame, right[0], entry, reading_slot);
    render_detail_formats(frame, right[1], &acquisitions, detail.format, downloads);
}

fn render_detail_info(
    frame: &mut Frame,
    area: Rect,
    entry: &Entry,
    reading: Option<&ReadingSlot>,
) {
    let mut lines = vec![Line::from(Span::styled(
        entry.title.clone(),
        Style::default().add_modifier(Modifier::BOLD).fg(ACCENT),
    ))];
    if !entry.authors.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("by {}", entry.authors.join(", ")),
            Style::default().add_modifier(Modifier::ITALIC),
        )));
    }
    push_meta_lines(&mut lines, entry);
    push_reading_lines(&mut lines, reading);
    if let Some(rights) = &entry.rights {
        lines.push(Line::from(vec![
            Span::styled("Rights: ", Style::default().fg(Color::DarkGray)),
            Span::raw(rights.clone()),
        ]));
    }
    if let Some(web) = entry.web_link() {
        lines.push(Line::from(vec![
            Span::styled("Web: ", Style::default().fg(Color::DarkGray)),
            Span::styled(web.href.clone(), Style::default().fg(Color::Blue)),
        ]));
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
    let block = Block::default()
        .borders(Borders::TOP)
        .title(Span::styled(
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

// --- Popups --------------------------------------------------------------

fn render_confirm_delete(frame: &mut Frame, area: Rect, app: &App, id: u64) {
    let name = app
        .config
        .feeds
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.name.clone())
        .unwrap_or_default();

    let popup = centered_rect(50, 7, area);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Red))
        .title(" Delete feed ");
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

// --- Helpers -------------------------------------------------------------

fn crumb_path(b: &BrowserState) -> String {
    let depth = b.stack.len();
    if depth == 0 {
        b.current_title.clone()
    } else {
        format!("{}  (depth {depth})", b.current_title)
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
