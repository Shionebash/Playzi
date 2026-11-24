pub mod commands;
pub mod media_controls;
pub mod runtime;

use std::time::Duration;

use tauri::{Emitter, Manager};
use url::Url;

use crate::runtime::{AppRuntime, resolve_playzi_root};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = resolve_playzi_root();
    let app_runtime = AppRuntime::initialize(root).expect("failed to initialize Playzi core runtime");
    let thumbs_for_proto = app_runtime.thumbs.clone();
    let stream_client = playzi_core::player::build_stream_proxy_client()
        .expect("failed to initialize stream proxy HTTP client");

    tauri::Builder::default()
        .manage(app_runtime.clone())
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
                    match (video_url, audio_url) {
                        (Some(v), Some(a)) if !v.is_empty() && !a.is_empty() => {
                            let body = playzi_core::player::build_hls_master_playlist(
                                &v,
                                &a,
                                &proxy_prefix,
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
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = player_rx.recv().await {
                    if let Some(mc) = os_media.as_mut() {
                        mc.sync_state(&ev.state);
                    }
                    let _ = app_player.emit("player-state", &ev);
                }
            });

            // Bridge Watch Party events -> Tauri events "party-state" and "party-notice"
            let mut party_rx = runtime.party.subscribe();
            let app_party = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while let Ok(ev) = party_rx.recv().await {
                    match ev {
                        playzi_core::party::PartyEvent::State { snapshot } => {
                            let _ = app_party.emit("party-state", &snapshot);
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

            Ok(())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Playzi desktop application");
}
