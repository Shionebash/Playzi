use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Local;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::state::StateStore;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub source: String,
    pub external_url: String,
    pub external_id: String,
    pub import_mode: String,
    pub last_sync: String,
    pub item_count: usize,
    pub thumbnail: String,
    pub items: Vec<Value>,
    pub created_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

fn first_thumb(items: &[Value]) -> String {
    items
        .iter()
        .filter_map(|i| i.get("thumbnail").and_then(|v| v.as_str()))
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

pub fn normalize_playlist(raw: &Value) -> Playlist {
    let mut pl = serde_json::from_value::<Playlist>(raw.clone()).unwrap_or_default();
    if pl.source.is_empty() {
        pl.source = "playzi".to_string();
    }
    if pl.import_mode.is_empty() {
        pl.import_mode = "local".to_string();
    }
    if pl.thumbnail.is_empty() {
        pl.thumbnail = first_thumb(&pl.items);
    }
    pl.item_count = pl.items.len();
    pl
}

fn refresh_playlist_meta_value(pl: &mut Value) {
    let items_len = pl
        .get("items")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let thumb_empty = pl
        .get("thumbnail")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true);
    let derived_thumb = if thumb_empty {
        pl.get("items")
            .and_then(|v| v.as_array())
            .map(|a| first_thumb(a))
            .unwrap_or_default()
    } else {
        String::new()
    };

    if let Some(obj) = pl.as_object_mut() {
        obj.insert("itemCount".into(), Value::from(items_len));
        if thumb_empty && !derived_thumb.is_empty() {
            obj.insert("thumbnail".into(), Value::String(derived_thumb));
        }
    }
}

pub fn list_playlists(store: &StateStore) -> Result<Vec<Playlist>> {
    let data = store.read()?;
    Ok(data.playlists.iter().map(normalize_playlist).collect())
}

pub fn create_playlist(store: &StateStore, name: &str) -> Result<Playlist> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err(CoreError::Validation("Nombre de playlist vacio".into()));
    }
    let playlist = Playlist {
        id: Uuid::new_v4().to_string(),
        name: clean.to_string(),
        source: "playzi".to_string(),
        external_url: String::new(),
        external_id: String::new(),
        import_mode: "local".to_string(),
        last_sync: String::new(),
        item_count: 0,
        thumbnail: String::new(),
        items: Vec::new(),
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        extra: BTreeMap::new(),
    };
    let val = serde_json::to_value(&playlist)?;
    store.update(|data| {
        data.playlists.push(val);
        Ok(())
    })?;
    Ok(playlist)
}

pub fn delete_playlist(store: &StateStore, playlist_id: &str) -> Result<()> {
    store.update(|data| {
        data.playlists
            .retain(|p| p.get("id").and_then(|v| v.as_str()) != Some(playlist_id));
        Ok(())
    })
}

pub fn add_item(store: &StateStore, playlist_id: &str, item: Value) -> Result<Value> {
    let item_url = item.get("url").and_then(|v| v.as_str()).map(str::to_string);
    let item_clone = item.clone();
    store.update(|data| {
        for pl in &mut data.playlists {
            if pl.get("id").and_then(|v| v.as_str()) == Some(playlist_id) {
                let items = pl
                    .as_object_mut()
                    .and_then(|o| o.entry("items").or_insert_with(|| Value::Array(vec![])).as_array_mut());
                if let Some(items) = items {
                    let exists = item_url.as_deref().is_some_and(|u| {
                        items.iter().any(|i| i.get("url").and_then(|v| v.as_str()) == Some(u))
                    });
                    if !exists {
                        items.push(item_clone.clone());
                    }
                }
                refresh_playlist_meta_value(pl);
            }
        }
        Ok(())
    })?;
    Ok(item)
}

pub fn remove_item(store: &StateStore, playlist_id: &str, url: &str) -> Result<()> {
    store.update(|data| {
        for pl in &mut data.playlists {
            if pl.get("id").and_then(|v| v.as_str()) == Some(playlist_id) {
                if let Some(items) = pl.get_mut("items").and_then(|v| v.as_array_mut()) {
                    items.retain(|i| i.get("url").and_then(|v| v.as_str()) != Some(url));
                }
                refresh_playlist_meta_value(pl);
            }
        }
        Ok(())
    })
}

pub fn rename_playlist(store: &StateStore, playlist_id: &str, name: &str) -> Result<Option<Playlist>> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err(CoreError::Validation("Nombre de playlist vacio".into()));
    }
    store.update(|data| {
        let mut updated: Option<Playlist> = None;
        for pl in &mut data.playlists {
            if pl.get("id").and_then(|v| v.as_str()) == Some(playlist_id) {
                if let Some(obj) = pl.as_object_mut() {
                    obj.insert("name".into(), Value::String(clean.to_string()));
                }
                updated = Some(normalize_playlist(pl));
            }
        }
        for imp in &mut data.imported_playlists {
            if imp.get("id").and_then(|v| v.as_str()) == Some(playlist_id) {
                if let Some(obj) = imp.as_object_mut() {
                    obj.insert("name".into(), Value::String(clean.to_string()));
                }
            }
        }
        Ok(updated)
    })
}

pub fn reorder_items(store: &StateStore, playlist_id: &str, urls: &[String]) -> Result<()> {
    store.update(|data| {
        let Some(pl) = data
            .playlists
            .iter_mut()
            .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(playlist_id))
        else {
            return Err(CoreError::NotFound("Playlist no encontrada".into()));
        };

        let existing = pl
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let by_url: HashMap<String, Value> = existing
            .iter()
            .filter_map(|item| {
                item.get("url")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|u| (u.to_string(), item.clone()))
            })
            .collect();

        let url_set: HashSet<&str> = urls.iter().map(String::as_str).collect();
        let mut reordered: Vec<Value> = urls
            .iter()
            .filter_map(|u| by_url.get(u).cloned())
            .collect();

        for item in existing {
            let keep = item
                .get("url")
                .and_then(|v| v.as_str())
                .map(|u| !url_set.contains(u))
                .unwrap_or(true);
            if keep {
                reordered.push(item);
            }
        }

        if let Some(obj) = pl.as_object_mut() {
            obj.insert("items".into(), Value::Array(reordered));
        }
        refresh_playlist_meta_value(pl);
        Ok(())
    })
}
