use std::sync::OnceLock;

use chrono::Local;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::Result;
use crate::state::StateStore;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEvent {
    pub id: String,
    pub action: String,
    pub source: String,
    pub url: String,
    pub target: String,
    pub title: String,
    pub thumbnail: String,
    pub channel: String,
    pub duration_text: String,
    pub date: String,
    pub player: String,
    pub kind: String,
}

fn video_id_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:v=|youtu\.be/|shorts/)([A-Za-z0-9_-]{11})").expect("valid regex")
    })
}

pub fn derive_thumbnail(url: &str) -> String {
    video_id_regex()
        .captures(url)
        .and_then(|c| c.get(1))
        .map(|m| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", m.as_str()))
        .unwrap_or_default()
}

pub fn source_for_url(url: &str) -> String {
    let lower = url.to_lowercase();
    if lower.contains("music.youtube.com") {
        "music".to_string()
    } else if lower.contains("youtube.com") || lower.contains("youtu.be") {
        "youtube".to_string()
    } else if !lower.is_empty() {
        "local".to_string()
    } else {
        "playzi".to_string()
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty())
}

pub fn normalize_history_value(raw: &Value) -> HistoryEvent {
    let action = str_field(raw, "action")
        .or_else(|| str_field(raw, "kind"))
        .unwrap_or("evento")
        .to_string();
    let url_or_target = str_field(raw, "url")
        .or_else(|| str_field(raw, "target"))
        .unwrap_or("");
    let source = str_field(raw, "source")
        .map(str::to_string)
        .unwrap_or_else(|| source_for_url(url_or_target));
    let date = str_field(raw, "date")
        .or_else(|| str_field(raw, "createdAt"))
        .or_else(|| str_field(raw, "updatedAt"))
        .unwrap_or("")
        .to_string();
    let id = str_field(raw, "id")
        .map(str::to_string)
        .unwrap_or_else(|| format!("hist-{}", Uuid::new_v4()));
    let url = if url_or_target.starts_with("http") {
        url_or_target.to_string()
    } else {
        str_field(raw, "url").unwrap_or("").to_string()
    };
    let target = str_field(raw, "target")
        .unwrap_or(url_or_target)
        .to_string();
    let title = str_field(raw, "title")
        .or_else(|| str_field(raw, "target"))
        .or_else(|| str_field(raw, "url"))
        .unwrap_or("Sin titulo")
        .to_string();
    let thumbnail = str_field(raw, "thumbnail")
        .map(str::to_string)
        .unwrap_or_else(|| derive_thumbnail(url_or_target));
    let channel = str_field(raw, "channel")
        .or_else(|| str_field(raw, "sourceChannelName"))
        .unwrap_or("")
        .to_string();

    HistoryEvent {
        id,
        action,
        source,
        url,
        target,
        title,
        thumbnail,
        channel,
        duration_text: str_field(raw, "durationText").unwrap_or("").to_string(),
        date,
        player: str_field(raw, "player").unwrap_or("").to_string(),
        kind: str_field(raw, "kind").unwrap_or("").to_string(),
    }
}

pub fn list_history(store: &StateStore) -> Result<Vec<HistoryEvent>> {
    let data = store.read()?;
    Ok(data.history.iter().map(normalize_history_value).collect())
}

pub fn add_history_event(store: &StateStore, mut raw: Value) -> Result<HistoryEvent> {
    if let Some(obj) = raw.as_object_mut() {
        if obj.get("id").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            obj.insert("id".into(), Value::String(format!("hist-{}", Uuid::new_v4())));
        }
        if obj.get("date").and_then(|v| v.as_str()).unwrap_or("").is_empty() {
            let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
            obj.insert("date".into(), Value::String(now));
        }
    }
    let normalized = normalize_history_value(&raw);
    let val = serde_json::to_value(&normalized)?;
    store.update(|data| {
        data.history.insert(0, val);
        Ok(())
    })?;
    Ok(normalized)
}

pub fn delete_history_event(store: &StateStore, event_id: &str) -> Result<()> {
    store.update(|data| {
        data.history.retain(|item| {
            item.get("id")
                .map(|v| match v {
                    Value::String(s) => s.as_str() != event_id,
                    other => other.to_string() != event_id,
                })
                .unwrap_or(true)
        });
        Ok(())
    })
}
