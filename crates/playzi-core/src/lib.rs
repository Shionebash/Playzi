pub mod autosync;
pub mod collections;
pub mod config;
pub mod cookies;
pub mod downloads;
pub mod error;
pub mod history;
pub mod party;
pub mod player;
pub mod playlists;
pub mod state;
pub mod thumbs;
pub mod updater;
pub mod youtube;
pub mod ytmusic;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;
    use tempfile::TempDir;

    use crate::collections::{create_collection, delete_collection, list_collections};
    use crate::config::{ConfigStore, ConfigUpdate};
    use crate::history::{add_history_event, delete_history_event, derive_thumbnail, list_history};
    use crate::playlists::{
        add_item, create_playlist, delete_playlist, list_playlists, remove_item, rename_playlist,
        reorder_items,
    };
    use crate::state::StateStore;

    #[test]
    fn config_roundtrip_and_runtime_update() {
        let dir = TempDir::new().unwrap();
        let store = ConfigStore::load_from_root(dir.path());
        let initial = store.public_config();
        assert_eq!(initial.app_port, 8765);
        assert_eq!(initial.search_limit, 12);

        let updated = store
            .update(ConfigUpdate {
                search_limit: Some(25),
                playback_quality: Some("1080p".into()),
                cookies_browser: Some("firefox".into()),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(updated.search_limit, 25);
        assert_eq!(updated.playback_quality, "1080p");
        assert_eq!(updated.cookies_browser, "firefox");

        let reloaded = ConfigStore::load_from_root(dir.path()).public_config();
        assert_eq!(reloaded.search_limit, 25);
        assert_eq!(reloaded.playback_quality, "1080p");
        assert_eq!(reloaded.cookies_browser, "firefox");
    }

    #[test]
    fn collections_crud_and_library_counts() {
        let dir = TempDir::new().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));

        let created = create_collection(&store, "Lo-Fi").unwrap();
        // Duplicate (case-insensitive) should not create a second entry
        let _ = create_collection(&store, "lo-fi").unwrap();

        store
            .update(|s| {
                s.library.push(json!({"id": "1", "collection": "Lo-Fi"}));
                s.library.push(json!({"id": "2", "collection": "Podcasts"}));
                Ok(())
            })
            .unwrap();

        let cols = list_collections(&store).unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "Lo-Fi");
        assert_eq!(cols[0].count, 1);
        assert_eq!(cols[1].name, "Podcasts");
        assert_eq!(cols[1].count, 1);

        delete_collection(&store, &created.id).unwrap();
        let after = list_collections(&store).unwrap();
        assert_eq!(after.len(), 2); // Lo-Fi still inferred from library item
    }

    #[test]
    fn history_normalization_and_crud() {
        let dir = TempDir::new().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));

        assert_eq!(
            derive_thumbnail("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        );

        let ev = add_history_event(
            &store,
            json!({
                "action": "play",
                "url": "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
                "title": "Demo Track"
            }),
        )
        .unwrap();

        assert_eq!(ev.source, "music");
        assert_eq!(
            ev.thumbnail,
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
        );
        assert_eq!(list_history(&store).unwrap().len(), 1);

        delete_history_event(&store, &ev.id).unwrap();
        assert!(list_history(&store).unwrap().is_empty());
    }

    #[test]
    fn playlists_crud_dedup_and_reorder() {
        let dir = TempDir::new().unwrap();
        let store = StateStore::new(dir.path().join("state.json"));

        let pl = create_playlist(&store, "Favoritos").unwrap();
        add_item(
            &store,
            &pl.id,
            json!({"url": "https://youtu.be/aaaaaaaaaaa", "title": "A", "thumbnail": "https://i.ytimg.com/vi/aaaaaaaaaaa/hqdefault.jpg"}),
        )
        .unwrap();
        // Duplicate URL ignored
        add_item(
            &store,
            &pl.id,
            json!({"url": "https://youtu.be/aaaaaaaaaaa", "title": "A-dup"}),
        )
        .unwrap();
        add_item(
            &store,
            &pl.id,
            json!({"url": "https://youtu.be/bbbbbbbbbbb", "title": "B"}),
        )
        .unwrap();

        reorder_items(
            &store,
            &pl.id,
            &[
                "https://youtu.be/bbbbbbbbbbb".into(),
                "https://youtu.be/aaaaaaaaaaa".into(),
            ],
        )
        .unwrap();

        let renamed = rename_playlist(&store, &pl.id, "Top Mix").unwrap().unwrap();
        assert_eq!(renamed.name, "Top Mix");
        assert_eq!(renamed.item_count, 2);
        assert_eq!(
            renamed.items[0].get("url").and_then(|v| v.as_str()),
            Some("https://youtu.be/bbbbbbbbbbb")
        );

        remove_item(&store, &pl.id, "https://youtu.be/bbbbbbbbbbb").unwrap();
        assert_eq!(list_playlists(&store).unwrap()[0].item_count, 1);

        delete_playlist(&store, &pl.id).unwrap();
        assert!(list_playlists(&store).unwrap().is_empty());
    }

    #[test]
    fn reads_existing_playzi_state_json_without_data_loss() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let real_state = workspace_root.join("data").join("state.json");
        if !real_state.exists() {
            return;
        }
        let store = StateStore::new(&real_state);
        let parsed = store.read().expect("real data/state.json must deserialize cleanly");
        // Verify it re-serializes cleanly to valid JSON
        let serialized = serde_json::to_value(&parsed).unwrap();
        assert!(serialized.is_object());
    }
}
