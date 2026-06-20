//! Terminal rendering for all screens.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap,
};
use ratatui_image::StatefulImage;

use crate::app::{App, BrowserState, FormState, ImageSlot, Screen, FORM_LABELS};
use crate::opds::Entry;

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
        Screen::Browser(_) => {
            render_browser(frame, chunks[1], &app.screen, &mut app.images);
            render_help(
                frame,
                chunks[2],
                "↑↓ move   Enter open   ⌫/h back   n next page   q feeds",
            );
        }
    }

    if !app.status.is_empty() {
        // Status text overrides the help line briefly.
        render_help(frame, chunks[2], &app.status);
    }

    if let Some(id) = app.confirm_delete {
        render_confirm_delete(frame, area, app, id);
    }
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
) {
    let Screen::Browser(b) = screen else { return };

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
            let label = if link.title.is_empty() {
                pretty_mime(&link.mime).to_string()
            } else {
                format!("{} ({})", link.title, pretty_mime(&link.mime))
            };
            lines.push(Line::from(format!("  • {label}")));
        }
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: true });
    frame.render_widget(p, area);
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

fn pretty_mime(mime: &str) -> &str {
    match mime {
        "application/epub+zip" => "EPUB",
        "application/x-mobipocket-ebook" => "MOBI",
        "application/pdf" => "PDF",
        "application/x-cbz" => "CBZ",
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
