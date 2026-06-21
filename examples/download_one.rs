//! Manual check of the download path: fetch a feed, then download the first
//! book's first acquisition link to disk via the worker.
//!
//! Usage: OPDS_USER=… cargo run --example download_one -- <opds-acquisition-url>

use std::time::Duration;

use opdsview::cache::Cache;
use opdsview::opds::Feed;
use opdsview::storage::{LibraryEntry, cache_dir};
use opdsview::worker::{DownloadDest, Request, Response, Worker};

fn main() -> anyhow::Result<()> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://standardebooks.org/feeds/opds/all".to_string());
    let auth = std::env::var("OPDS_USER")
        .ok()
        .map(|u| (u, std::env::var("OPDS_PASS").unwrap_or_default()));

    // Fetch the feed directly to find a real acquisition link.
    let client = reqwest::blocking::Client::new();
    let mut req = client.get(&url);
    if let Some((u, p)) = &auth {
        req = req.basic_auth(u, Some(p));
    }
    let xml = req.send()?.error_for_status()?.text()?;
    let feed = Feed::parse(&xml, &url)?;
    let entry = feed
        .entries
        .iter()
        .find(|e| e.acquisition_links().next().is_some())
        .expect("a publication entry");
    let link = entry.acquisition_links().next().unwrap();
    println!("Downloading {:?}: {}", entry.title, link.href);

    // Headless check: no terminal to query, so a half-blocks picker suffices
    // (this path downloads, it doesn't render covers).
    let picker = ratatui_image::picker::Picker::halfblocks();
    let (worker, responses) = Worker::spawn(Cache::new(cache_dir()?)?, picker)?;
    worker.request(Request::Download {
        meta: Box::new(LibraryEntry::from_entry(entry)),
        url: link.href.clone(),
        mime: link.mime.clone(),
        length: link.length,
        cover_url: entry.image_link().map(|l| l.href.clone()),
        auth,
        dest: DownloadDest::Library,
    });

    match responses.recv_timeout(Duration::from_secs(60))? {
        Response::Download { result, .. } => match result {
            Ok(path) => {
                let size = std::fs::metadata(&path)?.len();
                println!("Saved {} bytes to {}", size, path.display());
            }
            Err(e) => println!("Download failed: {e:#}"),
        },
        _ => println!("unexpected response"),
    }
    Ok(())
}
