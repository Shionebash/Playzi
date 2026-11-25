use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use playzi_core::autosync::SyncSection;
use playzi_core::collections::{create_collection, delete_collection, list_collections};
use playzi_core::config::ConfigUpdate;
use playzi_core::cookies::{
    CookieJar, candidates_for_config, export_cookies_from_browser, looks_like_netscape_cookie_file,
    open_url_in_default_browser,
};
use playzi_core::downloads::DownloadRequest;
use playzi_core::history::{delete_history_event, list_history};
use playzi_core::party::{
    AddQueueItemInput, FriendUpsertRequest, JoinSessionRequest,
    ParticipantPermissionsUpdateRequest, PlaybackSyncUpdate, PolicyUpdateRequest,
    ProfileUpdateRequest, StartHostRequest, ensure_user_profile, list_friends, remove_friend,
    update_user_profile, upsert_friend,
};
use playzi_core::player::{PlayRequest, PlayerControlCommand};
use playzi_core::playlists::{
    add_item, create_playlist, delete_playlist, get_playlist, import_playlist_items,
    list_playlists, remove_item, rename_playlist, reorder_items, sync_playlist_items,
};
use playzi_core::youtube::{delete_channel, extract_video_id, list_channels, list_feed};
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager, State};
use url::Url;

use crate::media_controls;
use crate::runtime::AppRuntime;
use crate::updater::DesktopUpdater;

pub const AUTH_WINDOW_LABEL: &str = "auth-google";
const AUTH_LOGIN_URL: &str =
    "https://accounts.google.com/AccountChooser?service=youtube&continue=https%3A%2F%2Fmusic.youtube.com%2F";
static AUTH_POLL_GENERATION: AtomicU64 = AtomicU64::new(0);

#[tauri::command]
pub async fn api_request(
    method: String,
    path: String,
    body: Option<Value>,
    runtime: State<'_, AppRuntime>,
    updater: State<'_, DesktopUpdater>,
    app: AppHandle,
) -> Result<Value, String> {
    let method_upper = method.to_ascii_uppercase();
    if method_upper == "GET" && path == "/api/update/status" {
        return serde_json::to_value(updater.snapshot()).map_err(|e| e.to_string());
    }
    if method_upper == "POST" && path == "/api/update/check" {
        let auto_install = body
            .as_ref()
            .and_then(|v| v.get("autoInstall"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let status = updater
            .check_and_install(app.clone(), auto_install, true)
            .await;
        return serde_json::to_value(status).map_err(|e| e.to_string());
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

    if path.starts_with("/api/music/auth/refresh")
        || path.starts_with("/api/music/accounts/select")
        || path.starts_with("/api/music/logout")
    {
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

#[tauri::command]
pub fn get_update_status(updater: State<'_, DesktopUpdater>) -> Result<Value, String> {
    serde_json::to_value(updater.snapshot()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn check_for_updates(
    auto_install: Option<bool>,
    updater: State<'_, DesktopUpdater>,
    app: AppHandle,
) -> Result<Value, String> {
    let status = updater
        .check_and_install(app, auto_install.unwrap_or(true), true)
        .await;
    serde_json::to_value(status).map_err(|e| e.to_string())
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

async fn try_import_cookies_from_system_browser(runtime: &AppRuntime) -> Option<String> {
    let cfg = runtime.config.snapshot();
    let ytdlp_bin = runtime.player.resolve_ytdlp_bin();
    let temp_dir = runtime.cookie_store().private_dir();
    let candidates = candidates_for_config(&cfg.cookies_browser);

    for candidate in candidates {
        if let Ok(jar) =
            export_cookies_from_browser(&ytdlp_bin, &candidate.ytdlp_spec, &temp_dir).await
        {
            if jar.sapisid().is_some() && persist_cookie_jar(&jar, runtime).is_ok() {
                return Some(candidate.label);
            }
        }
    }
    None
}

fn open_auth_window_inner(app: &AppHandle) -> Result<(), String> {
    open_url_in_default_browser(AUTH_LOGIN_URL)
        .map_err(|e| format!("No se pudo abrir el navegador predeterminado: {e}"))?;

    let poll_gen = AUTH_POLL_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let runtime = app_handle.state::<AppRuntime>().inner().clone();
        // Poll the user's default browser for up to ~3 minutes until signed in.
        for _ in 0..45 {
            if AUTH_POLL_GENERATION.load(Ordering::SeqCst) != poll_gen {
                break;
            }
            if try_import_cookies_from_system_browser(&runtime)
                .await
                .is_some()
            {
                if let Ok(status) = check_and_emit_auth(&runtime, &app_handle).await {
                    if status.get("ok").and_then(Value::as_bool) == Some(true) {
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
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
            if tab.eq_ignore_ascii_case("music") {
                let resolved_url = if playzi_core::youtube::extract_video_id(url).is_some()
                    && !url.contains("/channel/")
                    && !url.contains("/@")
                {
                    let meta = runtime
                        .youtube
                        .metadata_for_url(url)
                        .await
                        .map_err(|e| e.to_string())?;
                    meta.channel_url
                        .ok_or_else(|| "No se pudo identificar el canal de este video".to_string())?
                } else {
                    url.to_string()
                };
                let channel_id = runtime
                    .youtube
                    .resolve_channel_id(&resolved_url)
                    .await
                    .map_err(|e| e.to_string())?;
                let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
                let artist = ytm
                    .get_artist_info(&channel_id)
                    .await
                    .map_err(|e| e.to_string())?;
                let canonical = format!("https://www.youtube.com/channel/{channel_id}");
                let mut items = artist.songs.clone();
                for rel in &artist.releases {
                    items.push(playzi_core::youtube::MediaItem {
                        id: rel.id.clone(),
                        url: rel.url.clone(),
                        title: rel.title.clone(),
                        channel: if rel.subtitle.is_empty() {
                            artist.name.clone()
                        } else {
                            rel.subtitle.clone()
                        },
                        channel_url: Some(canonical.clone()),
                        duration: None,
                        duration_text: "Lanzamiento".into(),
                        thumbnail: if rel.thumbnail.is_empty() {
                            None
                        } else {
                            Some(rel.thumbnail.clone())
                        },
                        view_count: None,
                        live: false,
                        members_only: false,
                        result_type: Some("album".into()),
                        watched_percent: None,
                    });
                }
                let followed_id = runtime
                    .state
                    .read()
                    .ok()
                    .and_then(|s| {
                        s.channels.iter().find_map(|c| {
                            let cu = c.get("url").and_then(Value::as_str).unwrap_or("");
                            if cu.eq_ignore_ascii_case(&canonical) || cu.contains(&channel_id) {
                                c.get("id").and_then(Value::as_str).map(ToString::to_string)
                            } else {
                                None
                            }
                        })
                    });
                let tab_error = if items.is_empty() {
                    "Este canal no tiene perfil o canciones en YouTube Music.".to_string()
                } else {
                    String::new()
                };
                return Ok(json!({
                    "name": artist.name,
                    "url": canonical,
                    "channelId": channel_id,
                    "handle": "",
                    "avatar": artist.thumbnail,
                    "subscriberCount": null,
                    "tab": "music",
                    "page": 1,
                    "pageSize": page_size,
                    "hasMore": false,
                    "items": items,
                    "tabError": tab_error,
                    "alreadyFollowed": followed_id.is_some(),
                    "followedChannelId": followed_id,
                }));
            }
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
        ("POST", "/api/playlists/import") => {
            let url = body
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let mode = body
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("copy")
                .to_string();
            if url.is_empty() {
                return Err("URL de playlist requerida".into());
            }
            let pid = extract_playlist_id(&url);
            let source = if url.contains("music.youtube.com") {
                "music"
            } else {
                "youtube"
            };
            let items = resolve_playlist_or_mix_for_api(runtime, &url, 150).await?;
            if items.is_empty() {
                return Err("No se encontraron videos o canciones en la playlist/mix".into());
            }
            let name = body
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    if pid.starts_with("RD") {
                        let first_title = items
                            .first()
                            .and_then(|i| i.get("title").and_then(Value::as_str))
                            .unwrap_or("Mix de YouTube");
                        format!("Mix - {first_title}")
                    } else if !pid.is_empty() {
                        format!("Playlist {pid}")
                    } else {
                        "Playlist importada".to_string()
                    }
                });
            let created = import_playlist_items(
                &runtime.state,
                &name,
                source,
                &url,
                &pid,
                &mode,
                items,
            )
            .map_err(|e| e.to_string())?;
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
        ("POST", "/api/recommendations/more") => {
            let cfg = runtime.config.snapshot();
            let configured = if cfg.cookies_file.trim().is_empty() {
                None
            } else {
                Some(cfg.cookies_file.as_str())
            };
            let jar = runtime.cookie_store().load_jar(configured).ok();
            let auth_user = runtime
                .state
                .read()
                .map(|s| s.music_auth_user)
                .unwrap_or(0);
            let items = runtime
                .youtube
                .fetch_recommendations_more_with_cookies_and_user(
                    jar.as_ref(),
                    auth_user,
                    Some(&runtime.state),
                )
                .await
                .map_err(|e| e.to_string())?;
            let last_sync = runtime
                .state
                .read()
                .map(|s| s.recommendations_last_sync)
                .unwrap_or_default();
            Ok(json!({ "ok": true, "items": items, "lastSync": last_sync }))
        }
        ("GET", "/api/music/recommendations") => {
            let items = if let Ok(ytm) = runtime.build_ytmusic_client() {
                ytm.enrich_stored_recommendations(&runtime.state)
                    .await
                    .unwrap_or_else(|_| {
                        runtime
                            .state
                            .read()
                            .map(|s| s.music_recommendations)
                            .unwrap_or_default()
                    })
            } else {
                runtime
                    .state
                    .read()
                    .map_err(|e| e.to_string())?
                    .music_recommendations
            };
            let last_sync = runtime
                .state
                .read()
                .map_err(|e| e.to_string())?
                .music_recom_last_sync;
            Ok(json!({
                "items": items,
                "lastSync": last_sync,
            }))
        }
        ("POST", "/api/music/recommendations/refresh") => runtime
            .autosync
            .refresh_section(SyncSection::MusicRecommendations)
            .await
            .map_err(|e| e.to_string()),
        ("POST", "/api/music/recommendations/more") => {
            let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
            let items = ytm
                .fetch_recommendations_more(Some(&runtime.state))
                .await
                .map_err(|e| e.to_string())?;
            let last_sync = runtime
                .state
                .read()
                .map(|s| s.music_recom_last_sync)
                .unwrap_or_default();
            Ok(json!({ "ok": true, "items": items, "lastSync": last_sync }))
        }
        ("POST", "/api/youtube/watch-report") => {
            let video_ref = body
                .get("videoId")
                .or_else(|| body.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let event_type = body
                .get("eventType")
                .and_then(Value::as_str)
                .unwrap_or("start");
            let current_time = body
                .get("currentTime")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let start_time = body.get("startTime").and_then(Value::as_f64);
            let duration = body.get("duration").and_then(Value::as_f64);
            let cpn = body.get("cpn").and_then(Value::as_str).unwrap_or("");
            let cfg = runtime.config.snapshot();
            let configured = if cfg.cookies_file.trim().is_empty() {
                None
            } else {
                Some(cfg.cookies_file.as_str())
            };
            let jar = runtime.cookie_store().load_jar(configured).ok();
            let auth_user = runtime
                .state
                .read()
                .map(|s| s.music_auth_user)
                .unwrap_or(0);
            let reported = runtime
                .youtube
                .report_watch_event(
                    video_ref,
                    event_type,
                    current_time,
                    start_time,
                    duration,
                    cpn,
                    jar.as_ref(),
                    auth_user,
                )
                .await
                .unwrap_or(false);
            Ok(json!({ "ok": true, "reported": reported }))
        }
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
            let force_import = body
                .get("forceImport")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let ytdlp_bin = runtime.player.resolve_ytdlp_bin();
            let ytdlp_ok = playzi_core::player::binary_exists(&ytdlp_bin);
            let mut imported_from: Option<String> = None;

            if force_import && ytdlp_ok {
                imported_from = try_import_cookies_from_system_browser(runtime).await;
            }

            let ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
            let mut status = ytm
                .check_auth(Some(&runtime.state))
                .await
                .map_err(|e| e.to_string())?;
            let has_saved_cookies = runtime
                .cookie_store()
                .managed_browser_cookies_path()
                .exists()
                || !runtime.config.snapshot().cookies_file.trim().is_empty();
            if status.state != "ok"
                && imported_from.is_none()
                && ytdlp_ok
                && (force_import || has_saved_cookies)
            {
                imported_from = try_import_cookies_from_system_browser(runtime).await;
                if imported_from.is_some() {
                    let refreshed_ytm =
                        runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
                    status = refreshed_ytm
                        .check_auth(Some(&runtime.state))
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }

            let accounts = if status.state == "ok" {
                let refreshed_ytm = runtime.build_ytmusic_client().map_err(|e| e.to_string())?;
                refreshed_ytm.list_signed_in_accounts().await
            } else {
                Vec::new()
            };
            let active_auth_user = runtime
                .state
                .read()
                .map(|s| s.music_auth_user)
                .unwrap_or(0);
            let error_msg = if status.state != "ok" && !ytdlp_ok {
                Some("No se encontró yt-dlp instalado en el sistema. Instala yt-dlp para sincronizar cookies del navegador.")
            } else {
                None
            };
            Ok(json!({
                "ok": status.state == "ok",
                "authState": status.state,
                "accountName": status.account_name,
                "checkedAt": status.checked_at,
                "browser": imported_from,
                "accounts": accounts,
                "activeAuthUser": active_auth_user,
                "error": error_msg,
                "task": Value::Null,
            }))
        }
        ("POST", "/api/music/accounts/select") => {
            let auth_user = body
                .get("authUser")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .min(10) as u8;
            runtime
                .state
                .update(|s| {
                    s.music_auth_user = auth_user;
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
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
                "activeAuthUser": auth_user,
            }))
        }
        ("POST", "/api/music/logout") => {
            AUTH_POLL_GENERATION.fetch_add(1, Ordering::SeqCst);
            let cookie_store = runtime.cookie_store();
            let _ = fs::remove_file(cookie_store.managed_browser_cookies_path());
            let _ = cookie_store.invalidate_cache();
            runtime
                .state
                .update(|s| {
                    s.music_auth_state = "expired".into();
                    s.music_account_name.clear();
                    s.music_auth_checked_at.clear();
                    s.music_auth_user = 0;
                    s.music_recommendations.clear();
                    s.music_recom_last_sync.clear();
                    s.music_playlists.clear();
                    s.music_playlists_last_sync.clear();
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            Ok(json!({
                "ok": false,
                "authState": "expired",
                "accountName": "",
                "checkedAt": "",
                "activeAuthUser": 0,
            }))
        }
        ("POST", "/api/music/login") => {
            if let Some(cb) = open_auth {
                cb()?;
            }
            let browser_label = candidates_for_config(&runtime.config.snapshot().cookies_browser)
                .into_iter()
                .next()
                .map(|c| c.label);
            Ok(json!({
                "ok": true,
                "mode": "external-browser",
                "browser": browser_label,
                "task": Value::Null,
            }))
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
                .trim()
                .to_string();
            let limit = body
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(100) as usize;
            let items = resolve_playlist_or_mix_for_api(runtime, &url, limit).await?;
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
            runtime
                .social
                .send_profile_update(&prof.display_name, &prof.google_account_name)
                .await;
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
            let current_title = snap
                .playback
                .current_index
                .and_then(|idx| snap.queue.get(idx))
                .map(|q| q.title.clone())
                .unwrap_or_default();
            let room_info = playzi_core::social::PublishedRoomInfo {
                room_code: snap.room_code.clone(),
                invite_url: if !snap.public_ws_url.is_empty() {
                    snap.public_ws_url.clone()
                } else {
                    snap.invite_url.clone()
                },
                room_access: snap.room_access.clone(),
                participants_count: snap.participants.len(),
                current_title,
            };
            runtime.social.publish_room_presence(Some(room_info)).await;
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
            runtime.social.force_clear_room_presence().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/party/leave") => {
            let snap = runtime.party.leave_or_stop().await;
            runtime.social.force_clear_room_presence().await;
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
        ("POST", "/api/party/permissions") => {
            let req: ParticipantPermissionsUpdateRequest = serde_json::from_value(body)
                .map_err(|e| format!("Invalid permissions update: {e}"))?;
            let snap = runtime
                .party
                .update_participant_permissions(req)
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
        ("POST", "/api/party/room-access") => {
            let room_access = body
                .get("roomAccess")
                .and_then(Value::as_str)
                .unwrap_or("request")
                .to_string();
            let snap = runtime
                .party
                .set_room_access(room_access)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("GET", "/api/social/status") => {
            let snap = runtime.social.status().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/sync") => {
            runtime.social.request_sync().await;
            let snap = runtime.social.status().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/config") => {
            let hub_url = body
                .get("socialHubUrl")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let _ = runtime
                .config
                .update(ConfigUpdate {
                    social_hub_url: Some(hub_url),
                    ..Default::default()
                })
                .map_err(|e| e.to_string())?;
            runtime.social.reconnect();
            let snap = runtime.social.status().await;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/friends/request") => {
            let user_id = body.get("userId").and_then(Value::as_str).unwrap_or("");
            let snap = runtime
                .social
                .send_friend_request(user_id)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/friends/respond") => {
            let request_id = body.get("requestId").and_then(Value::as_str).unwrap_or("");
            let accept = body.get("accept").and_then(Value::as_bool).unwrap_or(false);
            let snap = runtime
                .social
                .respond_friend_request(request_id, accept)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/friends/cancel") => {
            let request_id = body.get("requestId").and_then(Value::as_str).unwrap_or("");
            let snap = runtime
                .social
                .cancel_friend_request(request_id)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/friends/remove") => {
            let user_id = body.get("userId").and_then(Value::as_str).unwrap_or("");
            let snap = runtime
                .social
                .remove_friend(user_id)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/room/invite") => {
            let user_id = body.get("userId").and_then(Value::as_str).unwrap_or("");
            let party_snap = runtime.party.status().await;
            let room_code = body
                .get("roomCode")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&party_snap.room_code);
            let fallback_url = if !party_snap.public_ws_url.is_empty() {
                &party_snap.public_ws_url
            } else {
                &party_snap.invite_url
            };
            let invite_url = body
                .get("inviteUrl")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(fallback_url);
            runtime
                .social
                .invite_friend_to_room(user_id, room_code, invite_url)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/social/room/join-request") => {
            let user_id = body.get("userId").and_then(Value::as_str).unwrap_or("");
            let room_code = body.get("roomCode").and_then(Value::as_str);
            runtime
                .social
                .request_join_friend_room(user_id, room_code)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true }))
        }
        ("POST", "/api/social/room/join-respond") => {
            let request_id = body.get("requestId").and_then(Value::as_str).unwrap_or("");
            let accept = body.get("accept").and_then(Value::as_bool).unwrap_or(false);
            let party_snap = runtime.party.status().await;
            let is_active_host =
                party_snap.active && party_snap.role == playzi_core::party::PartyRole::Host;
            let invite_url = if !is_active_host {
                None
            } else if !party_snap.public_ws_url.is_empty() {
                Some(party_snap.public_ws_url.as_str())
            } else if !party_snap.invite_url.is_empty() {
                Some(party_snap.invite_url.as_str())
            } else {
                None
            };
            if accept && (!is_active_host || invite_url.is_none()) {
                let _ = runtime
                    .social
                    .respond_join_request(request_id, false, None)
                    .await;
                return Err(
                    "No tienes una sesión activa para aceptar esta solicitud.".to_string(),
                );
            }
            let snap = runtime
                .social
                .respond_join_request(request_id, accept, invite_url)
                .await
                .map_err(|e| e.to_string())?;
            serde_json::to_value(snap).map_err(|e| e.to_string())
        }
        ("POST", "/api/social/room/invite/dismiss") => {
            let invite_id = body.get("inviteId").and_then(Value::as_str).unwrap_or("");
            let snap = runtime.social.dismiss_room_invite(invite_id).await;
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
        if method == "PATCH" && !rest.contains('/') {
            let name = body.get("name").and_then(Value::as_str).unwrap_or("");
            let updated = rename_playlist(&runtime.state, rest, name).map_err(|e| e.to_string())?;
            return serde_json::to_value(updated).map_err(|e| e.to_string());
        }
        if let Some((pl_id, sub)) = rest.split_once('/') {
            match (method, sub) {
                ("POST", "rename") => {
                    let name = body.get("name").and_then(Value::as_str).unwrap_or("");
                    let updated = rename_playlist(&runtime.state, pl_id, name)
                        .map_err(|e| e.to_string())?;
                    return serde_json::to_value(updated).map_err(|e| e.to_string());
                }
                ("POST", "sync") => {
                    let current = get_playlist(&runtime.state, pl_id)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "Playlist no encontrada".to_string())?;
                    if current.import_mode != "linked" || current.external_url.trim().is_empty() {
                        return Err("Solo las playlists vinculadas se pueden sincronizar".into());
                    }
                    let items =
                        resolve_playlist_or_mix_for_api(runtime, &current.external_url, 150)
                            .await?;
                    let pid = extract_playlist_id(&current.external_url);
                    let source = if current.external_url.contains("music.youtube.com") {
                        "music"
                    } else {
                        "youtube"
                    };
                    let synced =
                        sync_playlist_items(&runtime.state, pl_id, None, source, &pid, items)
                            .map_err(|e| e.to_string())?;
                    return serde_json::to_value(synced).map_err(|e| e.to_string());
                }
                ("POST", "items") => {
                    let raw_url = body.get("url").and_then(Value::as_str).unwrap_or("").trim();
                    let is_playlist_url = raw_url.contains("list=")
                        && (!raw_url.contains("watch?v=") || extract_playlist_id(raw_url).starts_with("RD"));
                    if is_playlist_url {
                        let resolved = resolve_playlist_or_mix_for_api(runtime, raw_url, 100).await?;
                        if resolved.is_empty() {
                            return Err("No se encontraron videos o canciones en la playlist/mix".into());
                        }
                        let count = resolved.len();
                        for entry in resolved {
                            let _ = add_item(&runtime.state, pl_id, entry);
                        }
                        return Ok(json!({ "ok": true, "addedCount": count }));
                    }
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

async fn resolve_playlist_or_mix_for_api(
    runtime: &AppRuntime,
    url: &str,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let clean_url = url.trim();
    if clean_url.is_empty() {
        return Err("URL de playlist o mix requerida".into());
    }
    let pid = extract_playlist_id(clean_url);
    let is_music = clean_url.contains("music.youtube.com")
        || pid.starts_with("RDCLAK")
        || pid.starts_with("VL")
        || pid.starts_with("OLAK");
    let is_mix = pid.starts_with("RD");

    // 1. Try YouTube Music InnerTube directly for YT Music playlists/mixes.
    if is_music && !pid.is_empty() {
        if let Ok(ytm) = runtime.build_ytmusic_client() {
            if let Ok(ytm_items) = ytm
                .fetch_mix_or_playlist_items(clean_url, limit.min(100))
                .await
            {
                if ytm_items.len() > 1 || (!is_mix && !ytm_items.is_empty()) {
                    let vals = ytm_items
                        .into_iter()
                        .filter_map(|i| serde_json::to_value(i).ok())
                        .collect::<Vec<_>>();
                    if !vals.is_empty() {
                        return Ok(vals);
                    }
                }
            }
        }
    }

    // 2. Try YouTube InnerTube /next for RD mixes (now derives seed videoId from RD<vid>).
    if is_mix {
        let snap = runtime.config.snapshot();
        let configured = if snap.cookies_file.trim().is_empty() {
            None
        } else {
            Some(snap.cookies_file.as_str())
        };
        let jar = runtime.cookie_store().load_jar(configured).ok();
        if let Ok(mix_items) = runtime
            .youtube
            .fetch_mix_items_with_cookies(clean_url, limit.min(50), jar.as_ref())
            .await
        {
            if mix_items.len() > 1 {
                let vals = mix_items
                    .into_iter()
                    .filter_map(|i| serde_json::to_value(i).ok())
                    .collect::<Vec<_>>();
                if !vals.is_empty() {
                    return Ok(vals);
                }
            }
        }
    }

    // 3. Fallback to yt-dlp --flat-playlist.
    let mut vals: Vec<Value> = match runtime.player.resolve_playlist(clean_url, limit).await {
        Ok(items) => items
            .into_iter()
            .filter_map(|i| serde_json::to_value(i).ok())
            .collect(),
        Err(err) => {
            if !is_mix {
                return Err(err.to_string());
            }
            Vec::new()
        }
    };

    // 4. For mixes, never settle for 0 or 1 item — expand via radio from the seed video.
    if is_mix && vals.len() <= 1 {
        let seed_url = vals
            .first()
            .and_then(|v| v.get("url").and_then(Value::as_str))
            .map(ToString::to_string)
            .or_else(|| {
                extract_video_id(clean_url)
                    .or_else(|| playzi_core::youtube::extract_seed_video_from_playlist_id(&pid))
                    .map(|vid| format!("https://www.youtube.com/watch?v={vid}"))
            });
        if let Some(seed) = seed_url {
            if let Ok(radio_items) = runtime.youtube.fetch_radio_items(&seed, limit.min(35)).await {
                let mut seen: std::collections::HashSet<String> = vals
                    .iter()
                    .filter_map(|v| v.get("url").and_then(Value::as_str).map(ToString::to_string))
                    .collect();
                for item in radio_items {
                    if seen.insert(item.url.clone()) {
                        if let Ok(val) = serde_json::to_value(item) {
                            vals.push(val);
                        }
                    }
                }
            }
        }
    }

    if vals.is_empty() {
        return Err("No se encontraron videos o canciones en la playlist/mix".into());
    }
    Ok(vals)
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
