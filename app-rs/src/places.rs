//! Где лежат данные и откуда берутся ресурсы.
//!
//! Всё по умолчанию встроено в exe: страницы, видео, иконки ролей, сценарии, `config.yaml`.
//! Данные пользователя — в `%APPDATA%\Tapswitch\` (прежняя `AgentControl` переносится): `state.json`, `config.yaml` (только
//! изменённые настройки), `scenarios/` (свои и замены встроенных), `icons/`, журнал.
//! Режим разработки: если выше exe найдена `app/` с `config.yaml` (или задана
//! `AGENT_CONTROL_APP`) — та же раскладка берётся из неё, данные — из `app/data`.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
}

pub const DEFAULT_CONFIG: &str = include_str!("../defaults/config.yaml");
pub const SCENARIOS: &[(&str, &str)] = embedded::SCENARIOS;
pub const LOCALES: &[(&str, &str)] = embedded::LOCALES;

/// Написания ролей, которые встречаются в названиях папок.
const ALIASES: &[(&str, &str)] = &[("lawer", "lawyer"), ("commiter", "committer")];

pub struct Places {
    /// Папка `app/` в режиме разработки; None — обычный режим с данными в APPDATA.
    pub app_dir: Option<PathBuf>,
    pub config: PathBuf,
    pub data: PathBuf,
    pub scenarios: PathBuf,
    pub icons: PathBuf,
}

/// Папка данных в `%APPDATA%`. До переименования программа звалась Agent Control: её папка
/// переносится в новую; если перенести нельзя (занята прежней версией) — используется как есть.
fn data_root(appdata: &Path) -> PathBuf {
    let root = appdata.join("Tapswitch");
    let old = appdata.join("AgentControl");
    if !root.exists() && old.is_dir() && std::fs::rename(&old, &root).is_err() {
        return old;
    }
    root
}

impl Places {
    pub fn find() -> Result<Self, String> {
        if let Some(app) = find_app_dir() {
            return Ok(Self::in_folder(&app, &app.join("data"), Some(app.clone())));
        }
        let appdata = std::env::var_os("APPDATA").ok_or("не задана переменная APPDATA")?;
        let root = data_root(&PathBuf::from(appdata));
        Ok(Self::in_folder(&root, &root, None))
    }

    fn in_folder(root: &Path, data: &Path, app_dir: Option<PathBuf>) -> Self {
        Self {
            app_dir,
            config: root.join("config.yaml"),
            data: data.to_path_buf(),
            scenarios: root.join("scenarios"),
            icons: root.join("icons"),
        }
    }

    pub fn state(&self) -> PathBuf {
        self.data.join("state.json")
    }

    /// Свои картинки кнопок (PNG, имя — хэш содержимого).
    pub fn user_icons(&self) -> PathBuf {
        self.data.join("usericons")
    }

    pub fn log(&self) -> PathBuf {
        self.data.join("tapswitch.log")
    }

    pub fn describe(&self) -> String {
        match &self.app_dir {
            Some(app) => format!("режим разработки, папка {}", app.display()),
            None => format!("данные в {}", self.data.display()),
        }
    }
}

/// Папка приложения: из переменной AGENT_CONTROL_APP; иначе выше exe — папка с `config.yaml`
/// или её подпапка `app/` (так находится `app/` при запуске из `app-rs/target`).
fn find_app_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("AGENT_CONTROL_APP") {
        return Some(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().ok()?;
    for dir in exe.ancestors().skip(1) {
        if dir.join("config.yaml").is_file() {
            return Some(dir.to_path_buf());
        }
        if dir.join("app").join("config.yaml").is_file() {
            return Some(dir.join("app"));
        }
    }
    None
}

/// Иконки ролей: сначала из папки (свои и правки без пересборки), затем встроенные.
pub struct Icons {
    dir: PathBuf,
}

impl Icons {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Имя файла иконки (без .svg) или None, если имя пустое, небезопасное или иконки нет.
    pub fn find(&self, name: &str) -> Option<String> {
        let name = name.trim().to_lowercase();
        if name.is_empty() || name.contains(['/', '\\', ':', '.']) {
            return None;
        }
        let name = ALIASES.iter().find(|(from, _)| *from == name).map_or(name.as_str(), |(_, to)| to);
        let exists = self.dir.join(format!("{name}.svg")).is_file() || embedded::ICONS.iter().any(|(n, _)| *n == name);
        exists.then(|| name.to_string())
    }

    pub fn read(&self, name: &str) -> Option<Cow<'static, [u8]>> {
        let name = self.find(name)?;
        if let Ok(bytes) = std::fs::read(self.dir.join(format!("{name}.svg"))) {
            return Some(Cow::Owned(bytes));
        }
        embedded::ICONS.iter().find(|(n, _)| *n == name).map(|(_, bytes)| Cow::Borrowed(*bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_has_embedded_icon_and_spellings_are_accepted() {
        let icons = Icons::new(PathBuf::from("нет такой папки"));
        for role in ["dev", "architect", "product", "lawyer", "builder", "build", "committer", "tester", "reviewer", "core"] {
            assert_eq!(icons.find(role).as_deref(), Some(role));
            assert!(icons.read(role).is_some_and(|b| b.windows(4).any(|w| w == b"<svg")));
        }
        assert_eq!(icons.find("lawer").as_deref(), Some("lawyer"));
        assert_eq!(icons.find("Dev").as_deref(), Some("dev"));
        for bad in ["", "  ", "designer", "../icons/dev", "dev.svg"] {
            assert!(icons.find(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn old_data_folder_moves_to_new_name() {
        let appdata = std::env::temp_dir().join(format!("agent_control_appdata_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&appdata);
        std::fs::create_dir_all(appdata.join("AgentControl")).unwrap();
        std::fs::write(appdata.join("AgentControl").join("state.json"), "{}").unwrap();
        assert_eq!(data_root(&appdata), appdata.join("Tapswitch"));
        assert!(appdata.join("Tapswitch").join("state.json").is_file());
        assert!(!appdata.join("AgentControl").exists());
        // Новая уже есть — старая не трогается.
        std::fs::create_dir_all(appdata.join("AgentControl")).unwrap();
        assert_eq!(data_root(&appdata), appdata.join("Tapswitch"));
        assert!(appdata.join("AgentControl").exists());
        std::fs::remove_dir_all(&appdata).unwrap();
    }

    #[test]
    fn scenarios_are_embedded() {
        assert!(SCENARIOS.iter().any(|(id, text)| *id == "auto" && text.contains("switch_session")));
    }
}
