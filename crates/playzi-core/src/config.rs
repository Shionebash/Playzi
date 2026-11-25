use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::Result;

const ENV_KEY_ORDER: &[&str] = &[
    "APP_HOST",
    "APP_PORT",
    "MEDIA_ROOT",
    "DATA_DIR",
    "STATE_FILE",
    "DEFAULT_PLAYER",
    "MPV_PATH",
    "VLC_PATH",
    "FFMPEG_PATH",
    "AUDIO_FORMAT",
    "YTDLP_SEARCH_LIMIT",
    "PLAYBACK_QUALITY",
    "SHOW_SHORTS",
    "SOCIAL_HUB_URL",
];

/// Built-in default Social Hub URL injected at compile time from `SOCIAL_HUB_URL`
/// (environment variable or workspace `.env`) via `build.rs`, keeping the URL out of Git.
pub const DEFAULT_SOCIAL_HUB_URL: &str = match option_env!("PLAYZI_BUILT_SOCIAL_HUB_URL") {
    Some(url) => url,
    None => "",
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayersConfig {
    pub mpv: String,
    pub vlc: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublicConfig {
    pub app_host: String,
    pub app_port: u16,
    pub media_root: String,
    pub data_dir: String,
    pub default_player: String,
    pub players: PlayersConfig,
    pub ffmpeg: String,
    pub audio_format: String,
    pub search_limit: usize,
    pub playback_quality: String,
    pub cookies_browser: String,
    pub cookies_file: String,
    pub show_shorts: bool,
    pub social_hub_url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigUpdate {
    pub media_root: Option<String>,
    pub default_player: Option<String>,
    pub mpv_path: Option<String>,
    pub vlc_path: Option<String>,
    pub ffmpeg_path: Option<String>,
    pub audio_format: Option<String>,
    pub search_limit: Option<usize>,
    pub playback_quality: Option<String>,
    pub cookies_browser: Option<String>,
    pub cookies_file: Option<String>,
    pub show_shorts: Option<bool>,
    pub social_hub_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub root: PathBuf,
    pub env_file: PathBuf,
    pub app_host: String,
    pub app_port: u16,
    pub media_root: PathBuf,
    pub data_dir: PathBuf,
    pub state_file: PathBuf,
    pub default_player: String,
    pub mpv_path: String,
    pub vlc_path: String,
    pub ffmpeg_path: String,
    pub audio_format: String,
    pub ytdlp_search_limit: usize,
    pub playback_quality: String,
    pub cookies_browser: String,
    pub cookies_file: String,
    pub show_shorts: bool,
    pub social_hub_url: String,
}

impl AppConfig {
    pub fn load_from_root(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        let env_file = root.join(".env");
        let file_map = read_env_map(&env_file);
        Self::from_env_map(root, env_file, &file_map)
    }

    fn from_env_map(root: PathBuf, env_file: PathBuf, file_map: &BTreeMap<String, String>) -> Self {
        let get = |key: &str, default: &str| -> String {
            file_map
                .get(key)
                .cloned()
                .or_else(|| std::env::var(key).ok())
                .map(|v| v.trim().trim_matches('"').to_string())
                .filter(|v| !v.is_empty() || key == "COOKIES_BROWSER" || key == "COOKIES_FILE")
                .unwrap_or_else(|| default.to_string())
        };

        let app_host = get("APP_HOST", "127.0.0.1");
        let app_port = get("APP_PORT", "8765").parse::<u16>().unwrap_or(8765);
        let media_root = PathBuf::from(get("MEDIA_ROOT", &root.join("media").to_string_lossy()));
        let data_dir = PathBuf::from(get("DATA_DIR", &root.join("data").to_string_lossy()));
        let state_file = PathBuf::from(get(
            "STATE_FILE",
            &data_dir.join("state.json").to_string_lossy(),
        ));
        let default_player = get("DEFAULT_PLAYER", "mpv").to_lowercase();
        let mpv_path = get("MPV_PATH", "mpv");
        let vlc_path = get("VLC_PATH", "vlc");
        let ffmpeg_path = get("FFMPEG_PATH", "ffmpeg");
        let audio_format = get("AUDIO_FORMAT", "opus").to_lowercase();
        let ytdlp_search_limit = get("YTDLP_SEARCH_LIMIT", "12")
            .parse::<usize>()
            .unwrap_or(12)
            .clamp(1, 100);
        let playback_quality = get("PLAYBACK_QUALITY", "best");
        let cookies_browser = file_map
            .get("COOKIES_BROWSER")
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string();
        let cookies_file = file_map
            .get("COOKIES_FILE")
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string();
        let show_shorts = matches!(
            get("SHOW_SHORTS", "false").to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
        let social_hub_url = file_map
            .get("SOCIAL_HUB_URL")
            .cloned()
            .or_else(|| std::env::var("SOCIAL_HUB_URL").ok())
            .map(|v| v.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| DEFAULT_SOCIAL_HUB_URL.trim().to_string());

        Self {
            root,
            env_file,
            app_host,
            app_port,
            media_root,
            data_dir,
            state_file,
            default_player,
            mpv_path,
            vlc_path,
            ffmpeg_path,
            audio_format,
            ytdlp_search_limit,
            playback_quality,
            cookies_browser,
            cookies_file,
            show_shorts,
            social_hub_url,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.media_root)?;
        fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }

    pub fn to_public(&self) -> PublicConfig {
        PublicConfig {
            app_host: self.app_host.clone(),
            app_port: self.app_port,
            media_root: self.media_root.to_string_lossy().into_owned(),
            data_dir: self.data_dir.to_string_lossy().into_owned(),
            default_player: self.default_player.clone(),
            players: PlayersConfig {
                mpv: self.mpv_path.clone(),
                vlc: self.vlc_path.clone(),
            },
            ffmpeg: self.ffmpeg_path.clone(),
            audio_format: self.audio_format.clone(),
            search_limit: self.ytdlp_search_limit,
            playback_quality: self.playback_quality.clone(),
            cookies_browser: self.cookies_browser.clone(),
            cookies_file: self.cookies_file.clone(),
            show_shorts: self.show_shorts,
            social_hub_url: self.social_hub_url.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<RwLock<AppConfig>>,
}

impl ConfigStore {
    pub fn load_from_root(root: impl AsRef<Path>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(AppConfig::load_from_root(root))),
        }
    }

    pub fn snapshot(&self) -> AppConfig {
        self.inner.read().expect("config lock poisoned").clone()
    }

    pub fn public_config(&self) -> PublicConfig {
        self.snapshot().to_public()
    }

    pub fn update(&self, patch: ConfigUpdate) -> Result<PublicConfig> {
        let mut cfg = self.inner.write().expect("config lock poisoned");
        let old_browser = cfg.cookies_browser.clone();
        let old_file = cfg.cookies_file.clone();

        let mut env_map = read_env_map(&cfg.env_file);

        let mut apply_non_empty = |key: &str, val: Option<String>| {
            if let Some(v) = val {
                let s = v.trim().to_string();
                if !s.is_empty() {
                    env_map.insert(key.to_string(), s);
                }
            }
        };

        apply_non_empty("MEDIA_ROOT", patch.media_root);
        apply_non_empty("DEFAULT_PLAYER", patch.default_player);
        apply_non_empty("MPV_PATH", patch.mpv_path);
        apply_non_empty("VLC_PATH", patch.vlc_path);
        apply_non_empty("FFMPEG_PATH", patch.ffmpeg_path);
        apply_non_empty("AUDIO_FORMAT", patch.audio_format);
        apply_non_empty(
            "YTDLP_SEARCH_LIMIT",
            patch.search_limit.map(|n| n.clamp(1, 100).to_string()),
        );
        apply_non_empty("PLAYBACK_QUALITY", patch.playback_quality);

        if let Some(v) = patch.cookies_browser {
            env_map.insert("COOKIES_BROWSER".to_string(), v.trim().to_string());
        }
        if let Some(v) = patch.cookies_file {
            env_map.insert("COOKIES_FILE".to_string(), v.trim().to_string());
        }
        if let Some(v) = patch.show_shorts {
            env_map.insert("SHOW_SHORTS".to_string(), if v { "true" } else { "false" }.to_string());
        }
        if let Some(v) = patch.social_hub_url {
            env_map.insert("SOCIAL_HUB_URL".to_string(), v.trim().to_string());
        }

        write_env_map(&cfg.env_file, &env_map)?;
        let refreshed = AppConfig::from_env_map(cfg.root.clone(), cfg.env_file.clone(), &env_map);
        refreshed.ensure_dirs()?;

        if old_browser != refreshed.cookies_browser || old_file != refreshed.cookies_file {
            let cookie_cache = refreshed.data_dir.join("private").join("cookies_cache.txt");
            let _ = fs::remove_file(cookie_cache);
        }

        *cfg = refreshed;
        Ok(cfg.to_public())
    }
}

fn read_env_map(path: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(raw) = fs::read_to_string(path) else {
        return map;
    };
    let content = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn write_env_map(path: &Path, values: &BTreeMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lines = Vec::with_capacity(values.len());
    for &key in ENV_KEY_ORDER {
        if let Some(val) = values.get(key) {
            lines.push(format!("{key}={val}"));
        }
    }
    for (key, val) in values {
        if !ENV_KEY_ORDER.contains(&key.as_str()) {
            lines.push(format!("{key}={val}"));
        }
    }
    let body = format!("{}\n", lines.join("\n"));
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&tmp, body)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
