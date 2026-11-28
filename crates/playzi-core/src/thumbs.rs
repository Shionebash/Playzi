use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use crate::error::{CoreError, Result};

const MAX_THUMB_BYTES: usize = 10 * 1024 * 1024;

pub const DEFAULT_THUMB_TTL_SECS: u64 = 24 * 60 * 60;
pub const DEFAULT_THUMB_MAX_FILES: usize = 500;

const ALLOWED_THUMB_HOSTS: &[&str] = &[
    "i.ytimg.com",
    "i1.ytimg.com",
    "i2.ytimg.com",
    "i3.ytimg.com",
    "i4.ytimg.com",
    "i9.ytimg.com",
    "img.youtube.com",
    "yt3.ggpht.com",
    "yt3.googleusercontent.com",
    "lh3.googleusercontent.com",
];

const ALLOWED_THUMB_SUFFIXES: &[&str] = &[
    ".ytimg.com",
    ".googleusercontent.com",
    ".ggpht.com",
    ".gstatic.com",
];

const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThumbMeta {
    url: String,
    content_type: String,
    cached_at: u64,
}

#[derive(Debug, Clone)]
pub struct CachedThumbnail {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub path: PathBuf,
    pub from_cache: bool,
}

#[derive(Debug, Clone)]
pub struct ThumbnailCache {
    cache_dir: PathBuf,
    client: reqwest::Client,
    ttl: Duration,
    max_entries: usize,
    allow_any_host: bool,
}

impl ThumbnailCache {
    pub fn new(cache_dir: impl AsRef<Path>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
        headers.insert(REFERER, HeaderValue::from_static("https://www.youtube.com/"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(
                "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            ),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(10)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.error("too many thumbnail redirects");
                }
                let host = attempt.url().host_str().unwrap_or("");
                if attempt.url().scheme() != "https" || !is_allowed_thumbnail_host(host) {
                    return attempt.error("redirect to disallowed thumbnail host");
                }
                attempt.follow()
            }))
            .build()?;

        Ok(Self {
            cache_dir: cache_dir.as_ref().to_path_buf(),
            client,
            ttl: Duration::from_secs(DEFAULT_THUMB_TTL_SECS),
            max_entries: DEFAULT_THUMB_MAX_FILES,
            allow_any_host: false,
        })
    }

    pub fn with_client(cache_dir: impl AsRef<Path>, client: reqwest::Client) -> Self {
        Self {
            cache_dir: cache_dir.as_ref().to_path_buf(),
            client,
            ttl: Duration::from_secs(DEFAULT_THUMB_TTL_SECS),
            max_entries: DEFAULT_THUMB_MAX_FILES,
            allow_any_host: false,
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries.max(1);
        self
    }

    /// Allows fetching from arbitrary hosts (useful for local mock servers in tests).
    pub fn allow_any_host(mut self, allow: bool) -> Self {
        self.allow_any_host = allow;
        self
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn normalize_and_validate_url(&self, raw_url: &str) -> Result<String> {
        let trimmed = raw_url.trim();
        let normalized = if trimmed.starts_with("//") {
            format!("https:{trimmed}")
        } else {
            trimmed.to_string()
        };

        let mut parsed = Url::parse(&normalized)?;
        match parsed.scheme() {
            "https" => {}
            "http" if self.allow_any_host => {}
            "http" => {
                let _ = parsed.set_scheme("https");
            }
            _ => {
                return Err(CoreError::Validation(
                    "Invalid thumbnail URL scheme (expected http or https)".into(),
                ));
            }
        }

        if !self.allow_any_host {
            let host = parsed.host_str().unwrap_or("");
            if !is_allowed_thumbnail_host(host) {
                return Err(CoreError::Validation(format!(
                    "Thumbnail host not allowed: {host}"
                )));
            }
        }

        Ok(parsed.to_string())
    }

    pub fn cache_key(url: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn data_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.img"))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.cache_dir.join(format!("{key}.meta.json"))
    }

    /// Reads a cached thumbnail from disk if present and within TTL.
    pub fn read_cached(&self, raw_url: &str) -> Result<Option<CachedThumbnail>> {
        let normalized = self.normalize_and_validate_url(raw_url)?;
        let key = Self::cache_key(&normalized);
        let data_path = self.data_path(&key);
        let meta_path = self.meta_path(&key);

        if !data_path.exists() || !meta_path.exists() {
            return Ok(None);
        }

        let meta_raw = match fs::read_to_string(&meta_path) {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let meta: ThumbMeta = match serde_json::from_str(&meta_raw) {
            Ok(m) => m,
            Err(_) => return Ok(None),
        };

        let now = now_unix_secs();
        if now.saturating_sub(meta.cached_at) >= self.ttl.as_secs() {
            return Ok(None);
        }

        let bytes = fs::read(&data_path)?;
        Ok(Some(CachedThumbnail {
            bytes,
            content_type: meta.content_type,
            path: data_path,
            from_cache: true,
        }))
    }

    /// Fetches a thumbnail from disk cache or downloads and persists it atomically.
    pub async fn get_or_fetch(&self, raw_url: &str) -> Result<CachedThumbnail> {
        let normalized = self.normalize_and_validate_url(raw_url)?;
        let cache_for_read = self.clone();
        let norm_for_read = normalized.clone();
        if let Some(hit) = tokio::task::spawn_blocking(move || cache_for_read.read_cached(&norm_for_read))
            .await
            .map_err(|e| CoreError::External(format!("Thumbnail cache read task failed: {e}")))??
        {
            return Ok(hit);
        }

        let mut response = self.client.get(&normalized).send().await?.error_for_status()?;
        if !self.allow_any_host {
            let final_host = response.url().host_str().unwrap_or("");
            if response.url().scheme() != "https" || !is_allowed_thumbnail_host(final_host) {
                return Err(CoreError::Validation(format!(
                    "Thumbnail redirect host not allowed: {final_host}"
                )));
            }
        }
        if let Some(cl) = response.content_length()
            && cl as usize > MAX_THUMB_BYTES
        {
            return Err(CoreError::External(format!(
                "Thumbnail too large ({cl} bytes)"
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
            .filter(|ct| ct.starts_with("image/"))
            .unwrap_or_else(|| "image/jpeg".to_string());

        let mut bytes = Vec::with_capacity(
            response
                .content_length()
                .map(|n| (n as usize).min(MAX_THUMB_BYTES))
                .unwrap_or(0),
        );
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > MAX_THUMB_BYTES {
                return Err(CoreError::External(
                    "Thumbnail exceeded maximum allowed size".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }

        let cache_for_write = self.clone();
        let bytes_for_write = bytes.clone();
        let ct_for_write = content_type.clone();
        let data_path = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            let key = Self::cache_key(&normalized);
            let data_path = cache_for_write.data_path(&key);
            let meta_path = cache_for_write.meta_path(&key);
            fs::create_dir_all(&cache_for_write.cache_dir)?;

            let suffix = Uuid::new_v4();
            let data_tmp = cache_for_write
                .cache_dir
                .join(format!("{key}.{suffix}.img.tmp"));
            fs::write(&data_tmp, &bytes_for_write)?;
            if fs::rename(&data_tmp, &data_path).is_err() {
                let _ = fs::remove_file(&data_tmp);
            }

            let meta = ThumbMeta {
                url: normalized,
                content_type: ct_for_write,
                cached_at: now_unix_secs(),
            };
            let meta_tmp = cache_for_write
                .cache_dir
                .join(format!("{key}.{suffix}.meta.json.tmp"));
            fs::write(&meta_tmp, serde_json::to_vec(&meta)?)?;
            if fs::rename(&meta_tmp, &meta_path).is_err() {
                let _ = fs::remove_file(&meta_tmp);
            }

            let _ = cache_for_write.prune_excess();
            Ok(data_path)
        })
        .await
        .map_err(|e| CoreError::External(format!("Thumbnail cache write task failed: {e}")))??;

        Ok(CachedThumbnail {
            bytes,
            content_type,
            path: data_path,
            from_cache: false,
        })
    }

    /// Removes oldest cached entries if total `.img` files exceed `max_entries`.
    pub fn prune_excess(&self) -> Result<usize> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }
        let mut entries: Vec<(SystemTime, PathBuf)> = fs::read_dir(&self.cache_dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext == "img")
                    .unwrap_or(false)
            })
            .map(|p| {
                let mtime = fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                (mtime, p)
            })
            .collect();

        if entries.len() <= self.max_entries {
            return Ok(0);
        }

        // Sort oldest first
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let to_remove = entries.len() - self.max_entries;
        let mut removed = 0;

        for (_, img_path) in entries.into_iter().take(to_remove) {
            if fs::remove_file(&img_path).is_ok() {
                removed += 1;
            }
            if let Some(stem) = img_path.file_stem().and_then(|s| s.to_str()) {
                let meta_path = self.meta_path(stem);
                let _ = fs::remove_file(meta_path);
            }
        }
        Ok(removed)
    }

    pub fn clear(&self) -> Result<()> {
        if self.cache_dir.exists() {
            for entry in fs::read_dir(&self.cache_dir)?.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let _ = fs::remove_file(path);
                }
            }
        }
        Ok(())
    }
}

pub fn is_allowed_thumbnail_host(hostname: &str) -> bool {
    let host = hostname.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    ALLOWED_THUMB_HOSTS.iter().any(|allowed| *allowed == host)
        || ALLOWED_THUMB_SUFFIXES
            .iter()
            .any(|suffix| host.ends_with(suffix))
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
