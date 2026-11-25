use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::{RwLock, broadcast, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use url::Url;
use uuid::Uuid;

use crate::config::ConfigStore;
use crate::error::{CoreError, Result};
use crate::social::build_relay_ws_url;
use crate::state::{FriendRecord, StateStore, UserProfile};

pub const DEFAULT_PARTY_PORT: u16 = 8766;
const ROOM_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueuePolicy {
    HostOnly,
    #[default]
    All,
    Selected,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PartyRole {
    #[default]
    Idle,
    Host,
    Guest,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PartyUserRef {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
}

impl From<&UserProfile> for PartyUserRef {
    fn from(p: &UserProfile) -> Self {
        Self {
            user_id: p.user_id.clone(),
            display_name: p.display_name.clone(),
            google_account_name: p.google_account_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PartyParticipant {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
    pub is_host: bool,
    pub can_queue: bool,
    pub joined_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PartyQueueItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub thumbnail: String,
    pub channel: String,
    pub duration_text: String,
    pub source_context: String,
    pub added_by: PartyUserRef,
    pub added_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AddQueueItemInput {
    pub url: String,
    pub title: String,
    pub thumbnail: String,
    pub channel: String,
    pub duration_text: String,
    pub source_context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PartyPlaybackState {
    pub current_index: Option<usize>,
    pub is_playing: bool,
    pub position_secs: f64,
    pub updated_at_ms: i64,
    pub playback_rate: f64,
    pub repeat: String,
    pub shuffle: bool,
    pub version: u64,
}

impl Default for PartyPlaybackState {
    fn default() -> Self {
        Self {
            current_index: None,
            is_playing: false,
            position_secs: 0.0,
            updated_at_ms: Utc::now().timestamp_millis(),
            playback_rate: 1.0,
            repeat: "none".to_string(),
            shuffle: false,
            version: 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WireRoomState {
    pub room_code: String,
    pub host_user_id: String,
    pub host_name: String,
    pub queue_policy: QueuePolicy,
    pub allowed_queue_user_ids: Vec<String>,
    pub participants: Vec<PartyParticipant>,
    pub queue: Vec<PartyQueueItem>,
    pub playback: PartyPlaybackState,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PartyRoomSnapshot {
    pub active: bool,
    pub role: PartyRole,
    pub connected: bool,
    pub room_code: String,
    pub host_user_id: String,
    pub host_name: String,
    pub bind_port: Option<u16>,
    pub local_ws_url: String,
    pub public_ws_url: String,
    pub invite_url: String,
    pub tunnel_running: bool,
    pub tunnel_error: String,
    pub relay_connected: bool,
    pub relay_error: String,
    pub room_access: String,
    pub queue_policy: QueuePolicy,
    pub allowed_queue_user_ids: Vec<String>,
    pub can_self_queue: bool,
    pub self_user_id: String,
    pub participants: Vec<PartyParticipant>,
    pub queue: Vec<PartyQueueItem>,
    pub playback: PartyPlaybackState,
    pub last_error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PartyEvent {
    State { snapshot: PartyRoomSnapshot },
    Notice { level: String, message: String },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StartHostRequest {
    pub port: Option<u16>,
    pub room_code: Option<String>,
    pub public_host: Option<String>,
    pub queue_policy: Option<QueuePolicy>,
    pub allowed_queue_user_ids: Option<Vec<String>>,
    pub start_tunnel: Option<bool>,
    pub use_relay: Option<bool>,
    pub room_access: Option<String>,
    pub initial_queue: Option<Vec<AddQueueItemInput>>,
    pub initial_index: Option<usize>,
    pub initial_position_secs: Option<f64>,
    pub initial_is_playing: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct JoinSessionRequest {
    pub invite: String,
    pub room_code: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PlaybackSyncUpdate {
    pub current_index: Option<usize>,
    pub is_playing: Option<bool>,
    pub position_secs: Option<f64>,
    pub playback_rate: Option<f64>,
    pub repeat: Option<String>,
    pub shuffle: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PolicyUpdateRequest {
    pub queue_policy: QueuePolicy,
    pub allowed_queue_user_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FriendUpsertRequest {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: Option<String>,
    pub note: Option<String>,
    pub default_allow_queue: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileUpdateRequest {
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Hello {
        room_code: String,
        user: PartyUserRef,
    },
    AddQueue {
        item: AddQueueItemInput,
        play_next: bool,
    },
    Ping {
        sent_at_ms: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMessage {
    Welcome {
        room_code: String,
        self_user_id: String,
        state: WireRoomState,
    },
    StateSync {
        state: WireRoomState,
    },
    ActionResult {
        ok: bool,
        action: String,
        message: String,
    },
    Pong {
        sent_at_ms: i64,
        server_time_ms: i64,
    },
    RoomClosed {
        reason: String,
    },
}

struct ConnectedClient {
    user: PartyUserRef,
    joined_at: String,
    tx: mpsc::UnboundedSender<ServerMessage>,
}

struct HostSessionState {
    room_code: String,
    host_user: PartyUserRef,
    bind_port: u16,
    local_ws_url: String,
    public_ws_url: String,
    queue_policy: QueuePolicy,
    allowed_queue_user_ids: HashSet<String>,
    queue: Vec<PartyQueueItem>,
    playback: PartyPlaybackState,
    clients: HashMap<String, ConnectedClient>,
    shutdown_tx: watch::Sender<bool>,
    tunnel_child: Option<Child>,
    tunnel_running: bool,
    tunnel_error: String,
    relay_connected: bool,
    relay_error: String,
    room_access: String,
}

struct GuestSessionState {
    room_code: String,
    invite_url: String,
    self_user_id: String,
    connected: bool,
    wire_state: WireRoomState,
    last_error: String,
    outbound_tx: mpsc::UnboundedSender<ClientMessage>,
    shutdown_tx: watch::Sender<bool>,
}

enum SessionMode {
    Idle,
    Host(HostSessionState),
    Guest(GuestSessionState),
}

struct PartyManagerInner {
    state_store: StateStore,
    config: Option<ConfigStore>,
    mode: RwLock<SessionMode>,
    events_tx: broadcast::Sender<PartyEvent>,
}

#[derive(Clone)]
pub struct PartyManager {
    inner: Arc<PartyManagerInner>,
}

// ── Profile & Friends Persistence ───────────────────────────────────────────

fn generate_user_id() -> String {
    let raw = Uuid::new_v4().simple().to_string().to_uppercase();
    format!("PZ-{}-{}", &raw[0..4], &raw[4..8])
}

fn generate_room_code() -> String {
    let bytes = *Uuid::new_v4().as_bytes();
    (0..6)
        .map(|i| {
            let idx = (bytes[i] as usize) % ROOM_CODE_ALPHABET.len();
            ROOM_CODE_ALPHABET[idx] as char
        })
        .collect()
}

pub fn ensure_user_profile(store: &StateStore) -> Result<UserProfile> {
    store.update(|s| {
        if s.user_profile.user_id.trim().is_empty() {
            s.user_profile.user_id = generate_user_id();
        }
        let google_name = s.music_account_name.trim().to_string();
        if !google_name.is_empty() && s.user_profile.google_account_name != google_name {
            s.user_profile.google_account_name = google_name.clone();
        }
        if s.user_profile.display_name.trim().is_empty() {
            if !s.user_profile.google_account_name.is_empty() {
                s.user_profile.display_name = s.user_profile.google_account_name.clone();
            } else {
                let suffix = s
                    .user_profile
                    .user_id
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>();
                s.user_profile.display_name = format!("Usuario-{suffix}");
            }
        }
        Ok(s.user_profile.clone())
    })
}

pub fn update_user_profile(store: &StateStore, req: ProfileUpdateRequest) -> Result<UserProfile> {
    let name = req.display_name.trim().to_string();
    if name.is_empty() {
        return Err(CoreError::Validation(
            "El nombre visible no puede estar vacío".to_string(),
        ));
    }
    ensure_user_profile(store)?;
    store.update(|s| {
        s.user_profile.display_name = name.chars().take(48).collect();
        if !s.music_account_name.trim().is_empty() {
            s.user_profile.google_account_name = s.music_account_name.trim().to_string();
        }
        Ok(s.user_profile.clone())
    })
}

pub fn list_friends(store: &StateStore) -> Result<Vec<FriendRecord>> {
    let s = store.read()?;
    Ok(s.friends)
}

pub fn upsert_friend(store: &StateStore, req: FriendUpsertRequest) -> Result<Vec<FriendRecord>> {
    let user_id = req.user_id.trim().to_string();
    if user_id.is_empty() {
        return Err(CoreError::Validation(
            "El ID del amigo es obligatorio".to_string(),
        ));
    }
    let display_name = if req.display_name.trim().is_empty() {
        user_id.clone()
    } else {
        req.display_name.trim().chars().take(48).collect()
    };
    let google_name = req.google_account_name.unwrap_or_default().trim().to_string();
    let note: String = req.note.unwrap_or_default().trim().chars().take(120).collect();

    store.update(|s| {
        if let Some(existing) = s
            .friends
            .iter_mut()
            .find(|f| f.user_id.eq_ignore_ascii_case(&user_id))
        {
            existing.display_name = display_name;
            if !google_name.is_empty() {
                existing.google_account_name = google_name;
            }
            if !note.is_empty() {
                existing.note = note;
            }
            if let Some(allow) = req.default_allow_queue {
                existing.default_allow_queue = allow;
            }
        } else {
            s.friends.push(FriendRecord {
                user_id,
                display_name,
                google_account_name: google_name,
                note,
                default_allow_queue: req.default_allow_queue.unwrap_or(true),
                added_at: Utc::now().to_rfc3339(),
            });
        }
        Ok(s.friends.clone())
    })
}

pub fn remove_friend(store: &StateStore, user_id: &str) -> Result<Vec<FriendRecord>> {
    let target = user_id.trim();
    store.update(|s| {
        s.friends
            .retain(|f| !f.user_id.eq_ignore_ascii_case(target));
        Ok(s.friends.clone())
    })
}

// ── Network Helpers ─────────────────────────────────────────────────────────

pub fn detect_lan_ip() -> String {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0")
        && sock.connect("8.8.8.8:80").is_ok()
        && let Ok(local) = sock.local_addr()
    {
        return local.ip().to_string();
    }
    "127.0.0.1".to_string()
}

pub fn is_private_lan_ws_url(ws_url: &str) -> bool {
    let Ok(parsed) = Url::parse(ws_url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let trimmed = host.trim_matches(|c| c == '[' || c == ']');
    let Ok(ip) = trimmed.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

fn normalize_room_access(raw: Option<&str>) -> String {
    match raw.unwrap_or("request").trim().to_ascii_lowercase().as_str() {
        "open" => "open".to_string(),
        "invite_only" => "invite_only".to_string(),
        _ => "request".to_string(),
    }
}

pub fn resolve_cloudflared_bin(data_dir: Option<&Path>, root_dir: Option<&Path>) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(d) = data_dir {
        candidates.push(d.join("bin").join(exe_name));
    }
    if let Some(r) = root_dir {
        candidates.push(r.join("bin").join(exe_name));
        candidates.push(r.join(exe_name));
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("bin").join(exe_name));
        candidates.push(dir.join(exe_name));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local_app);
            candidates.push(base.join("Playzi").join("bin").join("cloudflared.exe"));
            candidates.push(
                base.join("Microsoft")
                    .join("WinGet")
                    .join("Links")
                    .join("cloudflared.exe"),
            );
        }
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(
                PathBuf::from(profile)
                    .join("scoop")
                    .join("shims")
                    .join("cloudflared.exe"),
            );
        }
        candidates.push(PathBuf::from(r"C:\Program Files (x86)\cloudflared\cloudflared.exe"));
        candidates.push(PathBuf::from(r"C:\Program Files\cloudflared\cloudflared.exe"));
        candidates.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin\cloudflared.exe"));
    }

    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join(exe_name);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

fn cloudflared_download_url() -> Option<&'static str> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-amd64.exe")
    }
    #[cfg(all(target_os = "windows", target_arch = "x86"))]
    {
        Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-windows-386.exe")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-amd64")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64")
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        None
    }
}

fn build_ws_url_from_host(raw_host: &str, default_port: u16, room_code: &str) -> String {
    let trimmed = raw_host.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        if let Ok(mut parsed) = Url::parse(trimmed) {
            if parsed.path().is_empty() || parsed.path() == "/" {
                parsed.set_path("/ws");
            }
            parsed.query_pairs_mut().clear().append_pair("room", room_code);
            return parsed.to_string();
        }
        return format!("{trimmed}/ws?room={room_code}");
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        return build_ws_url_from_host(&format!("wss://{rest}"), default_port, room_code);
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        return build_ws_url_from_host(&format!("ws://{rest}"), default_port, room_code);
    }
    if trimmed.contains(':') {
        format!("ws://{trimmed}/ws?room={room_code}")
    } else {
        format!("ws://{trimmed}:{default_port}/ws?room={room_code}")
    }
}

pub fn parse_invite_input(raw: &str, room_override: Option<&str>) -> Result<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CoreError::Validation(
            "Ingresa un enlace de invitación o dirección IP:puerto".to_string(),
        ));
    }

    let candidate = if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if trimmed.contains(':') {
        format!("ws://{trimmed}/ws")
    } else {
        format!("ws://{trimmed}:{DEFAULT_PARTY_PORT}/ws")
    };

    let mut parsed = Url::parse(&candidate)
        .map_err(|e| CoreError::Validation(format!("Invitación inválida: {e}")))?;

    let query_room = parsed
        .query_pairs()
        .find(|(k, _)| k.eq_ignore_ascii_case("room"))
        .map(|(_, v)| v.trim().to_uppercase())
        .filter(|v| !v.is_empty());

    let override_room = room_override
        .map(|r| r.trim().to_uppercase())
        .filter(|v| !v.is_empty());

    let room_code = override_room.or(query_room).ok_or_else(|| {
        CoreError::Validation(
            "Falta el código de sala (inclúyelo en el enlace o en el campo Código de sala)"
                .to_string(),
        )
    })?;

    if parsed.path().is_empty() || parsed.path() == "/" {
        parsed.set_path("/ws");
    }
    parsed
        .query_pairs_mut()
        .clear()
        .append_pair("room", &room_code);

    Ok((parsed.to_string(), room_code))
}

// ── Host State Helpers ──────────────────────────────────────────────────────

impl HostSessionState {
    fn can_user_queue(&self, user_id: &str) -> bool {
        if user_id == self.host_user.user_id {
            return true;
        }
        match self.queue_policy {
            QueuePolicy::HostOnly => false,
            QueuePolicy::All => true,
            QueuePolicy::Selected => self.allowed_queue_user_ids.contains(user_id),
        }
    }

    fn wire_state(&self) -> WireRoomState {
        let mut participants = Vec::with_capacity(self.clients.len() + 1);
        participants.push(PartyParticipant {
            user_id: self.host_user.user_id.clone(),
            display_name: self.host_user.display_name.clone(),
            google_account_name: self.host_user.google_account_name.clone(),
            is_host: true,
            can_queue: true,
            joined_at: String::new(),
        });

        let mut guests: Vec<&ConnectedClient> = self.clients.values().collect();
        guests.sort_by(|a, b| a.joined_at.cmp(&b.joined_at));
        for client in guests {
            participants.push(PartyParticipant {
                user_id: client.user.user_id.clone(),
                display_name: client.user.display_name.clone(),
                google_account_name: client.user.google_account_name.clone(),
                is_host: false,
                can_queue: self.can_user_queue(&client.user.user_id),
                joined_at: client.joined_at.clone(),
            });
        }

        let mut allowed: Vec<String> = self.allowed_queue_user_ids.iter().cloned().collect();
        allowed.sort();

        WireRoomState {
            room_code: self.room_code.clone(),
            host_user_id: self.host_user.user_id.clone(),
            host_name: self.host_user.display_name.clone(),
            queue_policy: self.queue_policy,
            allowed_queue_user_ids: allowed,
            participants,
            queue: self.queue.clone(),
            playback: self.playback.clone(),
        }
    }

    fn snapshot(&self) -> PartyRoomSnapshot {
        let wire = self.wire_state();
        let invite_url = if !self.public_ws_url.is_empty() {
            self.public_ws_url.clone()
        } else {
            self.local_ws_url.clone()
        };
        PartyRoomSnapshot {
            active: true,
            role: PartyRole::Host,
            connected: true,
            room_code: wire.room_code,
            host_user_id: wire.host_user_id.clone(),
            host_name: wire.host_name,
            bind_port: Some(self.bind_port),
            local_ws_url: self.local_ws_url.clone(),
            public_ws_url: self.public_ws_url.clone(),
            invite_url,
            tunnel_running: self.tunnel_running,
            tunnel_error: self.tunnel_error.clone(),
            relay_connected: self.relay_connected,
            relay_error: self.relay_error.clone(),
            room_access: self.room_access.clone(),
            queue_policy: wire.queue_policy,
            allowed_queue_user_ids: wire.allowed_queue_user_ids,
            can_self_queue: true,
            self_user_id: self.host_user.user_id.clone(),
            participants: wire.participants,
            queue: wire.queue,
            playback: wire.playback,
            last_error: String::new(),
        }
    }

    fn broadcast_state(&self) {
        let msg = ServerMessage::StateSync {
            state: self.wire_state(),
        };
        for client in self.clients.values() {
            let _ = client.tx.send(msg.clone());
        }
    }

    fn enqueue_item(
        &mut self,
        input: AddQueueItemInput,
        play_next: bool,
        added_by: PartyUserRef,
    ) -> Result<PartyQueueItem> {
        let url = input.url.trim().to_string();
        if url.is_empty() {
            return Err(CoreError::Validation(
                "La URL del video o canción es obligatoria".to_string(),
            ));
        }
        let item = PartyQueueItem {
            id: Uuid::new_v4().to_string(),
            url: url.clone(),
            title: if input.title.trim().is_empty() {
                url
            } else {
                input.title.trim().to_string()
            },
            thumbnail: input.thumbnail.trim().to_string(),
            channel: input.channel.trim().to_string(),
            duration_text: input.duration_text.trim().to_string(),
            source_context: if input.source_context.trim().is_empty() {
                "search".to_string()
            } else {
                input.source_context.trim().to_string()
            },
            added_by,
            added_at: Utc::now().to_rfc3339(),
        };

        let was_empty = self.queue.is_empty() || self.playback.current_index.is_none();
        if play_next && !self.queue.is_empty() {
            let insert_at = self
                .playback
                .current_index
                .map(|i| (i + 1).min(self.queue.len()))
                .unwrap_or(self.queue.len());
            self.queue.insert(insert_at, item.clone());
        } else {
            self.queue.push(item.clone());
        }

        if was_empty {
            self.playback.current_index = Some(0);
            self.playback.position_secs = 0.0;
            self.playback.is_playing = true;
            self.playback.updated_at_ms = Utc::now().timestamp_millis();
        }
        self.playback.version += 1;
        Ok(item)
    }
}

impl GuestSessionState {
    fn snapshot(&self) -> PartyRoomSnapshot {
        let can_self_queue = self
            .wire_state
            .participants
            .iter()
            .find(|p| p.user_id == self.self_user_id)
            .map(|p| p.can_queue)
            .unwrap_or_else(|| match self.wire_state.queue_policy {
                QueuePolicy::HostOnly => false,
                QueuePolicy::All => true,
                QueuePolicy::Selected => self
                    .wire_state
                    .allowed_queue_user_ids
                    .iter()
                    .any(|id| id == &self.self_user_id),
            });

        PartyRoomSnapshot {
            active: true,
            role: PartyRole::Guest,
            connected: self.connected,
            room_code: if self.wire_state.room_code.is_empty() {
                self.room_code.clone()
            } else {
                self.wire_state.room_code.clone()
            },
            host_user_id: self.wire_state.host_user_id.clone(),
            host_name: self.wire_state.host_name.clone(),
            bind_port: None,
            local_ws_url: String::new(),
            public_ws_url: String::new(),
            invite_url: self.invite_url.clone(),
            tunnel_running: false,
            tunnel_error: String::new(),
            relay_connected: false,
            relay_error: String::new(),
            room_access: String::new(),
            queue_policy: self.wire_state.queue_policy,
            allowed_queue_user_ids: self.wire_state.allowed_queue_user_ids.clone(),
            can_self_queue,
            self_user_id: self.self_user_id.clone(),
            participants: self.wire_state.participants.clone(),
            queue: self.wire_state.queue.clone(),
            playback: self.wire_state.playback.clone(),
            last_error: self.last_error.clone(),
        }
    }
}

// ── PartyManager Implementation ─────────────────────────────────────────────

impl PartyManager {
    pub fn new(state_store: StateStore) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(PartyManagerInner {
                state_store,
                config: None,
                mode: RwLock::new(SessionMode::Idle),
                events_tx,
            }),
        }
    }

    pub fn with_config(state_store: StateStore, config: ConfigStore) -> Self {
        let (events_tx, _) = broadcast::channel(128);
        Self {
            inner: Arc::new(PartyManagerInner {
                state_store,
                config: Some(config),
                mode: RwLock::new(SessionMode::Idle),
                events_tx,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PartyEvent> {
        self.inner.events_tx.subscribe()
    }

    fn emit_snapshot(&self, snapshot: PartyRoomSnapshot) {
        let _ = self.inner.events_tx.send(PartyEvent::State { snapshot });
    }

    fn emit_notice(&self, level: &str, message: impl Into<String>) {
        let _ = self.inner.events_tx.send(PartyEvent::Notice {
            level: level.to_string(),
            message: message.into(),
        });
    }

    pub async fn status(&self) -> PartyRoomSnapshot {
        let mode = self.inner.mode.read().await;
        match &*mode {
            SessionMode::Idle => {
                let profile = ensure_user_profile(&self.inner.state_store).unwrap_or_default();
                PartyRoomSnapshot {
                    self_user_id: profile.user_id,
                    ..Default::default()
                }
            }
            SessionMode::Host(h) => h.snapshot(),
            SessionMode::Guest(g) => g.snapshot(),
        }
    }

    pub async fn start_host(&self, req: StartHostRequest) -> Result<PartyRoomSnapshot> {
        // Close any previous session cleanly
        self.leave_or_stop().await;

        let profile = ensure_user_profile(&self.inner.state_store)?;
        let host_user = PartyUserRef::from(&profile);
        let requested_port = req.port.unwrap_or(DEFAULT_PARTY_PORT);

        let listener = TcpListener::bind(("0.0.0.0", requested_port))
            .await
            .map_err(|e| {
                CoreError::Io(std::io::Error::new(
                    e.kind(),
                    format!("No se pudo abrir el puerto {requested_port}: {e}"),
                ))
            })?;
        let actual_port = listener.local_addr()?.port();

        let room_code = req
            .room_code
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(generate_room_code);

        let lan_ip = detect_lan_ip();
        let local_ws_url = format!("ws://{lan_ip}:{actual_port}/ws?room={room_code}");
        let public_ws_url = req
            .public_host
            .as_deref()
            .map(|h| build_ws_url_from_host(h, actual_port, &room_code))
            .unwrap_or_default();

        let mut allowed_set: HashSet<String> = HashSet::new();
        if let Some(ids) = req.allowed_queue_user_ids {
            for id in ids {
                if !id.trim().is_empty() {
                    allowed_set.insert(id.trim().to_string());
                }
            }
        } else if let Ok(friends) = list_friends(&self.inner.state_store) {
            for f in friends {
                if f.default_allow_queue {
                    allowed_set.insert(f.user_id);
                }
            }
        }

        let mut queue = Vec::new();
        if let Some(initial) = req.initial_queue {
            for item in initial {
                if item.url.trim().is_empty() {
                    continue;
                }
                queue.push(PartyQueueItem {
                    id: Uuid::new_v4().to_string(),
                    url: item.url.trim().to_string(),
                    title: if item.title.trim().is_empty() {
                        item.url.trim().to_string()
                    } else {
                        item.title.trim().to_string()
                    },
                    thumbnail: item.thumbnail,
                    channel: item.channel,
                    duration_text: item.duration_text,
                    source_context: if item.source_context.is_empty() {
                        "search".to_string()
                    } else {
                        item.source_context
                    },
                    added_by: host_user.clone(),
                    added_at: Utc::now().to_rfc3339(),
                });
            }
        }

        let current_index = if queue.is_empty() {
            None
        } else {
            Some(req.initial_index.unwrap_or(0).min(queue.len() - 1))
        };

        let playback = PartyPlaybackState {
            current_index,
            is_playing: req.initial_is_playing.unwrap_or(false) && current_index.is_some(),
            position_secs: req.initial_position_secs.unwrap_or(0.0).max(0.0),
            updated_at_ms: Utc::now().timestamp_millis(),
            playback_rate: 1.0,
            repeat: "none".to_string(),
            shuffle: false,
            version: 1,
        };

        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let relay_shutdown_rx = shutdown_tx.subscribe();
        let room_access = normalize_room_access(req.room_access.as_deref());

        let host_state = HostSessionState {
            room_code: room_code.clone(),
            host_user,
            bind_port: actual_port,
            local_ws_url,
            public_ws_url: public_ws_url.clone(),
            queue_policy: req.queue_policy.unwrap_or(QueuePolicy::All),
            allowed_queue_user_ids: allowed_set,
            queue,
            playback,
            clients: HashMap::new(),
            shutdown_tx,
            tunnel_child: None,
            tunnel_running: false,
            tunnel_error: String::new(),
            relay_connected: false,
            relay_error: String::new(),
            room_access,
        };

        let snap = host_state.snapshot();
        {
            let mut mode = self.inner.mode.write().await;
            *mode = SessionMode::Host(host_state);
        }
        self.emit_snapshot(snap.clone());

        // Spawn accept loop
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        match accepted {
                            Ok((stream, peer_addr)) => {
                                let mgr = manager.clone();
                                tokio::spawn(async move {
                                    mgr.handle_incoming_connection(stream, peer_addr).await;
                                });
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        let configured_hub = self
            .inner
            .config
            .as_ref()
            .map(|c| c.snapshot().social_hub_url)
            .unwrap_or_default();

        if req.use_relay.unwrap_or(true) && !configured_hub.trim().is_empty() && public_ws_url.is_empty() {
            self.spawn_host_relay(configured_hub, room_code, relay_shutdown_rx);
        }

        if req.start_tunnel.unwrap_or(false) {
            let _ = self.start_cloudflare_tunnel().await;
            return Ok(self.status().await);
        }

        Ok(self.status().await)
    }

    pub async fn set_room_access(&self, room_access: String) -> Result<PartyRoomSnapshot> {
        let normalized = normalize_room_access(Some(&room_access));
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede cambiar el acceso a la sala".to_string(),
            ));
        };
        host.room_access = normalized;
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }

    async fn ensure_cloudflared_installed(&self) -> Result<PathBuf> {
        let (data_dir, root_dir) = self
            .inner
            .config
            .as_ref()
            .map(|c| {
                let s = c.snapshot();
                (Some(s.data_dir), Some(s.root))
            })
            .unwrap_or((None, None));

        if let Some(existing) = resolve_cloudflared_bin(data_dir.as_deref(), root_dir.as_deref()) {
            return Ok(existing);
        }

        let Some(url) = cloudflared_download_url() else {
            return Err(CoreError::External(
                "No se encontró 'cloudflared' y no hay descarga automática para esta arquitectura."
                    .to_string(),
            ));
        };

        let target_dir = data_dir
            .unwrap_or_else(|| std::env::temp_dir().join("playzi"))
            .join("bin");
        tokio::fs::create_dir_all(&target_dir).await?;

        let exe_name = if cfg!(windows) {
            "cloudflared.exe"
        } else {
            "cloudflared"
        };
        let target_path = target_dir.join(exe_name);
        let tmp_path = target_dir.join(format!("{exe_name}.downloading"));

        self.emit_notice("info", "Descargando cloudflared automáticamente...");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent("Playzi/1.0")
            .build()
            .map_err(|e| CoreError::External(format!("Error HTTP descargando cloudflared: {e}")))?;

        let resp = client.get(url).send().await.map_err(|e| {
            CoreError::External(format!("No se pudo descargar cloudflared automáticamente: {e}"))
        })?;
        if !resp.status().is_success() {
            return Err(CoreError::External(format!(
                "Error descargando cloudflared (HTTP {})",
                resp.status()
            )));
        }
        let bytes = resp.bytes().await.map_err(|e| {
            CoreError::External(format!("Error leyendo descarga de cloudflared: {e}"))
        })?;
        if bytes.len() < 1_000_000 {
            return Err(CoreError::External(
                "La descarga de cloudflared resultó incompleta.".to_string(),
            ));
        }

        tokio::fs::write(&tmp_path, &bytes).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
        }
        let _ = tokio::fs::remove_file(&target_path).await;
        tokio::fs::rename(&tmp_path, &target_path).await?;

        self.emit_notice("ok", "cloudflared instalado automáticamente.");
        Ok(target_path)
    }

    fn spawn_host_relay(
        &self,
        hub_url: String,
        room_code: String,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        let Some(host_relay_url) = build_relay_ws_url(&hub_url, &room_code, Some("host")) else {
            return;
        };
        let Some(guest_relay_url) = build_relay_ws_url(&hub_url, &room_code, None) else {
            return;
        };
        let manager = self.clone();

        tokio::spawn(async move {
            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                let connect_res = tokio::time::timeout(
                    Duration::from_secs(12),
                    tokio_tungstenite::connect_async(&host_relay_url),
                )
                .await;

                let ws_stream = match connect_res {
                    Ok(Ok((stream, _))) => stream,
                    Ok(Err(e)) => {
                        let mut mode = manager.inner.mode.write().await;
                        if let SessionMode::Host(host) = &mut *mode {
                            host.relay_connected = false;
                            host.relay_error = format!("Relay online desconectado: {e}");
                            let snap = host.snapshot();
                            drop(mode);
                            manager.emit_snapshot(snap);
                        } else {
                            break;
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() { break; }
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        let mut mode = manager.inner.mode.write().await;
                        if let SessionMode::Host(host) = &mut *mode {
                            host.relay_connected = false;
                            host.relay_error =
                                "Tiempo de espera agotado conectando al Relay online.".to_string();
                            let snap = host.snapshot();
                            drop(mode);
                            manager.emit_snapshot(snap);
                        } else {
                            break;
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() { break; }
                            }
                        }
                        continue;
                    }
                };

                {
                    let mut mode = manager.inner.mode.write().await;
                    if let SessionMode::Host(host) = &mut *mode {
                        host.relay_connected = true;
                        host.relay_error.clear();
                        if host.public_ws_url.is_empty() || host.public_ws_url == guest_relay_url {
                            host.public_ws_url = guest_relay_url.clone();
                        }
                        let snap = host.snapshot();
                        drop(mode);
                        manager.emit_snapshot(snap);
                    } else {
                        break;
                    }
                }

                let (mut ws_sink, mut ws_source) = ws_stream.split();
                let (relay_out_tx, mut relay_out_rx) = mpsc::unbounded_channel::<String>();

                let writer = tokio::spawn(async move {
                    while let Some(txt) = relay_out_rx.recv().await {
                        if ws_sink.send(Message::Text(txt.into())).await.is_err() {
                            break;
                        }
                    }
                    let _ = ws_sink.close().await;
                });

                // conn_id -> (ServerMessage tx, registered_user_id)
                let mut relay_guests: HashMap<
                    String,
                    (mpsc::UnboundedSender<ServerMessage>, Option<String>),
                > = HashMap::new();
                let mut ping_interval = tokio::time::interval(Duration::from_secs(25));
                ping_interval.tick().await;

                loop {
                    tokio::select! {
                        _ = ping_interval.tick() => {
                            let _ = relay_out_tx.send(json!({ "relayPing": true }).to_string());
                        }
                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() {
                                break;
                            }
                        }
                        frame = ws_source.next() => {
                            match frame {
                                Some(Ok(Message::Text(text))) => {
                                    let Ok(val) = serde_json::from_str::<Value>(&text) else {
                                        continue;
                                    };
                                    if let Some(conn_id) = val.get("relayOpen").and_then(Value::as_str) {
                                        let cid = conn_id.to_string();
                                        let (guest_tx, mut guest_rx) = mpsc::unbounded_channel::<ServerMessage>();
                                        let rtx = relay_out_tx.clone();
                                        let cid_clone = cid.clone();
                                        tokio::spawn(async move {
                                            while let Some(smsg) = guest_rx.recv().await {
                                                if let Ok(payload) = serde_json::to_string(&smsg) {
                                                    let envelope = json!({
                                                        "relayTo": cid_clone,
                                                        "payload": payload,
                                                    }).to_string();
                                                    if rtx.send(envelope).is_err() {
                                                        break;
                                                    }
                                                }
                                            }
                                        });
                                        relay_guests.insert(cid, (guest_tx, None));
                                    } else if let Some(conn_id) = val.get("relayFrom").and_then(Value::as_str)
                                        && let Some(payload) = val.get("payload").and_then(Value::as_str)
                                    {
                                        if !relay_guests.contains_key(conn_id) {
                                            let cid = conn_id.to_string();
                                            let (guest_tx, mut guest_rx) = mpsc::unbounded_channel::<ServerMessage>();
                                            let rtx = relay_out_tx.clone();
                                            let cid_clone = cid.clone();
                                            tokio::spawn(async move {
                                                while let Some(smsg) = guest_rx.recv().await {
                                                    if let Ok(p) = serde_json::to_string(&smsg) {
                                                        let envelope = json!({
                                                            "relayTo": cid_clone,
                                                            "payload": p,
                                                        }).to_string();
                                                        if rtx.send(envelope).is_err() {
                                                            break;
                                                        }
                                                    }
                                                }
                                            });
                                            relay_guests.insert(cid, (guest_tx, None));
                                        }
                                        if let Ok(client_msg) = serde_json::from_str::<ClientMessage>(payload)
                                            && let Some((guest_tx, reg_uid)) = relay_guests.get_mut(conn_id)
                                        {
                                            let keep_open = manager
                                                .handle_client_message(client_msg, guest_tx, reg_uid)
                                                .await;
                                            if !keep_open {
                                                let _ = relay_out_tx.send(json!({ "relayKick": conn_id }).to_string());
                                            }
                                        }
                                    } else if let Some(conn_id) = val.get("relayClosed").and_then(Value::as_str)
                                        && let Some((_, reg_uid)) = relay_guests.remove(conn_id)
                                        && let Some(uid) = reg_uid
                                    {
                                        manager.unregister_client(&uid).await;
                                    }
                                }
                                Some(Ok(Message::Close(_))) | None => break,
                                Some(Err(_)) => break,
                                _ => {}
                            }
                        }
                    }
                }

                writer.abort();
                for (_, (_, reg_uid)) in relay_guests.drain() {
                    if let Some(uid) = reg_uid {
                        manager.unregister_client(&uid).await;
                    }
                }

                {
                    let mut mode = manager.inner.mode.write().await;
                    if let SessionMode::Host(host) = &mut *mode {
                        host.relay_connected = false;
                        let snap = host.snapshot();
                        drop(mode);
                        manager.emit_snapshot(snap);
                    } else {
                        break;
                    }
                }

                if *shutdown_rx.borrow() {
                    break;
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(4)) => {}
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() { break; }
                    }
                }
            }
        });
    }

    pub async fn start_cloudflare_tunnel(&self) -> Result<PartyRoomSnapshot> {
        let (port, room_code) = {
            let mut mode = self.inner.mode.write().await;
            let SessionMode::Host(host) = &mut *mode else {
                return Err(CoreError::Validation(
                    "Debes iniciar una sesión como host antes de activar el túnel".to_string(),
                ));
            };
            if host.tunnel_running {
                return Ok(host.snapshot());
            }
            host.tunnel_error.clear();
            (host.bind_port, host.room_code.clone())
        };

        let cf_bin = match self.ensure_cloudflared_installed().await {
            Ok(bin) => bin,
            Err(e) => {
                let msg = e.to_string();
                let mut mode = self.inner.mode.write().await;
                if let SessionMode::Host(host) = &mut *mode {
                    host.tunnel_error = msg.clone();
                    let snap = host.snapshot();
                    drop(mode);
                    self.emit_snapshot(snap);
                }
                return Err(CoreError::External(msg));
            }
        };

        let mut cmd = Command::new(&cf_bin);
        cmd.args([
            "tunnel",
            "--url",
            &format!("http://127.0.0.1:{port}"),
            "--no-autoupdate",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

        #[cfg(target_os = "windows")]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!(
                    "No se pudo iniciar cloudflared ({e}). Usa el Hub Online (Cloudflare) o Dirección Pública."
                );
                let mut mode = self.inner.mode.write().await;
                if let SessionMode::Host(host) = &mut *mode {
                    host.tunnel_error = msg.clone();
                    let snap = host.snapshot();
                    drop(mode);
                    self.emit_snapshot(snap);
                }
                return Err(CoreError::External(msg));
            }
        };

        let stderr = child.stderr.take();
        {
            let mut mode = self.inner.mode.write().await;
            if let SessionMode::Host(host) = &mut *mode {
                host.tunnel_child = Some(child);
                host.tunnel_running = true;
                host.tunnel_error.clear();
                let snap = host.snapshot();
                drop(mode);
                self.emit_snapshot(snap);
            }
        }

        if let Some(stderr) = stderr {
            let manager = self.clone();
            tokio::spawn(async move {
                let re = Regex::new(r"https://[a-zA-Z0-9-]+\.trycloudflare\.com").ok();
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(regex) = &re
                        && let Some(m) = regex.find(&line)
                    {
                        let https_url = m.as_str();
                        let ws_url = format!(
                            "{}/ws?room={}",
                            https_url.replacen("https://", "wss://", 1),
                            room_code
                        );
                        let mut mode = manager.inner.mode.write().await;
                        if let SessionMode::Host(host) = &mut *mode {
                            host.public_ws_url = ws_url;
                            host.tunnel_error.clear();
                            let snap = host.snapshot();
                            drop(mode);
                            manager.emit_snapshot(snap);
                            manager.emit_notice("ok", "Túnel público de internet listo.");
                        }
                        break;
                    }
                }
            });
        }

        Ok(self.status().await)
    }

    pub async fn set_public_host(&self, public_host: String) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede configurar la dirección pública".to_string(),
            ));
        };
        host.public_ws_url = build_ws_url_from_host(&public_host, host.bind_port, &host.room_code);
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }

    async fn handle_client_message(
        &self,
        client_msg: ClientMessage,
        out_tx: &mpsc::UnboundedSender<ServerMessage>,
        registered_user_id: &mut Option<String>,
    ) -> bool {
        match client_msg {
            ClientMessage::Hello { room_code, mut user } => {
                let mut mode = self.inner.mode.write().await;
                let SessionMode::Host(host) = &mut *mode else {
                    let _ = out_tx.send(ServerMessage::RoomClosed {
                        reason: "La sesión ya no está activa".to_string(),
                    });
                    return false;
                };

                if !host.room_code.eq_ignore_ascii_case(room_code.trim()) {
                    let _ = out_tx.send(ServerMessage::RoomClosed {
                        reason: "Código de sala incorrecto".to_string(),
                    });
                    return false;
                }

                if user.user_id.trim().is_empty() {
                    user.user_id = generate_user_id();
                }
                if user.display_name.trim().is_empty() {
                    user.display_name = if !user.google_account_name.is_empty() {
                        user.google_account_name.clone()
                    } else {
                        user.user_id.clone()
                    };
                }

                let uid = user.user_id.clone();
                let uname = user.display_name.clone();
                host.clients.insert(
                    uid.clone(),
                    ConnectedClient {
                        user,
                        joined_at: Utc::now().to_rfc3339(),
                        tx: out_tx.clone(),
                    },
                );
                *registered_user_id = Some(uid.clone());

                let wire = host.wire_state();
                let _ = out_tx.send(ServerMessage::Welcome {
                    room_code: host.room_code.clone(),
                    self_user_id: uid,
                    state: wire,
                });
                host.broadcast_state();
                let snap = host.snapshot();
                drop(mode);
                self.emit_snapshot(snap);
                self.emit_notice("info", format!("{uname} se unió a la sesión"));
                true
            }
            ClientMessage::AddQueue { item, play_next } => {
                let Some(uid) = registered_user_id.clone() else {
                    return true;
                };
                let mut mode = self.inner.mode.write().await;
                let SessionMode::Host(host) = &mut *mode else {
                    return false;
                };
                if !host.can_user_queue(&uid) {
                    let _ = out_tx.send(ServerMessage::ActionResult {
                        ok: false,
                        action: "add_queue".to_string(),
                        message: "El host no ha habilitado permisos de cola para tu usuario"
                            .to_string(),
                    });
                    return true;
                }
                let Some(user_ref) = host.clients.get(&uid).map(|c| c.user.clone()) else {
                    return true;
                };
                match host.enqueue_item(item, play_next, user_ref.clone()) {
                    Ok(added) => {
                        let _ = out_tx.send(ServerMessage::ActionResult {
                            ok: true,
                            action: "add_queue".to_string(),
                            message: format!("Agregado a la cola: {}", added.title),
                        });
                        host.broadcast_state();
                        let snap = host.snapshot();
                        drop(mode);
                        self.emit_snapshot(snap);
                        self.emit_notice(
                            "info",
                            format!(
                                "{} agregó \"{}\" a la cola",
                                user_ref.display_name, added.title
                            ),
                        );
                    }
                    Err(e) => {
                        let _ = out_tx.send(ServerMessage::ActionResult {
                            ok: false,
                            action: "add_queue".to_string(),
                            message: e.to_string(),
                        });
                    }
                }
                true
            }
            ClientMessage::Ping { sent_at_ms } => {
                let _ = out_tx.send(ServerMessage::Pong {
                    sent_at_ms,
                    server_time_ms: Utc::now().timestamp_millis(),
                });
                true
            }
        }
    }

    async fn unregister_client(&self, uid: &str) {
        let mut mode = self.inner.mode.write().await;
        if let SessionMode::Host(host) = &mut *mode
            && let Some(removed) = host.clients.remove(uid)
        {
            host.broadcast_state();
            let snap = host.snapshot();
            drop(mode);
            self.emit_snapshot(snap);
            self.emit_notice(
                "info",
                format!("{} salió de la sesión", removed.user.display_name),
            );
        }
    }

    async fn handle_incoming_connection(
        &self,
        stream: tokio::net::TcpStream,
        _peer_addr: SocketAddr,
    ) {
        let Ok(ws_stream) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (mut ws_sink, mut ws_source) = ws_stream.split();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMessage>();

        let writer_task = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                if let Ok(txt) = serde_json::to_string(&msg)
                    && ws_sink.send(Message::Text(txt.into())).await.is_err()
                {
                    break;
                }
            }
            let _ = ws_sink.close().await;
        });

        let mut registered_user_id: Option<String> = None;

        while let Some(frame) = ws_source.next().await {
            let Ok(msg) = frame else { break };
            let Message::Text(text) = msg else {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
                continue;
            };
            let Ok(client_msg) = serde_json::from_str::<ClientMessage>(&text) else {
                continue;
            };

            if !self
                .handle_client_message(client_msg, &out_tx, &mut registered_user_id)
                .await
            {
                break;
            }
        }

        if let Some(uid) = registered_user_id {
            self.unregister_client(&uid).await;
        }

        writer_task.abort();
    }

    pub async fn join_session(&self, req: JoinSessionRequest) -> Result<PartyRoomSnapshot> {
        self.leave_or_stop().await;

        let (ws_url, room_code) = parse_invite_input(&req.invite, req.room_code.as_deref())?;
        let profile = ensure_user_profile(&self.inner.state_store)?;
        let user_ref = PartyUserRef::from(&profile);

        let connect_result = tokio::time::timeout(
            Duration::from_secs(12),
            tokio_tungstenite::connect_async(&ws_url),
        )
        .await;

        let (ws_stream, _) = match connect_result {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                if is_private_lan_ws_url(&ws_url) {
                    return Err(CoreError::External(format!(
                        "No se pudo conectar a la sesión ({ws_url}): esa dirección es una IP privada de red local (LAN) y no funciona por internet. Pide al host que use el Hub Online (Cloudflare) o active el Túnel Cloudflare. Detalle: {e}"
                    )));
                }
                return Err(CoreError::External(format!(
                    "No se pudo conectar a la sesión ({ws_url}): {e}"
                )));
            }
            Err(_) => {
                if is_private_lan_ws_url(&ws_url) {
                    return Err(CoreError::External(format!(
                        "Tiempo de espera agotado al conectar a {ws_url}: esa dirección es una IP privada de red local (LAN) y no es accesible desde otra red. Pide al host que active el Hub Online (Cloudflare) o el Túnel Cloudflare."
                    )));
                }
                return Err(CoreError::External(format!(
                    "Tiempo de espera agotado al conectar a la sesión ({ws_url}). Verifica que la sala siga abierta."
                )));
            }
        };

        let (mut ws_sink, mut ws_source) = ws_stream.split();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let _ = outbound_tx.send(ClientMessage::Hello {
            room_code: room_code.clone(),
            user: user_ref.clone(),
        });

        let guest = GuestSessionState {
            room_code: room_code.clone(),
            invite_url: ws_url.clone(),
            self_user_id: user_ref.user_id.clone(),
            connected: true,
            wire_state: WireRoomState {
                room_code: room_code.clone(),
                ..Default::default()
            },
            last_error: String::new(),
            outbound_tx,
            shutdown_tx,
        };
        let initial_snap = guest.snapshot();
        {
            let mut mode = self.inner.mode.write().await;
            *mode = SessionMode::Guest(guest);
        }
        self.emit_snapshot(initial_snap.clone());

        let manager = self.clone();
        tokio::spawn(async move {
            let writer = tokio::spawn(async move {
                while let Some(msg) = outbound_rx.recv().await {
                    if let Ok(txt) = serde_json::to_string(&msg)
                        && ws_sink.send(Message::Text(txt.into())).await.is_err()
                    {
                        break;
                    }
                }
                let _ = ws_sink.close().await;
            });

            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    frame = ws_source.next() => {
                        match frame {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) {
                                    match server_msg {
                                        ServerMessage::Welcome { self_user_id, state, .. } => {
                                            let mut mode = manager.inner.mode.write().await;
                                            if let SessionMode::Guest(g) = &mut *mode {
                                                g.self_user_id = self_user_id;
                                                g.connected = true;
                                                g.wire_state = state;
                                                g.last_error.clear();
                                                let snap = g.snapshot();
                                                drop(mode);
                                                manager.emit_snapshot(snap);
                                            }
                                        }
                                        ServerMessage::StateSync { state } => {
                                            let mut mode = manager.inner.mode.write().await;
                                            if let SessionMode::Guest(g) = &mut *mode {
                                                g.wire_state = state;
                                                let snap = g.snapshot();
                                                drop(mode);
                                                manager.emit_snapshot(snap);
                                            }
                                        }
                                        ServerMessage::ActionResult { ok, message, .. } => {
                                            manager.emit_notice(if ok { "ok" } else { "error" }, message);
                                        }
                                        ServerMessage::RoomClosed { reason } => {
                                            let mut mode = manager.inner.mode.write().await;
                                            *mode = SessionMode::Idle;
                                            drop(mode);
                                            let idle = manager.status().await;
                                            manager.emit_snapshot(idle);
                                            manager.emit_notice("error", reason);
                                            break;
                                        }
                                        ServerMessage::Pong { .. } => {}
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) | None => {
                                let mut mode = manager.inner.mode.write().await;
                                if let SessionMode::Guest(g) = &mut *mode {
                                    g.connected = false;
                                    g.last_error = "Desconectado del host".to_string();
                                    let snap = g.snapshot();
                                    drop(mode);
                                    manager.emit_snapshot(snap);
                                    manager.emit_notice("error", "Conexión cerrada con el host");
                                }
                                break;
                            }
                            Some(Err(e)) => {
                                let mut mode = manager.inner.mode.write().await;
                                if let SessionMode::Guest(g) = &mut *mode {
                                    g.connected = false;
                                    g.last_error = format!("Error de conexión: {e}");
                                    let snap = g.snapshot();
                                    drop(mode);
                                    manager.emit_snapshot(snap);
                                }
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }

            writer.abort();
        });

        Ok(initial_snap)
    }

    pub async fn leave_or_stop(&self) -> PartyRoomSnapshot {
        let mut mode = self.inner.mode.write().await;
        match std::mem::replace(&mut *mode, SessionMode::Idle) {
            SessionMode::Idle => {}
            SessionMode::Host(mut host) => {
                let _ = host.shutdown_tx.send(true);
                for client in host.clients.values() {
                    let _ = client.tx.send(ServerMessage::RoomClosed {
                        reason: "El host cerró la sesión".to_string(),
                    });
                }
                if let Some(mut child) = host.tunnel_child.take() {
                    let _ = child.kill().await;
                }
            }
            SessionMode::Guest(guest) => {
                let _ = guest.shutdown_tx.send(true);
            }
        }
        drop(mode);
        let snap = self.status().await;
        self.emit_snapshot(snap.clone());
        snap
    }

    pub async fn add_to_queue(
        &self,
        item: AddQueueItemInput,
        play_next: bool,
    ) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        match &mut *mode {
            SessionMode::Idle => Err(CoreError::Validation(
                "No estás en una sesión compartida".to_string(),
            )),
            SessionMode::Host(host) => {
                let added_by = host.host_user.clone();
                host.enqueue_item(item, play_next, added_by)?;
                host.broadcast_state();
                let snap = host.snapshot();
                drop(mode);
                self.emit_snapshot(snap.clone());
                Ok(snap)
            }
            SessionMode::Guest(guest) => {
                let snap = guest.snapshot();
                if !snap.can_self_queue {
                    return Err(CoreError::Validation(
                        "El host no ha habilitado permisos de cola para tu usuario".to_string(),
                    ));
                }
                guest
                    .outbound_tx
                    .send(ClientMessage::AddQueue { item, play_next })
                    .map_err(|_| {
                        CoreError::External("No se pudo enviar la solicitud al host".to_string())
                    })?;
                Ok(snap)
            }
        }
    }

    pub async fn remove_queue_item(&self, index: usize) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede eliminar elementos de la cola".to_string(),
            ));
        };
        if index >= host.queue.len() {
            return Err(CoreError::Validation("Índice fuera de rango".to_string()));
        }
        host.queue.remove(index);
        if host.queue.is_empty() {
            host.playback.current_index = None;
            host.playback.is_playing = false;
            host.playback.position_secs = 0.0;
        } else if let Some(cur) = host.playback.current_index {
            if index < cur {
                host.playback.current_index = Some(cur - 1);
            } else if index == cur {
                host.playback.current_index = Some(cur.min(host.queue.len() - 1));
                host.playback.position_secs = 0.0;
            }
        }
        host.playback.updated_at_ms = Utc::now().timestamp_millis();
        host.playback.version += 1;
        host.broadcast_state();
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }

    pub async fn sync_playback(&self, update: PlaybackSyncUpdate) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede controlar la reproducción sincronizada".to_string(),
            ));
        };
        if let Some(idx) = update.current_index {
            if idx < host.queue.len() {
                host.playback.current_index = Some(idx);
            }
        }
        if let Some(playing) = update.is_playing {
            host.playback.is_playing = playing;
        }
        if let Some(pos) = update.position_secs
            && pos.is_finite()
        {
            host.playback.position_secs = pos.max(0.0);
        }
        if let Some(rate) = update.playback_rate
            && rate.is_finite()
            && rate > 0.0
        {
            host.playback.playback_rate = rate;
        }
        if let Some(repeat) = update.repeat {
            host.playback.repeat = repeat;
        }
        if let Some(shuffle) = update.shuffle {
            host.playback.shuffle = shuffle;
        }
        host.playback.updated_at_ms = Utc::now().timestamp_millis();
        host.playback.version += 1;
        host.broadcast_state();
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }

    pub async fn update_policy(&self, req: PolicyUpdateRequest) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede cambiar los permisos de la cola".to_string(),
            ));
        };
        host.queue_policy = req.queue_policy;
        if let Some(ids) = req.allowed_queue_user_ids {
            host.allowed_queue_user_ids = ids
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        host.playback.version += 1;
        host.broadcast_state();
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }

    pub async fn kick_participant(&self, user_id: &str) -> Result<PartyRoomSnapshot> {
        let mut mode = self.inner.mode.write().await;
        let SessionMode::Host(host) = &mut *mode else {
            return Err(CoreError::Validation(
                "Solo el host puede expulsar participantes".to_string(),
            ));
        };
        if let Some(removed) = host.clients.remove(user_id.trim()) {
            let _ = removed.tx.send(ServerMessage::RoomClosed {
                reason: "Fuiste removido de la sesión por el host".to_string(),
            });
            host.broadcast_state();
        }
        let snap = host.snapshot();
        drop(mode);
        self.emit_snapshot(snap.clone());
        Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir, name: &str) -> StateStore {
        let store = StateStore::new(dir.path().join(format!("{name}.json")));
        let _ = store.update(|s| {
            s.music_account_name = format!("{name}@google");
            Ok(())
        });
        store
    }

    #[test]
    fn profile_and_friends_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir, "alice");
        let prof = ensure_user_profile(&store).unwrap();
        assert!(prof.user_id.starts_with("PZ-"));
        assert_eq!(prof.google_account_name, "alice@google");
        assert_eq!(prof.display_name, "alice@google");

        let updated = update_user_profile(
            &store,
            ProfileUpdateRequest {
                display_name: "Alice DJ".to_string(),
            },
        )
        .unwrap();
        assert_eq!(updated.display_name, "Alice DJ");

        let friends = upsert_friend(
            &store,
            FriendUpsertRequest {
                user_id: "PZ-1111-2222".to_string(),
                display_name: "Bob".to_string(),
                google_account_name: Some("Bob YT".to_string()),
                note: Some("Amigo".to_string()),
                default_allow_queue: Some(true),
            },
        )
        .unwrap();
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].user_id, "PZ-1111-2222");

        let after = remove_friend(&store, "PZ-1111-2222").unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn host_and_guest_sync_queue_and_permissions() {
        let dir = TempDir::new().unwrap();
        let host_store = test_store(&dir, "host");
        let guest_store = test_store(&dir, "guest");
        let host_mgr = PartyManager::new(host_store.clone());
        let guest_mgr = PartyManager::new(guest_store.clone());
        let guest_profile = ensure_user_profile(&guest_store).unwrap();

        let host_snap = host_mgr
            .start_host(StartHostRequest {
                port: Some(0),
                room_code: Some("TEST99".to_string()),
                queue_policy: Some(QueuePolicy::HostOnly),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(host_snap.active);
        let port = host_snap.bind_port.unwrap();

        let _ = guest_mgr
            .join_session(JoinSessionRequest {
                invite: format!("ws://127.0.0.1:{port}/ws?room=TEST99"),
                room_code: None,
            })
            .await
            .unwrap();

        // Wait briefly for Welcome + StateSync
        tokio::time::sleep(Duration::from_millis(120)).await;
        let g_status = guest_mgr.status().await;
        assert!(g_status.connected);
        assert_eq!(g_status.participants.len(), 2);
        assert!(!g_status.can_self_queue);

        // Guest cannot add when HostOnly
        let err = guest_mgr
            .add_to_queue(
                AddQueueItemInput {
                    url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
                    title: "Track 1".to_string(),
                    ..Default::default()
                },
                false,
            )
            .await;
        assert!(err.is_err());

        // Host enables Selected policy for this guest
        host_mgr
            .update_policy(PolicyUpdateRequest {
                queue_policy: QueuePolicy::Selected,
                allowed_queue_user_ids: Some(vec![guest_profile.user_id.clone()]),
            })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        let g_status = guest_mgr.status().await;
        assert!(g_status.can_self_queue);

        // Guest adds a track -> attributed to guest
        guest_mgr
            .add_to_queue(
                AddQueueItemInput {
                    url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".to_string(),
                    title: "Guest Song".to_string(),
                    channel: "Artist".to_string(),
                    ..Default::default()
                },
                false,
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(150)).await;
        let h_status = host_mgr.status().await;
        assert_eq!(h_status.queue.len(), 1);
        assert_eq!(h_status.queue[0].title, "Guest Song");
        assert_eq!(h_status.queue[0].added_by.user_id, guest_profile.user_id);

        // Host syncs playback position -> guest receives it
        host_mgr
            .sync_playback(PlaybackSyncUpdate {
                current_index: Some(0),
                is_playing: Some(true),
                position_secs: Some(42.5),
                ..Default::default()
            })
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(120)).await;
        let g_status = guest_mgr.status().await;
        assert!(g_status.playback.is_playing);
        assert!((g_status.playback.position_secs - 42.5).abs() < 0.01);

        // Cleanup
        host_mgr.leave_or_stop().await;
        guest_mgr.leave_or_stop().await;
    }

    #[test]
    fn detects_private_lan_ws_urls() {
        assert!(is_private_lan_ws_url("ws://192.168.1.11:8766/ws?room=4ENUAH"));
        assert!(is_private_lan_ws_url("ws://10.0.0.5:8766/ws?room=ABC123"));
        assert!(is_private_lan_ws_url("ws://172.16.4.2:8766/ws?room=ABC123"));
        assert!(!is_private_lan_ws_url("wss://playzi-hub.example.workers.dev/ws?room=4ENUAH"));
        assert!(!is_private_lan_ws_url("wss://cool-name.trycloudflare.com/ws?room=4ENUAH"));
        assert!(!is_private_lan_ws_url("ws://127.0.0.1:8766/ws?room=4ENUAH"));
    }
}
