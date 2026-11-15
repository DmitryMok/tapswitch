//! Загрузка и проверка сценариев из YAML-файлов (перенос `scenarios.py`).
//! Формат тот же, что у Python-версии.

use serde_yaml_ng::Value as Yaml;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::actions;
use crate::gestures::Gesture;

#[derive(Clone, Debug)]
pub struct ActionCall {
    pub name: String,
    pub params: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct Scenario {
    /// Название для выбора сценария на странице управления (следующая задача).
    #[allow(dead_code)]
    pub name: String,
    pub label: String,
    /// Шаблон имени иконки, например {session.role|}.
    pub icon: String,
    /// Шаблон надписи над иконкой.
    pub icon_title: String,
    pub on: HashMap<Gesture, Vec<ActionCall>>,
    /// Клик срабатывает сразу при отпускании, не дожидаясь второго нажатия;
    /// двойной клик тогда идёт после уже выполненного клика.
    pub eager_click: bool,
}

const FIELDS: [&str; 6] = ["name", "label", "icon", "icon_title", "eager_click", "gestures"];

/// Скаляр YAML как текст — так, как его прочла бы Python-версия.
fn text(value: &Yaml) -> String {
    match value {
        Yaml::String(s) => s.clone(),
        Yaml::Bool(true) => "True".into(),
        Yaml::Bool(false) => "False".into(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Null => String::new(),
        other => serde_yaml_ng::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

/// Поле сценария: многоязычное {ru: …, en: …} — на текущем языке, затем на языке по
/// умолчанию, затем первое; простая строка — как есть.
fn localized(value: &Yaml) -> String {
    let Some(map) = value.as_mapping() else { return text(value) };
    let pick = |code: &str| map.get(code).map(text);
    pick(crate::i18n::current())
        .or_else(|| pick(crate::i18n::DEFAULT))
        .or_else(|| map.values().next().map(text))
        .unwrap_or_default()
}

fn truthy(value: &Yaml) -> bool {
    match value {
        Yaml::Bool(b) => *b,
        Yaml::Null => false,
        Yaml::String(s) => !s.is_empty(),
        Yaml::Number(n) => n.as_f64() != Some(0.0),
        Yaml::Sequence(s) => !s.is_empty(),
        Yaml::Mapping(m) => !m.is_empty(),
        Yaml::Tagged(t) => truthy(&t.value),
    }
}

fn parse_call(raw: &Yaml) -> Result<ActionCall, String> {
    let Some(map) = raw.as_mapping().filter(|m| m.contains_key("action")) else {
        return Err(format!("шаг без поля action: {}", text(raw)));
    };
    let name = text(map.get("action").unwrap());
    let Some(expected) = actions::params_of(&name) else {
        return Err(format!("неизвестное действие «{name}»"));
    };
    let mut params = BTreeMap::new();
    for (key, value) in map {
        let key = key.as_str().ok_or_else(|| format!("действие «{name}»: имя параметра не текст"))?;
        if key == "action" {
            continue;
        }
        if !expected.contains(&key) {
            return Err(format!("действие «{name}»: лишний параметр «{key}»"));
        }
        params.insert(key.to_string(), text(value));
    }
    if let Some(missing) = expected.iter().find(|p| !params.contains_key(**p)) {
        return Err(format!("действие «{name}»: нет параметра «{missing}»"));
    }
    Ok(ActionCall { name, params })
}

pub fn parse_scenario(id: &str, data: &Yaml) -> Result<Scenario, String> {
    let Some(map) = data.as_mapping() else {
        return Err("файл должен содержать словарь".into());
    };
    let mut unknown: Vec<String> = map
        .keys()
        .filter(|k| !k.as_str().is_some_and(|k| FIELDS.contains(&k)))
        .map(text)
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!("неизвестные поля: {unknown:?}"));
    }
    let mut on = HashMap::new();
    match map.get("gestures") {
        None | Some(Yaml::Null) => {}
        Some(Yaml::Mapping(gestures)) => {
            for (gesture, calls) in gestures {
                let name = text(gesture);
                let gesture = Gesture::parse(&name).ok_or_else(|| format!("неизвестный жест «{name}»"))?;
                let calls = calls.as_sequence().ok_or_else(|| format!("жест «{name}»: нужен список шагов"))?;
                on.insert(gesture, calls.iter().map(parse_call).collect::<Result<_, _>>()?);
            }
        }
        Some(_) => return Err("gestures: нужен словарь жестов".into()),
    }
    let field = |name: &str| map.get(name).map(localized);
    Ok(Scenario {
        name: field("name").unwrap_or_else(|| id.to_string()),
        label: field("label").unwrap_or_default(),
        icon: field("icon").unwrap_or_default(),
        icon_title: field("icon_title").unwrap_or_default(),
        on,
        eager_click: map.get("eager_click").is_some_and(truthy),
    })
}

/// Встроенные сценарии и поверх них — файлы из папки (свои и замены встроенных).
pub fn load_all(folder: &Path) -> (BTreeMap<String, Scenario>, Vec<String>) {
    let mut scenarios = BTreeMap::new();
    let mut errors = Vec::new();
    for (id, text) in crate::places::SCENARIOS {
        match serde_yaml_ng::from_str::<Yaml>(text).map_err(|e| e.to_string()).and_then(|d| parse_scenario(id, &d)) {
            Ok(scenario) => {
                scenarios.insert(id.to_string(), scenario);
            }
            Err(error) => errors.push(format!("встроенный {id}: {error}")),
        }
    }
    let (own, own_errors) = load_scenarios(folder);
    scenarios.extend(own);
    errors.extend(own_errors);
    (scenarios, errors)
}

/// Рабочие сценарии и ошибки: один плохой файл не ломает остальные.
pub fn load_scenarios(folder: &Path) -> (BTreeMap<String, Scenario>, Vec<String>) {
    let mut scenarios = BTreeMap::new();
    let mut errors = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(folder)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.retain(|p| p.extension().is_some_and(|e| e == "yaml"));
    paths.sort();
    for path in paths {
        let id = path.file_stem().unwrap().to_string_lossy().into_owned();
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| serde_yaml_ng::from_str::<Yaml>(&text).map_err(|e| e.to_string()))
            .and_then(|data| parse_scenario(&id, &data));
        match loaded {
            Ok(scenario) => {
                scenarios.insert(id, scenario);
            }
            Err(error) => errors.push(format!("{file}: {error}")),
        }
    }
    (scenarios, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn shipped() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/scenarios")
    }

    #[test]
    fn shipped_scenarios_are_valid() {
        let (scenarios, errors) = load_scenarios(&shipped());
        assert_eq!(errors, Vec::<String>::new());
        let ids: Vec<&str> = scenarios.keys().map(String::as_str).collect();
        assert_eq!(ids, ["auto", "dictation", "herdr_session", "journal", "keys", "label_demo", "remember_session"]);
        assert_eq!(scenarios["keys"].on[&Gesture::Click][0].name, "send_keys");
        assert!(scenarios["auto"].eager_click);
        assert_eq!(scenarios["auto"].on[&Gesture::HoldStart][0].name, "read_current_session");
    }

    #[test]
    fn embedded_scenarios_load_without_folder() {
        let (scenarios, errors) = load_all(Path::new("нет такой папки"));
        assert!(errors.is_empty(), "{errors:?}");
        assert!(scenarios.contains_key("auto") && scenarios.contains_key("remember_session"));
    }

    #[test]
    fn multilingual_fields_pick_current_language() {
        // Язык в тестах — по умолчанию (русский).
        let data: Yaml = serde_yaml_ng::from_str("name: {ru: Сессия, en: Session}\nlabel: {en: Only English}\n").unwrap();
        let scenario = parse_scenario("x", &data).unwrap();
        assert_eq!((scenario.name.as_str(), scenario.label.as_str()), ("Сессия", "Only English"));
    }

    #[test]
    fn invalid_scenarios_are_rejected() {
        for bad in [
            "gestures: {swipe: []}",
            "gestures: {click: [{action: no_such_action}]}",
            "gestures: {click: [{action: log}]}",
            "gestures: {click: [{action: log, message: x, extra: 1}]}",
            "gestures: {click: log}",
            "true: {click: []}",
            "[1, 2]",
        ] {
            let data: Yaml = serde_yaml_ng::from_str(bad).unwrap();
            assert!(parse_scenario("bad", &data).is_err(), "{bad}");
        }
    }

    #[test]
    fn bad_file_does_not_break_other_scenarios() {
        let dir = std::env::temp_dir().join(format!("agent_control_scenarios_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.yaml"), "name: ok\n").unwrap();
        std::fs::write(dir.join("bad.yaml"), "gestures: {swipe: []}\n").unwrap();
        let (scenarios, errors) = load_scenarios(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(scenarios.keys().collect::<Vec<_>>(), ["good"]);
        assert_eq!(errors.len(), 1);
    }
}
