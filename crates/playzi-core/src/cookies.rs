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

    pub fn get_for_host(&self, host: &str, name: &str) -> Option<&str> {
        let now = now_unix_secs();
        self.cookies
            .iter()
            .rev()
            .find(|c| c.name == name && !c.is_expired(now) && c.matches_host(host))
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

    /// Returns a new jar containing only YouTube and Google domains so unrelated browser cookies
    /// are never persisted on disk or leaked in requests.
    pub fn filter_youtube_and_google(&self) -> Self {
        let filtered = self
            .cookies
            .iter()
            .filter(|c| is_youtube_or_google_domain(&c.domain))
            .cloned()
            .collect();
        Self { cookies: filtered }
    }

    /// Returns the SAPISID token for a specific host (e.g. `music.youtube.com`), preventing
    /// `.google.com` cookies from shadowing `.youtube.com` session tokens.
    pub fn sapisid_for_host(&self, host: &str) -> Option<&str> {
        self.get_for_host(host, "SAPISID")
            .or_else(|| self.get_for_host(host, "__Secure-3PAPISID"))
            .or_else(|| self.get_for_host(host, "__Secure-1PAPISID"))
    }

    /// Returns the SAPISID token used to sign YouTube/YouTube Music InnerTube requests.
    pub fn sapisid(&self) -> Option<&str> {
        self.sapisid_for_host("music.youtube.com")
            .or_else(|| self.get("SAPISID"))
            .or_else(|| self.get("__Secure-3PAPISID"))
            .or_else(|| self.get("__Secure-1PAPISID"))
    }

    /// Computes `SAPISIDHASH {timestamp}_{sha1_hex}` for the given origin and UNIX timestamp.
    pub fn sapisidhash(&self, origin: &str, timestamp_secs: i64) -> Option<String> {
        let sapisid = origin_host(origin)
            .and_then(|host| self.sapisid_for_host(host))
            .or_else(|| self.sapisid_for_host("music.youtube.com"))
            .or_else(|| self.sapisid())?;
        Some(compute_sapisidhash(sapisid, origin, timestamp_secs))
    }

    /// Computes the full YouTube/Google InnerTube `Authorization` header value,
    /// emitting `SAPISIDHASH` plus `SAPISID1PHASH` / `SAPISID3PHASH` when
    /// `__Secure-1PAPISID` / `__Secure-3PAPISID` cookies are present.
    pub fn authorization_header(&self, origin: &str, timestamp_secs: i64) -> Option<String> {
        let host = origin_host(origin).unwrap_or("music.youtube.com");
        let sapisid = self.get_for_host(host, "SAPISID").or_else(|| self.get("SAPISID"));
        let p1 = self
            .get_for_host(host, "__Secure-1PAPISID")
            .or_else(|| self.get("__Secure-1PAPISID"));
        let p3 = self
            .get_for_host(host, "__Secure-3PAPISID")
            .or_else(|| self.get("__Secure-3PAPISID"));

        let primary = sapisid.or(p3).or(p1)?;
        let mut parts = vec![compute_sapisidhash(primary, origin, timestamp_secs)];
        if let Some(tok) = p1 {
            parts.push(compute_sapisidhash_scheme("SAPISID1PHASH", tok, origin, timestamp_secs));
        }
        if let Some(tok) = p3 {
            parts.push(compute_sapisidhash_scheme("SAPISID3PHASH", tok, origin, timestamp_secs));
        }
        Some(parts.join(" "))
    }

    /// Builds YouTube Music authenticated InnerTube headers (`Cookie`, `Authorization`, `x-origin`, `X-Goog-AuthUser`).
    pub fn ytmusic_auth_headers(&self, origin: &str) -> Option<BTreeMap<String, String>> {
        self.ytmusic_auth_headers_for_user(origin, 0)
    }

    /// Builds YouTube Music authenticated InnerTube headers for a specific Google `authuser` index.
    pub fn ytmusic_auth_headers_for_user(
        &self,
        origin: &str,
        auth_user: u8,
    ) -> Option<BTreeMap<String, String>> {
        let auth = self.authorization_header(origin, now_unix_secs())?;
        let mut cookie_header = origin_host(origin)
            .map(|host| self.cookie_header_for_host(host))
            .unwrap_or_default();
        if cookie_header.is_empty() {
            cookie_header = self.cookie_header_for_host("music.youtube.com");
        }
        if cookie_header.is_empty() {
            return None;
        }
        let mut headers = BTreeMap::new();
        headers.insert("Cookie".into(), cookie_header);
        headers.insert("Authorization".into(), auth);
        headers.insert("x-origin".into(), origin.to_string());
        headers.insert("X-Goog-AuthUser".into(), auth_user.to_string());
        Some(headers)
    }
}

fn origin_host(origin: &str) -> Option<&str> {
    let stripped = origin
        .trim()
        .strip_prefix("https://")
        .or_else(|| origin.trim().strip_prefix("http://"))
        .unwrap_or_else(|| origin.trim());
    let authority = stripped.split('/').next()?;
    let host = authority.split(':').next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn is_youtube_or_google_domain(domain: &str) -> bool {
    let norm = domain
        .trim()
        .trim_start_matches("#HttpOnly_")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    matches!(
        norm.as_str(),
        "youtube.com" | "google.com" | "googleusercontent.com" | "ytimg.com"
    ) || norm.ends_with(".youtube.com")
        || norm.ends_with(".google.com")
        || norm.ends_with(".googleusercontent.com")
        || norm.ends_with(".ytimg.com")
}

pub fn compute_sapisidhash_scheme(
    scheme: &str,
    sapisid: &str,
    origin: &str,
    timestamp_secs: i64,
) -> String {
    let payload = format!("{timestamp_secs} {sapisid} {origin}");
    let mut hasher = Sha1::new();
    hasher.update(payload.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{scheme} {timestamp_secs}_{digest}")
}

pub fn compute_sapisidhash(sapisid: &str, origin: &str, timestamp_secs: i64) -> String {
    compute_sapisidhash_scheme("SAPISIDHASH", sapisid, origin, timestamp_secs)
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

/// Describes a system browser and the `--cookies-from-browser` spec yt-dlp expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCandidate {
    pub label: String,
    pub ytdlp_spec: String,
}

#[cfg(windows)]
fn hide_window_std(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window_std(_cmd: &mut std::process::Command) {}

#[cfg(windows)]
fn hide_window_tokio(cmd: &mut tokio::process::Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window_tokio(_cmd: &mut tokio::process::Command) {}

/// Finds the most recently modified Firefox-style profile directory containing `cookies.sqlite`.
fn newest_firefox_profile_dir(profiles_root: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(profiles_root).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let cookies_db = dir.join("cookies.sqlite");
        if !cookies_db.is_file() {
            continue;
        }
        let mtime = fs::metadata(&cookies_db)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        match &best {
            Some((best_time, _)) if mtime <= *best_time => {}
            _ => best = Some((mtime, dir)),
        }
    }
    best.map(|(_, path)| path)
}

fn firefox_fork_candidate(label: &str, profiles_roots: &[PathBuf]) -> Option<BrowserCandidate> {
    for root in profiles_roots {
        if let Some(profile) = newest_firefox_profile_dir(root) {
            return Some(BrowserCandidate {
                label: label.to_string(),
                ytdlp_spec: format!("firefox:{}", profile.display()),
            });
        }
    }
    None
}

/// Finds the most recently modified Chromium-style profile directory containing `Network/Cookies` or `Cookies`.
fn newest_chromium_profile_dir(user_data_root: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(user_data_root).ok()?;
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let network_cookies = dir.join("Network").join("Cookies");
        let legacy_cookies = dir.join("Cookies");
        let cookies_db = if network_cookies.is_file() {
            network_cookies
        } else if legacy_cookies.is_file() {
            legacy_cookies
        } else {
            continue;
        };
        let mtime = fs::metadata(&cookies_db)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        match &best {
            Some((best_time, _)) if mtime <= *best_time => {}
            _ => best = Some((mtime, dir)),
        }
    }
    best.map(|(_, path)| path)
}

fn chromium_fork_candidate(label: &str, user_data_roots: &[PathBuf]) -> Option<BrowserCandidate> {
    for root in user_data_roots {
        if let Some(profile) = newest_chromium_profile_dir(root) {
            return Some(BrowserCandidate {
                label: label.to_string(),
                ytdlp_spec: format!("chrome:{}", profile.display()),
            });
        }
    }
    None
}

fn known_chromium_fork_roots(name: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        let base = PathBuf::from(local_appdata);
        if name == "dia" {
            let packages = base.join("Packages");
            if let Ok(entries) = fs::read_dir(&packages) {
                for entry in entries.flatten() {
                    let pkg_name = entry.file_name();
                    let pkg_lower = pkg_name.to_string_lossy().to_ascii_lowercase();
                    if pkg_lower.starts_with("thebrowsercompany.dia_") {
                        roots.push(
                            entry
                                .path()
                                .join("LocalCache")
                                .join("Local")
                                .join("Dia")
                                .join("User Data"),
                        );
                    }
                }
            }
            roots.push(base.join("Dia").join("User Data"));
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(home) = std::env::var("HOME") {
        let base = PathBuf::from(home);
        if name == "dia" {
            roots.push(
                base.join("Library")
                    .join("Application Support")
                    .join("Dia")
                    .join("User Data"),
            );
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Ok(home) = std::env::var("HOME") {
        let base = PathBuf::from(home);
        if name == "dia" {
            roots.push(base.join(".config").join("Dia"));
        }
    }
    roots
}

fn matches_dia_browser(combined_lower: &str) -> bool {
    combined_lower.contains("dia.exe")
        || combined_lower.contains("thebrowsercompany.dia")
        || combined_lower.contains("thebrowser.dia")
        || combined_lower.contains("dia.desktop")
}

fn known_firefox_fork_roots(name: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    if let Ok(appdata) = std::env::var("APPDATA") {
        let base = PathBuf::from(appdata);
        match name {
            "zen" => roots.push(base.join("zen").join("Profiles")),
            "floorp" => roots.push(base.join("Floorp").join("Profiles")),
            "librewolf" => roots.push(base.join("librewolf").join("Profiles")),
            "waterfox" => roots.push(base.join("Waterfox").join("Profiles")),
            _ => {}
        }
    }
    #[cfg(not(windows))]
    if let Ok(home) = std::env::var("HOME") {
        let base = PathBuf::from(home);
        match name {
            "zen" => {
                roots.push(base.join(".zen"));
                roots.push(base.join(".var/app/io.github.zen_browser.zen/.zen"));
            }
            "floorp" => roots.push(base.join(".floorp")),
            "librewolf" => roots.push(base.join(".librewolf")),
            "waterfox" => roots.push(base.join(".waterfox")),
            _ => {}
        }
    }
    roots
}

fn push_unique_candidate(list: &mut Vec<BrowserCandidate>, candidate: BrowserCandidate) {
    if !list
        .iter()
        .any(|c| c.ytdlp_spec.eq_ignore_ascii_case(&candidate.ytdlp_spec))
    {
        list.push(candidate);
    }
}

#[cfg(windows)]
fn query_windows_registry_value(key: &str, value_name: Option<&str>) -> Option<String> {
    let mut cmd = std::process::Command::new("reg");
    cmd.arg("query").arg(key);
    match value_name {
        Some(v) => {
            cmd.arg("/v").arg(v);
        }
        None => {
            cmd.arg("/ve");
        }
    }
    hide_window_std(&mut cmd);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(pos) = line.find("REG_SZ") {
            let val = line[pos + "REG_SZ".len()..].trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn detect_primary_browser_windows() -> Option<BrowserCandidate> {
    let prog_id = query_windows_registry_value(
        r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoiceLatest\ProgId",
        Some("ProgId"),
    )
    .or_else(|| {
        query_windows_registry_value(
            r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
            Some("ProgId"),
        )
    })?;

    let command_str = query_windows_registry_value(
        &format!(r"HKCU\Software\Classes\{prog_id}\shell\open\command"),
        None,
    )
    .or_else(|| {
        query_windows_registry_value(
            &format!(r"HKLM\Software\Classes\{prog_id}\shell\open\command"),
            None,
        )
    })
    .unwrap_or_default();

    let combined = format!("{} {}", prog_id, command_str).to_ascii_lowercase();
    if matches_dia_browser(&combined) {
        if let Some(c) = chromium_fork_candidate("Dia", &known_chromium_fork_roots("dia")) {
            return Some(c);
        }
    }
    if combined.contains("zen.exe") || combined.contains("zen browser") {
        if let Some(c) = firefox_fork_candidate("Zen Browser", &known_firefox_fork_roots("zen")) {
            return Some(c);
        }
    }
    if combined.contains("floorp") {
        if let Some(c) = firefox_fork_candidate("Floorp", &known_firefox_fork_roots("floorp")) {
            return Some(c);
        }
    }
    if combined.contains("librewolf") {
        if let Some(c) = firefox_fork_candidate("LibreWolf", &known_firefox_fork_roots("librewolf"))
        {
            return Some(c);
        }
    }
    if combined.contains("waterfox") {
        if let Some(c) = firefox_fork_candidate("Waterfox", &known_firefox_fork_roots("waterfox")) {
            return Some(c);
        }
    }
    if combined.contains("firefox") {
        return Some(BrowserCandidate {
            label: "Firefox".into(),
            ytdlp_spec: "firefox".into(),
        });
    }
    if combined.contains("brave") {
        return Some(BrowserCandidate {
            label: "Brave".into(),
            ytdlp_spec: "brave".into(),
        });
    }
    if combined.contains("msedge") || combined.contains("edge") {
        return Some(BrowserCandidate {
            label: "Edge".into(),
            ytdlp_spec: "edge".into(),
        });
    }
    if combined.contains("opera") {
        return Some(BrowserCandidate {
            label: "Opera".into(),
            ytdlp_spec: "opera".into(),
        });
    }
    if combined.contains("vivaldi") {
        return Some(BrowserCandidate {
            label: "Vivaldi".into(),
            ytdlp_spec: "vivaldi".into(),
        });
    }
    if combined.contains("chromium") {
        return Some(BrowserCandidate {
            label: "Chromium".into(),
            ytdlp_spec: "chromium".into(),
        });
    }
    if combined.contains("chrome") {
        return Some(BrowserCandidate {
            label: "Chrome".into(),
            ytdlp_spec: "chrome".into(),
        });
    }
    None
}

#[cfg(not(windows))]
fn detect_primary_browser_unix() -> Option<BrowserCandidate> {
    let output = std::process::Command::new("xdg-settings")
        .args(["get", "default-web-browser"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let lower = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if matches_dia_browser(&lower) {
        if let Some(c) = chromium_fork_candidate("Dia", &known_chromium_fork_roots("dia")) {
            return Some(c);
        }
    }
    if lower.contains("zen") {
        if let Some(c) = firefox_fork_candidate("Zen Browser", &known_firefox_fork_roots("zen")) {
            return Some(c);
        }
    }
    if lower.contains("floorp") {
        if let Some(c) = firefox_fork_candidate("Floorp", &known_firefox_fork_roots("floorp")) {
            return Some(c);
        }
    }
    if lower.contains("librewolf") {
        if let Some(c) = firefox_fork_candidate("LibreWolf", &known_firefox_fork_roots("librewolf"))
        {
            return Some(c);
        }
    }
    if lower.contains("firefox") {
        return Some(BrowserCandidate {
            label: "Firefox".into(),
            ytdlp_spec: "firefox".into(),
        });
    }
    if lower.contains("brave") {
        return Some(BrowserCandidate {
            label: "Brave".into(),
            ytdlp_spec: "brave".into(),
        });
    }
    if lower.contains("edge") {
        return Some(BrowserCandidate {
            label: "Edge".into(),
            ytdlp_spec: "edge".into(),
        });
    }
    if lower.contains("opera") {
        return Some(BrowserCandidate {
            label: "Opera".into(),
            ytdlp_spec: "opera".into(),
        });
    }
    if lower.contains("vivaldi") {
        return Some(BrowserCandidate {
            label: "Vivaldi".into(),
            ytdlp_spec: "vivaldi".into(),
        });
    }
    if lower.contains("chromium") {
        return Some(BrowserCandidate {
            label: "Chromium".into(),
            ytdlp_spec: "chromium".into(),
        });
    }
    if lower.contains("chrome") {
        return Some(BrowserCandidate {
            label: "Chrome".into(),
            ytdlp_spec: "chrome".into(),
        });
    }
    None
}

/// Detects the user's default browser first, followed by installed fallback browsers.
pub fn detect_default_browser_candidates() -> Vec<BrowserCandidate> {
    let mut out = Vec::new();
    #[cfg(windows)]
    if let Some(primary) = detect_primary_browser_windows() {
        push_unique_candidate(&mut out, primary);
    }
    #[cfg(not(windows))]
    if let Some(primary) = detect_primary_browser_unix() {
        push_unique_candidate(&mut out, primary);
    }

    for (name, label) in [("dia", "Dia")] {
        if let Some(c) = chromium_fork_candidate(label, &known_chromium_fork_roots(name)) {
            push_unique_candidate(&mut out, c);
        }
    }

    for (name, label) in [
        ("zen", "Zen Browser"),
        ("floorp", "Floorp"),
        ("librewolf", "LibreWolf"),
        ("waterfox", "Waterfox"),
    ] {
        if let Some(c) = firefox_fork_candidate(label, &known_firefox_fork_roots(name)) {
            push_unique_candidate(&mut out, c);
        }
    }

    for (spec, label) in [
        ("firefox", "Firefox"),
        ("edge", "Edge"),
        ("chrome", "Chrome"),
        ("brave", "Brave"),
        ("opera", "Opera"),
        ("vivaldi", "Vivaldi"),
        ("chromium", "Chromium"),
    ] {
        push_unique_candidate(
            &mut out,
            BrowserCandidate {
                label: label.into(),
                ytdlp_spec: spec.into(),
            },
        );
    }
    out
}

/// Resolves browser candidates honoring an explicit `COOKIES_BROWSER` setting when provided.
pub fn candidates_for_config(cookies_browser: &str) -> Vec<BrowserCandidate> {
    let mut out = Vec::new();
    let trimmed = cookies_browser.trim();
    let lower = trimmed.to_ascii_lowercase();
    if !lower.is_empty() && lower != "playzi" && lower != "auto" && lower != "default" {
        match lower.as_str() {
            "dia" => {
                if let Some(c) =
                    chromium_fork_candidate("Dia", &known_chromium_fork_roots("dia"))
                {
                    push_unique_candidate(&mut out, c);
                }
            }
            "zen" => {
                if let Some(c) =
                    firefox_fork_candidate("Zen Browser", &known_firefox_fork_roots("zen"))
                {
                    push_unique_candidate(&mut out, c);
                }
            }
            "floorp" => {
                if let Some(c) =
                    firefox_fork_candidate("Floorp", &known_firefox_fork_roots("floorp"))
                {
                    push_unique_candidate(&mut out, c);
                }
            }
            "librewolf" => {
                if let Some(c) =
                    firefox_fork_candidate("LibreWolf", &known_firefox_fork_roots("librewolf"))
                {
                    push_unique_candidate(&mut out, c);
                }
            }
            "waterfox" => {
                if let Some(c) =
                    firefox_fork_candidate("Waterfox", &known_firefox_fork_roots("waterfox"))
                {
                    push_unique_candidate(&mut out, c);
                }
            }
            _ => {
                push_unique_candidate(
                    &mut out,
                    BrowserCandidate {
                        label: trimmed.to_string(),
                        ytdlp_spec: trimmed.to_string(),
                    },
                );
            }
        }
    }

    for c in detect_default_browser_candidates() {
        push_unique_candidate(&mut out, c);
    }
    out
}

/// Opens `url` in the operating system's default web browser.
pub fn open_url_in_default_browser(url: &str) -> Result<()> {
    let trimmed = url.trim();
    let parsed = url::Url::parse(trimmed)
        .map_err(|_| CoreError::Validation("URL inválida para abrir en el navegador".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CoreError::Validation(
            "Solo se permiten enlaces http/https para abrir en el navegador".into(),
        ));
    }
    let safe_url = parsed.as_str();
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new("rundll32.exe");
        cmd.args(["url.dll,FileProtocolHandler", safe_url]);
        hide_window_std(&mut cmd);
        cmd.spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(safe_url).spawn()?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(safe_url).spawn()?;
        return Ok(());
    }
}

/// Exports cookies from a browser profile via `yt-dlp --cookies-from-browser <spec> --cookies <tmp>`.
pub async fn export_cookies_from_browser(
    ytdlp_bin: &str,
    spec: &str,
    temp_dir: &Path,
) -> Result<CookieJar> {
    fs::create_dir_all(temp_dir)?;
    let unique = format!(
        "playzi_browser_import_{}_{}.txt",
        std::process::id(),
        now_unix_secs()
    );
    let tmp_path = temp_dir.join(unique);
    let _ = fs::remove_file(&tmp_path);

    let mut cmd = tokio::process::Command::new(ytdlp_bin);
    cmd.args([
        "--cookies-from-browser",
        spec,
        "--cookies",
        &tmp_path.to_string_lossy(),
        "--skip-download",
        "--flat-playlist",
        "--playlist-end",
        "1",
        "--no-warnings",
        "https://www.youtube.com/feed/subscriptions",
    ]);
    hide_window_tokio(&mut cmd);

    let output_res = tokio::time::timeout(Duration::from_secs(25), cmd.output()).await;
    let output = match output_res {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(CoreError::Auth(format!("Failed to run {ytdlp_bin}: {e}")));
        }
        Err(_) => {
            let _ = fs::remove_file(&tmp_path);
            return Err(CoreError::Auth(format!(
                "Timeout exporting cookies from {spec}"
            )));
        }
    };

    let jar_result = if tmp_path.exists() && looks_like_netscape_cookie_file(&tmp_path) {
        CookieJar::from_file(&tmp_path)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(CoreError::Auth(format!(
            "Could not export cookies from {spec}: {}",
            stderr.lines().next().unwrap_or("no cookie file produced")
        )))
    };
    let _ = fs::remove_file(&tmp_path);

    let jar = jar_result?.filter_youtube_and_google();
    if jar.sapisid_for_host("music.youtube.com").is_none() {
        return Err(CoreError::Auth(format!(
            "No authenticated YouTube session found in {spec}"
        )));
    }
    Ok(jar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_dia_browser_detects_dia_without_false_positives() {
        assert!(matches_dia_browser(
            r#"appx06t8cqx "c:\program files\windowsapps\thebrowsercompany.dia_1.7.4.2_x64__ttt1ap7aakyb4\dia.exe""#
        ));
        assert!(matches_dia_browser("thebrowsercompany.dia"));
        assert!(matches_dia_browser("dia.desktop"));
        assert!(!matches_dia_browser("vivaldi"));
        assert!(!matches_dia_browser("msedgehtm"));
        assert!(!matches_dia_browser("media-player"));
    }

    #[test]
    fn chromium_fork_candidate_finds_network_cookies_profile() {
        let base = std::env::temp_dir().join(format!(
            "playzi_dia_test_{}_{}",
            std::process::id(),
            now_unix_secs()
        ));
        let default_net = base.join("Default").join("Network");
        fs::create_dir_all(&default_net).unwrap();
        fs::write(default_net.join("Cookies"), b"sqlite").unwrap();

        let candidate = chromium_fork_candidate("Dia", std::slice::from_ref(&base));
        let _ = fs::remove_dir_all(&base);

        let candidate = candidate.expect("expected Dia candidate");
        assert_eq!(candidate.label, "Dia");
        assert!(candidate.ytdlp_spec.starts_with("chrome:"));
        assert!(candidate.ytdlp_spec.ends_with("Default"));
    }
}
