//! Persistence of the saved-feed list and on-disk locations.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};

/// A saved OPDS catalog, including optional HTTP Basic Auth credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feed {
    pub id: u64,
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

impl Feed {
    /// Basic-auth credentials as a `(user, pass)` pair, if a username is set.
    pub fn auth(&self) -> Option<(String, String)> {
        let user = self.username.clone().filter(|u| !u.is_empty())?;
        Some((user, self.password.clone().unwrap_or_default()))
    }
}

/// The persisted application configuration: the list of saved feeds.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub feeds: Vec<Feed>,
    #[serde(default)]
    next_id: u64,
}

impl Config {
    /// Load the config from disk, returning a default (empty) config if none exists.
    pub fn load() -> Result<Self> {
        let path = config_file()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg = serde_json::from_str(&data)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    /// Atomically write the config to disk.
    pub fn save(&self) -> Result<()> {
        let path = config_file()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Add a new feed, assigning it a fresh id.
    pub fn add(&mut self, mut feed: Feed) {
        self.next_id += 1;
        feed.id = self.next_id;
        self.feeds.push(feed);
    }

    /// Replace an existing feed (matched by id) in place.
    pub fn update(&mut self, feed: Feed) {
        if let Some(slot) = self.feeds.iter_mut().find(|f| f.id == feed.id) {
            *slot = feed;
        }
    }

    /// Remove the feed with the given id.
    pub fn remove(&mut self, id: u64) {
        self.feeds.retain(|f| f.id != id);
    }
}

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("dev", "opdsview", "opdsview")
        .context("could not determine application directories")
}

fn config_file() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("feeds.json"))
}

/// Directory used for cached feed and image data.
pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

/// Directory where downloaded books are saved.
///
/// Prefers the user's `Downloads` folder (under an `opdsview` subdirectory),
/// falling back to the application data directory when no such folder exists.
pub fn download_dir() -> Result<PathBuf> {
    if let Some(dirs) = UserDirs::new()
        && let Some(downloads) = dirs.download_dir()
    {
        return Ok(downloads.join("opdsview"));
    }
    Ok(project_dirs()?.data_dir().join("downloads"))
}
