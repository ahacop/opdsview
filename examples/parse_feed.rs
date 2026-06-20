//! Manual end-to-end check of the fetch + parse path against a live OPDS feed.
//!
//! Usage: cargo run --example parse_feed -- <opds-url>

use opdsview::opds::Feed;

fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://standardebooks.org/feeds/opds".to_string());

    let client = reqwest::blocking::Client::builder()
        .user_agent("opdsview-example")
        .build()?;
    let xml = client.get(&url).send()?.error_for_status()?.text()?;

    let feed = Feed::parse(&xml, &url)?;
    println!("Feed title: {}", feed.title);
    println!("Entries: {}", feed.entries.len());
    if let Some(next) = feed.next_link() {
        println!("Next page: {}", next.href);
    }
    for entry in feed.entries.iter().take(8) {
        let kind = if entry.is_navigation() { "NAV" } else { "PUB" };
        let cover = entry.image_link().map(|l| l.href.as_str()).unwrap_or("-");
        println!("  [{kind}] {}", entry.title);
        if kind == "NAV" {
            println!("        -> {}", entry.nav_link().unwrap().href);
        } else {
            println!(
                "        authors: {} | cover: {}",
                entry.authors.join(", "),
                cover
            );
        }
    }
    Ok(())
}
