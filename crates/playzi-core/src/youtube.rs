use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Local;
use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::cookies::CookieJar;
use crate::error::{CoreError, Result};
use crate::state::StateStore;

pub const DEFAULT_YT_BASE_URL: &str = "https://www.youtube.com";
const DEFAULT_WEB_CLIENT_VERSION: &str = "2.20260401.00.00";
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36";

const SEARCH_CACHE_TTL: Duration = Duration::from_secs(300);
const SEARCH_CACHE_MAX: usize = 50;
const BROWSE_CACHE_TTL: Duration = Duration::from_secs(600);
const BROWSE_CACHE_MAX: usize = 80;

pub const SUPPORTED_CHANNEL_TABS: &[&str] = &["videos", "shorts", "streams", "playlists", "releases"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MediaItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub channel_url: Option<String>,
    pub duration: Option<u64>,
    pub duration_text: String,
    pub thumbnail: Option<String>,
    pub view_count: Option<u64>,
    pub live: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub members_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watched_percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelBrowseResult {
    pub name: String,
    pub url: String,
    pub channel_id: String,
    pub handle: String,
    pub avatar: String,
    pub subscriber_count: Option<u64>,
    pub tab: String,
    pub page: usize,
    pub page_size: usize,
    pub has_more: bool,
    pub items: Vec<MediaItem>,
    pub tab_error: String,
    pub already_followed: bool,
    pub followed_channel_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubscribedChannel {
    pub channel_id: String,
    pub name: String,
    pub url: String,
    pub handle: String,
    pub thumbnail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscriber_count: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionSyncResult {
    pub authenticated: bool,
    pub synced_count: usize,
    pub added_count: usize,
    pub updated_count: usize,
    pub removed_count: usize,
    pub total_channels: usize,
    pub feed_count: usize,
    pub last_sync: String,
}

#[derive(Debug, Clone)]
struct CacheEntry<T> {
    inserted_at: Instant,
    value: T,
}

#[derive(Debug)]
struct TtlCache<T> {
    ttl: Duration,
    max_entries: usize,
    entries: Mutex<HashMap<String, CacheEntry<T>>>,
}

impl<T: Clone> TtlCache<T> {
    fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries,
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, key: &str) -> Option<T> {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let entry = guard.get(key)?;
        if entry.inserted_at.elapsed() > self.ttl {
            guard.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    fn set(&self, key: String, value: T) {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if guard.len() >= self.max_entries {
            if let Some(oldest_key) = guard
                .iter()
                .min_by_key(|(_, e)| e.inserted_at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest_key);
            }
        }
        guard.insert(
            key,
            CacheEntry {
                inserted_at: Instant::now(),
                value,
            },
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VideoChapter {
    pub title: String,
    pub start_time: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CaptionTrack {
    pub base_url: String,
    pub language_code: String,
    pub name: String,
    pub is_auto: bool,
    pub is_translatable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranslationLanguage {
    pub language_code: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardSpec {
    pub template_url: String,
    pub frame_width: u32,
    pub frame_height: u32,
    pub frame_count: u32,
    pub cols: u32,
    pub rows: u32,
    pub interval_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommentSortOption {
    pub label: String,
    pub continuation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDetails {
    pub video_id: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publish_date_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_date_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view_count_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub like_count_text: Option<String>,
    pub chapters: Vec<VideoChapter>,
    pub caption_tracks: Vec<CaptionTrack>,
    pub translation_languages: Vec<TranslationLanguage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storyboard: Option<StoryboardSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_continuation: Option<String>,
    pub comments_sort_options: Vec<CommentSortOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VideoComment {
    pub id: String,
    pub author: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_channel_url: Option<String>,
    pub is_channel_owner: bool,
    pub is_pinned: bool,
    pub is_hearted: bool,
    pub published_time_text: String,
    pub text: String,
    pub like_count_text: String,
    pub reply_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replies_continuation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommentsPage {
    pub comments: Vec<VideoComment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_continuation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_count_text: Option<String>,
}

#[derive(Debug, Clone)]
struct PlaybackTrackingUrls {
    playback_url: Option<String>,
    watchtime_url: Option<String>,
    length_seconds: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct YouTubeClient {
    http: reqwest::Client,
    base_url: String,
    hl: String,
    gl: String,
    search_cache: Arc<TtlCache<Vec<MediaItem>>>,
    metadata_cache: Arc<TtlCache<MediaItem>>,
    browse_cache: Arc<TtlCache<ChannelBrowseResult>>,
    tracking_cache: Arc<TtlCache<PlaybackTrackingUrls>>,
    details_cache: Arc<TtlCache<VideoDetails>>,
}

impl YouTubeClient {
    pub fn new() -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(DEFAULT_USER_AGENT));
        headers.insert(ORIGIN, HeaderValue::from_static(DEFAULT_YT_BASE_URL));
        headers.insert(REFERER, HeaderValue::from_static("https://www.youtube.com/"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .pool_max_idle_per_host(10)
            .build()?;

        Ok(Self::with_client(http, DEFAULT_YT_BASE_URL))
    }

    pub fn with_client(http: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            hl: "es".into(),
            gl: "EC".into(),
            search_cache: Arc::new(TtlCache::new(SEARCH_CACHE_TTL, SEARCH_CACHE_MAX)),
            metadata_cache: Arc::new(TtlCache::new(SEARCH_CACHE_TTL, SEARCH_CACHE_MAX)),
            browse_cache: Arc::new(TtlCache::new(BROWSE_CACHE_TTL, BROWSE_CACHE_MAX)),
            tracking_cache: Arc::new(TtlCache::new(Duration::from_secs(600), 128)),
            details_cache: Arc::new(TtlCache::new(BROWSE_CACHE_TTL, 64)),
        }
    }

    fn web_context(&self) -> Value {
        json!({
            "client": {
                "clientName": "WEB",
                "clientVersion": DEFAULT_WEB_CLIENT_VERSION,
                "hl": self.hl,
                "gl": self.gl,
            }
        })
    }

    async fn post_innertube(&self, endpoint: &str, body: Value) -> Result<Value> {
        self.post_innertube_with_cookies_and_user(endpoint, body, None, 0)
            .await
    }

    async fn post_innertube_with_cookies(
        &self,
        endpoint: &str,
        body: Value,
        cookies: Option<&CookieJar>,
    ) -> Result<Value> {
        self.post_innertube_with_cookies_and_user(endpoint, body, cookies, 0)
            .await
    }

    async fn post_innertube_with_cookies_and_user(
        &self,
        endpoint: &str,
        mut body: Value,
        cookies: Option<&CookieJar>,
        auth_user: u8,
    ) -> Result<Value> {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("context".into(), self.web_context());
        }
        let url = format!("{}/youtubei/v1/{}?prettyPrint=false", self.base_url, endpoint);
        let mut req = self.http.post(url).json(&body);
        if let Some(jar) = cookies {
            if let Some(headers) = jar.ytmusic_auth_headers_for_user(&self.base_url, auth_user) {
                for (k, v) in headers {
                    req = req.header(k, v);
                }
            } else {
                let raw_cookie = jar.cookie_header_for_host("www.youtube.com");
                if !raw_cookie.is_empty() {
                    req = req.header("Cookie", raw_cookie);
                }
            }
        }
        let resp = req.send().await?.error_for_status()?;
        Ok(resp.json::<Value>().await?)
    }

    /// Searches YouTube via InnerTube `/youtubei/v1/search` (or fetches metadata if `query` is a YouTube URL).
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<MediaItem>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 50);
        let cache_key = format!("{query}|{limit}");
        if let Some(cached) = self.search_cache.get(&cache_key) {
            return Ok(cached);
        }

        let items = if is_youtube_url(query) {
            vec![self.metadata_for_url(query).await?]
        } else {
            let data = self
                .post_innertube("search", json!({ "query": query }))
                .await?;
            parse_search_items(&data, limit)
        };

        self.search_cache.set(cache_key, items.clone());
        Ok(items)
    }

    /// Fetches video metadata for a YouTube URL or 11-char video ID via `/youtubei/v1/player`.
    pub async fn metadata_for_url(&self, url: &str) -> Result<MediaItem> {
        let trimmed = url.trim();
        if let Some(cached) = self.metadata_cache.get(trimmed) {
            return Ok(cached);
        }
        let video_id = extract_video_id(trimmed)
            .ok_or_else(|| CoreError::Validation(format!("Invalid YouTube video URL: {trimmed}")))?;

        let data = self
            .post_innertube("player", json!({ "videoId": video_id }))
            .await?;

        let details = data.get("videoDetails").ok_or_else(|| {
            CoreError::External(format!("Missing videoDetails for video {video_id}"))
        })?;

        let id = details
            .get("videoId")
            .and_then(Value::as_str)
            .unwrap_or(&video_id)
            .to_string();
        let title = details
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("Sin titulo")
            .to_string();
        let channel = details
            .get("author")
            .and_then(Value::as_str)
            .unwrap_or("YouTube")
            .to_string();
        let channel_id = details
            .get("channelId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let duration = details
            .get("lengthSeconds")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()));
        let view_count = details
            .get("viewCount")
            .and_then(|v| v.as_str().and_then(|s| s.parse::<u64>().ok()).or_else(|| v.as_u64()));
        let live = details
            .get("isLiveContent")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let thumbnail = last_thumbnail_url(details.get("thumbnail"))
            .or_else(|| Some(default_thumbnail(&id)));

        let is_short = trimmed.to_ascii_lowercase().contains("/shorts/");
        let item = MediaItem {
            url: if is_short {
                format!("https://www.youtube.com/shorts/{id}")
            } else {
                format!("https://www.youtube.com/watch?v={id}")
            },
            id,
            title,
            channel,
            channel_url: channel_id.map(|cid| format!("https://www.youtube.com/channel/{cid}")),
            duration,
            duration_text: if live {
                "EN VIVO".into()
            } else if is_short && duration.is_none() {
                "Short".into()
            } else {
                format_duration(duration)
            },
            thumbnail,
            view_count,
            live,
            members_only: is_members_only_playability(&data),
            result_type: if is_short { Some("short".into()) } else { None },
            watched_percent: None,
        };

        self.metadata_cache.set(trimmed.to_string(), item.clone());
        Ok(item)
    }

    /// Fetches YouTube home recommendations (`FEwhat_to_watch`) via InnerTube `/youtubei/v1/browse`.
    pub async fn fetch_recommendations(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_with_cookies_and_user(None, 0, store)
            .await
    }

    pub async fn fetch_recommendations_with_cookies(
        &self,
        cookies: Option<&CookieJar>,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_with_cookies_and_user(cookies, 0, store)
            .await
    }

    pub async fn fetch_recommendations_with_cookies_and_user(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_pages(cookies, auth_user, store, 2, false)
            .await
    }

    /// Fetches additional YouTube home recommendations via InnerTube continuation tokens
    /// and merges them into the persisted recommendation list.
    pub async fn fetch_recommendations_more_with_cookies_and_user(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_pages(cookies, auth_user, store, 3, true)
            .await
    }

    async fn fetch_recommendations_pages(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
        store: Option<&StateStore>,
        max_pages: usize,
        merge_existing: bool,
    ) -> Result<Vec<MediaItem>> {
        let mut data = self
            .post_innertube_with_cookies_and_user(
                "browse",
                json!({ "browseId": "FEwhat_to_watch" }),
                cookies,
                auth_user,
            )
            .await?;

        let mut mixes = parse_recommended_mixes(&data);
        let mut items = parse_channel_videos(&data, "", "");
        let mut seen: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
        for item in parse_lockup_videos(&data) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }

        let mut page = 1usize;
        while page < max_pages {
            let Some(token) = extract_yt_continuation(&data) else {
                break;
            };
            let Ok(next_data) = self
                .post_innertube_with_cookies_and_user(
                    "browse",
                    json!({ "continuation": token }),
                    cookies,
                    auth_user,
                )
                .await
            else {
                break;
            };
            mixes.extend(parse_recommended_mixes(&next_data));
            for item in parse_channel_videos(&next_data, "", "") {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
            for item in parse_lockup_videos(&next_data) {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
            data = next_data;
            page += 1;
        }

        // Filter out already-watched videos (from local history or YouTube's watched overlay)
        // so recommendations keep surfacing fresh content instead of re-suggesting watched videos.
        let watched_ids: HashSet<String> = store
            .and_then(|st| st.read().ok())
            .map(|s| {
                s.history
                    .iter()
                    .take(250)
                    .filter_map(|h| {
                        h.get("url")
                            .or_else(|| h.get("target"))
                            .and_then(Value::as_str)
                            .and_then(extract_video_id)
                    })
                    .collect()
            })
            .unwrap_or_default();

        let is_watched = |item: &MediaItem| -> bool {
            watched_ids.contains(&item.id) || item.watched_percent.unwrap_or(0) >= 80
        };

        let fresh: Vec<MediaItem> = items.iter().filter(|item| !is_watched(item)).cloned().collect();
        if !fresh.is_empty() {
            items = fresh;
        }

        let mut combined: Vec<MediaItem> = Vec::new();
        let mut combined_ids: HashSet<String> = HashSet::new();

        if merge_existing {
            if let Some(st) = store {
                if let Ok(guard) = st.read() {
                    for val in &guard.recommendations {
                        if let Ok(existing) = serde_json::from_value::<MediaItem>(val.clone()) {
                            let is_mix = existing.result_type.as_deref().is_some_and(|t| t == "mix" || t == "playlist");
                            if !is_mix && is_watched(&existing) {
                                continue;
                            }
                            if combined_ids.insert(existing.id.clone()) {
                                combined.push(existing);
                            }
                        }
                    }
                }
            }
        }

        for mix in mixes.into_iter().take(20) {
            if combined_ids.insert(mix.id.clone()) {
                combined.push(mix);
            }
        }
        let video_cap = if merge_existing { 120 } else { 60 };
        for item in items.into_iter().take(video_cap) {
            if combined_ids.insert(item.id.clone()) {
                combined.push(item);
            }
        }

        if let Some(st) = store {
            if !combined.is_empty() {
                let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let serialized: Vec<Value> = combined
                    .iter()
                    .filter_map(|i| serde_json::to_value(i).ok())
                    .collect();
                st.update(|s| {
                    s.recommendations = serialized;
                    s.recommendations_last_sync = now;
                    Ok(())
                })?;
            }
        }

        Ok(combined)
    }

    /// Reports video playback and watchtime directly to YouTube's official playback tracking endpoints
    /// (`/api/stats/playback` and `/api/stats/watchtime`) using authenticated cookies so YouTube's
    /// Watch History and recommendation algorithm stay updated when watching in Playzi.
    pub async fn report_watch_event(
        &self,
        video_id: &str,
        event_type: &str,
        current_time: f64,
        start_time: Option<f64>,
        duration: Option<f64>,
        cpn: &str,
        cookies: Option<&CookieJar>,
        auth_user: u8,
    ) -> Result<bool> {
        let Some(jar) = cookies else {
            return Ok(false);
        };
        let Some(vid) = extract_video_id(video_id) else {
            return Ok(false);
        };
        let cookie_header = jar.cookie_header_for_host("www.youtube.com");
        if cookie_header.is_empty() {
            return Ok(false);
        }

        let tracking = if let Some(cached) = self.tracking_cache.get(&vid) {
            cached
        } else {
            let player = self
                .post_innertube_with_cookies_and_user(
                    "player",
                    json!({ "videoId": vid }),
                    Some(jar),
                    auth_user,
                )
                .await?;
            let playback_url = player
                .pointer("/playbackTracking/videostatsPlaybackUrl/baseUrl")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let watchtime_url = player
                .pointer("/playbackTracking/videostatsWatchtimeUrl/baseUrl")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            let length_seconds = player
                .pointer("/videoDetails/lengthSeconds")
                .and_then(|v| {
                    v.as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                        .or_else(|| v.as_f64())
                });
            let info = PlaybackTrackingUrls {
                playback_url,
                watchtime_url,
                length_seconds,
            };
            self.tracking_cache.set(vid.clone(), info.clone());
            info
        };

        let cpn_clean = if cpn.trim().len() >= 8 {
            cpn.trim().to_string()
        } else {
            format!("playzi{:010x}", chrono::Utc::now().timestamp_millis() & 0xffff_ffff_ff)
        };
        let len_secs = duration.or(tracking.length_seconds).unwrap_or(0.0).max(0.0);
        let cmt = current_time.max(0.0);
        let referer = format!("https://www.youtube.com/watch?v={vid}");

        let mut reported = false;

        if event_type == "start" {
            if let Some(base_url) = tracking.playback_url.as_deref() {
                if let Ok(mut parsed) = reqwest::Url::parse(base_url) {
                    let existing_keys: HashSet<String> = parsed
                        .query_pairs()
                        .map(|(k, _)| k.into_owned())
                        .collect();
                    {
                        let mut qp = parsed.query_pairs_mut();
                        qp.append_pair("cpn", &cpn_clean);
                        if !existing_keys.contains("ver") {
                            qp.append_pair("ver", "2");
                        }
                        qp.append_pair("fmt", "251");
                        qp.append_pair("fs", "0");
                        qp.append_pair("rt", "0.5");
                        qp.append_pair("euri", "");
                        if !existing_keys.contains("el") {
                            qp.append_pair("el", "detailpage");
                        }
                        qp.append_pair("c", "WEB");
                        qp.append_pair("cver", DEFAULT_WEB_CLIENT_VERSION);
                        qp.append_pair("hl", &self.hl);
                        qp.append_pair("gl", &self.gl);
                    }
                    let mut req = self
                        .http
                        .get(parsed)
                        .header("Cookie", &cookie_header)
                        .header("Referer", &referer)
                        .header("Origin", "https://www.youtube.com")
                        .header("X-Goog-AuthUser", auth_user.to_string());
                    if let Some(auth) = jar.authorization_header("https://www.youtube.com", chrono::Utc::now().timestamp()) {
                        req = req.header("Authorization", auth);
                    }
                    if let Ok(resp) = req.send().await {
                        if resp.status().is_success() {
                            reported = true;
                        }
                    }
                }
            }
        }

        if let Some(base_url) = tracking.watchtime_url.as_deref() {
            if let Ok(mut parsed) = reqwest::Url::parse(base_url) {
                let state_str = match event_type {
                    "ended" | "pause" => "paused",
                    _ => "playing",
                };
                let et_val = if event_type == "start" {
                    cmt.max(0.5)
                } else {
                    cmt.max(1.0)
                };
                let st_val = if event_type == "start" {
                    0.0
                } else if let Some(st) = start_time {
                    st.clamp(0.0, et_val)
                } else {
                    (et_val - 15.0).max(0.0)
                };
                let existing_keys: HashSet<String> = parsed
                    .query_pairs()
                    .map(|(k, _)| k.into_owned())
                    .collect();
                {
                    let mut qp = parsed.query_pairs_mut();
                    qp.append_pair("cpn", &cpn_clean);
                    if !existing_keys.contains("ver") {
                        qp.append_pair("ver", "2");
                    }
                    qp.append_pair("fmt", "251");
                    qp.append_pair("fs", "0");
                    qp.append_pair("rt", &format!("{:.1}", et_val.max(0.5)));
                    qp.append_pair("cmt", &format!("{:.1}", et_val));
                    qp.append_pair("st", &format!("{:.1}", st_val));
                    qp.append_pair("et", &format!("{:.1}", et_val));
                    if len_secs > 0.0 {
                        qp.append_pair("len", &format!("{:.1}", len_secs));
                    }
                    qp.append_pair("state", state_str);
                    qp.append_pair("final", if event_type == "ended" || event_type == "pause" { "1" } else { "0" });
                    if !existing_keys.contains("el") {
                        qp.append_pair("el", "detailpage");
                    }
                    qp.append_pair("c", "WEB");
                    qp.append_pair("cver", DEFAULT_WEB_CLIENT_VERSION);
                    qp.append_pair("hl", &self.hl);
                    qp.append_pair("gl", &self.gl);
                }
                let mut req = self
                    .http
                    .get(parsed)
                    .header("Cookie", &cookie_header)
                    .header("Referer", &referer)
                    .header("Origin", "https://www.youtube.com")
                    .header("X-Goog-AuthUser", auth_user.to_string());
                if let Some(auth) = jar.authorization_header("https://www.youtube.com", chrono::Utc::now().timestamp()) {
                    req = req.header("Authorization", auth);
                }
                if let Ok(resp) = req.send().await {
                    if resp.status().is_success() {
                        reported = true;
                    }
                }
            }
        }

        Ok(reported)
    }

    /// Fetches YouTube Radio Mix (`RD<videoId>`) items via `/youtubei/v1/next`, skipping the seed video.
    pub async fn fetch_radio_items(&self, url: &str, limit: usize) -> Result<Vec<MediaItem>> {
        let Some(video_id) = extract_video_id(url) else {
            return Ok(Vec::new());
        };
        let playlist_id = format!("RD{video_id}");
        self.fetch_next_playlist_items(Some(&video_id), &playlist_id, limit, true, None)
            .await
    }

    /// Fetches items for a YouTube Mix / Radio playlist URL (`list=RD...`, `RDMM`, etc.)
    /// via InnerTube `/youtubei/v1/next` using optional cookies, keeping the seed track.
    pub async fn fetch_mix_items_with_cookies(
        &self,
        url: &str,
        limit: usize,
        cookies: Option<&CookieJar>,
    ) -> Result<Vec<MediaItem>> {
        let Some(playlist_id) = extract_playlist_id_from_url(url) else {
            return Ok(Vec::new());
        };
        let video_id = extract_video_id(url)
            .or_else(|| extract_seed_video_from_playlist_id(&playlist_id));
        self.fetch_next_playlist_items(video_id.as_deref(), &playlist_id, limit, false, cookies)
            .await
    }

    async fn fetch_next_playlist_items(
        &self,
        video_id: Option<&str>,
        playlist_id: &str,
        limit: usize,
        skip_seed: bool,
        cookies: Option<&CookieJar>,
    ) -> Result<Vec<MediaItem>> {
        let mut payload = json!({ "playlistId": playlist_id });
        if let Some(vid) = video_id.filter(|v| !v.is_empty()) {
            payload["videoId"] = Value::String(vid.to_string());
        }
        let data = self
            .post_innertube_with_cookies("next", payload, cookies)
            .await?;

        let mut items = Vec::new();
        let mut seen = HashSet::new();
        if skip_seed {
            if let Some(vid) = video_id {
                seen.insert(vid.to_string());
            }
        }

        for renderer in walk_renderers(&data, "playlistPanelVideoRenderer") {
            let Some(vid) = renderer.get("videoId").and_then(Value::as_str) else {
                continue;
            };
            if vid.is_empty() || !seen.insert(vid.to_string()) {
                continue;
            }
            if is_ad_renderer(renderer) {
                continue;
            }
            let title = text_from_node(renderer.get("title")).unwrap_or_else(|| "Sin titulo".into());
            let channel = text_from_node(renderer.get("shortBylineText"))
                .or_else(|| text_from_node(renderer.get("longBylineText")))
                .unwrap_or_else(|| "YouTube".into());
            let (duration, duration_text_raw, live) = extract_duration_and_live(renderer);
            let channel_url = extract_browse_channel_url(renderer);
            let thumbnail = last_thumbnail_url(renderer.get("thumbnail"))
                .or_else(|| Some(default_thumbnail(vid)));

            items.push(MediaItem {
                id: vid.to_string(),
                url: format!("https://www.youtube.com/watch?v={vid}"),
                title,
                channel,
                channel_url,
                duration,
                duration_text: duration_text_raw.unwrap_or_else(|| {
                    if live {
                        "EN VIVO".into()
                    } else {
                        format_duration(duration)
                    }
                }),
                thumbnail,
                view_count: None,
                live,
                members_only: is_members_only_renderer(renderer),
                result_type: None,
                watched_percent: extract_watched_percent(renderer),
            });
            if items.len() >= limit.max(1) {
                break;
            }
        }

        Ok(items)
    }

    /// Resolves any channel handle/URL to its `UC...` browse ID via `/youtubei/v1/navigation/resolve_url`.
    pub async fn resolve_channel_id(&self, url: &str) -> Result<String> {
        let base = resolve_channel_base(url)?;
        if let Some(id) = extract_uc_id(&base) {
            return Ok(id);
        }
        let data = self
            .post_innertube("navigation/resolve_url", json!({ "url": base }))
            .await?;
        let browse_id = data
            .pointer("/endpoint/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .filter(|s| s.starts_with("UC"))
            .map(ToString::to_string)
            .ok_or_else(|| {
                CoreError::NotFound(format!("Could not resolve YouTube channel ID for {base}"))
            })?;
        Ok(browse_id)
    }

    /// Browses a YouTube channel tab (`videos`, `shorts`, `streams`, `playlists`, `releases`).
    pub async fn browse_channel(
        &self,
        url: &str,
        tab: &str,
        page: usize,
        page_size: usize,
        state: Option<&StateStore>,
    ) -> Result<ChannelBrowseResult> {
        let tab = if tab.trim().is_empty() {
            "videos".to_string()
        } else {
            tab.trim().to_ascii_lowercase()
        };
        if !SUPPORTED_CHANNEL_TABS.contains(&tab.as_str()) {
            return Err(CoreError::Validation(format!("Unsupported channel tab: {tab}")));
        }
        let page = page.max(1);
        let page_size = page_size.clamp(5, 60);
        let resolved_input = if extract_video_id(url).is_some() && !url.contains("/channel/") && !url.contains("/@") {
            let meta = self.metadata_for_url(url).await?;
            meta.channel_url.ok_or_else(|| {
                CoreError::NotFound("No se pudo identificar el canal de este video".into())
            })?
        } else {
            url.to_string()
        };
        let base = resolve_channel_base(&resolved_input)?;

        let cache_key = format!("{base}|{tab}|{page}|{page_size}");
        if let Some(mut cached) = self.browse_cache.get(&cache_key) {
            if let Some(store) = state {
                let followed = find_followed_channel(store, &base, &cached.url, &cached.handle);
                cached.already_followed = followed.is_some();
                cached.followed_channel_id = followed;
            }
            return Ok(cached);
        }

        let channel_id = self.resolve_channel_id(&base).await?;
        let mut payload = json!({ "browseId": channel_id });
        if let Some(params) = tab_params(&tab) {
            payload["params"] = Value::String(params.to_string());
        }

        let data = self.post_innertube("browse", payload).await?;
        let (name, handle, avatar, subscriber_count) = extract_channel_header(&data);

        let mut all_items = if tab == "playlists" {
            parse_channel_playlists(&data)
        } else {
            parse_channel_videos(&data, &name, &channel_id)
        };
        if tab == "shorts" {
            for item in &mut all_items {
                item.result_type = Some("short".into());
                if !item.id.is_empty() {
                    item.url = format!("https://www.youtube.com/shorts/{}", item.id);
                }
                if item.duration_text.is_empty() || item.duration_text == "--:--" {
                    item.duration_text = "Short".into();
                }
            }
        }

        let start = (page - 1) * page_size;
        let has_more = all_items.len() > start + page_size;
        let items: Vec<MediaItem> = all_items.into_iter().skip(start).take(page_size).collect();
        let tab_error = if items.is_empty() {
            "Este canal no tiene contenido en esta pestaña.".to_string()
        } else {
            String::new()
        };

        let canonical = format!("https://www.youtube.com/channel/{channel_id}");
        let followed = state
            .and_then(|s| find_followed_channel(s, &base, &canonical, &handle));

        let result = ChannelBrowseResult {
            name,
            url: canonical.clone(),
            channel_id,
            handle,
            avatar,
            subscriber_count,
            tab: tab.clone(),
            page,
            page_size,
            has_more,
            items,
            tab_error,
            already_followed: followed.is_some(),
            followed_channel_id: followed,
        };

        self.browse_cache.set(cache_key, result.clone());
        if canonical != base {
            self.browse_cache
                .set(format!("{canonical}|{tab}|{page}|{page_size}"), result.clone());
        }
        Ok(result)
    }

    /// Adds a channel to `StateStore` and immediately refreshes its feed.
    pub async fn add_channel(
        &self,
        store: &StateStore,
        url: &str,
        name: Option<&str>,
    ) -> Result<Value> {
        let normalized = channel_videos_url(url)?;
        let existing_state = store.read()?;
        if let Some(existing) = existing_state
            .channels
            .iter()
            .find(|c| c.get("url").and_then(Value::as_str) == Some(normalized.as_str()))
        {
            let existing_name = existing
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&normalized);
            return Err(CoreError::Validation(format!(
                "Canal ya agregado: {existing_name}"
            )));
        }

        let channel_id = Uuid::new_v4().to_string();
        let channel = json!({
            "id": channel_id,
            "name": name.filter(|s| !s.trim().is_empty()).unwrap_or(url),
            "url": normalized,
            "thumbnail": "",
            "lastSync": "",
            "error": "",
        });

        store.update(|s| {
            s.channels.insert(0, channel);
            Ok(())
        })?;

        self.refresh_channel(store, &channel_id).await
    }

    /// Refreshes a single followed channel's feed items in `StateStore`.
    pub async fn refresh_channel(&self, store: &StateStore, channel_id: &str) -> Result<Value> {
        let state = store.read()?;
        let channel = state
            .channels
            .iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(channel_id))
            .cloned()
            .ok_or_else(|| CoreError::NotFound("Canal no encontrado".into()))?;

        let channel_url = channel
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let fallback_name = channel
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("YouTube")
            .to_string();

        match self
            .browse_channel(&channel_url, "videos", 1, 20, Some(store))
            .await
        {
            Ok(browsed) => {
                let channel_name = if browsed.name.is_empty() {
                    fallback_name
                } else {
                    browsed.name
                };
                let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

                let mut feed_items = Vec::new();
                for item in browsed.items {
                    let mut obj = serde_json::to_value(&item)?;
                    if let Some(map) = obj.as_object_mut() {
                        let feed_id = format!(
                            "{channel_id}:{}",
                            if item.id.is_empty() {
                                &item.url
                            } else {
                                &item.id
                            }
                        );
                        map.insert("feedId".into(), Value::String(feed_id));
                        map.insert("sourceChannelId".into(), Value::String(channel_id.into()));
                        map.insert(
                            "sourceChannelName".into(),
                            Value::String(channel_name.clone()),
                        );
                        map.insert("seenAt".into(), Value::String(now.clone()));
                    }
                    feed_items.push(obj);
                }

                let avatar = browsed.avatar;
                store.update(|s| {
                    for saved in &mut s.channels {
                        if saved.get("id").and_then(Value::as_str) == Some(channel_id) {
                            if let Some(map) = saved.as_object_mut() {
                                map.insert("name".into(), Value::String(channel_name.clone()));
                                map.insert("lastSync".into(), Value::String(now.clone()));
                                map.insert("error".into(), Value::String(String::new()));
                                if !avatar.is_empty() {
                                    map.insert("thumbnail".into(), Value::String(avatar.clone()));
                                }
                            }
                            break;
                        }
                    }

                    let mut by_feed_id: HashMap<String, Value> = HashMap::new();
                    let mut order: Vec<String> = Vec::new();
                    for item in &s.feed {
                        if let Some(fid) = item.get("feedId").and_then(Value::as_str) {
                            if !by_feed_id.contains_key(fid) {
                                order.push(fid.to_string());
                            }
                            by_feed_id.insert(fid.to_string(), item.clone());
                        }
                    }
                    for item in feed_items {
                        if let Some(fid) = item.get("feedId").and_then(Value::as_str) {
                            if !by_feed_id.contains_key(fid) {
                                order.insert(0, fid.to_string());
                            }
                            by_feed_id.insert(fid.to_string(), item);
                        }
                    }
                    s.feed = order
                        .into_iter()
                        .filter_map(|fid| by_feed_id.remove(&fid))
                        .take(200)
                        .collect();
                    Ok(())
                })?;
            }
            Err(err) => {
                let err_msg = err.to_string();
                let _ = store.update(|s| {
                    for saved in &mut s.channels {
                        if saved.get("id").and_then(Value::as_str) == Some(channel_id) {
                            if let Some(map) = saved.as_object_mut() {
                                map.insert("error".into(), Value::String(err_msg.clone()));
                            }
                            break;
                        }
                    }
                    Ok(())
                });
                return Err(err);
            }
        }

        let updated = store.read()?;
        updated
            .channels
            .into_iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(channel_id))
            .ok_or_else(|| CoreError::NotFound("Canal no encontrado".into()))
    }

    /// Concurrently refreshes all followed channels (bounded to 4 concurrent requests).
    pub async fn refresh_all_channels(&self, store: &StateStore) -> Result<Vec<Value>> {
        let channels = list_channels(store)?;
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        let sem = Arc::new(Semaphore::new(4));
        let mut handles = Vec::with_capacity(channels.len());

        for ch in channels {
            let Some(cid) = ch.get("id").and_then(Value::as_str).map(ToString::to_string) else {
                continue;
            };
            let sem_clone = Arc::clone(&sem);
            let client = self.clone();
            let store_clone = store.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem_clone.acquire_owned().await.ok()?;
                match client.refresh_channel(&store_clone, &cid).await {
                    Ok(v) => Some(v),
                    Err(_) => store_clone
                        .read()
                        .ok()?
                        .channels
                        .into_iter()
                        .find(|c| c.get("id").and_then(Value::as_str) == Some(cid.as_str())),
                }
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            if let Ok(Some(val)) = handle.await {
                results.push(val);
            }
        }
        Ok(results)
    }

    /// Concurrently refreshes only manually-followed channels (not synced from YouTube subscriptions).
    pub async fn refresh_manual_channels(&self, store: &StateStore) -> Result<Vec<Value>> {
        let channels: Vec<Value> = list_channels(store)?
            .into_iter()
            .filter(|c| !c.get("syncedFromYoutube").and_then(Value::as_bool).unwrap_or(false))
            .take(30)
            .collect();
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        let sem = Arc::new(Semaphore::new(4));
        let mut handles = Vec::with_capacity(channels.len());

        for ch in channels {
            let Some(cid) = ch.get("id").and_then(Value::as_str).map(ToString::to_string) else {
                continue;
            };
            let sem_clone = Arc::clone(&sem);
            let client = self.clone();
            let store_clone = store.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem_clone.acquire_owned().await.ok()?;
                match client.refresh_channel(&store_clone, &cid).await {
                    Ok(v) => Some(v),
                    Err(_) => store_clone
                        .read()
                        .ok()?
                        .channels
                        .into_iter()
                        .find(|c| c.get("id").and_then(Value::as_str) == Some(cid.as_str())),
                }
            }));
        }

        let mut results = Vec::new();
        for handle in handles {
            if let Ok(Some(val)) = handle.await {
                results.push(val);
            }
        }
        Ok(results)
    }

    /// Fetches the signed-in user's YouTube channel subscriptions via InnerTube `FEchannels`
    /// (paginating through continuations) with a fallback to `/youtubei/v1/guide`.
    /// Returns `(channels, pagination_complete)`.
    pub async fn fetch_subscriptions_with_cookies_and_user(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
    ) -> Result<(Vec<SubscribedChannel>, bool)> {
        let Some(jar) = cookies else {
            return Ok((Vec::new(), false));
        };
        if jar.sapisid().is_none() {
            return Ok((Vec::new(), false));
        }

        let mut subs = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seen_tokens: HashSet<String> = HashSet::new();
        let mut pagination_complete = false;

        if let Ok(mut data) = self
            .post_innertube_with_cookies_and_user(
                "browse",
                json!({ "browseId": "FEchannels" }),
                Some(jar),
                auth_user,
            )
            .await
        {
            for ch in parse_subscribed_channels(&data) {
                let key = ch.channel_id.to_ascii_lowercase();
                if !key.is_empty() && seen_ids.insert(key) {
                    subs.push(ch);
                }
            }

            for _ in 1..10usize {
                let Some(token) = extract_yt_continuation(&data) else {
                    pagination_complete = true;
                    break;
                };
                if !seen_tokens.insert(token.clone()) {
                    pagination_complete = true;
                    break;
                }
                match self
                    .post_innertube_with_cookies_and_user(
                        "browse",
                        json!({ "continuation": token }),
                        Some(jar),
                        auth_user,
                    )
                    .await
                {
                    Ok(next_data) => {
                        let before = subs.len();
                        for ch in parse_subscribed_channels(&next_data) {
                            let key = ch.channel_id.to_ascii_lowercase();
                            if !key.is_empty() && seen_ids.insert(key) {
                                subs.push(ch);
                            }
                        }
                        data = next_data;
                        if subs.len() == before {
                            pagination_complete = true;
                            break;
                        }
                    }
                    Err(_) => {
                        pagination_complete = false;
                        break;
                    }
                }
            }
            if !pagination_complete && extract_yt_continuation(&data).is_none() {
                pagination_complete = true;
            }
        }

        if subs.is_empty() {
            if let Ok(guide_data) = self
                .post_innertube_with_cookies_and_user("guide", json!({}), Some(jar), auth_user)
                .await
            {
                for ch in parse_guide_subscriptions(&guide_data) {
                    let key = ch.channel_id.to_ascii_lowercase();
                    if !key.is_empty() && seen_ids.insert(key) {
                        subs.push(ch);
                    }
                }
                pagination_complete = false;
            }
        }

        Ok((subs, pagination_complete))
    }

    /// Fetches the signed-in user's YouTube Subscriptions video feed (`FEsubscriptions`)
    /// in a single InnerTube request and merges recent items into `StateStore.feed`.
    pub async fn fetch_subscription_feed_with_cookies_and_user(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
        store: &StateStore,
    ) -> Result<usize> {
        let Some(jar) = cookies else {
            return Ok(0);
        };
        if jar.sapisid().is_none() {
            return Ok(0);
        }

        let data = self
            .post_innertube_with_cookies_and_user(
                "browse",
                json!({ "browseId": "FEsubscriptions" }),
                Some(jar),
                auth_user,
            )
            .await?;

        let mut items = parse_channel_videos(&data, "", "");
        let mut seen: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
        for item in parse_lockup_videos(&data) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }
        if items.is_empty() {
            return Ok(0);
        }

        let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let count = items.len();
        store.update(|s| {
            // Build lookup maps from channelId / handle / lowercase name -> (id, name)
            let mut by_uc: HashMap<String, (String, String)> = HashMap::new();
            let mut by_handle: HashMap<String, (String, String)> = HashMap::new();
            let mut by_name: HashMap<String, (String, String)> = HashMap::new();
            for ch in &s.channels {
                let Some(local_id) = ch.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let cname = ch
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(uc) = ch
                    .get("channelId")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .or_else(|| ch.get("url").and_then(Value::as_str).and_then(extract_uc_id))
                {
                    by_uc.insert(uc.to_ascii_lowercase(), (local_id.to_string(), cname.clone()));
                }
                if let Some(handle) = ch.get("handle").and_then(Value::as_str).filter(|h| !h.is_empty()) {
                    by_handle.insert(
                        handle.trim_start_matches('@').to_ascii_lowercase(),
                        (local_id.to_string(), cname.clone()),
                    );
                }
                if !cname.is_empty() {
                    by_name.insert(cname.to_ascii_lowercase(), (local_id.to_string(), cname));
                }
            }

            let mut by_feed_id: HashMap<String, Value> = HashMap::new();
            let mut order: Vec<String> = Vec::new();
            for existing in &s.feed {
                if let Some(fid) = existing.get("feedId").and_then(Value::as_str) {
                    if !by_feed_id.contains_key(fid) {
                        order.push(fid.to_string());
                    }
                    by_feed_id.insert(fid.to_string(), existing.clone());
                }
            }

            let mut touched_channels: HashSet<String> = HashSet::new();
            for item in items.into_iter().rev() {
                let matched = item
                    .channel_url
                    .as_deref()
                    .and_then(extract_uc_id)
                    .and_then(|uc| by_uc.get(&uc.to_ascii_lowercase()))
                    .or_else(|| {
                        item.channel_url
                            .as_deref()
                            .and_then(extract_handle_from_url)
                            .and_then(|h| by_handle.get(&h))
                    })
                    .or_else(|| by_name.get(&item.channel.to_ascii_lowercase()));

                let (source_id, source_name) = match matched {
                    Some((id, name)) => {
                        touched_channels.insert(id.clone());
                        (id.clone(), name.clone())
                    }
                    None => ("youtube_subscriptions".to_string(), item.channel.clone()),
                };

                if let Ok(mut obj) = serde_json::to_value(&item) {
                    if let Some(map) = obj.as_object_mut() {
                        let feed_id = format!(
                            "{source_id}:{}",
                            if item.id.is_empty() { &item.url } else { &item.id }
                        );
                        map.insert("feedId".into(), Value::String(feed_id.clone()));
                        map.insert("sourceChannelId".into(), Value::String(source_id));
                        map.insert("sourceChannelName".into(), Value::String(source_name));
                        map.insert("seenAt".into(), Value::String(now.clone()));
                        if !by_feed_id.contains_key(&feed_id) {
                            order.insert(0, feed_id.clone());
                        }
                        by_feed_id.insert(feed_id, obj);
                    }
                }
            }

            for ch in &mut s.channels {
                if let Some(id) = ch.get("id").and_then(Value::as_str) {
                    if touched_channels.contains(id) {
                        if let Some(map) = ch.as_object_mut() {
                            map.insert("lastSync".into(), Value::String(now.clone()));
                            map.insert("error".into(), Value::String(String::new()));
                        }
                    }
                }
            }

            s.feed = order
                .into_iter()
                .filter_map(|fid| by_feed_id.remove(&fid))
                .take(200)
                .collect();
            Ok(())
        })?;

        Ok(count)
    }

    /// Synchronizes the signed-in user's YouTube channel subscriptions into `StateStore.channels`
    /// and refreshes the subscription feed.
    pub async fn sync_subscriptions_with_cookies_and_user(
        &self,
        cookies: Option<&CookieJar>,
        auth_user: u8,
        store: &StateStore,
    ) -> Result<SubscriptionSyncResult> {
        let Some(jar) = cookies else {
            let total = list_channels(store)?.len();
            return Ok(SubscriptionSyncResult {
                authenticated: false,
                total_channels: total,
                ..Default::default()
            });
        };
        if jar.sapisid().is_none() {
            let total = list_channels(store)?.len();
            return Ok(SubscriptionSyncResult {
                authenticated: false,
                total_channels: total,
                ..Default::default()
            });
        }

        let (subs, pagination_complete) = self
            .fetch_subscriptions_with_cookies_and_user(Some(jar), auth_user)
            .await?;

        let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let synced_count = subs.len();
        let mut added_count = 0usize;
        let mut updated_count = 0usize;
        let mut removed_count = 0usize;
        let mut total_channels = 0usize;

        let active_uc_ids: HashSet<String> = subs
            .iter()
            .map(|s| s.channel_id.to_ascii_lowercase())
            .collect();

        store.update(|s| {
            for sub in &subs {
                let sub_uc = sub.channel_id.to_ascii_lowercase();
                let sub_handle = sub.handle.trim_start_matches('@').to_ascii_lowercase();
                let sub_url_lower = sub.url.to_ascii_lowercase();

                let existing_idx = s.channels.iter().position(|ch| {
                    let ch_uc = ch
                        .get("channelId")
                        .and_then(Value::as_str)
                        .map(|v| v.to_ascii_lowercase())
                        .or_else(|| {
                            ch.get("url")
                                .and_then(Value::as_str)
                                .and_then(extract_uc_id)
                                .map(|v| v.to_ascii_lowercase())
                        });
                    if ch_uc.as_deref() == Some(sub_uc.as_str()) {
                        return true;
                    }
                    let ch_url = ch
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if ch_url == sub_url_lower {
                        return true;
                    }
                    if !sub_handle.is_empty() {
                        let ch_handle = ch
                            .get("handle")
                            .and_then(Value::as_str)
                            .map(|h| h.trim_start_matches('@').to_ascii_lowercase())
                            .or_else(|| extract_handle_from_url(&ch_url));
                        if ch_handle.as_deref() == Some(sub_handle.as_str()) {
                            return true;
                        }
                    }
                    false
                });

                if let Some(idx) = existing_idx {
                    if let Some(map) = s.channels[idx].as_object_mut() {
                        if !sub.name.is_empty() {
                            map.insert("name".into(), Value::String(sub.name.clone()));
                        }
                        if !sub.thumbnail.is_empty() {
                            map.insert("thumbnail".into(), Value::String(sub.thumbnail.clone()));
                        }
                        if !sub.channel_id.is_empty() {
                            map.insert("channelId".into(), Value::String(sub.channel_id.clone()));
                        }
                        if !sub.handle.is_empty() {
                            map.insert("handle".into(), Value::String(sub.handle.clone()));
                        }
                        if let Some(ref sc) = sub.subscriber_count {
                            map.insert("subscriberCount".into(), Value::String(sc.clone()));
                        }
                        map.insert("syncedFromYoutube".into(), Value::Bool(true));
                    }
                    updated_count += 1;
                } else {
                    let mut obj = json!({
                        "id": Uuid::new_v4().to_string(),
                        "name": sub.name,
                        "url": sub.url,
                        "channelId": sub.channel_id,
                        "handle": sub.handle,
                        "thumbnail": sub.thumbnail,
                        "syncedFromYoutube": true,
                        "lastSync": "",
                        "error": "",
                    });
                    if let Some(ref sc) = sub.subscriber_count {
                        obj["subscriberCount"] = Value::String(sc.clone());
                    }
                    s.channels.push(obj);
                    added_count += 1;
                }
            }

            if pagination_complete && !subs.is_empty() {
                let before = s.channels.len();
                let mut removed_ids: HashSet<String> = HashSet::new();
                s.channels.retain(|ch| {
                    let is_synced = ch
                        .get("syncedFromYoutube")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if !is_synced {
                        return true;
                    }
                    let ch_uc = ch
                        .get("channelId")
                        .and_then(Value::as_str)
                        .map(|v| v.to_ascii_lowercase())
                        .or_else(|| {
                            ch.get("url")
                                .and_then(Value::as_str)
                                .and_then(extract_uc_id)
                                .map(|v| v.to_ascii_lowercase())
                        });
                    let keep = ch_uc
                        .as_deref()
                        .is_some_and(|uc| active_uc_ids.contains(uc));
                    if !keep {
                        if let Some(id) = ch.get("id").and_then(Value::as_str) {
                            removed_ids.insert(id.to_string());
                        }
                    }
                    keep
                });
                removed_count = before.saturating_sub(s.channels.len());
                if !removed_ids.is_empty() {
                    s.feed.retain(|f| {
                        f.get("sourceChannelId")
                            .and_then(Value::as_str)
                            .is_none_or(|id| !removed_ids.contains(id))
                    });
                }
            }

            s.channels_last_sync = now.clone();
            total_channels = s.channels.len();
            Ok(())
        })?;

        let feed_count = self
            .fetch_subscription_feed_with_cookies_and_user(Some(jar), auth_user, store)
            .await
            .unwrap_or(0);

        Ok(SubscriptionSyncResult {
            authenticated: true,
            synced_count,
            added_count,
            updated_count,
            removed_count,
            total_channels,
            feed_count,
            last_sync: now,
        })
    }

    /// Fetches rich watch-page video details (description, publish date, relative time,
    /// chapters, caption tracks, storyboard spec, and comments continuation) via InnerTube.
    pub async fn fetch_video_details(
        &self,
        url_or_id: &str,
        cookies: Option<&CookieJar>,
        auth_user: u8,
    ) -> Result<VideoDetails> {
        let video_id = extract_video_id(url_or_id).ok_or_else(|| {
            CoreError::Validation(format!("Invalid YouTube video URL or ID: {url_or_id}"))
        })?;
        if let Some(cached) = self.details_cache.get(&video_id) {
            return Ok(cached);
        }

        let (player_res, next_res) = tokio::join!(
            self.post_innertube_with_cookies_and_user(
                "player",
                json!({ "videoId": video_id }),
                cookies,
                auth_user,
            ),
            self.post_innertube_with_cookies_and_user(
                "next",
                json!({ "videoId": video_id }),
                cookies,
                auth_user,
            ),
        );

        let player_data = player_res.unwrap_or(Value::Null);
        let next_data = next_res.unwrap_or(Value::Null);
        if player_data.is_null() && next_data.is_null() {
            return Err(CoreError::External(format!(
                "Could not fetch video details for {video_id}"
            )));
        }

        let details = parse_video_details(&video_id, &player_data, &next_data);
        self.details_cache.set(video_id, details.clone());
        Ok(details)
    }

    /// Fetches a page of comments (or replies) using an InnerTube continuation token.
    pub async fn fetch_comments(
        &self,
        continuation: &str,
        cookies: Option<&CookieJar>,
        auth_user: u8,
    ) -> Result<CommentsPage> {
        let token = continuation.trim();
        if token.is_empty() {
            return Err(CoreError::Validation(
                "Missing comments continuation token".into(),
            ));
        }
        let data = self
            .post_innertube_with_cookies_and_user(
                "next",
                json!({ "continuation": token }),
                cookies,
                auth_user,
            )
            .await?;
        Ok(parse_comments_response(&data))
    }

    /// Fetches a YouTube subtitle track as WebVTT, optionally auto-translated via `tlang`.
    pub async fn fetch_subtitle_vtt(
        &self,
        base_url: &str,
        tlang: Option<&str>,
        cookies: Option<&CookieJar>,
    ) -> Result<String> {
        let target_url = build_timedtext_vtt_url(base_url, tlang)?;
        let mut req = self.http.get(&target_url);
        if let Some(jar) = cookies {
            let raw_cookie = jar.cookie_header_for_host("www.youtube.com");
            if !raw_cookie.is_empty() {
                req = req.header("Cookie", raw_cookie);
            }
        }
        let resp = req.send().await?.error_for_status()?;
        let body = resp.text().await?;
        let trimmed = body.trim();
        if trimmed.starts_with("WEBVTT") {
            return Ok(body);
        }
        if let Some(vtt) = convert_xml_subtitles_to_vtt(trimmed) {
            return Ok(vtt);
        }
        if trimmed.is_empty() {
            return Ok("WEBVTT\n\n".to_string());
        }
        Err(CoreError::External(
            "Unsupported subtitle format returned by YouTube".into(),
        ))
    }
}

pub fn list_channels(store: &StateStore) -> Result<Vec<Value>> {
    Ok(store.read()?.channels)
}

pub fn list_feed(store: &StateStore) -> Result<Vec<Value>> {
    Ok(store.read()?.feed)
}

pub fn delete_channel(store: &StateStore, channel_id: &str) -> Result<()> {
    store.update(|s| {
        s.channels
            .retain(|c| c.get("id").and_then(Value::as_str) != Some(channel_id));
        s.feed
            .retain(|f| f.get("sourceChannelId").and_then(Value::as_str) != Some(channel_id));
        Ok(())
    })?;
    Ok(())
}

pub fn is_youtube_url(value: &str) -> bool {
    let trimmed = value.trim();
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtu.be"
}

pub fn extract_video_id(value: &str) -> Option<String> {
    static FULL_ID: OnceLock<Regex> = OnceLock::new();
    static URL_ID: OnceLock<Regex> = OnceLock::new();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let full = FULL_ID.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());
    if full.is_match(trimmed) {
        return Some(trimmed.to_string());
    }
    let url_re = URL_ID
        .get_or_init(|| Regex::new(r"(?:v=|youtu\.be/|shorts/|embed/)([A-Za-z0-9_-]{11})").unwrap());
    url_re
        .captures(trimmed)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn resolve_channel_base(url: &str) -> Result<String> {
    static TAB_SUFFIX: OnceLock<Regex> = OnceLock::new();
    let tab_re = TAB_SUFFIX.get_or_init(|| {
        Regex::new(r"(?i)/(videos|shorts|streams|playlists|releases|featured|community|about)/?$")
            .unwrap()
    });

    let mut clean = url.trim().trim_end_matches('/').to_string();
    if clean.is_empty() {
        return Err(CoreError::Validation("URL de canal vacia".into()));
    }
    if !clean.starts_with("http://") && !clean.starts_with("https://") {
        if clean.starts_with("UC") && clean.len() == 24 {
            clean = format!("https://www.youtube.com/channel/{clean}");
        } else {
            clean = format!("https://www.youtube.com/@{}", clean.trim_start_matches('@'));
        }
    } else if clean.contains("music.youtube.com/") {
        clean = clean.replace("music.youtube.com/", "www.youtube.com/");
    }
    let stripped = tab_re.replace(&clean, "").trim_end_matches('/').to_string();
    Ok(stripped)
}

pub fn channel_videos_url(url: &str) -> Result<String> {
    let base = resolve_channel_base(url)?;
    Ok(format!("{base}/videos"))
}

pub fn format_duration(seconds: Option<u64>) -> String {
    let Some(secs) = seconds else {
        return "--:--".into();
    };
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn parse_duration_text(text: &str) -> Option<u64> {
    let parts: Vec<u64> = text
        .trim()
        .split(':')
        .map(|p| p.trim().parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [m, s] => Some(m * 60 + s),
        [h, m, s] => Some(h * 3600 + m * 60 + s),
        _ => None,
    }
}

fn default_thumbnail(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")
}

fn extract_uc_id(base_url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"/channel/(UC[A-Za-z0-9_-]{22})").unwrap());
    re.captures(base_url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn tab_params(tab: &str) -> Option<&'static str> {
    match tab {
        "videos" => Some("EgZ2aWRlb3PyBgQKAjoA"),
        "shorts" => Some("EgZzaG9ydHPyBgUKA5oBAA=="),
        "streams" => Some("EgdzdHJlYW1z8gYECgJ6AA=="),
        "playlists" => Some("EglwbGF5bGlzdHPyBgQKAkIA"),
        "releases" => Some("EghyZWxlYXNlc_IGBQoDsgEA"),
        _ => None,
    }
}

fn extract_yt_continuation(data: &Value) -> Option<String> {
    for node in walk_renderers(data, "continuationCommand") {
        if let Some(tok) = node.get("token").and_then(Value::as_str) {
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
    }
    for node in walk_renderers(data, "nextContinuationData") {
        if let Some(tok) = node.get("continuation").and_then(Value::as_str) {
            if !tok.is_empty() {
                return Some(tok.to_string());
            }
        }
    }
    None
}

fn find_followed_channel(
    store: &StateStore,
    base: &str,
    canonical: &str,
    handle: &str,
) -> Option<String> {
    let state = store.read().ok()?;
    let mut targets = HashSet::new();
    targets.insert(base.to_ascii_lowercase());
    targets.insert(canonical.to_ascii_lowercase());
    let target_uc = extract_uc_id(canonical)
        .or_else(|| extract_uc_id(base))
        .map(|u| u.to_ascii_lowercase());
    let target_handle = if !handle.is_empty() {
        let h = handle.trim_start_matches('@').to_ascii_lowercase();
        targets.insert(format!("https://www.youtube.com/@{h}"));
        Some(h)
    } else {
        extract_handle_from_url(base)
    };
    for ch in &state.channels {
        let url = ch.get("url").and_then(Value::as_str).unwrap_or("");
        let resolved = resolve_channel_base(url)
            .unwrap_or_else(|_| url.to_string())
            .to_ascii_lowercase();
        if targets.contains(&resolved) {
            return ch.get("id").and_then(Value::as_str).map(ToString::to_string);
        }
        if let Some(ref uc) = target_uc {
            let ch_uc = ch
                .get("channelId")
                .and_then(Value::as_str)
                .map(|v| v.to_ascii_lowercase())
                .or_else(|| extract_uc_id(url).map(|v| v.to_ascii_lowercase()));
            if ch_uc.as_deref() == Some(uc.as_str()) {
                return ch.get("id").and_then(Value::as_str).map(ToString::to_string);
            }
        }
        if let Some(ref h) = target_handle {
            let ch_handle = ch
                .get("handle")
                .and_then(Value::as_str)
                .map(|v| v.trim_start_matches('@').to_ascii_lowercase())
                .or_else(|| extract_handle_from_url(url));
            if ch_handle.as_deref() == Some(h.as_str()) {
                return ch.get("id").and_then(Value::as_str).map(ToString::to_string);
            }
        }
    }
    None
}

fn extract_handle_from_url(url: &str) -> Option<String> {
    let idx = url.find("/@")?;
    let rest = &url[idx + 2..];
    let handle: String = rest
        .chars()
        .take_while(|c| !matches!(c, '/' | '?' | '#'))
        .collect();
    if handle.is_empty() {
        None
    } else {
        Some(handle.to_ascii_lowercase())
    }
}

pub(crate) fn parse_subscribed_channels(data: &Value) -> Vec<SubscribedChannel> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for renderer_name in ["channelRenderer", "gridChannelRenderer"] {
        for node in walk_renderers(data, renderer_name) {
            let channel_id = node
                .get("channelId")
                .and_then(Value::as_str)
                .or_else(|| {
                    node.pointer("/navigationEndpoint/browseEndpoint/browseId")
                        .and_then(Value::as_str)
                })
                .unwrap_or("")
                .trim()
                .to_string();
            if channel_id.is_empty() || !seen.insert(channel_id.to_ascii_lowercase()) {
                continue;
            }
            let name = text_from_node(node.get("title")).unwrap_or_else(|| channel_id.clone());
            let canonical_path = node
                .pointer("/navigationEndpoint/browseEndpoint/canonicalBaseUrl")
                .or_else(|| {
                    node.pointer("/navigationEndpoint/commandMetadata/webCommandMetadata/url")
                })
                .and_then(Value::as_str)
                .unwrap_or("");
            let handle = if canonical_path.starts_with("/@") {
                canonical_path.trim_start_matches('/').to_string()
            } else {
                String::new()
            };
            let url = format!("https://www.youtube.com/channel/{channel_id}/videos");
            let thumbnail = last_thumbnail_url(node.get("thumbnail")).unwrap_or_default();
            let subscriber_count = text_from_node(node.get("subscriberCountText"))
                .or_else(|| text_from_node(node.get("videoCountText")));

            out.push(SubscribedChannel {
                channel_id,
                name,
                url,
                handle,
                thumbnail,
                subscriber_count,
            });
        }
    }

    for node in walk_renderers(data, "lockupViewModel") {
        let browse_id = node
            .pointer("/rendererContext/commandContext/onTap/innertubeCommand/browseEndpoint/browseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !browse_id.starts_with("UC") || !seen.insert(browse_id.to_ascii_lowercase()) {
            continue;
        }
        let name = node
            .pointer("/metadata/lockupMetadataViewModel/title/content")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(browse_id)
            .to_string();
        let canonical_path = node
            .pointer("/rendererContext/commandContext/onTap/innertubeCommand/browseEndpoint/canonicalBaseUrl")
            .and_then(Value::as_str)
            .unwrap_or("");
        let handle = if canonical_path.starts_with("/@") {
            canonical_path.trim_start_matches('/').to_string()
        } else {
            String::new()
        };
        let thumbnail = lockup_thumbnail(node).unwrap_or_default();
        out.push(SubscribedChannel {
            channel_id: browse_id.to_string(),
            name,
            url: format!("https://www.youtube.com/channel/{browse_id}/videos"),
            handle,
            thumbnail,
            subscriber_count: None,
        });
    }

    out
}

pub(crate) fn parse_guide_subscriptions(data: &Value) -> Vec<SubscribedChannel> {
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for section in walk_renderers(data, "guideSubscriptionsSectionRenderer") {
        for entry in walk_renderers(section, "guideEntryRenderer") {
            let browse_id = entry
                .pointer("/navigationEndpoint/browseEndpoint/browseId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !browse_id.starts_with("UC") || !seen.insert(browse_id.to_ascii_lowercase()) {
                continue;
            }
            let name = text_from_node(entry.get("formattedTitle"))
                .unwrap_or_else(|| browse_id.to_string());
            let canonical_path = entry
                .pointer("/navigationEndpoint/browseEndpoint/canonicalBaseUrl")
                .and_then(Value::as_str)
                .unwrap_or("");
            let handle = if canonical_path.starts_with("/@") {
                canonical_path.trim_start_matches('/').to_string()
            } else {
                String::new()
            };
            let thumbnail = last_thumbnail_url(entry.get("thumbnail")).unwrap_or_default();
            out.push(SubscribedChannel {
                channel_id: browse_id.to_string(),
                name,
                url: format!("https://www.youtube.com/channel/{browse_id}/videos"),
                handle,
                thumbnail,
                subscriber_count: None,
            });
        }
    }

    out
}

fn parse_search_items(data: &Value, limit: usize) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for renderer in walk_renderers(data, "videoRenderer") {
        if let Some(item) = parse_video_renderer(renderer, None, None, false) {
            if seen.insert(item.id.clone()) {
                items.push(item);
                if items.len() >= limit {
                    return items;
                }
            }
        }
    }
    for renderer in walk_renderers(data, "reelItemRenderer") {
        if let Some(item) = parse_video_renderer(renderer, None, None, true) {
            if seen.insert(item.id.clone()) {
                items.push(item);
                if items.len() >= limit {
                    return items;
                }
            }
        }
    }
    for shorts in walk_renderers(data, "shortsLockupViewModel") {
        if let Some(item) = parse_shorts_lockup(shorts, None, None) {
            if seen.insert(item.id.clone()) {
                items.push(item);
                if items.len() >= limit {
                    return items;
                }
            }
        }
    }
    for item in parse_lockup_videos(data) {
        if seen.insert(item.id.clone()) {
            items.push(item);
            if items.len() >= limit {
                return items;
            }
        }
    }
    items
}

fn parse_shorts_lockup(
    shorts: &Value,
    fallback_channel: Option<&str>,
    fallback_url: Option<&str>,
) -> Option<MediaItem> {
    let vid = shorts
        .pointer("/onTap/innertubeCommand/reelWatchEndpoint/videoId")
        .and_then(Value::as_str)
        .unwrap_or("");
    if vid.len() != 11 {
        return None;
    }
    let title = shorts
        .pointer("/overlayMetadata/primaryText/content")
        .and_then(Value::as_str)
        .unwrap_or("Short")
        .to_string();
    let thumbnail = lockup_thumbnail(shorts).or_else(|| Some(default_thumbnail(vid)));
    Some(MediaItem {
        id: vid.to_string(),
        url: format!("https://www.youtube.com/shorts/{vid}"),
        title,
        channel: fallback_channel.unwrap_or("YouTube").to_string(),
        channel_url: fallback_url.map(ToString::to_string),
        duration: None,
        duration_text: "Short".into(),
        thumbnail,
        view_count: None,
        live: false,
        members_only: is_members_only_renderer(shorts),
        result_type: Some("short".into()),
        watched_percent: extract_watched_percent(shorts),
    })
}

fn parse_channel_videos(data: &Value, channel_name: &str, channel_id: &str) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let fallback_channel = if channel_name.is_empty() {
        None
    } else {
        Some(channel_name)
    };
    let fallback_url = if channel_id.is_empty() {
        None
    } else {
        Some(format!("https://www.youtube.com/channel/{channel_id}"))
    };

    for key in ["videoRenderer", "gridVideoRenderer", "reelItemRenderer"] {
        let force_short = key == "reelItemRenderer";
        for renderer in walk_renderers(data, key) {
            if let Some(item) =
                parse_video_renderer(renderer, fallback_channel, fallback_url.as_deref(), force_short)
            {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
            }
        }
    }
    // Also parse modern lockupViewModel and shortsLockupViewModel used on channel Videos/Streams/Shorts tabs
    for mut item in parse_lockup_videos(data) {
        if (item.channel.is_empty() || item.channel == "YouTube") && fallback_channel.is_some() {
            item.channel = fallback_channel.unwrap().to_string();
        }
        if item.channel_url.is_none() && fallback_url.is_some() {
            item.channel_url = fallback_url.clone();
        }
        if seen.insert(item.id.clone()) {
            items.push(item);
        }
    }
    for shorts in walk_renderers(data, "shortsLockupViewModel") {
        if let Some(item) = parse_shorts_lockup(shorts, fallback_channel, fallback_url.as_deref()) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }
    }
    items
}

fn parse_lockup_videos(data: &Value) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for lockup in walk_renderers(data, "lockupViewModel") {
        if is_ad_renderer(lockup) {
            continue;
        }
        let content_type = lockup
            .get("contentType")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !content_type.is_empty() && !content_type.contains("VIDEO") {
            continue;
        }
        let Some(video_id) = lockup.get("contentId").and_then(Value::as_str) else {
            continue;
        };
        if video_id.len() != 11 || !seen.insert(video_id.to_string()) {
            continue;
        }
        let title = lockup
            .pointer("/metadata/lockupMetadataViewModel/title/content")
            .and_then(Value::as_str)
            .unwrap_or("Sin titulo")
            .to_string();
        let channel = lockup
            .pointer("/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows/0/metadataParts/0/text/content")
            .and_then(Value::as_str)
            .unwrap_or("YouTube")
            .to_string();
        let channel_url = extract_lockup_channel_url(lockup);
        let is_short = is_short_renderer(lockup);
        let (duration, duration_text_raw, live) = extract_duration_and_live(lockup);
        let thumbnail = lockup_thumbnail(lockup).or_else(|| Some(default_thumbnail(video_id)));

        items.push(MediaItem {
            id: video_id.to_string(),
            url: if is_short {
                format!("https://www.youtube.com/shorts/{video_id}")
            } else {
                format!("https://www.youtube.com/watch?v={video_id}")
            },
            title,
            channel,
            channel_url,
            duration,
            duration_text: duration_text_raw.unwrap_or_else(|| {
                if live {
                    "EN VIVO".into()
                } else if is_short && duration.is_none() {
                    "Short".into()
                } else {
                    format_duration(duration)
                }
            }),
            thumbnail,
            view_count: None,
            live,
            members_only: is_members_only_renderer(lockup),
            result_type: if is_short { Some("short".into()) } else { None },
            watched_percent: extract_watched_percent(lockup),
        });
    }
    items
}

fn lockup_thumbnail(lockup: &Value) -> Option<String> {
    for ptr in [
        "/contentImage/thumbnailViewModel/image/sources",
        "/contentImage/collectionThumbnailViewModel/primaryThumbnail/thumbnailViewModel/image/sources",
    ] {
        if let Some(url) = lockup
            .pointer(ptr)
            .and_then(Value::as_array)
            .and_then(|arr| arr.last())
            .and_then(|s| s.get("url"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            return Some(url.to_string());
        }
    }
    None
}

fn extract_lockup_channel_url(lockup: &Value) -> Option<String> {
    let scope = lockup.get("metadata").unwrap_or(lockup);
    find_channel_url_in_value(scope)
}

fn find_channel_url_in_value(node: &Value) -> Option<String> {
    match node {
        Value::Object(map) => {
            if let Some(be) = map.get("browseEndpoint") {
                if let Some(bid) = be.get("browseId").and_then(Value::as_str) {
                    if bid.starts_with("UC") && bid.len() >= 20 {
                        return Some(format!("https://www.youtube.com/channel/{bid}"));
                    }
                }
                if let Some(canon) = be.get("canonicalBaseUrl").and_then(Value::as_str) {
                    if canon.starts_with("/@")
                        || canon.starts_with("/channel/")
                        || canon.starts_with("/c/")
                        || canon.starts_with("/user/")
                    {
                        return Some(format!("https://www.youtube.com{canon}"));
                    }
                }
            }
            for v in map.values() {
                if let Some(found) = find_channel_url_in_value(v) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(found) = find_channel_url_in_value(item) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn find_string_field<'a>(node: &'a Value, key: &str) -> Option<&'a str> {
    match node {
        Value::Object(map) => {
            if let Some(val) = map.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                return Some(val);
            }
            for v in map.values() {
                if let Some(found) = find_string_field(v, key) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => {
            for item in arr {
                if let Some(found) = find_string_field(item, key) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn extract_playlist_id_from_url(url: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[?&]list=([A-Za-z0-9_-]+)").unwrap());
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn extract_seed_video_from_playlist_id(playlist_id: &str) -> Option<String> {
    let clean = playlist_id.trim();
    if clean.len() == 13 && clean.starts_with("RD") {
        let candidate = &clean[2..];
        if candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Some(candidate.to_string());
        }
    } else if clean.len() == 17 && clean.starts_with("RDAMVM") {
        let candidate = &clean[6..];
        if candidate.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Some(candidate.to_string());
        }
    }
    None
}

fn parse_recommended_mixes(data: &Value) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for lockup in walk_renderers(data, "lockupViewModel") {
        if is_ad_renderer(lockup) {
            continue;
        }
        let content_type = lockup
            .get("contentType")
            .and_then(Value::as_str)
            .unwrap_or("");
        let content_id = lockup
            .get("contentId")
            .and_then(Value::as_str)
            .unwrap_or("");

        let is_mix_or_playlist = content_type.contains("PLAYLIST")
            || content_type.contains("MIX")
            || content_type.contains("RADIO")
            || content_id.starts_with("RD")
            || content_id.starts_with("PL")
            || content_id.starts_with("OLAK");
        if !is_mix_or_playlist {
            continue;
        }

        let watch_ep = lockup.pointer("/rendererContext/commandContext/onTap/innertubeCommand/watchEndpoint");
        let playlist_id = watch_ep
            .and_then(|w| w.get("playlistId"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| (!content_id.is_empty()).then_some(content_id))
            .or_else(|| find_string_field(lockup, "playlistId"));

        let Some(pid) = playlist_id else {
            continue;
        };
        if !seen.insert(pid.to_string()) {
            continue;
        }

        let seed_vid = watch_ep
            .and_then(|w| w.get("videoId"))
            .and_then(Value::as_str)
            .filter(|s| s.len() == 11);

        let url = if let Some(vid) = seed_vid {
            format!("https://www.youtube.com/watch?v={vid}&list={pid}")
        } else {
            format!("https://www.youtube.com/playlist?list={pid}")
        };

        let title = lockup
            .pointer("/metadata/lockupMetadataViewModel/title/content")
            .and_then(Value::as_str)
            .unwrap_or("Mix de YouTube")
            .to_string();

        let is_mix = pid.starts_with("RD") || content_type.contains("MIX") || content_type.contains("RADIO");
        let channel = lockup
            .pointer("/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows/0/metadataParts/0/text/content")
            .and_then(Value::as_str)
            .unwrap_or(if is_mix { "YouTube Mix" } else { "Playlist de YouTube" })
            .to_string();

        let thumbnail = lockup_thumbnail(lockup).or_else(|| seed_vid.map(default_thumbnail));

        items.push(MediaItem {
            id: pid.to_string(),
            url,
            title,
            channel,
            channel_url: extract_lockup_channel_url(lockup),
            duration: None,
            duration_text: if is_mix { "Mix".into() } else { "Playlist".into() },
            thumbnail,
            view_count: None,
            live: false,
            members_only: false,
            result_type: Some(if is_mix { "mix".into() } else { "playlist".into() }),
            watched_percent: None,
        });
    }

    for key in ["radioRenderer", "compactRadioRenderer"] {
        for renderer in walk_renderers(data, key) {
            if is_ad_renderer(renderer) {
                continue;
            }
            let Some(pid) = renderer.get("playlistId").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
                continue;
            };
            if !seen.insert(pid.to_string()) {
                continue;
            }
            let seed_vid = renderer
                .pointer("/navigationEndpoint/watchEndpoint/videoId")
                .and_then(Value::as_str)
                .filter(|s| s.len() == 11);
            let url = if let Some(vid) = seed_vid {
                format!("https://www.youtube.com/watch?v={vid}&list={pid}")
            } else {
                format!("https://www.youtube.com/playlist?list={pid}")
            };
            let title = text_from_node(renderer.get("title")).unwrap_or_else(|| "Mix de YouTube".into());
            let channel = text_from_node(renderer.get("longBylineText"))
                .or_else(|| text_from_node(renderer.get("shortBylineText")))
                .unwrap_or_else(|| "YouTube Mix".into());
            let thumbnail = last_thumbnail_url(renderer.get("thumbnail"))
                .or_else(|| seed_vid.map(default_thumbnail));
            items.push(MediaItem {
                id: pid.to_string(),
                url,
                title,
                channel,
                channel_url: extract_browse_channel_url(renderer),
                duration: None,
                duration_text: "Mix".into(),
                thumbnail,
                view_count: None,
                live: false,
                members_only: false,
                result_type: Some("mix".into()),
                watched_percent: None,
            });
        }
    }

    items
}

fn parse_channel_playlists(data: &Value) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for key in ["gridPlaylistRenderer", "playlistRenderer"] {
        for renderer in walk_renderers(data, key) {
            let Some(pid) = renderer.get("playlistId").and_then(Value::as_str) else {
                continue;
            };
            if pid.is_empty() || !seen.insert(pid.to_string()) {
                continue;
            }
            let title = text_from_node(renderer.get("title")).unwrap_or_else(|| "Sin titulo".into());
            let thumb = last_thumbnail_url(renderer.get("thumbnail"));
            items.push(MediaItem {
                id: pid.to_string(),
                url: format!("https://www.youtube.com/playlist?list={pid}"),
                title,
                channel: text_from_node(renderer.get("shortBylineText"))
                    .unwrap_or_else(|| "YouTube".into()),
                channel_url: extract_browse_channel_url(renderer),
                duration: None,
                duration_text: text_from_node(renderer.get("videoCountText"))
                    .unwrap_or_else(|| "--:--".into()),
                thumbnail: thumb,
                view_count: None,
                live: false,
                members_only: false,
                result_type: Some("playlist".into()),
                watched_percent: None,
            });
        }
    }
    items
}

fn parse_video_renderer(
    renderer: &Value,
    fallback_channel: Option<&str>,
    fallback_channel_url: Option<&str>,
    force_short: bool,
) -> Option<MediaItem> {
    if is_ad_renderer(renderer) {
        return None;
    }
    let video_id = renderer.get("videoId").and_then(Value::as_str)?;
    if video_id.is_empty() {
        return None;
    }
    let title = text_from_node(renderer.get("title"))
        .or_else(|| text_from_node(renderer.get("headline")))
        .unwrap_or_else(|| "Sin titulo".into());
    let channel = text_from_node(renderer.get("ownerText"))
        .or_else(|| text_from_node(renderer.get("longBylineText")))
        .or_else(|| text_from_node(renderer.get("shortBylineText")))
        .or_else(|| fallback_channel.map(ToString::to_string))
        .unwrap_or_else(|| "YouTube".into());
    let channel_url = extract_browse_channel_url(renderer)
        .or_else(|| fallback_channel_url.map(ToString::to_string));

    let is_short = force_short || is_short_renderer(renderer);
    let (duration, duration_raw, live) = extract_duration_and_live(renderer);
    let view_count = text_from_node(renderer.get("viewCountText"))
        .as_deref()
        .and_then(parse_view_count);

    let thumbnail =
        last_thumbnail_url(renderer.get("thumbnail")).or_else(|| Some(default_thumbnail(video_id)));

    Some(MediaItem {
        id: video_id.to_string(),
        url: if is_short {
            format!("https://www.youtube.com/shorts/{video_id}")
        } else {
            format!("https://www.youtube.com/watch?v={video_id}")
        },
        title,
        channel,
        channel_url,
        duration,
        duration_text: duration_raw.unwrap_or_else(|| {
            if live {
                "EN VIVO".into()
            } else if is_short && duration.is_none() {
                "Short".into()
            } else {
                format_duration(duration)
            }
        }),
        thumbnail,
        view_count,
        live,
        members_only: is_members_only_renderer(renderer),
        result_type: if is_short { Some("short".into()) } else { None },
        watched_percent: extract_watched_percent(renderer),
    })
}

fn extract_watched_percent(node: &Value) -> Option<u8> {
    for overlay in walk_renderers(node, "thumbnailOverlayResumePlaybackRenderer") {
        if let Some(pct) = overlay.get("percentDurationWatched").and_then(Value::as_u64) {
            if pct > 0 {
                return Some(pct.min(100) as u8);
            }
        }
    }
    for overlay in walk_renderers(node, "thumbnailBottomOverlayViewModel") {
        if let Some(pct) = overlay
            .pointer("/progressBar/percentDurationWatched")
            .and_then(Value::as_u64)
        {
            if pct > 0 {
                return Some(pct.min(100) as u8);
            }
        }
    }
    for overlay in walk_renderers(node, "thumbnailOverlayProgressBarViewModel") {
        if let Some(pct) = overlay.get("startPercent").and_then(Value::as_u64) {
            if pct > 0 {
                return Some(pct.min(100) as u8);
            }
        }
    }
    None
}

fn is_short_renderer(node: &Value) -> bool {
    for ptr in [
        "/navigationEndpoint/reelWatchEndpoint",
        "/onTap/innertubeCommand/reelWatchEndpoint",
        "/rendererContext/commandContext/onTap/innertubeCommand/reelWatchEndpoint",
    ] {
        if node.pointer(ptr).is_some() {
            return true;
        }
    }
    for ptr in [
        "/navigationEndpoint/commandMetadata/webCommandMetadata/url",
        "/onTap/innertubeCommand/commandMetadata/webCommandMetadata/url",
        "/rendererContext/commandContext/onTap/innertubeCommand/commandMetadata/webCommandMetadata/url",
    ] {
        if let Some(url) = node.pointer(ptr).and_then(Value::as_str) {
            if url.to_ascii_lowercase().contains("/shorts/") {
                return true;
            }
        }
    }
    for overlay in walk_renderers(node, "thumbnailOverlayTimeStatusRenderer") {
        if let Some(style) = overlay.get("style").and_then(Value::as_str) {
            if style.to_ascii_uppercase().contains("SHORT") {
                return true;
            }
        }
        if let Some(text) = text_from_node(overlay.get("text")) {
            let up = text.trim().to_ascii_uppercase();
            if up == "SHORT" || up == "SHORTS" {
                return true;
            }
        }
    }
    for badge in walk_renderers(node, "thumbnailBadgeViewModel") {
        if let Some(style) = badge.get("badgeStyle").and_then(Value::as_str) {
            if style.to_ascii_uppercase().contains("SHORT") {
                return true;
            }
        }
        if let Some(text) = badge.get("text").and_then(Value::as_str) {
            let up = text.trim().to_ascii_uppercase();
            if up == "SHORT" || up == "SHORTS" {
                return true;
            }
        }
    }
    false
}

fn extract_channel_header(data: &Value) -> (String, String, String, Option<u64>) {
    let meta = data.pointer("/metadata/channelMetadataRenderer");
    let header = data.pointer("/header/c4TabbedHeaderRenderer");

    let name = meta
        .and_then(|m| m.get("title"))
        .and_then(Value::as_str)
        .or_else(|| header.and_then(|h| h.get("title")).and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    let handle = header
        .and_then(|h| text_from_node(h.get("channelHandleText")))
        .or_else(|| {
            meta.and_then(|m| m.get("vanityChannelUrl"))
                .and_then(Value::as_str)
                .and_then(|u| u.rsplit('/').next())
                .filter(|s| s.starts_with('@'))
                .map(ToString::to_string)
        })
        .unwrap_or_default();

    let avatar = last_thumbnail_url(meta.and_then(|m| m.get("avatar")))
        .or_else(|| last_thumbnail_url(header.and_then(|h| h.get("avatar"))))
        .unwrap_or_default();

    let subscriber_count = header
        .and_then(|h| text_from_node(h.get("subscriberCountText")))
        .as_deref()
        .and_then(parse_view_count);

    (name, handle, avatar, subscriber_count)
}

fn extract_browse_channel_url(renderer: &Value) -> Option<String> {
    for key in ["ownerText", "longBylineText", "shortBylineText"] {
        if let Some(runs) = renderer.get(key).and_then(|n| n.get("runs")).and_then(Value::as_array) {
            for run in runs {
                if let Some(bid) = run
                    .pointer("/navigationEndpoint/browseEndpoint/browseId")
                    .and_then(Value::as_str)
                {
                    if bid.starts_with("UC") {
                        return Some(format!("https://www.youtube.com/channel/{bid}"));
                    }
                }
                if let Some(canon) = run
                    .pointer("/navigationEndpoint/browseEndpoint/canonicalBaseUrl")
                    .and_then(Value::as_str)
                {
                    if !canon.is_empty() {
                        return Some(format!("https://www.youtube.com{canon}"));
                    }
                }
            }
        }
    }
    None
}

fn parse_view_count(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u64>().ok()
}

pub(crate) fn text_from_node(node: Option<&Value>) -> Option<String> {
    let node = node?;
    if let Some(s) = node.as_str() {
        let t = s.trim();
        return if t.is_empty() { None } else { Some(t.to_string()) };
    }
    if let Some(simple) = node.get("simpleText").and_then(Value::as_str) {
        let t = simple.trim();
        return if t.is_empty() { None } else { Some(t.to_string()) };
    }
    if let Some(runs) = node.get("runs").and_then(Value::as_array) {
        let joined: String = runs
            .iter()
            .filter_map(|r| r.get("text").and_then(Value::as_str))
            .collect();
        let t = joined.trim();
        return if t.is_empty() { None } else { Some(t.to_string()) };
    }
    None
}

pub(crate) fn upgrade_thumbnail_url(url: &str) -> String {
    let normalized = if url.starts_with("//") {
        format!("https:{url}")
    } else {
        url.to_string()
    };

    // Upgrade tiny YouTube Music / Google Photos square thumbnails (e.g. =w120-h120-l90-rj)
    // to HD (=w544-h544...) while preserving any trailing flags.
    if normalized.contains("googleusercontent.com/") {
        if let Some(eq_idx) = normalized.rfind('=') {
            let prefix = &normalized[..eq_idx];
            let suffix = &normalized[eq_idx + 1..];
            if let Some(rest) = suffix.strip_prefix('w') {
                let w_digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !w_digits.is_empty() {
                    let after_w = &rest[w_digits.len()..];
                    if let Some(after_h_prefix) = after_w.strip_prefix("-h") {
                        let h_digits: String = after_h_prefix
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if !h_digits.is_empty() {
                            let tail = &after_h_prefix[h_digits.len()..];
                            if let (Ok(w), Ok(h)) = (w_digits.parse::<u32>(), h_digits.parse::<u32>()) {
                                if w == h && w < 544 {
                                    return format!("{prefix}=w544-h544{tail}");
                                }
                            }
                        }
                    }
                }
            } else if let Some(rest) = suffix.strip_prefix('s') {
                let s_digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !s_digits.is_empty() {
                    let tail = &rest[s_digits.len()..];
                    if let Ok(size) = s_digits.parse::<u32>() {
                        if size > 0 && size < 544 {
                            return format!("{prefix}=s544{tail}");
                        }
                    }
                }
            }
        }
    }

    normalized
}

pub(crate) fn last_thumbnail_url(node: Option<&Value>) -> Option<String> {
    let node = node?;
    let thumbs = node
        .get("thumbnails")
        .and_then(Value::as_array)
        .or_else(|| node.as_array())?;
    let best = thumbs
        .iter()
        .enumerate()
        .filter(|(_, t)| t.get("url").and_then(Value::as_str).is_some())
        .max_by_key(|(idx, t)| {
            let w = t.get("width").and_then(Value::as_u64).unwrap_or(0);
            let h = t.get("height").and_then(Value::as_u64).unwrap_or(0);
            (w.max(h), *idx)
        })
        .map(|(_, t)| t)
        .or_else(|| thumbs.last())?;
    best.get("url")
        .and_then(Value::as_str)
        .map(upgrade_thumbnail_url)
}

pub(crate) fn walk_renderers<'a>(node: &'a Value, renderer_name: &str) -> Vec<&'a Value> {
    let mut found = Vec::new();
    walk_renderers_rec(node, renderer_name, &mut found);
    found
}

fn is_ad_container_key(key: &str) -> bool {
    matches!(
        key,
        "adSlotRenderer"
            | "inFeedAdLayoutRenderer"
            | "promotedSparklesWebRenderer"
            | "promotedVideoRenderer"
            | "compactPromotedVideoRenderer"
            | "searchPyvRenderer"
            | "displayAdRenderer"
            | "feedAdRenderer"
            | "adBreakServiceRenderer"
            | "actionCompanionAdRenderer"
            | "bannerPromoRenderer"
            | "statementBannerRenderer"
            | "brandVideoSingletonRenderer"
            | "brandVideoShelfRenderer"
            | "fullWidthPortraitAdRenderer"
            | "adLayoutMetadata"
            | "adBadgeViewModel"
            | "adDetailsViewModel"
            | "videoDisplayFullButtonedAdRenderer"
            | "videoDisplayButtonGroupRenderer"
    ) || key.ends_with("AdRenderer")
        || key.ends_with("AdLayoutRenderer")
        || key.starts_with("promoted")
}

fn is_ad_badge_text(text: &str) -> bool {
    let trimmed = text.trim().to_ascii_lowercase();
    matches!(
        trimmed.as_str(),
        "ad" | "anuncio" | "sponsored" | "patrocinado" | "promoted" | "promocionado"
    )
}

fn badge_node_is_ad(node: &Value) -> bool {
    match node {
        Value::Object(map) => {
            for key in ["style", "badgeStyle"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    let up = s.to_ascii_uppercase();
                    if up.contains("AD_BADGE") || up.contains("SPONSORED") {
                        return true;
                    }
                }
            }
            for key in ["label", "text"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    if is_ad_badge_text(s) {
                        return true;
                    }
                }
            }
            map.values().any(badge_node_is_ad)
        }
        Value::Array(arr) => arr.iter().any(badge_node_is_ad),
        _ => false,
    }
}

fn is_ad_renderer(node: &Value) -> bool {
    if let Some(map) = node.as_object() {
        for key in [
            "adSlotMetadata",
            "adBadge",
            "adBadgeViewModel",
            "adDetailsViewModel",
            "adLayoutMetadata",
        ] {
            if map.contains_key(key) {
                return true;
            }
        }
        if map.get("isAd").and_then(Value::as_bool) == Some(true) {
            return true;
        }
        if let Some(badges) = map.get("badges") {
            if badge_node_is_ad(badges) {
                return true;
            }
        }
    }
    for key in [
        "metadataBadgeRenderer",
        "badgeViewModel",
        "thumbnailBadgeViewModel",
    ] {
        for badge in walk_renderers(node, key) {
            if badge_node_is_ad(badge) {
                return true;
            }
        }
    }
    false
}

fn extract_duration_and_live(node: &Value) -> (Option<u64>, Option<String>, bool) {
    // 1. Classic videoRenderer lengthText
    if let Some(raw) = text_from_node(node.get("lengthText")) {
        let trimmed = raw.trim().to_string();
        if let Some(secs) = parse_duration_text(&trimmed) {
            return (Some(secs), Some(trimmed), false);
        }
    }

    // 2. Classic thumbnailOverlays -> thumbnailOverlayTimeStatusRenderer
    let mut saw_live = false;
    for overlay in walk_renderers(node, "thumbnailOverlayTimeStatusRenderer") {
        if let Some(style) = overlay.get("style").and_then(Value::as_str) {
            if style.to_ascii_uppercase().contains("LIVE") {
                saw_live = true;
            }
        }
        if let Some(raw) = text_from_node(overlay.get("text")) {
            let trimmed = raw.trim().to_string();
            if let Some(secs) = parse_duration_text(&trimmed) {
                return (Some(secs), Some(trimmed), false);
            }
            let up = trimmed.to_ascii_uppercase();
            if up == "LIVE" || up == "EN VIVO" || up == "DIRECTO" {
                saw_live = true;
            }
        }
    }

    // 3. Modern lockupViewModel -> scan ALL thumbnailBadgeViewModel overlays
    for badge in walk_renderers(node, "thumbnailBadgeViewModel") {
        if let Some(style) = badge.get("badgeStyle").and_then(Value::as_str) {
            if style.to_ascii_uppercase().contains("LIVE") {
                saw_live = true;
            }
        }
        if let Some(raw) = badge.get("text").and_then(Value::as_str) {
            let trimmed = raw.trim().to_string();
            if let Some(secs) = parse_duration_text(&trimmed) {
                return (Some(secs), Some(trimmed), false);
            }
            let up = trimmed.to_ascii_uppercase();
            if up == "LIVE" || up == "EN VIVO" || up == "DIRECTO" {
                saw_live = true;
            }
        }
    }

    // 4. Fallback live detection via metadataBadgeRenderer
    if !saw_live {
        if let Some(arr) = node.get("badges").and_then(Value::as_array) {
            saw_live = arr.iter().any(|b| {
                b.pointer("/metadataBadgeRenderer/style")
                    .and_then(Value::as_str)
                    .map(|s| s.to_ascii_uppercase().contains("LIVE"))
                    .unwrap_or(false)
            });
        }
    }

    (None, None, saw_live)
}

fn walk_renderers_rec<'a>(node: &'a Value, renderer_name: &str, out: &mut Vec<&'a Value>) {
    match node {
        Value::Object(map) => {
            if let Some(val) = map.get(renderer_name) {
                if val.is_object() {
                    out.push(val);
                }
            }
            for (k, v) in map {
                if is_ad_container_key(k) {
                    continue;
                }
                walk_renderers_rec(v, renderer_name, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                walk_renderers_rec(v, renderer_name, out);
            }
        }
        _ => {}
    }
}

fn is_members_only_text(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("members only")
        || lower.contains("members-only")
        || lower.contains("members first")
        || lower.contains("solo para miembros")
        || lower.contains("solo miembros")
        || lower.contains("miembros del canal")
        || lower.contains("join this channel")
}

fn badge_node_is_members_only(node: &Value) -> bool {
    match node {
        Value::Object(map) => {
            for key in ["style", "badgeStyle"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    if s.to_ascii_uppercase().contains("MEMBERS_ONLY") {
                        return true;
                    }
                }
            }
            for key in ["iconType", "iconName"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    let up = s.to_ascii_uppercase();
                    if up.contains("SPONSORSHIP") || up.contains("MEMBERS_ONLY") {
                        return true;
                    }
                }
            }
            for key in ["label", "text", "accessibilityLabel"] {
                if let Some(s) = map.get(key).and_then(Value::as_str) {
                    if is_members_only_text(s) {
                        return true;
                    }
                }
            }
            map.values().any(badge_node_is_members_only)
        }
        Value::Array(arr) => arr.iter().any(badge_node_is_members_only),
        _ => false,
    }
}

fn is_members_only_renderer(node: &Value) -> bool {
    if let Some(badges) = node.get("badges") {
        if badge_node_is_members_only(badges) {
            return true;
        }
    }
    if let Some(overlays) = node.get("thumbnailOverlays") {
        if badge_node_is_members_only(overlays) {
            return true;
        }
    }
    for key in [
        "metadataBadgeRenderer",
        "badgeViewModel",
        "thumbnailBadgeViewModel",
    ] {
        for badge in walk_renderers(node, key) {
            if badge_node_is_members_only(badge) {
                return true;
            }
        }
    }
    false
}

fn is_members_only_playability(data: &Value) -> bool {
    let Some(ps) = data.get("playabilityStatus") else {
        return false;
    };
    if let Some(reason) = ps.get("reason").and_then(Value::as_str) {
        if is_members_only_text(reason) {
            return true;
        }
    }
    if let Some(sub) = ps
        .pointer("/errorScreen/playerErrorMessageRenderer/subreason")
        .and_then(|v| text_from_node(Some(v)))
    {
        if is_members_only_text(&sub) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_members_only_from_video_renderer_badges() {
        let members_renderer = json!({
            "videoId": "v5rmK9i1jI8",
            "title": { "runs": [{ "text": "Exclusive stream" }] },
            "badges": [
                {
                    "metadataBadgeRenderer": {
                        "style": "BADGE_STYLE_TYPE_MEMBERS_ONLY",
                        "label": "Members only",
                        "icon": { "iconType": "SPONSORSHIP_STAR" }
                    }
                }
            ]
        });
        let item = parse_video_renderer(&members_renderer, Some("Channel"), None, false)
            .expect("should parse members video");
        assert!(item.members_only);

        let normal_renderer = json!({
            "videoId": "dQw4w9WgXcQ",
            "title": { "runs": [{ "text": "Members only reaction (public video)" }] },
            "badges": []
        });
        let normal = parse_video_renderer(&normal_renderer, Some("Channel"), None, false)
            .expect("should parse normal video");
        assert!(!normal.members_only);
    }

    #[test]
    fn detects_members_only_from_lockup_view_model() {
        let data = json!({
            "lockupViewModel": {
                "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                "contentId": "v5rmK9i1jI8",
                "metadata": {
                    "lockupMetadataViewModel": {
                        "title": { "content": "Video exclusivo" },
                        "metadata": {
                            "contentMetadataViewModel": {
                                "metadataRows": [
                                    { "metadataParts": [{ "text": { "content": "Canal" } }] },
                                    {
                                        "metadataParts": [{
                                            "badgeViewModel": {
                                                "badgeStyle": "BADGE_STYLE_TYPE_MEMBERS_ONLY",
                                                "label": "Solo para miembros"
                                            }
                                        }]
                                    }
                                ]
                            }
                        }
                    }
                }
            }
        });
        let items = parse_lockup_videos(&data);
        assert_eq!(items.len(), 1);
        assert!(items[0].members_only);
    }

    #[test]
    fn extracts_duration_from_non_first_lockup_overlay_and_time_status_overlay() {
        let lockup_data = json!({
            "lockupViewModel": {
                "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                "contentId": "abcDEF12345",
                "contentImage": {
                    "thumbnailViewModel": {
                        "overlays": [
                            { "thumbnailBottomOverlayViewModel": { "progressBar": { "percentDurationWatched": 40 } } },
                            {
                                "thumbnailOverlayBadgeViewModel": {
                                    "thumbnailBadges": [
                                        { "thumbnailBadgeViewModel": { "text": "HD" } },
                                        { "thumbnailBadgeViewModel": { "text": "14:27" } }
                                    ]
                                }
                            }
                        ]
                    }
                },
                "metadata": {
                    "lockupMetadataViewModel": {
                        "title": { "content": "Video con barra de progreso" }
                    }
                }
            }
        });
        let lockups = parse_lockup_videos(&lockup_data);
        assert_eq!(lockups.len(), 1);
        assert_eq!(lockups[0].duration, Some(14 * 60 + 27));
        assert_eq!(lockups[0].duration_text, "14:27");
        assert_eq!(lockups[0].watched_percent, Some(40));

        let overlay_only_renderer = json!({
            "videoId": "xyz98765432",
            "title": { "simpleText": "Grid video without lengthText" },
            "thumbnailOverlays": [
                {
                    "thumbnailOverlayTimeStatusRenderer": {
                        "text": { "simpleText": "1:02:05" },
                        "style": "DEFAULT"
                    }
                }
            ]
        });
        let item = parse_video_renderer(&overlay_only_renderer, None, None, false).unwrap();
        assert_eq!(item.duration, Some(3725));
        assert_eq!(item.duration_text, "1:02:05");
    }

    #[test]
    fn filters_out_ads_from_search_and_lockup_feeds() {
        let feed = json!({
            "contents": [
                {
                    "adSlotRenderer": {
                        "fulfillmentContent": {
                            "fulfilledLayout": {
                                "inFeedAdLayoutRenderer": {
                                    "renderingContent": {
                                        "lockupViewModel": {
                                            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                                            "contentId": "ADvid000001",
                                            "metadata": { "lockupMetadataViewModel": { "title": { "content": "Sponsored Product" } } }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
                {
                    "promotedVideoRenderer": {
                        "videoId": "ADvid000002",
                        "title": { "simpleText": "Promoted Ad" }
                    }
                },
                {
                    "videoRenderer": {
                        "videoId": "ADvid000003",
                        "title": { "simpleText": "Badged Ad" },
                        "badges": [
                            { "metadataBadgeRenderer": { "label": "Sponsored" } }
                        ]
                    }
                },
                {
                    "videoRenderer": {
                        "videoId": "REALvid0001",
                        "title": { "simpleText": "Real Video" },
                        "lengthText": { "simpleText": "8:15" }
                    }
                }
            ]
        });

        let search_items = parse_search_items(&feed, 20);
        assert_eq!(search_items.len(), 1);
        assert_eq!(search_items[0].id, "REALvid0001");
        assert_eq!(search_items[0].duration_text, "8:15");

        let lockup_items = parse_lockup_videos(&feed);
        assert!(lockup_items.is_empty(), "adSlotRenderer lockup must be filtered out");
    }

    #[test]
    fn extracts_channel_url_and_recommended_mixes_from_lockups() {
        let feed = json!({
            "contents": [
                {
                    "lockupViewModel": {
                        "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                        "contentId": "dQw4w9WgXcQ",
                        "contentImage": {
                            "thumbnailViewModel": {
                                "image": { "sources": [{ "url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg" }] }
                            }
                        },
                        "metadata": {
                            "lockupMetadataViewModel": {
                                "title": { "content": "Never Gonna Give You Up" },
                                "image": {
                                    "decoratedAvatarViewModel": {
                                        "rendererContext": {
                                            "commandContext": {
                                                "onTap": {
                                                    "innertubeCommand": {
                                                        "browseEndpoint": {
                                                            "browseId": "UCuAXFkgsw1L7xaCfnd5JJOw",
                                                            "canonicalBaseUrl": "/@RickAstleyYT"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                },
                                "metadata": {
                                    "contentMetadataViewModel": {
                                        "metadataRows": [{
                                            "metadataParts": [{ "text": { "content": "Rick Astley" } }]
                                        }]
                                    }
                                }
                            }
                        }
                    }
                },
                {
                    "lockupViewModel": {
                        "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST",
                        "contentId": "RDMM",
                        "rendererContext": {
                            "commandContext": {
                                "onTap": {
                                    "innertubeCommand": {
                                        "watchEndpoint": {
                                            "videoId": "dQw4w9WgXcQ",
                                            "playlistId": "RDMM"
                                        }
                                    }
                                }
                            }
                        },
                        "contentImage": {
                            "collectionThumbnailViewModel": {
                                "primaryThumbnail": {
                                    "thumbnailViewModel": {
                                        "image": { "sources": [{ "url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg" }] }
                                    }
                                }
                            }
                        },
                        "metadata": {
                            "lockupMetadataViewModel": {
                                "title": { "content": "My Mix" },
                                "metadata": {
                                    "contentMetadataViewModel": {
                                        "metadataRows": [{
                                            "metadataParts": [{ "text": { "content": "Rick Astley, a-ha and more" } }]
                                        }]
                                    }
                                }
                            }
                        }
                    }
                }
            ]
        });

        let videos = parse_lockup_videos(&feed);
        assert_eq!(videos.len(), 1);
        assert_eq!(
            videos[0].channel_url.as_deref(),
            Some("https://www.youtube.com/channel/UCuAXFkgsw1L7xaCfnd5JJOw")
        );

        let mixes = parse_recommended_mixes(&feed);
        assert_eq!(mixes.len(), 1);
        assert_eq!(mixes[0].id, "RDMM");
        assert_eq!(mixes[0].result_type.as_deref(), Some("mix"));
        assert_eq!(mixes[0].url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=RDMM");
    }

    #[test]
    fn upgrades_low_res_music_thumbnails_and_picks_largest() {
        assert_eq!(
            upgrade_thumbnail_url("https://lh3.googleusercontent.com/abc=w120-h120-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w544-h544-l90-rj"
        );
        assert_eq!(
            upgrade_thumbnail_url("https://lh3.googleusercontent.com/abc=s88-c-k-no-rj"),
            "https://lh3.googleusercontent.com/abc=s544-c-k-no-rj"
        );
        assert_eq!(
            upgrade_thumbnail_url("https://lh3.googleusercontent.com/abc=w800-h800-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w800-h800-l90-rj"
        );
        // Non-square thumbnails keep their original aspect ratio
        assert_eq!(
            upgrade_thumbnail_url("https://lh3.googleusercontent.com/abc=w320-h180-l90-rj"),
            "https://lh3.googleusercontent.com/abc=w320-h180-l90-rj"
        );

        let node = json!({
            "thumbnails": [
                {"url": "https://lh3.googleusercontent.com/xyz=w226-h226-l90-rj", "width": 226, "height": 226},
                {"url": "https://lh3.googleusercontent.com/xyz=w60-h60-l90-rj", "width": 60, "height": 60}
            ]
        });
        assert_eq!(
            last_thumbnail_url(Some(&node)).as_deref(),
            Some("https://lh3.googleusercontent.com/xyz=w544-h544-l90-rj")
        );
    }

    #[test]
    fn parses_chapters_from_description_timestamps() {
        let desc = "Check out this video!\n0:00 Intro\n01:30 - Main Topic\n[05:45] Deep Dive\n1:02:10 Outro & Summary\n";
        let chapters = parse_chapters_from_description(desc, Some(4000.0));
        assert_eq!(chapters.len(), 4);
        assert_eq!(chapters[0].title, "Intro");
        assert_eq!(chapters[0].start_time, 0.0);
        assert_eq!(chapters[0].end_time, Some(90.0));
        assert_eq!(chapters[1].title, "Main Topic");
        assert_eq!(chapters[1].start_time, 90.0);
        assert_eq!(chapters[2].title, "Deep Dive");
        assert_eq!(chapters[2].start_time, 345.0);
        assert_eq!(chapters[3].title, "Outro & Summary");
        assert_eq!(chapters[3].start_time, 3730.0);
        assert_eq!(chapters[3].end_time, Some(4000.0));
    }

    #[test]
    fn parses_youtube_storyboard_spec() {
        let spec = "https://i.ytimg.com/sb/abc12345678/storyboard3_L$L/$N.jpg?sqp=test|48#27#100#10#10#0#default#rs1|80#45#100#10#10#2000#M$M#rs2|160#90#100#5#5#2000#M$M#rs3";
        let sb = parse_storyboard_spec(spec, Some(200.0)).expect("storyboard parsed");
        assert_eq!(sb.frame_width, 160);
        assert_eq!(sb.frame_height, 90);
        assert_eq!(sb.frame_count, 100);
        assert_eq!(sb.cols, 5);
        assert_eq!(sb.rows, 5);
        assert_eq!(sb.interval_ms, 2000);
        assert!(sb.template_url.contains("storyboard3_L2/M$M.jpg"));
        assert!(sb.template_url.contains("sigh=rs3"));
    }

    #[test]
    fn builds_safe_timedtext_vtt_url_with_translation() {
        let url = build_timedtext_vtt_url(
            "https://www.youtube.com/api/timedtext?v=dQw4w9WgXcQ&lang=en&fmt=srv3",
            Some("es"),
        )
        .expect("valid timedtext url");
        assert!(url.contains("fmt=vtt"));
        assert!(url.contains("tlang=es"));
        assert!(!url.contains("fmt=srv3"));

        assert!(build_timedtext_vtt_url("https://evil.example.com/api/timedtext?v=1", None).is_err());
    }

    #[test]
    fn parses_modern_entity_comments_response() {
        let resp = json!({
            "onResponseReceivedEndpoints": [
                {
                    "reloadContinuationItemsCommand": {
                        "continuationItems": [
                            {
                                "commentThreadRenderer": {
                                    "commentViewModel": {
                                        "commentViewModel": {
                                            "commentKey": "ck_1",
                                            "pinnedText": "Fijado por Creador"
                                        }
                                    },
                                    "replies": {
                                        "commentRepliesRenderer": {
                                            "contents": [{
                                                "continuationItemRenderer": {
                                                    "continuationEndpoint": {
                                                        "continuationCommand": { "token": "rep_tok_1" }
                                                    }
                                                }
                                            }]
                                        }
                                    }
                                }
                            },
                            {
                                "continuationItemRenderer": {
                                    "continuationEndpoint": {
                                        "continuationCommand": { "token": "next_page_tok" }
                                    }
                                }
                            }
                        ]
                    }
                }
            ],
            "frameworkUpdates": {
                "entityBatchUpdate": {
                    "mutations": [
                        {
                            "entityKey": "ck_1",
                            "payload": {
                                "commentEntityPayload": {
                                    "properties": {
                                        "commentId": "Ugx123",
                                        "content": { "content": "Excelente video 01:20!" },
                                        "publishedTime": "hace 2 días",
                                        "replyLevel": 0
                                    },
                                    "author": {
                                        "displayName": "@tester",
                                        "avatarThumbnailUrl": "https://yt3.ggpht.com/avatar.jpg",
                                        "channelUrl": "/@tester",
                                        "isCreator": true
                                    },
                                    "toolbar": {
                                        "likeCountLiked": "42",
                                        "likeCountNotliked": "41",
                                        "replyCount": "3"
                                    }
                                }
                            }
                        }
                    ]
                }
            }
        });

        let page = parse_comments_response(&resp);
        assert_eq!(page.comments.len(), 1);
        assert_eq!(page.comments[0].author, "@tester");
        assert_eq!(page.comments[0].text, "Excelente video 01:20!");
        assert!(page.comments[0].is_channel_owner);
        assert!(page.comments[0].is_pinned);
        assert_eq!(page.comments[0].like_count_text, "41");
        assert_eq!(page.comments[0].reply_count, 3);
        assert_eq!(page.comments[0].replies_continuation.as_deref(), Some("rep_tok_1"));
        assert_eq!(page.next_continuation.as_deref(), Some("next_page_tok"));
    }
}

fn parse_video_details(video_id: &str, player_data: &Value, next_data: &Value) -> VideoDetails {
    let video_details = player_data.get("videoDetails");
    let microformat = player_data
        .get("microformat")
        .and_then(|m| m.get("playerMicroformatRenderer"));

    let duration_secs = video_details
        .and_then(|d| d.get("lengthSeconds"))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        });

    let publish_date = microformat
        .and_then(|m| m.get("publishDate").or_else(|| m.get("uploadDate")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let view_count = video_details
        .and_then(|d| d.get("viewCount"))
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .or_else(|| v.as_u64())
        });

    let short_desc = video_details
        .and_then(|d| d.get("shortDescription"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let (next_desc, publish_date_text, relative_date_text, view_count_text, like_count_text) =
        extract_primary_and_secondary_info(next_data);

    let description = if next_desc.trim().len() >= short_desc.trim().len() && !next_desc.is_empty() {
        next_desc
    } else {
        short_desc
    };

    let mut chapters = extract_chapters_from_next(next_data, duration_secs);
    if chapters.is_empty() {
        chapters = parse_chapters_from_description(&description, duration_secs);
    }

    let (caption_tracks, translation_languages) = parse_caption_tracks(player_data);

    let storyboard = player_data
        .get("storyboards")
        .and_then(|s| s.get("playerStoryboardSpecRenderer"))
        .and_then(|r| r.get("spec"))
        .and_then(Value::as_str)
        .and_then(|spec| parse_storyboard_spec(spec, duration_secs));

    let (comments_continuation, comments_sort_options) = extract_comments_entry(next_data);

    VideoDetails {
        video_id: video_id.to_string(),
        description,
        publish_date,
        publish_date_text,
        relative_date_text,
        view_count,
        view_count_text,
        like_count_text,
        chapters,
        caption_tracks,
        translation_languages,
        storyboard,
        comments_continuation,
        comments_sort_options,
    }
}

fn extract_text_runs(val: Option<&Value>) -> Option<String> {
    let v = val?;
    if let Some(s) = v.as_str() {
        let t = s.trim();
        return (!t.is_empty()).then(|| t.to_string());
    }
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        let t = s.trim();
        return (!t.is_empty()).then(|| t.to_string());
    }
    if let Some(s) = v.get("content").and_then(Value::as_str) {
        let t = s.trim();
        return (!t.is_empty()).then(|| t.to_string());
    }
    if let Some(runs) = v.get("runs").and_then(Value::as_array) {
        let joined: String = runs
            .iter()
            .filter_map(|r| r.get("text").and_then(Value::as_str))
            .collect();
        if !joined.trim().is_empty() {
            return Some(joined);
        }
    }
    None
}

fn extract_primary_and_secondary_info(
    next_data: &Value,
) -> (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let mut description = String::new();
    let mut publish_date_text = None;
    let mut relative_date_text = None;
    let mut view_count_text = None;
    let mut like_count_text = None;

    let contents = next_data
        .pointer("/contents/twoColumnWatchNextResults/results/results/contents")
        .and_then(Value::as_array);

    if let Some(arr) = contents {
        for item in arr {
            if let Some(primary) = item.get("videoPrimaryInfoRenderer") {
                if publish_date_text.is_none() {
                    publish_date_text = extract_text_runs(primary.get("dateText"));
                }
                if relative_date_text.is_none() {
                    relative_date_text = extract_text_runs(primary.get("relativeDateText"));
                }
                if view_count_text.is_none() {
                    view_count_text = extract_text_runs(
                        primary.pointer("/viewCount/videoViewCountRenderer/viewCount"),
                    )
                    .or_else(|| {
                        extract_text_runs(
                            primary.pointer("/viewCount/videoViewCountRenderer/shortViewCount"),
                        )
                    });
                }
                if like_count_text.is_none() {
                    like_count_text = extract_like_count_text(primary);
                }
            }
            if let Some(secondary) = item.get("videoSecondaryInfoRenderer") {
                if description.is_empty() {
                    if let Some(content) = secondary
                        .pointer("/attributedDescription/content")
                        .and_then(Value::as_str)
                    {
                        description = content.to_string();
                    } else if let Some(d) = extract_text_runs(secondary.get("description")) {
                        description = d;
                    }
                }
            }
        }
    }

    (
        description,
        publish_date_text,
        relative_date_text,
        view_count_text,
        like_count_text,
    )
}

fn extract_like_count_text(primary: &Value) -> Option<String> {
    let buttons = primary
        .pointer("/videoActions/menuRenderer/topLevelButtons")
        .and_then(Value::as_array)?;
    for btn in buttons {
        if let Some(title) = btn
            .pointer("/segmentedLikeDislikeButtonViewModel/likeButtonViewModel/likeButtonViewModel/toggleButtonViewModel/toggleButtonViewModel/defaultButtonViewModel/buttonViewModel/title")
            .and_then(Value::as_str)
        {
            if !title.trim().is_empty() {
                return Some(title.trim().to_string());
            }
        }
        if let Some(t) = extract_text_runs(
            btn.pointer("/segmentedLikeDislikeButtonRenderer/likeButton/toggleButtonRenderer/defaultText"),
        ) {
            return Some(t);
        }
    }
    None
}

fn extract_chapters_from_next(next_data: &Value, duration_secs: Option<f64>) -> Vec<VideoChapter> {
    let mut chapters = Vec::new();
    let mut stack = vec![next_data];
    while let Some(node) = stack.pop() {
        match node {
            Value::Object(map) => {
                if let Some(ch) = map.get("chapterRenderer") {
                    let title = extract_text_runs(ch.get("title")).unwrap_or_default();
                    let start_ms = ch
                        .get("timeRangeStartMillis")
                        .and_then(|v| {
                            v.as_u64()
                                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                        });
                    if !title.is_empty() {
                        if let Some(ms) = start_ms {
                            chapters.push(VideoChapter {
                                title,
                                start_time: ms as f64 / 1000.0,
                                end_time: None,
                            });
                        }
                    }
                    continue;
                }
                for v in map.values() {
                    if v.is_object() || v.is_array() {
                        stack.push(v);
                    }
                }
            }
            Value::Array(arr) => {
                for v in arr.iter().rev() {
                    if v.is_object() || v.is_array() {
                        stack.push(v);
                    }
                }
            }
            _ => {}
        }
    }
    finalize_chapters(chapters, duration_secs)
}

fn finalize_chapters(mut chapters: Vec<VideoChapter>, duration_secs: Option<f64>) -> Vec<VideoChapter> {
    if chapters.is_empty() {
        return chapters;
    }
    chapters.sort_by(|a, b| {
        a.start_time
            .partial_cmp(&b.start_time)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    chapters.dedup_by(|a, b| (a.start_time - b.start_time).abs() < 0.5);
    for i in 0..chapters.len() {
        if i + 1 < chapters.len() {
            chapters[i].end_time = Some(chapters[i + 1].start_time);
        } else if let Some(dur) = duration_secs {
            if dur > chapters[i].start_time {
                chapters[i].end_time = Some(dur);
            }
        }
    }
    chapters
}

pub fn parse_chapters_from_description(
    description: &str,
    duration_secs: Option<f64>,
) -> Vec<VideoChapter> {
    static TS_RE: OnceLock<Regex> = OnceLock::new();
    let re = TS_RE.get_or_init(|| {
        Regex::new(r"(?:^|[\s(\[])(?:(\d{1,2}):)?(\d{1,2}):(\d{2})(?:[\s)\]]|$)").unwrap()
    });

    let mut chapters = Vec::new();
    for raw_line in description.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.len() > 240 {
            continue;
        }
        let Some(caps) = re.captures(line) else {
            continue;
        };
        let hours: u64 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let mins: u64 = caps
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        let secs: u64 = caps
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0);
        if mins >= 60 || secs >= 60 {
            continue;
        }
        let start = (hours * 3600 + mins * 60 + secs) as f64;
        if let Some(dur) = duration_secs {
            if dur > 0.0 && start >= dur {
                continue;
            }
        }
        let full_match = caps.get(0).unwrap();
        let before = line[..full_match.start()].trim();
        let after = line[full_match.end()..].trim();
        let raw_title = if !after.is_empty() { after } else { before };
        let title = raw_title
            .trim_matches(|c: char| matches!(c, '-' | '–' | '—' | ':' | '|' | '•' | '·' | ')' | ']' | ' '))
            .trim()
            .to_string();
        if title.is_empty() {
            continue;
        }
        chapters.push(VideoChapter {
            title,
            start_time: start,
            end_time: None,
        });
    }

    if chapters.len() < 2 {
        return Vec::new();
    }
    let finalized = finalize_chapters(chapters, duration_secs);
    // Require ascending timestamps starting near the beginning or at least 2 distinct entries
    if finalized.len() < 2 {
        return Vec::new();
    }
    finalized
}

pub fn parse_caption_tracks(player_data: &Value) -> (Vec<CaptionTrack>, Vec<TranslationLanguage>) {
    let renderer = match player_data
        .get("captions")
        .and_then(|c| c.get("playerCaptionsTracklistRenderer"))
    {
        Some(r) => r,
        None => return (Vec::new(), Vec::new()),
    };

    let tracks = renderer
        .get("captionTracks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let base_url = t.get("baseUrl").and_then(Value::as_str)?.trim().to_string();
                    if base_url.is_empty() {
                        return None;
                    }
                    let language_code = t
                        .get("languageCode")
                        .and_then(Value::as_str)
                        .unwrap_or("und")
                        .to_string();
                    let name = extract_text_runs(t.get("name"))
                        .unwrap_or_else(|| language_code.clone());
                    let is_auto = t.get("kind").and_then(Value::as_str) == Some("asr");
                    let is_translatable = t
                        .get("isTranslatable")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Some(CaptionTrack {
                        base_url,
                        language_code,
                        name,
                        is_auto,
                        is_translatable,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let translation_languages = renderer
        .get("translationLanguages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    let language_code = l
                        .get("languageCode")
                        .and_then(Value::as_str)?
                        .trim()
                        .to_string();
                    if language_code.is_empty() {
                        return None;
                    }
                    let name = extract_text_runs(l.get("languageName"))
                        .unwrap_or_else(|| language_code.clone());
                    Some(TranslationLanguage {
                        language_code,
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    (tracks, translation_languages)
}

pub fn parse_storyboard_spec(spec: &str, duration_secs: Option<f64>) -> Option<StoryboardSpec> {
    let parts: Vec<&str> = spec.split('|').collect();
    if parts.len() < 2 {
        return None;
    }
    let base_template = parts[0].trim();
    if !base_template.starts_with("http") {
        return None;
    }

    let mut best: Option<(usize, StoryboardSpec)> = None;
    for (level_idx, level_raw) in parts.iter().skip(1).enumerate() {
        let fields: Vec<&str> = level_raw.split('#').collect();
        if fields.len() < 8 {
            continue;
        }
        let frame_width: u32 = fields[0].parse().ok()?;
        let frame_height: u32 = fields[1].parse().ok()?;
        let frame_count: u32 = fields[2].parse().ok()?;
        let cols: u32 = fields[3].parse().ok()?;
        let rows: u32 = fields[4].parse().ok()?;
        let mut interval_ms: u32 = fields[5].parse().unwrap_or(0);
        let name_tpl = fields[6];
        let sigh = fields[7];

        if frame_width == 0 || frame_height == 0 || cols == 0 || rows == 0 || frame_count == 0 {
            continue;
        }
        if interval_ms == 0 {
            if let Some(dur) = duration_secs {
                if dur > 0.0 {
                    interval_ms = ((dur * 1000.0) / frame_count as f64).round().max(1000.0) as u32;
                }
            }
        }
        if interval_ms == 0 {
            interval_ms = 2000;
        }

        let mut url = base_template
            .replace("$L", &level_idx.to_string())
            .replace("$N", name_tpl);
        if !sigh.is_empty() {
            if url.contains('?') {
                url.push_str(&format!("&sigh={sigh}"));
            } else {
                url.push_str(&format!("?sigh={sigh}"));
            }
        }

        let candidate = StoryboardSpec {
            template_url: url,
            frame_width,
            frame_height,
            frame_count,
            cols,
            rows,
            interval_ms,
        };

        // Prefer ~160px width (standard YouTube hover preview), otherwise highest <= 320px
        let replace = match &best {
            None => true,
            Some((_, prev)) => {
                let prev_ok = prev.frame_width <= 240;
                let cand_ok = candidate.frame_width <= 240;
                (cand_ok && (!prev_ok || candidate.frame_width >= prev.frame_width))
                    || (!prev_ok && candidate.frame_width < prev.frame_width)
            }
        };
        if replace {
            best = Some((level_idx, candidate));
        }
    }

    best.map(|(_, s)| s)
}

fn extract_comments_entry(next_data: &Value) -> (Option<String>, Vec<CommentSortOption>) {
    let mut continuation = None;
    let mut sort_options = Vec::new();

    if let Some(contents) = next_data
        .pointer("/contents/twoColumnWatchNextResults/results/results/contents")
        .and_then(Value::as_array)
    {
        for item in contents {
            if let Some(sec) = item.get("itemSectionRenderer") {
                let sec_id = sec
                    .get("sectionIdentifier")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if sec_id == "comment-item-section" {
                    if let Some(tok) = sec
                        .pointer("/contents/0/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                        .and_then(Value::as_str)
                    {
                        continuation = Some(tok.to_string());
                    }
                }
            }
        }
    }

    if let Some(panels) = next_data.get("engagementPanels").and_then(Value::as_array) {
        for panel in panels {
            let panel_id = panel
                .pointer("/engagementPanelSectionListRenderer/panelIdentifier")
                .and_then(Value::as_str)
                .unwrap_or("");
            if panel_id == "engagement-panel-comments-section" {
                if let Some(sub_items) = panel
                    .pointer("/engagementPanelSectionListRenderer/header/engagementPanelTitleHeaderRenderer/menu/sortFilterSubMenuRenderer/subMenuItems")
                    .and_then(Value::as_array)
                {
                    for opt in sub_items {
                        let label = opt
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let tok = opt
                            .pointer("/serviceEndpoint/continuationCommand/token")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if !label.is_empty() {
                            if let Some(t) = tok {
                                sort_options.push(CommentSortOption {
                                    label,
                                    continuation: t,
                                });
                            }
                        }
                    }
                }
                if continuation.is_none() {
                    continuation = panel
                        .pointer("/engagementPanelSectionListRenderer/content/sectionListRenderer/contents/0/itemSectionRenderer/contents/0/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
        }
    }

    (continuation, sort_options)
}

pub fn parse_comments_response(data: &Value) -> CommentsPage {
    // Index modern commentEntityPayload mutations by entityKey
    let mut entity_map: HashMap<String, &Value> = HashMap::new();
    if let Some(mutations) = data
        .pointer("/frameworkUpdates/entityBatchUpdate/mutations")
        .and_then(Value::as_array)
    {
        for m in mutations {
            if let (Some(key), Some(payload)) = (
                m.get("entityKey").and_then(Value::as_str),
                m.pointer("/payload/commentEntityPayload"),
            ) {
                entity_map.insert(key.to_string(), payload);
            }
        }
    }

    let mut comments = Vec::new();
    let mut next_continuation = None;
    let mut total_count_text = None;

    let endpoints = data
        .get("onResponseReceivedEndpoints")
        .and_then(Value::as_array);
    if let Some(eps) = endpoints {
        for ep in eps {
            let items = ep
                .pointer("/reloadContinuationItemsCommand/continuationItems")
                .or_else(|| ep.pointer("/appendContinuationItemsAction/continuationItems"))
                .and_then(Value::as_array);

            if let Some(arr) = items {
                for entry in arr {
                    if let Some(hdr) = entry.get("commentsHeaderRenderer") {
                        if total_count_text.is_none() {
                            total_count_text = extract_text_runs(hdr.get("countText"))
                                .or_else(|| extract_text_runs(hdr.get("commentsCount")));
                        }
                        continue;
                    }
                    if let Some(tok) = entry
                        .pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                        .or_else(|| {
                            entry.pointer("/continuationItemRenderer/button/buttonRenderer/command/continuationCommand/token")
                        })
                        .and_then(Value::as_str)
                    {
                        next_continuation = Some(tok.to_string());
                        continue;
                    }
                    if let Some(thread) = entry.get("commentThreadRenderer") {
                        if let Some(c) = parse_comment_thread(thread, &entity_map) {
                            comments.push(c);
                        }
                        continue;
                    }
                    if let Some(vm) = entry.get("commentViewModel") {
                        if let Some(c) = parse_comment_view_model(vm, &entity_map, None) {
                            comments.push(c);
                        }
                        continue;
                    }
                    if let Some(cr) = entry.get("commentRenderer") {
                        if let Some(c) = parse_legacy_comment_renderer(cr, None) {
                            comments.push(c);
                        }
                    }
                }
            }
        }
    }

    CommentsPage {
        comments,
        next_continuation,
        total_count_text,
    }
}

fn parse_comment_thread(
    thread: &Value,
    entity_map: &HashMap<String, &Value>,
) -> Option<VideoComment> {
    let replies_continuation = thread
        .pointer("/replies/commentRepliesRenderer/contents/0/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
        .or_else(|| {
            thread.pointer("/replies/commentRepliesRenderer/contents/0/continuationItemRenderer/button/buttonRenderer/command/continuationCommand/token")
        })
        .and_then(Value::as_str)
        .map(str::to_string);

    if let Some(vm) = thread
        .pointer("/commentViewModel/commentViewModel")
        .or_else(|| thread.get("commentViewModel"))
    {
        return parse_comment_view_model(vm, entity_map, replies_continuation);
    }

    if let Some(cr) = thread.pointer("/comment/commentRenderer") {
        return parse_legacy_comment_renderer(cr, replies_continuation);
    }

    None
}

fn parse_comment_view_model(
    vm: &Value,
    entity_map: &HashMap<String, &Value>,
    replies_continuation: Option<String>,
) -> Option<VideoComment> {
    let inner = vm.get("commentViewModel").unwrap_or(vm);
    let key = inner.get("commentKey").and_then(Value::as_str)?;
    let payload = entity_map.get(key)?;

    let props = payload.get("properties")?;
    let author_obj = payload.get("author");
    let toolbar = payload.get("toolbar");

    let id = props
        .get("commentId")
        .and_then(Value::as_str)
        .unwrap_or(key)
        .to_string();
    let text = props
        .pointer("/content/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if text.trim().is_empty() {
        return None;
    }
    let published_time_text = props
        .get("publishedTime")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let author = author_obj
        .and_then(|a| a.get("displayName"))
        .and_then(Value::as_str)
        .unwrap_or("Usuario")
        .to_string();
    let author_avatar = author_obj
        .and_then(|a| a.get("avatarThumbnailUrl"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let author_channel_url = author_obj
        .and_then(|a| a.get("channelUrl"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|u| {
            if u.starts_with("http") {
                u.to_string()
            } else {
                format!("{DEFAULT_YT_BASE_URL}{u}")
            }
        });
    let is_channel_owner = author_obj
        .and_then(|a| a.get("isCreator"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_pinned = inner.get("pinnedText").is_some();
    let is_hearted = toolbar
        .and_then(|t| t.get("heartActiveTooltip"))
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    let like_count_text = toolbar
        .and_then(|t| t.get("likeCountNotliked").or_else(|| t.get("likeCountLiked")))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let reply_count = toolbar
        .and_then(|t| t.get("replyCount"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
        .unwrap_or(0);

    Some(VideoComment {
        id,
        author,
        author_avatar,
        author_channel_url,
        is_channel_owner,
        is_pinned,
        is_hearted,
        published_time_text,
        text,
        like_count_text,
        reply_count,
        replies_continuation,
    })
}

fn parse_legacy_comment_renderer(
    cr: &Value,
    replies_continuation: Option<String>,
) -> Option<VideoComment> {
    let id = cr
        .get("commentId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let text = extract_text_runs(cr.get("contentText"))?;
    let author = extract_text_runs(cr.get("authorText")).unwrap_or_else(|| "Usuario".into());
    let author_avatar = last_thumbnail_url(cr.get("authorThumbnail"));
    let author_channel_url = cr
        .pointer("/authorEndpoint/browseEndpoint/canonicalBaseUrl")
        .and_then(Value::as_str)
        .map(|u| format!("{DEFAULT_YT_BASE_URL}{u}"));
    let is_channel_owner = cr
        .get("authorIsChannelOwner")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let is_pinned = cr.get("pinnedCommentBadge").is_some();
    let is_hearted = cr
        .pointer("/actionButtons/commentActionButtonsRenderer/creatorHeart/creatorHeartRenderer/isHearted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let published_time_text = extract_text_runs(cr.get("publishedTimeText")).unwrap_or_default();
    let like_count_text = extract_text_runs(cr.get("voteCount")).unwrap_or_default();
    let reply_count = cr.get("replyCount").and_then(Value::as_u64).unwrap_or(0);

    Some(VideoComment {
        id,
        author,
        author_avatar,
        author_channel_url,
        is_channel_owner,
        is_pinned,
        is_hearted,
        published_time_text,
        text,
        like_count_text,
        reply_count,
        replies_continuation,
    })
}

pub fn build_timedtext_vtt_url(base_url: &str, tlang: Option<&str>) -> Result<String> {
    let mut parsed = reqwest::Url::parse(base_url.trim())
        .map_err(|_| CoreError::Validation("Invalid subtitle URL".into()))?;
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    let allowed_host = host == "www.youtube.com" || host == "youtube.com";
    if !allowed_host || !parsed.path().starts_with("/api/timedtext") {
        return Err(CoreError::Validation(
            "Subtitle URL must point to youtube.com/api/timedtext".into(),
        ));
    }

    let existing: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| k != "fmt" && k != "tlang")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    {
        let mut qp = parsed.query_pairs_mut();
        qp.clear();
        for (k, v) in existing {
            qp.append_pair(&k, &v);
        }
        qp.append_pair("fmt", "vtt");
        if let Some(lang) = tlang.map(str::trim).filter(|s| !s.is_empty()) {
            qp.append_pair("tlang", lang);
        }
    }

    Ok(parsed.to_string())
}

fn convert_xml_subtitles_to_vtt(xml: &str) -> Option<String> {
    static TEXT_RE: OnceLock<Regex> = OnceLock::new();
    let re = TEXT_RE.get_or_init(|| {
        Regex::new(r#"(?s)<text\s+start="([0-9.]+)"(?:\s+dur="([0-9.]+)")?[^>]*>(.*?)</text>"#)
            .unwrap()
    });
    let mut out = String::from("WEBVTT\n\n");
    let mut count = 0usize;
    for cap in re.captures_iter(xml) {
        let start: f64 = cap.get(1)?.as_str().parse().ok()?;
        let dur: f64 = cap
            .get(2)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(2.5);
        let raw_text = cap.get(3)?.as_str();
        let clean = decode_xml_entities(raw_text);
        if clean.trim().is_empty() {
            continue;
        }
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            format_vtt_ts(start),
            format_vtt_ts(start + dur.max(0.2)),
            clean.trim()
        ));
        count += 1;
    }
    (count > 0).then_some(out)
}

fn format_vtt_ts(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as u64;
    let h = total_ms / 3_600_000;
    let m = (total_ms % 3_600_000) / 60_000;
    let s = (total_ms % 60_000) / 1000;
    let ms = total_ms % 1000;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

fn decode_xml_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod subscription_tests {
    use super::*;

    #[test]
    fn parses_fechannels_and_guide_subscriptions() {
        let fechannels = json!({
            "contents": {
                "twoColumnBrowseResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "itemSectionRenderer": {
                                            "contents": [{
                                                "shelfRenderer": {
                                                    "content": {
                                                        "expandedShelfContentsRenderer": {
                                                            "items": [{
                                                                "channelRenderer": {
                                                                    "channelId": "UC1234567890abcdefghijkl",
                                                                    "title": { "simpleText": "Veritasium" },
                                                                    "navigationEndpoint": {
                                                                        "browseEndpoint": {
                                                                            "browseId": "UC1234567890abcdefghijkl",
                                                                            "canonicalBaseUrl": "/@veritasium"
                                                                        }
                                                                    },
                                                                    "thumbnail": {
                                                                        "thumbnails": [{
                                                                            "url": "//yt3.googleusercontent.com/abc=s88-c-k-c0x00ffffff-no-rj",
                                                                            "width": 88,
                                                                            "height": 88
                                                                        }]
                                                                    },
                                                                    "subscriberCountText": { "simpleText": "15M suscriptores" }
                                                                }
                                                            }]
                                                        }
                                                    }
                                                }
                                            }]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });

        let parsed = parse_subscribed_channels(&fechannels);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].channel_id, "UC1234567890abcdefghijkl");
        assert_eq!(parsed[0].name, "Veritasium");
        assert_eq!(parsed[0].handle, "@veritasium");
        assert_eq!(
            parsed[0].url,
            "https://www.youtube.com/channel/UC1234567890abcdefghijkl/videos"
        );
        assert!(parsed[0].thumbnail.starts_with("https://yt3.googleusercontent.com/"));
        assert_eq!(parsed[0].subscriber_count.as_deref(), Some("15M suscriptores"));

        let guide = json!({
            "items": [{
                "guideSubscriptionsSectionRenderer": {
                    "items": [{
                        "guideEntryRenderer": {
                            "navigationEndpoint": {
                                "browseEndpoint": {
                                    "browseId": "UC9999999999abcdefghijkl",
                                    "canonicalBaseUrl": "/@dotcsv"
                                }
                            },
                            "formattedTitle": { "simpleText": "Dot CSV" },
                            "thumbnail": {
                                "thumbnails": [{ "url": "https://yt3.ggpht.com/avatar.jpg" }]
                            }
                        }
                    }]
                }
            }]
        });
        let guide_parsed = parse_guide_subscriptions(&guide);
        assert_eq!(guide_parsed.len(), 1);
        assert_eq!(guide_parsed[0].channel_id, "UC9999999999abcdefghijkl");
        assert_eq!(guide_parsed[0].name, "Dot CSV");
        assert_eq!(guide_parsed[0].handle, "@dotcsv");
    }
}
