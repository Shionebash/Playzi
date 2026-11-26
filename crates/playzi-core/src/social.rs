use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{RwLock, broadcast, mpsc, watch};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

use crate::config::ConfigStore;
use crate::error::{CoreError, Result};
use crate::party::{ensure_user_profile, list_friends, remove_friend as remove_local_friend};
use crate::state::{FriendRecord, StateStore};

// ── Public Data Types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PublishedRoomInfo {
    pub room_code: String,
    pub invite_url: String,
    pub room_access: String, // "request" | "open" | "invite_only"
    pub participants_count: usize,
    pub current_title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct FriendRoomInfo {
    pub room_code: String,
    pub invite_url: String,
    pub room_access: String,
    pub participants_count: usize,
    pub current_title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteFriendState {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
    pub online: bool,
    pub room: Option<FriendRoomInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct SocialFriendView {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
    pub default_allow_queue: bool,
    pub online: bool,
    pub mutual: bool,
    pub room: Option<FriendRoomInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct FriendRequestItem {
    pub id: String,
    pub from_user_id: String,
    pub from_display_name: String,
    pub from_google_account_name: String,
    pub to_user_id: String,
    pub to_display_name: String,
    pub to_google_account_name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RoomInviteItem {
    pub id: String,
    pub from_user_id: String,
    pub from_display_name: String,
    pub from_google_account_name: String,
    pub room_code: String,
    pub invite_url: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RoomJoinRequestItem {
    pub id: String,
    pub from_user_id: String,
    pub from_display_name: String,
    pub from_google_account_name: String,
    pub to_user_id: String,
    pub room_code: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocialSnapshot {
    pub configured: bool,
    pub connected: bool,
    pub hub_url: String,
    pub last_error: String,
    pub friends: Vec<SocialFriendView>,
    pub incoming_friend_requests: Vec<FriendRequestItem>,
    pub outgoing_friend_requests: Vec<FriendRequestItem>,
    pub incoming_room_invites: Vec<RoomInviteItem>,
    pub incoming_join_requests: Vec<RoomJoinRequestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SocialEvent {
    State {
        snapshot: SocialSnapshot,
    },
    Notice {
        level: String,
        message: String,
    },
    JoinApproved {
        host_user_id: String,
        host_display_name: String,
        room_code: String,
        invite_url: String,
    },
}

// ── URL Helpers ─────────────────────────────────────────────────────────────

pub fn normalize_hub_ws_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.starts_with("wss://") || trimmed.starts_with("ws://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("wss://{trimmed}")
    };

    let mut parsed = Url::parse(&with_scheme).ok()?;
    let clean_path = {
        let path = parsed.path().trim_end_matches('/');
        path.strip_suffix("/social/ws")
            .or_else(|| path.strip_suffix("/ws"))
            .unwrap_or(path)
            .to_string()
    };
    parsed.set_path(&clean_path);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string().trim_end_matches('/').to_string())
}

pub fn normalize_hub_http_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let with_scheme = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        format!("https://{trimmed}")
    };

    let mut parsed = Url::parse(&with_scheme).ok()?;
    let clean_path = {
        let path = parsed.path().trim_end_matches('/');
        path.strip_suffix("/social/ws")
            .or_else(|| path.strip_suffix("/api/social"))
            .or_else(|| path.strip_suffix("/api/room"))
            .or_else(|| path.strip_suffix("/api/health"))
            .or_else(|| path.strip_suffix("/ws"))
            .unwrap_or(path)
            .to_string()
    };
    parsed.set_path(&clean_path);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string().trim_end_matches('/').to_string())
}

/// Detects whether a configured Hub URL uses the Vercel HTTP serverless transport (`/api/health` -> `transport: "http"`).
pub async fn detect_http_hub(http: &reqwest::Client, raw: &str) -> Option<String> {
    let base = normalize_hub_http_base(raw)?;
    let lower = raw.to_ascii_lowercase();
    if lower.contains(".vercel.app") || lower.contains("/api/room") || lower.contains("/api/social") {
        return Some(base);
    }
    let health_url = format!("{base}/api/health");
    let resp = tokio::time::timeout(Duration::from_secs(4), http.get(&health_url).send())
        .await
        .ok()?
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body = resp.json::<Value>().await.ok()?;
    if body.get("transport").and_then(Value::as_str) == Some("http") {
        Some(base)
    } else {
        None
    }
}

pub fn build_relay_ws_url(hub_url: &str, room_code: &str, role: Option<&str>) -> Option<String> {
    let base = normalize_hub_ws_base(hub_url)?;
    let mut parsed = Url::parse(&format!("{base}/ws")).ok()?;
    {
        let mut qp = parsed.query_pairs_mut();
        qp.append_pair("room", &room_code.trim().to_uppercase());
        if let Some(r) = role {
            qp.append_pair("role", r);
        }
    }
    Some(parsed.to_string())
}

// ── Manager ─────────────────────────────────────────────────────────────────

#[derive(Default)]
struct SocialRuntimeState {
    connected: bool,
    hub_url: String,
    last_error: String,
    remote_friends: Vec<RemoteFriendState>,
    incoming_friend_requests: Vec<FriendRequestItem>,
    outgoing_friend_requests: Vec<FriendRequestItem>,
    incoming_room_invites: Vec<RoomInviteItem>,
    incoming_join_requests: Vec<RoomJoinRequestItem>,
    published_room: Option<PublishedRoomInfo>,
    outbound_tx: Option<mpsc::UnboundedSender<Value>>,
}

struct SocialManagerInner {
    state_store: StateStore,
    config: ConfigStore,
    runtime: RwLock<SocialRuntimeState>,
    events_tx: broadcast::Sender<SocialEvent>,
    reconnect_tx: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct SocialManager {
    inner: Arc<SocialManagerInner>,
}

impl SocialManager {
    pub fn new(state_store: StateStore, config: ConfigStore) -> Self {
        let (events_tx, _) = broadcast::channel(64);
        let (reconnect_tx, _) = watch::channel(0u64);
        let hub_url = config.snapshot().social_hub_url;
        Self {
            inner: Arc::new(SocialManagerInner {
                state_store,
                config,
                runtime: RwLock::new(SocialRuntimeState {
                    hub_url,
                    ..Default::default()
                }),
                events_tx,
                reconnect_tx,
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SocialEvent> {
        self.inner.events_tx.subscribe()
    }

    fn emit_notice(&self, level: &str, message: impl Into<String>) {
        let _ = self.inner.events_tx.send(SocialEvent::Notice {
            level: level.to_string(),
            message: message.into(),
        });
    }

    async fn emit_snapshot(&self) {
        let snap = self.status().await;
        let _ = self.inner.events_tx.send(SocialEvent::State { snapshot: snap });
    }

    pub fn reconnect(&self) {
        let next = self.inner.reconnect_tx.borrow().wrapping_add(1);
        let _ = self.inner.reconnect_tx.send(next);
    }

    pub async fn status(&self) -> SocialSnapshot {
        let cfg_hub = self.inner.config.snapshot().social_hub_url;
        let local_friends = list_friends(&self.inner.state_store).unwrap_or_default();
        let local_map: HashMap<String, FriendRecord> = local_friends
            .iter()
            .map(|f| (f.user_id.to_uppercase(), f.clone()))
            .collect();

        let rt = self.inner.runtime.read().await;
        let mut friends_out = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for rf in &rt.remote_friends {
            let key = rf.user_id.to_uppercase();
            seen.insert(key.clone());
            let local = local_map.get(&key);
            friends_out.push(SocialFriendView {
                user_id: rf.user_id.clone(),
                display_name: rf.display_name.clone(),
                google_account_name: rf.google_account_name.clone(),
                default_allow_queue: local.map(|l| l.default_allow_queue).unwrap_or(true),
                online: rf.online,
                mutual: true,
                room: rf.room.clone(),
            });
        }

        for lf in local_friends {
            let key = lf.user_id.to_uppercase();
            if seen.contains(&key) {
                continue;
            }
            friends_out.push(SocialFriendView {
                user_id: lf.user_id,
                display_name: lf.display_name,
                google_account_name: lf.google_account_name,
                default_allow_queue: lf.default_allow_queue,
                online: false,
                mutual: false,
                room: None,
            });
        }

        // Sort: friends with open room first, then online, then alphabetical
        friends_out.sort_by(|a, b| {
            b.room
                .is_some()
                .cmp(&a.room.is_some())
                .then_with(|| b.online.cmp(&a.online))
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
        });

        SocialSnapshot {
            configured: !cfg_hub.trim().is_empty(),
            connected: rt.connected,
            hub_url: cfg_hub,
            last_error: rt.last_error.clone(),
            friends: friends_out,
            incoming_friend_requests: rt.incoming_friend_requests.clone(),
            outgoing_friend_requests: rt.outgoing_friend_requests.clone(),
            incoming_room_invites: rt.incoming_room_invites.clone(),
            incoming_join_requests: rt.incoming_join_requests.clone(),
        }
    }

    async fn send_hub_message(&self, msg: Value) -> Result<()> {
        let rt = self.inner.runtime.read().await;
        if let Some(tx) = &rt.outbound_tx {
            tx.send(msg).map_err(|_| {
                CoreError::External("No conectado al servidor Hub Social.".to_string())
            })?;
            Ok(())
        } else {
            Err(CoreError::Validation(
                "El servidor Hub Social no está conectado. Configura la URL del Hub primero."
                    .to_string(),
            ))
        }
    }

    pub async fn send_profile_update(&self, display_name: &str, google_account_name: &str) {
        let _ = self
            .send_hub_message(json!({
                "type": "profile_update",
                "displayName": display_name,
                "googleAccountName": google_account_name,
            }))
            .await;
    }

    pub async fn send_friend_request(&self, to_user_id: &str) -> Result<SocialSnapshot> {
        let clean = to_user_id.trim().to_uppercase();
        if clean.is_empty() {
            return Err(CoreError::Validation(
                "Ingresa el ID de Playzi del amigo.".to_string(),
            ));
        }
        self.send_hub_message(json!({
            "type": "friend_request_send",
            "toUserId": clean,
        }))
        .await?;
        Ok(self.status().await)
    }

    pub async fn respond_friend_request(
        &self,
        request_id: &str,
        accept: bool,
    ) -> Result<SocialSnapshot> {
        self.send_hub_message(json!({
            "type": "friend_request_respond",
            "requestId": request_id.trim(),
            "accept": accept,
        }))
        .await?;
        Ok(self.status().await)
    }

    pub async fn cancel_friend_request(&self, request_id: &str) -> Result<SocialSnapshot> {
        self.send_hub_message(json!({
            "type": "friend_request_cancel",
            "requestId": request_id.trim(),
        }))
        .await?;
        Ok(self.status().await)
    }

    pub async fn remove_friend(&self, friend_user_id: &str) -> Result<SocialSnapshot> {
        let _ = remove_local_friend(&self.inner.state_store, friend_user_id);
        let _ = self
            .send_hub_message(json!({
                "type": "friend_remove",
                "friendUserId": friend_user_id.trim().to_uppercase(),
            }))
            .await;
        self.emit_snapshot().await;
        Ok(self.status().await)
    }

    pub async fn publish_room_presence(&self, room: Option<PublishedRoomInfo>) {
        let cleared = room.is_none();
        let outbound = {
            let mut rt = self.inner.runtime.write().await;
            if rt.published_room == room {
                return;
            }
            rt.published_room = room.clone();
            if cleared {
                rt.incoming_join_requests.clear();
            }
            rt.outbound_tx.clone()
        };
        if let Some(tx) = outbound {
            let _ = tx.send(json!({
                "type": "room_presence_update",
                "room": room,
            }));
        }
        if cleared {
            self.emit_snapshot().await;
        }
    }

    pub async fn force_clear_room_presence(&self) {
        let outbound = {
            let mut rt = self.inner.runtime.write().await;
            rt.published_room = None;
            rt.incoming_join_requests.clear();
            rt.outbound_tx.clone()
        };
        if let Some(tx) = outbound {
            let _ = tx.send(json!({
                "type": "room_presence_update",
                "room": Value::Null,
            }));
        }
        self.emit_snapshot().await;
    }

    pub async fn request_sync(&self) {
        let outbound = {
            let rt = self.inner.runtime.read().await;
            rt.outbound_tx.clone()
        };
        if let Some(tx) = outbound {
            let _ = tx.send(json!({ "type": "sync" }));
        }
    }

    pub async fn shutdown_notify(&self) {
        let outbound = {
            let mut rt = self.inner.runtime.write().await;
            rt.published_room = None;
            rt.incoming_join_requests.clear();
            rt.outbound_tx.clone()
        };

        let raw_hub = self.inner.config.snapshot().social_hub_url;
        if let Some(http_base) = normalize_hub_http_base(&raw_hub)
            && let Ok(profile) = ensure_user_profile(&self.inner.state_store)
        {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_millis(1800))
                .build()
                .unwrap_or_default();
            if let Some(detected) = detect_http_hub(&client, &raw_hub).await {
                let endpoint = format!("{detected}/api/social");
                let _ = client
                    .post(&endpoint)
                    .json(&json!({
                        "userId": profile.user_id,
                        "displayName": profile.display_name,
                        "googleAccountName": profile.google_account_name,
                        "action": "disconnect",
                    }))
                    .send()
                    .await;
                return;
            }
            let _ = http_base;
        }

        if let Some(tx) = outbound {
            let _ = tx.send(json!({
                "type": "room_presence_update",
                "room": Value::Null,
            }));
            let _ = tx.send(json!({
                "type": "client_disconnect",
            }));
            tokio::time::sleep(Duration::from_millis(180)).await;
        }
    }

    pub async fn invite_friend_to_room(
        &self,
        to_user_id: &str,
        room_code: &str,
        invite_url: &str,
    ) -> Result<()> {
        if invite_url.trim().is_empty() {
            return Err(CoreError::Validation(
                "Tu sala aún no tiene un enlace público listo para invitar.".to_string(),
            ));
        }
        self.send_hub_message(json!({
            "type": "room_invite_send",
            "toUserId": to_user_id.trim().to_uppercase(),
            "roomCode": room_code.trim().to_uppercase(),
            "inviteUrl": invite_url.trim(),
        }))
        .await
    }

    pub async fn request_join_friend_room(
        &self,
        to_user_id: &str,
        room_code: Option<&str>,
    ) -> Result<()> {
        self.send_hub_message(json!({
            "type": "room_join_request",
            "toUserId": to_user_id.trim().to_uppercase(),
            "roomCode": room_code.unwrap_or("").trim().to_uppercase(),
        }))
        .await
    }

    pub async fn respond_join_request(
        &self,
        request_id: &str,
        accept: bool,
        invite_url: Option<&str>,
    ) -> Result<SocialSnapshot> {
        {
            let mut rt = self.inner.runtime.write().await;
            rt.incoming_join_requests.retain(|r| r.id != request_id);
        }
        self.send_hub_message(json!({
            "type": "room_join_respond",
            "requestId": request_id.trim(),
            "accept": accept,
            "inviteUrl": invite_url.unwrap_or(""),
        }))
        .await?;
        self.emit_snapshot().await;
        Ok(self.status().await)
    }

    pub async fn dismiss_room_invite(&self, invite_id: &str) -> SocialSnapshot {
        let clean_id = invite_id.trim().to_string();
        {
            let mut rt = self.inner.runtime.write().await;
            rt.incoming_room_invites.retain(|i| i.id != clean_id);
        }
        if !clean_id.is_empty() {
            let _ = self
                .send_hub_message(json!({
                    "type": "room_invite_dismiss",
                    "inviteId": clean_id,
                }))
                .await;
        }
        self.emit_snapshot().await;
        self.status().await
    }

    pub fn start_background(&self) {
        let manager = self.clone();
        let mut reconnect_rx = self.inner.reconnect_tx.subscribe();

        tokio::spawn(async move {
            let mut http_backoff_secs = 6u64;
            loop {
                let raw_hub = manager.inner.config.snapshot().social_hub_url;
                let Some(ws_base) = normalize_hub_ws_base(&raw_hub) else {
                    {
                        let mut rt = manager.inner.runtime.write().await;
                        rt.connected = false;
                        rt.hub_url = raw_hub;
                        rt.last_error.clear();
                        rt.outbound_tx = None;
                        rt.remote_friends.clear();
                        rt.incoming_friend_requests.clear();
                        rt.outgoing_friend_requests.clear();
                        rt.incoming_join_requests.clear();
                    }
                    manager.emit_snapshot().await;
                    if reconnect_rx.changed().await.is_err() {
                        break;
                    }
                    continue;
                };

                let Ok(profile) = ensure_user_profile(&manager.inner.state_store) else {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                };

                let http_client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(12))
                    .build()
                    .unwrap_or_default();

                if let Some(http_base) = detect_http_hub(&http_client, &raw_hub).await {
                    let social_endpoint = format!("{http_base}/api/social");
                    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();

                    let initial_res = http_client
                        .post(&social_endpoint)
                        .json(&json!({
                            "userId": profile.user_id,
                            "displayName": profile.display_name,
                            "googleAccountName": profile.google_account_name,
                            "action": "sync",
                        }))
                        .send()
                        .await;

                    match initial_res {
                        Ok(resp) if resp.status().is_success() => {
                            http_backoff_secs = 6;
                            let body = resp.json::<Value>().await.ok();
                            let published_room = {
                                let mut rt = manager.inner.runtime.write().await;
                                rt.connected = true;
                                rt.hub_url = raw_hub.clone();
                                rt.last_error.clear();
                                rt.outbound_tx = Some(outbound_tx.clone());
                                rt.published_room.clone()
                            };
                            if let Some(val) = body {
                                manager.apply_http_social_response(&val).await;
                            } else {
                                manager.emit_snapshot().await;
                            }
                            let _ = outbound_tx.send(json!({
                                "type": "room_presence_update",
                                "room": published_room,
                            }));
                        }
                        Ok(resp) => {
                            let status = resp.status();
                            let err_detail = resp
                                .json::<Value>()
                                .await
                                .ok()
                                .and_then(|v| {
                                    v.get("error")
                                        .and_then(Value::as_str)
                                        .map(|s| s.trim().to_string())
                                })
                                .filter(|s| !s.is_empty());
                            {
                                let mut rt = manager.inner.runtime.write().await;
                                rt.connected = false;
                                rt.hub_url = raw_hub.clone();
                                rt.outbound_tx = None;
                                rt.last_error = match err_detail {
                                    Some(detail) => format!("Hub Social HTTP {status}: {detail}"),
                                    None => format!("Hub Social HTTP error ({status})"),
                                };
                            }
                            manager.emit_snapshot().await;
                            let wait_secs = http_backoff_secs;
                            http_backoff_secs = (http_backoff_secs * 2).min(30);
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => {}
                                res = reconnect_rx.changed() => {
                                    if res.is_err() { break; }
                                    http_backoff_secs = 6;
                                }
                            }
                            continue;
                        }
                        Err(e) => {
                            {
                                let mut rt = manager.inner.runtime.write().await;
                                rt.connected = false;
                                rt.hub_url = raw_hub.clone();
                                rt.outbound_tx = None;
                                rt.last_error = format!("No se pudo conectar al Hub Social (Vercel): {e}");
                            }
                            manager.emit_snapshot().await;
                            let wait_secs = http_backoff_secs;
                            http_backoff_secs = (http_backoff_secs * 2).min(30);
                            tokio::select! {
                                _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => {}
                                res = reconnect_rx.changed() => {
                                    if res.is_err() { break; }
                                    http_backoff_secs = 6;
                                }
                            }
                            continue;
                        }
                    }

                    let mut poll_interval = tokio::time::interval(Duration::from_secs(10));
                    poll_interval.tick().await;
                    let mut consecutive_errors = 0u32;

                    loop {
                        tokio::select! {
                            res = reconnect_rx.changed() => {
                                let _ = res;
                                break;
                            }
                            maybe_msg = outbound_rx.recv() => {
                                let Some(val) = maybe_msg else { break; };
                                let action = val.get("type").and_then(Value::as_str).unwrap_or("sync");
                                if action == "ping" {
                                    continue;
                                }
                                let current_profile = ensure_user_profile(&manager.inner.state_store)
                                    .unwrap_or_else(|_| profile.clone());
                                let resp = http_client
                                    .post(&social_endpoint)
                                    .json(&json!({
                                        "userId": current_profile.user_id,
                                        "displayName": current_profile.display_name,
                                        "googleAccountName": current_profile.google_account_name,
                                        "action": action,
                                        "payload": val,
                                    }))
                                    .send()
                                    .await;
                                if let Ok(r) = resp && r.status().is_success() {
                                    consecutive_errors = 0;
                                    if let Ok(body) = r.json::<Value>().await {
                                        manager.apply_http_social_response(&body).await;
                                    }
                                }
                            }
                            _ = poll_interval.tick() => {
                                let current_profile = ensure_user_profile(&manager.inner.state_store)
                                    .unwrap_or_else(|_| profile.clone());
                                let current_room = {
                                    let rt = manager.inner.runtime.read().await;
                                    rt.published_room.clone()
                                };
                                let resp = http_client
                                    .post(&social_endpoint)
                                    .json(&json!({
                                        "userId": current_profile.user_id,
                                        "displayName": current_profile.display_name,
                                        "googleAccountName": current_profile.google_account_name,
                                        "action": "sync",
                                        "hasRoomState": true,
                                        "room": current_room,
                                    }))
                                    .send()
                                    .await;
                                match resp {
                                    Ok(r) if r.status().is_success() => {
                                        consecutive_errors = 0;
                                        if let Ok(body) = r.json::<Value>().await {
                                            manager.apply_http_social_response(&body).await;
                                        }
                                    }
                                    _ => {
                                        consecutive_errors += 1;
                                        if consecutive_errors >= 4 {
                                            let mut rt = manager.inner.runtime.write().await;
                                            rt.last_error = "Conexión perdida con Hub Social (Vercel).".to_string();
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    {
                        let mut rt = manager.inner.runtime.write().await;
                        rt.connected = false;
                        rt.outbound_tx = None;
                    }
                    manager.emit_snapshot().await;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(4)) => {}
                        res = reconnect_rx.changed() => {
                            if res.is_err() { break; }
                        }
                    }
                    continue;
                }

                let mut url = match Url::parse(&format!("{ws_base}/social/ws")) {
                    Ok(u) => u,
                    Err(e) => {
                        {
                            let mut rt = manager.inner.runtime.write().await;
                            rt.connected = false;
                            rt.last_error = format!("URL de Hub inválida: {e}");
                        }
                        manager.emit_snapshot().await;
                        if reconnect_rx.changed().await.is_err() {
                            break;
                        }
                        continue;
                    }
                };
                url.query_pairs_mut()
                    .append_pair("userId", &profile.user_id)
                    .append_pair("displayName", &profile.display_name)
                    .append_pair("googleAccountName", &profile.google_account_name);

                let connect_res = tokio::time::timeout(
                    Duration::from_secs(12),
                    tokio_tungstenite::connect_async(url.as_str()),
                )
                .await;

                let ws_stream = match connect_res {
                    Ok(Ok((stream, _))) => stream,
                    Ok(Err(e)) => {
                        {
                            let mut rt = manager.inner.runtime.write().await;
                            rt.connected = false;
                            rt.hub_url = raw_hub;
                            rt.outbound_tx = None;
                            rt.last_error = format!("No se pudo conectar al Hub Social: {e}");
                        }
                        manager.emit_snapshot().await;
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(6)) => {}
                            res = reconnect_rx.changed() => {
                                if res.is_err() { break; }
                            }
                        }
                        continue;
                    }
                    Err(_) => {
                        {
                            let mut rt = manager.inner.runtime.write().await;
                            rt.connected = false;
                            rt.hub_url = raw_hub;
                            rt.outbound_tx = None;
                            rt.last_error = "Tiempo de espera agotado al conectar con el Hub Social.".to_string();
                        }
                        manager.emit_snapshot().await;
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(6)) => {}
                            res = reconnect_rx.changed() => {
                                if res.is_err() { break; }
                            }
                        }
                        continue;
                    }
                };

                let (mut ws_sink, mut ws_source) = ws_stream.split();
                let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();

                let published_room = {
                    let mut rt = manager.inner.runtime.write().await;
                    rt.connected = true;
                    rt.hub_url = raw_hub;
                    rt.last_error.clear();
                    rt.outbound_tx = Some(outbound_tx.clone());
                    rt.published_room.clone()
                };
                manager.emit_snapshot().await;

                let _ = outbound_tx.send(json!({
                    "type": "room_presence_update",
                    "room": published_room,
                }));

                let writer = tokio::spawn(async move {
                    while let Some(val) = outbound_rx.recv().await {
                        if let Ok(txt) = serde_json::to_string(&val)
                            && ws_sink.send(Message::Text(txt.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    let _ = ws_sink.close().await;
                });

                let mut ping_interval = tokio::time::interval(Duration::from_secs(25));
                ping_interval.tick().await;

                loop {
                    tokio::select! {
                        _ = ping_interval.tick() => {
                            let _ = outbound_tx.send(json!({ "type": "ping" }));
                        }
                        res = reconnect_rx.changed() => {
                            let _ = res;
                            break;
                        }
                        frame = ws_source.next() => {
                            match frame {
                                Some(Ok(Message::Text(text))) => {
                                    manager.handle_incoming_text(&text).await;
                                }
                                Some(Ok(Message::Close(_))) | None => break,
                                Some(Err(e)) => {
                                    let mut rt = manager.inner.runtime.write().await;
                                    rt.last_error = format!("Conexión perdida con Hub Social: {e}");
                                    break;
                                }
                                _ => {}
                            }
                        }
                    }
                }

                writer.abort();
                {
                    let mut rt = manager.inner.runtime.write().await;
                    rt.connected = false;
                    rt.outbound_tx = None;
                }
                manager.emit_snapshot().await;

                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(4)) => {}
                    res = reconnect_rx.changed() => {
                        if res.is_err() { break; }
                    }
                }
            }
        });
    }

    async fn apply_http_social_response(&self, body: &Value) {
        if let Some(sync_val) = body.get("sync") {
            if let Ok(txt) = serde_json::to_string(sync_val) {
                self.handle_incoming_text(&txt).await;
            }
        }
        if let Some(events) = body.get("events").and_then(Value::as_array) {
            for ev in events {
                if let Ok(txt) = serde_json::to_string(ev) {
                    self.handle_incoming_text(&txt).await;
                }
            }
        }
        if let Some(notice) = body.get("notice") {
            let level = notice.get("level").and_then(Value::as_str).unwrap_or("info");
            let message = notice.get("message").and_then(Value::as_str).unwrap_or("");
            if !message.is_empty() {
                self.emit_notice(level, message);
            }
        }
    }

    async fn handle_incoming_text(&self, text: &str) {
        let Ok(val) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let msg_type = val.get("type").and_then(Value::as_str).unwrap_or("");

        match msg_type {
            "social_sync" => {
                let remote_friends: Vec<RemoteFriendState> = val
                    .get("friends")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let incoming_friend_requests: Vec<FriendRequestItem> = val
                    .get("incomingFriendRequests")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let outgoing_friend_requests: Vec<FriendRequestItem> = val
                    .get("outgoingFriendRequests")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let incoming_room_invites: Option<Vec<RoomInviteItem>> = val
                    .get("incomingRoomInvites")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
                let incoming_join_requests: Vec<RoomJoinRequestItem> = val
                    .get("incomingJoinRequests")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                // Sync mutual friends into local StateStore (preserving default_allow_queue)
                let _ = self.inner.state_store.update(|s| {
                    for rf in &remote_friends {
                        if let Some(existing) = s
                            .friends
                            .iter_mut()
                            .find(|f| f.user_id.eq_ignore_ascii_case(&rf.user_id))
                        {
                            existing.display_name = rf.display_name.clone();
                            if !rf.google_account_name.is_empty() {
                                existing.google_account_name = rf.google_account_name.clone();
                            }
                        } else {
                            s.friends.push(FriendRecord {
                                user_id: rf.user_id.clone(),
                                display_name: rf.display_name.clone(),
                                google_account_name: rf.google_account_name.clone(),
                                note: String::new(),
                                default_allow_queue: true,
                                added_at: Utc::now().to_rfc3339(),
                            });
                        }
                    }
                    Ok(())
                });

                {
                    let mut rt = self.inner.runtime.write().await;
                    rt.remote_friends = remote_friends;
                    rt.incoming_friend_requests = incoming_friend_requests;
                    rt.outgoing_friend_requests = outgoing_friend_requests;
                    if let Some(invites) = incoming_room_invites {
                        rt.incoming_room_invites = invites;
                    }
                    rt.incoming_join_requests = incoming_join_requests;
                }
                self.emit_snapshot().await;
            }
            "room_invite_received" => {
                if let Some(inv_val) = val.get("invite")
                    && let Ok(invite) = serde_json::from_value::<RoomInviteItem>(inv_val.clone())
                {
                    {
                        let mut rt = self.inner.runtime.write().await;
                        rt.incoming_room_invites.retain(|i| {
                            !(i.from_user_id.eq_ignore_ascii_case(&invite.from_user_id)
                                && i.room_code.eq_ignore_ascii_case(&invite.room_code))
                        });
                        rt.incoming_room_invites.insert(0, invite);
                        rt.incoming_room_invites.truncate(15);
                    }
                    self.emit_snapshot().await;
                }
            }
            "room_join_request_received" => {
                if let Some(req_val) = val.get("request")
                    && let Ok(req) = serde_json::from_value::<RoomJoinRequestItem>(req_val.clone())
                {
                    {
                        let mut rt = self.inner.runtime.write().await;
                        rt.incoming_join_requests.retain(|r| r.id != req.id);
                        rt.incoming_join_requests.insert(0, req);
                    }
                    self.emit_snapshot().await;
                }
            }
            "room_join_approved" => {
                let host_user_id = val
                    .get("hostUserId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let host_display_name = val
                    .get("hostDisplayName")
                    .and_then(Value::as_str)
                    .unwrap_or("Host")
                    .to_string();
                let room_code = val
                    .get("roomCode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let invite_url = val
                    .get("inviteUrl")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if !invite_url.is_empty() {
                    let _ = self.inner.events_tx.send(SocialEvent::JoinApproved {
                        host_user_id,
                        host_display_name,
                        room_code,
                        invite_url,
                    });
                }
            }
            "notice" => {
                let level = val.get("level").and_then(Value::as_str).unwrap_or("info");
                let message = val.get("message").and_then(Value::as_str).unwrap_or("");
                if !message.is_empty() {
                    self.emit_notice(level, message);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_hub_urls_and_builds_relay_url() {
        assert_eq!(
            normalize_hub_ws_base("https://playzi-hub.example.workers.dev/"),
            Some("wss://playzi-hub.example.workers.dev".to_string())
        );
        assert_eq!(
            normalize_hub_ws_base("http://localhost:8787/social/ws"),
            Some("ws://localhost:8787".to_string())
        );
        assert_eq!(
            normalize_hub_ws_base("playzi-hub.example.workers.dev"),
            Some("wss://playzi-hub.example.workers.dev".to_string())
        );
        assert_eq!(
            build_relay_ws_url("https://playzi-hub.example.workers.dev", "4enuah", None),
            Some("wss://playzi-hub.example.workers.dev/ws?room=4ENUAH".to_string())
        );
        assert_eq!(
            build_relay_ws_url("https://playzi-hub.example.workers.dev", "4enuah", Some("host")),
            Some("wss://playzi-hub.example.workers.dev/ws?room=4ENUAH&role=host".to_string())
        );
    }
}
