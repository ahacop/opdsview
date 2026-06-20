//! OPDS 1.2 (Atom-based) catalog model and parser.
//!
//! OPDS catalogs are Atom feeds. Navigation feeds link to other catalogs;
//! acquisition feeds describe publications with download and cover links.

use anyhow::{Context, Result};
use url::Url;

/// OPDS link relation prefix marking an acquisition (downloadable) link.
const REL_ACQUISITION: &str = "http://opds-spec.org/acquisition";
const REL_IMAGE: &str = "http://opds-spec.org/image";
const REL_THUMBNAIL: &str = "http://opds-spec.org/image/thumbnail";

/// A single `<link>` within a feed or entry, with its href resolved to absolute.
#[derive(Debug, Clone)]
pub struct Link {
    pub rel: String,
    pub href: String,
    pub mime: String,
    pub title: String,
}

impl Link {
    fn is_catalog(&self) -> bool {
        self.mime.contains("application/atom+xml")
    }
    fn is_acquisition(&self) -> bool {
        self.rel.starts_with(REL_ACQUISITION)
    }
}

/// An OPDS entry: either a navigation item (sub-catalog) or a publication.
#[derive(Debug, Clone)]
pub struct Entry {
    pub title: String,
    pub authors: Vec<String>,
    pub summary: Option<String>,
    pub links: Vec<Link>,
}

impl Entry {
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

    /// Parse a feed from XML, resolving relative links against `base_url`.
    pub fn parse(xml: &str, base_url: &str) -> Result<Feed> {
        let doc = roxmltree::Document::parse(xml).context("parsing OPDS XML")?;
        let root = doc.root_element();
        if !local_name_is(&root, "feed") {
            anyhow::bail!("not an OPDS feed (root element <{}>)", root.tag_name().name());
        }

        let base = Url::parse(base_url).ok();
        let resolve = |href: &str| -> String {
            match &base {
                Some(b) => b.join(href).map(|u| u.to_string()).unwrap_or_else(|_| href.to_string()),
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

fn parse_entry<F: Fn(&str) -> String>(node: &roxmltree::Node, resolve: &F) -> Entry {
    let mut entry = Entry {
        title: String::new(),
        authors: Vec::new(),
        summary: None,
        links: Vec::new(),
    };
    for child in node.children().filter(|n| n.is_element()) {
        match local_name(&child) {
            "title" => entry.title = text_of(&child),
            "summary" | "content" => {
                let t = text_of(&child);
                if !t.is_empty() && entry.summary.is_none() {
                    entry.summary = Some(t);
                }
            }
            "author" => {
                if let Some(name) = child
                    .children()
                    .find(|c| c.is_element() && local_name(c) == "name")
                {
                    let n = text_of(&name);
                    if !n.is_empty() {
                        entry.authors.push(n);
                    }
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
    })
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
            && let Some(t) = d.text() {
                out.push_str(t);
            }
    }
    collapse_ws(&out)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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
        <author><name>Jane Doe</name></author>
        <summary>A thrilling tale.</summary>
        <link rel="http://opds-spec.org/image" href="/covers/1.jpg" type="image/jpeg"/>
        <link rel="http://opds-spec.org/image/thumbnail" href="/covers/1-t.jpg" type="image/jpeg"/>
        <link rel="http://opds-spec.org/acquisition" href="/books/1.epub"
              type="application/epub+zip"/>
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
        assert_eq!(book.authors, vec!["Jane Doe"]);
        assert_eq!(book.summary.as_deref(), Some("A thrilling tale."));
        // Full image is preferred over the thumbnail.
        assert_eq!(
            book.image_link().unwrap().href,
            "https://example.com/covers/1.jpg"
        );
        let downloads: Vec<_> = book.acquisition_links().collect();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].mime, "application/epub+zip");
    }

    #[test]
    fn rejects_non_feed_xml() {
        assert!(Feed::parse("<html><body>nope</body></html>", "https://x/").is_err());
    }
}
