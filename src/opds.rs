//! OPDS 1.2 (Atom-based) catalog model and parser.
//!
//! OPDS catalogs are Atom feeds. Navigation feeds link to other catalogs;
//! acquisition feeds describe publications with download and cover links.

use anyhow::{Context, Result};
use url::{Url, form_urlencoded};

/// OPDS link relation prefix marking an acquisition (downloadable) link.
pub(crate) const REL_ACQUISITION: &str = "http://opds-spec.org/acquisition";
pub(crate) const REL_IMAGE: &str = "http://opds-spec.org/image";
const REL_THUMBNAIL: &str = "http://opds-spec.org/image/thumbnail";

/// Scheme URI marking a broad genre vocabulary (Standard Ebooks' own subject
/// list: "Fiction", "Nonfiction", …) as opposed to detailed subject headings
/// like LCSH. Matched as a substring so the http/https forms both hit.
const SCHEME_GENRE: &str = "standardebooks.org/vocab/subjects";

/// A single `<link>` within a feed or entry, with its href resolved to absolute.
#[derive(Debug, Clone)]
pub struct Link {
    pub rel: String,
    pub href: String,
    pub mime: String,
    pub title: String,
    /// The `length` attribute (file size in bytes), when advertised.
    pub length: Option<u64>,
}

impl Link {
    fn is_catalog(&self) -> bool {
        self.mime.contains("application/atom+xml")
    }
    fn is_acquisition(&self) -> bool {
        self.rel.starts_with(REL_ACQUISITION)
    }
    fn is_alternate(&self) -> bool {
        self.rel == "alternate"
    }
}

/// A subject or genre term from a `<category>`, tagged with the vocabulary
/// (`scheme`) it came from so broad genres can be separated from detailed
/// subject headings.
#[derive(Debug, Clone)]
pub struct Category {
    pub term: String,
    pub scheme: Option<String>,
}

impl Category {
    /// True when the term comes from a broad genre vocabulary rather than a
    /// detailed subject-heading scheme like LCSH.
    fn is_genre(&self) -> bool {
        self.scheme
            .as_deref()
            .is_some_and(|s| s.contains(SCHEME_GENRE))
    }
}

/// An entry's author: a display name plus, when the feed supplies it, the URI
/// of the author's catalog page (`<author><uri>`). Standard Ebooks points this
/// at the author's collection, e.g. `https://standardebooks.org/ebooks/willa-cather`.
#[derive(Debug, Clone, Default)]
pub struct Author {
    pub name: String,
    pub uri: Option<String>,
}

/// An OPDS entry: either a navigation item (sub-catalog) or a publication.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub title: String,
    pub authors: Vec<Author>,
    /// The Atom `<id>` of this entry — a stable identifier (often a `urn:uuid:`,
    /// `urn:isbn:`, or tag URI). Used, with `identifiers`, to match a catalog
    /// book against an external library (e.g. Calibre) by shared identifier.
    pub id: Option<String>,
    /// `<dc:identifier>` values: ISBNs, URNs, or URIs naming this publication.
    pub identifiers: Vec<String>,
    /// Short plain-text blurb (`<summary>`).
    pub summary: Option<String>,
    /// Long description (`<content>`), with HTML markup stripped.
    pub content: Option<String>,
    /// Language tag, e.g. `en-GB` (`<dc:language>`).
    pub language: Option<String>,
    /// Publisher name (`<dc:publisher>`).
    pub publisher: Option<String>,
    /// Publication date (`<published>` or `<dc:issued>`).
    pub published: Option<String>,
    /// Last-updated timestamp (`<updated>`).
    pub updated: Option<String>,
    /// Rights / licensing statement (`<rights>`).
    pub rights: Option<String>,
    /// Subject/genre terms (`<category>`), de-duplicated, each tagged with the
    /// vocabulary it came from. See [`Entry::genres`] and [`Entry::subjects`].
    pub categories: Vec<Category>,
    pub links: Vec<Link>,
}

impl Entry {
    /// Author display names, for joining or searching where the URI is unwanted.
    pub fn author_names(&self) -> impl Iterator<Item = &str> {
        self.authors.iter().map(|a| a.name.as_str())
    }

    /// The link to follow when this entry is a navigation item.
    pub fn nav_link(&self) -> Option<&Link> {
        // A navigation entry has a catalog-typed link and no acquisition links.
        if self.acquisition_links().next().is_some() {
            return None;
        }
        self.links.iter().find(|l| l.is_catalog())
    }

    /// True if following this entry leads to another catalog feed.
    pub fn is_navigation(&self) -> bool {
        self.nav_link().is_some()
    }

    /// All acquisition (download) links.
    pub fn acquisition_links(&self) -> impl Iterator<Item = &Link> {
        self.links.iter().filter(|l| l.is_acquisition())
    }

    /// Best cover-image link, preferring a full image over a thumbnail.
    pub fn image_link(&self) -> Option<&Link> {
        self.links
            .iter()
            .find(|l| l.rel == REL_IMAGE)
            .or_else(|| self.links.iter().find(|l| l.rel == REL_THUMBNAIL))
    }

    /// Human-facing web page for this publication (`rel="alternate"`), if any.
    pub fn web_link(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.is_alternate())
    }

    /// Broad genre terms (e.g. "Fiction"), drawn from a genre vocabulary.
    pub fn genres(&self) -> impl Iterator<Item = &str> {
        self.categories
            .iter()
            .filter(|c| c.is_genre())
            .map(|c| c.term.as_str())
    }

    /// Detailed subject terms (e.g. LCSH headings), excluding broad genres.
    pub fn subjects(&self) -> impl Iterator<Item = &str> {
        self.categories
            .iter()
            .filter(|c| !c.is_genre())
            .map(|c| c.term.as_str())
    }

    /// Normalized ISBNs drawn from this entry's `<id>` and `<dc:identifier>`
    /// values, de-duplicated. Lets a catalog book be matched against an external
    /// library by a stable identifier rather than fuzzy title+author.
    pub fn isbns(&self) -> Vec<String> {
        let mut out = Vec::new();
        let candidates = self
            .id
            .iter()
            .map(String::as_str)
            .chain(self.identifiers.iter().map(String::as_str));
        for s in candidates {
            if let Some(isbn) = normalize_isbn(s)
                && !out.contains(&isbn)
            {
                out.push(isbn);
            }
        }
        out
    }

    /// Identifier tokens for this entry in the canonical `scheme:value` form
    /// ([`identifier_token`]), drawn from `<id>` and `<dc:identifier>`,
    /// de-duplicated. These match a catalog book against an external library by
    /// any shared identifier — for Standard Ebooks, the stable per-book URL,
    /// which Calibre stores as a `url:` identifier and which beats fuzzy
    /// title+author matching.
    pub fn identifier_tokens(&self) -> Vec<String> {
        let mut out = Vec::new();
        for raw in self.id.iter().chain(self.identifiers.iter()) {
            if let Some(token) = raw_identifier_token(raw)
                && !out.contains(&token)
            {
                out.push(token);
            }
        }
        out
    }
}

/// Normalize an ISBN-like string to bare characters (digits plus an optional
/// trailing check `X`), or `None` if it isn't a 10- or 13-character ISBN.
///
/// Strips a leading `urn:isbn:` or `isbn:` prefix (any case), removes hyphens
/// and whitespace, and uppercases the check digit, so the many ways feeds and
/// Calibre spell the same ISBN collapse to one comparable form.
pub(crate) fn normalize_isbn(s: &str) -> Option<String> {
    let mut core = s.trim();
    for prefix in ["urn:isbn:", "isbn:"] {
        if let Some(head) = core.get(..prefix.len())
            && head.eq_ignore_ascii_case(prefix)
        {
            core = &core[prefix.len()..];
            break;
        }
    }
    let cleaned: String = core
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    let len_ok = cleaned.len() == 10 || cleaned.len() == 13;
    let last = cleaned.len().saturating_sub(1);
    let chars_ok = cleaned
        .chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_digit() || (c == 'X' && i == last));
    (len_ok && chars_ok && !cleaned.is_empty()).then_some(cleaned)
}

/// Canonicalize a `(scheme, value)` identifier pair into a token comparable
/// across an OPDS feed and a Calibre library — e.g. `isbn:9780306406157`,
/// `url:https://standardebooks.org/ebooks/willa-cather/the-professors-house`.
///
/// ISBNs are normalized via [`normalize_isbn`]; URL values lose a trailing
/// slash; the scheme is lowercased. Returns `None` for empty values.
pub(crate) fn identifier_token(scheme: &str, value: &str) -> Option<String> {
    let scheme = scheme.trim().to_ascii_lowercase();
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match scheme.as_str() {
        "isbn" => normalize_isbn(value).map(|isbn| format!("isbn:{isbn}")),
        "url" => Some(format!("url:{}", value.trim_end_matches('/'))),
        "" => None,
        _ => Some(format!("{scheme}:{value}")),
    }
}

/// Derive an identifier token from a raw OPDS `<id>`/`<dc:identifier>` string,
/// inferring its scheme.
///
/// Recognizes ISBNs (in any of the usual `urn:isbn:` / `isbn:` / bare spellings),
/// absolute URLs (Standard Ebooks' stable per-book URL — its `<id>` and
/// `<dc:identifier>` are exactly that URL, which Calibre stores as `url:`), and an
/// explicit `url:<href>` prefix. Other shapes (uuids, `tag:` URIs) are ignored,
/// since they don't reliably correspond to a Calibre identifier.
pub(crate) fn raw_identifier_token(raw: &str) -> Option<String> {
    let s = raw.trim();
    if let Some(isbn) = normalize_isbn(s) {
        return Some(format!("isbn:{isbn}"));
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return identifier_token("url", s);
    }
    if let Some(head) = s.get(..4)
        && head.eq_ignore_ascii_case("url:")
    {
        return identifier_token("url", &s[4..]);
    }
    None
}

/// A parsed OPDS feed.
#[derive(Debug, Clone)]
pub struct Feed {
    pub title: String,
    pub entries: Vec<Entry>,
    pub links: Vec<Link>,
}

impl Feed {
    /// The `rel="next"` pagination link, if present.
    pub fn next_link(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.rel == "next")
    }

    /// The `rel="search"` link advertising an OpenSearch description, if any.
    ///
    /// OPDS catalogs that support full-text search point here (OPDS 1.2 §4).
    /// The href resolves to an OpenSearch description document, not a feed.
    pub fn search_link(&self) -> Option<&Link> {
        self.links.iter().find(|l| l.rel == "search")
    }

    /// Parse a feed from XML, resolving relative links against `base_url`.
    pub fn parse(xml: &str, base_url: &str) -> Result<Feed> {
        let doc = roxmltree::Document::parse(xml).context("parsing OPDS XML")?;
        let root = doc.root_element();
        if !local_name_is(&root, "feed") {
            anyhow::bail!(
                "not an OPDS feed (root element <{}>)",
                root.tag_name().name()
            );
        }

        let base = Url::parse(base_url).ok();
        let resolve = |href: &str| -> String {
            match &base {
                Some(b) => b
                    .join(href)
                    .map(|u| u.to_string())
                    .unwrap_or_else(|_| href.to_string()),
                None => href.to_string(),
            }
        };

        let mut feed = Feed {
            title: String::new(),
            entries: Vec::new(),
            links: Vec::new(),
        };

        for child in root.children().filter(|n| n.is_element()) {
            match local_name(&child) {
                "title" if feed.title.is_empty() => feed.title = text_of(&child),
                "link" => {
                    if let Some(link) = parse_link(&child, &resolve) {
                        feed.links.push(link);
                    }
                }
                "entry" => feed.entries.push(parse_entry(&child, &resolve)),
                _ => {}
            }
        }
        Ok(feed)
    }
}

/// Pick the best OPDS search-URL template from an OpenSearch description.
///
/// An OpenSearch description (OpenSearch 1.1) lists one or more `<Url>`
/// templates, one per result format. We prefer an OPDS acquisition feed, then
/// any Atom feed; templates we can't parse as a feed (HTML, RSS) rank lowest
/// and the self-description link is skipped. Returns the chosen `template`
/// attribute verbatim, with its `{placeholders}` still intact.
pub fn opensearch_template(xml: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();
    if !local_name_is(&root, "OpenSearchDescription") {
        return None;
    }
    let mut best: Option<(u8, String)> = None;
    for url in root
        .children()
        .filter(|n| n.is_element() && local_name(n) == "Url")
    {
        let Some(template) = url.attribute("template") else {
            continue;
        };
        let mime = url.attribute("type").unwrap_or("");
        // Skip the link that points back at the description document itself.
        if mime.contains("opensearchdescription") {
            continue;
        }
        let rank = if mime.contains("opds-catalog") {
            3
        } else if mime.contains("atom+xml") {
            2
        } else {
            1
        };
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, template.to_string()));
        }
    }
    best.map(|(_, t)| t)
}

/// Fill an OpenSearch URL template with `query`, producing a fetchable URL.
///
/// `{searchTerms}` is replaced with the form-encoded query; every other
/// `{placeholder}` (e.g. `{count}`, `{startPage}`) is dropped, letting the
/// server apply its defaults.
pub fn build_search_url(template: &str, query: &str) -> String {
    let encoded: String = form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let replaced = template.replace("{searchTerms}", &encoded);
    strip_placeholders(&replaced)
}

/// Remove any remaining `{...}` template placeholders from a string.
fn strip_placeholders(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '{' {
            // Skip through to the closing brace.
            for c2 in chars.by_ref() {
                if c2 == '}' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_entry<F: Fn(&str) -> String>(node: &roxmltree::Node, resolve: &F) -> Entry {
    let mut entry = Entry::default();
    for child in node.children().filter(|n| n.is_element()) {
        // Match on the element's local name so namespaced elements such as
        // `dc:language` or `dc:issued` are picked up regardless of prefix.
        match local_name(&child) {
            "title" => entry.title = text_of(&child),
            "id" => set_if_empty(&mut entry.id, text_of(&child)),
            // `<dc:identifier>` (local name "identifier") may appear more than
            // once: ISBN, a URN, a permalink. Keep each distinct value.
            "identifier" => {
                let value = text_of(&child);
                if !value.is_empty() && !entry.identifiers.contains(&value) {
                    entry.identifiers.push(value);
                }
            }
            "summary" => set_if_empty(&mut entry.summary, text_of(&child)),
            "content" => set_if_empty(&mut entry.content, strip_html(&text_of(&child))),
            "language" => set_if_empty(&mut entry.language, text_of(&child)),
            "publisher" => set_if_empty(&mut entry.publisher, text_of(&child)),
            // `published` (Atom) and `issued` (Dublin Core) carry the same date.
            "published" | "issued" => set_if_empty(&mut entry.published, text_of(&child)),
            "updated" => set_if_empty(&mut entry.updated, text_of(&child)),
            "rights" => set_if_empty(&mut entry.rights, text_of(&child)),
            "category" => {
                if let Some(term) = child.attribute("label").or_else(|| child.attribute("term")) {
                    let term = collapse_ws(term);
                    if !term.is_empty() && !entry.categories.iter().any(|c| c.term == term) {
                        let scheme = child.attribute("scheme").map(str::to_string);
                        entry.categories.push(Category { term, scheme });
                    }
                }
            }
            "author" => {
                let mut name = String::new();
                let mut uri = None;
                for c in child.children().filter(|c| c.is_element()) {
                    match local_name(&c) {
                        "name" => name = text_of(&c),
                        // The author's catalog page; resolve in case it's relative.
                        "uri" => {
                            let u = text_of(&c);
                            if !u.is_empty() {
                                uri = Some(resolve(&u));
                            }
                        }
                        _ => {}
                    }
                }
                if !name.is_empty() {
                    entry.authors.push(Author { name, uri });
                }
            }
            "link" => {
                if let Some(link) = parse_link(&child, resolve) {
                    entry.links.push(link);
                }
            }
            _ => {}
        }
    }
    if entry.title.is_empty() {
        entry.title = "(untitled)".to_string();
    }
    entry
}

fn parse_link<F: Fn(&str) -> String>(node: &roxmltree::Node, resolve: &F) -> Option<Link> {
    let href = node.attribute("href")?;
    Some(Link {
        rel: node.attribute("rel").unwrap_or("").to_string(),
        href: resolve(href),
        mime: node.attribute("type").unwrap_or("").to_string(),
        title: node.attribute("title").unwrap_or("").to_string(),
        length: node.attribute("length").and_then(|l| l.trim().parse().ok()),
    })
}

/// Store `value` into `slot` only if it's non-empty and `slot` is still unset.
fn set_if_empty(slot: &mut Option<String>, value: String) {
    if !value.is_empty() && slot.is_none() {
        *slot = Some(value);
    }
}

fn local_name<'i>(node: &roxmltree::Node<'_, 'i>) -> &'i str {
    node.tag_name().name()
}

fn local_name_is(node: &roxmltree::Node, name: &str) -> bool {
    node.tag_name().name() == name
}

/// Concatenated text content of an element, trimmed and whitespace-collapsed.
fn text_of(node: &roxmltree::Node) -> String {
    let mut out = String::new();
    for d in node.descendants() {
        if d.is_text()
            && let Some(t) = d.text()
        {
            out.push_str(t);
        }
    }
    collapse_ws(&out)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reduce a fragment of HTML to plain text, keeping paragraph breaks.
///
/// OPDS `<content type="html">` carries escaped markup; roxmltree hands it back
/// with the tags intact (e.g. literal `<p>`). We drop the tags, turn paragraph
/// and line-break boundaries into newlines, and decode the handful of entities
/// that commonly survive a round of un-escaping.
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in s.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                let t = tag.trim().trim_start_matches('/').to_ascii_lowercase();
                if t == "p" || t.starts_with("p ") || t.starts_with("br") {
                    out.push('\n');
                }
                tag.clear();
            } else {
                tag.push(ch);
            }
        } else if ch == '<' {
            in_tag = true;
            tag.clear();
        } else {
            out.push(ch);
        }
    }
    let decoded = decode_entities(&out);
    // Collapse whitespace within each line, but preserve paragraph breaks.
    decoded
        .lines()
        .map(collapse_ws)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decode the common named/numeric entities left after one un-escaping pass.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom"
          xmlns:dc="http://purl.org/dc/terms/">
      <title>Example Catalog</title>
      <link rel="self" href="/opds" type="application/atom+xml;profile=opds-catalog"/>
      <link rel="next" href="/opds?page=2" type="application/atom+xml;profile=opds-catalog"/>
      <entry>
        <title>Fiction</title>
        <link rel="subsection" href="fiction"
              type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
      </entry>
      <entry>
        <title>A Great Book</title>
        <id>urn:uuid:3f2a9c1e-0000-4000-8000-000000000001</id>
        <dc:identifier>urn:isbn:978-0-306-40615-7</dc:identifier>
        <dc:identifier>https://example.com/books/1</dc:identifier>
        <author><name>Jane Doe</name><uri>https://example.com/authors/jane-doe</uri></author>
        <summary>A thrilling tale.</summary>
        <content type="html">&lt;p&gt;First paragraph &amp;amp; more.&lt;/p&gt; &lt;p&gt;Second paragraph.&lt;/p&gt;</content>
        <dc:language>en-GB</dc:language>
        <dc:publisher>Example Press</dc:publisher>
        <published>2024-01-02T03:04:05Z</published>
        <rights>Public domain.</rights>
        <category scheme="http://purl.org/dc/terms/LCSH" term="Fiction"/>
        <category scheme="http://purl.org/dc/terms/LCSH" term="Adventure" label="Adventure"/>
        <category scheme="http://purl.org/dc/terms/LCSH" term="Fiction"/>
        <category scheme="https://standardebooks.org/vocab/subjects" term="Nonfiction"/>
        <link rel="http://opds-spec.org/image" href="/covers/1.jpg" type="image/jpeg"/>
        <link rel="http://opds-spec.org/image/thumbnail" href="/covers/1-t.jpg" type="image/jpeg"/>
        <link rel="alternate" href="/books/1" type="application/xhtml+xml"/>
        <link rel="http://opds-spec.org/acquisition" href="/books/1.epub"
              type="application/epub+zip" length="12345"/>
      </entry>
    </feed>"#;

    #[test]
    fn parses_feed_metadata_and_pagination() {
        let feed = Feed::parse(SAMPLE, "https://example.com/opds").unwrap();
        assert_eq!(feed.title, "Example Catalog");
        assert_eq!(feed.entries.len(), 2);
        assert_eq!(
            feed.next_link().unwrap().href,
            "https://example.com/opds?page=2"
        );
    }

    #[test]
    fn classifies_navigation_entry() {
        let feed = Feed::parse(SAMPLE, "https://example.com/opds").unwrap();
        let nav = &feed.entries[0];
        assert!(nav.is_navigation());
        // Relative href is resolved against the feed URL.
        assert_eq!(nav.nav_link().unwrap().href, "https://example.com/fiction");
        assert!(nav.image_link().is_none());
    }

    #[test]
    fn classifies_publication_entry() {
        let feed = Feed::parse(SAMPLE, "https://example.com/opds").unwrap();
        let book = &feed.entries[1];
        assert!(!book.is_navigation());
        assert_eq!(book.author_names().collect::<Vec<_>>(), vec!["Jane Doe"]);
        assert_eq!(
            book.authors[0].uri.as_deref(),
            Some("https://example.com/authors/jane-doe")
        );
        assert_eq!(book.summary.as_deref(), Some("A thrilling tale."));
        // Full image is preferred over the thumbnail.
        assert_eq!(
            book.image_link().unwrap().href,
            "https://example.com/covers/1.jpg"
        );
        let downloads: Vec<_> = book.acquisition_links().collect();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].mime, "application/epub+zip");
        assert_eq!(downloads[0].length, Some(12345));
    }

    #[test]
    fn parses_extended_metadata() {
        let feed = Feed::parse(SAMPLE, "https://example.com/opds").unwrap();
        let book = &feed.entries[1];
        assert_eq!(book.language.as_deref(), Some("en-GB"));
        assert_eq!(book.publisher.as_deref(), Some("Example Press"));
        assert_eq!(book.published.as_deref(), Some("2024-01-02T03:04:05Z"));
        assert_eq!(book.rights.as_deref(), Some("Public domain."));
        // Detailed subjects prefer label over term and are de-duplicated; the
        // genre-vocabulary term is split out from the subject headings.
        assert_eq!(
            book.subjects().collect::<Vec<_>>(),
            vec!["Fiction", "Adventure"]
        );
        assert_eq!(book.genres().collect::<Vec<_>>(), vec!["Nonfiction"]);
        // Content has tags stripped, entities decoded, and paragraphs split.
        assert_eq!(
            book.content.as_deref(),
            Some("First paragraph & more.\nSecond paragraph.")
        );
        // The alternate link points at the publication's web page.
        assert_eq!(book.web_link().unwrap().href, "https://example.com/books/1");
        // The Atom id and dc:identifier are captured; the ISBN (hyphenated, urn-
        // prefixed) normalizes to bare digits, while the uuid id is not an ISBN.
        assert_eq!(
            book.id.as_deref(),
            Some("urn:uuid:3f2a9c1e-0000-4000-8000-000000000001")
        );
        assert_eq!(
            book.identifiers,
            vec!["urn:isbn:978-0-306-40615-7", "https://example.com/books/1"]
        );
        assert_eq!(book.isbns(), vec!["9780306406157".to_string()]);
        // Tokens skip the non-matching uuid id and carry both the ISBN and the
        // URL (the latter is how Standard Ebooks books match a Calibre `url:`).
        assert_eq!(
            book.identifier_tokens(),
            vec![
                "isbn:9780306406157".to_string(),
                "url:https://example.com/books/1".to_string(),
            ]
        );
    }

    #[test]
    fn identifier_tokens_canonicalize_scheme_and_value() {
        // A trailing slash on a URL is dropped so both sides compare equal.
        assert_eq!(
            identifier_token("URL", "https://standardebooks.org/ebooks/x/y/"),
            Some("url:https://standardebooks.org/ebooks/x/y".to_string())
        );
        assert_eq!(
            identifier_token("isbn", "978-0-306-40615-7"),
            Some("isbn:9780306406157".to_string())
        );
        // A bare SE URL infers the `url` scheme; an explicit `url:` prefix works too.
        assert_eq!(
            raw_identifier_token("https://standardebooks.org/ebooks/a/b"),
            Some("url:https://standardebooks.org/ebooks/a/b".to_string())
        );
        assert_eq!(
            raw_identifier_token("url:https://standardebooks.org/ebooks/a/b"),
            Some("url:https://standardebooks.org/ebooks/a/b".to_string())
        );
        // Uuids and tag URIs don't map to a Calibre identifier and are ignored.
        assert!(raw_identifier_token("urn:uuid:3f2a9c1e-0000-4000-8000-000000000001").is_none());
    }

    #[test]
    fn normalizes_isbns_and_rejects_non_isbns() {
        assert_eq!(
            normalize_isbn("urn:isbn:978-0-306-40615-7").as_deref(),
            Some("9780306406157")
        );
        assert_eq!(
            normalize_isbn("ISBN:0306406152").as_deref(),
            Some("0306406152")
        );
        assert_eq!(
            normalize_isbn("0-19-852663-6").as_deref(),
            Some("0198526636")
        );
        // A trailing check 'X' is kept and uppercased.
        assert_eq!(normalize_isbn("097522980x").as_deref(), Some("097522980X"));
        // Non-ISBN identifiers (uuid, plain words, wrong length) are rejected.
        assert!(normalize_isbn("urn:uuid:3f2a9c1e-0000-4000-8000-000000000001").is_none());
        assert!(normalize_isbn("https://example.com/books/1").is_none());
        assert!(normalize_isbn("12345").is_none());
    }

    #[test]
    fn rejects_non_feed_xml() {
        assert!(Feed::parse("<html><body>nope</body></html>", "https://x/").is_err());
    }

    const OPENSEARCH: &str = r#"<?xml version="1.0" encoding="utf-8"?>
    <OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
      <Url type="application/opensearchdescription+xml" rel="self" template="https://e.org/opensearch"/>
      <Url type="text/html" template="https://e.org/search?q={searchTerms}&amp;page={startPage}"/>
      <Url type="application/atom+xml" template="https://e.org/atom?query={searchTerms}"/>
      <Url type="application/atom+xml;profile=opds-catalog;kind=acquisition"
           template="https://e.org/opds/all?query={searchTerms}&amp;per-page={count}&amp;page={startPage}"/>
    </OpenSearchDescription>"#;

    #[test]
    fn picks_opds_template_over_html_and_plain_atom() {
        let t = opensearch_template(OPENSEARCH).unwrap();
        assert_eq!(
            t,
            "https://e.org/opds/all?query={searchTerms}&per-page={count}&page={startPage}"
        );
    }

    #[test]
    fn builds_search_url_encoding_terms_and_dropping_placeholders() {
        let t = opensearch_template(OPENSEARCH).unwrap();
        assert_eq!(
            build_search_url(&t, "war and peace"),
            "https://e.org/opds/all?query=war+and+peace&per-page=&page="
        );
    }

    #[test]
    fn opensearch_template_rejects_other_documents() {
        assert!(opensearch_template("<feed/>").is_none());
        assert!(opensearch_template("not xml").is_none());
    }

    #[test]
    fn finds_search_link_in_feed() {
        let xml = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <title>X</title>
          <link rel="search" href="/opensearch" type="application/opensearchdescription+xml"/>
        </feed>"#;
        let feed = Feed::parse(xml, "https://e.org/opds").unwrap();
        assert_eq!(feed.search_link().unwrap().href, "https://e.org/opensearch");
    }
}
