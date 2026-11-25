use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::Local;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Semaphore, broadcast, watch};
use uuid::Uuid;

use crate::config::ConfigStore;
use crate::error::{CoreError, Result};
use crate::state::StateStore;

pub const DEFAULT_MAX_CONCURRENT_DOWNLOADS: usize = 3;
const EVENT_CHANNEL_CAPACITY: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    pub url: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub quality: Option<String>,
    pub collection: Option<String>,
    pub title: Option<String>,
    pub channel: Option<String>,
    pub thumbnail: Option<String>,
}

fn default_kind() -> String {
    "video".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadStatusSummary {
    pub id: String,
    pub status: String,
    pub progress: f64,
    pub speed: String,
    pub eta: String,
    pub error: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEvent {
    pub job_id: String,
    pub status: String,
    pub progress: f64,
    pub speed: String,
    pub eta: String,
    pub error: String,
    pub path: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressUpdate {
    pub progress: f64,
    pub speed: String,
    pub eta: String,
    pub finished: bool,
}

#[derive(Clone)]
pub struct DownloadManager {
    state: StateStore,
    config: ConfigStore,
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    events: broadcast::Sender<DownloadEvent>,
    cancellers: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    ytdlp_bin: String,
}

impl DownloadManager {
    pub fn new(state: StateStore, config: ConfigStore, max_concurrent: usize) -> Self {
        let max_concurrent = max_concurrent.max(1);
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            state,
            config,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent,
            events,
            cancellers: Arc::new(Mutex::new(HashMap::new())),
            ytdlp_bin: "yt-dlp".into(),
        }
    }

    pub fn with_ytdlp_bin(mut self, bin: impl Into<String>) -> Self {
        self.ytdlp_bin = bin.into();
        self
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DownloadEvent> {
        self.events.subscribe()
    }

    pub fn list_downloads(&self) -> Result<Vec<Value>> {
        Ok(self.state.read()?.downloads)
    }

    pub fn list_download_status(&self) -> Result<Vec<DownloadStatusSummary>> {
        let downloads = self.state.read()?.downloads;
        Ok(downloads
            .into_iter()
            .map(|job| {
                let created = job
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                DownloadStatusSummary {
                    id: job
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    status: job
                        .get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    progress: job
                        .get("progress")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    speed: job
                        .get("speed")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    eta: job
                        .get("eta")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    error: job
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    updated_at: job
                        .get("updatedAt")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or(created),
                }
            })
            .collect())
    }

    pub fn list_library(&self) -> Result<Vec<Value>> {
        let mut library = self.state.read()?.library;
        library.sort_by(|a, b| {
            let ca = a.get("createdAt").and_then(Value::as_str).unwrap_or("");
            let cb = b.get("createdAt").and_then(Value::as_str).unwrap_or("");
            cb.cmp(ca)
        });
        Ok(library)
    }

    /// Enqueues a download job, deduplicating any already active download for the same URL.
    /// Spawns a Tokio task gated by `tokio::sync::Semaphore`.
    pub fn start_download(&self, req: DownloadRequest) -> Result<Value> {
        let url = req.url.trim().to_string();
        if url.is_empty() {
            return Err(CoreError::Validation("Download URL cannot be empty".into()));
        }

        let current = self.state.read()?;
        if let Some(active) = current.downloads.iter().find(|j| {
            j.get("url").and_then(Value::as_str) == Some(url.as_str())
                && !matches!(
                    j.get("status").and_then(Value::as_str),
                    Some("done" | "error" | "cancelled")
                )
        }) {
            return Ok(active.clone());
        }

        let kind = if req.kind.eq_ignore_ascii_case("audio") {
            "audio"
        } else {
            "video"
        };
        let quality = if kind == "audio" {
            Value::Null
        } else {
            Value::String(
                req.quality
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| "1080p".into()),
            )
        };
        let collection = req.collection.unwrap_or_default();
        let title = req
            .title
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| url.clone());
        let channel = req
            .channel
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "YouTube".into());
        let thumbnail = req.thumbnail.map(Value::String).unwrap_or(Value::Null);

        let item_id = Uuid::new_v4().to_string();
        let now = now_timestamp();
        let job = json!({
            "id": item_id,
            "url": url,
            "title": title,
            "channel": channel,
            "thumbnail": thumbnail,
            "kind": kind,
            "format": kind,
            "quality": quality,
            "collection": collection,
            "progress": 0.0,
            "speed": "",
            "eta": "",
            "status": "queued",
            "error": "",
            "createdAt": now,
            "updatedAt": now,
        });

        let mut history_entry = job.clone();
        if let Some(map) = history_entry.as_object_mut() {
            map.insert("action".into(), Value::String("download".into()));
            map.insert(
                "source".into(),
                Value::String(source_for_url(&url).to_string()),
            );
            map.insert("date".into(), Value::String(now.clone()));
        }

        self.state.update(|s| {
            s.downloads.insert(0, job.clone());
            s.history.insert(0, history_entry);
            Ok(())
        })?;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        {
            let mut guard = self.cancellers.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(item_id.clone(), cancel_tx);
        }

        self.emit_event(DownloadEvent {
            job_id: item_id.clone(),
            status: "queued".into(),
            progress: 0.0,
            speed: String::new(),
            eta: String::new(),
            error: String::new(),
            path: None,
            updated_at: now,
        });

        let manager = self.clone();
        let job_clone = job.clone();
        tokio::spawn(async move {
            manager.run_job(item_id, job_clone, cancel_rx).await;
        });

        Ok(job)
    }

    pub fn cancel_download(&self, job_id: &str) -> Result<()> {
        if let Some(tx) = self
            .cancellers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
        {
            let _ = tx.send(true);
        }
        self.patch_job(
            job_id,
            "cancelled",
            None,
            "",
            "",
            "Cancelado por el usuario",
            None,
        )?;
        Ok(())
    }

    pub fn delete_download(&self, job_id: &str) -> Result<()> {
        if let Some(tx) = self
            .cancellers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job_id)
        {
            let _ = tx.send(true);
        }
        self.state.update(|s| {
            s.downloads
                .retain(|j| j.get("id").and_then(Value::as_str) != Some(job_id));
            Ok(())
        })?;
        Ok(())
    }

    /// Builds `yt-dlp` CLI arguments for a download job.
    pub fn build_ytdlp_args(&self, job: &Value, output_template: &str) -> Vec<String> {
        let cfg = self.config.snapshot();
        let kind = job.get("kind").and_then(Value::as_str).unwrap_or("video");
        let url = job.get("url").and_then(Value::as_str).unwrap_or("");

        let mut args = vec![
            "--newline".to_string(),
            "--no-playlist".to_string(),
            "--write-thumbnail".to_string(),
            "--write-info-json".to_string(),
            "--progress-template".to_string(),
            "download:PLAYZI_PROG|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s"
                .to_string(),
            "--print".to_string(),
            "after_move:PLAYZI_FILE|%(filepath)s".to_string(),
            "-o".to_string(),
            output_template.to_string(),
        ];

        let ffmpeg_bin = crate::player::discover_ffmpeg_binary(&cfg.ffmpeg_path, &cfg.root);
        if !ffmpeg_bin.trim().is_empty() {
            args.push("--ffmpeg-location".to_string());
            args.push(ffmpeg_bin);
        }

        if kind == "audio" {
            args.push("-f".to_string());
            args.push("bestaudio/best".to_string());
            args.push("-x".to_string());
            args.push("--audio-format".to_string());
            args.push(cfg.audio_format.clone());
            args.push("--audio-quality".to_string());
            args.push("0".to_string());
        } else {
            let quality = job
                .get("quality")
                .and_then(Value::as_str)
                .unwrap_or("1080p");
            let height = quality_to_height(quality);
            args.push("-f".to_string());
            args.push(format!(
                "bv*[height<={height}][ext=mp4]+ba[ext=m4a]/bv*[height<={height}]+ba/b[height<={height}]/best"
            ));
            args.push("--merge-output-format".to_string());
            args.push("mp4".to_string());
        }

        args.push(url.to_string());
        args
    }

    async fn run_job(&self, job_id: String, job: Value, mut cancel_rx: watch::Receiver<bool>) {
        let _permit = match self.semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                self.cleanup_canceller(&job_id);
                return;
            }
        };

        if *cancel_rx.borrow() {
            let _ = self.patch_job(
                &job_id,
                "cancelled",
                None,
                "",
                "",
                "Cancelado por el usuario",
                None,
            );
            self.cleanup_canceller(&job_id);
            return;
        }

        let _ = self.patch_job(&job_id, "starting", Some(0.0), "", "", "", None);

        let cfg = self.config.snapshot();
        let collection = job.get("collection").and_then(Value::as_str).unwrap_or("");
        let channel = job
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("YouTube");
        let folder_name = safe_name(if collection.is_empty() {
            channel
        } else {
            collection
        });
        let out_dir = cfg.media_root.join(folder_name);
        if let Err(err) = fs::create_dir_all(&out_dir) {
            let _ = self.patch_job(&job_id, "error", None, "", "", &err.to_string(), None);
            self.cleanup_canceller(&job_id);
            return;
        }

        let out_tmpl = out_dir
            .join("%(title).180B [%(id)s].%(ext)s")
            .to_string_lossy()
            .to_string();
        let args = self.build_ytdlp_args(&job, &out_tmpl);

        let mut cmd = Command::new(&self.ytdlp_bin);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);

        let mut child = match cmd.spawn()
        {
            Ok(c) => c,
            Err(err) => {
                let _ = self.patch_job(
                    &job_id,
                    "error",
                    None,
                    "",
                    "",
                    &format!("Failed to launch {}: {err}", self.ytdlp_bin),
                    None,
                );
                self.cleanup_canceller(&job_id);
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut final_path: Option<PathBuf> = None;
        let mut last_err_line = String::new();

        let stderr_task = tokio::spawn(async move {
            let mut collected = String::new();
            if let Some(err_stream) = stderr {
                let mut lines = BufReader::new(err_stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        collected = trimmed.to_string();
                    }
                }
            }
            collected
        });

        let mut cancelled = false;
        if let Some(out_stream) = stdout {
            let mut lines = BufReader::new(out_stream).lines();
            loop {
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        if *cancel_rx.borrow() {
                            cancelled = true;
                            let _ = child.kill().await;
                            break;
                        }
                    }
                    line_res = lines.next_line() => {
                        match line_res {
                            Ok(Some(line)) => {
                                if let Some(path_str) = line.strip_prefix("PLAYZI_FILE|") {
                                    let p = path_str.trim();
                                    if !p.is_empty() {
                                        final_path = Some(PathBuf::from(p));
                                    }
                                } else if let Some(prog) = parse_progress_line(&line) {
                                    if prog.finished {
                                        let _ = self.patch_job(
                                            &job_id,
                                            "processing",
                                            Some(99.0),
                                            "",
                                            "procesando",
                                            "",
                                            None,
                                        );
                                    } else {
                                        let _ = self.patch_job(
                                            &job_id,
                                            "downloading",
                                            Some(prog.progress),
                                            &prog.speed,
                                            &prog.eta,
                                            "",
                                            None,
                                        );
                                    }
                                }
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }
                }
            }
        }

        let status = child.wait().await;
        if let Ok(err_line) = stderr_task.await {
            last_err_line = err_line;
        }

        if cancelled || *cancel_rx.borrow() {
            let _ = self.patch_job(
                &job_id,
                "cancelled",
                None,
                "",
                "",
                "Cancelado por el usuario",
                None,
            );
            self.cleanup_canceller(&job_id);
            return;
        }

        match status {
            Ok(exit) if exit.success() => {
                let resolved_path = final_path.unwrap_or_else(|| out_dir.clone());
                let kind = job.get("kind").and_then(Value::as_str).unwrap_or("video");
                let format_label = if kind == "audio" {
                    if cfg.audio_format.eq_ignore_ascii_case("mp3") {
                        "MP3"
                    } else {
                        "OPUS"
                    }
                } else {
                    "MP4"
                };

                let now = now_timestamp();
                let library_item = json!({
                    "id": job_id,
                    "url": job.get("url").cloned().unwrap_or(Value::Null),
                    "title": job.get("title").cloned().unwrap_or(Value::Null),
                    "channel": job.get("channel").cloned().unwrap_or(Value::Null),
                    "thumbnail": job.get("thumbnail").cloned().unwrap_or(Value::Null),
                    "kind": kind,
                    "format": format_label,
                    "quality": job.get("quality").cloned().unwrap_or(Value::Null),
                    "collection": collection,
                    "durationText": "--:--",
                    "path": resolved_path.to_string_lossy().to_string(),
                    "mediaUrl": media_url(&resolved_path, &cfg.media_root),
                    "size": size_text(&resolved_path),
                    "createdAt": now,
                });

                let path_str = resolved_path.to_string_lossy().to_string();
                let _ = self.finish_job(&job_id, library_item, &path_str);
            }
            Ok(exit) => {
                let msg = if last_err_line.is_empty() {
                    format!("yt-dlp exited with status {exit}")
                } else {
                    last_err_line
                };
                let _ = self.patch_job(&job_id, "error", None, "", "", &msg, None);
            }
            Err(err) => {
                let _ = self.patch_job(&job_id, "error", None, "", "", &err.to_string(), None);
            }
        }

        self.cleanup_canceller(&job_id);
    }

    fn patch_job(
        &self,
        job_id: &str,
        status: &str,
        progress: Option<f64>,
        speed: &str,
        eta: &str,
        error: &str,
        path: Option<&str>,
    ) -> Result<()> {
        let now = now_timestamp();
        let mut final_progress = progress.unwrap_or(0.0);

        self.state.update(|s| {
            for job in &mut s.downloads {
                if job.get("id").and_then(Value::as_str) == Some(job_id) {
                    if let Some(map) = job.as_object_mut() {
                        map.insert("status".into(), Value::String(status.to_string()));
                        if let Some(p) = progress {
                            map.insert("progress".into(), json!(p));
                            final_progress = p;
                        } else if let Some(existing) = map.get("progress").and_then(Value::as_f64) {
                            final_progress = existing;
                        }
                        map.insert("speed".into(), Value::String(speed.to_string()));
                        map.insert("eta".into(), Value::String(eta.to_string()));
                        map.insert("error".into(), Value::String(error.to_string()));
                        map.insert("updatedAt".into(), Value::String(now.clone()));
                        if let Some(p) = path {
                            map.insert("path".into(), Value::String(p.to_string()));
                        }
                    }
                    break;
                }
            }
            Ok(())
        })?;

        self.emit_event(DownloadEvent {
            job_id: job_id.to_string(),
            status: status.to_string(),
            progress: final_progress,
            speed: speed.to_string(),
            eta: eta.to_string(),
            error: error.to_string(),
            path: path.map(ToString::to_string),
            updated_at: now,
        });

        Ok(())
    }

    fn finish_job(&self, job_id: &str, library_item: Value, path: &str) -> Result<()> {
        let now = now_timestamp();
        self.state.update(|s| {
            for job in &mut s.downloads {
                if job.get("id").and_then(Value::as_str) == Some(job_id) {
                    if let Some(map) = job.as_object_mut() {
                        map.insert("status".into(), Value::String("done".into()));
                        map.insert("progress".into(), json!(100.0));
                        map.insert("speed".into(), Value::String(String::new()));
                        map.insert("eta".into(), Value::String(String::new()));
                        map.insert("error".into(), Value::String(String::new()));
                        map.insert("path".into(), Value::String(path.to_string()));
                        map.insert("updatedAt".into(), Value::String(now.clone()));
                    }
                    break;
                }
            }
            s.library.insert(0, library_item);
            Ok(())
        })?;

        self.emit_event(DownloadEvent {
            job_id: job_id.to_string(),
            status: "done".into(),
            progress: 100.0,
            speed: String::new(),
            eta: String::new(),
            error: String::new(),
            path: Some(path.to_string()),
            updated_at: now,
        });
        Ok(())
    }

    fn emit_event(&self, event: DownloadEvent) {
        let _ = self.events.send(event);
    }

    fn cleanup_canceller(&self, job_id: &str) {
        self.cancellers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job_id);
    }
}

pub fn quality_to_height(quality: &str) -> u32 {
    match quality.trim() {
        "4K" | "2160p" => 2160,
        "1440p" => 1440,
        "1080p" => 1080,
        "720p" => 720,
        "480p" => 480,
        "360p" => 360,
        _ => 1080,
    }
}

/// Parses either Playzi's custom `PLAYZI_PROG|pct|speed|eta` line or standard `[download]` output.
pub fn parse_progress_line(line: &str) -> Option<ProgressUpdate> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("PLAYZI_PROG|") {
        let parts: Vec<&str> = rest.split('|').collect();
        if parts.len() >= 3 {
            let pct_str = parts[0].trim().trim_end_matches('%');
            let progress = pct_str.parse::<f64>().unwrap_or(0.0).clamp(0.0, 100.0);
            let speed = clean_metric(parts[1]);
            let eta = clean_metric(parts[2]);
            let finished = progress >= 100.0;
            return Some(ProgressUpdate {
                progress: (progress * 10.0).round() / 10.0,
                speed,
                eta,
                finished,
            });
        }
    }

    static STD_RE: OnceLock<Regex> = OnceLock::new();
    let re = STD_RE.get_or_init(|| {
        Regex::new(r"\[download\]\s+(\d+(?:\.\d+)?)%(?:.*?at\s+(\S+))?(?:.*?ETA\s+(\S+))?").unwrap()
    });
    if let Some(caps) = re.captures(trimmed) {
        let progress = caps
            .get(1)
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .unwrap_or(0.0)
            .clamp(0.0, 100.0);
        let speed = caps
            .get(2)
            .map(|m| clean_metric(m.as_str()))
            .unwrap_or_default();
        let eta = caps
            .get(3)
            .map(|m| clean_metric(m.as_str()))
            .unwrap_or_default();
        return Some(ProgressUpdate {
            progress: (progress * 10.0).round() / 10.0,
            speed,
            eta,
            finished: progress >= 100.0,
        });
    }
    None
}

fn clean_metric(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("NA") || t.eq_ignore_ascii_case("N/A") || t == "Unknown" {
        String::new()
    } else {
        t.to_string()
    }
}

pub fn safe_name(value: &str) -> String {
    let raw = if value.trim().is_empty() {
        "YouTube"
    } else {
        value
    };
    let cleaned: String = raw
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "YouTube".into()
    } else {
        trimmed.to_string()
    }
}

pub fn media_url(path: &Path, media_root: &Path) -> String {
    let resolved_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let resolved_root = media_root
        .canonicalize()
        .unwrap_or_else(|_| media_root.to_path_buf());
    let Ok(rel) = resolved_path.strip_prefix(&resolved_root) else {
        return String::new();
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    format!("/media/{}", parts.join("/"))
}

pub fn size_text(path: &Path) -> String {
    let Ok(meta) = fs::metadata(path) else {
        return "--".into();
    };
    let mut size = meta.len() as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if size < 1024.0 || unit == "GB" {
            return if unit == "B" {
                format!("{} B", meta.len())
            } else {
                format!("{size:.1} {unit}")
            };
        }
        size /= 1024.0;
    }
    "--".into()
}

pub fn source_for_url(url: &str) -> &'static str {
    let lower = url.to_ascii_lowercase();
    if lower.contains("music.youtube.com") {
        "ytmusic"
    } else if lower.contains("youtube.com") || lower.contains("youtu.be") {
        "youtube"
    } else {
        "other"
    }
}

fn now_timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}
