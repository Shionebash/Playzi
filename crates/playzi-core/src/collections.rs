use std::collections::HashMap;

use chrono::Local;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::state::StateStore;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CollectionView {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub count: usize,
}

pub fn list_collections(store: &StateStore) -> Result<Vec<CollectionView>> {
    let data = store.read()?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for item in &data.library {
        if let Some(name) = item.get("collection").and_then(|v| v.as_str()) {
            if !name.is_empty() {
                *counts.entry(name.to_string()).or_insert(0) += 1;
            }
        }
    }

    let mut by_name: HashMap<String, CollectionView> = HashMap::new();
    for raw in &data.collections {
        if let Ok(col) = serde_json::from_value::<Collection>(raw.clone()) {
            let count = counts.get(&col.name).copied().unwrap_or(0);
            by_name.insert(
                col.name.clone(),
                CollectionView {
                    id: col.id,
                    name: col.name,
                    created_at: col.created_at,
                    count,
                },
            );
        }
    }

    for (name, count) in counts {
        by_name.entry(name.clone()).or_insert_with(|| CollectionView {
            id: name.clone(),
            name,
            created_at: String::new(),
            count,
        });
    }

    let mut list: Vec<CollectionView> = by_name.into_values().collect();
    list.sort_by_key(|c| c.name.to_lowercase());
    Ok(list)
}

pub fn create_collection(store: &StateStore, name: &str) -> Result<Collection> {
    let clean = name.trim();
    if clean.is_empty() {
        return Err(CoreError::Validation("Nombre de coleccion vacio".into()));
    }
    let collection = Collection {
        id: Uuid::new_v4().to_string(),
        name: clean.to_string(),
        created_at: Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };
    let col_val = serde_json::to_value(&collection)?;
    let lower = clean.to_lowercase();

    store.update(|data| {
        let exists = data.collections.iter().any(|c| {
            c.get("name")
                .and_then(|v| v.as_str())
                .map(|n| n.to_lowercase() == lower)
                .unwrap_or(false)
        });
        if !exists {
            data.collections.push(col_val);
        }
        Ok(())
    })?;

    Ok(collection)
}

pub fn delete_collection(store: &StateStore, collection_id: &str) -> Result<()> {
    store.update(|data| {
        data.collections
            .retain(|c| c.get("id").and_then(|v| v.as_str()) != Some(collection_id));
        Ok(())
    })
}
