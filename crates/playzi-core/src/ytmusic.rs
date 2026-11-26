use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::Local;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::cookies::CookieJar;
use crate::error::{CoreError, Result};
use crate::state::StateStore;
use crate::youtube::{
    MediaItem, format_duration, last_thumbnail_url, parse_duration_text, text_from_node,
    walk_renderers,
};

pub const DEFAULT_YTMUSIC_BASE_URL: &str = "https://music.youtube.com";
const DEFAULT_MUSIC_CLIENT_VERSION: &str = "1.20250501.01.00";
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36";

#[derive(Debug, Clone)]
struct CachedTrackMeta {
    duration: u64,
    channel_url: Option<String>,
    author: Option<String>,
}

fn track_meta_cache() -> &'static Mutex<HashMap<String, CachedTrackMeta>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedTrackMeta>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MusicPlaylistSummary {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub url: String,
    pub thumbnail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    pub browse_id: String,
    pub name: String,
    pub description: String,
    pub thumbnail: String,
    pub songs: Vec<MediaItem>,
    pub releases: Vec<MusicPlaylistSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MusicAuthStatus {
    pub state: String,
    pub account_name: String,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoogleAccountOption {
    pub auth_user: u8,
    pub name: String,
    pub handle: String,
    pub photo: String,
}

#[derive(Debug, Clone)]
pub struct YtMusicClient {
    http: reqwest::Client,
    base_url: String,
    client_version: String,
    hl: String,
    gl: String,
    cookies: Option<CookieJar>,
    auth_user: u8,
}

impl YtMusicClient {
    pub fn new(cookies: Option<CookieJar>) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
        headers.insert(ORIGIN, HeaderValue::from_static(DEFAULT_YTMUSIC_BASE_URL));
        headers.insert(REFERER, HeaderValue::from_static("https://music.youtube.com/"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(10)
            .build()?;

        Ok(Self::with_client(http, DEFAULT_YTMUSIC_BASE_URL, cookies))
    }

    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        cookies: Option<CookieJar>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client_version: DEFAULT_MUSIC_CLIENT_VERSION.to_string(),
            hl: "es".into(),
            gl: "EC".into(),
            cookies,
            auth_user: 0,
        }
    }

    pub fn with_auth_user(mut self, auth_user: u8) -> Self {
        self.auth_user = auth_user;
        self
    }

    pub fn has_auth(&self) -> bool {
        self.cookies
            .as_ref()
            .and_then(|jar| jar.sapisid())
            .is_some()
    }

    fn music_context(&self) -> Value {
        json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": self.client_version,
                "hl": self.hl,
                "gl": self.gl,
            }
        })
    }

    fn auth_header_map(&self) -> HeaderMap {
        let mut map = HeaderMap::new();
        if let Some(jar) = &self.cookies {
            if let Some(auth_headers) =
                jar.ytmusic_auth_headers_for_user(DEFAULT_YTMUSIC_BASE_URL, self.auth_user)
            {
                for (k, v) in auth_headers {
                    if let (Ok(name), Ok(val)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(&v),
                    ) {
                        map.insert(name, val);
                    }
                }
            } else {
                let cookie_str = jar.cookie_header_for_host("music.youtube.com");
                if !cookie_str.is_empty() {
                    if let Ok(val) = HeaderValue::from_str(&cookie_str) {
                        map.insert(reqwest::header::COOKIE, val);
                    }
                }
            }
        }
        map
    }

    async fn post_innertube(&self, endpoint: &str, mut body: Value) -> Result<Value> {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("context".into(), self.music_context());
        }
        let url = format!(
            "{}/youtubei/v1/{}?prettyPrint=false",
            self.base_url, endpoint
        );
        let mut headers = self.auth_header_map();
        headers
            .entry(USER_AGENT)
            .or_insert_with(|| HeaderValue::from_static(DEFAULT_USER_AGENT));
        headers
            .entry(ORIGIN)
            .or_insert_with(|| HeaderValue::from_static(DEFAULT_YTMUSIC_BASE_URL));
        headers
            .entry(REFERER)
            .or_insert_with(|| HeaderValue::from_static("https://music.youtube.com/"));
        headers.insert(
            HeaderName::from_static("x-youtube-client-name"),
            HeaderValue::from_static("67"),
        );
        if let Ok(ver) = HeaderValue::from_str(&self.client_version) {
            headers.insert(HeaderName::from_static("x-youtube-client-version"), ver);
        }
        let resp = self
            .http
            .post(url)
            .headers(headers)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json::<Value>().await?)
    }

    pub async fn browse(&self, browse_id: &str) -> Result<Value> {
        self.post_innertube("browse", json!({ "browseId": browse_id }))
            .await
    }

    /// Verifies whether the configured cookie jar is authenticated on YouTube Music.
    pub async fn check_auth(&self, store: Option<&StateStore>) -> Result<MusicAuthStatus> {
        let checked_at = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        if !self.has_auth() {
            let status = MusicAuthStatus {
                state: "expired".into(),
                account_name: String::new(),
                checked_at,
            };
            persist_auth_status(store, &status);
            return Ok(status);
        }

        let data = self.post_innertube("account/account_menu", json!({})).await;
        let status = match data {
            Ok(val) => {
                let account_name = val
                    .pointer("/actions/0/openPopupAction/popup/multiPageMenuRenderer/header/activeAccountHeaderRenderer/accountName")
                    .and_then(|n| text_from_node(Some(n)))
                    .unwrap_or_default();
                let state = if account_name.is_empty() {
                    "expired"
                } else {
                    "ok"
                };
                MusicAuthStatus {
                    state: state.into(),
                    account_name,
                    checked_at,
                }
            }
            Err(_) => MusicAuthStatus {
                state: "expired".into(),
                account_name: String::new(),
                checked_at,
            },
        };

        persist_auth_status(store, &status);
        Ok(status)
    }

    /// Probes signed-in Google accounts (`X-Goog-AuthUser: 0..5`) present in the cookie jar.
    pub async fn list_signed_in_accounts(&self) -> Vec<GoogleAccountOption> {
        let mut out = Vec::new();
        if !self.has_auth() {
            return out;
        }
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for idx in 0..5u8 {
            let probe = self.clone().with_auth_user(idx);
            let Ok(val) = probe.post_innertube("account/account_menu", json!({})).await else {
                if idx > 0 {
                    break;
                }
                continue;
            };
            let header = val.pointer(
                "/actions/0/openPopupAction/popup/multiPageMenuRenderer/header/activeAccountHeaderRenderer",
            );
            let Some(header) = header else {
                if idx > 0 {
                    break;
                }
                continue;
            };
            let name = text_from_node(header.get("accountName")).unwrap_or_default();
            if name.is_empty() {
                if idx > 0 {
                    break;
                }
                continue;
            }
            let handle = text_from_node(header.get("channelHandle"))
                .or_else(|| text_from_node(header.get("email")))
                .unwrap_or_default();
            let photo = header
                .pointer("/accountPhoto/thumbnails/0/url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let key = (name.to_ascii_lowercase(), handle.to_ascii_lowercase());
            if !seen.insert(key) {
                break;
            }
            out.push(GoogleAccountOption {
                auth_user: idx,
                name,
                handle,
                photo,
            });
        }
        out
    }

    /// Searches YouTube Music via `/youtubei/v1/search` with optional filter (`songs`, `videos`, `albums`, `artists`, `playlists`).
    pub async fn search_music(
        &self,
        query: &str,
        filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MediaItem>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut payload = json!({ "query": query });
        if let Some(params) = filter.and_then(search_filter_params) {
            payload["params"] = Value::String(params.to_string());
        }
        let data = self.post_innertube("search", payload).await?;
        let mut items = parse_music_items(&data, limit.clamp(1, 50));
        self.enrich_missing_durations(&mut items).await;
        if let Some(f) = filter {
            for item in &mut items {
                if item.result_type.is_none() {
                    item.result_type = Some(f.to_ascii_lowercase());
                }
            }
        }
        Ok(items)
    }

    /// Fetches personalized YouTube Music home recommendations (`FEmusic_home`),
    /// optionally following continuation pages to collect more songs, and enriches missing durations.
    pub async fn fetch_recommendations(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_pages(store, 1, false).await
    }

    /// Fetches additional personalized YouTube Music recommendations via continuation tokens
    /// and merges them into the persisted recommendation list.
    pub async fn fetch_recommendations_more(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_pages(store, 3, true).await
    }

    async fn fetch_recommendations_pages(
        &self,
        store: Option<&StateStore>,
        max_pages: usize,
        merge_existing: bool,
    ) -> Result<Vec<MediaItem>> {
        let mut data = self.browse("FEmusic_home").await?;
        let mut playlists = parse_music_playlists(&data, 16);
        let mut songs = parse_music_items(&data, 50);

        let mut extra_pages = 0usize;
        while extra_pages + 1 < max_pages {
            let Some(token) = extract_music_continuation(&data) else {
                break;
            };
            let Ok(next_data) = self
                .post_innertube(
                    "browse",
                    json!({ "continuation": token }),
                )
                .await
            else {
                break;
            };
            playlists.extend(parse_music_playlists(&next_data, 12));
            songs.extend(parse_music_items(&next_data, 40));
            data = next_data;
            extra_pages += 1;
        }

        let mut items: Vec<MediaItem> = Vec::new();
        let mut seen = HashSet::new();

        if merge_existing {
            if let Some(st) = store {
                if let Ok(guard) = st.read() {
                    for val in &guard.music_recommendations {
                        if let Ok(mut existing) = serde_json::from_value::<MediaItem>(val.clone()) {
                            // Clean legacy compound channel names on merge too
                            existing.channel = clean_music_channel_text(&existing.channel);
                            if seen.insert(existing.id.clone()) {
                                items.push(existing);
                            }
                        }
                    }
                }
            }
        }

        for p in playlists {
            if seen.insert(p.id.clone()) {
                let is_mix = p.id.starts_with("RD");
                items.push(MediaItem {
                    id: p.id,
                    url: p.url,
                    title: p.title,
                    channel: if p.subtitle.is_empty() {
                        "YouTube Music".into()
                    } else {
                        p.subtitle
                    },
                    channel_url: None,
                    duration: None,
                    duration_text: if is_mix { "Mix".into() } else { "Playlist".into() },
                    thumbnail: if p.thumbnail.is_empty() {
                        None
                    } else {
                        Some(p.thumbnail)
                    },
                    view_count: None,
                    live: false,
                    members_only: false,
                    result_type: Some(if is_mix { "mix".into() } else { "playlist".into() }),
                    watched_percent: None,
                });
            }
        }
        for song in songs {
            if seen.insert(song.id.clone()) {
                items.push(song);
            }
        }

        // Enrich missing durations for all playable YT Music items (including merged legacy items).
        self.enrich_missing_durations(&mut items).await;

        if let Some(st) = store {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let serialized: Vec<Value> = items
                .iter()
                .filter_map(|i| serde_json::to_value(i).ok())
                .collect();
            st.update(|s| {
                s.music_recommendations = serialized;
                s.music_recom_last_sync = now;
                Ok(())
            })?;
        }
        Ok(items)
    }

    /// Concurrently enriches items missing duration (or channel_url) via `/youtubei/v1/player`.
    pub async fn enrich_missing_durations(&self, items: &mut [MediaItem]) {
        let mut missing_indices = Vec::new();
        for (idx, item) in items.iter_mut().enumerate() {
            let is_playable = !matches!(
                item.result_type.as_deref(),
                Some("mix" | "playlist" | "album" | "artist")
            );
            if !is_playable || item.id.is_empty() {
                continue;
            }
            if item.duration.is_some() && item.duration_text != "--:--" {
                continue;
            }
            if let Ok(cache) = track_meta_cache().lock() {
                if let Some(cached) = cache.get(&item.id) {
                    item.duration = Some(cached.duration);
                    item.duration_text = format_duration(Some(cached.duration));
                    if item.channel_url.is_none() {
                        item.channel_url = cached.channel_url.clone();
                    }
                    if (item.channel.is_empty() || item.channel == "YouTube Music")
                        && cached.author.is_some()
                    {
                        item.channel = cached.author.clone().unwrap_or_default();
                    }
                    continue;
                }
            }
            missing_indices.push((idx, item.id.clone()));
        }

        if missing_indices.is_empty() {
            return;
        }

        // Bound concurrency to 8 and cap enrichment to 60 tracks per call for responsiveness.
        let sem = Arc::new(Semaphore::new(8));
        let mut join_set = tokio::task::JoinSet::new();

        for (idx, video_id) in missing_indices.into_iter().take(60) {
            let client = self.clone();
            let sem_clone = sem.clone();
            join_set.spawn(async move {
                let _permit = sem_clone.acquire().await.ok()?;
                let meta = tokio::time::timeout(
                    Duration::from_secs(6),
                    client.fetch_track_player_meta(&video_id),
                )
                .await
                .ok()??;
                Some((idx, video_id, meta))
            });
        }

        while let Some(res) = join_set.join_next().await {
            if let Ok(Some((idx, video_id, meta))) = res {
                if let Ok(mut cache) = track_meta_cache().lock() {
                    if cache.len() > 1000 {
                        cache.clear();
                    }
                    cache.insert(video_id, meta.clone());
                }
                if let Some(item) = items.get_mut(idx) {
                    item.duration = Some(meta.duration);
                    item.duration_text = format_duration(Some(meta.duration));
                    if item.channel_url.is_none() {
                        item.channel_url = meta.channel_url;
                    }
                    if (item.channel.is_empty() || item.channel == "YouTube Music")
                        && meta.author.is_some()
                    {
                        item.channel = meta.author.unwrap_or_default();
                    }
                }
            }
        }
    }

    async fn fetch_track_player_meta(&self, video_id: &str) -> Option<CachedTrackMeta> {
        let extract = |data: &Value| -> Option<CachedTrackMeta> {
            let details = data.get("videoDetails")?;
            let duration = details
                .get("lengthSeconds")
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<u64>().ok())
                        .or_else(|| v.as_u64())
                })
                .filter(|&d| d > 0)?;
            let channel_url = details
                .get("channelId")
                .and_then(Value::as_str)
                .filter(|s| s.starts_with("UC"))
                .map(|cid| format!("https://www.youtube.com/channel/{cid}"));
            let author = details
                .get("author")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            Some(CachedTrackMeta {
                duration,
                channel_url,
                author,
            })
        };

        if let Ok(data) = self
            .post_innertube(
                "player",
                json!({
                    "videoId": video_id,
                    "racyCheckOk": true,
                    "contentCheckOk": true,
                }),
            )
            .await
        {
            if let Some(meta) = extract(&data) {
                return Some(meta);
            }
        }

        // Fallback to standard YouTube Web InnerTube /player when WEB_REMIX omits videoDetails.
        if self.base_url.contains("music.youtube.com") {
            let web_body = json!({
                "videoId": video_id,
                "racyCheckOk": true,
                "contentCheckOk": true,
                "context": {
                    "client": {
                        "clientName": "WEB",
                        "clientVersion": "2.20250501.00.00",
                        "hl": self.hl,
                        "gl": self.gl,
                    }
                }
            });
            if let Ok(resp) = self
                .http
                .post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false")
                .header(USER_AGENT, DEFAULT_USER_AGENT)
                .header(ORIGIN, "https://www.youtube.com")
                .header(REFERER, "https://www.youtube.com/")
                .json(&web_body)
                .send()
                .await
            {
                if let Ok(ok_resp) = resp.error_for_status() {
                    if let Ok(data) = ok_resp.json::<Value>().await {
                        if let Some(meta) = extract(&data) {
                            return Some(meta);
                        }
                    }
                }
            }
        }
        None
    }

    /// Fetches items for a YouTube Music mix (`RD...`) or playlist (`VL...`, `PL...`, `OLAK...`).
    /// Pass `limit: None` to fetch all tracks in a playlist without an artificial cap.
    pub async fn fetch_mix_or_playlist_items(
        &self,
        url_or_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<MediaItem>> {
        let playlist_id = crate::youtube::extract_playlist_id_from_url(url_or_id)
            .unwrap_or_else(|| url_or_id.trim().to_string());
        if playlist_id.is_empty() {
            return Ok(Vec::new());
        }

        if playlist_id.starts_with("RD") {
            let mix_limit = limit.unwrap_or(50).max(1);
            let seed_vid = crate::youtube::extract_video_id(url_or_id).or_else(|| {
                if playlist_id.len() == 13 && playlist_id.starts_with("RD") {
                    Some(playlist_id[2..].to_string())
                } else if playlist_id.len() == 17 && playlist_id.starts_with("RDAMVM") {
                    Some(playlist_id[6..].to_string())
                } else {
                    None
                }
            });
            let mut payload = json!({
                "playlistId": playlist_id,
                "enablePersistentPlaylistPanel": true,
                "isAudioOnly": true,
            });
            if let Some(vid) = seed_vid.as_deref().filter(|v| !v.is_empty()) {
                payload["videoId"] = Value::String(vid.to_string());
            }
            if let Ok(data) = self.post_innertube("next", payload).await {
                let mut items = parse_music_next_items(&data, mix_limit);
                if !items.is_empty() {
                    self.enrich_missing_durations(&mut items).await;
                    return Ok(items);
                }
            }
        }

        self.fetch_playlist_songs_with_limit(&playlist_id, limit).await
    }

    /// Enriches any stored `music_recommendations` in `StateStore` that are still missing duration.
    pub async fn enrich_stored_recommendations(&self, store: &StateStore) -> Result<Vec<Value>> {
        let raw_items = store.read()?.music_recommendations;
        let has_missing = raw_items.iter().any(|val| {
            let rtype = val.get("resultType").and_then(Value::as_str);
            let is_playable = !matches!(rtype, Some("mix" | "playlist" | "album" | "artist"));
            let has_id = val
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty());
            let dur_missing = val.get("duration").is_none()
                || val.get("duration").is_some_and(Value::is_null)
                || val.get("durationText").and_then(Value::as_str) == Some("--:--");
            is_playable && has_id && dur_missing
        });
        if !has_missing {
            return Ok(raw_items);
        }

        let mut parsed: Vec<MediaItem> = raw_items
            .iter()
            .filter_map(|v| serde_json::from_value::<MediaItem>(v.clone()).ok())
            .collect();
        if parsed.is_empty() {
            return Ok(raw_items);
        }
        self.enrich_missing_durations(&mut parsed).await;
        let serialized: Vec<Value> = parsed
            .iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect();
        let serialized_clone = serialized.clone();
        let _ = store.update(|s| {
            s.music_recommendations = serialized_clone;
            Ok(())
        });
        Ok(serialized)
    }

    /// Fetches user playlists from YouTube Music (`FEmusic_liked_playlists`).
    pub async fn fetch_playlists(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MusicPlaylistSummary>> {
        if !self.has_auth() {
            return Err(CoreError::Auth(
                "YouTube Music playlists require authenticated Netscape cookies (SAPISID).".into(),
            ));
        }
        let data = self.browse("FEmusic_liked_playlists").await?;
        let playlists = parse_music_playlists(&data, 40);

        if let Some(st) = store {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            let serialized: Vec<Value> = playlists
                .iter()
                .filter_map(|p| serde_json::to_value(p).ok())
                .collect();
            st.update(|s| {
                s.music_playlists = serialized;
                s.music_playlists_last_sync = now;
                Ok(())
            })?;
        }
        Ok(playlists)
    }

    /// Fetches all songs in a YouTube Music playlist by playlist ID (following continuations).
    pub async fn fetch_playlist_songs(&self, playlist_id: &str) -> Result<Vec<MediaItem>> {
        self.fetch_playlist_songs_with_limit(playlist_id, None).await
    }

    /// Fetches songs in a YouTube Music playlist by playlist ID, following continuation pages
    /// until all songs are collected or `limit` (if `Some`) is reached.
    pub async fn fetch_playlist_songs_with_limit(
        &self,
        playlist_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<MediaItem>> {
        let clean = playlist_id.trim();
        if clean.is_empty() {
            return Err(CoreError::Validation("Empty playlist ID".into()));
        }
        let browse_id = if clean.starts_with("VL") || clean.starts_with("MPREb_") {
            clean.to_string()
        } else {
            format!("VL{clean}")
        };
        let mut data = self.browse(&browse_id).await?;
        let mut items: Vec<MediaItem> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_tokens: HashSet<String> = HashSet::new();

        let target_limit = limit.unwrap_or(usize::MAX);
        // Safety ceiling against malformed/infinite server loops (~50,000 tracks at 100/page).
        const MAX_PAGES: usize = 500;

        for page_idx in 0..MAX_PAGES {
            let page_items = parse_playlist_page_items(&data, page_idx == 0);
            let mut added_this_page = 0usize;
            for item in page_items {
                if seen_ids.insert(item.id.clone()) {
                    items.push(item);
                    added_this_page += 1;
                    if items.len() >= target_limit {
                        break;
                    }
                }
            }

            if items.len() >= target_limit {
                break;
            }

            let Some(token) = extract_playlist_continuation(&data, page_idx == 0) else {
                break;
            };
            if !seen_tokens.insert(token.clone()) {
                break;
            }
            if page_idx > 0 && added_this_page == 0 {
                break;
            }

            match self
                .post_innertube("browse", json!({ "continuation": token }))
                .await
            {
                Ok(next_data) => data = next_data,
                Err(_) => break,
            }
        }

        self.enrich_missing_durations(&mut items).await;
        Ok(items)
    }

    /// Fetches artist profile, top songs, and releases for a YouTube Music artist browse ID.
    pub async fn get_artist_info(&self, browse_id: &str) -> Result<ArtistInfo> {
        let clean = browse_id.trim();
        if clean.is_empty() {
            return Err(CoreError::Validation("Empty artist browseId".into()));
        }
        let data = self.browse(clean).await?;
        let header = data
            .pointer("/header/musicImmersiveHeaderRenderer")
            .or_else(|| data.pointer("/header/musicVisualHeaderRenderer"));

        let name = header
            .and_then(|h| text_from_node(h.get("title")))
            .unwrap_or_else(|| "Artista".into());
        let description = header
            .and_then(|h| text_from_node(h.get("description")))
            .unwrap_or_default();
        let thumbnail = header
            .and_then(|h| h.get("thumbnail"))
            .and_then(|t| first_nested_thumbnail(t))
            .unwrap_or_default();

        let mut songs = parse_music_items(&data, 20);
        self.enrich_missing_durations(&mut songs).await;
        let releases = parse_music_playlists(&data, 20);

        Ok(ArtistInfo {
            browse_id: clean.to_string(),
            name,
            description,
            thumbnail,
            songs,
            releases,
        })
    }
}

fn persist_auth_status(store: Option<&StateStore>, status: &MusicAuthStatus) {
    if let Some(st) = store {
        let _ = st.update(|s| {
            s.music_auth_state = status.state.clone();
            s.music_auth_checked_at = status.checked_at.clone();
            s.music_account_name = status.account_name.clone();
            Ok(())
        });
    }
}

fn search_filter_params(filter: &str) -> Option<&'static str> {
    match filter.trim().to_ascii_lowercase().as_str() {
        "songs" | "song" => Some("EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D"),
        "videos" | "video" => Some("EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D"),
        "albums" | "album" => Some("EgWKAQIYAWoKEAkQChAFEAMQBA%3D%3D"),
        "artists" | "artist" => Some("EgWKAQIgAWoKEAkQChAFEAMQBA%3D%3D"),
        "playlists" | "playlist" => Some("Eg-KAQwIABAAGAAgACgB"),
        _ => None,
    }
}

pub fn parse_music_next_items(data: &Value, limit: usize) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for renderer in walk_renderers(data, "playlistPanelVideoRenderer") {
        let Some(vid) = renderer.get("videoId").and_then(Value::as_str) else {
            continue;
        };
        if vid.is_empty() || !seen.insert(vid.to_string()) {
            continue;
        }
        let title = text_from_node(renderer.get("title")).unwrap_or_else(|| "Sin titulo".into());
        let subtitle = text_from_node(renderer.get("longBylineText"))
            .or_else(|| text_from_node(renderer.get("shortBylineText")))
            .unwrap_or_else(|| "YouTube Music".into());
        let channel = extract_music_artist_name(renderer, &subtitle);
        let duration_text_raw = text_from_node(renderer.get("lengthText"))
            .filter(|s| parse_duration_text(s).is_some())
            .or_else(|| extract_duration_from_subtitle(&subtitle));
        let duration = duration_text_raw.as_deref().and_then(parse_duration_text);
        let thumbnail = last_thumbnail_url(renderer.get("thumbnail"))
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{vid}/hqdefault.jpg"));

        items.push(MediaItem {
            id: vid.to_string(),
            url: format!("https://music.youtube.com/watch?v={vid}"),
            title,
            channel,
            channel_url: extract_music_channel_url(renderer),
            duration,
            duration_text: duration_text_raw.unwrap_or_else(|| "--:--".into()),
            thumbnail: Some(thumbnail),
            view_count: None,
            live: false,
            members_only: false,
            result_type: Some("song".into()),
            watched_percent: None,
        });
        if items.len() >= limit.max(1) {
            break;
        }
    }

    items
}

pub fn parse_music_items(data: &Value, limit: usize) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    let mut renderers = walk_renderers(data, "musicResponsiveListItemRenderer");
    renderers.extend(walk_renderers(data, "musicTwoRowItemRenderer"));

    for renderer in renderers {
        let Some(video_id) = first_string_field(renderer, "videoId") else {
            continue;
        };
        if video_id.is_empty() || !seen.insert(video_id.clone()) {
            continue;
        }

        let title = flex_column_text(renderer, 0)
            .or_else(|| text_from_node(renderer.get("title")))
            .unwrap_or_else(|| "Sin titulo".into());
        let subtitle = flex_column_text(renderer, 1)
            .or_else(|| text_from_node(renderer.get("subtitle")))
            .unwrap_or_else(|| "YouTube Music".into());
        let channel = extract_music_artist_name(renderer, &subtitle);

        let duration_text_raw = fixed_column_text(renderer, 0)
            .filter(|s| parse_duration_text(s).is_some())
            .or_else(|| fixed_column_text(renderer, 1).filter(|s| parse_duration_text(s).is_some()))
            .or_else(|| flex_column_text(renderer, 2).and_then(|s| extract_duration_from_subtitle(&s)))
            .or_else(|| extract_duration_from_subtitle(&subtitle))
            .or_else(|| extract_duration_from_renderer_fields(renderer));
        let duration = duration_text_raw.as_deref().and_then(parse_duration_text);
        let thumbnail = first_nested_thumbnail(renderer)
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"));

        items.push(MediaItem {
            id: video_id.clone(),
            url: format!("https://music.youtube.com/watch?v={video_id}"),
            title,
            channel,
            channel_url: extract_music_channel_url(renderer),
            duration,
            duration_text: duration_text_raw.unwrap_or_else(|| "--:--".into()),
            thumbnail: Some(thumbnail),
            view_count: None,
            live: false,
            members_only: false,
            result_type: Some("song".into()),
            watched_percent: None,
        });

        if items.len() >= limit {
            break;
        }
    }

    items
}

pub fn parse_music_playlists(data: &Value, limit: usize) -> Vec<MusicPlaylistSummary> {
    let mut playlists = Vec::new();
    let mut seen = HashSet::new();

    for renderer in walk_renderers(data, "musicTwoRowItemRenderer") {
        let Some(playlist_id) = first_string_field(renderer, "playlistId") else {
            continue;
        };
        if playlist_id.is_empty() || !seen.insert(playlist_id.clone()) {
            continue;
        }
        let title = text_from_node(renderer.get("title")).unwrap_or_else(|| "Playlist".into());
        let subtitle = text_from_node(renderer.get("subtitle")).unwrap_or_default();
        let thumbnail = first_nested_thumbnail(renderer).unwrap_or_default();

        playlists.push(MusicPlaylistSummary {
            url: format!("https://music.youtube.com/playlist?list={playlist_id}"),
            id: playlist_id,
            title,
            subtitle,
            thumbnail,
        });
        if playlists.len() >= limit {
            break;
        }
    }

    playlists
}

fn flex_column_text(renderer: &Value, index: usize) -> Option<String> {
    let cols = renderer.get("flexColumns")?.as_array()?;
    let col = cols.get(index)?;
    let text_node = col.pointer("/musicResponsiveListItemFlexColumnRenderer/text")?;
    text_from_node(Some(text_node))
}

fn fixed_column_text(renderer: &Value, index: usize) -> Option<String> {
    let cols = renderer.get("fixedColumns")?.as_array()?;
    let col = cols.get(index)?;
    let text_node = col.pointer("/musicResponsiveListItemFixedColumnRenderer/text")?;
    text_from_node(Some(text_node))
}

fn extract_duration_from_subtitle(subtitle: &str) -> Option<String> {
    subtitle
        .split(['•', '·'])
        .map(str::trim)
        .rev()
        .find(|part| parse_duration_text(part).is_some())
        .map(ToString::to_string)
}

fn extract_duration_from_renderer_fields(renderer: &Value) -> Option<String> {
    if let Some(txt) = text_from_node(renderer.get("lengthText")) {
        if parse_duration_text(&txt).is_some() {
            return Some(txt);
        }
    }
    if let Some(overlays) = renderer.get("thumbnailOverlays").and_then(Value::as_array) {
        for ov in overlays {
            if let Some(txt) = text_from_node(ov.pointer("/thumbnailOverlayTimeStatusRenderer/text"))
            {
                if parse_duration_text(&txt).is_some() {
                    return Some(txt);
                }
            }
        }
    }
    None
}

fn extract_music_artist_name(renderer: &Value, subtitle: &str) -> String {
    // 1. Prefer runs that link directly to a UC... channel or artist page.
    let runs_node = renderer
        .pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .or_else(|| renderer.pointer("/subtitle/runs"));
    if let Some(runs) = runs_node.and_then(Value::as_array) {
        let mut artists = Vec::new();
        for run in runs {
            let browse_id = run
                .pointer("/navigationEndpoint/browseEndpoint/browseId")
                .and_then(Value::as_str)
                .unwrap_or("");
            let page_type = run
                .pointer("/navigationEndpoint/browseEndpoint/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType")
                .and_then(Value::as_str)
                .unwrap_or("");
            let is_artist = browse_id.starts_with("UC")
                || page_type == "MUSIC_PAGE_TYPE_ARTIST"
                || page_type == "MUSIC_PAGE_TYPE_USER_CHANNEL";
            if is_artist {
                if let Some(text) = run.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() && !artists.iter().any(|a| a == trimmed) {
                        artists.push(trimmed.to_string());
                    }
                }
            }
        }
        if !artists.is_empty() {
            return artists.join(", ");
        }
    }
    // 2. Fallback: strip media-type prefixes, view counts, durations, and years from subtitle.
    clean_music_channel_text(subtitle)
}

pub fn clean_music_channel_text(subtitle: &str) -> String {
    let parts: Vec<&str> = subtitle
        .split(['•', '·'])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() <= 1 {
        return subtitle.trim().to_string();
    }
    let filtered: Vec<&str> = parts
        .iter()
        .copied()
        .filter(|part| !is_non_channel_subtitle_part(part))
        .collect();
    if let Some(first) = filtered.first() {
        (*first).to_string()
    } else {
        subtitle.trim().to_string()
    }
}

fn is_non_channel_subtitle_part(part: &str) -> bool {
    let lower = part.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    if matches!(
        lower.as_str(),
        "canción"
            | "cancion"
            | "song"
            | "video"
            | "vídeo"
            | "episodio"
            | "episode"
            | "podcast"
            | "álbum"
            | "album"
            | "sencillo"
            | "single"
            | "ep"
    ) {
        return true;
    }
    if parse_duration_text(part).is_some() {
        return true;
    }
    if lower.contains("visualizaciones")
        || lower.contains("visualización")
        || lower.contains("vistas")
        || lower.contains("views")
        || lower.contains("reproducciones")
        || lower.contains("plays")
        || lower.contains("listeners")
        || lower.contains("oyentes")
    {
        return true;
    }
    if part.len() == 4 && part.chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    false
}

fn extract_music_continuation(data: &Value) -> Option<String> {
    first_string_field(data, "continuation").or_else(|| {
        for node in walk_renderers(data, "continuationCommand") {
            if let Some(tok) = node.get("token").and_then(Value::as_str) {
                if !tok.is_empty() {
                    return Some(tok.to_string());
                }
            }
        }
        None
    })
}

/// Parses playlist tracks from a YouTube Music browse page, preferring the dedicated
/// playlist shelf container so unrelated recommendation carousels at the bottom are skipped.
fn parse_playlist_page_items(data: &Value, is_first_page: bool) -> Vec<MediaItem> {
    let containers: Vec<&Value> = if is_first_page {
        walk_renderers(data, "musicPlaylistShelfRenderer")
    } else {
        let mut c = walk_renderers(data, "musicPlaylistShelfContinuation");
        c.extend(walk_renderers(data, "appendContinuationItemsAction"));
        c
    };

    if !containers.is_empty() {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for container in containers {
            for item in parse_music_items(container, usize::MAX) {
                if seen.insert(item.id.clone()) {
                    out.push(item);
                }
            }
        }
        if !out.is_empty() {
            return out;
        }
    }

    parse_music_items(data, usize::MAX)
}

/// Extracts the continuation token for a YouTube Music playlist page, scoping to the
/// playlist shelf/continuation container first so unrelated shelves are not followed.
fn extract_playlist_continuation(data: &Value, is_first_page: bool) -> Option<String> {
    let containers: Vec<&Value> = if is_first_page {
        walk_renderers(data, "musicPlaylistShelfRenderer")
    } else {
        let mut c = walk_renderers(data, "musicPlaylistShelfContinuation");
        c.extend(walk_renderers(data, "appendContinuationItemsAction"));
        c
    };

    for container in &containers {
        if let Some(tok) = extract_music_continuation(container) {
            return Some(tok);
        }
    }

    if containers.is_empty() {
        extract_music_continuation(data)
    } else {
        None
    }
}

fn extract_music_channel_url(node: &Value) -> Option<String> {
    match node {
        Value::Object(map) => {
            if let Some(bid) = map
                .get("browseEndpoint")
                .and_then(|b| b.get("browseId"))
                .and_then(Value::as_str)
            {
                if bid.starts_with("UC") && bid.len() >= 20 {
                    return Some(format!("https://www.youtube.com/channel/{bid}"));
                }
            }
            for v in map.values() {
                if let Some(found) = extract_music_channel_url(v) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(found) = extract_music_channel_url(item) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn first_string_field(node: &Value, key: &str) -> Option<String> {
    match node {
        Value::Object(map) => {
            if let Some(val) = map.get(key).and_then(Value::as_str) {
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
            for v in map.values() {
                if let Some(found) = first_string_field(v, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(found) = first_string_field(v, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn first_nested_thumbnail(node: &Value) -> Option<String> {
    if let Some(url) = last_thumbnail_url(Some(node)) {
        return Some(url);
    }
    match node {
        Value::Object(map) => {
            if let Some(t) = map.get("thumbnails") {
                if let Some(url) = last_thumbnail_url(Some(t)) {
                    return Some(url);
                }
            }
            for v in map.values() {
                if let Some(url) = first_nested_thumbnail(v) {
                    return Some(url);
                }
            }
            None
        }
        Value::Array(arr) => {
            for v in arr {
                if let Some(url) = first_nested_thumbnail(v) {
                    return Some(url);
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn playlist_page_parsing_scopes_to_playlist_shelf_and_ignores_cap() {
        let mut shelf_contents = Vec::new();
        for i in 0..120 {
            shelf_contents.push(json!({
                "musicResponsiveListItemRenderer": {
                    "playlistItemData": { "videoId": format!("vid{i:08}") },
                    "flexColumns": [
                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": format!("Track {i}") }] } } },
                        { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [{ "text": "Artist" }] } } }
                    ]
                }
            }));
        }
        shelf_contents.push(json!({
            "continuationItemRenderer": {
                "continuationEndpoint": {
                    "continuationCommand": { "token": "PAGE_2_TOKEN" }
                }
            }
        }));

        let data = json!({
            "contents": {
                "twoColumnBrowseResultsRenderer": {
                    "secondaryContents": {
                        "sectionListRenderer": {
                            "contents": [
                                {
                                    "musicPlaylistShelfRenderer": {
                                        "contents": shelf_contents
                                    }
                                },
                                {
                                    "musicCarouselShelfRenderer": {
                                        "contents": [
                                            {
                                                "musicTwoRowItemRenderer": {
                                                    "navigationEndpoint": { "watchEndpoint": { "videoId": "unrelated999" } },
                                                    "title": { "runs": [{ "text": "Suggested Track" }] }
                                                }
                                            }
                                        ]
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let items = parse_playlist_page_items(&data, true);
        assert_eq!(items.len(), 120);
        assert!(!items.iter().any(|i| i.id == "unrelated999"));
        assert_eq!(
            extract_playlist_continuation(&data, true).as_deref(),
            Some("PAGE_2_TOKEN")
        );
    }
}
