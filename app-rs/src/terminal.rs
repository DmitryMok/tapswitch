//! Окна и вкладки Windows Terminal (перенос `terminal.py`): вкладки читаются и выбираются
//! через UI Automation скриптом `terminal_tabs.ps1` в постоянном PowerShell.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::sessions::{Chrome, Result, SessionError, TabRef, Terminal, TerminalTab};
use crate::{win, workers};

const TERMINAL_CLASS: &str = "CASCADIA_HOSTING_WINDOW_CLASS";
/// Чтение выбранной вкладки — запуск скрипта, поэтому для рамки активной сессии ответ
/// хранится, пока не сменился заголовок окна, но не дольше этого срока.
const SELECTED_TAB_TTL: Duration = Duration::from_secs(3);
/// Список вкладок для серой иконки закрытой сессии: закрытие невыбранной вкладки заголовок
/// окна не меняет, поэтому список перечитывается не реже, чем раз в этот срок.
const TAB_LIST_TTL: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(5);

pub fn is_terminal_window(window: i64) -> bool {
    win::class_name(window) == TERMINAL_CLASS
}

/// Окна терминала от последнего активного к давним.
fn terminal_windows(include_minimized: bool) -> Vec<i64> {
    win::top_level_windows(include_minimized).into_iter().filter(|&w| is_terminal_window(w)).collect()
}

fn failure(text: impl std::fmt::Display) -> SessionError {
    SessionError::Other(format!("Не удалось обратиться к терминалу: {text}"))
}

pub struct WindowsTerminal {
    script: PathBuf,
    /// Окно → заголовок, время, выбранная вкладка.
    selected: Mutex<HashMap<i64, (String, Instant, Option<String>)>>,
    /// Окно → заголовок, время, вкладки.
    tabs: Mutex<HashMap<i64, (String, Instant, Vec<String>)>>,
}

impl WindowsTerminal {
    pub fn new() -> Self {
        Self {
            script: workers::script("terminal_tabs.ps1", workers::TERMINAL_TABS),
            selected: Mutex::default(),
            tabs: Mutex::default(),
        }
    }

    fn run(&self, action: &str, window: i64, tab: &str) -> Result<String> {
        let window = window.to_string();
        let env = [("AGENT_CONTROL_ACTION", action), ("AGENT_CONTROL_WINDOW", window.as_str()), ("AGENT_CONTROL_TAB", tab)];
        let (code, out) = workers::powershell_script(&self.script, &env, TIMEOUT).map_err(failure)?;
        match code {
            0 => Ok(out),
            3 => Err(SessionError::TabNotFound("Вкладка терминала не найдена".into())),
            _ => Err(failure(out.trim().chars().take(300).collect::<String>())),
        }
    }

    fn run_json(&self, action: &str, window: i64, tab: &str) -> Result<Value> {
        let out = self.run(action, window, tab)?;
        serde_json::from_str(out.trim()).map_err(|e| failure(format!("непонятный ответ: {e}")))
    }

    /// Вкладки окна: (id, выбрана ли, заголовок). Пустой список — окно на весь экран.
    fn list(&self, window: i64) -> Result<Vec<(String, bool, String)>> {
        let data = self.run_json("list", window, "")?;
        // ConvertTo-Json из одного элемента может выдать объект вместо списка.
        let entries = match data {
            Value::Array(items) => items,
            Value::Null => vec![],
            single => vec![single],
        };
        Ok(entries
            .iter()
            .map(|e| {
                let text = |name: &str| e[name].as_str().unwrap_or("").to_string();
                (text("tab"), e["selected"].as_bool().unwrap_or(false), text("title"))
            })
            .collect())
    }

    /// Клавиатурный фокус — в сам терминал: после выбора вкладки он остаётся на её
    /// заголовке, и набранный текст никуда не попадает. Не вышло — окно всё равно показано.
    fn focus_input(&self, window: i64) {
        let _ = self.run("focus", window, "");
    }
}

impl Terminal for WindowsTerminal {
    fn active_tab(&self) -> Result<TerminalTab> {
        let Some(&window) = terminal_windows(false).first() else {
            return Err(SessionError::Other("Окно Windows Terminal не найдено".into()));
        };
        self.read_selected(window)
    }

    fn read_selected(&self, window: i64) -> Result<TerminalTab> {
        let data = self.run_json("read", window, "")?;
        Ok(TerminalTab {
            window,
            tab: data["tab"].as_str().map(str::to_string),
            text: data["text"].as_str().unwrap_or("").to_string(),
            chrome: serde_json::from_value::<Chrome>(data["chrome"].clone()).ok(),
        })
    }

    fn list_tabs(&self) -> Result<Vec<TabRef>> {
        let mut tabs = Vec::new();
        for window in terminal_windows(true) {
            let entries = match self.list(window) {
                Ok(entries) => entries,
                Err(SessionError::TabNotFound(_)) => continue, // окно закрылось во время поиска
                Err(e) => return Err(e),
            };
            if entries.is_empty() {
                // Полноэкранное окно: доступна только видимая вкладка.
                tabs.push(TabRef { window, tab: None, selected: true, title: String::new() });
            }
            tabs.extend(entries.into_iter().map(|(tab, selected, title)| TabRef { window, tab: Some(tab), selected, title }));
        }
        Ok(tabs)
    }

    fn select_tab(&self, window: i64, tab: Option<&str>) -> Result<()> {
        self.selected.lock().unwrap().remove(&window);
        match tab {
            Some(tab) => self.run("select", window, tab).map(drop),
            None => Ok(()), // полноэкранное окно: выбирать нечего
        }
    }

    fn show_tab(&self, window: i64, tab: Option<&str>) -> Result<()> {
        if tab.is_some() {
            self.select_tab(window, tab)?;
        }
        if !win::bring_to_foreground(window) {
            return Err(SessionError::Other("Windows не дал вывести на передний план окно терминала".into()));
        }
        self.focus_input(window);
        Ok(())
    }

    fn selected_tab(&self, window: i64) -> Result<Option<String>> {
        let (title, now) = (win::window_title(window), Instant::now());
        if let Some((cached_title, at, tab)) = self.selected.lock().unwrap().get(&window) {
            if *cached_title == title && now - *at < SELECTED_TAB_TTL {
                return Ok(tab.clone());
            }
        }
        let tab = self.list(window)?.into_iter().find(|(_, selected, _)| *selected).map(|(tab, _, _)| tab);
        self.selected.lock().unwrap().insert(window, (title, now, tab.clone()));
        Ok(tab)
    }

    fn window_tabs(&self, window: i64) -> Result<Option<Vec<String>>> {
        if !is_terminal_window(window) {
            self.tabs.lock().unwrap().remove(&window);
            return Ok(None);
        }
        let (title, now) = (win::window_title(window), Instant::now());
        if let Some((cached_title, at, tabs)) = self.tabs.lock().unwrap().get(&window) {
            if *cached_title == title && now - *at < TAB_LIST_TTL {
                return Ok(Some(tabs.clone()));
            }
        }
        let tabs: Vec<String> = match self.list(window) {
            Ok(entries) => entries.into_iter().map(|(tab, _, _)| tab).collect(),
            Err(SessionError::TabNotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        self.tabs.lock().unwrap().insert(window, (title, now, tabs.clone()));
        Ok(Some(tabs))
    }

    fn is_terminal(&self, window: i64) -> bool {
        is_terminal_window(window)
    }
}
