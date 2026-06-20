//! Content-addressed on-disk cache for feed responses and images.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use sha2::{Digest, Sha256};

/// A simple filesystem cache keyed by the SHA-256 of a request URL.
#[derive(Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn new(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, url: &str, ext: &str) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        let hex = hex_encode(&hasher.finalize());
        self.dir.join(format!("{hex}.{ext}"))
    }

    /// Read a cached entry if it exists and is younger than `max_age`.
    /// Pass `None` to accept any cached entry regardless of age.
    pub fn get(&self, url: &str, ext: &str, max_age: Option<Duration>) -> Option<Vec<u8>> {
        let path = self.path(url, ext);
        let meta = fs::metadata(&path).ok()?;
        if let Some(max_age) = max_age {
            let modified = meta.modified().ok()?;
            let age = SystemTime::now().duration_since(modified).ok()?;
            if age > max_age {
                return None;
            }
        }
        fs::read(&path).ok()
    }

    /// Store bytes for the given URL.
    pub fn put(&self, url: &str, ext: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path(url, ext);
        fs::write(path, bytes)?;
        Ok(())
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
