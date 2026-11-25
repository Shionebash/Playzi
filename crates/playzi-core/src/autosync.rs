use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, broadcast, watch};

use crate::config::ConfigStore;
use crate::cookies::CookieStore;
use crate::error::{CoreError, Result};
use crate::state::{AppState, StateStore, SyncStatusState};
use crate::youtube::YouTubeClient;
use crate::ytmusic::{DEFAULT_YTMUSIC_BASE_URL, YtMusicClient};

pub const CHANNELS_INTERVAL_SECS: i64 = 3 * 60 * 60;
pub const RECOMMENDATIONS_INTERVAL_SECS: i64 = 6 * 60 * 60;
pub const PLAYLISTS_INTERVAL_SECS: i64 = 12 * 60 * 60;
pub const FAILED_RETRY_SECS: i64 = 15 * 60;
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

const TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S";
const EVENT_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncSection {
    Channels,
    Recommendations,
    MusicRecommendations,
    MusicPlaylists,
}

impl SyncSection {
    pub fn all() -> &'static [SyncSection] {
        &[
            SyncSection::Channels,
            SyncSection::Recommendations,
            SyncSection::MusicRecommendations,
            SyncSection::MusicPlaylists,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SyncSection::Channels => "channels",
            SyncSection::Recommendations => "recommendations",
            SyncSection::MusicRecommendations => "musicRecommendations",
            SyncSection::MusicPlaylists => "musicPlaylists",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim() {
            "channels" => Some(SyncSection::Channels),
            "recommendations" => Some(SyncSection::Recommendations),
            "musicRecommendations" => Some(SyncSection::MusicRecommendations),
            "musicPlaylists" => Some(SyncSection::MusicPlaylists),
            _ => None,
        }
    }

    pub fn interval_secs(&self) -> i64 {
        match self {
            SyncSection::Channels => CHANNELS_INTERVAL_SECS,
            SyncSection::Recommendations => RECOMMENDATIONS_INTERVAL_SECS,
            SyncSection::MusicRecommendations => RECOMMENDATIONS_INTERVAL_SECS,
            SyncSection::MusicPlaylists => PLAYLISTS_INTERVAL_SECS,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SyncEvent {
    pub running: bool,
    pub section: String,
    pub last_attempt: String,
    pub last_success: String,
    pub last_error: String,
    pub warning: String,
}

#[derive(Clone)]
pub struct AutoSyncManager {
    state: StateStore,
    config: ConfigStore,
    youtube: YouTubeClient,
    http: reqwest::Client,
    ytmusic_base_url: String,
    refresh_lock: Arc<AsyncMutex<()>>,
    events: broadcast::Sender<SyncEvent>,
    shutdown_tx: Arc<Mutex<Option<watch::Sender<bool>>>>,
}

impl AutoSyncManager {
    pub fn new(state: StateStore, config: ConfigStore, youtube: YouTubeClient) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            state,
            config,
            youtube,
            http,
            ytmusic_base_url: DEFAULT_YTMUSIC_BASE_URL.to_string(),
            refresh_lock: Arc::new(AsyncMutex::new(())),
            events,
            shutdown_tx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_ytmusic_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.ytmusic_base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SyncEvent> {
        self.events.subscribe()
    }

    pub fn get_sync_status(&self) -> Result<SyncStatusState> {
        Ok(self.state.read()?.sync_status)
    }

    pub fn is_section_due(&self, section: SyncSection) -> Result<bool> {
        let data = self.state.read()?;
        Ok(section_is_due(&data, section))
    }

    /// Runs a single sync section exclusively (errors if another sync is currently running).
    pub async fn refresh_section(&self, section: SyncSection) -> Result<Value> {
        let _guard = self.refresh_lock.try_lock().map_err(|_| {
            CoreError::Validation("Ya hay una sincronizacion en curso.".into())
        })?;
        self.run_section_locked(section).await
    }

    /// Runs all sync sections sequentially under a single non-overlapping lock.
    pub async fn refresh_all(&self) -> Result<Value> {
        let _guard = self.refresh_lock.try_lock().map_err(|_| {
            CoreError::Validation("Ya hay una sincronizacion en curso.".into())
        })?;

        let mut results = serde_json::Map::new();
        for &section in SyncSection::all() {
            match self.run_section_locked(section).await {
                Ok(val) => {
                    results.insert(section.as_str().to_string(), val);
                }
                Err(err) => {
                    results.insert(
                        section.as_str().to_string(),
                        json!({ "ok": false, "error": err.to_string() }),
                    );
                }
            }
        }
        let status = self.get_sync_status()?;
        Ok(json!({
            "ok": true,
            "results": Value::Object(results),
            "status": status,
        }))
    }

    /// Runs only sections that are currently due. Returns the list of section names executed.
    pub async fn refresh_due_sections(&self) -> Result<Vec<String>> {
        let Ok(_guard) = self.refresh_lock.try_lock() else {
            return Ok(Vec::new());
        };

        let mut executed = Vec::new();
        for &section in SyncSection::all() {
            let data = self.state.read()?;
            if section_is_due(&data, section) {
                let _ = self.run_section_locked(section).await;
                executed.push(section.as_str().to_string());
            }
        }
        Ok(executed)
    }

    /// Starts the periodic background Tokio scheduler. Returns `false` if already running.
    pub fn start_background(&self, initial_delay: Duration, poll_interval: Duration) -> bool {
        let mut slot = self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_some() {
            return false;
        }
        let (tx, mut rx) = watch::channel(false);
        *slot = Some(tx);
        drop(slot);

        let manager = self.clone();
        let poll_interval = poll_interval.max(Duration::from_millis(50));
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(initial_delay) => {}
                _ = rx.changed() => {
                    if *rx.borrow() {
                        return;
                    }
                }
            }

            loop {
                if *rx.borrow() {
                    break;
                }
                let _ = manager.refresh_due_sections().await;

                tokio::select! {
                    _ = tokio::time::sleep(poll_interval) => {}
                    _ = rx.changed() => {
                        if *rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        true
    }

    /// Signals the background scheduler task to stop cleanly.
    pub fn stop_background(&self) {
        let mut slot = self.shutdown_tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = slot.take() {
            let _ = tx.send(true);
        }
    }

    async fn run_section_locked(&self, section: SyncSection) -> Result<Value> {
        let attempt = now_timestamp();
        self.mark_running(section, &attempt)?;

        let outcome = match section {
            SyncSection::Channels => self.sync_channels().await,
            SyncSection::Recommendations => self.sync_recommendations().await,
            SyncSection::MusicRecommendations => self.sync_music_recommendations().await,
            SyncSection::MusicPlaylists => self.sync_music_playlists().await,
        };

        match outcome {
            Ok(result) => {
                let success = result
                    .get("lastSync")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(now_timestamp);
                let warning = result
                    .get("warning")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let used_stale = result
                    .get("usedStaleCookies")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.mark_finished(section, &attempt, Some(&success), "", used_stale, &warning)?;
                Ok(result)
            }
            Err(err) => {
                let msg = err.to_string();
                let _ = self.mark_finished(section, &attempt, None, &msg, false, "");
                Err(err)
            }
        }
    }

    async fn sync_channels(&self) -> Result<Value> {
        let state = self.state.read()?;
        if !state.channels.is_empty() {
            let _ = self.youtube.refresh_all_channels(&self.state).await?;
        }
        let now = now_timestamp();
        self.state.update(|s| {
            s.channels_last_sync = now.clone();
            Ok(())
        })?;
        Ok(json!({ "ok": true, "lastSync": now }))
    }

    async fn sync_recommendations(&self) -> Result<Value> {
        let cfg = self.config.snapshot();
        let cookie_store = CookieStore::new(&cfg.root, &cfg.data_dir);
        let configured = if cfg.cookies_file.trim().is_empty() {
            None
        } else {
            Some(cfg.cookies_file.as_str())
        };
        let jar = cookie_store.load_jar(configured).ok();
        let auth_user = self
            .state
            .read()
            .map(|s| s.music_auth_user)
            .unwrap_or(0);
        let items = self
            .youtube
            .fetch_recommendations_with_cookies_and_user(jar.as_ref(), auth_user, Some(&self.state))
            .await?;
        let now = self.state.read()?.recommendations_last_sync;
        Ok(json!({ "ok": true, "count": items.len(), "lastSync": now }))
    }

    async fn sync_music_recommendations(&self) -> Result<Value> {
        let client = self.build_ytmusic_client();
        let items = client.fetch_recommendations(Some(&self.state)).await?;
        let now = self.state.read()?.music_recom_last_sync;
        Ok(json!({ "ok": true, "count": items.len(), "lastSync": now }))
    }

    async fn sync_music_playlists(&self) -> Result<Value> {
        let client = self.build_ytmusic_client();
        let playlists = client.fetch_playlists(Some(&self.state)).await?;
        let now = self.state.read()?.music_playlists_last_sync;
        Ok(json!({ "ok": true, "count": playlists.len(), "lastSync": now }))
    }

    fn build_ytmusic_client(&self) -> YtMusicClient {
        let cfg = self.config.snapshot();
        let cookie_store = CookieStore::new(&cfg.root, &cfg.data_dir);
        let configured = if cfg.cookies_file.trim().is_empty() {
            None
        } else {
            Some(cfg.cookies_file.as_str())
        };
        let jar = cookie_store.load_jar(configured).ok();
        let auth_user = self
            .state
            .read()
            .map(|s| s.music_auth_user)
            .unwrap_or(0);
        YtMusicClient::with_client(self.http.clone(), &self.ytmusic_base_url, jar)
            .with_auth_user(auth_user)
    }

    fn mark_running(&self, section: SyncSection, attempt: &str) -> Result<()> {
        let sec_key = section.as_str().to_string();
        let mut last_success = String::new();
        self.state.update(|s| {
            s.sync_status.running = true;
            s.sync_status.section = sec_key.clone();
            s.sync_status.last_attempt = attempt.to_string();
            s.sync_status.last_error.clear();
            last_success = s.sync_status.last_success.clone();

            let entry = s
                .sync_status
                .sections
                .entry(sec_key.clone())
                .or_insert_with(|| json!({}));
            if let Some(map) = entry.as_object_mut() {
                map.insert("running".into(), Value::Bool(true));
                map.insert("lastAttempt".into(), Value::String(attempt.to_string()));
                map.insert("lastError".into(), Value::String(String::new()));
            }
            Ok(())
        })?;

        let _ = self.events.send(SyncEvent {
            running: true,
            section: sec_key,
            last_attempt: attempt.to_string(),
            last_success,
            last_error: String::new(),
            warning: String::new(),
        });
        Ok(())
    }

    fn mark_finished(
        &self,
        section: SyncSection,
        attempt: &str,
        success: Option<&str>,
        error: &str,
        used_stale: bool,
        warning: &str,
    ) -> Result<()> {
        let sec_key = section.as_str().to_string();
        let mut final_success = String::new();

        self.state.update(|s| {
            s.sync_status.running = false;
            s.sync_status.section.clear();
            s.sync_status.last_attempt = attempt.to_string();
            s.sync_status.last_error = error.to_string();
            s.sync_status.used_stale_cookies = used_stale;
            s.sync_status.warning = warning.to_string();
            if let Some(ok_ts) = success {
                s.sync_status.last_success = ok_ts.to_string();
            }
            final_success = s.sync_status.last_success.clone();

            let entry = s
                .sync_status
                .sections
                .entry(sec_key.clone())
                .or_insert_with(|| json!({}));
            if let Some(map) = entry.as_object_mut() {
                map.insert("running".into(), Value::Bool(false));
                map.insert("lastAttempt".into(), Value::String(attempt.to_string()));
                map.insert("lastError".into(), Value::String(error.to_string()));
                map.insert("usedStaleCookies".into(), Value::Bool(used_stale));
                map.insert("warning".into(), Value::String(warning.to_string()));
                if let Some(ok_ts) = success {
                    map.insert("lastSuccess".into(), Value::String(ok_ts.to_string()));
                }
            }
            Ok(())
        })?;

        let _ = self.events.send(SyncEvent {
            running: false,
            section: sec_key,
            last_attempt: attempt.to_string(),
            last_success: final_success,
            last_error: error.to_string(),
            warning: warning.to_string(),
        });
        Ok(())
    }
}

pub fn section_is_due(data: &AppState, section: SyncSection) -> bool {
    if let Some(sec_status) = data.sync_status.sections.get(section.as_str()) {
        let has_error = sec_status
            .get("lastError")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let last_attempt = sec_status
            .get("lastAttempt")
            .and_then(Value::as_str)
            .unwrap_or("");
        if has_error && is_recent(last_attempt, FAILED_RETRY_SECS) {
            return false;
        }
    }

    let (items_empty, last_sync) = match section {
        SyncSection::Channels => (data.feed.is_empty() && !data.channels.is_empty(), data.channels_last_sync.as_str()),
        SyncSection::Recommendations => (data.recommendations.is_empty(), data.recommendations_last_sync.as_str()),
        SyncSection::MusicRecommendations => (data.music_recommendations.is_empty(), data.music_recom_last_sync.as_str()),
        SyncSection::MusicPlaylists => (data.music_playlists.is_empty(), data.music_playlists_last_sync.as_str()),
    };

    if items_empty || last_sync.is_empty() {
        return true;
    }

    !is_recent(last_sync, section.interval_secs())
}

fn is_recent(timestamp_str: &str, max_age_secs: i64) -> bool {
    let Ok(naive) = NaiveDateTime::parse_from_str(timestamp_str.trim(), TIME_FORMAT) else {
        return false;
    };
    let Some(dt) = Local.from_local_datetime(&naive).earliest() else {
        return false;
    };
    let elapsed = Local::now().signed_duration_since(dt).num_seconds();
    elapsed < max_age_secs
}

fn now_timestamp() -> String {
    Local::now().format(TIME_FORMAT).to_string()
}
