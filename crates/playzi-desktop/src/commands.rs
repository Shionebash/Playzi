use std::collections::{BTreeMap, HashMap};
use std::fs;

use playzi_core::autosync::SyncSection;
use playzi_core::collections::{create_collection, delete_collection, list_collections};
use playzi_core::config::ConfigUpdate;
use playzi_core::cookies::{CookieJar, NetscapeCookie, looks_like_netscape_cookie_file};
use playzi_core::downloads::DownloadRequest;
use playzi_core::history::{delete_history_event, list_history};
use playzi_core::party::{
    AddQueueItemInput, FriendUpsertRequest, JoinSessionRequest, PlaybackSyncUpdate,
    PolicyUpdateRequest, ProfileUpdateRequest, StartHostRequest, ensure_user_profile,
    list_friends, remove_friend, update_user_profile, upsert_friend,
};
use playzi_core::player::{PlayRequest, PlayerControlCommand};
use playzi_core::playlists::{
    add_item, create_playlist, delete_playlist, list_playlists, remove_item, rename_playlist,
    reorder_items,
};
use playzi_core::youtube::{delete_channel, list_channels, list_feed};
use serde_json::{Value, json};
use tauri::webview::PageLoadEvent;
use tauri::{
    AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use url::Url;

use crate::media_controls;
use crate::runtime::AppRuntime;

pub const AUTH_WINDOW_LABEL: &str = "auth-google";
const AUTH_LOGIN_URL: &str = "https://music.youtube.com/";

#[tauri::command]
pub async fn api_request(
    method: String,
    path: String,
    body: Option<Value>,
    runtime: State<'_, AppRuntime>,
    app: AppHandle,
) -> Result<Value, String> {
    // Before refreshing auth, sync any Google/YouTube cookies already present in WebView2
    if path.starts_with("/api/music/auth/refresh") {
        if let Some(win) = app
            .get_webview_window(AUTH_WINDOW_LABEL)
            .or_else(|| app.get_webview_window("main"))
        {
            let jar = extract_webview_cookies_to_jar(&win);
            if jar.sapisid().is_some() {
                let _ = persist_cookie_jar(&jar, runtime.inner());
            }
        }
    }

    let app_for_auth = app.clone();
    let open_auth = move || open_auth_window_inner(&app_for_auth);
    let result = dispatch_api_request(
        runtime.inner(),
        &method,
        &path,
        body,
        Some(&open_auth),
    )
    .await?;

    if path.starts_with("/api/music/auth/refresh") {
        let _ = app.emit("auth-status", &result);
    }
    Ok(result)
}

#[tauri::command]
pub fn get_bootstrap(runtime: State<'_, AppRuntime>) -> Result<Value, String> {
    runtime.bootstrap_payload().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_state_summary(runtime: State<'_, AppRuntime>) -> Result<Value, String> {
    runtime.state_summary_payload().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_auth_window(app: AppHandle) -> Result<Value, String> {
    open_auth_window_inner(&app)?;
    Ok(json!({ "ok": true, "window": AUTH_WINDOW_LABEL }))
}

#[tauri::command]
pub fn player_status(runtime: State<'_, AppRuntime>) -> Result<Value, String> {
    Ok(json!({
        "state": runtime.player.playback_state(),
        "capabilities": runtime.player.capabilities(),
    }))
}

#[tauri::command]
pub fn player_control(
    command: PlayerControlCommand,
    runtime: State<'_, AppRuntime>,
) -> Result<Value, String> {
    let st = runtime.player.control(command).map_err(|e| e.to_string())?;
    serde_json::to_value(st).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_main_window_hwnd(app: AppHandle) -> Result<Option<i64>, String> {
    Ok(app
        .get_webview_window("main")
        .and_then(|w| media_controls::window_hwnd_isize(&w))
        .map(|h| h as i64))
}

/// Saves Netscape-formatted browser cookies into `data/private/playzi_browser_cookies.txt`,
/// refreshes the private cookie cache, checks YouTube Music auth, and emits `auth-status`.
#[tauri::command]
pub async fn save_browser_cookies(
    netscape_content: String,
    runtime: State<'_, AppRuntime>,
    app: AppHandle,
) -> Result<Value, String> {
    let jar = CookieJar::parse(&netscape_content);
    if jar.is_empty() {
        return Err("No se encontraron cookies Netscape validas.".into());
    }

    persist_cookie_jar(&jar, runtime.inner())?;
    check_and_emit_auth(runtime.inner(), &app).await
}

fn persist_cookie_jar(jar: &CookieJar, runtime: &AppRuntime) -> Result<(), String> {
    let cookie_store = runtime.cookie_store();
    let target = cookie_store.managed_browser_cookies_path();
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = target.with_extension("txt.tmp");
    fs::write(&tmp, jar.to_netscape_string()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &target).map_err(|e| e.to_string())?;

    if !looks_like_netscape_cookie_file(&target) {
        return Err("El archivo de cookies generado no es valido.".into());
    }
    let _ = cookie_store.invalidate_cache();
    let _ = cookie_store.cache_from_file(&target);
    Ok(())
}

async fn check_and_emit_auth(runtime: &AppRuntime, app: &AppHandle) -> Result<Value, String> {
    let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
    let auth = ytm
        .check_auth(Some(&runtime.state))
        .await
        .map_err(|e| e.to_string())?;

    let payload = json!({
        "ok": auth.state == "ok",
        "authState": auth.state,
        "accountName": auth.account_name,
        "checkedAt": auth.checked_at,
    });
    let _ = app.emit("auth-status", &payload);
    Ok(payload)
}

fn extract_webview_cookies_to_jar(window: &WebviewWindow) -> CookieJar {
    const AUTH_COOKIE_URLS: &[(&str, &str)] = &[
        ("https://music.youtube.com/", ".youtube.com"),
        ("https://www.youtube.com/", ".youtube.com"),
        ("https://accounts.google.com/", ".google.com"),
        ("https://www.google.com/", ".google.com"),
    ];

    let mut dedup: BTreeMap<(String, String, String), NetscapeCookie> = BTreeMap::new();
    let mut push_cookie = |c: tauri::webview::Cookie<'_>, fallback_domain: &str| {
        let raw_domain = c.domain().unwrap_or(fallback_domain).trim();
        if raw_domain.is_empty() {
            return;
        }
        let lower_dom = raw_domain.to_ascii_lowercase();
        if !(lower_dom.ends_with("youtube.com") || lower_dom.ends_with("google.com")) {
            return;
        }
        let domain = raw_domain.to_string();
        let include_subdomains = domain.starts_with('.');
        let path = c.path().unwrap_or("/").to_string();
        let secure = c.secure().unwrap_or(false);
        let http_only = c.http_only().unwrap_or(false);
        let expires = c
            .expires_datetime()
            .map(|dt| dt.unix_timestamp())
            .unwrap_or(0);
        let name = c.name().to_string();
        let value = c.value().to_string();
        if name.is_empty() {
            return;
        }
        dedup.insert(
            (domain.clone(), path.clone(), name.clone()),
            NetscapeCookie {
                domain,
                include_subdomains,
                path,
                secure,
                expires,
                name,
                value,
                http_only,
            },
        );
    };

    // First pull ALL cookies from the WebView2 cookie manager (captures SameSite=Lax/1P cookies like SID, SAPISID, LOGIN_INFO)
    if let Ok(all_cookies) = window.cookies() {
        for c in all_cookies {
            push_cookie(c, "");
        }
    }

    for (raw_url, fallback_domain) in AUTH_COOKIE_URLS {
        let Ok(url) = Url::parse(raw_url) else {
            continue;
        };
        let Ok(cookies) = window.cookies_for_url(url) else {
            continue;
        };
        for c in cookies {
            push_cookie(c, fallback_domain);
        }
    }
    CookieJar::new(dedup.into_values().collect())
}

fn open_auth_window_inner(app: &AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window(AUTH_WINDOW_LABEL) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }
    let url = Url::parse(AUTH_LOGIN_URL).map_err(|e| e.to_string())?;
    let app_on_load = app.clone();
    let win = WebviewWindowBuilder::new(app, AUTH_WINDOW_LABEL, WebviewUrl::External(url))
        .title("Iniciar sesion en YouTube / Google - Playzi")
        .inner_size(980.0, 740.0)
        .resizable(true)
        .on_page_load(move |window, payload| {
            if payload.event() != PageLoadEvent::Finished {
                return;
            }
            let host = payload.url().host_str().unwrap_or("");
            if !host.ends_with("youtube.com") {
                return;
            }
            let jar = extract_webview_cookies_to_jar(&window);
            if jar.sapisid().is_none() {
                return;
            }
            let app_handle = app_on_load.clone();
            let win_clone = window.clone();
            tauri::async_runtime::spawn(async move {
                let runtime = app_handle.state::<AppRuntime>().inner().clone();
                if persist_cookie_jar(&jar, &runtime).is_ok() {
                    if let Ok(status) = check_and_emit_auth(&runtime, &app_handle).await {
                        if status.get("ok").and_then(Value::as_bool) == Some(true) {
                            let _ = win_clone.close();
                        }
                    }
                }
            });
        })
        .build()
        .map_err(|e| e.to_string())?;

    let app_on_close = app.clone();
    let win_on_close = win.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { .. } = event {
            let jar = extract_webview_cookies_to_jar(&win_on_close);
            if jar.sapisid().is_some() {
                let app_handle = app_on_close.clone();
                tauri::async_runtime::spawn(async move {
                    let runtime = app_handle.state::<AppRuntime>().inner().clone();
                    if persist_cookie_jar(&jar, &runtime).is_ok() {
                        let _ = check_and_emit_auth(&runtime, &app_handle).await;
                    }
                });
            }
        }
    });

    Ok(())
}

/// Core in-process API router invoked by `api_request` (and directly testable without a GUI).
pub async fn dispatch_api_request(
    runtime: &AppRuntime,
    method: &str,
    raw_path: &str,
    body: Option<Value>,
    open_auth: Option<&(dyn Fn() -> Result<(), String> + Send + Sync)>,
) -> Result<Value, String> {
    let method = method.trim().to_ascii_uppercase();
    let (path, query) = parse_path_and_query(raw_path)?;
    let body = body.unwrap_or_else(|| json!({}));

    match (method.as_str(), path.as_str()) {
        ("GET", "/api/bootstrap") => runtime.bootstrap_payload().map_err(|e| e.to_string()),
        ("GET", "/api/state/summary") => runtime.state_summary_payload().map_err(|e| e.to_string()),
        ("GET", "/api/config") => {
            serde_json::to_value(runtime.config.public_config()).map_err(|e| e.to_string())
        }
        ("POST", "/api/config") => {
            let patch: ConfigUpdate =
                serde_json::from_value(body).map_err(|e| format!("Invalid config payload: {e}"))?;
            let updated = runtime.config.update(patch).map_err(|e| e.to_string())?;
            serde_json::to_value(updated).map_err(|e| e.to_string())
        }
        ("GET", "/api/search") => {
            let q = query.get("q").map(String::as_str).unwrap_or("");
            let scope = query.get("scope").map(String::as_str).unwrap_or("youtube");
            let filter = query.get("search_filter").map(String::as_str);
            let default_limit = runtime.config.snapshot().ytdlp_search_limit;
            let limit = query
                .get("limit")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(default_limit);

            let items = if scope.eq_ignore_ascii_case("music")
                || scope.eq_ignore_ascii_case("ytmusic")
            {
                let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
                ytm.search_music(q, filter, limit)
                    .await
                    .map_err(|e| e.to_string())?
            } else {
                runtime
                    .youtube
                    .search(q, limit)
                    .await
                    .map_err(|e| e.to_string())?
            };
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/downloads") => {
            let items = runtime.downloads.list_downloads().map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/downloads/status") => {
            let items = runtime
                .downloads
                .list_download_status()
                .map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("POST", "/api/downloads") => {
            let req: DownloadRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid download request: {e}"))?;
            runtime.downloads.start_download(req).map_err(|e| e.to_string())
        }
        ("GET", "/api/library") => {
            let items = runtime.downloads.list_library().map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/history") => {
            let items = list_history(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/channels") => {
            let items = list_channels(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("POST", "/api/channels") => {
            let url = body.get("url").and_then(Value::as_str).unwrap_or("");
            let name = body.get("name").and_then(Value::as_str);
            runtime
                .youtube
                .add_channel(&runtime.state, url, name)
                .await
                .map_err(|e| e.to_string())
        }
        ("POST", "/api/channels/refresh") => {
            let res = runtime
                .autosync
                .refresh_section(SyncSection::Channels)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true, "result": res }))
        }
        ("GET", "/api/channel") => {
            let url = query.get("url").map(String::as_str).unwrap_or("");
            let tab = query.get("tab").map(String::as_str).unwrap_or("videos");
            let page = query
                .get("page")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1);
            let page_size = query
                .get("page_size")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(30);
            let res = runtime
                .youtube
                .browse_channel(url, tab, page, page_size, Some(&runtime.state))
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(res).map_err(|e| e.to_string())
        }
        ("GET", "/api/feed") => {
            let items = list_feed(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/collections") => {
            let items = list_collections(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("POST", "/api/collections") => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            let created = create_collection(&runtime.state, name).map_err(|e| e.to_string())?;
            serde_json::to_value(created).map_err(|e| e.to_string())
        }
        ("GET", "/api/playlists") => {
            let items = list_playlists(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("POST", "/api/playlists") => {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            let created = create_playlist(&runtime.state, name).map_err(|e| e.to_string())?;
            serde_json::to_value(created).map_err(|e| e.to_string())
        }
        ("GET", "/api/radio") => {
            let url = query.get("url").map(String::as_str).unwrap_or("");
            let items = runtime
                .youtube
                .fetch_radio_items(url, 25)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/recommendations") => {
            let s = runtime.state.read().map_err(|e| e.to_string())?;
            Ok(json!({
                "items": s.recommendations,
                "lastSync": s.recommendations_last_sync,
            }))
        }
        ("POST", "/api/recommendations/refresh") => runtime
            .autosync
            .refresh_section(SyncSection::Recommendations)
            .await
            .map_err(|e| e.to_string()),
        ("GET", "/api/music/recommendations") => {
            let s = runtime.state.read().map_err(|e| e.to_string())?;
            Ok(json!({
                "items": s.music_recommendations,
                "lastSync": s.music_recom_last_sync,
            }))
        }
        ("POST", "/api/music/recommendations/refresh") => runtime
            .autosync
            .refresh_section(SyncSection::MusicRecommendations)
            .await
            .map_err(|e| e.to_string()),
        ("GET", "/api/music/playlists") => {
            let s = runtime.state.read().map_err(|e| e.to_string())?;
            Ok(json!({
                "items": s.music_playlists,
                "lastSync": s.music_playlists_last_sync,
            }))
        }
        ("POST", "/api/music/playlists/refresh") => runtime
            .autosync
            .refresh_section(SyncSection::MusicPlaylists)
            .await
            .map_err(|e| e.to_string()),
        ("POST", "/api/music/playlist-songs") => {
            let url_or_id = body.get("url").and_then(Value::as_str).unwrap_or("");
            let playlist_id = extract_playlist_id(url_or_id);
            let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
            let items = ytm
                .fetch_playlist_songs(&playlist_id)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/music/artist") => {
            let channel_id = query.get("channel_id").map(String::as_str).unwrap_or("");
            let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
            let info = ytm
                .get_artist_info(channel_id)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(info).map_err(|e| e.to_string())
        }
        ("POST", "/api/music/auth/refresh") => {
            let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
            let status = ytm
                .check_auth(Some(&runtime.state))
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": status.state == "ok",
                "authState": status.state,
                "accountName": status.account_name,
                "checkedAt": status.checked_at,
                "task": Value::Null,
            }))
        }
        ("POST", "/api/music/login") => {
            if let Some(cb) = open_auth {
                cb()?;
            }
            Ok(json!({ "ok": true, "mode": "webview", "task": Value::Null }))
        }
        ("GET", "/api/sync/status") => {
            let status = runtime
                .autosync
                .get_sync_status()
                .map_err(|e| e.to_string())?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        ("POST", "/api/sync/refresh") => runtime
            .autosync
            .refresh_all()
            .await
            .map_err(|e| e.to_string()),
        ("POST", "/api/play") => {
            let req: PlayRequest = serde_json::from_value(body).map_err(|e| e.to_string())?;
            let player = runtime.player.clone();
            let resp = tokio::task::spawn_blocking(move || player.play(req))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            serde_json::to_value(resp).map_err(|e| e.to_string())
        }
        ("GET", "/api/player/status") => Ok(json!({
            "state": runtime.player.playback_state(),
            "capabilities": runtime.player.capabilities(),
        })),
        ("POST", "/api/player/control") => {
            let cmd: PlayerControlCommand =
                serde_json::from_value(body).map_err(|e| e.to_string())?;
            let st = runtime.player.control(cmd).map_err(|e| e.to_string())?;
            serde_json::to_value(st).map_err(|e| e.to_string())
        }
        ("POST", "/api/player/stop") => {
            let st = runtime.player.stop().map_err(|e| e.to_string())?;
            serde_json::to_value(st).map_err(|e| e.to_string())
        }
        ("GET", "/api/player/info") => {
            let url = query.get("url").cloned().unwrap_or_default();
            let quality = query.get("quality").map(String::as_str);
            let info = runtime
                .player
                .resolve_stream(&url, quality, false)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(info).map_err(|e| e.to_string())
        }
        ("GET", "/api/player/audio") => {
            let url = query.get("url").cloned().unwrap_or_default();
            runtime
                .player
                .resolve_audio(&url)
                .await
                .map_err(|e| e.to_string())
        }
        ("GET", "/api/player/audio-tracks") => {
            let url = query.get("url").cloned().unwrap_or_default();
            let tracks = runtime
                .player
                .resolve_audio_tracks(&url)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "tracks": tracks }))
        }
        ("POST", "/api/player/playlist") => {
            let url = body
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let limit = body
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100) as usize;
            let items = runtime
                .player
                .resolve_playlist(&url, limit)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "items": items, "requiresLogin": false }))
        }
        ("GET", "/api/profile") => {
            let prof = ensure_user_profile(&runtime.state).map_err(|e| e.to_string())?;
            serde_json::to_value(prof).map_err(|e| e.to_string())
        }
        ("POST", "/api/profile") => {
            let req: ProfileUpdateRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid profile payload: {e}"))?;
            let prof = update_user_profile(&runtime.state, req).map_err(|e| e.to_string())?;
            serde_json::to_value(prof).map_err(|e| e.to_string())
        }
        ("GET", "/api/friends") => {
            let items = list_friends(&runtime.state).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("POST", "/api/friends") => {
            let req: FriendUpsertRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid friend payload: {e}"))?;
            let items = upsert_friend(&runtime.state, req).map_err(|e| e.to_string())?;
            Ok(json!({ "items": items }))
        }
        ("GET", "/api/party/status") => {
            let snap = runtime.party.status().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/host") => {
            let req: StartHostRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid host request: {e}"))?;
            let snap = runtime
                .party
                .start_host(req)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/tunnel") => {
            let snap = runtime
                .party
                .start_cloudflare_tunnel()
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/public-host") => {
            let public_host = body
                .get("publicHost")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let snap = runtime
                .party
                .set_public_host(public_host)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/join") => {
            let req: JoinSessionRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid join request: {e}"))?;
            let snap = runtime
                .party
                .join_session(req)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/leave") => {
            let snap = runtime.party.leave_or_stop().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/queue/add") => {
            let play_next = body
                .get("playNext")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let item_val = body.get("item").cloned().unwrap_or(body);
            let item: AddQueueItemInput = serde_json::from_value(item_val)
                .map_err(|e| format!("Invalid queue item: {e}"))?;
            let snap = runtime
                .party
                .add_to_queue(item, play_next)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/queue/remove") => {
            let index = body
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| "Missing index".to_string())? as usize;
            let snap = runtime
                .party
                .remove_queue_item(index)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/playback") => {
            let update: PlaybackSyncUpdate = serde_json::from_value(body)
                .map_err(|e| format!("Invalid playback update: {e}"))?;
            let snap = runtime
                .party
                .sync_playback(update)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/policy") => {
            let req: PolicyUpdateRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid policy update: {e}"))?;
            let snap = runtime
                .party
                .update_policy(req)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/kick") => {
            let user_id = body
                .get("userId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let snap = runtime
                .party
                .kick_participant(&user_id)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        _ => dispatch_dynamic_routes(runtime, &method, &path, body).await,
    }
}

async fn dispatch_dynamic_routes(
    runtime: &AppRuntime,
    method: &str,
    path: &str,
    body: Value,
) -> Result<Value, String> {
    // /api/downloads/{id}/cancel
    if method == "POST" && path.starts_with("/api/downloads/") && path.ends_with("/cancel") {
        let id = path
            .trim_start_matches("/api/downloads/")
            .trim_end_matches("/cancel");
        runtime
            .downloads
            .cancel_download(id)
            .map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    // DELETE /api/downloads/{id}
    if method == "DELETE" && path.starts_with("/api/downloads/") {
        let id = path.trim_start_matches("/api/downloads/");
        runtime
            .downloads
            .delete_download(id)
            .map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    // DELETE /api/history/{id}
    if method == "DELETE" && path.starts_with("/api/history/") {
        let id = path.trim_start_matches("/api/history/");
        delete_history_event(&runtime.state, id).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    // DELETE /api/collections/{id}
    if method == "DELETE" && path.starts_with("/api/collections/") {
        let id = path.trim_start_matches("/api/collections/");
        delete_collection(&runtime.state, id).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    // DELETE /api/friends/{user_id}
    if method == "DELETE" && path.starts_with("/api/friends/") {
        let user_id = path.trim_start_matches("/api/friends/");
        let items = remove_friend(&runtime.state, user_id).map_err(|e| e.to_string())?;
        return Ok(json!({ "items": items }));
    }

    // POST /api/channels/{id}/refresh
    if method == "POST" && path.starts_with("/api/channels/") && path.ends_with("/refresh") {
        let id = path
            .trim_start_matches("/api/channels/")
            .trim_end_matches("/refresh");
        let ch = runtime
            .youtube
            .refresh_channel(&runtime.state, id)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true, "channel": ch }));
    }

    // DELETE /api/channels/{id}
    if method == "DELETE" && path.starts_with("/api/channels/") {
        let id = path.trim_start_matches("/api/channels/");
        delete_channel(&runtime.state, id).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    // Playlist dynamic routes: /api/playlists/{id}...
    if let Some(rest) = path.strip_prefix("/api/playlists/") {
        if method == "DELETE" && !rest.contains('/') {
            delete_playlist(&runtime.state, rest).map_err(|e| e.to_string())?;
            return Ok(json!({ "ok": true }));
        }
        if let Some((pl_id, sub)) = rest.split_once('/') {
            match (method, sub) {
                ("POST", "rename") => {
                    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
                    let updated = rename_playlist(&runtime.state, pl_id, name)
                        .map_err(|e| e.to_string())?;
                    return serde_json::to_value(updated).map_err(|e| e.to_string());
                }
                ("POST", "items") => {
                    let added = add_item(&runtime.state, pl_id, body).map_err(|e| e.to_string())?;
                    return Ok(added);
                }
                ("POST", "items/remove") => {
                    let url = body.get("url").and_then(Value::as_str).unwrap_or("");
                    remove_item(&runtime.state, pl_id, url).map_err(|e| e.to_string())?;
                    return Ok(json!({ "ok": true }));
                }
                ("POST", "items/reorder") => {
                    let urls: Vec<String> = body
                        .get("urls")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(ToString::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    reorder_items(&runtime.state, pl_id, &urls).map_err(|e| e.to_string())?;
                    return Ok(json!({ "ok": true }));
                }
                _ => {}
            }
        }
    }

    Err(format!("Unsupported desktop API route: {method} {path}"))
}

fn parse_path_and_query(raw_path: &str) -> Result<(String, HashMap<String, String>), String> {
    let full = if raw_path.starts_with("http://") || raw_path.starts_with("https://") {
        raw_path.to_string()
    } else if raw_path.starts_with('/') {
        format!("http://playzi.local{raw_path}")
    } else {
        format!("http://playzi.local/{raw_path}")
    };
    let parsed = Url::parse(&full).map_err(|e| format!("Invalid API path '{raw_path}': {e}"))?;
    let query: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
    Ok((parsed.path().to_string(), query))
}

fn extract_playlist_id(url_or_id: &str) -> String {
    if let Ok(parsed) = Url::parse(url_or_id) {
        if let Some((_, list)) = parsed.query_pairs().find(|(k, _)| k == "list") {
            return list.into_owned();
        }
    }
    url_or_id.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn desktop_api_router_handles_bootstrap_config_collections_and_playlists() {
        let dir = TempDir::new().unwrap();
        let runtime = AppRuntime::initialize(dir.path().to_path_buf()).unwrap();

        let boot = dispatch_api_request(&runtime, "GET", "/api/bootstrap", None, None)
            .await
            .unwrap();
        assert!(boot.get("config").is_some());
        assert!(boot.get("downloads").is_some());

        let updated_cfg = dispatch_api_request(
            &runtime,
            "POST",
            "/api/config",
            Some(json!({ "searchLimit": 18, "playbackQuality": "720p" })),
            None,
        )
        .await
        .unwrap();
        assert_eq!(updated_cfg["searchLimit"], 18);
        assert_eq!(updated_cfg["playbackQuality"], "720p");

        let col = dispatch_api_request(
            &runtime,
            "POST",
            "/api/collections",
            Some(json!({ "name": "Synthwave" })),
            None,
        )
        .await
        .unwrap();
        let col_id = col["id"].as_str().unwrap();

        let cols = dispatch_api_request(&runtime, "GET", "/api/collections", None, None)
            .await
            .unwrap();
        assert_eq!(cols["items"].as_array().unwrap().len(), 1);

        dispatch_api_request(
            &runtime,
            "DELETE",
            &format!("/api/collections/{col_id}"),
            None,
            None,
        )
        .await
        .unwrap();

        let pl = dispatch_api_request(
            &runtime,
            "POST",
            "/api/playlists",
            Some(json!({ "name": "Desk Mix" })),
            None,
        )
        .await
        .unwrap();
        let pl_id = pl["id"].as_str().unwrap();

        dispatch_api_request(
            &runtime,
            "POST",
            &format!("/api/playlists/{pl_id}/items"),
            Some(json!({ "url": "https://youtu.be/dQw4w9WgXcQ", "title": "Track 1" })),
            None,
        )
        .await
        .unwrap();

        let pls = dispatch_api_request(&runtime, "GET", "/api/playlists", None, None)
            .await
            .unwrap();
        assert_eq!(pls["items"][0]["items"].as_array().unwrap().len(), 1);

        let summary = dispatch_api_request(&runtime, "GET", "/api/state/summary", None, None)
            .await
            .unwrap();
        assert!(summary.get("syncStatus").is_some());
    }
}
