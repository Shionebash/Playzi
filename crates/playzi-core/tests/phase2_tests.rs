use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use playzi_core::autosync::{AutoSyncManager, SyncSection};
use playzi_core::config::ConfigStore;
use playzi_core::cookies::{
    CookieJar, CookieStore, compute_sapisidhash, looks_like_netscape_cookie_file,
};
use playzi_core::downloads::{
    DownloadManager, DownloadRequest, parse_progress_line, quality_to_height, safe_name,
    source_for_url,
};
use playzi_core::state::StateStore;
use playzi_core::thumbs::{ThumbnailCache, is_allowed_thumbnail_host};
use playzi_core::youtube::{
    YouTubeClient, channel_videos_url, delete_channel, extract_video_id, format_duration,
    is_youtube_url, list_channels, list_feed, parse_duration_text, resolve_channel_base,
};
use playzi_core::ytmusic::YtMusicClient;

/// Spawns a tiny local HTTP/1.1 mock server that routes requests using `handler(path, headers, body)`.
async fn spawn_mock_server<F>(handler: F) -> String
where
    F: Fn(&str, &str, &str) -> (u16, &'static str, Vec<u8>) + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handler = Arc::new(handler);

    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let h = Arc::clone(&handler);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    return;
                }
                let raw = String::from_utf8_lossy(&buf[..n]);
                let mut sections = raw.splitn(2, "\r\n\r\n");
                let head = sections.next().unwrap_or("");
                let body = sections.next().unwrap_or("");
                let first_line = head.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");

                let (status, content_type, resp_body) = h(path, head, body);
                let reason = if status == 200 { "OK" } else { "ERROR" };
                let header = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(&resp_body).await;
            });
        }
    });

    format!("http://{addr}")
}

#[test]
fn cookies_netscape_parsing_sapisidhash_and_store_resolution() {
    let sample = "\u{feff}# Netscape HTTP Cookie File\n\
        # Comment line\n\
        .youtube.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tabc123xyz\n\
        #HttpOnly_.music.youtube.com\tTRUE\t/\tTRUE\t0\tLOGIN_INFO\tsession_token\n\
        .youtube.com\tTRUE\t/\tTRUE\t1000\tEXPIRED_COOKIE\told_val\n\
        example.com\tFALSE\t/\tFALSE\t4102444800\tOTHER\tval\n";

    let jar = CookieJar::parse(sample);
    assert_eq!(jar.len(), 4);
    assert_eq!(jar.sapisid(), Some("abc123xyz"));
    assert_eq!(jar.get("LOGIN_INFO"), Some("session_token"));
    assert_eq!(jar.get("EXPIRED_COOKIE"), None);

    let yt_header = jar.cookie_header_for_host("music.youtube.com");
    assert!(yt_header.contains("SAPISID=abc123xyz"));
    assert!(yt_header.contains("LOGIN_INFO=session_token"));
    assert!(!yt_header.contains("EXPIRED_COOKIE"));
    assert!(!yt_header.contains("OTHER=val"));

    let expected_hash = compute_sapisidhash("abc123xyz", "https://music.youtube.com", 1700000000);
    assert_eq!(
        jar.sapisidhash("https://music.youtube.com", 1700000000),
        Some(expected_hash.clone())
    );
    assert!(expected_hash.starts_with("SAPISIDHASH 1700000000_"));
    assert_eq!(expected_hash.len(), "SAPISIDHASH 1700000000_".len() + 40);

    let auth_headers = jar.ytmusic_auth_headers("https://music.youtube.com").unwrap();
    assert!(auth_headers.contains_key("Authorization"));
    assert!(auth_headers.contains_key("Cookie"));
    assert_eq!(
        auth_headers.get("x-origin").map(String::as_str),
        Some("https://music.youtube.com")
    );

    // Roundtrip and CookieStore resolution
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let private_dir = data_dir.join("private");
    fs::create_dir_all(&private_dir).unwrap();
    let managed = private_dir.join("playzi_browser_cookies.txt");
    fs::write(&managed, jar.to_netscape_string()).unwrap();
    assert!(looks_like_netscape_cookie_file(&managed));

    let store = CookieStore::new(dir.path(), &data_dir);
    let resolved = store.resolve_cookie_file(None).unwrap();
    assert_eq!(resolved, store.cache_path());
    assert!(store.is_cache_fresh(12));

    let loaded = store.load_jar(None).unwrap();
    assert_eq!(loaded.sapisid(), Some("abc123xyz"));

    store.invalidate_cache().unwrap();
    assert!(!store.cache_path().exists());
}

#[test]
fn cookies_ytmusic_headers_filter_host_and_ignore_google_or_third_party_cookies() {
    let sample = "# Netscape HTTP Cookie File\n\
        .youtube.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tyt_sapisid_real\n\
        .youtube.com\tTRUE\t/\tTRUE\t4102444800\tSID\tyt_sid\n\
        .google.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tgoogle_sapisid_other\n\
        .google.com\tTRUE\t/\tTRUE\t4102444800\tNID\tgoogle_nid\n\
        .claude.ai\tTRUE\t/\tTRUE\t4102444800\tsessionKey\tsecret_claude\n\
        account.proton.me\tFALSE\t/\tTRUE\t4102444800\tAUTH\tsecret_proton\n";

    let jar = CookieJar::parse(sample);
    assert_eq!(jar.sapisid(), Some("yt_sapisid_real"));
    assert_eq!(
        jar.sapisid_for_host("music.youtube.com"),
        Some("yt_sapisid_real")
    );

    let expected_hash =
        compute_sapisidhash("yt_sapisid_real", "https://music.youtube.com", 1700000000);
    assert_eq!(
        jar.sapisidhash("https://music.youtube.com", 1700000000),
        Some(expected_hash)
    );

    let headers = jar
        .ytmusic_auth_headers_for_user("https://music.youtube.com", 1)
        .unwrap();
    let cookie_hdr = headers.get("Cookie").unwrap();
    assert!(cookie_hdr.contains("SAPISID=yt_sapisid_real"));
    assert!(cookie_hdr.contains("SID=yt_sid"));
    assert!(!cookie_hdr.contains("google_sapisid_other"));
    assert!(!cookie_hdr.contains("google_nid"));
    assert!(!cookie_hdr.contains("secret_claude"));
    assert!(!cookie_hdr.contains("secret_proton"));
    assert_eq!(headers.get("X-Goog-AuthUser").map(String::as_str), Some("1"));

    let filtered = jar.filter_youtube_and_google();
    assert_eq!(filtered.len(), 4);
    assert_eq!(filtered.get("sessionKey"), None);
    assert_eq!(filtered.get("AUTH"), None);
}

#[tokio::test]
async fn youtube_search_metadata_radio_and_channels_with_mock_innertube() {
    let base_url = spawn_mock_server(|path, _headers, body| {
        if path.starts_with("/youtubei/v1/search") {
            let payload = json!({
                "contents": {
                    "twoColumnSearchResultsRenderer": {
                        "primaryContents": {
                            "sectionListRenderer": {
                                "contents": [{
                                    "itemSectionRenderer": {
                                        "contents": [{
                                            "videoRenderer": {
                                                "videoId": "dQw4w9WgXcQ",
                                                "title": {"runs": [{"text": "Mock Track"}]},
                                                "ownerText": {
                                                    "runs": [{
                                                        "text": "Mock Channel",
                                                        "navigationEndpoint": {
                                                            "browseEndpoint": {
                                                                "browseId": "UC1234567890123456789012"
                                                            }
                                                        }
                                                    }]
                                                },
                                                "lengthText": {"simpleText": "3:33"},
                                                "viewCountText": {"simpleText": "1,234,567 views"},
                                                "thumbnail": {"thumbnails": [{"url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"}]}
                                            }
                                        }]
                                    }
                                }]
                            }
                        }
                    }
                }
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/player") {
            let payload = json!({
                "videoDetails": {
                    "videoId": "dQw4w9WgXcQ",
                    "title": "Direct Video Title",
                    "author": "Direct Author",
                    "channelId": "UC1234567890123456789012",
                    "lengthSeconds": "213",
                    "viewCount": "999",
                    "isLiveContent": false,
                    "thumbnail": {"thumbnails": [{"url": "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"}]}
                }
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/next") {
            let payload = json!({
                "contents": {
                    "twoColumnWatchNextResults": {
                        "playlist": {
                            "playlist": {
                                "contents": [
                                    {"playlistPanelVideoRenderer": {"videoId": "dQw4w9WgXcQ", "title": {"simpleText": "Seed"}}},
                                    {"playlistPanelVideoRenderer": {"videoId": "abcdefghijk", "title": {"simpleText": "Radio Next"}, "shortBylineText": {"simpleText": "Artist B"}, "lengthText": {"simpleText": "4:05"}}}
                                ]
                            }
                        }
                    }
                }
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/navigation/resolve_url") {
            let payload = json!({
                "endpoint": {"browseEndpoint": {"browseId": "UC1234567890123456789012"}}
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/browse") {
            let is_playlists = body.contains("EglwbGF5bGlzdHPyBgQKAkIA");
            let payload = if is_playlists {
                json!({
                    "metadata": {"channelMetadataRenderer": {"title": "Mock Channel"}},
                    "contents": [{
                        "gridPlaylistRenderer": {
                            "playlistId": "PL12345",
                            "title": {"simpleText": "Best Hits"},
                            "videoCountText": {"simpleText": "12 videos"}
                        }
                    }]
                })
            } else {
                json!({
                    "metadata": {
                        "channelMetadataRenderer": {
                            "title": "Mock Channel",
                            "vanityChannelUrl": "https://www.youtube.com/@mockchannel",
                            "avatar": {"thumbnails": [{"url": "https://yt3.ggpht.com/avatar.jpg"}]}
                        }
                    },
                    "contents": [{
                        "gridVideoRenderer": {
                            "videoId": "xyz12345678",
                            "title": {"simpleText": "Channel Video 1"},
                            "lengthText": {"simpleText": "10:00"}
                        }
                    }]
                })
            };
            return (200, "application/json", payload.to_string().into_bytes());
        }
        (404, "text/plain", b"not found".to_vec())
    })
    .await;

    // Pure URL/helper tests
    assert!(is_youtube_url("https://youtu.be/dQw4w9WgXcQ"));
    assert_eq!(
        extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5s").as_deref(),
        Some("dQw4w9WgXcQ")
    );
    assert_eq!(
        resolve_channel_base("@playzi/videos").unwrap(),
        "https://www.youtube.com/@playzi"
    );
    assert_eq!(
        channel_videos_url("UC1234567890123456789012").unwrap(),
        "https://www.youtube.com/channel/UC1234567890123456789012/videos"
    );
    assert_eq!(format_duration(Some(213)), "3:33");
    assert_eq!(parse_duration_text("1:02:03"), Some(3723));

    let client = YouTubeClient::with_client(reqwest::Client::new(), base_url);

    let results = client.search("mock query", 10).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "dQw4w9WgXcQ");
    assert_eq!(results[0].duration, Some(213));
    assert_eq!(results[0].view_count, Some(1_234_567));

    let meta = client
        .metadata_for_url("https://youtu.be/dQw4w9WgXcQ")
        .await
        .unwrap();
    assert_eq!(meta.title, "Direct Video Title");

    let radio = client
        .fetch_radio_items("https://www.youtube.com/watch?v=dQw4w9WgXcQ", 10)
        .await
        .unwrap();
    assert_eq!(radio.len(), 1);
    assert_eq!(radio[0].id, "abcdefghijk");

    let dir = TempDir::new().unwrap();
    let store = StateStore::new(dir.path().join("state.json"));
    let added = client
        .add_channel(&store, "@mockchannel", Some("Mock"))
        .await
        .unwrap();
    let cid = added["id"].as_str().unwrap().to_string();
    assert_eq!(list_channels(&store).unwrap().len(), 1);
    assert_eq!(list_feed(&store).unwrap().len(), 1);

    let browsed = client
        .browse_channel("@mockchannel", "videos", 1, 10, Some(&store))
        .await
        .unwrap();
    assert!(browsed.already_followed);
    assert_eq!(browsed.followed_channel_id.as_deref(), Some(cid.as_str()));

    let pls = client
        .browse_channel("@mockchannel", "playlists", 1, 10, Some(&store))
        .await
        .unwrap();
    assert_eq!(pls.items.len(), 1);
    assert_eq!(pls.items[0].result_type.as_deref(), Some("playlist"));

    delete_channel(&store, &cid).unwrap();
    assert!(list_channels(&store).unwrap().is_empty());
    assert!(list_feed(&store).unwrap().is_empty());
}

#[tokio::test]
async fn ytmusic_innertube_auth_search_recommendations_and_playlists() {
    let saw_auth = Arc::new(AtomicUsize::new(0));
    let saw_auth_clone = Arc::clone(&saw_auth);

    let base_url = spawn_mock_server(move |path, headers, body| {
        if headers.to_ascii_lowercase().contains("authorization: sapisidhash ") {
            saw_auth_clone.fetch_add(1, Ordering::SeqCst);
        }
        if path.starts_with("/youtubei/v1/account/account_menu") {
            let payload = json!({
                "actions": [{
                    "openPopupAction": {
                        "popup": {
                            "multiPageMenuRenderer": {
                                "header": {
                                    "activeAccountHeaderRenderer": {
                                        "accountName": {"simpleText": "Playzi User"}
                                    }
                                }
                            }
                        }
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/search") || (path.starts_with("/youtubei/v1/browse") && body.contains("FEmusic_home")) {
            let payload = json!({
                "contents": [{
                    "musicResponsiveListItemRenderer": {
                        "playlistItemData": {"videoId": "song1234567"},
                        "flexColumns": [
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Lo-Fi Beats"}]}}},
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Chill Artist • 3:45"}]}}}
                        ],
                        "thumbnail": {"musicThumbnailRenderer": {"thumbnail": {"thumbnails": [{"url": "https://lh3.googleusercontent.com/cover.jpg"}]}}}
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/browse") && body.contains("FEmusic_liked_playlists") {
            let payload = json!({
                "contents": [{
                    "musicTwoRowItemRenderer": {
                        "title": {"runs": [{"text": "Focus Mix"}]},
                        "subtitle": {"runs": [{"text": "Playlist • 25 songs"}]},
                        "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPL_FOCUS"}},
                        "thumbnailOverlay": {"playlistId": "PL_FOCUS"}
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        (404, "text/plain", b"not found".to_vec())
    })
    .await;

    let jar = CookieJar::parse(
        ".youtube.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tsecret_sapisid\n\
         .youtube.com\tTRUE\t/\tTRUE\t4102444800\tSID\tsecret_sid\n",
    );
    let client = YtMusicClient::with_client(reqwest::Client::new(), base_url, Some(jar));

    let dir = TempDir::new().unwrap();
    let store = StateStore::new(dir.path().join("state.json"));

    let auth = client.check_auth(Some(&store)).await.unwrap();
    assert_eq!(auth.state, "ok");
    assert_eq!(auth.account_name, "Playzi User");

    let songs = client.search_music("lofi", Some("songs"), 10).await.unwrap();
    assert_eq!(songs.len(), 1);
    assert_eq!(songs[0].id, "song1234567");
    assert_eq!(songs[0].duration, Some(225));

    let recs = client.fetch_recommendations(Some(&store)).await.unwrap();
    assert_eq!(recs.len(), 1);

    let playlists = client.fetch_playlists(Some(&store)).await.unwrap();
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].id, "PL_FOCUS");

    assert!(saw_auth.load(Ordering::SeqCst) >= 4);
    let saved = store.read().unwrap();
    assert_eq!(saved.music_auth_state, "ok");
    assert_eq!(saved.music_recommendations.len(), 1);
    assert_eq!(saved.music_playlists.len(), 1);
}

#[tokio::test]
async fn thumbnail_disk_cache_hits_disk_and_prunes() {
    assert!(is_allowed_thumbnail_host("i.ytimg.com"));
    assert!(is_allowed_thumbnail_host("lh3.googleusercontent.com"));
    assert!(!is_allowed_thumbnail_host("evil.example.com"));

    let hits = Arc::new(AtomicUsize::new(0));
    let hits_clone = Arc::clone(&hits);
    let base_url = spawn_mock_server(move |_path, _headers, _body| {
        hits_clone.fetch_add(1, Ordering::SeqCst);
        (200, "image/webp", vec![0x52, 0x49, 0x46, 0x46, 0x01, 0x02])
    })
    .await;

    let dir = TempDir::new().unwrap();
    let strict_cache = ThumbnailCache::new(dir.path().join("thumbs_strict")).unwrap();
    assert!(
        strict_cache
            .normalize_and_validate_url("https://evil.example.com/a.jpg")
            .is_err()
    );

    let cache = ThumbnailCache::new(dir.path().join("thumbs"))
        .unwrap()
        .allow_any_host(true)
        .with_max_entries(2);

    let url1 = format!("{base_url}/thumb1.webp");
    let first = cache.get_or_fetch(&url1).await.unwrap();
    assert!(!first.from_cache);
    assert_eq!(first.content_type, "image/webp");
    assert!(first.path.exists());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    let second = cache.get_or_fetch(&url1).await.unwrap();
    assert!(second.from_cache);
    assert_eq!(second.bytes, first.bytes);
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    // Fetch 2 more URLs so max_entries=2 triggers pruning
    let _ = cache.get_or_fetch(&format!("{base_url}/thumb2.webp")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let _ = cache.get_or_fetch(&format!("{base_url}/thumb3.webp")).await.unwrap();

    let img_count = fs::read_dir(cache.cache_dir())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("img"))
        .count();
    assert!(img_count <= 2);
}

#[tokio::test]
async fn downloads_queue_semaphore_progress_and_cancellation() {
    assert_eq!(quality_to_height("4K"), 2160);
    assert_eq!(quality_to_height("720p"), 720);
    assert_eq!(safe_name("My:Cool/Channel?"), "My_Cool_Channel_");
    assert_eq!(source_for_url("https://music.youtube.com/watch?v=abc"), "ytmusic");

    let p1 = parse_progress_line("PLAYZI_PROG| 45.2% | 2.1MiB/s | 00:12").unwrap();
    assert_eq!(p1.progress, 45.2);
    assert_eq!(p1.speed, "2.1MiB/s");
    assert_eq!(p1.eta, "00:12");
    assert!(!p1.finished);

    let p2 = parse_progress_line("[download]  100.0% of 10.00MiB at 5.00MiB/s ETA 00:00").unwrap();
    assert_eq!(p2.progress, 100.0);
    assert!(p2.finished);

    let dir = TempDir::new().unwrap();
    let cfg = ConfigStore::load_from_root(dir.path());
    let state = StateStore::new(dir.path().join("data").join("state.json"));

    // Create a mock yt-dlp script that emits progress and a final file path
    #[cfg(windows)]
    let mock_bin = {
        let script = dir.path().join("mock_ytdlp.cmd");
        let media_file = dir.path().join("media").join("fake_video.mp4");
        fs::create_dir_all(media_file.parent().unwrap()).unwrap();
        fs::write(&media_file, b"fake-mp4-bytes").unwrap();
        fs::write(
            &script,
            format!(
                "@echo off\r\necho PLAYZI_PROG^|50.0%%^|1.5MiB/s^|00:02\r\necho PLAYZI_PROG^|100.0%%^|1.5MiB/s^|00:00\r\necho PLAYZI_FILE^|{}\r\n",
                media_file.display()
            ),
        )
        .unwrap();
        script.to_string_lossy().to_string()
    };

    #[cfg(not(windows))]
    let mock_bin = {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.path().join("mock_ytdlp.sh");
        let media_file = dir.path().join("media").join("fake_video.mp4");
        fs::create_dir_all(media_file.parent().unwrap()).unwrap();
        fs::write(&media_file, b"fake-mp4-bytes").unwrap();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\necho 'PLAYZI_PROG|50.0%|1.5MiB/s|00:02'\necho 'PLAYZI_PROG|100.0%|1.5MiB/s|00:00'\necho 'PLAYZI_FILE|{}'\n",
                media_file.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().to_string()
    };

    let manager = DownloadManager::new(state.clone(), cfg, 2).with_ytdlp_bin(mock_bin);
    assert_eq!(manager.max_concurrent(), 2);

    let video_args = manager.build_ytdlp_args(
        &json!({"kind": "video", "quality": "720p", "url": "https://youtu.be/dQw4w9WgXcQ"}),
        "out.%(ext)s",
    );
    assert!(video_args.iter().any(|a| a.contains("height<=720")));

    let audio_args = manager.build_ytdlp_args(
        &json!({"kind": "audio", "url": "https://youtu.be/dQw4w9WgXcQ"}),
        "out.%(ext)s",
    );
    assert!(audio_args.contains(&"--audio-format".to_string()));

    let mut rx = manager.subscribe();
    let job = manager
        .start_download(DownloadRequest {
            url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            kind: "video".into(),
            quality: Some("720p".into()),
            collection: Some("TestCol".into()),
            title: Some("Test Video".into()),
            channel: Some("Test Chan".into()),
            thumbnail: None,
        })
        .unwrap();
    let job_id = job["id"].as_str().unwrap().to_string();

    // Wait for completion event
    let mut saw_done = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if let Ok(Ok(ev)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            if ev.job_id == job_id && ev.status == "done" {
                saw_done = true;
                break;
            }
        }
    }
    assert!(saw_done, "Expected download job to emit 'done' event");
    assert_eq!(manager.list_library().unwrap().len(), 1);
    assert_eq!(manager.available_permits(), 2);

    manager.delete_download(&job_id).unwrap();
    assert!(manager.list_downloads().unwrap().is_empty());
}

#[tokio::test]
async fn autosync_due_checks_refresh_all_and_background_loop() {
    let base_url = spawn_mock_server(|path, _headers, body| {
        if path.starts_with("/youtubei/v1/browse") && body.contains("FEwhat_to_watch") {
            let payload = json!({
                "contents": [{
                    "videoRenderer": {
                        "videoId": "rec12345678",
                        "title": {"simpleText": "Recommended Video"},
                        "ownerText": {"simpleText": "Rec Channel"},
                        "lengthText": {"simpleText": "5:10"}
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/browse") && body.contains("FEmusic_home") {
            let payload = json!({
                "contents": [{
                    "musicResponsiveListItemRenderer": {
                        "playlistItemData": {"videoId": "mrec1234567"},
                        "flexColumns": [
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Sync Song"}]}}},
                            {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Sync Artist"}]}}}
                        ]
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        if path.starts_with("/youtubei/v1/browse") && body.contains("FEmusic_liked_playlists") {
            let payload = json!({
                "contents": [{
                    "musicTwoRowItemRenderer": {
                        "title": {"runs": [{"text": "Synced Playlist"}]},
                        "subtitle": {"runs": [{"text": "10 songs"}]},
                        "thumbnailOverlay": {"playlistId": "PL_SYNC_1"}
                    }
                }]
            });
            return (200, "application/json", payload.to_string().into_bytes());
        }
        (404, "text/plain", b"not found".to_vec())
    })
    .await;

    let dir = TempDir::new().unwrap();
    let cfg = ConfigStore::load_from_root(dir.path());
    let state = StateStore::new(dir.path().join("data").join("state.json"));

    // Seed valid Netscape cookies so musicPlaylists sync succeeds
    let private_dir = dir.path().join("data").join("private");
    fs::create_dir_all(&private_dir).unwrap();
    fs::write(
        private_dir.join("playzi_browser_cookies.txt"),
        "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t4102444800\tSAPISID\tsync_sapisid\n",
    )
    .unwrap();

    let yt = YouTubeClient::with_client(reqwest::Client::new(), base_url.clone());
    let autosync = AutoSyncManager::new(state.clone(), cfg, yt).with_ytmusic_base_url(base_url);

    assert!(autosync.is_section_due(SyncSection::Recommendations).unwrap());

    let mut rx = autosync.subscribe();
    let summary = autosync.refresh_all().await.unwrap();
    assert_eq!(summary["ok"], true);

    let saved = state.read().unwrap();
    assert_eq!(saved.recommendations.len(), 1);
    assert_eq!(saved.music_recommendations.len(), 1);
    assert_eq!(saved.music_playlists.len(), 1);
    assert!(!saved.channels_last_sync.is_empty());
    assert!(!saved.sync_status.running);
    assert!(!saved.sync_status.last_success.is_empty());

    // After fresh sync, sections should no longer be due
    assert!(!autosync.is_section_due(SyncSection::Recommendations).unwrap());
    assert!(!autosync.is_section_due(SyncSection::MusicPlaylists).unwrap());

    // Verify at least one broadcast event was emitted
    let ev = rx.try_recv().unwrap();
    assert!(!ev.last_attempt.is_empty());

    // Verify background scheduler starts once and stops cleanly
    assert!(autosync.start_background(Duration::from_millis(10), Duration::from_millis(50)));
    assert!(!autosync.start_background(Duration::from_millis(10), Duration::from_millis(50)));
    tokio::time::sleep(Duration::from_millis(40)).await;
    autosync.stop_background();
}
