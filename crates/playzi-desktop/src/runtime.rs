use std::path::PathBuf;

use playzi_core::autosync::AutoSyncManager;
use playzi_core::collections::list_collections;
use playzi_core::config::ConfigStore;
use playzi_core::cookies::CookieStore;
use playzi_core::downloads::{DEFAULT_MAX_CONCURRENT_DOWNLOADS, DownloadManager};
use playzi_core::error::Result;
use playzi_core::history::list_history;
use playzi_core::party::{PartyManager, ensure_user_profile, list_friends};
use playzi_core::player::PlayerManager;
use playzi_core::playlists::list_playlists;
use playzi_core::state::StateStore;
use playzi_core::thumbs::ThumbnailCache;
use playzi_core::youtube::YouTubeClient;
use playzi_core::ytmusic::YtMusicClient;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct AppRuntime {
    pub root_dir: PathBuf,
    pub config: ConfigStore,
    pub state: StateStore,
    pub youtube: YouTubeClient,
    pub thumbs: ThumbnailCache,
    pub downloads: DownloadManager,
    pub autosync: AutoSyncManager,
    pub player: PlayerManager,
    pub party: PartyManager,
}

impl AppRuntime {
    pub fn initialize(root_dir: PathBuf) -> Result<Self> {
        let config = ConfigStore::load_from_root(&root_dir);
        let snap = config.snapshot();
        snap.ensure_dirs()?;

        let state = StateStore::new(&snap.state_file);
        let youtube = YouTubeClient::new()?;
        let thumbs = ThumbnailCache::new(snap.data_dir.join("thumbs"))?;
        let downloads = DownloadManager::new(
            state.clone(),
            config.clone(),
            DEFAULT_MAX_CONCURRENT_DOWNLOADS,
        );
        let autosync = AutoSyncManager::new(state.clone(), config.clone(), youtube.clone());
        let player = PlayerManager::new(state.clone(), config.clone());
        let party = PartyManager::new(state.clone());

        Ok(Self {
            root_dir,
            config,
            state,
            youtube,
            thumbs,
            downloads,
            autosync,
            player,
            party,
        })
    }

    pub fn cookie_store(&self) -> CookieStore {
        let snap = self.config.snapshot();
        CookieStore::new(&snap.root, &snap.data_dir)
    }

    pub fn build_ytmusic_client(&self) -> Result<YtMusicClient> {
        let snap = self.config.snapshot();
        let configured = if snap.cookies_file.trim().is_empty() {
            None
        } else {
            Some(snap.cookies_file.as_str())
        };
        let jar = self.cookie_store().load_jar(configured).ok();
        YtMusicClient::new(jar)
    }

    pub fn bootstrap_payload(&self) -> Result<Value> {
        let s = self.state.read()?;
        let cfg = self.config.public_config();
        let library = self.downloads.list_library()?;
        let history = list_history(&self.state)?;
        let collections = list_collections(&self.state)?;
        let playlists = list_playlists(&self.state)?;
        let user_profile = ensure_user_profile(&self.state)?;
        let friends = list_friends(&self.state)?;

        Ok(json!({
            "config": cfg,
            "downloads": { "items": s.downloads },
            "library": { "items": library },
            "history": { "items": history },
            "channels": { "items": s.channels },
            "feed": { "items": s.feed },
            "collections": { "items": collections },
            "playlists": { "items": playlists },
            "recommendations": {
                "items": s.recommendations,
                "lastSync": s.recommendations_last_sync,
            },
            "musicRecommendations": {
                "items": s.music_recommendations,
                "lastSync": s.music_recom_last_sync,
            },
            "musicPlaylists": {
                "items": s.music_playlists,
                "lastSync": s.music_playlists_last_sync,
            },
            "musicLibrary": s.music_library,
            "channelsLastSync": s.channels_last_sync,
            "musicAuthState": s.music_auth_state,
            "musicAccountName": s.music_account_name,
            "syncStatus": s.sync_status,
            "userProfile": user_profile,
            "friends": friends,
        }))
    }

    pub fn state_summary_payload(&self) -> Result<Value> {
        let s = self.state.read()?;
        let status_items = self.downloads.list_download_status()?;
        Ok(json!({
            "downloads": { "items": s.downloads, "status": status_items },
            "syncStatus": s.sync_status,
            "musicAuthState": s.music_auth_state,
            "musicAccountName": s.music_account_name,
        }))
    }
}

/// Resolves the Playzi project root directory (prefers existing `data/state.json` or `.env` nearby).
pub fn resolve_playzi_root() -> PathBuf {
    if let Ok(env_root) = std::env::var("PLAYZI_ROOT") {
        let p = PathBuf::from(env_root);
        if p.exists() {
            return p;
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("data").exists() || cwd.join(".env").exists() || cwd.join("static").exists() {
            return cwd;
        }
        if let Some(parent) = cwd.parent().and_then(|p| p.parent()) {
            if parent.join("data").exists() || parent.join("static").exists() {
                return parent.to_path_buf();
            }
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1).take(5) {
            if ancestor.join(".env").exists()
                || ancestor.join("data").exists()
                || ancestor.join("static").exists()
            {
                return ancestor.to_path_buf();
            }
        }
        if let Some(dir) = exe.parent() {
            return dir.to_path_buf();
        }
    }

    PathBuf::from(".")
}
