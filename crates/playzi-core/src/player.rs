use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::process::Command as TokioCommand;
use tokio::sync::broadcast;
use url::Url;
use uuid::Uuid;

use crate::config::ConfigStore;
use crate::cookies::CookieStore;
use crate::error::{CoreError, Result};
use crate::history::add_history_event;
use crate::state::StateStore;
use crate::youtube::{extract_video_id, format_duration, is_youtube_url};

const EVENT_CHANNEL_CAPACITY: usize = 64;
const DEFAULT_STREAM_CACHE_TTL_SECS: u64 = 600;
const EXPIRE_SAFETY_MARGIN_SECS: i64 = 60;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/143 Safari/537.36";
const IOS_INNERTUBE_CLIENT_VERSION: &str = "20.10.4";
const IOS_INNERTUBE_USER_AGENT: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
const INNERTUBE_PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerBackend {
    Mpv,
    Vlc,
}

impl PlayerBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "mpv" => Some(Self::Mpv),
            "vlc" => Some(Self::Vlc),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mpv => "mpv",
            Self::Vlc => "vlc",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayRequest {
    pub target: String,
    pub player: Option<String>,
    pub title: Option<String>,
    pub thumbnail: Option<String>,
    pub channel: Option<String>,
    pub quality: Option<String>,
    #[serde(default)]
    pub audio_only: bool,
    pub wid: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayResponse {
    pub ok: bool,
    pub player: String,
    pub executable: String,
    pub target: String,
    pub session_id: String,
    pub ipc_endpoint: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StreamFormat {
    pub format_id: String,
    pub url: String,
    pub ext: String,
    pub vcodec: String,
    pub acodec: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub tbr: Option<f64>,
    pub abr: Option<f64>,
    pub language: Option<String>,
    pub language_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_range: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<u32>,
    #[serde(default)]
    pub http_headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AudioTrack {
    pub lang: String,
    pub label: String,
    pub format_id: String,
    pub abr: Option<f64>,
    pub acodec: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedStreamInfo {
    pub item: Value,
    pub duration: f64,
    pub quality: String,
    pub stream_url: Option<String>,
    pub video: Option<StreamFormat>,
    pub audio: Option<StreamFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hls_audio: Option<StreamFormat>,
    pub audio_tracks: Vec<AudioTrack>,
    #[serde(default)]
    pub available_qualities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_db: Option<f64>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub channel: String,
    pub thumbnail: String,
    pub duration_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackState {
    pub active: bool,
    pub paused: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub volume: f64,
    pub muted: bool,
    pub title: String,
    pub target: String,
    pub backend: String,
    pub hwdec: String,
    pub audio_only: bool,
    pub session_id: Option<String>,
    pub ipc_endpoint: Option<String>,
    pub error: Option<String>,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            active: false,
            paused: false,
            position_secs: 0.0,
            duration_secs: 0.0,
            volume: 100.0,
            muted: false,
            title: String::new(),
            target: String::new(),
            backend: "mpv".into(),
            hwdec: "auto-safe".into(),
            audio_only: false,
            session_id: None,
            ipc_endpoint: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerEvent {
    pub kind: String,
    pub state: PlaybackState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerCapabilities {
    pub mpv_available: bool,
    pub mpv_path: String,
    pub vlc_available: bool,
    pub vlc_path: String,
    pub ytdlp_available: bool,
    pub ytdlp_path: String,
    pub hwdec_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum PlayerControlCommand {
    Pause { paused: bool },
    TogglePause,
    Seek { seconds: f64, #[serde(default)] relative: bool },
    SetVolume { volume: f64 },
    SetMute { muted: bool },
    SetAudioTrack { track: String },
    LoadFile { url: String, title: Option<String>, #[serde(default)] audio_only: bool },
    Stop,
}

#[derive(Clone)]
struct CachedRawVideo {
    item: Value,
    duration: f64,
    formats: Vec<StreamFormat>,
    direct_url: Option<String>,
    loudness_db: Option<f64>,
    expires_at: Option<i64>,
    cached_at: Instant,
}

impl CachedRawVideo {
    fn project(&self, quality: &str, audio_only: bool) -> ResolvedStreamInfo {
        let max_h = quality_to_height(quality);
        let selected_hls_audio = select_best_hls_audio(&self.formats);
        let selected_audio = select_best_audio(&self.formats);
        let has_dash_audio = selected_audio.as_ref().is_some_and(is_dash_format);
        let available_qualities = if audio_only {
            Vec::new()
        } else {
            extract_available_qualities(&self.formats, selected_hls_audio.is_some(), has_dash_audio)
        };
        let selected_video = if audio_only {
            None
        } else {
            select_best_video(
                &self.formats,
                max_h,
                selected_hls_audio.is_some(),
                has_dash_audio,
            )
        };
        let audio_tracks = extract_audio_tracks(&self.formats);

        let stream_url = self.direct_url.clone().or_else(|| {
            if audio_only {
                selected_audio.as_ref().map(|a| a.url.clone())
            } else {
                selected_video
                    .as_ref()
                    .map(|v| v.url.clone())
                    .or_else(|| selected_audio.as_ref().map(|a| a.url.clone()))
            }
        });

        let mut expires_candidates = Vec::new();
        if let Some(exp) = self.expires_at {
            expires_candidates.push(exp);
        }
        if let Some(v) = &selected_video {
            if let Some(exp) = extract_url_expire(&v.url) {
                expires_candidates.push(exp);
            }
        }
        if let Some(a) = &selected_audio {
            if let Some(exp) = extract_url_expire(&a.url) {
                expires_candidates.push(exp);
            }
        }
        if let Some(u) = &stream_url {
            if let Some(exp) = extract_url_expire(u) {
                expires_candidates.push(exp);
            }
        }
        let expires_at = expires_candidates.into_iter().min();

        ResolvedStreamInfo {
            item: self.item.clone(),
            duration: self.duration,
            quality: quality.to_string(),
            stream_url,
            video: selected_video,
            audio: selected_audio,
            hls_audio: selected_hls_audio,
            audio_tracks,
            available_qualities,
            loudness_db: self.loudness_db,
            expires_at,
        }
    }
}

type InFlightMap = HashMap<String, broadcast::Sender<std::result::Result<CachedRawVideo, String>>>;

struct ManagedSession {
    child: Option<Child>,
    ipc_endpoint: Option<String>,
}

impl Drop for ManagedSession {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Clone)]
pub struct PlayerManager {
    state_store: StateStore,
    config: ConfigStore,
    ytdlp_override: Option<String>,
    raw_cache: Arc<Mutex<HashMap<String, CachedRawVideo>>>,
    in_flight: Arc<Mutex<InFlightMap>>,
    loudness_cache: Arc<Mutex<HashMap<String, Option<f64>>>>,
    loudness_client: reqwest::Client,
    innertube_client: reqwest::Client,
    playback: Arc<Mutex<PlaybackState>>,
    session: Arc<Mutex<ManagedSession>>,
    events: broadcast::Sender<PlayerEvent>,
}

impl PlayerManager {
    pub fn new(state_store: StateStore, config: ConfigStore) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let loudness_client = reqwest::Client::builder()
            .user_agent(DEFAULT_USER_AGENT)
            .timeout(Duration::from_secs(4))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let innertube_client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(4))
            .timeout(Duration::from_secs(6))
            .pool_max_idle_per_host(8)
            .tcp_nodelay(true)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            state_store,
            config,
            ytdlp_override: None,
            raw_cache: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            loudness_cache: Arc::new(Mutex::new(HashMap::new())),
            loudness_client,
            innertube_client,
            playback: Arc::new(Mutex::new(PlaybackState::default())),
            session: Arc::new(Mutex::new(ManagedSession {
                child: None,
                ipc_endpoint: None,
            })),
            events,
        }
    }

    pub fn with_ytdlp_bin(mut self, bin: impl Into<String>) -> Self {
        self.ytdlp_override = Some(bin.into());
        self
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PlayerEvent> {
        self.events.subscribe()
    }

    pub fn playback_state(&self) -> PlaybackState {
        let mut guard = self.playback.lock().expect("playback lock");
        // Refresh active status if child exited
        if guard.active {
            let mut sess = self.session.lock().expect("session lock");
            if let Some(child) = sess.child.as_mut() {
                if let Ok(Some(_)) = child.try_wait() {
                    sess.child = None;
                    sess.ipc_endpoint = None;
                    guard.active = false;
                    guard.paused = false;
                    guard.ipc_endpoint = None;
                }
            }
        }
        guard.clone()
    }

    pub fn capabilities(&self) -> PlayerCapabilities {
        let cfg = self.config.snapshot();
        let mpv_path = resolve_player_binary(PlayerBackend::Mpv, &cfg.mpv_path, &cfg.root);
        let vlc_path = resolve_player_binary(PlayerBackend::Vlc, &cfg.vlc_path, &cfg.root);
        let ytdlp_path = self.resolve_ytdlp_bin();
        PlayerCapabilities {
            mpv_available: binary_exists(&mpv_path),
            mpv_path,
            vlc_available: binary_exists(&vlc_path),
            vlc_path,
            ytdlp_available: binary_exists(&ytdlp_path),
            ytdlp_path,
            hwdec_mode: "auto-safe".into(),
        }
    }

    pub fn resolve_ytdlp_bin(&self) -> String {
        if let Some(custom) = &self.ytdlp_override {
            if !custom.trim().is_empty() {
                return custom.clone();
            }
        }
        let cfg = self.config.snapshot();
        discover_ytdlp_binary(&cfg.root)
    }

    fn resolve_cookie_file(&self) -> Option<PathBuf> {
        let cfg = self.config.snapshot();
        let store = CookieStore::new(&cfg.root, &cfg.data_dir);
        let configured = if cfg.cookies_file.trim().is_empty() {
            None
        } else {
            Some(cfg.cookies_file.as_str())
        };
        store.resolve_cookie_file(configured).ok()
    }

    /// Launches playback in `mpv` (with GPU hwdec + JSON IPC + optional `--wid`) or `vlc`,
    /// validates local paths against `MEDIA_ROOT`, and records a play event in history.
    pub fn play(&self, req: PlayRequest) -> Result<PlayResponse> {
        let cfg = self.config.snapshot();
        let selected_name = req
            .player
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&cfg.default_player);
        let backend = PlayerBackend::parse(selected_name)
            .ok_or_else(|| CoreError::Validation(format!("Unsupported player: {selected_name}")))?;

        let exe = match backend {
            PlayerBackend::Mpv => resolve_player_binary(PlayerBackend::Mpv, &cfg.mpv_path, &cfg.root),
            PlayerBackend::Vlc => resolve_player_binary(PlayerBackend::Vlc, &cfg.vlc_path, &cfg.root),
        };
        if exe.trim().is_empty() {
            return Err(CoreError::Validation(format!(
                "Player executable is not configured for {}",
                backend.as_str()
            )));
        }
        if !binary_exists(&exe) {
            return Err(CoreError::External(format!(
                "Configured {} executable was not found: {}",
                backend.as_str(),
                exe
            )));
        }

        let target = validate_play_target(&req.target, &cfg.media_root)?;
        let quality = req
            .quality
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&cfg.playback_quality);

        // Stop any previous managed session cleanly before spawning a new one
        let _ = self.stop();

        let session_id = format!("play-{}", Uuid::new_v4());
        let ipc_endpoint = if backend == PlayerBackend::Mpv {
            Some(build_ipc_endpoint(&session_id))
        } else {
            None
        };

        let ytdlp_bin = self.resolve_ytdlp_bin();
        let cookie_file = self.resolve_cookie_file();
        let ytdlp_cache = cfg.data_dir.join("ytdlp_cache");
        let js_runtime = resolve_js_runtime();
        let title_for_player = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let log_path = cfg.data_dir.join("player.log");

        let args = if backend == PlayerBackend::Vlc && is_youtube_url(&target) {
            match resolve_vlc_stream_urls(
                &ytdlp_bin,
                &ytdlp_cache,
                js_runtime.as_ref(),
                cookie_file.as_deref(),
                &target,
                quality,
                req.audio_only,
            ) {
                Ok((stream_url, audio_slave)) => build_vlc_args(
                    &stream_url,
                    req.audio_only,
                    audio_slave.as_deref(),
                    title_for_player,
                ),
                Err(err) => {
                    append_player_log(
                        &log_path,
                        &format!("[playzi] VLC yt-dlp resolution failed for {target}: {err}\n"),
                    );
                    return Err(err);
                }
            }
        } else if backend == PlayerBackend::Mpv {
            build_mpv_args_with_ytdlp(
                &target,
                quality,
                req.audio_only,
                req.wid,
                ipc_endpoint.as_deref(),
                &ytdlp_bin,
                Some(&ytdlp_cache),
                js_runtime.as_ref(),
                None,
            )
        } else {
            build_player_args(
                backend,
                &target,
                quality,
                req.audio_only,
                req.wid,
                ipc_endpoint.as_deref(),
                &ytdlp_bin,
                cookie_file.as_deref(),
            )
        };

        let (stdout_cfg, stderr_cfg) = prepare_player_log_stdio(&log_path, backend, &exe, &args);

        let mut cmd = StdCommand::new(&exe);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(stdout_cfg)
            .stderr(stderr_cfg);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = cmd
            .spawn()
            .map_err(|err| {
                CoreError::External(format!(
                    "Failed to launch {} ({}): {}",
                    backend.as_str(),
                    exe,
                    err
                ))
            })?;

        {
            let mut sess = self.session.lock().expect("session lock");
            sess.child = Some(child);
            sess.ipc_endpoint = ipc_endpoint.clone();
        }

        let title = req
            .title
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| target.clone());

        let new_state = {
            let mut st = self.playback.lock().expect("playback lock");
            st.active = true;
            st.paused = false;
            st.position_secs = 0.0;
            st.duration_secs = 0.0;
            st.title = title.clone();
            st.target = target.clone();
            st.backend = backend.as_str().to_string();
            st.hwdec = if backend == PlayerBackend::Mpv {
                "auto-safe".into()
            } else {
                "external".into()
            };
            st.audio_only = req.audio_only;
            st.session_id = Some(session_id.clone());
            st.ipc_endpoint = ipc_endpoint.clone();
            st.error = None;
            st.clone()
        };

        self.emit_event("started", new_state);

        let url_field = if target.starts_with("http://") || target.starts_with("https://") {
            target.clone()
        } else {
            String::new()
        };
        let _ = add_history_event(
            &self.state_store,
            json!({
                "id": session_id,
                "action": "play",
                "title": title,
                "url": url_field,
                "target": target,
                "player": backend.as_str(),
                "thumbnail": req.thumbnail.unwrap_or_default(),
                "channel": req.channel.unwrap_or_default(),
            }),
        );

        Ok(PlayResponse {
            ok: true,
            player: backend.as_str().to_string(),
            executable: exe,
            target,
            session_id,
            ipc_endpoint,
        })
    }

    /// Stops any active managed player process and resets playback state.
    pub fn stop(&self) -> Result<PlaybackState> {
        {
            let mut sess = self.session.lock().expect("session lock");
            if let Some(ref endpoint) = sess.ipc_endpoint {
                let _ = send_mpv_ipc_json(endpoint, &json!({ "command": ["quit"] }));
            }
            if let Some(mut child) = sess.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            sess.ipc_endpoint = None;
        }

        let updated = {
            let mut st = self.playback.lock().expect("playback lock");
            st.active = false;
            st.paused = false;
            st.position_secs = 0.0;
            st.ipc_endpoint = None;
            st.clone()
        };

        self.emit_event("stopped", updated.clone());
        Ok(updated)
    }

    /// Sends a control command to the active `mpv` JSON IPC server and updates local state.
    pub fn control(&self, cmd: PlayerControlCommand) -> Result<PlaybackState> {
        if matches!(cmd, PlayerControlCommand::Stop) {
            return self.stop();
        }

        let endpoint = {
            let sess = self.session.lock().expect("session lock");
            sess.ipc_endpoint.clone()
        };

        let Some(endpoint) = endpoint else {
            return Err(CoreError::Validation(
                "No active mpv IPC session to control".into(),
            ));
        };

        let ipc_payload = build_mpv_ipc_command(&cmd);
        send_mpv_ipc_json(&endpoint, &ipc_payload)?;

        let updated = {
            let mut st = self.playback.lock().expect("playback lock");
            match cmd {
                PlayerControlCommand::Pause { paused } => st.paused = paused,
                PlayerControlCommand::TogglePause => st.paused = !st.paused,
                PlayerControlCommand::Seek { seconds, relative } => {
                    if relative {
                        st.position_secs = (st.position_secs + seconds).max(0.0);
                    } else {
                        st.position_secs = seconds.max(0.0);
                    }
                }
                PlayerControlCommand::SetVolume { volume } => {
                    st.volume = volume.clamp(0.0, 150.0);
                }
                PlayerControlCommand::SetMute { muted } => st.muted = muted,
                PlayerControlCommand::LoadFile {
                    url,
                    title,
                    audio_only,
                } => {
                    st.target = url.clone();
                    st.title = title.unwrap_or(url);
                    st.audio_only = audio_only;
                    st.paused = false;
                    st.position_secs = 0.0;
                }
                PlayerControlCommand::SetAudioTrack { .. } | PlayerControlCommand::Stop => {}
            }
            st.clone()
        };

        self.emit_event("updated", updated.clone());
        Ok(updated)
    }

    /// Resolves direct video/audio streams and metadata via `yt-dlp --dump-single-json`,
    /// with expiry-aware in-memory caching.
    pub async fn resolve_stream(
        &self,
        url: &str,
        quality: Option<&str>,
        audio_only: bool,
    ) -> Result<ResolvedStreamInfo> {
        self.resolve_stream_with_refresh(url, quality, audio_only, false)
            .await
    }

    /// Resolves direct video/audio streams, optionally bypassing and replacing any cached entry.
    pub async fn resolve_stream_with_refresh(
        &self,
        url: &str,
        quality: Option<&str>,
        audio_only: bool,
        force_refresh: bool,
    ) -> Result<ResolvedStreamInfo> {
        let trimmed = url.trim();
        validate_http_url(trimmed)?;
        let cfg = self.config.snapshot();
        let qual = quality
            .filter(|q| !q.trim().is_empty())
            .unwrap_or(&cfg.playback_quality);
        let raw = self.resolve_raw_video(trimmed, force_refresh).await?;
        Ok(raw.project(qual, audio_only))
    }

    /// Generates an in-memory DASH `.mpd` manifest for a video URL using the cached or resolved formats.
    pub async fn resolve_dash_manifest(
        &self,
        url: &str,
        quality: Option<&str>,
        proxy_prefix: &str,
    ) -> Result<String> {
        let info = self.resolve_stream(url, quality, false).await?;
        let video = info.video.as_ref().ok_or_else(|| {
            CoreError::External("No video stream available for DASH manifest".into())
        })?;
        let audio = info.audio.as_ref().ok_or_else(|| {
            CoreError::External("No audio stream available for DASH manifest".into())
        })?;
        build_dash_mpd_manifest(info.duration, video, audio, proxy_prefix)
    }

    async fn resolve_raw_video(&self, url: &str, force_refresh: bool) -> Result<CachedRawVideo> {
        let cache_key = extract_video_id(url)
            .map(|id| format!("yt:{id}"))
            .unwrap_or_else(|| url.to_string());

        if force_refresh {
            if let Ok(mut cache) = self.raw_cache.lock() {
                cache.remove(&cache_key);
            }
        } else if let Some(cached) = self.get_cached_raw(&cache_key) {
            return Ok(cached);
        }

        // Single-flight deduplication: if another task (e.g. hover prefetch) is already
        // resolving the same video, await its result instead of firing a duplicate request.
        let mut rx_opt = None;
        if !force_refresh {
            let mut inflight = self.in_flight.lock().expect("in_flight lock");
            if let Some(tx) = inflight.get(&cache_key) {
                rx_opt = Some(tx.subscribe());
            } else {
                let (tx, _) = broadcast::channel(8);
                inflight.insert(cache_key.clone(), tx);
            }
        }

        if let Some(mut rx) = rx_opt {
            return match rx.recv().await {
                Ok(Ok(raw)) => Ok(raw),
                Ok(Err(msg)) => Err(CoreError::External(msg)),
                Err(_) => {
                    // Sender dropped unexpectedly; fall back to fresh resolution.
                    self.fetch_raw_video_uncached(url, &cache_key, force_refresh)
                        .await
                }
            };
        }

        let result = self
            .fetch_raw_video_uncached(url, &cache_key, force_refresh)
            .await;

        if !force_refresh {
            let tx_opt = self
                .in_flight
                .lock()
                .expect("in_flight lock")
                .remove(&cache_key);
            if let Some(tx) = tx_opt {
                let _ = tx.send(result.as_ref().cloned().map_err(|e| e.to_string()));
            }
        }

        result
    }

    async fn fetch_raw_video_uncached(
        &self,
        url: &str,
        cache_key: &str,
        force_refresh: bool,
    ) -> Result<CachedRawVideo> {
        // Fast path: direct InnerTube IOS /youtubei/v1/player (~240-350ms warm vs ~5.8s yt-dlp).
        // Skip fast path on explicit force_refresh (auto-recovery after playback error) so
        // recovery transparently switches to yt-dlp HLS.
        if !force_refresh && is_youtube_url(url) {
            if let Some(vid) = extract_video_id(url) {
                if let Ok(mut fast_raw) = self.resolve_via_innertube_ios(&vid, url).await {
                    if fast_raw.loudness_db.is_none() {
                        // Reuse cached loudness if already known, without blocking playback on HTML scraping.
                        if let Ok(lcache) = self.loudness_cache.lock() {
                            if let Some(Some(db)) = lcache.get(&vid) {
                                fast_raw.loudness_db = Some(*db);
                            }
                        }
                    }
                    self.put_cached_raw(cache_key.to_string(), fast_raw.clone());
                    return Ok(fast_raw);
                }
            }
        }

        let (raw_res, fetched_loudness) = tokio::join!(
            self.run_ytdlp_json(url, false, None),
            self.fetch_loudness_db(url),
        );
        let raw = raw_res?;
        let mut cached = parse_ytdlp_raw_video(&raw, url);
        if cached.loudness_db.is_none() {
            cached.loudness_db = fetched_loudness;
        }
        self.put_cached_raw(cache_key.to_string(), cached.clone());
        Ok(cached)
    }

    async fn resolve_via_innertube_ios(
        &self,
        video_id: &str,
        original_url: &str,
    ) -> Result<CachedRawVideo> {
        let body = json!({
            "videoId": video_id,
            "context": {
                "client": {
                    "clientName": "IOS",
                    "clientVersion": IOS_INNERTUBE_CLIENT_VERSION,
                    "deviceMake": "Apple",
                    "deviceModel": "iPhone16,2",
                    "osName": "iPhone",
                    "osVersion": "18.3.2.22D82",
                    "hl": "es",
                    "gl": "EC",
                }
            },
            "contentCheckOk": true,
            "racyCheckOk": true,
        });

        let resp = self
            .innertube_client
            .post(INNERTUBE_PLAYER_URL)
            .header("User-Agent", IOS_INNERTUBE_USER_AGENT)
            .header("X-YouTube-Client-Name", "5")
            .header("X-YouTube-Client-Version", IOS_INNERTUBE_CLIENT_VERSION)
            .json(&body)
            .send()
            .await
            .map_err(|e| CoreError::External(format!("InnerTube IOS request failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(CoreError::External(format!(
                "InnerTube IOS returned HTTP {}",
                resp.status()
            )));
        }

        let data: Value = resp
            .json()
            .await
            .map_err(|e| CoreError::External(format!("InnerTube IOS JSON parse failed: {e}")))?;

        parse_innertube_player_json(&data, video_id, original_url)
    }

    async fn fetch_loudness_db(&self, url: &str) -> Option<f64> {
        let vid = extract_video_id(url)?;
        {
            let cache = self.loudness_cache.lock().expect("loudness cache lock");
            if let Some(cached) = cache.get(&vid) {
                return *cached;
            }
        }
        let watch_url = format!("https://www.youtube.com/watch?v={vid}");
        let result = match self.loudness_client.get(&watch_url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(html) => parse_loudness_db_from_html(&html),
                Err(_) => None,
            },
            _ => None,
        };
        let mut cache = self.loudness_cache.lock().expect("loudness cache lock");
        if cache.len() >= 512 {
            cache.clear();
        }
        cache.insert(vid, result);
        result
    }

    /// Resolves audio-only stream payload compatible with `/api/player/audio`.
    pub async fn resolve_audio(&self, url: &str) -> Result<Value> {
        self.resolve_audio_with_refresh(url, false).await
    }

    /// Resolves audio-only stream payload, optionally bypassing the stream cache.
    pub async fn resolve_audio_with_refresh(
        &self,
        url: &str,
        force_refresh: bool,
    ) -> Result<Value> {
        let info = self
            .resolve_stream_with_refresh(url, Some("audio"), true, force_refresh)
            .await?;
        let audio = info
            .audio
            .as_ref()
            .ok_or_else(|| CoreError::External("No audio stream found for URL".into()))?;
        Ok(json!({
            "item": info.item,
            "duration": info.duration,
            "audio": audio,
            "streamUrl": audio.url,
            "proxyUrl": audio.url,
            "loudnessDb": info.loudness_db,
            "expiresAt": info.expires_at,
        }))
    }

    /// Lists available audio tracks for a video URL (`/api/player/audio-tracks`).
    pub async fn resolve_audio_tracks(&self, url: &str) -> Result<Vec<AudioTrack>> {
        let info = self.resolve_stream(url, None, false).await?;
        Ok(info.audio_tracks)
    }

    /// Resolves playlist entries cleanly via `yt-dlp --flat-playlist --dump-single-json`.
    /// Pass `limit: None` to fetch all entries in the playlist without a cap.
    pub async fn resolve_playlist(
        &self,
        url: &str,
        limit: Option<usize>,
    ) -> Result<Vec<PlaylistItem>> {
        let trimmed = url.trim();
        validate_http_url(trimmed)?;
        let playlist_end = limit.map(|l| l.max(1));
        let raw = self.run_ytdlp_json(trimmed, true, playlist_end).await?;
        Ok(parse_ytdlp_playlist_json(&raw, trimmed))
    }

    async fn run_ytdlp_json(
        &self,
        url: &str,
        flat_playlist: bool,
        playlist_end: Option<usize>,
    ) -> Result<Value> {
        let ytdlp = self.resolve_ytdlp_bin();
        let cfg = self.config.snapshot();
        let cache_dir = cfg.data_dir.join("ytdlp_cache");
        let js_runtime = resolve_js_runtime();

        if flat_playlist {
            let cookie_file = self.resolve_cookie_file();
            let args = build_ytdlp_json_args(
                &cache_dir,
                js_runtime.as_ref(),
                cookie_file.as_deref(),
                url,
                true,
                playlist_end,
            );
            return self.run_ytdlp_json_once(&ytdlp, &args).await;
        }

        // Try unauthenticated web_safari/default first (avoids YouTube SABR/PO-token 403);
        // fall back to cookies when YouTube requires sign-in (bot check, age-restricted, private).
        let unauth_args = build_ytdlp_json_args(
            &cache_dir,
            js_runtime.as_ref(),
            None,
            url,
            false,
            None,
        );
        match self.run_ytdlp_json_once(&ytdlp, &unauth_args).await {
            Ok(val) => Ok(val),
            Err(first_err) => {
                if let Some(cookie_file) = self.resolve_cookie_file() {
                    let auth_args = build_ytdlp_json_args(
                        &cache_dir,
                        js_runtime.as_ref(),
                        Some(cookie_file.as_path()),
                        url,
                        false,
                        None,
                    );
                    self.run_ytdlp_json_once(&ytdlp, &auth_args).await
                } else {
                    Err(first_err)
                }
            }
        }
    }

    async fn run_ytdlp_json_once(&self, ytdlp: &str, args: &[String]) -> Result<Value> {
        let mut cmd = TokioCommand::new(ytdlp);
        cmd.args(args);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let output = cmd.output().await.map_err(|err| {
            CoreError::External(format!("Failed to execute yt-dlp ({ytdlp}): {err}"))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(CoreError::External(if stderr.is_empty() {
                format!("yt-dlp exited with status {}", output.status)
            } else {
                stderr
            }));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str::<Value>(stdout.trim())
            .map_err(|err| CoreError::External(format!("Invalid yt-dlp JSON output: {err}")))
    }

    fn get_cached_raw(&self, key: &str) -> Option<CachedRawVideo> {
        let now_unix = now_unix_secs();
        let mut cache = self.raw_cache.lock().expect("raw_cache lock");
        let entry = cache.get(key)?;
        if entry.cached_at.elapsed() > Duration::from_secs(DEFAULT_STREAM_CACHE_TTL_SECS) {
            cache.remove(key);
            return None;
        }
        if let Some(exp) = entry.expires_at {
            if now_unix >= exp - EXPIRE_SAFETY_MARGIN_SECS {
                cache.remove(key);
                return None;
            }
        }
        Some(entry.clone())
    }

    fn put_cached_raw(&self, key: String, entry: CachedRawVideo) {
        let mut cache = self.raw_cache.lock().expect("raw_cache lock");
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(key, entry);
    }

    fn emit_event(&self, kind: &str, state: PlaybackState) {
        let _ = self.events.send(PlayerEvent {
            kind: kind.to_string(),
            state,
        });
    }
}

/// Validates a playback target: HTTP(S) URLs pass through; local files must exist
/// and reside inside `media_root` (preventing path traversal).
pub fn validate_play_target(target: &str, media_root: &Path) -> Result<String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err(CoreError::Validation("Playback target is empty".into()));
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Url::parse(trimmed)?;
        return Ok(trimmed.to_string());
    }

    let candidate = PathBuf::from(trimmed);
    if !candidate.exists() {
        return Err(CoreError::NotFound(format!(
            "Local media file not found: {trimmed}"
        )));
    }
    let canonical_target = candidate.canonicalize()?;
    let canonical_root = media_root.canonicalize()?;
    if !canonical_target.starts_with(&canonical_root) {
        return Err(CoreError::Validation(
            "Local media path is outside MEDIA_ROOT".into(),
        ));
    }
    Ok(canonical_target.to_string_lossy().to_string())
}

/// Builds `yt-dlp --dump-single-json` CLI arguments for stream or playlist resolution.
fn build_ytdlp_json_args(
    cache_dir: &Path,
    js_runtime: Option<&(String, String)>,
    cookie_file: Option<&Path>,
    url: &str,
    flat_playlist: bool,
    playlist_end: Option<usize>,
) -> Vec<String> {
    let mut args = vec![
        "--dump-single-json".to_string(),
        "--no-warnings".to_string(),
        "--skip-download".to_string(),
        "--cache-dir".to_string(),
        cache_dir.to_string_lossy().to_string(),
    ];

    if let Some((rt_name, rt_path)) = js_runtime {
        args.push("--js-runtimes".to_string());
        args.push(format!("{rt_name}:{rt_path}"));
        args.push("--remote-components".to_string());
        args.push("ejs:github".to_string());
    }

    if flat_playlist {
        args.push("--flat-playlist".to_string());
        args.push("--ignore-errors".to_string());
        if let Some(end) = playlist_end {
            args.push("--playlist-end".to_string());
            args.push(end.to_string());
        }
    } else {
        args.push("--no-playlist".to_string());
        args.push("--extractor-args".to_string());
        args.push("youtube:player_client=web_safari,default".to_string());
    }

    if let Some(cf) = cookie_file {
        args.push("--cookies".to_string());
        args.push(cf.to_string_lossy().to_string());
    }

    args.push("--".to_string());
    args.push(url.to_string());
    args
}

/// Validates that a string is a safe `http://` or `https://` URL with a non-empty host
/// (blocking CLI flag injection via leading `-` and non-HTTP schemes like `file://`).
pub fn validate_http_url(raw: &str) -> Result<Url> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Err(CoreError::Validation("URL inválida".into()));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|_| CoreError::Validation("URL inválida".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CoreError::Validation(
            "Solo se permiten URLs http:// o https://".into(),
        ));
    }
    if parsed.host_str().map(|h| h.is_empty()).unwrap_or(true) {
        return Err(CoreError::Validation("URL sin host válido".into()));
    }
    Ok(parsed)
}

/// Builds a `yt-dlp` format selector matching `backend/player.py`.
pub fn format_selector(quality: &str, vlc: bool, audio_only: bool) -> String {
    if audio_only || quality.eq_ignore_ascii_case("audio") {
        if vlc {
            return "bestaudio[protocol*=m3u8]/bestaudio[acodec!=none]/bestaudio/best".into();
        }
        return "bestaudio[acodec!=none]/bestaudio/best".into();
    }
    let max_h = quality_to_height(quality);
    if vlc {
        if let Some(h) = max_h {
            return format!(
                "best[height<={h}][protocol*=m3u8]/bestvideo[height<={h}][protocol*=m3u8]+bestaudio[protocol*=m3u8]/best[vcodec^=avc1][acodec!=none][height<={h}]/best[ext=mp4][acodec!=none][height<={h}]/best[height<={h}]/best"
            );
        }
        return "best[protocol*=m3u8]/bestvideo[protocol*=m3u8]+bestaudio[protocol*=m3u8]/best[vcodec^=avc1][acodec!=none]/best[ext=mp4][acodec!=none]/best"
            .into();
    }
    if let Some(h) = max_h {
        format!(
            "bestvideo[height<={h}]+bestaudio/best[height<={h}]/best"
        )
    } else {
        "bestvideo+bestaudio/best".into()
    }
}

pub fn quality_to_height(quality: &str) -> Option<u32> {
    let normalized = quality.trim().to_lowercase();
    match normalized.as_str() {
        "best" | "auto" | "" => None,
        "4320p" | "8k" => Some(4320),
        "2160p" | "4k" => Some(2160),
        "1440p" | "2k" => Some(1440),
        "1080p" => Some(1080),
        "720p" => Some(720),
        "480p" => Some(480),
        "360p" => Some(360),
        "240p" => Some(240),
        "144p" => Some(144),
        other => {
            let digits: String = other.chars().filter(|c| c.is_ascii_digit()).collect();
            digits.parse::<u32>().ok().filter(|&h| h >= 144)
        }
    }
}

/// Builds CLI arguments for `mpv` (GPU hwdec, IPC, optional `--wid`, yt-dlp hook) or `vlc`.
#[allow(clippy::too_many_arguments)]
pub fn build_player_args(
    backend: PlayerBackend,
    target: &str,
    quality: &str,
    audio_only: bool,
    wid: Option<i64>,
    ipc_endpoint: Option<&str>,
    ytdlp_bin: &str,
    cookie_file: Option<&Path>,
) -> Vec<String> {
    match backend {
        PlayerBackend::Mpv => build_mpv_args_with_ytdlp(
            target,
            quality,
            audio_only,
            wid,
            ipc_endpoint,
            ytdlp_bin,
            None,
            resolve_js_runtime().as_ref(),
            cookie_file,
        ),
        PlayerBackend::Vlc => build_vlc_args(target, audio_only, None, None),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_mpv_args_with_ytdlp(
    target: &str,
    quality: &str,
    audio_only: bool,
    wid: Option<i64>,
    ipc_endpoint: Option<&str>,
    ytdlp_bin: &str,
    cache_dir: Option<&Path>,
    js_runtime: Option<&(String, String)>,
    cookie_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "--hwdec=auto-safe".to_string(),
        "--osd-level=1".to_string(),
        "--keep-open=no".to_string(),
        "--no-terminal".to_string(),
    ];
    if let Some(endpoint) = ipc_endpoint {
        args.push(format!("--input-ipc-server={endpoint}"));
    }
    if let Some(handle) = wid {
        args.push(format!("--wid={handle}"));
    }
    if audio_only {
        args.push("--no-video".to_string());
    }
    if is_youtube_url(target) {
        args.push(format!("--script-opts=ytdl_hook-ytdl_path={ytdlp_bin}"));
        args.push(format!(
            "--ytdl-format={}",
            format_selector(quality, false, audio_only)
        ));
        args.push("--ytdl-raw-options-append=extractor-args=youtube:player_client=web_safari".to_string());
        if let Some(cd) = cache_dir {
            args.push(format!("--ytdl-raw-options-append=cache-dir={}", cd.display()));
        }
        if let Some((rt_name, rt_path)) = js_runtime {
            args.push(format!(
                "--ytdl-raw-options-append=js-runtimes={rt_name}:{rt_path}"
            ));
            args.push("--ytdl-raw-options-append=remote-components=ejs:github".to_string());
        }
        if let Some(cf) = cookie_file {
            args.push(format!("--ytdl-raw-options-append=cookies={}", cf.display()));
        }
    }
    args.push("--".to_string());
    args.push(target.to_string());
    args
}

/// Builds CLI arguments for `vlc`, supporting optional adaptive audio slave and display title.
pub fn build_vlc_args(
    target: &str,
    audio_only: bool,
    audio_slave: Option<&str>,
    title: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if target.starts_with("http://") || target.starts_with("https://") {
        args.push("--network-caching=2500".to_string());
    }
    if audio_only {
        args.push("--no-video".to_string());
    }
    if let Some(slave) = audio_slave.filter(|s| !s.trim().is_empty()) {
        args.push(format!("--input-slave={slave}"));
    }
    if let Some(meta_title) = title.map(str::trim).filter(|s| !s.is_empty()) {
        args.push(format!("--meta-title={meta_title}"));
    }
    args.push("--".to_string());
    args.push(target.to_string());
    args
}

/// Auto-detects a supported JavaScript runtime (`node`, `deno`, `bun`) for `yt-dlp` YouTube EJS solving.
pub fn resolve_js_runtime() -> Option<(String, String)> {
    let candidates: Vec<(&str, PathBuf)> = {
        let mut list = Vec::new();
        #[cfg(windows)]
        {
            list.push(("node", PathBuf::from(r"C:\Program Files\nodejs\node.exe")));
            list.push(("node", PathBuf::from(r"C:\Program Files (x86)\nodejs\node.exe")));
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                list.push((
                    "node",
                    PathBuf::from(&local).join("Programs").join("nodejs").join("node.exe"),
                ));
            }
            if let Ok(profile) = std::env::var("USERPROFILE") {
                list.push(("deno", PathBuf::from(&profile).join(".deno").join("bin").join("deno.exe")));
                list.push(("bun", PathBuf::from(&profile).join(".bun").join("bin").join("bun.exe")));
            }
        }
        #[cfg(not(windows))]
        {
            list.push(("node", PathBuf::from("/usr/bin/node")));
            list.push(("node", PathBuf::from("/usr/local/bin/node")));
            list.push(("node", PathBuf::from("/opt/homebrew/bin/node")));
            list.push(("deno", PathBuf::from("/usr/local/bin/deno")));
            list.push(("bun", PathBuf::from("/usr/local/bin/bun")));
        }
        list
    };

    for name in ["node", "deno", "bun"] {
        if binary_exists(name) {
            return Some((name.to_string(), name.to_string()));
        }
    }
    for (name, path) in candidates {
        if path.is_file() {
            return Some((name.to_string(), path.to_string_lossy().to_string()));
        }
    }
    None
}

fn prepare_player_log_stdio(
    log_path: &Path,
    backend: PlayerBackend,
    exe: &str,
    args: &[String],
) -> (Stdio, Stdio) {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            let _ = writeln!(
                file,
                "[playzi] ts={} backend={} exe={} args={:?}",
                now_unix_secs(),
                backend.as_str(),
                exe,
                args
            );
            let _ = file.flush();
            match file.try_clone() {
                Ok(err_file) => (Stdio::from(file), Stdio::from(err_file)),
                Err(_) => (Stdio::from(file), Stdio::null()),
            }
        }
        Err(_) => (Stdio::null(), Stdio::null()),
    }
}

fn append_player_log(log_path: &Path, msg: &str) {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
    {
        use std::io::Write;
        let _ = file.write_all(msg.as_bytes());
    }
}

/// Resolves direct playable HTTP/HLS stream URL(s) for VLC via `yt-dlp -g`.
fn resolve_vlc_stream_urls(
    ytdlp_bin: &str,
    cache_dir: &Path,
    js_runtime: Option<&(String, String)>,
    cookie_file: Option<&Path>,
    url: &str,
    quality: &str,
    audio_only: bool,
) -> Result<(String, Option<String>)> {
    validate_http_url(url)?;
    let run_once = |cf: Option<&Path>| -> Result<(String, Option<String>)> {
        let mut cmd = StdCommand::new(ytdlp_bin);
        cmd.arg("--no-warnings")
            .arg("--no-playlist")
            .arg("--extractor-args")
            .arg("youtube:player_client=web_safari")
            .arg("--cache-dir")
            .arg(cache_dir)
            .arg("-f")
            .arg(format_selector(quality, true, audio_only))
            .arg("-g");

        if let Some((rt_name, rt_path)) = js_runtime {
            cmd.arg("--js-runtimes")
                .arg(format!("{rt_name}:{rt_path}"))
                .arg("--remote-components")
                .arg("ejs:github");
        }
        if let Some(cookie) = cf {
            cmd.arg("--cookies").arg(cookie);
        }
        cmd.arg("--")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = cmd.output().map_err(|err| {
            CoreError::External(format!(
                "Failed to execute yt-dlp ({ytdlp_bin}) for VLC playback: {err}"
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(CoreError::External(if stderr.is_empty() {
                format!("yt-dlp could not resolve a VLC stream (exit {})", output.status)
            } else {
                format!("yt-dlp could not resolve a VLC stream: {stderr}")
            }));
        }

        let urls: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("http://") || line.starts_with("https://"))
            .map(ToString::to_string)
            .collect();

        let Some(primary) = urls.first().cloned() else {
            return Err(CoreError::External(
                "yt-dlp returned no playable HTTP stream URL for VLC".into(),
            ));
        };

        Ok((primary, urls.get(1).cloned()))
    };

    // Try unauthenticated web_safari HLS first (avoids YouTube SABR/PO-token 403);
    // fall back to cookies only when needed (e.g. age-restricted/private videos).
    match run_once(None) {
        Ok(res) => Ok(res),
        Err(first_err) => {
            if let Some(cf) = cookie_file {
                run_once(Some(cf)).or(Err(first_err))
            } else {
                Err(first_err)
            }
        }
    }
}

/// Converts a high-level `PlayerControlCommand` into an `mpv` JSON IPC request payload.
pub fn build_mpv_ipc_command(cmd: &PlayerControlCommand) -> Value {
    match cmd {
        PlayerControlCommand::Pause { paused } => json!({
            "command": ["set_property", "pause", paused]
        }),
        PlayerControlCommand::TogglePause => json!({
            "command": ["cycle", "pause"]
        }),
        PlayerControlCommand::Seek { seconds, relative } => json!({
            "command": ["seek", seconds, if *relative { "relative" } else { "absolute" }]
        }),
        PlayerControlCommand::SetVolume { volume } => json!({
            "command": ["set_property", "volume", volume.clamp(0.0, 150.0)]
        }),
        PlayerControlCommand::SetMute { muted } => json!({
            "command": ["set_property", "mute", muted]
        }),
        PlayerControlCommand::SetAudioTrack { track } => json!({
            "command": ["set_property", "aid", track]
        }),
        PlayerControlCommand::LoadFile { url, .. } => json!({
            "command": ["loadfile", url, "replace"]
        }),
        PlayerControlCommand::Stop => json!({
            "command": ["quit"]
        }),
    }
}

pub fn build_ipc_endpoint(session_id: &str) -> String {
    let safe_id: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if cfg!(windows) {
        format!(r"\\.\pipe\playzi-mpv-{safe_id}")
    } else {
        format!("/tmp/playzi-mpv-{safe_id}.sock")
    }
}

/// Sends a single newline-delimited JSON command to an active `mpv` IPC pipe/socket.
pub fn send_mpv_ipc_json(endpoint: &str, payload: &Value) -> Result<()> {
    let mut line = serde_json::to_string(payload)?;
    line.push('\n');

    #[cfg(windows)]
    {
        use std::fs::OpenOptions;
        use std::io::Write;
        const ERROR_PIPE_BUSY: i32 = 231;

        let mut last_err = None;
        for attempt in 0..15 {
            match OpenOptions::new().write(true).open(endpoint) {
                Ok(mut pipe) => {
                    pipe.write_all(line.as_bytes())
                        .map_err(|err| CoreError::External(format!("mpv IPC write error: {err}")))?;
                    pipe.flush()
                        .map_err(|err| CoreError::External(format!("mpv IPC flush error: {err}")))?;
                    return Ok(());
                }
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempt < 14 => {
                    last_err = Some(err);
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    last_err = Some(err);
                    break;
                }
            }
        }
        Err(CoreError::External(format!(
            "mpv IPC pipe error ({endpoint}): {}",
            last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".into())
        )))
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(endpoint)
            .map_err(|err| CoreError::External(format!("mpv IPC socket error ({endpoint}): {err}")))?;
        stream
            .write_all(line.as_bytes())
            .map_err(|err| CoreError::External(format!("mpv IPC write error: {err}")))?;
        stream
            .flush()
            .map_err(|err| CoreError::External(format!("mpv IPC flush error: {err}")))?;
        Ok(())
    }
}

/// Parses `yt-dlp --dump-single-json` output into normalized `ResolvedStreamInfo`.
fn parse_ytdlp_raw_video(raw: &Value, original_url: &str) -> CachedRawVideo {
    let duration = raw
        .get("duration")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let id = raw
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| extract_video_id(original_url))
        .unwrap_or_default();
    let title = raw
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(original_url)
        .to_string();
    let channel = raw
        .get("channel")
        .or_else(|| raw.get("uploader"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let thumbnail = raw
        .get("thumbnail")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if !id.is_empty() {
                format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
            } else {
                String::new()
            }
        });
    let webpage_url = raw
        .get("webpage_url")
        .and_then(Value::as_str)
        .unwrap_or(original_url)
        .to_string();

    let formats: Vec<StreamFormat> = raw
        .get("formats")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_format_value).collect())
        .unwrap_or_default();

    let direct_url = raw
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("http"))
        .map(str::to_string);

    let expires_at = formats
        .iter()
        .filter_map(|f| extract_url_expire(&f.url))
        .chain(direct_url.as_deref().and_then(extract_url_expire))
        .min();

    let item = json!({
        "id": id,
        "title": title,
        "channel": channel,
        "thumbnail": thumbnail,
        "url": webpage_url,
        "duration": duration as u64,
        "durationText": format_duration((duration > 0.0).then_some(duration as u64)),
    });

    let loudness_db = raw
        .get("loudnessDb")
        .or_else(|| raw.get("loudness_db"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (-60.0..=30.0).contains(v));

    CachedRawVideo {
        item,
        duration,
        formats,
        direct_url,
        loudness_db,
        expires_at,
        cached_at: Instant::now(),
    }
}

pub fn parse_ytdlp_info_json(
    raw: &Value,
    original_url: &str,
    quality: &str,
    audio_only: bool,
) -> ResolvedStreamInfo {
    parse_ytdlp_raw_video(raw, original_url).project(quality, audio_only)
}

fn parse_innertube_range(obj: Option<&Value>) -> Option<String> {
    let map = obj?.as_object()?;
    let start = map
        .get("start")
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))?;
    let end = map
        .get("end")
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string())))?;
    if start.is_empty() || end.is_empty() {
        return None;
    }
    Some(format!("{start}-{end}"))
}

fn parse_innertube_mime(raw_mime: &str) -> (String, String, String, String) {
    let mut parts = raw_mime.split(';');
    let base_mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let ext = match base_mime.as_str() {
        "video/mp4" => "mp4",
        "audio/mp4" => "m4a",
        "video/webm" | "audio/webm" => "webm",
        _ => "",
    }
    .to_string();

    let mut codecs_str = String::new();
    for part in parts {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("codecs=") {
            codecs_str = rest.trim_matches('"').trim_matches('\'').trim().to_string();
            break;
        }
    }

    let (vcodec, acodec) = if base_mime.starts_with("video/") {
        if let Some((v, a)) = codecs_str.split_once(',') {
            (v.trim().to_string(), a.trim().to_string())
        } else if !codecs_str.is_empty() {
            (codecs_str, "none".to_string())
        } else {
            ("unknown".to_string(), "none".to_string())
        }
    } else if base_mime.starts_with("audio/") {
        (
            "none".to_string(),
            if codecs_str.is_empty() {
                "unknown".to_string()
            } else {
                codecs_str
            },
        )
    } else {
        ("none".to_string(), "none".to_string())
    };

    (base_mime, ext, vcodec, acodec)
}

fn parse_innertube_format(v: &Value) -> Option<StreamFormat> {
    let url = v.get("url").and_then(Value::as_str)?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    let raw_mime = v.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let (mime_type, ext, vcodec, acodec) = parse_innertube_mime(raw_mime);
    let itag = v.get("itag").and_then(Value::as_u64).unwrap_or(0);
    let is_drc = v.get("isDrc").and_then(Value::as_bool).unwrap_or(false);
    let format_id = if is_drc {
        format!("{itag}-drc")
    } else {
        itag.to_string()
    };

    let bitrate_kbps = v
        .get("averageBitrate")
        .or_else(|| v.get("bitrate"))
        .and_then(Value::as_f64)
        .map(|b| b / 1000.0);
    let is_audio_only = vcodec == "none" && acodec != "none";

    let audio_track = v.get("audioTrack");
    let language = audio_track
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(|s| s.split('.').next().unwrap_or(s).to_string())
        .filter(|s| !s.is_empty());
    let language_name = audio_track
        .and_then(|t| t.get("displayName"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let asr = v.get("audioSampleRate").and_then(|n| {
        n.as_u64()
            .map(|x| x as u32)
            .or_else(|| n.as_str().and_then(|s| s.parse::<u32>().ok()))
    });
    let audio_channels = v
        .get("audioChannels")
        .and_then(Value::as_u64)
        .map(|n| n as u32);

    let mut http_headers = HashMap::new();
    http_headers.insert("User-Agent".into(), IOS_INNERTUBE_USER_AGENT.into());

    Some(StreamFormat {
        format_id,
        url: url.to_string(),
        ext,
        vcodec,
        acodec,
        width: v.get("width").and_then(Value::as_u64).map(|n| n as u32),
        height: v.get("height").and_then(Value::as_u64).map(|n| n as u32),
        fps: v.get("fps").and_then(Value::as_f64),
        tbr: bitrate_kbps,
        abr: if is_audio_only { bitrate_kbps } else { None },
        language,
        language_name,
        mime_type: if mime_type.is_empty() {
            None
        } else {
            Some(mime_type)
        },
        init_range: parse_innertube_range(v.get("initRange")),
        index_range: parse_innertube_range(v.get("indexRange")),
        asr,
        audio_channels,
        http_headers,
    })
}

fn parse_innertube_player_json(
    data: &Value,
    video_id: &str,
    original_url: &str,
) -> Result<CachedRawVideo> {
    let status = data
        .get("playabilityStatus")
        .and_then(|p| p.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if status != "OK" {
        return Err(CoreError::External(format!(
            "InnerTube playabilityStatus={status}"
        )));
    }

    let details = data.get("videoDetails").ok_or_else(|| {
        CoreError::External("InnerTube response missing videoDetails".into())
    })?;
    if details
        .get("isLive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(CoreError::External(
            "Live streams fall back to yt-dlp HLS".into(),
        ));
    }

    let streaming = data.get("streamingData").ok_or_else(|| {
        CoreError::External("InnerTube response missing streamingData".into())
    })?;

    let mut formats = Vec::new();
    for key in ["formats", "adaptiveFormats"] {
        if let Some(arr) = streaming.get(key).and_then(Value::as_array) {
            for entry in arr {
                if let Some(fmt) = parse_innertube_format(entry) {
                    formats.push(fmt);
                }
            }
        }
    }

    let has_dash_audio = formats
        .iter()
        .any(|f| (f.vcodec.is_empty() || f.vcodec == "none") && is_dash_format(f));
    let has_dash_video = formats
        .iter()
        .any(|f| !f.vcodec.is_empty() && f.vcodec != "none" && is_dash_format(f));
    let has_muxed = formats.iter().any(|f| {
        !f.vcodec.is_empty()
            && f.vcodec != "none"
            && !f.acodec.is_empty()
            && f.acodec != "none"
    });
    if !(has_dash_audio && has_dash_video) && !has_muxed {
        return Err(CoreError::External(
            "InnerTube response has no playable DASH or muxed formats".into(),
        ));
    }

    let duration = details
        .get("lengthSeconds")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or_else(|| v.as_f64())
        })
        .unwrap_or(0.0);
    let id = details
        .get("videoId")
        .and_then(Value::as_str)
        .unwrap_or(video_id)
        .to_string();
    let title = details
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(original_url)
        .to_string();
    let channel = details
        .get("author")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let thumbnail = details
        .get("thumbnail")
        .and_then(|t| t.get("thumbnails"))
        .and_then(Value::as_array)
        .and_then(|arr| arr.last())
        .and_then(|t| t.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));

    let loudness_db = data
        .get("playerConfig")
        .and_then(|p| p.get("audioConfig"))
        .and_then(|a| a.get("loudnessDb"))
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (-60.0..=30.0).contains(v));

    let expires_at = formats
        .iter()
        .filter_map(|f| extract_url_expire(&f.url))
        .min();

    let item = json!({
        "id": id,
        "title": title,
        "channel": channel,
        "thumbnail": thumbnail,
        "url": original_url,
        "duration": duration as u64,
        "durationText": format_duration((duration > 0.0).then_some(duration as u64)),
    });

    Ok(CachedRawVideo {
        item,
        duration,
        formats,
        direct_url: None,
        loudness_db,
        expires_at,
        cached_at: Instant::now(),
    })
}

/// Extracts YouTube's `loudnessDb` (relative to -14 LUFS) from a watch page HTML response.
/// Falls back to `perceptualLoudnessDb + 14.0` when `loudnessDb` is absent.
pub fn parse_loudness_db_from_html(html: &str) -> Option<f64> {
    fn extract_json_f64(haystack: &str, key: &str) -> Option<f64> {
        let mut search = haystack;
        while let Some(idx) = search.find(key) {
            let after_key = &search[idx + key.len()..];
            let trimmed = after_key.trim_start();
            if let Some(rest) = trimmed.strip_prefix(':') {
                let num_start = rest.trim_start();
                let end = num_start
                    .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
                    .unwrap_or(num_start.len());
                if end > 0 {
                    if let Ok(v) = num_start[..end].parse::<f64>() {
                        if v.is_finite() && (-60.0..=30.0).contains(&v) {
                            return Some(v);
                        }
                    }
                }
            }
            search = &search[idx + key.len()..];
        }
        None
    }

    extract_json_f64(html, "\"loudnessDb\"").or_else(|| {
        extract_json_f64(html, "\"perceptualLoudnessDb\"")
            .map(|p| p + 14.0)
            .filter(|v| v.is_finite() && (-60.0..=30.0).contains(v))
    })
}

/// Parses `yt-dlp --flat-playlist --dump-single-json` output into normalized `PlaylistItem`s.
pub fn parse_ytdlp_playlist_json(raw: &Value, fallback_url: &str) -> Vec<PlaylistItem> {
    let entries = match raw.get("entries").and_then(Value::as_array) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let is_music = fallback_url.contains("music.youtube.com");
    let mut items = Vec::new();
    for entry in entries {
        if entry.is_null() {
            continue;
        }
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let raw_url = entry
            .get("webpage_url")
            .or_else(|| entry.get("url"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let url = if raw_url.starts_with("http://") || raw_url.starts_with("https://") {
            raw_url.to_string()
        } else if !id.is_empty() {
            if is_music {
                format!("https://music.youtube.com/watch?v={id}")
            } else {
                format!("https://www.youtube.com/watch?v={id}")
            }
        } else if !raw_url.is_empty() {
            format!("https://www.youtube.com/watch?v={raw_url}")
        } else {
            continue;
        };

        let title = entry
            .get("title")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(&url)
            .to_string();
        let channel = entry
            .get("channel")
            .or_else(|| entry.get("uploader"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let thumbnail = entry
            .get("thumbnail")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                entry
                    .get("thumbnails")
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.last())
                    .and_then(|t| t.get("url"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| {
                if !id.is_empty() {
                    format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")
                } else {
                    String::new()
                }
            });
        let duration_secs = entry
            .get("duration")
            .and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.round() as u64)));
        let duration_text = format_duration(duration_secs);

        items.push(PlaylistItem {
            id,
            url,
            title,
            channel,
            thumbnail,
            duration_text,
        });
    }
    items
}

fn parse_format_value(v: &Value) -> Option<StreamFormat> {
    let url = v.get("url").and_then(Value::as_str)?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    let mut http_headers = HashMap::new();
    if let Some(obj) = v.get("http_headers").and_then(Value::as_object) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                http_headers.insert(k.clone(), s.to_string());
            }
        }
    }
    http_headers
        .entry("User-Agent".into())
        .or_insert_with(|| DEFAULT_USER_AGENT.into());

    Some(StreamFormat {
        format_id: v
            .get("format_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: url.to_string(),
        ext: v
            .get("ext")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        vcodec: v
            .get("vcodec")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
        acodec: v
            .get("acodec")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_string(),
        width: v.get("width").and_then(Value::as_u64).map(|n| n as u32),
        height: v.get("height").and_then(Value::as_u64).map(|n| n as u32),
        fps: v.get("fps").and_then(Value::as_f64),
        tbr: v.get("tbr").and_then(Value::as_f64),
        abr: v.get("abr").and_then(Value::as_f64),
        language: v
            .get("language")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        language_name: v
            .get("format_note")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        mime_type: None,
        init_range: None,
        index_range: None,
        asr: v.get("asr").and_then(Value::as_u64).map(|n| n as u32),
        audio_channels: v
            .get("audio_channels")
            .and_then(Value::as_u64)
            .map(|n| n as u32),
        http_headers,
    })
}

fn is_hls_url(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains(".m3u8") || lower.contains("hls_playlist") || lower.contains("/manifest/hls")
}

fn is_drc_format(f: &StreamFormat) -> bool {
    f.format_id.to_ascii_lowercase().ends_with("-drc")
}

fn select_best_hls_audio(formats: &[StreamFormat]) -> Option<StreamFormat> {
    let all_hls: Vec<&StreamFormat> = formats
        .iter()
        .filter(|f| {
            is_hls_url(&f.url)
                && (f.vcodec.is_empty() || f.vcodec == "none")
                && !f.ext.eq_ignore_ascii_case("mhtml")
        })
        .collect();
    let non_drc: Vec<&StreamFormat> = all_hls
        .iter()
        .copied()
        .filter(|f| !is_drc_format(f))
        .collect();
    let pool = if !non_drc.is_empty() { non_drc } else { all_hls };
    pool.into_iter()
        .max_by(|a, b| {
            let sa = a.abr.or(a.tbr).unwrap_or(0.0);
            let sb = b.abr.or(b.tbr).unwrap_or(0.0);
            sa.partial_cmp(&sb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    let ia = a.format_id.parse::<u32>().unwrap_or(0);
                    let ib = b.format_id.parse::<u32>().unwrap_or(0);
                    ia.cmp(&ib)
                })
        })
        .cloned()
}

fn is_isobmff_container(f: &StreamFormat) -> bool {
    if let Some(mt) = f.mime_type.as_deref() {
        let mt_lc = mt.to_ascii_lowercase();
        if mt_lc.starts_with("video/mp4") || mt_lc.starts_with("audio/mp4") {
            return true;
        }
        if mt_lc.starts_with("video/webm") || mt_lc.starts_with("audio/webm") {
            return false;
        }
    }
    matches!(f.ext.to_ascii_lowercase().as_str(), "mp4" | "m4a")
}

fn is_dash_format(f: &StreamFormat) -> bool {
    // Video.js VHS requires ISOBMFF sidx boxes for SegmentBase DASH manifests;
    // WebM uses EBML Cues which fails with MEDIA_ERR_SRC_NOT_SUPPORTED.
    !is_hls_url(&f.url)
        && is_isobmff_container(f)
        && f.init_range.as_deref().is_some_and(|s| !s.is_empty())
        && f.index_range.as_deref().is_some_and(|s| !s.is_empty())
}

fn video_compat_tier(f: &StreamFormat) -> u8 {
    let is_hls = is_hls_url(&f.url);
    let is_dash = is_dash_format(f);
    let has_audio = !f.acodec.is_empty() && f.acodec != "none";
    let vcodec = f.vcodec.to_lowercase();
    if is_hls && has_audio {
        5
    } else if (is_hls || is_dash) && vcodec.starts_with("avc1") {
        4
    } else if (is_hls || is_dash)
        && (vcodec.starts_with("vp09.00") || vcodec == "vp09" || vcodec == "vp9")
    {
        // Prefer 8-bit SDR VP9 (Profile 0) over 10-bit HDR VP9 (Profile 2) for universal MSE playback.
        3
    } else if (is_hls || is_dash) && (vcodec.starts_with("vp09") || vcodec.starts_with("vp9")) {
        2
    } else if is_hls || is_dash {
        1
    } else {
        0
    }
}

fn collect_playable_videos<'a>(
    formats: &'a [StreamFormat],
    has_hls_audio: bool,
    has_dash_audio: bool,
) -> Vec<&'a StreamFormat> {
    let all_video: Vec<&StreamFormat> = formats
        .iter()
        .filter(|f| !f.vcodec.is_empty() && f.vcodec != "none")
        .collect();

    // Include all playable HLS formats: muxed HLS (m3u8 with audio) plus video-only HLS
    // (including 1440p/2160p VP9/AV1 HLS) whenever a separate HLS audio playlist is available.
    let hls_playable: Vec<&StreamFormat> = all_video
        .iter()
        .copied()
        .filter(|f| {
            is_hls_url(&f.url)
                && (has_hls_audio || (!f.acodec.is_empty() && f.acodec != "none"))
        })
        .collect();
    if !hls_playable.is_empty() {
        return hls_playable;
    }

    // Include DASH adaptive video-only streams (InnerTube IOS fast path) when DASH audio exists.
    if has_dash_audio {
        let dash_playable: Vec<&StreamFormat> = all_video
            .iter()
            .copied()
            .filter(|f| is_dash_format(f))
            .collect();
        if !dash_playable.is_empty() {
            return dash_playable;
        }
    }

    // Otherwise prefer muxed direct HTTP (e.g. itag 18 mp4) over silent video-only streams.
    let http_muxed: Vec<&StreamFormat> = all_video
        .iter()
        .copied()
        .filter(|f| !is_hls_url(&f.url) && !f.acodec.is_empty() && f.acodec != "none")
        .collect();
    if !http_muxed.is_empty() {
        return http_muxed;
    }

    let hls_any: Vec<&StreamFormat> = all_video
        .iter()
        .copied()
        .filter(|f| is_hls_url(&f.url))
        .collect();
    if !hls_any.is_empty() {
        return hls_any;
    }

    all_video
}

fn extract_available_qualities(
    formats: &[StreamFormat],
    has_hls_audio: bool,
    has_dash_audio: bool,
) -> Vec<String> {
    let playable = collect_playable_videos(formats, has_hls_audio, has_dash_audio);
    let mut heights: Vec<u32> = playable
        .into_iter()
        .filter_map(|f| f.height)
        .filter(|&h| h >= 144)
        .collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();
    heights.into_iter().map(|h| format!("{h}p")).collect()
}

fn select_best_video(
    formats: &[StreamFormat],
    max_height: Option<u32>,
    has_hls_audio: bool,
    has_dash_audio: bool,
) -> Option<StreamFormat> {
    let mut candidates = collect_playable_videos(formats, has_hls_audio, has_dash_audio);
    if let Some(limit) = max_height {
        let within: Vec<&StreamFormat> = candidates
            .iter()
            .copied()
            .filter(|f| f.height.unwrap_or(0) <= limit)
            .collect();
        if !within.is_empty() {
            candidates = within;
        }
    }
    candidates
        .into_iter()
        .max_by(|a, b| {
            let ha = a.height.unwrap_or(0);
            let hb = b.height.unwrap_or(0);
            ha.cmp(&hb)
                .then_with(|| video_compat_tier(a).cmp(&video_compat_tier(b)))
                .then_with(|| {
                    a.fps
                        .unwrap_or(0.0)
                        .partial_cmp(&b.fps.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| {
                    a.tbr
                        .unwrap_or(0.0)
                        .partial_cmp(&b.tbr.unwrap_or(0.0))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })
        .cloned()
}

fn select_best_audio(formats: &[StreamFormat]) -> Option<StreamFormat> {
    // 1. Prefer audio-only HLS (e.g. itag 234/233) first: YouTube's HLS audio manifests
    // consistently return HTTP 200 without DASH 403 throttling on direct range probes.
    if let Some(hls_aud) = select_best_hls_audio(formats) {
        return Some(hls_aud);
    }
    let audio_only: Vec<&StreamFormat> = formats
        .iter()
        .filter(|f| {
            (f.vcodec.is_empty() || f.vcodec == "none")
                && !f.acodec.is_empty()
                && f.acodec != "none"
        })
        .collect();
    let pool = if !audio_only.is_empty() {
        audio_only
    } else {
        let hls_audio: Vec<&StreamFormat> = formats
            .iter()
            .filter(|f| is_hls_url(&f.url) && !f.acodec.is_empty() && f.acodec != "none")
            .collect();
        if !hls_audio.is_empty() {
            hls_audio
        } else {
            formats
                .iter()
                .filter(|f| !f.acodec.is_empty() && f.acodec != "none")
                .collect()
        }
    };

    let non_drc: Vec<&StreamFormat> = pool
        .iter()
        .copied()
        .filter(|f| !is_drc_format(f))
        .collect();
    let non_drc_pool = if !non_drc.is_empty() { non_drc } else { pool };

    // Prefer ISOBMFF DASH audio (audio/mp4 / m4a with init+index ranges) over WebM/Opus
    // so DASH MPD manifests always pair ISOBMFF video + ISOBMFF audio for Video.js VHS.
    let dash_mp4: Vec<&StreamFormat> = non_drc_pool
        .iter()
        .copied()
        .filter(|f| is_dash_format(f))
        .collect();
    let final_pool = if !dash_mp4.is_empty() {
        dash_mp4
    } else {
        non_drc_pool
    };

    final_pool
        .into_iter()
        .max_by(|a, b| {
            let sa = a.abr.or(a.tbr).unwrap_or(0.0);
            let sb = b.abr.or(b.tbr).unwrap_or(0.0);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
}

/// Response payload returned by the local `playzi-stream://` proxy for WebView2 Video.js playback.
#[derive(Debug, Clone)]
pub struct ProxiedStreamResponse {
    pub status: u16,
    pub content_type: String,
    pub content_range: Option<String>,
    pub accept_ranges: Option<String>,
    pub bytes: Vec<u8>,
}

/// Builds a reusable `reqwest::Client` configured for proxying YouTube HLS/DASH playlists and segments.
pub fn build_stream_proxy_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .pool_max_idle_per_host(12)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|err| CoreError::External(format!("Failed to build stream proxy client: {err}")))
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Builds a static DASH `isoff-on-demand:2011` `.mpd` manifest combining a video-only and
/// audio-only adaptive stream through the local `playzi-stream` proxy.
pub fn build_dash_mpd_manifest(
    duration: f64,
    video: &StreamFormat,
    audio: &StreamFormat,
    proxy_prefix: &str,
) -> Result<String> {
    if !is_isobmff_container(video) || !is_isobmff_container(audio) {
        return Err(CoreError::External(
            "DASH manifest requires ISOBMFF (mp4/m4a) video and audio streams".into(),
        ));
    }
    let v_init = video
        .init_range
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CoreError::External("Video stream missing DASH initRange".into()))?;
    let v_index = video
        .index_range
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CoreError::External("Video stream missing DASH indexRange".into()))?;
    let a_init = audio
        .init_range
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CoreError::External("Audio stream missing DASH initRange".into()))?;
    let a_index = audio
        .index_range
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CoreError::External("Audio stream missing DASH indexRange".into()))?;

    let dur = if duration > 0.0 && duration.is_finite() {
        duration
    } else {
        36000.0
    };
    let video_mime = video.mime_type.as_deref().unwrap_or_else(|| {
        if video.ext.eq_ignore_ascii_case("webm") {
            "video/webm"
        } else {
            "video/mp4"
        }
    });
    let audio_mime = audio.mime_type.as_deref().unwrap_or_else(|| {
        if audio.ext.eq_ignore_ascii_case("webm") {
            "audio/webm"
        } else {
            "audio/mp4"
        }
    });
    let video_codec = xml_escape(if video.vcodec.is_empty() || video.vcodec == "none" {
        "avc1.640028"
    } else {
        &video.vcodec
    });
    let audio_codec = xml_escape(if audio.acodec.is_empty() || audio.acodec == "none" {
        "mp4a.40.2"
    } else {
        &audio.acodec
    });
    let video_url = xml_escape(&encode_proxy_url(proxy_prefix, &video.url));
    let audio_url = xml_escape(&encode_proxy_url(proxy_prefix, &audio.url));
    let width = video.width.unwrap_or(1280);
    let height = video.height.unwrap_or(720);
    let fps = video.fps.unwrap_or(30.0).round().max(1.0) as u32;
    let bw_v = ((video.tbr.unwrap_or(2000.0) * 1000.0).round() as u64).max(100_000);
    let bw_a = ((audio.abr.or(audio.tbr).unwrap_or(128.0) * 1000.0).round() as u64).max(32_000);
    let asr = audio.asr.unwrap_or(44100);
    let channels = audio.audio_channels.unwrap_or(2);
    let v_id = xml_escape(if video.format_id.is_empty() {
        "video"
    } else {
        &video.format_id
    });
    let a_id = xml_escape(if audio.format_id.is_empty() {
        "audio"
    } else {
        &audio.format_id
    });

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT{dur:.3}S" minBufferTime="PT2S" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011">
  <Period duration="PT{dur:.3}S">
    <AdaptationSet id="video" contentType="video" mimeType="{video_mime}" codecs="{video_codec}" width="{width}" height="{height}" frameRate="{fps}" startWithSAP="1" subsegmentAlignment="true">
      <Representation id="{v_id}" bandwidth="{bw_v}" width="{width}" height="{height}">
        <BaseURL>{video_url}</BaseURL>
        <SegmentBase indexRange="{v_index}" indexRangeExact="true">
          <Initialization range="{v_init}" />
        </SegmentBase>
      </Representation>
    </AdaptationSet>
    <AdaptationSet id="audio" contentType="audio" mimeType="{audio_mime}" codecs="{audio_codec}" startWithSAP="1" subsegmentAlignment="true">
      <Representation id="{a_id}" bandwidth="{bw_a}" audioSamplingRate="{asr}">
        <AudioChannelConfiguration schemeIdUri="urn:mpeg:dash:23003:3:audio_channel_configuration:2011" value="{channels}" />
        <BaseURL>{audio_url}</BaseURL>
        <SegmentBase indexRange="{a_index}" indexRangeExact="true">
          <Initialization range="{a_init}" />
        </SegmentBase>
      </Representation>
    </AdaptationSet>
  </Period>
</MPD>
"#
    ))
}

fn encode_proxy_url(proxy_prefix: &str, absolute_url: &str) -> String {
    let encoded: String = url::form_urlencoded::byte_serialize(absolute_url.as_bytes()).collect();
    format!("{proxy_prefix}?url={encoded}")
}

/// Optional stream metadata embedded into the synthetic HLS Master Playlist
/// (`CODECS`, `RESOLUTION`, `BANDWIDTH`) so Video.js VHS configures MSE properly
/// for fMP4 VP9/AV1 streams above 1080p.
#[derive(Debug, Clone, Default)]
pub struct HlsMasterMeta {
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bandwidth: Option<u64>,
}

fn sanitize_hls_codec(raw: Option<&str>) -> Option<String> {
    let s = raw?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("unknown") {
        return None;
    }
    let cleaned: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Builds a synthetic HLS Master Playlist combining separate video-only and audio-only HLS
/// media playlists through the local `playzi-stream` proxy so Video.js plays both tracks together.
pub fn build_hls_master_playlist(
    video_url: &str,
    audio_url: &str,
    proxy_prefix: &str,
) -> String {
    build_hls_master_playlist_with_meta(
        video_url,
        audio_url,
        proxy_prefix,
        &HlsMasterMeta::default(),
    )
}

/// Builds a synthetic HLS Master Playlist with explicit `CODECS`, `RESOLUTION`, and `BANDWIDTH`
/// attributes. Without `CODECS`, Video.js VHS falls back to probing the fMP4 `stsd` box which
/// yields bare `vp09` (rejected by Chromium MSE), breaking 1440p/2160p playback.
pub fn build_hls_master_playlist_with_meta(
    video_url: &str,
    audio_url: &str,
    proxy_prefix: &str,
    meta: &HlsMasterMeta,
) -> String {
    let proxied_audio = encode_proxy_url(proxy_prefix, audio_url);
    let proxied_video = encode_proxy_url(proxy_prefix, video_url);
    let bandwidth = meta.bandwidth.filter(|&b| b > 0).unwrap_or(5_000_000);

    let mut stream_inf = format!("BANDWIDTH={bandwidth}");
    if let (Some(w), Some(h)) = (
        meta.width.filter(|&v| v > 0),
        meta.height.filter(|&v| v > 0),
    ) {
        stream_inf.push_str(&format!(",RESOLUTION={w}x{h}"));
    }
    if let Some(vc) = sanitize_hls_codec(meta.vcodec.as_deref()) {
        let ac = sanitize_hls_codec(meta.acodec.as_deref())
            .unwrap_or_else(|| "mp4a.40.2".to_string());
        stream_inf.push_str(&format!(",CODECS=\"{vc},{ac}\""));
    }
    stream_inf.push_str(",AUDIO=\"aud\"");

    format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:6\n\
         #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"Default\",DEFAULT=YES,AUTOSELECT=YES,URI=\"{proxied_audio}\"\n\
         #EXT-X-STREAM-INF:{stream_inf}\n\
         {proxied_video}\n"
    )
}

/// Detects YouTube muxed HLS (`itag 91..96`) trailing segments that start at or after the end of
/// the video track (`sgovp` `dur`) so they contain only trailing AAC audio packets and 0 video
/// packets. Video.js VHS (`checkForIllegalMediaSwitch`) treats such a segment in a muxed video
/// playlist as a fatal `playlistExclusionDuration: Infinity` error ("Playback cannot continue").
fn is_youtube_muxed_audio_only_tail_segment(
    seg_url: &str,
    extinf_secs: Option<f64>,
    is_last_vod_segment: bool,
) -> bool {
    let lower = seg_url.to_ascii_lowercase();
    if !lower.contains("sgovp/") || !lower.contains("sgoap/") {
        return false;
    }

    // Extract video track duration from `sgovp/...dur%3D<secs>` or `dur=<secs>`.
    let video_dur_ms = lower.find("sgovp/").and_then(|pos| {
        let rest = &lower[pos + 6..];
        let seg = rest.split('/').next()?;
        let dur_part = seg
            .find("dur%3d")
            .map(|i| &seg[i + 6..])
            .or_else(|| seg.find("dur=").map(|i| &seg[i + 4..]))?;
        let num_str: String = dur_part
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        num_str.parse::<f64>().ok().map(|s| s * 1000.0)
    });

    // Extract segment start offset from `/begin/<ms>/`.
    let begin_ms = lower.find("/begin/").and_then(|pos| {
        let rest = &lower[pos + 7..];
        let num_str = rest.split('/').next()?;
        num_str.parse::<f64>().ok()
    });

    if let (Some(v_ms), Some(b_ms)) = (video_dur_ms, begin_ms) {
        if b_ms + 1.0 >= v_ms {
            return true;
        }
    }

    // Fallback: if this is the final VOD segment of a YouTube muxed playlist and is sub-250ms,
    // it is an audio-overhang tail with 0 video frames.
    is_last_vod_segment && extinf_secs.is_some_and(|d| d < 0.25)
}

fn parse_extinf_duration(line: &str) -> Option<f64> {
    let rest = line.trim().strip_prefix("#EXTINF:")?;
    let dur_str = rest.split(',').next()?.trim();
    dur_str.parse::<f64>().ok()
}

/// Rewrites every segment URI and `URI="..."` tag attribute inside an HLS `.m3u8` playlist
/// so WebView2 fetches all sub-playlists and media segments through `playzi-stream://` without CORS errors.
/// Also strips YouTube muxed HLS trailing audio-only tail segments that trigger fatal VHS decode switches.
pub fn rewrite_m3u8_playlist(body: &str, base_url: &str, proxy_prefix: &str) -> String {
    let base = Url::parse(base_url).ok();
    let resolve_abs = |raw: &str| -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            trimmed.to_string()
        } else if let Some(b) = &base {
            b.join(trimmed)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| trimmed.to_string())
        } else {
            trimmed.to_string()
        }
    };
    let resolve = |raw: &str| -> String {
        let abs = resolve_abs(raw);
        if abs.is_empty() {
            return String::new();
        }
        encode_proxy_url(proxy_prefix, &abs)
    };

    let raw_lines: Vec<&str> = body.lines().collect();
    let is_vod = raw_lines
        .iter()
        .any(|l| l.trim().eq_ignore_ascii_case("#EXT-X-ENDLIST"));
    let media_uri_indices: Vec<usize> = raw_lines
        .iter()
        .enumerate()
        .filter_map(|(idx, l)| {
            let t = l.trim();
            (!t.is_empty() && !t.starts_with('#')).then_some(idx)
        })
        .collect();
    let last_media_idx = if is_vod && media_uri_indices.len() >= 2 {
        media_uri_indices.last().copied()
    } else {
        None
    };

    // Identify line indices to drop (trailing audio-only segment URI + its preceding #EXTINF).
    let mut skip_lines = std::collections::HashSet::new();
    if media_uri_indices.len() >= 2 {
        for &uri_idx in &media_uri_indices {
            let abs_uri = resolve_abs(raw_lines[uri_idx]);
            let prev_extinf_idx = (0..uri_idx)
                .rev()
                .find(|&i| !raw_lines[i].trim().is_empty())
                .filter(|&i| raw_lines[i].trim().starts_with("#EXTINF:"));
            let extinf_secs = prev_extinf_idx.and_then(|i| parse_extinf_duration(raw_lines[i]));
            let is_last_vod = last_media_idx == Some(uri_idx);
            if is_youtube_muxed_audio_only_tail_segment(&abs_uri, extinf_secs, is_last_vod) {
                skip_lines.insert(uri_idx);
                if let Some(ext_idx) = prev_extinf_idx {
                    skip_lines.insert(ext_idx);
                }
            }
        }
    }

    let mut out = String::with_capacity(body.len() + 512);
    for (idx, line) in raw_lines.into_iter().enumerate() {
        if skip_lines.contains(&idx) {
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            out.push('\n');
            continue;
        }
        if trimmed.starts_with('#') {
            if let Some(uri_pos) = line.find("URI=\"") {
                let value_start = uri_pos + 5;
                if let Some(rel_end) = line[value_start..].find('"') {
                    let value_end = value_start + rel_end;
                    let orig_uri = &line[value_start..value_end];
                    let rewritten = resolve(orig_uri);
                    out.push_str(&line[..value_start]);
                    out.push_str(&rewritten);
                    out.push_str(&line[value_end..]);
                    out.push('\n');
                    continue;
                }
            }
            out.push_str(line);
            out.push('\n');
        } else {
            out.push_str(&resolve(trimmed));
            out.push('\n');
        }
    }
    out
}

fn is_allowed_stream_proxy_host(parsed: &Url) -> bool {
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str().map(|h| h.to_ascii_lowercase()) else {
        return false;
    };
    // Reject raw IP literals (prevents loopback/LAN/metadata SSRF).
    if host.parse::<std::net::IpAddr>().is_ok()
        || (host.starts_with('[') && host.ends_with(']'))
    {
        return false;
    }
    const ALLOWED_EXACT: &[&str] = &[
        "googlevideo.com",
        "youtube.com",
        "ytimg.com",
        "ggpht.com",
        "googleusercontent.com",
    ];
    ALLOWED_EXACT
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// Fetches an upstream media URL or HLS playlist for the `playzi-stream://` custom protocol.
pub async fn fetch_proxied_stream(
    client: &reqwest::Client,
    raw_url: &str,
    range_header: Option<&str>,
    proxy_prefix: &str,
) -> Result<ProxiedStreamResponse> {
    let parsed = validate_http_url(raw_url)?;
    if !is_allowed_stream_proxy_host(&parsed) {
        return Err(CoreError::Validation(
            "Host no permitido para el proxy de stream".into(),
        ));
    }

    const MAX_ATTEMPTS: usize = 3;
    const BACKOFF_MS: [u64; 2] = [200, 500];
    let mut attempt = 0usize;

    let (status, final_url, content_type, content_range, accept_ranges, bytes) = loop {
        attempt += 1;
        let mut req = client.get(parsed.clone()).header("Accept", "*/*");
        if parsed.query().is_some_and(|q| q.contains("c=IOS")) {
            req = req.header("User-Agent", IOS_INNERTUBE_USER_AGENT);
        }
        if let Some(range) = range_header.filter(|r| !r.trim().is_empty()) {
            req = req.header("Range", range);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(err) => {
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
                    continue;
                }
                return Err(CoreError::External(format!(
                    "Stream proxy request failed: {err}"
                )));
            }
        };

        if !is_allowed_stream_proxy_host(resp.url()) {
            return Err(CoreError::Validation(
                "Redirección a host no permitido en el proxy de stream".into(),
            ));
        }

        let status = resp.status().as_u16();
        let retryable_status = status == 429 || (500..=504).contains(&status);
        if retryable_status && attempt < MAX_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
            continue;
        }

        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let content_range = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);
        let accept_ranges = resp
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);

        match resp.bytes().await {
            Ok(b) => {
                break (
                    status,
                    final_url,
                    content_type,
                    content_range,
                    accept_ranges,
                    b.to_vec(),
                );
            }
            Err(err) => {
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(BACKOFF_MS[attempt - 1])).await;
                    continue;
                }
                return Err(CoreError::External(format!(
                    "Stream proxy body read failed: {err}"
                )));
            }
        }
    };

    let looks_like_m3u8 = content_type.to_lowercase().contains("mpegurl")
        || is_hls_url(&final_url)
        || bytes.starts_with(b"#EXTM3U");

    if looks_like_m3u8 && (200..300).contains(&status) {
        let text = String::from_utf8_lossy(&bytes);
        if text.trim_start().starts_with("#EXTM3U") {
            let rewritten = rewrite_m3u8_playlist(&text, &final_url, proxy_prefix);
            return Ok(ProxiedStreamResponse {
                status,
                content_type: "application/vnd.apple.mpegurl".to_string(),
                content_range: None,
                accept_ranges: None,
                bytes: rewritten.into_bytes(),
            });
        }
    }

    let normalized_ct = if content_type.eq_ignore_ascii_case("application/octet-stream")
        && (final_url.contains(".ts") || final_url.contains("/seg.ts"))
    {
        "video/mp2t".to_string()
    } else {
        content_type
    };

    Ok(ProxiedStreamResponse {
        status,
        content_type: normalized_ct,
        content_range,
        accept_ranges,
        bytes,
    })
}

fn extract_audio_tracks(formats: &[StreamFormat]) -> Vec<AudioTrack> {
    let mut best_by_lang: HashMap<String, AudioTrack> = HashMap::new();
    for f in formats {
        if f.acodec.is_empty() || f.acodec == "none" {
            continue;
        }
        if !f.vcodec.is_empty() && f.vcodec != "none" {
            continue;
        }
        let lang = f.language.clone().unwrap_or_else(|| "default".into());
        let label = f
            .language_name
            .clone()
            .unwrap_or_else(|| lang.clone());
        let bitrate = f.abr.or(f.tbr).unwrap_or(0.0);
        let replace = match best_by_lang.get(&lang) {
            None => true,
            Some(existing) => bitrate > existing.abr.unwrap_or(0.0),
        };
        if replace {
            best_by_lang.insert(
                lang.clone(),
                AudioTrack {
                    lang: lang.clone(),
                    label,
                    format_id: f.format_id.clone(),
                    abr: f.abr.or(f.tbr),
                    acodec: f.acodec.clone(),
                    is_default: lang == "default",
                },
            );
        }
    }
    let mut tracks: Vec<AudioTrack> = best_by_lang.into_values().collect();
    tracks.sort_by(|a, b| a.lang.cmp(&b.lang));
    tracks
}

pub fn extract_url_expire(raw_url: &str) -> Option<i64> {
    let parsed = Url::parse(raw_url).ok()?;
    for (k, v) in parsed.query_pairs() {
        if k == "expire" {
            if let Ok(ts) = v.parse::<i64>() {
                return Some(ts);
            }
        }
    }
    if let Some(mut segments) = parsed.path_segments() {
        while let Some(seg) = segments.next() {
            if seg == "expire" {
                if let Some(val) = segments.next() {
                    if let Ok(ts) = val.parse::<i64>() {
                        return Some(ts);
                    }
                }
            }
        }
    }
    None
}

/// Resolves the player executable path, falling back to standard installation
/// locations (e.g. `winget install shinchiro.mpv` under `C:\Program Files\MPV Player`)
/// Resolves a player binary path, honoring explicit custom paths (file, directory, or root-relative)
/// and falling back to standard installation paths and local `bin/` sidecars only for bare names.
pub fn resolve_player_binary(backend: PlayerBackend, configured: &str, root: &Path) -> String {
    let trimmed = configured
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    if !trimmed.is_empty() {
        if let Some(resolved) = resolve_explicit_binary_path(backend, trimmed, root) {
            return resolved;
        }
        // If the user explicitly configured a path (contains directory separators or drive prefix),
        // honor it verbatim instead of silently substituting another installation.
        if looks_like_explicit_path(trimmed) {
            return trimmed.to_string();
        }
        if binary_exists(trimmed) {
            return trimmed.to_string();
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));

    let mut candidates: Vec<PathBuf> = Vec::new();
    match backend {
        PlayerBackend::Mpv => {
            #[cfg(windows)]
            {
                if let Some(dir) = &exe_dir {
                    candidates.push(dir.join("bin").join("mpv.exe"));
                    candidates.push(dir.join("mpv.exe"));
                }
                candidates.push(root.join("bin").join("mpv.exe"));
                candidates.push(PathBuf::from(r"C:\Program Files\MPV Player\mpv.exe"));
                candidates.push(PathBuf::from(r"C:\Program Files (x86)\MPV Player\mpv.exe"));
                candidates.push(PathBuf::from(r"C:\Program Files\mpv\mpv.exe"));
                if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                    candidates.push(PathBuf::from(&local_app_data).join("Programs").join("mpv").join("mpv.exe"));
                    candidates.push(PathBuf::from(&local_app_data).join("Microsoft").join("WinGet").join("Links").join("mpv.exe"));
                    if let Some(winget_mpv) = find_winget_package_exe(&local_app_data, "mpv", "mpv.exe") {
                        candidates.push(winget_mpv);
                    }
                }
                if let Ok(user_profile) = std::env::var("USERPROFILE") {
                    candidates.push(PathBuf::from(&user_profile).join("scoop").join("shims").join("mpv.exe"));
                }
            }
            #[cfg(not(windows))]
            {
                if let Some(dir) = &exe_dir {
                    candidates.push(dir.join("bin").join("mpv"));
                    candidates.push(dir.join("mpv"));
                }
                candidates.push(root.join("bin").join("mpv"));
                candidates.push(PathBuf::from("/usr/bin/mpv"));
                candidates.push(PathBuf::from("/usr/local/bin/mpv"));
                candidates.push(PathBuf::from("/opt/homebrew/bin/mpv"));
            }
        }
        PlayerBackend::Vlc => {
            #[cfg(windows)]
            {
                if let Some(dir) = &exe_dir {
                    candidates.push(dir.join("bin").join("vlc.exe"));
                    candidates.push(dir.join("vlc.exe"));
                }
                candidates.push(root.join("bin").join("vlc.exe"));
                candidates.push(PathBuf::from(r"C:\Program Files\VideoLAN\VLC\vlc.exe"));
                candidates.push(PathBuf::from(r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe"));
            }
            #[cfg(not(windows))]
            {
                if let Some(dir) = &exe_dir {
                    candidates.push(dir.join("bin").join("vlc"));
                    candidates.push(dir.join("vlc"));
                }
                candidates.push(root.join("bin").join("vlc"));
                candidates.push(PathBuf::from("/usr/bin/vlc"));
                candidates.push(PathBuf::from("/usr/local/bin/vlc"));
            }
        }
    }

    for candidate in candidates {
        if candidate.is_file() {
            return candidate.to_string_lossy().to_string();
        }
    }

    trimmed.to_string()
}

fn looks_like_explicit_path(value: &str) -> bool {
    value.contains('/') || value.contains('\\') || value.contains(':')
}

fn resolve_explicit_binary_path(
    backend: PlayerBackend,
    trimmed: &str,
    root: &Path,
) -> Option<String> {
    let raw = PathBuf::from(trimmed);
    let mut bases = vec![raw.clone()];
    if raw.is_relative() {
        bases.push(root.join(&raw));
    }

    let default_names: &[&str] = match backend {
        PlayerBackend::Mpv => {
            #[cfg(windows)]
            { &["mpv.exe", "mpv"] }
            #[cfg(not(windows))]
            { &["mpv"] }
        }
        PlayerBackend::Vlc => {
            #[cfg(windows)]
            { &["vlc.exe", "vlc"] }
            #[cfg(not(windows))]
            { &["vlc"] }
        }
    };

    for base in bases {
        if base.is_file() {
            return Some(base.to_string_lossy().to_string());
        }
        #[cfg(windows)]
        {
            if !base.to_string_lossy().to_lowercase().ends_with(".exe") {
                let with_exe = PathBuf::from(format!("{}.exe", base.display()));
                if with_exe.is_file() {
                    return Some(with_exe.to_string_lossy().to_string());
                }
            }
        }
        if base.is_dir() {
            for name in default_names {
                let candidate = base.join(name);
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}

#[cfg(windows)]
fn find_winget_package_exe(local_app_data: &str, name_substr: &str, exe_name: &str) -> Option<PathBuf> {
    let winget_pkgs = PathBuf::from(local_app_data)
        .join("Microsoft")
        .join("WinGet")
        .join("Packages");
    let entries = std::fs::read_dir(&winget_pkgs).ok()?;
    let needle = name_substr.to_ascii_lowercase();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !name.contains(&needle) {
            continue;
        }
        let dir = entry.path();
        let direct = dir.join(exe_name);
        if direct.is_file() {
            return Some(direct);
        }
        if let Ok(sub_entries) = std::fs::read_dir(&dir) {
            for sub in sub_entries.flatten() {
                let sub_path = sub.path();
                let nested = sub_path.join(exe_name);
                if nested.is_file() {
                    return Some(nested);
                }
                let nested_bin = sub_path.join("bin").join(exe_name);
                if nested_bin.is_file() {
                    return Some(nested_bin);
                }
            }
        }
    }
    None
}

pub fn discover_ytdlp_binary(root: &Path) -> String {
    let venv_candidate = if cfg!(windows) {
        root.join(".venv").join("Scripts").join("yt-dlp.exe")
    } else {
        root.join(".venv").join("bin").join("yt-dlp")
    };
    if venv_candidate.is_file() {
        return venv_candidate.to_string_lossy().to_string();
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar_bin = if cfg!(windows) {
                dir.join("bin").join("yt-dlp.exe")
            } else {
                dir.join("bin").join("yt-dlp")
            };
            if sidecar_bin.is_file() {
                return sidecar_bin.to_string_lossy().to_string();
            }
            let sidecar = if cfg!(windows) {
                dir.join("yt-dlp.exe")
            } else {
                dir.join("yt-dlp")
            };
            if sidecar.is_file() {
                return sidecar.to_string_lossy().to_string();
            }
        }
    }

    let root_bin = if cfg!(windows) {
        root.join("bin").join("yt-dlp.exe")
    } else {
        root.join("bin").join("yt-dlp")
    };
    if root_bin.is_file() {
        return root_bin.to_string_lossy().to_string();
    }

    if binary_exists("yt-dlp") {
        return "yt-dlp".into();
    }

    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(&local_app_data);
            let direct_candidates = [
                base.join("Microsoft").join("WinGet").join("Links").join("yt-dlp.exe"),
                base.join("Programs").join("yt-dlp").join("yt-dlp.exe"),
            ];
            for cand in direct_candidates {
                if cand.is_file() {
                    return cand.to_string_lossy().to_string();
                }
            }
            if let Some(winget_exe) = find_winget_package_exe(&local_app_data, "yt-dlp", "yt-dlp.exe") {
                return winget_exe.to_string_lossy().to_string();
            }
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let scoop = PathBuf::from(profile).join("scoop").join("shims").join("yt-dlp.exe");
            if scoop.is_file() {
                return scoop.to_string_lossy().to_string();
            }
        }
        let choco = PathBuf::from(r"C:\ProgramData\chocolatey\bin\yt-dlp.exe");
        if choco.is_file() {
            return choco.to_string_lossy().to_string();
        }
    }

    "yt-dlp".into()
}

pub fn discover_ffmpeg_binary(configured: &str, root: &Path) -> String {
    let trimmed = configured
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim();

    if !trimmed.is_empty() {
        let raw = PathBuf::from(trimmed);
        if raw.is_file() {
            return raw.to_string_lossy().to_string();
        }
        if raw.is_relative() {
            let rel = root.join(&raw);
            if rel.is_file() {
                return rel.to_string_lossy().to_string();
            }
        }
        if looks_like_explicit_path(trimmed) {
            return trimmed.to_string();
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower != "ffmpeg" && lower != "ffmpeg.exe" && binary_exists(trimmed) {
            return trimmed.to_string();
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar_bin = if cfg!(windows) {
                dir.join("bin").join("ffmpeg.exe")
            } else {
                dir.join("bin").join("ffmpeg")
            };
            if sidecar_bin.is_file() {
                return sidecar_bin.to_string_lossy().to_string();
            }
            let sidecar = if cfg!(windows) {
                dir.join("ffmpeg.exe")
            } else {
                dir.join("ffmpeg")
            };
            if sidecar.is_file() {
                return sidecar.to_string_lossy().to_string();
            }
        }
    }

    let root_bin = if cfg!(windows) {
        root.join("bin").join("ffmpeg.exe")
    } else {
        root.join("bin").join("ffmpeg")
    };
    if root_bin.is_file() {
        return root_bin.to_string_lossy().to_string();
    }

    if binary_exists("ffmpeg") {
        return "ffmpeg".into();
    }

    #[cfg(windows)]
    {
        if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
            let link = PathBuf::from(&local_app_data)
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join("ffmpeg.exe");
            if link.is_file() {
                return link.to_string_lossy().to_string();
            }
            if let Some(winget_exe) = find_winget_package_exe(&local_app_data, "ffmpeg", "ffmpeg.exe") {
                return winget_exe.to_string_lossy().to_string();
            }
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let scoop = PathBuf::from(profile).join("scoop").join("shims").join("ffmpeg.exe");
            if scoop.is_file() {
                return scoop.to_string_lossy().to_string();
            }
        }
        let choco = PathBuf::from(r"C:\ProgramData\chocolatey\bin\ffmpeg.exe");
        if choco.is_file() {
            return choco.to_string_lossy().to_string();
        }
    }

    if trimmed.is_empty() {
        "ffmpeg".into()
    } else {
        trimmed.to_string()
    }
}

pub fn binary_exists(bin: &str) -> bool {
    let trimmed = bin.trim();
    if trimmed.is_empty() {
        return false;
    }
    let path = Path::new(trimmed);
    if path.is_file() {
        return true;
    }
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join(trimmed);
            if candidate.is_file() {
                return true;
            }
            #[cfg(windows)]
            {
                if !trimmed.to_lowercase().ends_with(".exe") {
                    let with_exe = dir.join(format!("{trimmed}.exe"));
                    if with_exe.is_file() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn format_selector_handles_quality_vlc_and_audio() {
        assert_eq!(
            format_selector("1080p", false, false),
            "bestvideo[height<=1080]+bestaudio/best[height<=1080]/best"
        );
        assert_eq!(
            format_selector("best", false, false),
            "bestvideo+bestaudio/best"
        );
        assert_eq!(
            format_selector("720p", false, true),
            "bestaudio[acodec!=none]/bestaudio/best"
        );
        assert!(format_selector("720p", true, false).contains("vcodec^=avc1"));
        assert!(format_selector("720p", true, false).contains("height<=720"));
    }

    #[test]
    fn build_mpv_args_includes_hwdec_ipc_wid_and_ytdlp_hook() {
        let args = build_player_args(
            PlayerBackend::Mpv,
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "1080p",
            false,
            Some(123456),
            Some(r"\\.\pipe\playzi-mpv-test"),
            r"E:\playzi\.venv\Scripts\yt-dlp.exe",
            Some(Path::new(r"E:\playzi\data\private\cookies.txt")),
        );
        assert!(args.contains(&"--hwdec=auto-safe".to_string()));
        assert!(args.contains(&"--wid=123456".to_string()));
        assert!(args.contains(&r"--input-ipc-server=\\.\pipe\playzi-mpv-test".to_string()));
        assert!(args
            .iter()
            .any(|a| a.starts_with("--script-opts=ytdl_hook-ytdl_path=")));
        assert!(args.iter().any(|a| a.starts_with("--ytdl-format=")));
        assert!(args
            .iter()
            .any(|a| a.starts_with("--ytdl-raw-options-append=cookies=")));
        assert!(args
            .iter()
            .any(|a| a == "--ytdl-raw-options-append=extractor-args=youtube:player_client=web_safari"));
    }

    #[test]
    fn build_vlc_args_includes_caching_audio_only_slave_and_title() {
        let args = build_vlc_args(
            "https://rr1.googlevideo.com/videoplayback",
            true,
            Some("https://rr1.googlevideo.com/audioplayback"),
            Some("Demo Track"),
        );
        assert!(args.contains(&"--network-caching=2500".to_string()));
        assert!(args.contains(&"--no-video".to_string()));
        assert!(args.contains(&"--input-slave=https://rr1.googlevideo.com/audioplayback".to_string()));
        assert!(args.contains(&"--meta-title=Demo Track".to_string()));
        assert_eq!(
            args.last().map(String::as_str),
            Some("https://rr1.googlevideo.com/videoplayback")
        );
    }

    #[test]
    fn resolve_player_binary_honors_custom_file_dir_quotes_and_missing_explicit_path() {
        let tmp = TempDir::new().unwrap();
        let custom_dir = tmp.path().join("custom-mpv");
        std::fs::create_dir_all(&custom_dir).unwrap();
        let exe_name = if cfg!(windows) { "mpv.exe" } else { "mpv" };
        let custom_exe = custom_dir.join(exe_name);
        std::fs::write(&custom_exe, b"fake").unwrap();

        // Exact file path (even wrapped in quotes)
        let quoted = format!("\"{}\"", custom_exe.display());
        assert_eq!(
            resolve_player_binary(PlayerBackend::Mpv, &quoted, tmp.path()),
            custom_exe.to_string_lossy()
        );

        // Directory path containing mpv executable
        assert_eq!(
            resolve_player_binary(
                PlayerBackend::Mpv,
                &custom_dir.to_string_lossy(),
                tmp.path()
            ),
            custom_exe.to_string_lossy()
        );

        // Explicit non-existent path must NOT silently fall back to another system binary
        let missing = tmp.path().join("does-not-exist").join(exe_name);
        let resolved_missing = resolve_player_binary(
            PlayerBackend::Mpv,
            &missing.to_string_lossy(),
            tmp.path(),
        );
        assert_eq!(resolved_missing, missing.to_string_lossy());
        assert!(!binary_exists(&resolved_missing));
    }

    #[test]
    fn rewrite_m3u8_playlist_rewrites_segments_and_uri_attributes() {
        let sample = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a1\",URI=\"https://cdn.example.com/audio.m3u8\"\n#EXTINF:5.0,\nseg001.ts\nhttps://cdn.example.com/seg002.ts\n";
        let rewritten = rewrite_m3u8_playlist(
            sample,
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/index.m3u8",
            "http://playzi-stream.localhost/",
        );
        assert!(rewritten.contains("URI=\"http://playzi-stream.localhost/?url=https%3A%2F%2Fcdn.example.com%2Faudio.m3u8\""));
        assert!(rewritten.contains("http://playzi-stream.localhost/?url=https%3A%2F%2Fmanifest.googlevideo.com%2Fapi%2Fmanifest%2Fhls_playlist%2Fseg001.ts"));
        assert!(rewritten.contains("http://playzi-stream.localhost/?url=https%3A%2F%2Fcdn.example.com%2Fseg002.ts"));
    }

    #[test]
    fn build_hls_master_playlist_combines_video_and_audio() {
        let master = build_hls_master_playlist(
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/video.m3u8",
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/audio.m3u8",
            "http://playzi-stream.localhost/",
        );
        assert!(master.starts_with("#EXTM3U\n"));
        assert!(master.contains("TYPE=AUDIO,GROUP-ID=\"aud\""));
        assert!(master.contains("URI=\"http://playzi-stream.localhost/?url=https%3A%2F%2Fmanifest.googlevideo.com%2Fapi%2Fmanifest%2Fhls_playlist%2Faudio.m3u8\""));
        assert!(master.contains("http://playzi-stream.localhost/?url=https%3A%2F%2Fmanifest.googlevideo.com%2Fapi%2Fmanifest%2Fhls_playlist%2Fvideo.m3u8"));
    }

    #[test]
    fn build_hls_master_playlist_with_meta_includes_vp9_codecs_and_resolution() {
        let master = build_hls_master_playlist_with_meta(
            "https://manifest.googlevideo.com/v4k.m3u8",
            "https://manifest.googlevideo.com/a.m3u8",
            "http://playzi-stream.localhost/",
            &HlsMasterMeta {
                vcodec: Some("vp09.00.51.08".into()),
                acodec: Some("none".into()),
                width: Some(3840),
                height: Some(2160),
                bandwidth: Some(28_034_000),
            },
        );
        assert!(master.contains("#EXT-X-VERSION:6"));
        assert!(master.contains(
            "#EXT-X-STREAM-INF:BANDWIDTH=28034000,RESOLUTION=3840x2160,CODECS=\"vp09.00.51.08,mp4a.40.2\",AUDIO=\"aud\""
        ));
    }

    #[test]
    fn validate_play_target_enforces_media_root_boundary() {
        let tmp = TempDir::new().unwrap();
        let media = tmp.path().join("media");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&media).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let good_file = media.join("clip.mp4");
        let bad_file = outside.join("secret.mp4");
        fs::write(&good_file, b"ok").unwrap();
        fs::write(&bad_file, b"no").unwrap();

        assert!(validate_play_target(&good_file.to_string_lossy(), &media).is_ok());
        assert!(validate_play_target(&bad_file.to_string_lossy(), &media).is_err());
        assert!(validate_play_target("https://www.youtube.com/watch?v=abc12345678", &media).is_ok());
    }

    #[test]
    fn parse_ytdlp_info_selects_formats_tracks_and_expiry() {
        let raw = json!({
            "id": "dQw4w9WgXcQ",
            "title": "Sample Video",
            "channel": "Sample Channel",
            "duration": 212.0,
            "webpage_url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "formats": [
                {
                    "format_id": "248",
                    "url": "https://rr1.googlevideo.com/videoplayback?expire=1999999999",
                    "ext": "webm",
                    "vcodec": "vp9",
                    "acodec": "none",
                    "width": 1920,
                    "height": 1080,
                    "tbr": 2500.0
                },
                {
                    "format_id": "313",
                    "url": "https://rr1.googlevideo.com/videoplayback?expire=1999999999",
                    "ext": "webm",
                    "vcodec": "vp9",
                    "acodec": "none",
                    "width": 3840,
                    "height": 2160,
                    "tbr": 12000.0
                },
                {
                    "format_id": "251-en",
                    "url": "https://rr1.googlevideo.com/videoplayback?expire=1999999500",
                    "ext": "webm",
                    "vcodec": "none",
                    "acodec": "opus",
                    "abr": 160.0,
                    "language": "en",
                    "format_note": "English"
                },
                {
                    "format_id": "251-es",
                    "url": "https://rr1.googlevideo.com/videoplayback?expire=1999999600",
                    "ext": "webm",
                    "vcodec": "none",
                    "acodec": "opus",
                    "abr": 128.0,
                    "language": "es",
                    "format_note": "Spanish"
                }
            ]
        });

        let info = parse_ytdlp_info_json(&raw, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", "1080p", false);
        assert_eq!(info.video.as_ref().unwrap().format_id, "248");
        assert_eq!(info.audio.as_ref().unwrap().format_id, "251-en");
        assert_eq!(info.audio_tracks.len(), 2);
        assert_eq!(info.available_qualities, vec!["2160p", "1080p"]);
        assert_eq!(info.expires_at, Some(1999999500));
        assert_eq!(info.item["title"], "Sample Video");
    }

    #[test]
    fn parse_ytdlp_info_detects_and_selects_hls_above_1080p() {
        let raw = json!({
            "id": "hiRes123456",
            "title": "4K Sample",
            "channel": "Demo",
            "duration": 180.0,
            "webpage_url": "https://www.youtube.com/watch?v=hiRes123456",
            "formats": [
                {
                    "format_id": "234",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/audio.m3u8",
                    "ext": "mp4",
                    "vcodec": "none",
                    "acodec": "mp4a.40.2",
                    "abr": 128.0
                },
                {
                    "format_id": "96",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/mux1080.m3u8",
                    "ext": "mp4",
                    "vcodec": "avc1.640028",
                    "acodec": "mp4a.40.2",
                    "width": 1920,
                    "height": 1080,
                    "tbr": 3500.0
                },
                {
                    "format_id": "270",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/avc1080.m3u8",
                    "ext": "mp4",
                    "vcodec": "avc1.640028",
                    "acodec": "none",
                    "width": 1920,
                    "height": 1080,
                    "tbr": 4200.0
                },
                {
                    "format_id": "620",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/vp9_1440.m3u8",
                    "ext": "mp4",
                    "vcodec": "vp09.00.50.08",
                    "acodec": "none",
                    "width": 2560,
                    "height": 1440,
                    "tbr": 9000.0
                },
                {
                    "format_id": "625",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/vp9_2160.m3u8",
                    "ext": "mp4",
                    "vcodec": "vp09.00.51.08",
                    "acodec": "none",
                    "width": 3840,
                    "height": 2160,
                    "tbr": 18000.0
                },
                {
                    "format_id": "642",
                    "url": "https://manifest.googlevideo.com/api/manifest/hls_playlist/vp9_2160_hdr.m3u8",
                    "ext": "mp4",
                    "vcodec": "vp09.02.51.10.01.09.16.09.00",
                    "acodec": "none",
                    "width": 3840,
                    "height": 2160,
                    "tbr": 32000.0
                }
            ]
        });

        let best = parse_ytdlp_info_json(&raw, "https://www.youtube.com/watch?v=hiRes123456", "best", false);
        assert_eq!(best.available_qualities, vec!["2160p", "1440p", "1080p"]);
        // Prefers 8-bit SDR VP9 (625) over higher-bitrate 10-bit HDR VP9 (642)
        assert_eq!(best.video.as_ref().unwrap().format_id, "625");

        let q1440 = parse_ytdlp_info_json(&raw, "https://www.youtube.com/watch?v=hiRes123456", "1440p", false);
        assert_eq!(q1440.video.as_ref().unwrap().format_id, "620");

        let q1080 = parse_ytdlp_info_json(&raw, "https://www.youtube.com/watch?v=hiRes123456", "1080p", false);
        assert_eq!(q1080.video.as_ref().unwrap().format_id, "96");
    }

    #[test]
    fn build_mpv_ipc_commands_serialize_valid_json() {
        let pause = build_mpv_ipc_command(&PlayerControlCommand::Pause { paused: true });
        assert_eq!(pause["command"], json!(["set_property", "pause", true]));

        let seek = build_mpv_ipc_command(&PlayerControlCommand::Seek {
            seconds: 15.0,
            relative: true,
        });
        assert_eq!(seek["command"], json!(["seek", 15.0, "relative"]));
    }

    #[test]
    fn mpv_real_playback_and_ipc_control_smoke() {
        let tmp_dir = std::env::temp_dir().join(format!("playzi-player-smoke-{}", Uuid::new_v4()));
        let media_dir = tmp_dir.join("media");
        let data_dir = tmp_dir.join("data");
        std::fs::create_dir_all(&media_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();

        let mpv_bin = resolve_player_binary(PlayerBackend::Mpv, "mpv", &tmp_dir);
        if !binary_exists(&mpv_bin) || !binary_exists("ffmpeg") {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return;
        }

        let clip = media_dir.join("smoke.mp4");
        let status = StdCommand::new("ffmpeg")
            .args([
                "-y",
                "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=24",
                "-f", "lavfi", "-i", "sine=frequency=440",
                "-t", "3",
                "-c:v", "libx264", "-pix_fmt", "yuv420p",
                "-c:a", "aac",
            ])
            .arg(&clip)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("ffmpeg should run");
        assert!(status.success(), "ffmpeg clip generation failed");

        let cfg_store = ConfigStore::load_from_root(&tmp_dir);
        let snap = cfg_store.snapshot();
        snap.ensure_dirs().unwrap();
        let state_store = StateStore::new(&snap.state_file);
        let player = PlayerManager::new(state_store, cfg_store);

        let caps = player.capabilities();
        assert!(caps.mpv_available, "expected mpv to be auto-detected");
        assert_eq!(caps.hwdec_mode, "auto-safe");

        let resp = player
            .play(PlayRequest {
                target: clip.to_string_lossy().to_string(),
                player: Some("mpv".into()),
                title: Some("Smoke Test".into()),
                thumbnail: None,
                channel: None,
                quality: None,
                audio_only: false,
                wid: None,
            })
            .expect("mpv launch should succeed");
        assert!(resp.ok);
        assert!(resp.ipc_endpoint.is_some());

        // Wait up to 3s for mpv to open its IPC pipe, then send real IPC commands
        let mut paused_state = None;
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(st) = player.control(PlayerControlCommand::Pause { paused: true }) {
                paused_state = Some(st);
                break;
            }
        }
        let st = paused_state.expect("mpv IPC pause command should succeed");
        assert!(st.active);
        assert!(st.paused);

        let muted = player
            .control(PlayerControlCommand::SetMute { muted: true })
            .expect("mpv IPC mute command should succeed");
        assert!(muted.muted);

        let seeked = player
            .control(PlayerControlCommand::Seek {
                seconds: 1.0,
                relative: false,
            })
            .expect("mpv IPC seek command should succeed");
        assert!((seeked.position_secs - 1.0).abs() < 0.01);

        let stopped = player
            .control(PlayerControlCommand::Stop)
            .expect("mpv IPC stop command should succeed");
        assert!(!stopped.active);

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn build_ytdlp_json_args_supports_cookie_fallback_for_single_streams() {
        let cache = PathBuf::from("data/ytdlp_cache");
        let cookies = PathBuf::from("data/private/cookies_cache.txt");
        let url = "https://www.youtube.com/watch?v=psRqvowrnkw";

        let unauth = build_ytdlp_json_args(&cache, None, None, url, false, None);
        assert!(unauth.contains(&"--no-playlist".to_string()));
        assert!(unauth.contains(&"youtube:player_client=web_safari,default".to_string()));
        assert!(!unauth.contains(&"--cookies".to_string()));
        assert_eq!(unauth.last().map(String::as_str), Some(url));

        let with_cookies = build_ytdlp_json_args(
            &cache,
            None,
            Some(cookies.as_path()),
            url,
            false,
            None,
        );
        assert!(with_cookies.contains(&"--no-playlist".to_string()));
        assert!(with_cookies.contains(&"--cookies".to_string()));
        assert!(with_cookies.contains(&cookies.to_string_lossy().to_string()));
        assert_eq!(with_cookies.last().map(String::as_str), Some(url));
    }

    #[test]
    fn parse_loudness_db_extracts_loudness_and_perceptual_fallback() {
        let html = r#"{"audioConfig":{"loudnessDb":8.7199993,"perceptualLoudnessDb":-5.28}}"#;
        let ldb = parse_loudness_db_from_html(html).expect("should parse loudnessDb");
        assert!((ldb - 8.7199993).abs() < 1e-5);

        let neg_html = r#"{"audioConfig":{"loudnessDb": -2.5}}"#;
        let neg = parse_loudness_db_from_html(neg_html).expect("should parse negative loudnessDb");
        assert!((neg + 2.5).abs() < 1e-5);

        let perceptual_only = r#"{"audioConfig":{"perceptualLoudnessDb":-9.5}}"#;
        let fallback = parse_loudness_db_from_html(perceptual_only).expect("should fallback");
        assert!((fallback - 4.5).abs() < 1e-5);

        assert!(parse_loudness_db_from_html("<html>no audio config</html>").is_none());
    }

    #[test]
    fn select_best_audio_prefers_non_drc_tracks() {
        let raw = json!({
            "id": "abc12345678",
            "title": "Test",
            "formats": [
                {
                    "format_id": "251-drc",
                    "url": "https://example.com/251-drc",
                    "ext": "webm",
                    "vcodec": "none",
                    "acodec": "opus",
                    "abr": 160.0
                },
                {
                    "format_id": "251",
                    "url": "https://example.com/251",
                    "ext": "webm",
                    "vcodec": "none",
                    "acodec": "opus",
                    "abr": 130.0
                }
            ]
        });
        let info = parse_ytdlp_info_json(&raw, "https://youtu.be/abc12345678", "best", true);
        let audio = info.audio.expect("should pick audio");
        assert_eq!(audio.format_id, "251");
    }

    #[test]
    fn extract_url_expire_parses_query_and_hls_path_segments() {
        assert_eq!(
            extract_url_expire("https://rr1.googlevideo.com/videoplayback?expire=1795745495&itag=140"),
            Some(1795745495)
        );
        assert_eq!(
            extract_url_expire("https://manifest.googlevideo.com/api/manifest/hls_playlist/expire/1795745495/ei/abc/itag/96/playlist/index.m3u8"),
            Some(1795745495)
        );
        assert_eq!(
            extract_url_expire("https://manifest.googlevideo.com/api/manifest/hls_playlist/itag/96/playlist/index.m3u8"),
            None
        );
    }

    #[test]
    fn rewrite_m3u8_playlist_drops_youtube_muxed_audio_only_tail_segment() {
        let body = concat!(
            "#EXTM3U\n",
            "#EXT-X-VERSION:3\n",
            "#EXT-X-PLAYLIST-TYPE:VOD\n",
            "#EXT-X-TARGETDURATION:7\n",
            "#EXTINF:6.133333,\n",
            "https://rr2.googlevideo.com/videoplayback/itag/96/sgoap/dur%3D254.931%3Bitag%3D140/sgovp/dur%3D254.866%3Bitag%3D137/begin/248733/len/6133/file/seg.ts\n",
            "#EXTINF:0.065,\n",
            "https://rr2.googlevideo.com/videoplayback/itag/96/sgoap/dur%3D254.931%3Bitag%3D140/sgovp/dur%3D254.866%3Bitag%3D137/begin/254866/len/65/file/seg.ts\n",
            "#EXT-X-ENDLIST\n"
        );
        let rewritten = rewrite_m3u8_playlist(
            body,
            "https://manifest.googlevideo.com/api/manifest/hls_playlist/itag/96/playlist/index.m3u8",
            "http://playzi-stream.localhost/",
        );
        assert!(rewritten.contains("begin%2F248733"));
        assert!(!rewritten.contains("begin%2F254866"));
        assert!(!rewritten.contains("#EXTINF:0.065,"));
        assert!(rewritten.contains("#EXT-X-ENDLIST"));
    }

    #[test]
    fn parse_innertube_player_json_projects_qualities_and_builds_dash_mpd() {
        let payload = json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": {
                "videoId": "LXb3EKWsInQ",
                "title": "Costa Rica 4K",
                "author": "Jacob + Katie Schwarz",
                "lengthSeconds": "314",
                "isLive": false,
                "thumbnail": {
                    "thumbnails": [
                        { "url": "https://i.ytimg.com/vi/LXb3EKWsInQ/hqdefault.jpg" }
                    ]
                }
            },
            "playerConfig": {
                "audioConfig": { "loudnessDb": -1.25 }
            },
            "streamingData": {
                "adaptiveFormats": [
                    {
                        "itag": 313,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=313&c=IOS",
                        "mimeType": "video/webm; codecs=\"vp09.00.50.08\"",
                        "bitrate": 13500000,
                        "width": 3840,
                        "height": 2160,
                        "fps": 30,
                        "initRange": { "start": "0", "end": "220" },
                        "indexRange": { "start": "221", "end": "893" }
                    },
                    {
                        "itag": 401,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=401&c=IOS",
                        "mimeType": "video/mp4; codecs=\"av01.0.12M.08\"",
                        "bitrate": 9024958,
                        "width": 3840,
                        "height": 2160,
                        "fps": 30,
                        "initRange": { "start": "0", "end": "700" },
                        "indexRange": { "start": "701", "end": "1188" }
                    },
                    {
                        "itag": 137,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=137&c=IOS",
                        "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                        "bitrate": 4500000,
                        "width": 1920,
                        "height": 1080,
                        "fps": 30,
                        "initRange": { "start": "0", "end": "740" },
                        "indexRange": { "start": "741", "end": "1480" }
                    },
                    {
                        "itag": 136,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=136&c=IOS",
                        "mimeType": "video/mp4; codecs=\"avc1.4d401f\"",
                        "bitrate": 2200000,
                        "width": 1280,
                        "height": 720,
                        "fps": 30,
                        "initRange": { "start": "0", "end": "738" },
                        "indexRange": { "start": "739", "end": "1420" }
                    },
                    {
                        "itag": 251,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=251&c=IOS",
                        "mimeType": "audio/webm; codecs=\"opus\"",
                        "bitrate": 160000,
                        "audioSampleRate": "48000",
                        "audioChannels": 2,
                        "initRange": { "start": "0", "end": "258" },
                        "indexRange": { "start": "259", "end": "629" }
                    },
                    {
                        "itag": 140,
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1899999999&itag=140&c=IOS",
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 129500,
                        "audioSampleRate": "44100",
                        "audioChannels": 2,
                        "initRange": { "start": "0", "end": "631" },
                        "indexRange": { "start": "632", "end": "1090" }
                    }
                ]
            }
        });

        let raw = parse_innertube_player_json(
            &payload,
            "LXb3EKWsInQ",
            "https://www.youtube.com/watch?v=LXb3EKWsInQ",
        )
        .expect("should parse valid InnerTube IOS payload");
        assert_eq!(raw.loudness_db, Some(-1.25));
        assert_eq!(raw.expires_at, Some(1899999999));

        // Same CachedRawVideo projects ISOBMFF MP4/M4A qualities (never WebM) without re-fetching.
        let best = raw.project("best", false);
        assert_eq!(best.available_qualities, vec!["2160p", "1080p", "720p"]);
        assert_eq!(best.video.as_ref().map(|v| v.format_id.as_str()), Some("401"));
        assert_eq!(best.audio.as_ref().map(|a| a.format_id.as_str()), Some("140"));

        let q1080 = raw.project("1080p", false);
        assert_eq!(q1080.video.as_ref().map(|v| v.format_id.as_str()), Some("137"));
        assert_eq!(q1080.audio.as_ref().map(|a| a.format_id.as_str()), Some("140"));

        let q720 = raw.project("720p", false);
        assert_eq!(q720.video.as_ref().and_then(|v| v.height), Some(720));

        let audio = raw.project("audio", true);
        assert!(audio.video.is_none());
        assert_eq!(audio.audio.as_ref().map(|a| a.format_id.as_str()), Some("140"));

        let mpd = build_dash_mpd_manifest(
            q1080.duration,
            q1080.video.as_ref().unwrap(),
            q1080.audio.as_ref().unwrap(),
            "http://playzi-stream.localhost/",
        )
        .expect("should build valid DASH MPD");
        assert!(mpd.contains("mediaPresentationDuration=\"PT314.000S\""));
        assert!(mpd.contains("mimeType=\"video/mp4\""));
        assert!(mpd.contains("mimeType=\"audio/mp4\""));
        assert!(mpd.contains("codecs=\"avc1.640028\""));
        assert!(mpd.contains("codecs=\"mp4a.40.2\""));
        assert!(mpd.contains("indexRange=\"741-1480\""));
        assert!(mpd.contains("<Initialization range=\"0-740\" />"));
        assert!(mpd.contains("http://playzi-stream.localhost/?url="));
    }

    #[test]
    fn parse_innertube_player_json_rejects_live_and_unplayable_for_ytdlp_fallback() {
        let live = json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": { "videoId": "live1234567", "isLive": true },
            "streamingData": { "adaptiveFormats": [] }
        });
        assert!(parse_innertube_player_json(&live, "live1234567", "https://www.youtube.com/watch?v=live1234567").is_err());

        let blocked = json!({
            "playabilityStatus": { "status": "LOGIN_REQUIRED" }
        });
        assert!(parse_innertube_player_json(&blocked, "blk12345678", "https://www.youtube.com/watch?v=blk12345678").is_err());
    }
}
