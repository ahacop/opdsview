//! Manual end-to-end check of the fetch + parse path against a live OPDS feed.
//!
//! Usage: cargo run --example parse_feed -- <opds-url>
//!
//! Set OPDS_USER (and optionally OPDS_PASS) to send HTTP Basic Auth, e.g. for
//! the gated Standard Ebooks Patrons Circle feed.

use opdsview::opds::Feed;

fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://standardebooks.org/feeds/opds".to_string());

    let client = reqwest::blocking::Client::builder()
        .user_agent("opdsview-example")
        .build()?;
    let mut req = client.get(&url);
    if let Ok(user) = std::env::var("OPDS_USER") {
        let pass = std::env::var("OPDS_PASS").ok();
        req = req.basic_auth(user, pass);
    }
    let xml = req.send()?.error_for_status()?.text()?;

    let feed = Feed::parse(&xml, &url)?;
    println!("Feed title: {}", feed.title);
    println!("Entries: {}", feed.entries.len());
    if let Some(next) = feed.next_link() {
        println!("Next page: {}", next.href);
    }
    for entry in feed.entries.iter().take(4) {
        let kind = if entry.is_navigation() { "NAV" } else { "PUB" };
        println!("\n[{kind}] {}", entry.title);
        if kind == "NAV" {
            println!("      -> {}", entry.nav_link().unwrap().href);
            continue;
        }
        println!("      authors:   {}", entry.authors.join(", "));
        println!("      published: {:?}", entry.published);
        println!("      language:  {:?}", entry.language);
        println!("      publisher: {:?}", entry.publisher);
        println!(
            "      genres:    {}",
            entry.genres().collect::<Vec<_>>().join(", ")
        );
        println!(
            "      subjects:  {}",
            entry.subjects().collect::<Vec<_>>().join(", ")
        );
        println!("      cover:     {:?}", entry.image_link().map(|l| &l.href));
        println!("      web page:  {:?}", entry.web_link().map(|l| &l.href));
        if let Some(content) = &entry.content {
            let preview: String = content.chars().take(80).collect();
            println!("      content:   {preview}…");
        }
        println!("      downloads:");
        for link in entry.acquisition_links() {
            println!(
                "        - {:>28} {:>10?} bytes  title={:?}",
                link.mime, link.length, link.title
            );
        }
    }
    Ok(())
}
