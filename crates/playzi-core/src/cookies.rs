use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};

use crate::error::{CoreError, Result};

pub const DEFAULT_CACHE_MAX_HOURS: u64 = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetscapeCookie {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: i64,
    pub name: String,
    pub value: String,
    pub http_only: bool,
}

impl NetscapeCookie {
    /// Returns true if the cookie is expired relative to `now_unix_secs`.
    /// Session cookies (`expires <= 0`) are treated as non-expired.
    pub fn is_expired(&self, now_unix_secs: i64) -> bool {
        self.expires > 0 && self.expires < now_unix_secs
    }

    /// Checks whether this cookie applies to `host` (case-insensitive).
    pub fn matches_host(&self, host: &str) -> bool {
        let target = host.trim().trim_start_matches('.').to_ascii_lowercase();
        let cookie_domain = self
            .domain
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if target.is_empty() || cookie_domain.is_empty() {
            return false;
        }
        if target == cookie_domain {
            return true;
        }
        if self.include_subdomains || self.domain.starts_with('.') {
            return target.ends_with(&format!(".{cookie_domain}"));
        }
        false
    }

    pub fn to_netscape_line(&self) -> String {
        let domain_col = if self.http_only {
            format!("#HttpOnly_{}", self.domain)
        } else {
            self.domain.clone()
        };
        let include = if self.include_subdomains {
            "TRUE"
        } else {
            "FALSE"
        };
        let secure = if self.secure { "TRUE" } else { "FALSE" };
        format!(
            "{domain_col}\t{include}\t{}\t{secure}\t{}\t{}\t{}",
            self.path, self.expires, self.name, self.value
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct CookieJar {
    cookies: Vec<NetscapeCookie>,
}

impl CookieJar {
    pub fn new(cookies: Vec<NetscapeCookie>) -> Self {
        Self { cookies }
    }

    /// Parses Netscape HTTP Cookie File content (with optional UTF-8 BOM and `#HttpOnly_` prefix).
    pub fn parse(content: &str) -> Self {
        let content = content.strip_prefix('\u{feff}').unwrap_or(content);
        let mut cookies = Vec::new();

        for raw_line in content.lines() {
            let line = raw_line.trim_end_matches('\r');
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let (http_only, effective) = if let Some(rest) = trimmed.strip_prefix("#HttpOnly_") {
                (true, rest)
            } else if trimmed.starts_with('#') {
                continue;
            } else {
                (false, trimmed)
            };

            let parts: Vec<&str> = effective.split('\t').collect();
            if parts.len() < 7 {
                continue;
            }

            let domain = parts[0].trim().to_string();
            let include_subdomains = parts[1].eq_ignore_ascii_case("TRUE");
            let path = parts[2].trim().to_string();
            let secure = parts[3].eq_ignore_ascii_case("TRUE");
            let expires = parts[4].trim().parse::<i64>().unwrap_or(0);
            let name = parts[5].trim().to_string();
            let value = parts[6..].join("\t");

            if domain.is_empty() || name.is_empty() {
                continue;
            }

            cookies.push(NetscapeCookie {
                domain,
                include_subdomains,
                path: if path.is_empty() { "/".into() } else { path },
                secure,
                expires,
                name,
                value,
                http_only,
            });
        }

        Self { cookies }
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = fs::read(path.as_ref())?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(Self::parse(&text))
    }

    pub fn cookies(&self) -> &[NetscapeCookie] {
        &self.cookies
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn to_netscape_string(&self) -> String {
        let mut out = String::from("# Netscape HTTP Cookie File\n");
        for cookie in &self.cookies {
            out.push_str(&cookie.to_netscape_line());
            out.push('\n');
        }
        out
    }

    /// Returns a map of cookie name -> value for all non-expired cookies.
    pub fn cookie_map(&self) -> BTreeMap<String, String> {
        let now = now_unix_secs();
        let mut map = BTreeMap::new();
        for c in &self.cookies {
            if !c.is_expired(now) {
                map.insert(c.name.clone(), c.value.clone());
            }
        }
        map
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let now = now_unix_secs();
        self.cookies
            .iter()
            .rev()
            .find(|c| c.name == name && !c.is_expired(now))
            .map(|c| c.value.as_str())
    }

    /// Builds a `Cookie` header filtered to `host` (excluding expired entries).
    pub fn cookie_header_for_host(&self, host: &str) -> String {
        let now = now_unix_secs();
        let mut seen = BTreeMap::new();
        for c in &self.cookies {
            if !c.is_expired(now) && c.matches_host(host) {
                seen.insert(c.name.as_str(), c.value.as_str());
            }
        }
        seen.into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Builds a `Cookie` header containing all non-expired cookies from the jar.
    pub fn cookie_header_all(&self) -> String {
        self.cookie_map()
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Returns the SAPISID token used to sign YouTube/YouTube Music InnerTube requests.
    pub fn sapisid(&self) -> Option<&str> {
        self.get("SAPISID")
            .or_else(|| self.get("__Secure-3PAPISID"))
            .or_else(|| self.get("__Secure-1PAPISID"))
    }

    /// Computes `SAPISIDHASH {timestamp}_{sha1_hex}` for the given origin and UNIX timestamp.
    pub fn sapisidhash(&self, origin: &str, timestamp_secs: i64) -> Option<String> {
        let sapisid = self.sapisid()?;
        Some(compute_sapisidhash(sapisid, origin, timestamp_secs))
    }

    /// Builds YouTube Music authenticated InnerTube headers (`Cookie`, `Authorization`, `x-origin`, `X-Goog-AuthUser`).
    pub fn ytmusic_auth_headers(&self, origin: &str) -> Option<BTreeMap<String, String>> {
        let auth = self.sapisidhash(origin, now_unix_secs())?;
        let cookie_header = self.cookie_header_all();
        if cookie_header.is_empty() {
            return None;
        }
        let mut headers = BTreeMap::new();
        headers.insert("Cookie".into(), cookie_header);
        headers.insert("Authorization".into(), auth);
        headers.insert("x-origin".into(), origin.to_string());
        headers.insert("X-Goog-AuthUser".into(), "0".into());
        Some(headers)
    }
}

pub fn compute_sapisidhash(sapisid: &str, origin: &str, timestamp_secs: i64) -> String {
    let payload = format!("{timestamp_secs} {sapisid} {origin}");
    let mut hasher = Sha1::new();
    hasher.update(payload.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("SAPISIDHASH {timestamp_secs}_{digest}")
}

/// Checks the first 20 lines of `path` to determine if it looks like a Netscape cookie file.
pub fn looks_like_netscape_cookie_file(path: impl AsRef<Path>) -> bool {
    let Ok(bytes) = fs::read(path.as_ref()) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    for line in text.lines().take(20) {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        if stripped.starts_with("# Netscape HTTP Cookie File") {
            return true;
        }
        if let Some(rest) = stripped.strip_prefix("#HttpOnly_") {
            return rest.matches('\t').count() >= 6;
        }
        if stripped.starts_with('#') {
            continue;
        }
        return stripped.matches('\t').count() >= 6;
    }
    false
}

/// Manages Playzi's private cookie cache (`data/private/cookies_cache.txt`) and exported cookie files.
#[derive(Debug, Clone)]
pub struct CookieStore {
    root_dir: PathBuf,
    data_dir: PathBuf,
}

impl CookieStore {
    pub fn new(root_dir: impl AsRef<Path>, data_dir: impl AsRef<Path>) -> Self {
        Self {
            root_dir: root_dir.as_ref().to_path_buf(),
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    pub fn private_dir(&self) -> PathBuf {
        self.data_dir.join("private")
    }

    pub fn cache_path(&self) -> PathBuf {
        self.private_dir().join("cookies_cache.txt")
    }

    pub fn managed_browser_cookies_path(&self) -> PathBuf {
        self.private_dir().join("playzi_browser_cookies.txt")
    }

    pub fn exported_cookies_dir(&self) -> PathBuf {
        self.root_dir.join("cookies")
    }

    pub fn is_cache_fresh(&self, max_age_hours: u64) -> bool {
        let cache = self.cache_path();
        if !cache.exists() {
            return false;
        }
        if !looks_like_netscape_cookie_file(&cache) {
            let _ = fs::remove_file(&cache);
            return false;
        }
        let Ok(meta) = fs::metadata(&cache) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
            return true;
        };
        elapsed < Duration::from_secs(max_age_hours.saturating_mul(3600))
    }

    pub fn invalidate_cache(&self) -> Result<()> {
        let cache = self.cache_path();
        if cache.exists() {
            fs::remove_file(&cache)?;
        }
        Ok(())
    }

    pub fn cache_from_file(&self, source: impl AsRef<Path>) -> Result<PathBuf> {
        let source = source.as_ref();
        if !source.exists() {
            return Err(CoreError::NotFound(format!(
                "Cookie file not found: {}",
                source.display()
            )));
        }
        if !looks_like_netscape_cookie_file(source) {
            return Err(CoreError::Validation(format!(
                "File is not a valid Netscape cookie file: {}",
                source.display()
            )));
        }

        let private_dir = self.private_dir();
        fs::create_dir_all(&private_dir)?;
        let cache = self.cache_path();

        let should_copy = if !cache.exists() {
            true
        } else {
            match (fs::metadata(source), fs::metadata(&cache)) {
                (Ok(src_meta), Ok(dst_meta)) => {
                    match (src_meta.modified(), dst_meta.modified()) {
                        (Ok(s), Ok(d)) => s > d,
                        _ => true,
                    }
                }
                _ => true,
            }
        };

        if should_copy {
            let tmp = cache.with_extension("txt.tmp");
            fs::copy(source, &tmp)?;
            fs::rename(&tmp, &cache)?;
        }
        restrict_file_permissions(&cache);
        Ok(cache)
    }

    pub fn find_exported_cookie_file(&self) -> Option<PathBuf> {
        let folder = self.exported_cookies_dir();
        if !folder.exists() {
            return None;
        }
        let Ok(entries) = fs::read_dir(&folder) else {
            return None;
        };
        let mut candidates: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("txt"))
                        .unwrap_or(false)
            })
            .filter_map(|p| {
                let mtime = fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                Some((mtime, p))
            })
            .collect();

        candidates.sort_by(|a, b| b.0.cmp(&a.0));
        candidates
            .into_iter()
            .map(|(_, p)| p)
            .find(|p| looks_like_netscape_cookie_file(p))
    }

    /// Resolves the best available Netscape cookie file in priority order:
    /// 1. Explicit `configured_file`
    /// 2. Managed browser export (`data/private/playzi_browser_cookies.txt`)
    /// 3. Exported `cookies/*.txt`
    /// 4. Existing fresh private cache (`data/private/cookies_cache.txt`)
    pub fn resolve_cookie_file(&self, configured_file: Option<&str>) -> Result<PathBuf> {
        if let Some(raw) = configured_file.map(str::trim).filter(|s| !s.is_empty()) {
            let source = PathBuf::from(raw);
            if source.exists() && looks_like_netscape_cookie_file(&source) {
                return self.cache_from_file(&source);
            }
        }

        let managed = self.managed_browser_cookies_path();
        if managed.exists() && looks_like_netscape_cookie_file(&managed) {
            return self.cache_from_file(&managed);
        }

        if let Some(exported) = self.find_exported_cookie_file() {
            return self.cache_from_file(&exported);
        }

        if self.is_cache_fresh(DEFAULT_CACHE_MAX_HOURS) {
            return Ok(self.cache_path());
        }

        Err(CoreError::Auth(
            "No valid Netscape cookie file found in configured path, data/private/, or cookies/."
                .into(),
        ))
    }

    pub fn load_jar(&self, configured_file: Option<&str>) -> Result<CookieJar> {
        let path = self.resolve_cookie_file(configured_file)?;
        CookieJar::from_file(path)
    }
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) {}
