use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

pub const MAX_HISTORY: usize = 300;
pub const MAX_FEED: usize = 200;
pub const MAX_LIBRARY: usize = 1000;
pub const MAX_DOWNLOADS: usize = 250;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MusicLibraryState {
    pub songs: Vec<Value>,
    pub liked: Vec<Value>,
    pub albums: Vec<Value>,
    pub artists: Vec<Value>,
    pub playlists: Vec<Value>,
    pub last_sync: String,
    pub error: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncStatusState {
    pub running: bool,
    pub section: String,
    pub last_attempt: String,
    pub last_success: String,
    pub last_error: String,
    pub used_stale_cookies: bool,
    pub warning: String,
    pub sections: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct UserProfile {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct FriendRecord {
    pub user_id: String,
    pub display_name: String,
    pub google_account_name: String,
    pub note: String,
    pub default_allow_queue: bool,
    pub added_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppState {
    pub downloads: Vec<Value>,
    pub library: Vec<Value>,
    pub history: Vec<Value>,
    pub channels: Vec<Value>,
    pub feed: Vec<Value>,
    pub collections: Vec<Value>,
    pub playlists: Vec<Value>,
    pub recommendations: Vec<Value>,
    pub recommendations_last_sync: String,
    pub music_recommendations: Vec<Value>,
    pub music_recom_last_sync: String,
    pub music_playlists: Vec<Value>,
    pub music_playlists_last_sync: String,
    pub channels_last_sync: String,
    pub music_auth_state: String,
    pub music_auth_checked_at: String,
    pub music_account_name: String,
    pub music_library: MusicLibraryState,
    pub music_history: Vec<Value>,
    pub imported_playlists: Vec<Value>,
    pub sync_status: SyncStatusState,
    pub user_profile: UserProfile,
    pub friends: Vec<FriendRecord>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl AppState {
    pub fn trim(&mut self) {
        self.history.truncate(MAX_HISTORY);
        self.music_history.truncate(MAX_HISTORY);
        self.feed.truncate(MAX_FEED);
        self.library.truncate(MAX_LIBRARY);
        self.downloads.truncate(MAX_DOWNLOADS);
    }
}

struct InnerState {
    path: PathBuf,
    cache: Option<AppState>,
}

#[derive(Clone)]
pub struct StateStore {
    inner: Arc<Mutex<InnerState>>,
}

impl StateStore {
    pub fn new(state_file: impl AsRef<Path>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(InnerState {
                path: state_file.as_ref().to_path_buf(),
                cache: None,
            })),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.inner.lock().expect("state lock poisoned").path.clone()
    }

    pub fn read(&self) -> Result<AppState> {
        let mut guard = self.inner.lock().expect("state lock poisoned");
        Self::ensure_loaded(&mut guard)?;
        Ok(guard.cache.as_ref().cloned().unwrap_or_default())
    }

    pub fn write(&self, mut data: AppState) -> Result<()> {
        let mut guard = self.inner.lock().expect("state lock poisoned");
        data.trim();
        write_atomic(&guard.path, &data)?;
        guard.cache = Some(data);
        Ok(())
    }

    pub fn update<F, R>(&self, mutator: F) -> Result<R>
    where
        F: FnOnce(&mut AppState) -> Result<R>,
    {
        let mut guard = self.inner.lock().expect("state lock poisoned");
        Self::ensure_loaded(&mut guard)?;
        let mut working = guard.cache.as_ref().cloned().unwrap_or_default();
        let outcome = mutator(&mut working)?;
        working.trim();
        write_atomic(&guard.path, &working)?;
        guard.cache = Some(working);
        Ok(outcome)
    }

    fn ensure_loaded(guard: &mut InnerState) -> Result<()> {
        if guard.cache.is_some() {
            return Ok(());
        }
        if !guard.path.exists() {
            let default_state = AppState::default();
            write_atomic(&guard.path, &default_state)?;
            guard.cache = Some(default_state);
            return Ok(());
        }
        let raw = fs::read_to_string(&guard.path)?;
        let mut state = match serde_json::from_str::<AppState>(&raw) {
            Ok(parsed) => parsed,
            Err(_) => {
                let backup = guard.path.with_extension("json.bak");
                let _ = fs::rename(&guard.path, backup);
                AppState::default()
            }
        };
        state.trim();
        guard.cache = Some(state);
        Ok(())
    }
}

fn write_atomic(path: &Path, state: &AppState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, path)?;
    Ok(())
}
