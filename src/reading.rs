//! Supplementary reading metrics scraped from a publication's web page.
//!
//! Standard Ebooks publishes a word count, an estimated reading time, and a
//! Flesch reading-ease score on each book's HTML page (inside an
//! `#reading-ease` block), but none of it appears in the OPDS feed. We fetch
//! that page on demand for the detail view and parse the single human-readable
//! sentence it renders, e.g.:
//!
//! > 60,463 words (3 hours 40 minutes) with a reading ease of 72.56 (fairly easy)
//!
//! These metrics are effectively immutable, so the fetched sentence is cached
//! on disk indefinitely (see [`crate::worker`]).

/// Reading-length and difficulty metrics for a single publication.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReadingStats {
    pub word_count: Option<u32>,
    pub reading_time: Option<String>,
    pub reading_ease: Option<String>,
    pub difficulty: Option<String>,
}

impl ReadingStats {
    /// Parse the reading-ease sentence into structured fields. Returns `None`
    /// when nothing recognizable is present.
    pub fn parse(sentence: &str) -> Option<ReadingStats> {
        // "60,463 words" → 60463
        let word_count = sentence.split_once(" words").and_then(|(head, _)| {
            let digits: String = head.chars().filter(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok()
        });

        // The first parenthesized group is the estimated reading time.
        let reading_time = slice_between(sentence, "(", ")").map(|s| s.trim().to_string());

        // "reading ease of 72.56 (fairly easy)" → "72.56" and "fairly easy".
        let after_ease = sentence.split_once("reading ease of ").map(|(_, rest)| rest);
        let reading_ease = after_ease
            .map(|rest| rest.split([' ', '(']).next().unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty());
        let difficulty = after_ease
            .and_then(|rest| slice_between(rest, "(", ")"))
            .map(|s| s.trim().to_string());

        let stats = ReadingStats { word_count, reading_time, reading_ease, difficulty };
        if stats.is_empty() {
            None
        } else {
            Some(stats)
        }
    }

    /// Extract and parse reading stats directly from a book's HTML page.
    pub fn from_html(html: &str) -> Option<ReadingStats> {
        Self::parse(&extract_reading_text(html)?)
    }

    fn is_empty(&self) -> bool {
        self.word_count.is_none()
            && self.reading_time.is_none()
            && self.reading_ease.is_none()
            && self.difficulty.is_none()
    }
}

/// Pull the reading-ease sentence out of a Standard Ebooks book page.
///
/// Returns the plain text of the `<p>` inside the `#reading-ease` block, or
/// `None` for pages that don't carry one (i.e. non-SE catalogs).
pub fn extract_reading_text(html: &str) -> Option<String> {
    let aside = slice_between(html, "id=\"reading-ease\"", "</aside>")?;
    let paragraph = slice_between(aside, "<p>", "</p>")?;
    let text = collapse_ws(&strip_tags(paragraph));
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The substring of `s` between the first `start` marker and the next `end`
/// after it, exclusive of both markers.
fn slice_between<'a>(s: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let from = s.find(start)? + start.len();
    let rest = &s[from..];
    let to = rest.find(end)?;
    Some(&rest[..to])
}

/// Remove any `<...>` tags from a fragment of HTML, keeping the text between.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASIDE: &str = r#"<aside id="reading-ease">
        <meta property="schema:wordCount" content="60463"/>
        <p>60,463 words (3 hours 40 minutes) with a reading ease of 72.56 (fairly easy)</p>
        <ul class="tags"><li><a href="/subjects/fiction">Fiction</a></li></ul>
    </aside>"#;

    #[test]
    fn parses_the_full_sentence() {
        let stats = ReadingStats::parse(
            "60,463 words (3 hours 40 minutes) with a reading ease of 72.56 (fairly easy)",
        )
        .unwrap();
        assert_eq!(stats.word_count, Some(60_463));
        assert_eq!(stats.reading_time.as_deref(), Some("3 hours 40 minutes"));
        assert_eq!(stats.reading_ease.as_deref(), Some("72.56"));
        assert_eq!(stats.difficulty.as_deref(), Some("fairly easy"));
    }

    #[test]
    fn extracts_sentence_from_page_markup() {
        let text = extract_reading_text(ASIDE).unwrap();
        assert_eq!(
            text,
            "60,463 words (3 hours 40 minutes) with a reading ease of 72.56 (fairly easy)"
        );
        // And the convenience path parses it end to end.
        assert_eq!(ReadingStats::from_html(ASIDE).unwrap().word_count, Some(60_463));
    }

    #[test]
    fn ignores_pages_without_reading_ease() {
        assert!(extract_reading_text("<html><body>no aside here</body></html>").is_none());
        assert!(ReadingStats::parse("just some unrelated prose").is_none());
    }
}
