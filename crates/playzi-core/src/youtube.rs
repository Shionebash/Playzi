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
const DEFAULT_WEB_CLIENT_VERSION: &str = "2.20250101.00.00";
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

#[derive(Debug, Clone)]
pub struct YouTubeClient {
    http: reqwest::Client,
    base_url: String,
    hl: String,
    gl: String,
    search_cache: Arc<TtlCache<Vec<MediaItem>>>,
    metadata_cache: Arc<TtlCache<MediaItem>>,
    browse_cache: Arc<TtlCache<ChannelBrowseResult>>,
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
        self.post_innertube_with_cookies(endpoint, body, None).await
    }

    async fn post_innertube_with_cookies(
        &self,
        endpoint: &str,
        mut body: Value,
        cookies: Option<&CookieJar>,
    ) -> Result<Value> {
        if let Some(obj) = body.as_object_mut() {
            obj.insert("context".into(), self.web_context());
        }
        let url = format!("{}/youtubei/v1/{}?prettyPrint=false", self.base_url, endpoint);
        let mut req = self.http.post(url).json(&body);
        if let Some(jar) = cookies {
            if let Some(headers) = jar.ytmusic_auth_headers(&self.base_url) {
                for (k, v) in headers {
                    req = req.header(k, v);
                }
            } else {
                let raw_cookie = jar.cookie_header_all();
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

        let item = MediaItem {
            url: format!("https://www.youtube.com/watch?v={id}"),
            id,
            title,
            channel,
            channel_url: channel_id.map(|cid| format!("https://www.youtube.com/channel/{cid}")),
            duration,
            duration_text: format_duration(duration),
            thumbnail,
            view_count,
            live,
            members_only: is_members_only_playability(&data),
            result_type: None,
        };

        self.metadata_cache.set(trimmed.to_string(), item.clone());
        Ok(item)
    }

    /// Fetches YouTube home recommendations (`FEwhat_to_watch`) via InnerTube `/youtubei/v1/browse`.
    pub async fn fetch_recommendations(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        self.fetch_recommendations_with_cookies(None, store).await
    }

    pub async fn fetch_recommendations_with_cookies(
        &self,
        cookies: Option<&CookieJar>,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        let data = self
            .post_innertube_with_cookies(
                "browse",
                json!({ "browseId": "FEwhat_to_watch" }),
                cookies,
            )
            .await?;
        let mut items = parse_channel_videos(&data, "", "");
        let mut seen: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
        for item in parse_lockup_videos(&data) {
            if seen.insert(item.id.clone()) {
                items.push(item);
            }
        }
        let limited: Vec<MediaItem> = items.into_iter().take(40).collect();

        if let Some(st) = store {
            if !limited.is_empty() {
                let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                let serialized: Vec<Value> = limited
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

        Ok(limited)
    }

    /// Fetches YouTube Radio Mix (`RD<videoId>`) items via `/youtubei/v1/next`, skipping the seed video.
    pub async fn fetch_radio_items(&self, url: &str, limit: usize) -> Result<Vec<MediaItem>> {
        let Some(video_id) = extract_video_id(url) else {
            return Ok(Vec::new());
        };
        let playlist_id = format!("RD{video_id}");
        let data = self
            .post_innertube(
                "next",
                json!({
                    "videoId": video_id,
                    "playlistId": playlist_id,
                }),
            )
            .await?;

        let mut items = Vec::new();
        let mut seen = HashSet::new();
        seen.insert(video_id);

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
        let base = resolve_channel_base(url)?;

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

        let all_items = if tab == "playlists" {
            parse_channel_playlists(&data)
        } else {
            parse_channel_videos(&data, &name, &channel_id)
        };

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
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)(youtube\.com|youtu\.be|music\.youtube\.com)").unwrap());
    re.is_match(value)
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
        "shorts" => Some("EgZzaG9ydHPyBgUKA5oBAA%3D%3D"),
        "streams" => Some("EgdzdHJlYW1z8gYECgJ6AA%3D%3D"),
        "playlists" => Some("EglwbGF5bGlzdHPyBgQKAkIA"),
        "releases" => Some("EghyZWxlYXNlc_IGBQoDsgEA"),
        _ => None,
    }
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
    if !handle.is_empty() {
        let h = handle.trim_start_matches('@').to_ascii_lowercase();
        targets.insert(format!("https://www.youtube.com/@{h}"));
    }
    for ch in &state.channels {
        let url = ch.get("url").and_then(Value::as_str).unwrap_or("");
        let resolved = resolve_channel_base(url)
            .unwrap_or_else(|_| url.to_string())
            .to_ascii_lowercase();
        if targets.contains(&resolved) {
            return ch.get("id").and_then(Value::as_str).map(ToString::to_string);
        }
    }
    None
}

fn parse_search_items(data: &Value, limit: usize) -> Vec<MediaItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for renderer in walk_renderers(data, "videoRenderer") {
        if let Some(item) = parse_video_renderer(renderer, None, None) {
            if seen.insert(item.id.clone()) {
                items.push(item);
                if items.len() >= limit {
                    return items;
                }
            }
        }
    }
    items
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
        for renderer in walk_renderers(data, key) {
            if let Some(item) =
                parse_video_renderer(renderer, fallback_channel, fallback_url.as_deref())
            {
                if seen.insert(item.id.clone()) {
                    items.push(item);
                }
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
        let (duration, duration_text_raw, live) = extract_duration_and_live(lockup);
        let thumbnail = lockup
            .pointer("/contentImage/thumbnailViewModel/image/sources")
            .and_then(Value::as_array)
            .and_then(|arr| arr.last())
            .and_then(|s| s.get("url"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(default_thumbnail(video_id)));

        items.push(MediaItem {
            id: video_id.to_string(),
            url: format!("https://www.youtube.com/watch?v={video_id}"),
            title,
            channel,
            channel_url: None,
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
            members_only: is_members_only_renderer(lockup),
            result_type: None,
        });
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
            });
        }
    }
    items
}

fn parse_video_renderer(
    renderer: &Value,
    fallback_channel: Option<&str>,
    fallback_channel_url: Option<&str>,
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

    let (duration, duration_raw, live) = extract_duration_and_live(renderer);
    let view_count = text_from_node(renderer.get("viewCountText"))
        .as_deref()
        .and_then(parse_view_count);

    let thumbnail =
        last_thumbnail_url(renderer.get("thumbnail")).or_else(|| Some(default_thumbnail(video_id)));

    Some(MediaItem {
        id: video_id.to_string(),
        url: format!("https://www.youtube.com/watch?v={video_id}"),
        title,
        channel,
        channel_url,
        duration,
        duration_text: duration_raw.unwrap_or_else(|| {
            if live {
                "EN VIVO".into()
            } else {
                format_duration(duration)
            }
        }),
        thumbnail,
        view_count,
        live,
        members_only: is_members_only_renderer(renderer),
        result_type: None,
    })
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

pub(crate) fn last_thumbnail_url(node: Option<&Value>) -> Option<String> {
    let node = node?;
    let thumbs = node
        .get("thumbnails")
        .and_then(Value::as_array)
        .or_else(|| node.as_array())?;
    thumbs
        .last()
        .and_then(|t| t.get("url"))
        .and_then(Value::as_str)
        .map(|s| {
            if s.starts_with("//") {
                format!("https:{s}")
            } else {
                s.to_string()
            }
        })
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
        let item = parse_video_renderer(&members_renderer, Some("Channel"), None)
            .expect("should parse members video");
        assert!(item.members_only);

        let normal_renderer = json!({
            "videoId": "dQw4w9WgXcQ",
            "title": { "runs": [{ "text": "Members only reaction (public video)" }] },
            "badges": []
        });
        let normal = parse_video_renderer(&normal_renderer, Some("Channel"), None)
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
        let item = parse_video_renderer(&overlay_only_renderer, None, None).unwrap();
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
}
