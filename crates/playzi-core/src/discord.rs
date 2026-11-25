use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Notify, RwLock, broadcast, mpsc};
use uuid::Uuid;

use crate::config::ConfigStore;
use crate::error::{CoreError, Result};
use crate::party::PartyRoomSnapshot;
use crate::player::PlaybackState;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;
const MAX_IPC_FRAME_BYTES: usize = 2 * 1024 * 1024;
const JOIN_SECRET_PREFIX: &str = "pz1|";
const MAX_JOIN_SECRET_BYTES: usize = 128;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscordMediaState {
    pub active: bool,
    pub title: String,
    pub channel: String,
    pub thumbnail: String,
    pub url: String,
    pub is_playing: bool,
    pub is_audio: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscordJoinRequest {
    pub user_id: String,
    pub username: String,
    pub global_name: String,
    pub avatar: String,
    pub requested_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscordStatusSnapshot {
    pub enabled: bool,
    pub configured: bool,
    pub connected: bool,
    pub show_media: bool,
    pub show_party: bool,
    pub invite_active: bool,
    pub user_id: String,
    pub username: String,
    pub global_name: String,
    pub last_error: String,
    pub pending_join_requests: Vec<DiscordJoinRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DiscordEvent {
    State {
        snapshot: DiscordStatusSnapshot,
    },
    Notice {
        level: String,
        message: String,
    },
    JoinRoom {
        room_code: String,
        invite_url: String,
    },
}

enum OutboundCmd {
    SetActivity(Option<Value>),
    RespondJoinRequest { user_id: String, accept: bool },
    Disconnect,
}

struct DiscordInner {
    connected: bool,
    user_id: String,
    username: String,
    global_name: String,
    last_error: String,
    media: Option<DiscordMediaState>,
    party: Option<PartyRoomSnapshot>,
    pending_join_requests: Vec<DiscordJoinRequest>,
    last_sent_activity: Option<Value>,
    invite_active: bool,
    outbound_tx: Option<mpsc::UnboundedSender<OutboundCmd>>,
    shutdown: bool,
}

impl Default for DiscordInner {
    fn default() -> Self {
        Self {
            connected: false,
            user_id: String::new(),
            username: String::new(),
            global_name: String::new(),
            last_error: String::new(),
            media: None,
            party: None,
            pending_join_requests: Vec::new(),
            last_sent_activity: None,
            invite_active: false,
            outbound_tx: None,
            shutdown: false,
        }
    }
}

#[derive(Clone)]
pub struct DiscordManager {
    config: ConfigStore,
    inner: Arc<RwLock<DiscordInner>>,
    events: broadcast::Sender<DiscordEvent>,
    wakeup: Arc<Notify>,
}

impl DiscordManager {
    pub fn new(config: ConfigStore) -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            config,
            inner: Arc::new(RwLock::new(DiscordInner::default())),
            events,
            wakeup: Arc::new(Notify::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DiscordEvent> {
        self.events.subscribe()
    }

    pub async fn status(&self) -> DiscordStatusSnapshot {
        let cfg = self.config.snapshot();
        let guard = self.inner.read().await;
        build_status_snapshot(&cfg, &guard)
    }

    pub async fn update_media(&self, media: Option<DiscordMediaState>) {
        let normalized = media.filter(|m| m.active && !m.title.trim().is_empty());
        {
            let mut guard = self.inner.write().await;
            guard.media = normalized;
        }
        self.sync_activity_now().await;
    }

    pub async fn update_native_player(&self, state: &PlaybackState) {
        if !state.active || state.title.trim().is_empty() {
            return;
        }
        let media = DiscordMediaState {
            active: state.active,
            title: state.title.clone(),
            channel: "Playzi".to_string(),
            thumbnail: String::new(),
            url: String::new(),
            is_playing: !state.paused,
            is_audio: false,
            position_secs: state.position_secs.max(0.0),
            duration_secs: state.duration_secs.max(0.0),
        };
        self.update_media(Some(media)).await;
    }

    pub async fn update_party(&self, snapshot: PartyRoomSnapshot) {
        {
            let mut guard = self.inner.write().await;
            if snapshot.active {
                guard.party = Some(snapshot);
            } else {
                guard.party = None;
                guard.pending_join_requests.clear();
            }
        }
        self.sync_activity_now().await;
        self.emit_state().await;
    }

    pub async fn refresh_config(&self) {
        let cfg = self.config.snapshot();
        let should_disconnect = !cfg.discord_enabled || cfg.discord_client_id.trim().is_empty();
        if should_disconnect {
            let mut guard = self.inner.write().await;
            if let Some(tx) = guard.outbound_tx.as_ref() {
                let _ = tx.send(OutboundCmd::SetActivity(None));
                let _ = tx.send(OutboundCmd::Disconnect);
            }
            guard.invite_active = false;
            guard.last_sent_activity = None;
            guard.pending_join_requests.clear();
        } else {
            self.sync_activity_now().await;
        }
        self.wakeup.notify_one();
        self.emit_state().await;
    }

    pub async fn reconnect(&self) {
        {
            let mut guard = self.inner.write().await;
            guard.last_error.clear();
            if let Some(tx) = guard.outbound_tx.as_ref() {
                let _ = tx.send(OutboundCmd::Disconnect);
            }
        }
        self.wakeup.notify_one();
        self.emit_state().await;
    }

    pub async fn respond_join_request(
        &self,
        user_id: &str,
        accept: bool,
    ) -> Result<DiscordStatusSnapshot> {
        let uid = user_id.trim().to_string();
        if uid.is_empty() {
            return Err(CoreError::Validation(
                "Falta el ID de usuario de Discord.".to_string(),
            ));
        }

        let tx_opt = {
            let mut guard = self.inner.write().await;
            guard.pending_join_requests.retain(|r| r.user_id != uid);
            guard.outbound_tx.clone()
        };

        let Some(tx) = tx_opt else {
            self.emit_state().await;
            return Err(CoreError::External(
                "Discord no está conectado en este momento.".to_string(),
            ));
        };

        tx.send(OutboundCmd::RespondJoinRequest {
            user_id: uid,
            accept,
        })
        .map_err(|_| CoreError::External("No se pudo enviar la respuesta a Discord.".to_string()))?;

        let snap = self.status().await;
        let _ = self.events.send(DiscordEvent::State {
            snapshot: snap.clone(),
        });
        Ok(snap)
    }

    pub async fn shutdown(&self) {
        let mut guard = self.inner.write().await;
        guard.shutdown = true;
        if let Some(tx) = guard.outbound_tx.as_ref() {
            let _ = tx.send(OutboundCmd::SetActivity(None));
            let _ = tx.send(OutboundCmd::Disconnect);
        }
        self.wakeup.notify_waiters();
    }

    pub fn start_background(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            this.run_loop().await;
        });
    }

    async fn emit_state(&self) {
        let snapshot = self.status().await;
        let _ = self.events.send(DiscordEvent::State { snapshot });
    }

    fn emit_notice(&self, level: &str, message: impl Into<String>) {
        let _ = self.events.send(DiscordEvent::Notice {
            level: level.to_string(),
            message: message.into(),
        });
    }

    async fn sync_activity_now(&self) {
        let cfg = self.config.snapshot();
        let mut guard = self.inner.write().await;
        if !guard.connected {
            guard.invite_active = false;
            return;
        }
        let (activity, invite_active) = build_activity(
            &cfg,
            guard.media.as_ref(),
            guard.party.as_ref(),
            Utc::now().timestamp_millis(),
        );
        guard.invite_active = invite_active;

        let comparable = strip_timestamps(&activity);
        if guard.last_sent_activity == comparable && activity.is_some() {
            // Still update if timestamps shifted significantly or just let it pass on explicit events
        }
        guard.last_sent_activity = comparable;
        if let Some(tx) = guard.outbound_tx.as_ref() {
            let _ = tx.send(OutboundCmd::SetActivity(activity));
        }
    }

    async fn run_loop(&self) {
        loop {
            if self.inner.read().await.shutdown {
                break;
            }

            let cfg = self.config.snapshot();
            let client_id = cfg.discord_client_id.trim().to_string();
            if !cfg.discord_enabled || client_id.is_empty() {
                {
                    let mut guard = self.inner.write().await;
                    guard.connected = false;
                    guard.invite_active = false;
                    guard.outbound_tx = None;
                    guard.user_id.clear();
                    guard.username.clear();
                    guard.global_name.clear();
                    guard.last_error = if !cfg.discord_enabled {
                        String::new()
                    } else {
                        "Falta configurar Discord Client ID en Configuración.".to_string()
                    };
                }
                self.emit_state().await;
                tokio::select! {
                    _ = self.wakeup.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(10)) => {},
                }
                continue;
            }

            match connect_ipc_stream().await {
                Ok(stream) => {
                    if let Err(err) = self.run_connected_session(stream, &client_id).await {
                        let mut guard = self.inner.write().await;
                        guard.last_error = err;
                    }
                    {
                        let mut guard = self.inner.write().await;
                        guard.connected = false;
                        guard.invite_active = false;
                        guard.outbound_tx = None;
                        guard.last_sent_activity = None;
                        guard.user_id.clear();
                        guard.username.clear();
                        guard.global_name.clear();
                    }
                    self.emit_state().await;
                }
                Err(err) => {
                    let mut guard = self.inner.write().await;
                    guard.connected = false;
                    guard.invite_active = false;
                    guard.outbound_tx = None;
                    guard.last_error = err;
                    drop(guard);
                    self.emit_state().await;
                }
            }

            if self.inner.read().await.shutdown {
                break;
            }

            tokio::select! {
                _ = self.wakeup.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(6)) => {},
            }
        }
    }

    async fn run_connected_session(
        &self,
        stream: IpcStream,
        client_id: &str,
    ) -> std::result::Result<(), String> {
        let (mut reader, mut writer) = tokio::io::split(stream);

        // 1. Handshake
        let handshake = json!({
            "v": 1,
            "client_id": client_id,
        });
        write_ipc_frame(&mut writer, OP_HANDSHAKE, &handshake)
            .await
            .map_err(|e| format!("Handshake con Discord falló: {e}"))?;

        // 2. Wait for READY frame
        let (op, ready_val) = tokio::time::timeout(
            Duration::from_secs(5),
            read_ipc_frame(&mut reader),
        )
        .await
        .map_err(|_| "Tiempo agotado esperando respuesta READY de Discord.".to_string())?
        .map_err(|e| format!("Error leyendo READY de Discord: {e}"))?;

        if op == OP_CLOSE {
            let msg = ready_val
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Discord cerró la conexión (verifica el Client ID).");
            return Err(msg.to_string());
        }

        let user = ready_val.pointer("/data/user");
        let user_id = user
            .and_then(|u| u.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let username = user
            .and_then(|u| u.get("username"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let global_name = user
            .and_then(|u| u.get("global_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // 3. Subscribe to invite & join request events
        for evt in ["ACTIVITY_JOIN", "ACTIVITY_JOIN_REQUEST", "ACTIVITY_INVITE"] {
            let sub = json!({
                "cmd": "SUBSCRIBE",
                "evt": evt,
                "args": {},
                "nonce": Uuid::new_v4().to_string(),
            });
            write_ipc_frame(&mut writer, OP_FRAME, &sub)
                .await
                .map_err(|e| format!("No se pudo suscribir a {evt}: {e}"))?;
        }

        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<OutboundCmd>();
        {
            let mut guard = self.inner.write().await;
            guard.connected = true;
            guard.user_id = user_id;
            guard.username = username;
            guard.global_name = global_name;
            guard.last_error.clear();
            guard.outbound_tx = Some(outbound_tx);
            guard.last_sent_activity = None;
        }
        self.sync_activity_now().await;
        self.emit_state().await;

        let pid = std::process::id();

        loop {
            tokio::select! {
                cmd = outbound_rx.recv() => {
                    let Some(cmd) = cmd else {
                        break;
                    };
                    match cmd {
                        OutboundCmd::SetActivity(activity) => {
                            let payload = json!({
                                "cmd": "SET_ACTIVITY",
                                "args": {
                                    "pid": pid,
                                    "activity": activity,
                                },
                                "nonce": Uuid::new_v4().to_string(),
                            });
                            write_ipc_frame(&mut writer, OP_FRAME, &payload)
                                .await
                                .map_err(|e| format!("Error enviando SET_ACTIVITY: {e}"))?;
                        }
                        OutboundCmd::RespondJoinRequest { user_id, accept } => {
                            let cmd_name = if accept {
                                "SEND_ACTIVITY_JOIN_INVITE"
                            } else {
                                "CLOSE_ACTIVITY_REQUEST"
                            };
                            let payload = json!({
                                "cmd": cmd_name,
                                "args": {
                                    "user_id": user_id,
                                },
                                "nonce": Uuid::new_v4().to_string(),
                            });
                            write_ipc_frame(&mut writer, OP_FRAME, &payload)
                                .await
                                .map_err(|e| format!("Error enviando {cmd_name}: {e}"))?;
                        }
                        OutboundCmd::Disconnect => {
                            break;
                        }
                    }
                }
                frame = read_ipc_frame(&mut reader) => {
                    let (op, val) = frame.map_err(|e| format!("Conexión con Discord cerrada: {e}"))?;
                    match op {
                        OP_PING => {
                            write_ipc_frame(&mut writer, OP_PONG, &val)
                                .await
                                .map_err(|e| format!("Error respondiendo PONG: {e}"))?;
                        }
                        OP_CLOSE => {
                            let msg = val
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("Discord cerró la sesión IPC.");
                            return Err(msg.to_string());
                        }
                        OP_FRAME => {
                            self.handle_incoming_frame(&val).await;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_incoming_frame(&self, val: &Value) {
        let evt = val.get("evt").and_then(Value::as_str).unwrap_or("");
        match evt {
            "ACTIVITY_JOIN" => {
                let secret = val
                    .pointer("/data/secret")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some((room_code, invite_url)) = decode_join_secret(secret) {
                    self.emit_notice(
                        "ok",
                        format!("Uniéndote a la sala {room_code} desde Discord..."),
                    );
                    let _ = self.events.send(DiscordEvent::JoinRoom {
                        room_code,
                        invite_url,
                    });
                } else {
                    self.emit_notice(
                        "error",
                        "No se pudo leer la invitación recibida desde Discord.",
                    );
                }
            }
            "ACTIVITY_JOIN_REQUEST" => {
                let Some(user) = val.pointer("/data/user") else {
                    return;
                };
                let user_id = user
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if user_id.is_empty() {
                    return;
                }
                let username = user
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or("Usuario")
                    .to_string();
                let global_name = user
                    .get("global_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let avatar = user
                    .get("avatar")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let display = if !global_name.trim().is_empty() {
                    global_name.clone()
                } else {
                    username.clone()
                };

                {
                    let mut guard = self.inner.write().await;
                    guard.pending_join_requests.retain(|r| r.user_id != user_id);
                    guard.pending_join_requests.push(DiscordJoinRequest {
                        user_id,
                        username,
                        global_name,
                        avatar,
                        requested_at: Utc::now().to_rfc3339(),
                    });
                    if guard.pending_join_requests.len() > 20 {
                        let excess = guard.pending_join_requests.len() - 20;
                        guard.pending_join_requests.drain(0..excess);
                    }
                }
                self.emit_notice(
                    "ok",
                    format!("{display} pidió unirse a tu sala desde Discord."),
                );
                self.emit_state().await;
            }
            "ACTIVITY_INVITE" => {
                let inviter = val
                    .pointer("/data/user/global_name")
                    .or_else(|| val.pointer("/data/user/username"))
                    .and_then(Value::as_str)
                    .unwrap_or("Un amigo");
                self.emit_notice(
                    "ok",
                    format!("{inviter} te envió una invitación de Playzi en Discord."),
                );
            }
            "ERROR" => {
                let msg = val
                    .pointer("/data/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Error de Discord RPC");
                {
                    let mut guard = self.inner.write().await;
                    guard.last_error = msg.to_string();
                }
                self.emit_state().await;
            }
            _ => {}
        }
    }
}

fn build_status_snapshot(
    cfg: &crate::config::AppConfig,
    inner: &DiscordInner,
) -> DiscordStatusSnapshot {
    DiscordStatusSnapshot {
        enabled: cfg.discord_enabled,
        configured: !cfg.discord_client_id.trim().is_empty(),
        connected: inner.connected,
        show_media: cfg.discord_show_media,
        show_party: cfg.discord_show_party,
        invite_active: inner.connected && inner.invite_active,
        user_id: inner.user_id.clone(),
        username: inner.username.clone(),
        global_name: inner.global_name.clone(),
        last_error: inner.last_error.clone(),
        pending_join_requests: inner.pending_join_requests.clone(),
    }
}

fn strip_timestamps(activity: &Option<Value>) -> Option<Value> {
    let mut cloned = activity.clone()?;
    if let Some(obj) = cloned.as_object_mut() {
        obj.remove("timestamps");
    }
    Some(cloned)
}

fn clamp_discord_text(input: &str, max_chars: usize) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if count >= max_chars {
            break;
        }
        out.push(ch);
        count += 1;
    }
    if out.chars().count() == 1 {
        out.push(' ');
    }
    out
}

pub fn encode_join_secret(room_code: &str, invite_url: &str) -> Option<String> {
    let code = room_code.trim().to_uppercase();
    let url = invite_url.trim();
    if code.is_empty() || url.is_empty() {
        return None;
    }

    let compact_url = if let Some(rest) = url.strip_prefix("wss://") {
        format!("s|{rest}")
    } else if let Some(rest) = url.strip_prefix("ws://") {
        format!("w|{rest}")
    } else if let Some(rest) = url.strip_prefix("https://") {
        format!("s|{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("w|{rest}")
    } else {
        format!("r|{url}")
    };

    let candidate = format!("{JOIN_SECRET_PREFIX}{code}|{compact_url}");
    if candidate.len() <= MAX_JOIN_SECRET_BYTES {
        return Some(candidate);
    }

    // Fallback: strip query params (since room_code is already encoded in the prefix)
    if let Some((base, _)) = compact_url.split_once('?') {
        let shorter = format!("{JOIN_SECRET_PREFIX}{code}|{base}");
        if shorter.len() <= MAX_JOIN_SECRET_BYTES {
            return Some(shorter);
        }
    }
    None
}

pub fn decode_join_secret(secret: &str) -> Option<(String, String)> {
    let rest = secret.trim().strip_prefix(JOIN_SECRET_PREFIX)?;
    let mut parts = rest.splitn(3, '|');
    let room_code = parts.next()?.trim().to_uppercase();
    let scheme_tag = parts.next()?.trim();
    let host_part = parts.next()?.trim();
    if room_code.is_empty() || host_part.is_empty() {
        return None;
    }
    let invite_url = match scheme_tag {
        "s" => format!("wss://{host_part}"),
        "w" => format!("ws://{host_part}"),
        "r" => host_part.to_string(),
        _ => return None,
    };
    Some((room_code, invite_url))
}

fn build_activity(
    cfg: &crate::config::AppConfig,
    media: Option<&DiscordMediaState>,
    party: Option<&PartyRoomSnapshot>,
    now_ms: i64,
) -> (Option<Value>, bool) {
    if !cfg.discord_enabled {
        return (None, false);
    }

    let active_party = party.filter(|p| p.active && cfg.discord_show_party);
    if let Some(p) = active_party {
        let participants_count = p.participants.len().max(1);
        let friends_count = participants_count.saturating_sub(1);
        let max_size = participants_count.max(16);

        let party_item = p
            .playback
            .current_index
            .and_then(|idx| p.queue.get(idx));

        let current_title = media
            .filter(|m| m.active && !m.title.trim().is_empty())
            .map(|m| m.title.as_str())
            .or_else(|| party_item.map(|q| q.title.as_str()))
            .unwrap_or("");
        let current_thumb = media
            .filter(|m| m.active && !m.thumbnail.trim().is_empty())
            .map(|m| m.thumbnail.as_str())
            .or_else(|| party_item.map(|q| q.thumbnail.as_str()))
            .unwrap_or("");

        let (details, state_text) = if cfg.discord_show_media && !current_title.trim().is_empty() {
            let state_line = match friends_count {
                0 => "En sala · Esperando amigos".to_string(),
                1 => "En sala con 1 amigo".to_string(),
                n => format!("En sala con {n} amigos"),
            };
            (clamp_discord_text(current_title, 128), state_line)
        } else {
            let state_line = match friends_count {
                0 => "Esperando amigos".to_string(),
                1 => "Con 1 amigo".to_string(),
                n => format!("Con {n} amigos"),
            };
            ("En una sala de Playzi".to_string(), state_line)
        };

        let invite_source = if !p.public_ws_url.trim().is_empty() {
            p.public_ws_url.as_str()
        } else if !p.invite_url.trim().is_empty() {
            p.invite_url.as_str()
        } else {
            p.local_ws_url.as_str()
        };
        let join_secret = encode_join_secret(&p.room_code, invite_source);
        let invite_active = join_secret.is_some();

        let mut act = json!({
            "type": 0,
            "details": details,
            "state": state_text,
            "party": {
                "id": format!("playzi-room-{}", p.room_code),
                "size": [participants_count, max_size],
            },
            "instance": true,
        });

        if let Some(secret) = join_secret {
            act["secrets"] = json!({ "join": secret });
        }

        if cfg.discord_show_media {
            if let Some(m) = media.filter(|m| m.active && m.is_playing && m.duration_secs > 1.0) {
                let start_ms = now_ms - (m.position_secs.max(0.0) * 1000.0) as i64;
                let end_ms = start_ms + (m.duration_secs * 1000.0) as i64;
                act["timestamps"] = json!({
                    "start": start_ms,
                    "end": end_ms,
                });
            }
            if current_thumb.starts_with("https://") || current_thumb.starts_with("http://") {
                act["assets"] = json!({
                    "large_image": current_thumb,
                    "large_text": clamp_discord_text(
                        if current_title.is_empty() { "Playzi" } else { current_title },
                        128
                    ),
                });
            }
        }

        return (Some(act), invite_active);
    }

    if cfg.discord_show_media
        && let Some(m) = media.filter(|m| m.active && !m.title.trim().is_empty())
    {
        let activity_type = if m.is_audio { 2 } else { 3 };
        let details = clamp_discord_text(&m.title, 128);
        let base_channel = if m.channel.trim().is_empty() {
            "Playzi"
        } else {
            m.channel.trim()
        };
        let state_text = if m.is_playing {
            clamp_discord_text(base_channel, 128)
        } else {
            clamp_discord_text(&format!("{base_channel} · Pausado"), 128)
        };

        let mut act = json!({
            "type": activity_type,
            "details": details,
            "state": state_text,
            "instance": false,
        });

        if m.is_playing && m.duration_secs > 1.0 {
            let start_ms = now_ms - (m.position_secs.max(0.0) * 1000.0) as i64;
            let end_ms = start_ms + (m.duration_secs * 1000.0) as i64;
            act["timestamps"] = json!({
                "start": start_ms,
                "end": end_ms,
            });
        }

        if m.thumbnail.starts_with("https://") || m.thumbnail.starts_with("http://") {
            act["assets"] = json!({
                "large_image": m.thumbnail,
                "large_text": clamp_discord_text(&m.title, 128),
            });
        }

        return (Some(act), false);
    }

    (None, false)
}

async fn write_ipc_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    opcode: u32,
    payload: &Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&opcode.to_le_bytes());
    header[4..8].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    writer.write_all(&header).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_ipc_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).await?;
    let opcode = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if len > MAX_IPC_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Discord IPC frame too large: {len} bytes"),
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    let val: Value = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((opcode, val))
}

#[cfg(windows)]
type IpcStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
type IpcStream = tokio::net::UnixStream;

#[cfg(windows)]
async fn connect_ipc_stream() -> std::result::Result<IpcStream, String> {
    use tokio::net::windows::named_pipe::ClientOptions;

    for idx in 0..10 {
        let pipe_name = format!(r"\\.\pipe\discord-ipc-{idx}");
        match ClientOptions::new().open(&pipe_name) {
            Ok(client) => return Ok(client),
            Err(_) => continue,
        }
    }
    Err("No se encontró Discord abierto en este equipo.".to_string())
}

#[cfg(unix)]
async fn connect_ipc_stream() -> std::result::Result<IpcStream, String> {
    let mut dirs = Vec::new();
    for var in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            dirs.push(v);
        }
    }
    dirs.push("/tmp".to_string());

    for dir in dirs {
        let base = dir.trim_end_matches('/');
        let prefixes = [
            base.to_string(),
            format!("{base}/app/com.discordapp.Discord"),
            format!("{base}/snap.discord"),
        ];
        for prefix in prefixes {
            for idx in 0..10 {
                let path = format!("{prefix}/discord-ipc-{idx}");
                if let Ok(stream) = tokio::net::UnixStream::connect(&path).await {
                    return Ok(stream);
                }
            }
        }
    }
    Err("No se encontró Discord abierto en este equipo.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::party::{PartyParticipant, PartyRole};
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> AppConfig {
        AppConfig::load_from_root(dir.path())
    }

    #[test]
    fn join_secret_roundtrip_short_and_long_urls() {
        let s1 = encode_join_secret("ab12cd", "wss://playzi-hub.example.workers.dev/relay/AB12CD")
            .expect("should encode wss url");
        assert!(s1.len() <= MAX_JOIN_SECRET_BYTES);
        assert_eq!(
            decode_join_secret(&s1),
            Some((
                "AB12CD".to_string(),
                "wss://playzi-hub.example.workers.dev/relay/AB12CD".to_string()
            ))
        );

        let s2 = encode_join_secret("XYZ999", "ws://192.168.1.50:8766/ws?room=XYZ999")
            .expect("should encode lan url");
        assert_eq!(
            decode_join_secret(&s2),
            Some((
                "XYZ999".to_string(),
                "ws://192.168.1.50:8766/ws?room=XYZ999".to_string()
            ))
        );
    }

    #[test]
    fn build_activity_for_solo_video_and_music() {
        let dir = TempDir::new().unwrap();
        let mut cfg = test_config(&dir);
        cfg.discord_enabled = true;
        cfg.discord_show_media = true;

        let video = DiscordMediaState {
            active: true,
            title: "Rust Async Deep Dive".to_string(),
            channel: "Systems Channel".to_string(),
            thumbnail: "https://i.ytimg.com/vi/abc/hqdefault.jpg".to_string(),
            url: "https://youtube.com/watch?v=abc".to_string(),
            is_playing: true,
            is_audio: false,
            position_secs: 30.0,
            duration_secs: 300.0,
        };

        let (act, invite_active) = build_activity(&cfg, Some(&video), None, 1_700_000_000_000);
        assert!(!invite_active);
        let act = act.expect("should produce activity");
        assert_eq!(act["type"], 3);
        assert_eq!(act["details"], "Rust Async Deep Dive");
        assert_eq!(act["state"], "Systems Channel");
        assert_eq!(
            act["assets"]["large_image"],
            "https://i.ytimg.com/vi/abc/hqdefault.jpg"
        );
        assert!(act["timestamps"]["end"].as_i64().unwrap() > act["timestamps"]["start"].as_i64().unwrap());

        let mut music = video.clone();
        music.is_audio = true;
        music.is_playing = false;
        let (act_m, _) = build_activity(&cfg, Some(&music), None, 1_700_000_000_000);
        let act_m = act_m.expect("should produce music activity");
        assert_eq!(act_m["type"], 2);
        assert_eq!(act_m["state"], "Systems Channel · Pausado");
        assert!(act_m.get("timestamps").is_none());
    }

    #[test]
    fn build_activity_for_party_with_and_without_media() {
        let dir = TempDir::new().unwrap();
        let mut cfg = test_config(&dir);
        cfg.discord_enabled = true;
        cfg.discord_show_party = true;
        cfg.discord_show_media = true;

        let party = PartyRoomSnapshot {
            active: true,
            role: PartyRole::Host,
            room_code: "RM42XY".to_string(),
            public_ws_url: "wss://playzi-hub.example.workers.dev/relay/RM42XY".to_string(),
            participants: vec![
                PartyParticipant {
                    user_id: "u1".to_string(),
                    display_name: "Host".to_string(),
                    is_host: true,
                    ..Default::default()
                },
                PartyParticipant {
                    user_id: "u2".to_string(),
                    display_name: "Ana".to_string(),
                    ..Default::default()
                },
                PartyParticipant {
                    user_id: "u3".to_string(),
                    display_name: "Luis".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        let media = DiscordMediaState {
            active: true,
            title: "Concert Live".to_string(),
            channel: "Band".to_string(),
            is_playing: true,
            duration_secs: 600.0,
            ..Default::default()
        };

        let (act, invite_active) =
            build_activity(&cfg, Some(&media), Some(&party), 1_700_000_000_000);
        assert!(invite_active);
        let act = act.expect("should produce party activity");
        assert_eq!(act["type"], 0);
        assert_eq!(act["details"], "Concert Live");
        assert_eq!(act["state"], "En sala con 2 amigos");
        assert_eq!(act["party"]["id"], "playzi-room-RM42XY");
        assert_eq!(act["party"]["size"], json!([3, 16]));
        assert!(act["secrets"]["join"].as_str().unwrap().starts_with("pz1|RM42XY|"));

        // When media sharing is disabled, still show the room and friends count
        cfg.discord_show_media = false;
        let (act_hidden, invite_hidden) =
            build_activity(&cfg, Some(&media), Some(&party), 1_700_000_000_000);
        assert!(invite_hidden);
        let act_hidden = act_hidden.expect("should produce party activity without media");
        assert_eq!(act_hidden["details"], "En una sala de Playzi");
        assert_eq!(act_hidden["state"], "Con 2 amigos");
    }

    #[test]
    fn build_activity_respects_disabled_flags() {
        let dir = TempDir::new().unwrap();
        let mut cfg = test_config(&dir);
        cfg.discord_enabled = false;
        let media = DiscordMediaState {
            active: true,
            title: "Hidden Video".to_string(),
            ..Default::default()
        };
        let (act, invite) = build_activity(&cfg, Some(&media), None, 1_700_000_000_000);
        assert!(act.is_none());
        assert!(!invite);
    }
}
