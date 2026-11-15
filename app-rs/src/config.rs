//! Настройки из `config.yaml` — того же файла, что у Python-версии.
//! Читаются только нужные Rust-версии поля, остальные пропускаются.

use serde::Deserialize;
use serde_yaml_ng::Value as Yaml;
use std::path::Path;

use crate::places::DEFAULT_CONFIG;

pub struct Config {
    pub port: u16,
    /// Адрес компьютера для ссылки на телефон, если определился не тот.
    pub host: Option<String>,
    pub rows: usize,
    pub cols: usize,
    pub double_click_ms: u64,
    pub hold_ms: u64,
    pub focus_poll_ms: u64,
    pub status_poll_ms: u64,
    /// Сценарий кнопок, которым свой не выбран.
    pub default_scenario: Option<String>,
    pub servers: Vec<Server>,
    /// Папка данных DeepSeek Harness; None — сессии dsh не привязываются.
    pub dsh_data: Option<String>,
    /// Вкладки браузера через расширение; None — не привязываются.
    pub browser: Option<Browser>,
}

#[derive(Deserialize, Clone)]
pub struct Browser {
    #[serde(default = "browser_port")]
    pub port: u16,
    /// Id расширения, если оно установлено не из папки проекта (свой ключ).
    #[serde(default)]
    pub extension_ids: Vec<String>,
}

fn browser_port() -> u16 {
    3001
}

#[derive(Deserialize, Clone)]
struct Dsh {
    data: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct Server {
    pub id: String,
    pub command: Vec<String>,
    /// herdr в PowerShell агентов не распознаёт: вместо «нет агента» — «неизвестен».
    #[serde(default = "yes")]
    pub detects_agents: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Raw {
    default_scenario: Option<String>,
    layout: Layout,
    timing: Timing,
    web: Web,
    herdr: Herdr,
    dsh: Option<Dsh>,
    browser: Option<Browser>,
}

#[derive(Deserialize)]
#[serde(default)]
struct Layout {
    rows: usize,
    cols: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self { rows: 2, cols: 4 }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct Timing {
    double_click_ms: u64,
    hold_ms: u64,
    focus_poll_ms: u64,
    status_poll_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self { double_click_ms: 300, hold_ms: 500, focus_poll_ms: 1000, status_poll_ms: 2000 }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct Web {
    port: u16,
    host: Option<String>,
}

impl Default for Web {
    fn default() -> Self {
        Self { port: 3000, host: None }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Herdr {
    servers: Vec<Server>,
}

/// Слить настройки: разделы — по полям, остальное (в том числе списки) заменяется.
fn merge(base: &mut Yaml, over: Yaml) {
    match (base, over) {
        (Yaml::Mapping(base), Yaml::Mapping(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// Встроенные настройки по умолчанию и поверх них — файл пользователя, если он есть.
pub fn load(user: &Path) -> Result<Config, String> {
    let mut data: Yaml = serde_yaml_ng::from_str(DEFAULT_CONFIG).map_err(|e| format!("встроенные настройки: {e}"))?;
    match std::fs::read_to_string(user) {
        Ok(text) => {
            let over: Yaml = serde_yaml_ng::from_str(&text).map_err(|e| e.to_string())?;
            if !over.is_null() {
                merge(&mut data, over);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let raw: Raw = serde_yaml_ng::from_value(data).map_err(|e| e.to_string())?;
    if raw.layout.rows == 0 || raw.layout.cols == 0 {
        return Err("layout.rows и layout.cols должны быть больше нуля".into());
    }
    if let Some(server) = raw.herdr.servers.iter().find(|s| s.command.is_empty()) {
        return Err(format!("herdr.servers: у сервера «{}» пустая command", server.id));
    }
    Ok(Config {
        port: raw.web.port,
        host: raw.web.host.filter(|h| !h.trim().is_empty()),
        rows: raw.layout.rows,
        cols: raw.layout.cols,
        double_click_ms: raw.timing.double_click_ms,
        hold_ms: raw.timing.hold_ms,
        focus_poll_ms: raw.timing.focus_poll_ms,
        status_poll_ms: raw.timing.status_poll_ms,
        default_scenario: raw.default_scenario.filter(|s| !s.is_empty()),
        servers: raw.herdr.servers,
        dsh_data: raw.dsh.and_then(|d| d.data).filter(|d| !d.trim().is_empty()),
        browser: raw.browser,
    })
}

/// Порт и адрес из файла пользователя (без встроенных значений) — для страницы управления.
pub fn web_settings(user: &Path) -> (Option<u16>, Option<String>) {
    let data: Yaml = std::fs::read_to_string(user)
        .ok()
        .and_then(|text| serde_yaml_ng::from_str(&text).ok())
        .unwrap_or(Yaml::Null);
    let web = &data["web"];
    (web["port"].as_u64().map(|p| p as u16), web["host"].as_str().map(str::to_string))
}

/// Поменять одно поле раздела верхнего уровня, сохранив остальной текст и комментарии.
/// value None — убрать поле.
fn set_field(text: &str, section: &str, field: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let header = |line: &str| {
        let code = line.split('#').next().unwrap_or("").trim_end();
        code == format!("{section}:")
    };
    let Some(start) = lines.iter().position(|l| header(l)) else {
        if let Some(value) = value {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push(format!("{section}:"));
            lines.push(format!("  {field}: {value}"));
        }
        return lines.join("\n") + "\n";
    };
    // Раздел — строки с отступом (и пустые/комментарии) после заголовка.
    let mut end = start + 1;
    while end < lines.len() {
        let line = &lines[end];
        let blank = line.trim().is_empty() || line.trim_start().starts_with('#');
        if !blank && !line.starts_with(' ') && !line.starts_with('\t') {
            break;
        }
        end += 1;
    }
    let is_field = |line: &str| {
        let trimmed = line.trim_start();
        line.len() > trimmed.len() && trimmed.strip_prefix(field).is_some_and(|rest| rest.starts_with(':'))
    };
    match (lines[start + 1..end].iter().position(|l| is_field(l)), value) {
        (Some(offset), Some(value)) => {
            let line = &lines[start + 1 + offset];
            let indent = &line[..line.len() - line.trim_start().len()];
            let comment = match line.find('#') {
                Some(hash) => line[line[..hash].trim_end().len()..].to_string(),
                None => String::new(),
            };
            lines[start + 1 + offset] = format!("{indent}{field}: {value}{comment}");
        }
        (Some(offset), None) => {
            lines.remove(start + 1 + offset);
        }
        (None, Some(value)) => lines.insert(start + 1, format!("  {field}: {value}")),
        (None, None) => {}
    }
    lines.join("\n") + "\n"
}

/// Сохранить порт и адрес панели в файл пользователя; применяются после перезапуска.
/// host: пустой — определять адрес самой программе.
pub fn save_web(user: &Path, port: Option<u16>, host: Option<&str>) -> Result<(), String> {
    let text = match std::fs::read_to_string(user) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            "# Свои настройки Tapswitch: только то, что отличается от встроенных.\n".into()
        }
        Err(e) => return Err(e.to_string()),
    };
    let mut text = text;
    if let Some(port) = port {
        text = set_field(&text, "web", "port", Some(&port.to_string()));
    }
    if let Some(host) = host {
        if host.chars().any(|c| c.is_whitespace() || "#:'\"".contains(c)) {
            return Err("адрес — имя или IP без пробелов и порта".into());
        }
        text = set_field(&text, "web", "host", Some(host).filter(|h| !h.is_empty()));
    }
    // Проверить, что файл по-прежнему читается, прежде чем записать.
    let check: Yaml = serde_yaml_ng::from_str(&text).map_err(|e| format!("не удалось изменить файл: {e}"))?;
    let mut data: Yaml = serde_yaml_ng::from_str(DEFAULT_CONFIG).map_err(|e| e.to_string())?;
    merge(&mut data, check);
    serde_yaml_ng::from_value::<Raw>(data).map_err(|e| format!("не удалось изменить файл: {e}"))?;
    if let Some(dir) = user.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(user, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_edits_keep_comments_and_other_sections() {
        let text = "# заголовок\nweb:\n  # комментарий\n  port: 3000  # порт\n  # host: x\nherdr:\n  servers: []\n";
        let changed = set_field(text, "web", "port", Some("3999"));
        assert_eq!(changed, text.replace("port: 3000  # порт", "port: 3999  # порт"));
        let with_host = set_field(&changed, "web", "host", Some("192.168.1.5"));
        assert!(with_host.contains("web:\n  host: 192.168.1.5\n  # комментарий"));
        assert_eq!(set_field(&with_host, "web", "host", None), changed);
        assert_eq!(set_field("herdr: {}\n", "web", "port", Some("1")), "herdr: {}\n\nweb:\n  port: 1\n");
    }

    #[test]
    fn saved_web_settings_are_read_back() {
        let path = std::env::temp_dir().join(format!("agent_control_web_{}.yaml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        save_web(&path, Some(3999), Some("10.0.0.2")).unwrap();
        assert_eq!(web_settings(&path), (Some(3999), Some("10.0.0.2".into())));
        assert_eq!(load(&path).unwrap().port, 3999);
        save_web(&path, None, Some("")).unwrap();
        assert_eq!(web_settings(&path), (Some(3999), None));
        assert!(save_web(&path, None, Some("a b")).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn defaults_alone_are_complete() {
        let config = load(Path::new("нет такого файла.yaml")).unwrap();
        assert_eq!((config.port, config.rows, config.cols), (3000, 2, 4));
        assert_eq!(config.servers.len(), 2);
        assert_eq!(config.default_scenario.as_deref(), Some("auto"));
        assert_eq!(config.dsh_data.as_deref(), Some(r"%USERPROFILE%\.dsh"));
        assert_eq!(config.browser.as_ref().map(|b| b.port), Some(3001));
    }

    #[test]
    fn sections_can_be_switched_off() {
        let path = std::env::temp_dir().join(format!("agent_control_config_off_{}.yaml", std::process::id()));
        std::fs::write(&path, "dsh: null\nbrowser: null\n").unwrap();
        let config = load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert!(config.dsh_data.is_none() && config.browser.is_none());
    }

    #[test]
    fn user_file_overrides_only_its_fields() {
        let path = std::env::temp_dir().join(format!("agent_control_config_{}.yaml", std::process::id()));
        std::fs::write(&path, "web: {port: 3999}\nherdr:\n  servers: [{id: only, command: [herdr]}]\n").unwrap();
        let config = load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!((config.port, config.hold_ms), (3999, 500));
        assert_eq!(config.servers.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["only"]);
    }

    #[test]
    fn reads_shipped_config() {
        let config = load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("defaults/config.yaml")).unwrap();
        assert_eq!(config.default_scenario.as_deref(), Some("auto"));
        let ids: Vec<&str> = config.servers.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["windows", "wsl"]);
        assert!(!config.servers[0].detects_agents && config.servers[1].detects_agents);
    }
}
