use std::collections::HashSet;
use std::time::Duration;

use chrono::Local;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ORIGIN, REFERER, USER_AGENT};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cookies::CookieJar;
use crate::error::{CoreError, Result};
use crate::state::StateStore;
use crate::youtube::{MediaItem, last_thumbnail_url, parse_duration_text, text_from_node, walk_renderers};

pub const DEFAULT_YTMUSIC_BASE_URL: &str = "https://music.youtube.com";
const DEFAULT_MUSIC_CLIENT_VERSION: &str = "1.20240520.01.00";
const DEFAULT_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0 Safari/537.36";

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
        let resp = self
            .http
            .post(url)
            .headers(self.auth_header_map())
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
        if let Some(f) = filter {
            for item in &mut items {
                if item.result_type.is_none() {
                    item.result_type = Some(f.to_ascii_lowercase());
                }
            }
        }
        Ok(items)
    }

    /// Fetches personalized YouTube Music home recommendations (`FEmusic_home`).
    pub async fn fetch_recommendations(
        &self,
        store: Option<&StateStore>,
    ) -> Result<Vec<MediaItem>> {
        let data = self.browse("FEmusic_home").await?;
        let playlists = parse_music_playlists(&data, 16);
        let songs = parse_music_items(&data, 40);

        let mut items: Vec<MediaItem> = Vec::new();
        let mut seen = HashSet::new();
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
                });
            }
        }
        for song in songs {
            if seen.insert(song.id.clone()) {
                items.push(song);
            }
        }

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

    /// Fetches songs in a YouTube Music playlist by playlist ID.
    pub async fn fetch_playlist_songs(&self, playlist_id: &str) -> Result<Vec<MediaItem>> {
        let clean = playlist_id.trim();
        if clean.is_empty() {
            return Err(CoreError::Validation("Empty playlist ID".into()));
        }
        let browse_id = if clean.starts_with("VL") || clean.starts_with("MPREb_") {
            clean.to_string()
        } else {
            format!("VL{clean}")
        };
        let data = self.browse(&browse_id).await?;
        Ok(parse_music_items(&data, 100))
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

        let songs = parse_music_items(&data, 20);
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

        let duration_text_raw = fixed_column_text(renderer, 0).or_else(|| extract_duration_from_subtitle(&subtitle));
        let duration = duration_text_raw.as_deref().and_then(parse_duration_text);
        let thumbnail = first_nested_thumbnail(renderer)
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg"));

        items.push(MediaItem {
            id: video_id.clone(),
            url: format!("https://music.youtube.com/watch?v={video_id}"),
            title,
            channel: subtitle,
            channel_url: extract_music_channel_url(renderer),
            duration,
            duration_text: duration_text_raw.unwrap_or_else(|| "--:--".into()),
            thumbnail: Some(thumbnail),
            view_count: None,
            live: false,
            members_only: false,
            result_type: Some("song".into()),
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
        .split('•')
        .map(str::trim)
        .rev()
        .find(|part| parse_duration_text(part).is_some())
        .map(ToString::to_string)
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
