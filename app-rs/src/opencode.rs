//! Вкладки сессий десктопного приложения OpenCode.
//!
//! Окно у OpenCode одно, его заголовок всегда «OpenCode», поэтому как окно оно запоминалось
//! целиком. Открытые вкладки, их названия, папки и активная вкладка — в данных OpenCode на
//! диске (`%APPDATA%\ai.opencode.desktop\opencode.window.<id>.dat`, обновляется за ~0,2 с
//! после переключения). Ссылки на сессию у OpenCode нет (`opencode://` умеет только
//! открыть проект и новую сессию), поэтому вкладка открывается через UI Automation
//! (`opencode_ui.ps1`): в строке вкладок у каждой — адрес с id сессии.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use crate::sessions::{Backend, Probe, Result, Session, SessionError, describe_pane};
use crate::{win, workers};

pub const KIND: &str = "opencode";
const PROCESS: &str = "OpenCode.exe";
const WINDOW_CLASS: &str = "Chrome_WidgetWin_1";
const AGENT: &str = "opencode";
const APP: &str = "OpenCode";
const TIMEOUT: Duration = Duration::from_secs(10);
const SCRIPT: &str = include_str!("../../app/agent_control/opencode_ui.ps1");

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: String,
    pub title: Option<String>,
    pub directory: Option<String>,
}

/// Вкладки одного окна OpenCode.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowTabs {
    pub tabs: Vec<Tab>,
    /// id сессии активной вкладки; None — открыта главная или другая не-сессия.
    pub active: Option<String>,
    modified: f64,
}

/// id сессии из ключа вкладки «sidecar\n/server/<сервер>/session/<id>».
fn session_of_key(key: &str) -> Option<&str> {
    key.rsplit_once("/session/").map(|(_, id)| id).filter(|id| !id.is_empty())
}

/// Значения в файле OpenCode — JSON, записанный строкой.
fn nested(data: &Value, name: &str) -> Value {
    data[name].as_str().and_then(|text| serde_json::from_str(text).ok()).unwrap_or(Value::Null)
}

pub fn parse_window(data: &Value) -> WindowTabs {
    let info = nested(data, "tabs.info");
    let about = |id: &str| {
        info.as_object()
            .and_then(|all| all.iter().find(|(key, _)| session_of_key(key) == Some(id)).map(|(_, v)| v.clone()))
            .unwrap_or(Value::Null)
    };
    let text = |value: &Value| value.as_str().filter(|t| !t.is_empty()).map(str::to_string);
    let tabs = nested(data, "tabs")
        .as_array()
        .into_iter()
        .flatten()
        .filter(|tab| tab["type"] == "session")
        .filter_map(|tab| tab["sessionId"].as_str())
        .map(|id| {
            let about = about(id);
            Tab { id: id.to_string(), title: text(&about["title"]), directory: text(&about["directory"]) }
        })
        .collect();
    let recent = nested(data, "tabs.recent");
    let active = recent["key"].as_str().and_then(session_of_key).map(str::to_string);
    WindowTabs { tabs, active, modified: 0.0 }
}

/// Данные OpenCode на диске: по файлу на окно.
pub struct OpencodeStore {
    home: PathBuf,
}

impl OpencodeStore {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    /// Окна, от изменённого последним к давним. OpenCode не установлен или как раз
    /// перезаписывает файл — такого окна нет.
    pub fn windows(&self) -> Vec<WindowTabs> {
        let Ok(entries) = std::fs::read_dir(&self.home) else { return vec![] };
        let mut found: Vec<WindowTabs> = entries
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with("opencode.window.") && name.ends_with(".dat")
            })
            .filter_map(|e| {
                let modified = e.metadata().ok()?.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
                let data: Value = serde_json::from_str(&std::fs::read_to_string(e.path()).ok()?).ok()?;
                Some(WindowTabs { modified, ..parse_window(&data) })
            })
            .collect();
        found.sort_by(|a, b| b.modified.total_cmp(&a.modified));
        found
    }

    /// Открытая вкладка сессии в любом окне.
    pub fn tab(&self, id: &str) -> Option<Tab> {
        self.windows().into_iter().flat_map(|w| w.tabs).find(|t| t.id == id)
    }
}

pub trait OpencodeUi: Send + Sync {
    /// Главное окно OpenCode или None, если приложение не запущено.
    fn window(&self) -> Option<i64>;
    /// Вывести окно на передний план.
    fn show(&self, window: i64);
    /// Переключить окно на вкладку сессии.
    fn open_tab(&self, window: i64, id: &str) -> Result<()>;
    /// Окно этого приложения.
    fn is_opencode(&self, window: i64) -> bool;
}

pub fn is_opencode_window(window: i64) -> bool {
    win::class_name(window) == WINDOW_CLASS && {
        let path = win::process_path(window);
        path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case(PROCESS))
    }
}

fn failure(text: impl std::fmt::Display) -> SessionError {
    SessionError::Other(format!("Не удалось обратиться к OpenCode: {text}"))
}

pub struct WindowsOpencodeUi {
    script: PathBuf,
}

impl WindowsOpencodeUi {
    pub fn new() -> Self {
        Self { script: workers::script("opencode_ui.ps1", SCRIPT) }
    }
}

impl OpencodeUi for WindowsOpencodeUi {
    fn window(&self) -> Option<i64> {
        win::top_level_windows(true).into_iter().find(|&w| is_opencode_window(w))
    }

    fn show(&self, window: i64) {
        if !win::bring_to_foreground(window) {
            crate::log::write("Windows не дал вывести на передний план окно OpenCode");
        }
    }

    fn open_tab(&self, window: i64, id: &str) -> Result<()> {
        let window = window.to_string();
        let env = [("AGENT_CONTROL_WINDOW", window.as_str()), ("AGENT_CONTROL_ID", id)];
        let (code, out) = workers::powershell_script(&self.script, &env, TIMEOUT).map_err(failure)?;
        match code {
            0 => Ok(()),
            3 => Err(SessionError::Other(
                "вкладки OpenCode не найдены: окно OpenCode должно быть видно на экране, повторите".into(),
            )),
            5 => Err(SessionError::Lost("вкладка сессии закрыта в OpenCode".into())),
            _ => Err(failure(out.trim().chars().take(300).collect::<String>())),
        }
    }

    fn is_opencode(&self, window: i64) -> bool {
        is_opencode_window(window)
    }
}

/// Папка, проект и роль из папки сессии; название вкладки — если папки нет.
/// Состояние агента OpenCode на диск не пишет — оно неизвестно, без значков на кнопке.
fn describe(session: &mut Session, tab: &Tab) {
    let pane = json!({"agent": AGENT, "agent_status": "unknown", "cwd": tab.directory});
    session.extend(describe_pane(&pane, ""));
    if let Some(title) = &tab.title {
        session.insert("name".into(), json!(title)); // вкладку могли переименовать
    }
    let empty = session.get("title").is_none_or(|t| t.as_str().is_none_or(str::is_empty));
    if empty {
        let name = session.get("name").cloned().unwrap_or(Value::Null);
        session.insert("title".into(), name);
    }
}

fn id_of(session: &Session) -> Option<&str> {
    session.get("id").and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub struct OpencodeSessions {
    ui: Box<dyn OpencodeUi>,
    store: OpencodeStore,
}

impl OpencodeSessions {
    pub fn new(ui: Box<dyn OpencodeUi>, store: OpencodeStore) -> Self {
        Self { ui, store }
    }

    fn window(&self) -> Result<i64> {
        self.ui.window().ok_or_else(|| SessionError::Other("OpenCode не запущен".into()))
    }
}

impl Backend for OpencodeSessions {
    fn matches(&self, window: i64) -> bool {
        self.ui.is_opencode(window)
    }

    /// Активная вкладка окна, которое менялось последним.
    fn current(&self) -> Result<Session> {
        self.window()?;
        let windows = self.store.windows();
        let Some(latest) = windows.first() else {
            return Err(SessionError::Other("не найдены данные OpenCode о вкладках".into()));
        };
        let tab = latest
            .active
            .as_deref()
            .and_then(|id| latest.tabs.iter().find(|t| t.id == id))
            .ok_or_else(|| SessionError::Other("в OpenCode не открыта вкладка сессии".into()))?;
        let mut session = json!({"kind": KIND, "server": AGENT, "app": APP, "id": tab.id, "name": tab.id})
            .as_object()
            .unwrap()
            .clone();
        describe(&mut session, tab);
        Ok(session)
    }

    fn focused(&self, _window: i64, _bound: &[Session]) -> Option<Session> {
        self.current().ok()
    }

    /// Доступна, пока OpenCode запущен и вкладка сессии открыта. Окно для этого не нужно —
    /// хватает данных на диске.
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        let running = self.ui.window().is_some();
        let tabs: Vec<Tab> = if running { self.store.windows().into_iter().flat_map(|w| w.tabs).collect() } else { vec![] };
        let results = sessions
            .iter()
            .map(|session| {
                let tab = id_of(session).and_then(|id| tabs.iter().find(|t| t.id == id));
                let Some(tab) = tab else { return Probe { available: Some(false), info: None } };
                let mut info = Session::new();
                info.insert("name".into(), session.get("name").cloned().unwrap_or(Value::Null));
                describe(&mut info, tab);
                Probe { available: Some(true), info: Some(info) }
            })
            .collect();
        Some(results)
    }

    fn open(&self, session: &mut Session) -> Result<()> {
        let window = self.window()?;
        let id = id_of(session).ok_or_else(|| SessionError::Lost("у кнопки нет id сессии OpenCode".into()))?.to_string();
        let Some(tab) = self.store.tab(&id) else {
            return Err(SessionError::Lost("вкладка сессии закрыта в OpenCode".into()));
        };
        self.ui.show(window);
        self.ui.open_tab(window, &id)?;
        describe(session, &tab);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
