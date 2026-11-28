use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command as TokioCommand};
use tokio::sync::Mutex as TokioMutex;

use crate::config::ConfigStore;
use crate::error::{CoreError, Result};
use crate::player::{
    extract_url_expire, is_dash_format, resolve_js_runtime, CachedRawVideo, StreamFormat,
};
use crate::youtube::format_duration;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const WORKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const WORKER_COLD_START_TIMEOUT: Duration = Duration::from_secs(16);

struct WorkerProcess {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

/// Persistent IPC client for the local MIT `yt-worker/worker.mjs` (`YouTube.js` + `BgUtils`).
/// Lazily spawns a single hidden Node.js child process and transparently falls back to `yt-dlp`
/// on any error, timeout, or missing runtime.
#[derive(Clone)]
pub(crate) struct YtWorkerClient {
    config: ConfigStore,
    proc: Arc<TokioMutex<Option<WorkerProcess>>>,
    next_id: Arc<AtomicU64>,
}

impl YtWorkerClient {
    pub(crate) fn new(config: ConfigStore) -> Self {
        Self {
            config,
            proc: Arc::new(TokioMutex::new(None)),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub(crate) async fn resolve_video(
        &self,
        video_id: &str,
        original_url: &str,
        cookie_file: Option<&Path>,
    ) -> Result<CachedRawVideo> {
        let script_path = self.find_worker_script().ok_or_else(|| {
            CoreError::External("yt-worker/worker.mjs not found".into())
        })?;
        let node_bin = resolve_node_binary().ok_or_else(|| {
            CoreError::External("Node.js runtime not found for yt-worker".into())
        })?;

        let req_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let payload = json!({
            "id": req_id,
            "videoId": video_id,
            "originalUrl": original_url,
            "cookieFile": cookie_file.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        });
        let mut line = serde_json::to_string(&payload)
            .map_err(|e| CoreError::External(format!("Failed to serialize yt-worker request: {e}")))?;
        line.push('\n');

        let mut guard = self.proc.lock().await;
        let is_cold_start = guard.is_none();
        if is_cold_start {
            *guard = Some(spawn_worker(&node_bin, &script_path)?);
        }
        let req_timeout = if is_cold_start {
            WORKER_COLD_START_TIMEOUT
        } else {
            WORKER_REQUEST_TIMEOUT
        };

        let resp_line = match tokio::time::timeout(req_timeout, async {
            let proc = guard.as_mut().expect("worker spawned");
            proc.stdin.write_all(line.as_bytes()).await?;
            proc.stdin.flush().await?;
            let mut out = String::new();
            let n = proc.stdout.read_line(&mut out).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "yt-worker closed stdout",
                ));
            }
            Ok::<String, std::io::Error>(out)
        })
        .await
        {
            Ok(Ok(out)) => out,
            Ok(Err(io_err)) => {
                *guard = None;
                return Err(CoreError::External(format!("yt-worker I/O failed: {io_err}")));
            }
            Err(_) => {
                *guard = None;
                return Err(CoreError::External("yt-worker request timed out".into()));
            }
        };

        parse_worker_response(&resp_line, video_id, original_url)
    }

    pub(crate) async fn shutdown(&self) {
        let mut guard = self.proc.lock().await;
        if let Some(mut worker) = guard.take() {
            let _ = worker._child.start_kill();
        }
    }

    fn find_worker_script(&self) -> Option<PathBuf> {
        let cfg = self.config.snapshot();
        let mut candidates = vec![
            cfg.root.join("yt-worker").join("worker.mjs"),
            cfg.root.join("scripts").join("yt-worker").join("worker.mjs"),
        ];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("yt-worker").join("worker.mjs"));
                candidates.push(dir.join("resources").join("yt-worker").join("worker.mjs"));
            }
        }
        candidates
            .into_iter()
            .find(|p| p.is_file() && p.parent().is_some_and(|d| d.join("node_modules").is_dir()))
    }
}

fn resolve_node_binary() -> Option<String> {
    if let Some((name, path)) = resolve_js_runtime() {
        if name == "node" {
            return Some(path);
        }
    }
    None
}

fn spawn_worker(node_bin: &str, script_path: &Path) -> Result<WorkerProcess> {
    let mut cmd = TokioCommand::new(node_bin);
    cmd.arg(script_path);
    if let Some(dir) = script_path.parent() {
        cmd.current_dir(dir);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn().map_err(|e| {
        CoreError::External(format!("Failed to spawn yt-worker ({node_bin}): {e}"))
    })?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::External("yt-worker missing stdin pipe".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::External("yt-worker missing stdout pipe".into()))?;

    Ok(WorkerProcess {
        _child: child,
        stdin,
        stdout: BufReader::new(stdout),
    })
}

pub(crate) fn parse_worker_response(
    line: &str,
    video_id: &str,
    original_url: &str,
) -> Result<CachedRawVideo> {
    let parsed: Value = serde_json::from_str(line.trim())
        .map_err(|e| CoreError::External(format!("Invalid yt-worker JSON: {e}")))?;

    if !parsed.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        let msg = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("Unknown yt-worker error");
        return Err(CoreError::External(format!("yt-worker error: {msg}")));
    }

    let data = parsed
        .get("data")
        .ok_or_else(|| CoreError::External("yt-worker response missing data".into()))?;

    let raw_formats = data
        .get("formats")
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::External("yt-worker response missing formats".into()))?;

    let mut formats = Vec::with_capacity(raw_formats.len());
    for entry in raw_formats {
        let url = entry
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            continue;
        }
        formats.push(StreamFormat {
            format_id: entry
                .get("formatId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            url: url.to_string(),
            ext: entry
                .get("ext")
                .and_then(Value::as_str)
                .unwrap_or("mp4")
                .to_string(),
            vcodec: entry
                .get("vcodec")
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_string(),
            acodec: entry
                .get("acodec")
                .and_then(Value::as_str)
                .unwrap_or("none")
                .to_string(),
            width: entry.get("width").and_then(Value::as_u64).map(|v| v as u32),
            height: entry.get("height").and_then(Value::as_u64).map(|v| v as u32),
            fps: entry.get("fps").and_then(Value::as_f64),
            tbr: entry.get("tbr").and_then(Value::as_f64),
            abr: entry.get("abr").and_then(Value::as_f64),
            content_length: entry.get("contentLength").and_then(Value::as_u64),
            init_range: entry
                .get("initRange")
                .and_then(Value::as_str)
                .map(str::to_string),
            index_range: entry
                .get("indexRange")
                .and_then(Value::as_str)
                .map(str::to_string),
            language: entry
                .get("language")
                .and_then(Value::as_str)
                .map(str::to_string),
            language_name: entry
                .get("languageName")
                .or_else(|| entry.get("audioTrackName"))
                .and_then(Value::as_str)
                .map(str::to_string),
            mime_type: entry
                .get("mimeType")
                .and_then(Value::as_str)
                .map(str::to_string),
            asr: entry.get("asr").and_then(Value::as_u64).map(|v| v as u32),
            audio_channels: entry
                .get("audioChannels")
                .and_then(Value::as_u64)
                .map(|v| v as u32),
            http_headers: std::collections::HashMap::new(),
        });
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
            "yt-worker returned no playable DASH or muxed formats".into(),
        ));
    }

    let duration = data.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    let loudness_db = data
        .get("loudnessDb")
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (-60.0..=30.0).contains(v));

    let expires_at = data
        .get("expiresAt")
        .and_then(Value::as_i64)
        .or_else(|| formats.iter().filter_map(|f| extract_url_expire(&f.url)).min());

    let raw_item = data.get("item");
    let id = raw_item
        .and_then(|i| i.get("id"))
        .and_then(Value::as_str)
        .unwrap_or(video_id)
        .to_string();
    let title = raw_item
        .and_then(|i| i.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(original_url)
        .to_string();
    let channel = raw_item
        .and_then(|i| i.get("channel"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let thumbnail = raw_item
        .and_then(|i| i.get("thumbnail"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::build_dash_mpd_manifest_for_video;

    #[test]
    fn parse_worker_response_projects_dash_mpd_and_metadata() {
        let payload = json!({
            "id": 1,
            "ok": true,
            "data": {
                "item": {
                    "id": "oa-D2KlvjbU",
                    "title": "Daisies",
                    "channel": "Black Gryph0n",
                    "thumbnail": "https://i.ytimg.com/vi/oa-D2KlvjbU/hqdefault.jpg",
                    "url": "https://www.youtube.com/watch?v=oa-D2KlvjbU",
                    "duration": 138
                },
                "duration": 138.0,
                "loudnessDb": -1.91,
                "expiresAt": 1800000000,
                "formats": [
                    {
                        "formatId": "137",
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1800000000&itag=137",
                        "ext": "mp4",
                        "vcodec": "avc1.640020",
                        "acodec": "none",
                        "width": 1080,
                        "height": 1080,
                        "fps": 25.0,
                        "tbr": 320.0,
                        "contentLength": 2230558,
                        "initRange": "0-742",
                        "indexRange": "743-1120"
                    },
                    {
                        "formatId": "140",
                        "url": "https://rr1.googlevideo.com/videoplayback?expire=1800000000&itag=140",
                        "ext": "m4a",
                        "vcodec": "none",
                        "acodec": "mp4a.40.2",
                        "abr": 129.0,
                        "tbr": 129.0,
                        "contentLength": 2236635,
                        "initRange": "0-758",
                        "indexRange": "759-1140",
                        "audioIsDefault": true
                    }
                ]
            }
        });

        let cached = parse_worker_response(
            &payload.to_string(),
            "oa-D2KlvjbU",
            "https://www.youtube.com/watch?v=oa-D2KlvjbU",
        )
        .expect("worker response should parse");
        assert_eq!(cached.formats.len(), 2);
        assert_eq!(cached.loudness_db, Some(-1.91));

        let projected = cached.project("best", false);
        let video = projected.video.as_ref().expect("should pick 1080p video");
        let audio = projected.audio.as_ref().expect("should pick m4a audio");
        assert_eq!(video.format_id, "137");
        assert_eq!(audio.format_id, "140");

        let mpd = build_dash_mpd_manifest_for_video(
            projected.duration,
            video,
            audio,
            "http://playzi-stream.localhost/",
            Some("oa-D2KlvjbU"),
        )
        .expect("should build valid DASH MPD");
        assert!(mpd.contains("yt-stream?v=oa-D2KlvjbU&amp;itag=137"));
        assert!(mpd.contains("yt-stream?v=oa-D2KlvjbU&amp;itag=140"));
    }

    #[test]
    fn parse_worker_response_rejects_errors_and_empty_formats() {
        let err_json = json!({ "id": 1, "ok": false, "error": "Live streams fall back to yt-dlp HLS" });
        assert!(parse_worker_response(&err_json.to_string(), "abc12345678", "https://youtu.be/abc12345678").is_err());

        let empty_json = json!({ "id": 1, "ok": true, "data": { "duration": 10, "formats": [] } });
        assert!(parse_worker_response(&empty_json.to_string(), "abc12345678", "https://youtu.be/abc12345678").is_err());
    }
}
