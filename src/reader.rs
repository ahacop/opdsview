//! Built-in EPUB reader: book content model and XHTML→terminal rendering.
//!
//! The worker thread extracts a book into a [`BookContent`] (owned, plain data)
//! using the `epub` crate; this module converts each chapter's raw XHTML into
//! styled, width-wrapped terminal [`Block`]s via `html2text`. Images surface as
//! their own blocks so the UI can render them inline through the same
//! ratatui-image pipeline used for cover art.

use std::path::{Component, Path, PathBuf};

use html2text::render::RichAnnotation;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// One entry in a book's flattened table of contents.
#[derive(Debug, Clone)]
pub struct TocEntry {
    pub label: String,
    /// Spine index of the chapter this entry points at.
    pub chapter: usize,
}

/// A whole book as extracted by the worker: the title, each spine document's
/// raw XHTML (in reading order), and a flattened table of contents.
#[derive(Debug, Clone, Default)]
pub struct BookContent {
    pub title: String,
    pub chapters: Vec<String>,
    pub toc: Vec<TocEntry>,
}

/// A renderable run within a chapter: either a block of styled text lines or an
/// image to be drawn inline.
pub enum Block {
    Text(Vec<Line<'static>>),
    /// An inline image. `key` is the stable cache key the UI tracks the decoded
    /// image under (and requests it by); `src` is the original href from the
    /// XHTML, resolved against the chapter on the worker side.
    Image {
        key: String,
        src: String,
    },
}

/// Rows a [`Block::Image`] occupies in the reader's content space. Search and
/// rendering share this so match row offsets line up with what is drawn.
pub const IMAGE_ROWS: u16 = 16;

/// Height of a reader block in content-space rows.
pub fn block_height(b: &Block) -> u32 {
    match b {
        Block::Text(lines) => lines.len() as u32,
        Block::Image { .. } => IMAGE_ROWS as u32,
    }
}

/// One occurrence of a search query within a book, located in content space so
/// the reader can scroll it into view and highlight it.
#[derive(Debug, Clone)]
pub struct Match {
    /// Spine index of the chapter the match is in.
    pub chapter: usize,
    /// Content-space row of the matched line within that chapter.
    pub row: u16,
    /// Character offset of the match within the line's text.
    pub col: usize,
    /// Length of the match in characters.
    pub len: usize,
}

/// Find every (non-overlapping) occurrence of `query` across the whole book,
/// rendering each chapter at `width` so match rows match what the reader draws.
/// Matching is ASCII case-insensitive.
pub fn search_book(chapters: &[String], width: u16, book_path: &str, query: &str) -> Vec<Match> {
    let needle: Vec<char> = query.chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for (ci, html) in chapters.iter().enumerate() {
        let blocks = render_chapter(html, width, ci, book_path);
        let mut row: u16 = 0;
        for blk in &blocks {
            if let Block::Text(lines) = blk {
                for line in lines {
                    let plain: Vec<char> =
                        line.spans.iter().flat_map(|s| s.content.chars()).collect();
                    for (col, len) in match_ranges(&plain, &needle) {
                        matches.push(Match {
                            chapter: ci,
                            row,
                            col,
                            len,
                        });
                    }
                    row = row.saturating_add(1);
                }
            } else {
                row = row.saturating_add(block_height(blk) as u16);
            }
        }
    }
    matches
}

/// Character ranges `(start, len)` where `needle` occurs in `haystack`,
/// non-overlapping and ASCII case-insensitive.
fn match_ranges(haystack: &[char], needle: &[char]) -> Vec<(usize, usize)> {
    let n = needle.len();
    let mut out = Vec::new();
    if n == 0 || haystack.len() < n {
        return out;
    }
    let mut i = 0;
    while i + n <= haystack.len() {
        if (0..n).all(|k| haystack[i + k].eq_ignore_ascii_case(&needle[k])) {
            out.push((i, n));
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

/// Convert one chapter's XHTML into width-wrapped, styled blocks.
///
/// Text reflows to `width` columns. A line carrying an image becomes its own
/// [`Block::Image`]; the surrounding text is flushed into [`Block::Text`] runs.
pub fn render_chapter(html: &str, width: u16, chapter: usize, book_path: &str) -> Vec<Block> {
    let width = (width as usize).max(1);
    let lines = match html2text::from_read_rich(html.as_bytes(), width) {
        Ok(lines) => lines,
        Err(_) => {
            return vec![Block::Text(vec![Line::from(Span::styled(
                "(this chapter could not be rendered)",
                Style::default().fg(Color::DarkGray),
            ))])];
        }
    };

    let mut blocks: Vec<Block> = Vec::new();
    let mut text: Vec<Line<'static>> = Vec::new();
    for tagged_line in &lines {
        let mut img_src: Option<String> = None;
        let mut spans: Vec<Span<'static>> = Vec::new();
        for ts in tagged_line.tagged_strings() {
            if img_src.is_none() {
                img_src = ts.tag.iter().find_map(image_src);
            }
            spans.push(Span::styled(ts.s.clone(), style_for(&ts.tag)));
        }
        // An image stands on its own block, replacing its alt text on this line.
        if let Some(src) = img_src {
            if !text.is_empty() {
                blocks.push(Block::Text(std::mem::take(&mut text)));
            }
            let key = format!("{book_path}::{chapter}::{src}");
            blocks.push(Block::Image { key, src });
            continue;
        }
        // html2text renders headings as Markdown-style `#` prefixes with no
        // distinguishing annotation; turn those back into a styled heading.
        let plain: String = spans.iter().map(|s| s.content.as_ref()).collect();
        if let Some(level) = heading_level(&plain) {
            let label = plain.trim_start()[level as usize..]
                .trim_start()
                .to_string();
            text.push(Line::from(Span::styled(label, heading_style(level))));
            continue;
        }
        text.push(Line::from(spans));
    }
    if !text.is_empty() {
        blocks.push(Block::Text(text));
    }
    if blocks.is_empty() {
        blocks.push(Block::Text(vec![Line::from("")]));
    }
    blocks
}

/// The Markdown heading level (1–6) of a rendered line, if html2text emitted it
/// as a heading (a run of 1–6 leading `#` followed by a space).
fn heading_level(text: &str) -> Option<u8> {
    let trimmed = text.trim_start();
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        Some(hashes as u8)
    } else {
        None
    }
}

/// Style for a heading of the given level: always bold and accented, with the
/// top two levels underlined for extra weight.
fn heading_style(level: u8) -> Style {
    let mut style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    if level <= 2 {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

/// The image `src` carried by an annotation, if it is an image tag.
fn image_src(tag: &RichAnnotation) -> Option<String> {
    match tag {
        RichAnnotation::Image(src) => Some(src.clone()),
        _ => None,
    }
}

/// Fold a fragment's annotations (outermost first) into a terminal style.
fn style_for(tags: &[RichAnnotation]) -> Style {
    let mut style = Style::default();
    for tag in tags {
        style = match tag {
            RichAnnotation::Strong => style.add_modifier(Modifier::BOLD),
            RichAnnotation::Emphasis => style.add_modifier(Modifier::ITALIC),
            RichAnnotation::Strikeout => style.add_modifier(Modifier::CROSSED_OUT),
            RichAnnotation::Link(_) => style.fg(Color::Blue).add_modifier(Modifier::UNDERLINED),
            RichAnnotation::Colour(c) => style.fg(Color::Rgb(c.r, c.g, c.b)),
            // Code/Preformat/BgColour/Default and any future variants: leave as-is
            // (preformatted code stays readable in the terminal's default colour).
            _ => style,
        };
    }
    style
}

/// Resolve an image/resource `href` (as it appears in a chapter's XHTML, e.g.
/// `../Images/fig.png`) against that chapter's archive path into a full,
/// normalized archive path suitable for `EpubDoc::get_resource_by_path`.
///
/// `chapter_path` is the chapter document's archive path (from
/// `EpubDoc::get_current_path`), e.g. `OEBPS/Text/chapter1.xhtml`.
pub fn resolve_href(chapter_path: &Path, href: &str) -> PathBuf {
    // Drop any fragment or query suffix.
    let href = href.split(['#', '?']).next().unwrap_or(href);
    let mut out = chapter_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    for comp in Path::new(href).components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            Component::RootDir => out = PathBuf::new(),
            Component::Normal(c) => out.push(c),
            Component::Prefix(_) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect the plain text of all [`Block::Text`] lines, for assertions.
    fn text_of(blocks: &[Block]) -> String {
        let mut out = String::new();
        for b in blocks {
            if let Block::Text(lines) = b {
                for line in lines {
                    for span in &line.spans {
                        out.push_str(&span.content);
                    }
                    out.push('\n');
                }
            }
        }
        out
    }

    #[test]
    fn renders_emphasis_and_strong_as_styles() {
        let blocks = render_chapter(
            "<p>plain <strong>bold</strong> and <em>italic</em></p>",
            40,
            0,
            "book",
        );
        let spans: Vec<&Span> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(lines) => Some(lines),
                _ => None,
            })
            .flatten()
            .flat_map(|l| l.spans.iter())
            .collect();
        let bold = spans
            .iter()
            .find(|s| s.content.contains("bold"))
            .expect("a span containing 'bold'");
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        let italic = spans
            .iter()
            .find(|s| s.content.contains("italic"))
            .expect("a span containing 'italic'");
        assert!(italic.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn headings_are_styled_without_hash_markers() {
        let blocks = render_chapter("<h2>The Sunningdale Mystery</h2>", 60, 0, "book");
        let span = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text(lines) => Some(lines),
                _ => None,
            })
            .flatten()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("Sunningdale"))
            .expect("a heading span");
        // The Markdown `#` markers are stripped and the line is styled.
        assert!(!span.content.contains('#'));
        assert_eq!(span.content.trim(), "The Sunningdale Mystery");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn image_becomes_its_own_block_between_text() {
        let html = r#"<p>before</p><img src="../Images/fig.png" alt="A figure"/><p>after</p>"#;
        let blocks = render_chapter(html, 40, 3, "/lib/book.epub");
        let img = blocks
            .iter()
            .find_map(|b| match b {
                Block::Image { key, src } => Some((key, src)),
                _ => None,
            })
            .expect("an image block");
        assert_eq!(img.1, "../Images/fig.png");
        assert_eq!(img.0, "/lib/book.epub::3::../Images/fig.png");
        // Surrounding text survives.
        let text = text_of(&blocks);
        assert!(text.contains("before"));
        assert!(text.contains("after"));
    }

    #[test]
    fn search_book_finds_matches_case_insensitively_across_chapters() {
        let chapters = vec![
            "<p>The quick brown fox</p>".to_string(),
            "<p>Foxes and a FOX</p>".to_string(),
        ];
        let matches = search_book(&chapters, 80, "book", "fox");
        // "fox" once in chapter 0, then "Fox" (in "Foxes") and "FOX" in chapter 1.
        assert_eq!(matches.len(), 3);
        assert_eq!(matches[0].chapter, 0);
        assert_eq!(matches[1].chapter, 1);
        assert_eq!(matches[2].chapter, 1);
        // Each match spans the length of the query.
        assert!(matches.iter().all(|m| m.len == 3));
    }

    #[test]
    fn search_book_empty_query_finds_nothing() {
        let chapters = vec!["<p>anything</p>".to_string()];
        assert!(search_book(&chapters, 80, "book", "").is_empty());
    }

    #[test]
    fn resolve_href_normalizes_relative_paths() {
        let chapter = Path::new("OEBPS/Text/chapter1.xhtml");
        assert_eq!(
            resolve_href(chapter, "../Images/fig.png"),
            PathBuf::from("OEBPS/Images/fig.png")
        );
        // Same directory, and a fragment is dropped.
        assert_eq!(
            resolve_href(chapter, "notes.xhtml#n2"),
            PathBuf::from("OEBPS/Text/notes.xhtml")
        );
    }
}
