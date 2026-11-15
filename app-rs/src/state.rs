//! `data/state.json` — общий с Python-версией файл состояния кнопок (перенос `state.py`).
//!
//! Значения принадлежат кнопке и общие для всех её сценариев: сессию, запомненную
//! одним сценарием, открывает другой. Значения с именем на «_» не сохраняются.
//! Поля, которых Rust-версия не знает, сохраняются как были.
//!
//! Профили — наборы кнопок (номера 1–6). Текущий лежит в `keys`, как и раньше, поэтому
//! Python-версия видит его как обычный набор; остальные — в `settings.profiles`
//! (Python-версия переписывает `settings` целиком и их не теряет).

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

const VERSION: u64 = 2;
const WEB_KEY_SETTING: &str = "web_key";
/// Сколько профилей кнопок.
pub const PROFILES: usize = 6;
const PROFILE_SETTING: &str = "profile";
const PROFILES_SETTING: &str = "profiles";

pub struct State {
    path: PathBuf,
    data: Value,
}

impl State {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut data = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                json!({"version": VERSION, "keys": {}, "settings": {}})
            }
            Err(e) => return Err(e.to_string()),
        };
        if data.get("version").and_then(Value::as_u64) != Some(VERSION) {
            return Err(format!(
                "версия файла не {VERSION}: откройте его один раз Python-версией, она обновит формат"
            ));
        }
        for name in ["keys", "settings"] {
            if !data[name].is_object() {
                data[name] = json!({});
            }
        }
        Ok(Self { path: path.to_path_buf(), data })
    }

    fn entry(&self, key: usize) -> Option<&Value> {
        self.data["keys"].get(key.to_string())
    }

    fn entry_mut(&mut self, key: usize) -> &mut Map<String, Value> {
        let keys = self.data["keys"].as_object_mut().unwrap();
        let entry = keys.entry(key.to_string()).or_insert_with(|| json!({"scenario": null, "values": {}}));
        if !entry.is_object() {
            *entry = json!({"scenario": null, "values": {}});
        }
        let entry = entry.as_object_mut().unwrap();
        if !entry.get("values").is_some_and(Value::is_object) {
            entry.insert("values".into(), json!({}));
        }
        entry
    }

    pub fn scenario_of(&self, key: usize) -> Option<&str> {
        self.entry(key)?.get("scenario")?.as_str().filter(|s| !s.is_empty())
    }

    pub fn set_scenario(&mut self, key: usize, scenario: Option<&str>) {
        self.entry_mut(key).insert("scenario".into(), json!(scenario));
    }

    /// Поменять кнопки местами: сценарий и значения переходят вместе.
    pub fn swap(&mut self, first: usize, second: usize) {
        let a = Value::Object(self.entry_mut(first).clone());
        let b = Value::Object(self.entry_mut(second).clone());
        let keys = self.data["keys"].as_object_mut().unwrap();
        keys.insert(first.to_string(), b);
        keys.insert(second.to_string(), a);
    }

    /// Значения кнопки (например, `session`).
    pub fn values(&self, key: usize) -> &Map<String, Value> {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        self.entry(key)
            .and_then(|e| e.get("values"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
    }

    pub fn values_mut(&mut self, key: usize) -> &mut Map<String, Value> {
        self.entry_mut(key).get_mut("values").and_then(Value::as_object_mut).unwrap()
    }

    /// Номер текущего профиля (с нуля).
    pub fn profile(&self) -> usize {
        self.setting(PROFILE_SETTING).as_u64().map(|p| p as usize).filter(|&p| p < PROFILES).unwrap_or(0)
    }

    /// Кнопки профиля: номер кнопки → {scenario, values}; у текущего — те, что в работе.
    pub fn profile_keys(&self, profile: usize) -> Map<String, Value> {
        let keys = if profile == self.profile() {
            &self.data["keys"]
        } else {
            &self.data["settings"][PROFILES_SETTING][profile.to_string()]
        };
        keys.as_object().cloned().unwrap_or_default()
    }

    /// Сделать профиль текущим: кнопки текущего уходят на хранение (без временных
    /// значений), кнопки выбранного становятся рабочими. false — профиль уже текущий или его нет.
    pub fn switch_profile(&mut self, profile: usize) -> bool {
        let current = self.profile();
        if profile == current || profile >= PROFILES {
            return false;
        }
        let mut keys = std::mem::take(&mut self.data["keys"]);
        strip_temporary(&mut keys);
        let settings = self.data["settings"].as_object_mut().unwrap();
        let stored = settings.entry(PROFILES_SETTING).or_insert_with(|| json!({}));
        if !stored.is_object() {
            *stored = json!({});
        }
        let stored = stored.as_object_mut().unwrap();
        let next = stored.remove(&profile.to_string()).filter(Value::is_object).unwrap_or_else(|| json!({}));
        stored.insert(current.to_string(), keys);
        self.data["keys"] = next;
        self.data["settings"][PROFILE_SETTING] = json!(profile);
        true
    }

    pub fn setting(&self, name: &str) -> &Value {
        &self.data["settings"][name]
    }

    pub fn set_setting(&mut self, name: &str, value: Value) {
        self.data["settings"][name] = value;
    }

    /// Ключ доступа к панели; новый создаётся и сохраняется, если его нет.
    pub fn web_key(&mut self) -> Result<String, String> {
        if let Some(key) = self.setting(WEB_KEY_SETTING).as_str().filter(|k| !k.is_empty()) {
            return Ok(key.to_string());
        }
        let key = crate::win::random_hex(16);
        self.data["settings"][WEB_KEY_SETTING] = Value::String(key.clone());
        self.save()?;
        Ok(key)
    }

    pub fn save(&self) -> Result<(), String> {
        let mut data = self.data.clone();
        strip_temporary(&mut data["keys"]);
        let text = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
        std::fs::rename(&temporary, &self.path).map_err(|e| e.to_string())
    }
}

/// Убрать временные значения (имя на «_») у всех кнопок набора.
fn strip_temporary(keys: &mut Value) {
    for entry in keys.as_object_mut().into_iter().flat_map(|k| k.values_mut()) {
        if let Some(values) = entry.get_mut("values").and_then(Value::as_object_mut) {
            values.retain(|name, _| !name.starts_with('_'));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unknown_fields_and_drops_temporary_values() {
        let dir = std::env::temp_dir().join(format!("agent_control_state_{}", std::process::id()));
        let path = dir.join("state.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &path,
            r#"{"version": 2, "keys": {"0": {"scenario": "auto", "values": {"session": {"name": "a", "extra": 1}}}},
               "settings": {"icon_project": true, "web_key": "k"}, "other": [1]}"#,
        )
        .unwrap();
        let mut state = State::load(&path).unwrap();
        state.values_mut(0).insert("_waiting".into(), json!("x"));
        state.values_mut(1).insert("text".into(), json!("t"));
        state.swap(0, 1);
        state.save().unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(saved["other"], json!([1]));
        assert_eq!(saved["settings"]["icon_project"], true);
        assert_eq!(saved["keys"]["1"], json!({"scenario": "auto", "values": {"session": {"name": "a", "extra": 1}}}));
        assert_eq!(saved["keys"]["0"]["values"], json!({"text": "t"}));
    }

    #[test]
    fn profiles_keep_their_keys_and_survive_saving() {
        let dir = std::env::temp_dir().join(format!("agent_control_profiles_{}", std::process::id()));
        let path = dir.join("state.json");
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = State::load(&path).unwrap();
        state.set_scenario(0, Some("auto"));
        state.values_mut(0).insert("session".into(), json!({"name": "a"}));
        state.values_mut(0).insert("_held".into(), json!("x"));
        assert_eq!(state.profile(), 0);
        assert!(!state.switch_profile(0) && !state.switch_profile(PROFILES));
        assert!(state.switch_profile(2));
        assert_eq!(state.profile(), 2);
        assert!(state.values(0).is_empty()); // новый профиль пустой
        state.values_mut(1).insert("keys".into(), json!("Win+H"));
        assert_eq!(state.profile_keys(0)["0"]["values"], json!({"session": {"name": "a"}}));
        assert_eq!(state.profile_keys(2)["1"]["values"], json!({"keys": "Win+H"}));
        state.save().unwrap();
        let mut state = State::load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(state.profile(), 2);
        assert!(state.switch_profile(0));
        assert_eq!(state.values(0)["session"]["name"], "a");
        assert_eq!(state.profile_keys(2)["1"]["values"]["keys"], "Win+H");
    }
}
