pub mod commands;
pub mod media_controls;
pub mod runtime;
pub mod updater;

use std::time::Duration;

use tauri::{Emitter, Manager};
use url::Url;

use crate::runtime::{AppRuntime, resolve_playzi_root};
use crate::updater::DesktopUpdater;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = resolve_playzi_root();
    let app_runtime = AppRuntime::initialize(root).expect("failed to initialize Playzi core runtime");
    let desktop_updater = DesktopUpdater::new();
    let thumbs_for_proto = app_runtime.thumbs.clone();
    let stream_client = playzi_core::player::build_stream_proxy_client()
        .expect("failed to initialize stream proxy HTTP client");

    tauri::Builder::default()
        .manage(app_runtime.clone())
        .manage(desktop_updater.clone())
        .register_asynchronous_uri_scheme_protocol("playzi-thumb", move |_ctx, request, responder| {
            let thumbs = thumbs_for_proto.clone();
            let req_uri = request.uri().to_string();
            tauri::async_runtime::spawn(async move {
                let target_url = Url::parse(&req_uri)
                    .ok()
                    .and_then(|u| {
                        u.query_pairs()
                            .find(|(k, _)| k == "url")
                            .map(|(_, v)| v.into_owned())
                    });

                let Some(url) = target_url else {
                    let resp = tauri::http::Response::builder()
                        .status(400)
                        .body(b"missing url parameter".to_vec())
                        .unwrap();
                    responder.respond(resp);
                    return;
                };

                match thumbs.get_or_fetch(&url).await {
                    Ok(cached) => {
                        let resp = tauri::http::Response::builder()
                            .status(200)
                            .header("Content-Type", cached.content_type)
                            .header("Cache-Control", "public, max-age=86400")
                            .header("Access-Control-Allow-Origin", "*")
                            .body(cached.bytes)
                            .unwrap();
                        responder.respond(resp);
                    }
                    Err(err) => {
                        let resp = tauri::http::Response::builder()
                            .status(502)
                            .body(err.to_string().into_bytes())
                            .unwrap();
                        responder.respond(resp);
                    }
                }
            });
        })
        .register_asynchronous_uri_scheme_protocol("playzi-stream", move |_ctx, request, responder| {
            let client = stream_client.clone();
            let method = request.method().clone();
            let req_uri = request.uri().to_string();
            let range_header = request
                .headers()
                .get("range")
                .and_then(|v| v.to_str().ok())
                .map(ToString::to_string);

            tauri::async_runtime::spawn(async move {
                if method == tauri::http::Method::OPTIONS {
                    let resp = tauri::http::Response::builder()
                        .status(204)
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
                        .header("Access-Control-Allow-Headers", "Range, Content-Type")
                        .body(Vec::new())
                        .unwrap();
                    responder.respond(resp);
                    return;
                }

                let parsed_req = Url::parse(&req_uri).ok();
                // On Windows WebView2, custom URI schemes must be requested via http://<scheme>.localhost/
                // even though wry normalizes incoming request.uri() to <scheme>://localhost/.
                #[cfg(windows)]
                let proxy_prefix = "http://playzi-stream.localhost/".to_string();
                #[cfg(not(windows))]
                let proxy_prefix = "playzi-stream://localhost/".to_string();

                if let Some(u) = parsed_req.as_ref().filter(|u| u.path() == "/master.m3u8") {
                    let video_url = u
                        .query_pairs()
                        .find(|(k, _)| k == "v")
                        .map(|(_, v)| v.into_owned());
                    let audio_url = u
                        .query_pairs()
                        .find(|(k, _)| k == "a")
                        .map(|(_, v)| v.into_owned());
                    let meta = playzi_core::player::HlsMasterMeta {
                        vcodec: u
                            .query_pairs()
                            .find(|(k, _)| k == "vc")
                            .map(|(_, v)| v.into_owned()),
                        acodec: u
                            .query_pairs()
                            .find(|(k, _)| k == "ac")
                            .map(|(_, v)| v.into_owned()),
                        width: u
                            .query_pairs()
                            .find(|(k, _)| k == "w")
                            .and_then(|(_, v)| v.parse::<u32>().ok()),
                        height: u
                            .query_pairs()
                            .find(|(k, _)| k == "h")
                            .and_then(|(_, v)| v.parse::<u32>().ok()),
                        bandwidth: u
                            .query_pairs()
                            .find(|(k, _)| k == "bw")
                            .and_then(|(_, v)| v.parse::<u64>().ok()),
                    };
                    match (video_url, audio_url) {
                        (Some(v), Some(a)) if !v.is_empty() && !a.is_empty() => {
                            let body = playzi_core::player::build_hls_master_playlist_with_meta(
                                &v,
                                &a,
                                &proxy_prefix,
                                &meta,
                            )
                            .into_bytes();
                            let resp = tauri::http::Response::builder()
                                .status(200)
                                .header("Content-Type", "application/vnd.apple.mpegurl")
                                .header("Content-Length", body.len().to_string())
                                .header("Access-Control-Allow-Origin", "*")
                                .header("Access-Control-Allow-Headers", "Range, Content-Type")
                                .body(body)
                                .unwrap();
                            responder.respond(resp);
                        }
                        _ => {
                            let resp = tauri::http::Response::builder()
                                .status(400)
                                .header("Access-Control-Allow-Origin", "*")
                                .body(b"missing v or a parameter".to_vec())
                                .unwrap();
                            responder.respond(resp);
                        }
                    }
                    return;
                }

                let target_url = parsed_req.as_ref().and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "url")
                        .map(|(_, v)| v.into_owned())
                });

                let Some(url) = target_url else {
                    let resp = tauri::http::Response::builder()
                        .status(400)
                        .header("Access-Control-Allow-Origin", "*")
                        .body(b"missing url parameter".to_vec())
                        .unwrap();
                    responder.respond(resp);
                    return;
                };

                match playzi_core::player::fetch_proxied_stream(
                    &client,
                    &url,
                    range_header.as_deref(),
                    &proxy_prefix,
                )
                .await
                {
                    Ok(proxied) => {
                        let mut builder = tauri::http::Response::builder()
                            .status(proxied.status)
                            .header("Content-Type", proxied.content_type)
                            .header("Content-Length", proxied.bytes.len().to_string())
                            .header("Access-Control-Allow-Origin", "*")
                            .header("Access-Control-Allow-Headers", "Range, Content-Type")
                            .header(
                                "Access-Control-Expose-Headers",
                                "Content-Length, Content-Range, Accept-Ranges",
                            );
                        if let Some(cr) = proxied.content_range {
                            builder = builder.header("Content-Range", cr);
                        }
                        if let Some(ar) = proxied.accept_ranges {
                            builder = builder.header("Accept-Ranges", ar);
                        }
                        let resp = builder.body(proxied.bytes).unwrap();
                        responder.respond(resp);
                    }
                    Err(err) => {
                        let resp = tauri::http::Response::builder()
                            .status(502)
                            .header("Access-Control-Allow-Origin", "*")
                            .body(err.to_string().into_bytes())
                            .unwrap();
                        responder.respond(resp);
                    }
                }
            });
        })
        .setup(|app| {
            let runtime = app.state::<AppRuntime>().inner().clone();

            // Start periodic background auto-sync scheduler inside Tauri's Tokio runtime
            let autosync = runtime.autosync.clone();
            tauri::async_runtime::spawn(async move {
                autosync.start_background(Duration::from_secs(3), Duration::from_secs(60));
            });

            // Bridge download progress broadcast events -> native Tauri event "download-progress"
            let mut dl_rx = runtime.downloads.subscribe();
            let app_dl = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = dl_rx.recv().await {
                    let _ = app_dl.emit("download-progress", &ev);
                }
            });

            // Bridge auto-sync status broadcast events -> native Tauri event "sync-status"
            let mut sync_rx = runtime.autosync.subscribe();
            let app_sync = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = sync_rx.recv().await {
                    let _ = app_sync.emit("sync-status", &ev);
                }
            });

            // Bridge native player state events -> Tauri event "player-state" + OS SMTC/MPRIS
            let main_hwnd = app
                .get_webview_window("main")
                .and_then(|w| media_controls::window_hwnd_isize(&w));
            let mut os_media =
                media_controls::OsMediaControls::attach(main_hwnd, runtime.player.clone()).ok();

            let mut player_rx = runtime.player.subscribe();
            let app_player = app.handle().clone();
            let discord_for_player = runtime.discord.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = player_rx.recv().await {
                    if let Some(mc) = os_media.as_mut() {
                        mc.sync_state(&ev.state);
                    }
                    discord_for_player.update_native_player(&ev.state).await;
                    let _ = app_player.emit("player-state", &ev);
                }
            });

            // Start Social Hub background WebSocket client
            let social_bg = runtime.social.clone();
            tauri::async_runtime::spawn(async move {
                social_bg.start_background();
            });

            // Start Discord Rich Presence & IPC background client
            let discord_bg = runtime.discord.clone();
            tauri::async_runtime::spawn(async move {
                discord_bg.start_background();
            });

            // Bridge Watch Party events -> Tauri events "party-state" and "party-notice"
            // Also sync host room presence to Social Hub and Discord automatically.
            let mut party_rx = runtime.party.subscribe();
            let app_party = app.handle().clone();
            let social_for_party = runtime.social.clone();
            let discord_for_party = runtime.discord.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = party_rx.recv().await {
                    match ev {
                        playzi_core::party::PartyEvent::State { snapshot } => {
                            let _ = app_party.emit("party-state", &snapshot);
                            discord_for_party.update_party(snapshot.clone()).await;
                            if snapshot.active
                                && snapshot.role == playzi_core::party::PartyRole::Host
                            {
                                let current_title = snapshot
                                    .playback
                                    .current_index
                                    .and_then(|idx| snapshot.queue.get(idx))
                                    .map(|q| q.title.clone())
                                    .unwrap_or_default();
                                let room_info = playzi_core::social::PublishedRoomInfo {
                                    room_code: snapshot.room_code.clone(),
                                    invite_url: if !snapshot.public_ws_url.is_empty() {
                                        snapshot.public_ws_url.clone()
                                    } else {
                                        snapshot.invite_url.clone()
                                    },
                                    room_access: snapshot.room_access.clone(),
                                    participants_count: snapshot.participants.len(),
                                    current_title,
                                };
                                social_for_party.publish_room_presence(Some(room_info)).await;
                            } else {
                                social_for_party.publish_room_presence(None).await;
                            }
                        }
                        playzi_core::party::PartyEvent::Notice { level, message } => {
                            let _ = app_party.emit(
                                "party-notice",
                                serde_json::json!({ "level": level, "message": message }),
                            );
                        }
                    }
                }
            });

            // Bridge Discord IPC events -> Tauri events + automatic room join on Discord invite
            let mut discord_rx = runtime.discord.subscribe();
            let app_discord = app.handle().clone();
            let party_for_discord = runtime.party.clone();
            let social_for_discord = runtime.social.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = discord_rx.recv().await {
                    match ev {
                        playzi_core::discord::DiscordEvent::State { snapshot } => {
                            let _ = app_discord.emit("discord-state", &snapshot);
                        }
                        playzi_core::discord::DiscordEvent::Notice { level, message } => {
                            let _ = app_discord.emit(
                                "discord-notice",
                                serde_json::json!({ "level": level, "message": message }),
                            );
                        }
                        playzi_core::discord::DiscordEvent::JoinRoom {
                            room_code,
                            invite_url,
                        } => {
                            if let Some(win) = app_discord.get_webview_window("main") {
                                let _ = win.unminimize();
                                let _ = win.set_focus();
                            }
                            let req = playzi_core::party::JoinSessionRequest {
                                invite: invite_url,
                                room_code: Some(room_code.clone()),
                            };
                            match party_for_discord.join_session(req).await {
                                Ok(_) => {
                                    social_for_discord.force_clear_room_presence().await;
                                    let _ = app_discord.emit(
                                        "discord-notice",
                                        serde_json::json!({
                                            "level": "ok",
                                            "message": format!("Conectado a la sala {room_code} desde Discord."),
                                        }),
                                    );
                                }
                                Err(err) => {
                                    let _ = app_discord.emit(
                                        "discord-notice",
                                        serde_json::json!({
                                            "level": "error",
                                            "message": err.to_string(),
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            });

            // Bridge Social Hub events -> Tauri events + automatic room join on host approval
            let mut social_rx = runtime.social.subscribe();
            let app_social = app.handle().clone();
            let party_for_social = runtime.party.clone();
            let social_for_join = runtime.social.clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = social_rx.recv().await {
                    match ev {
                        playzi_core::social::SocialEvent::State { snapshot } => {
                            let _ = app_social.emit("social-state", &snapshot);
                        }
                        playzi_core::social::SocialEvent::Notice { level, message } => {
                            let _ = app_social.emit(
                                "social-notice",
                                serde_json::json!({ "level": level, "message": message }),
                            );
                        }
                        playzi_core::social::SocialEvent::JoinApproved {
                            host_user_id,
                            host_display_name,
                            room_code,
                            invite_url,
                        } => {
                            let _ = app_social.emit(
                                "social-notice",
                                serde_json::json!({
                                    "level": "ok",
                                    "message": format!("Acceso aprobado por {host_display_name}. Conectando a sala {room_code}...")
                                }),
                            );
                            let _ = app_social.emit(
                                "social-join-approved",
                                serde_json::json!({
                                    "hostUserId": host_user_id,
                                    "hostDisplayName": host_display_name,
                                    "roomCode": room_code,
                                    "inviteUrl": invite_url,
                                }),
                            );
                            let req = playzi_core::party::JoinSessionRequest {
                                invite: invite_url,
                                room_code: Some(room_code),
                            };
                            match party_for_social.join_session(req).await {
                                Ok(_) => {
                                    social_for_join.force_clear_room_presence().await;
                                }
                                Err(err) => {
                                    let _ = app_social.emit(
                                        "social-notice",
                                        serde_json::json!({
                                            "level": "error",
                                            "message": err.to_string(),
                                        }),
                                    );
                                }
                            }
                        }
                    }
                }
            });

            // Automatic background release check & self-update on startup
            let disable_auto_update = std::env::var("PLAYZI_DISABLE_AUTO_UPDATE")
                .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
                .unwrap_or(false);
            if !disable_auto_update {
                let updater = app.state::<DesktopUpdater>().inner().clone();
                let app_update = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let _ = updater.check_and_install(app_update, true, false).await;
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && let tauri::WindowEvent::CloseRequested { api, .. } = event
            {
                use std::sync::atomic::{AtomicBool, Ordering};
                static CLOSING: AtomicBool = AtomicBool::new(false);
                if !CLOSING.swap(true, Ordering::SeqCst) {
                    api.prevent_close();
                    let app_handle = window.app_handle().clone();
                    let win = window.clone();
                    tauri::async_runtime::spawn(async move {
                        let runtime = app_handle.state::<AppRuntime>().inner().clone();
                        let _ = tokio::time::timeout(Duration::from_secs(3), async {
                            runtime.discord.shutdown().await;
                            runtime.party.leave_or_stop().await;
                            runtime.social.shutdown_notify().await;
                        })
                        .await;
                        let _ = win.destroy();
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::api_request,
            commands::get_bootstrap,
            commands::get_state_summary,
            commands::open_auth_window,
            commands::save_browser_cookies,
            commands::player_status,
            commands::player_control,
            commands::get_main_window_hwnd,
            commands::get_update_status,
            commands::check_for_updates,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Playzi desktop application");
}
