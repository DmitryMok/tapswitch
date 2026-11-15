//! Вкладки браузеров (Chrome, Opera, Comet и других на Chromium) как сессии кнопок
//! (перенос `browser.py`).
//!
//! Список вкладок присылает расширение `app/browser_extension` по WebSocket. Окно браузера
//! в Windows и окно в расширении связываются по заголовку: Windows показывает заголовок
//! активной вкладки («Почта - Google Chrome»). Нажатие кнопки просит расширение сделать
//! вкладку активной, а окно на передний план выводит сама программа — расширению Windows
//! этого обычно не даёт.
//!
//! Без расширения окна браузера по-прежнему привязываются как окна приложений.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};

use crate::sessions::{Backend, Probe, Result, Session, SessionError};
use crate::{log, win, windows};

pub const KIND: &str = "tab";
/// Постоянный: задан ключом в manifest.json расширения.
pub const EXTENSION_ID: &str = "adfbcjbmjkgmibjeolcjgclchfjngdkk";
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: i64,
    pub focused: bool,
    pub tabs: Vec<Tab>,
}

impl Window {
    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.active)
    }
}

/// Один браузер с одним профилем: своё подключение расширения.
struct Instance {
    id: String,
    browser: String,
    windows: Vec<Window>,
}

/// (браузер, окно, вкладка).
pub type Found = (String, Window, Tab);

/// Заголовок окна — заголовок вкладки, после него название браузера (и профиля).
pub fn title_matches(window_title: &str, tab_title: &str) -> bool {
    !tab_title.is_empty()
        && (window_title == tab_title
            || window_title.strip_prefix(tab_title).is_some_and(|rest| rest.starts_with(' ')))
}

/// Вкладки всех подключённых браузеров (в порядке подключения).
#[derive(Default)]
pub struct TabRegistry {
    instances: Mutex<Vec<Instance>>,
}

impl TabRegistry {
    pub fn connect(&self, instance: &str, browser: &str) {
        let mut instances = self.instances.lock().unwrap();
        instances.retain(|i| i.id != instance);
        instances.push(Instance { id: instance.into(), browser: browser.into(), windows: vec![] });
    }

    pub fn set_windows(&self, instance: &str, windows: Vec<Window>) {
        if let Some(found) = self.instances.lock().unwrap().iter_mut().find(|i| i.id == instance) {
            found.windows = windows;
        }
    }

    pub fn disconnect(&self, instance: &str) {
        self.instances.lock().unwrap().retain(|i| i.id != instance);
    }

    pub fn connected(&self) -> bool {
        !self.instances.lock().unwrap().is_empty()
    }

    /// Имена подключённых браузеров без повторов, по алфавиту.
    pub fn browsers(&self) -> Vec<String> {
        let mut names: Vec<String> = self.instances.lock().unwrap().iter().map(|i| i.browser.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn browser(&self, instance: &str) -> Option<String> {
        self.instances.lock().unwrap().iter().find(|i| i.id == instance).map(|i| i.browser.clone())
    }

    /// Браузер, окно и активная вкладка окна Windows с таким заголовком; при нескольких
    /// подходящих — с самым длинным заголовком вкладки («Docs - План» точнее, чем «Docs»).
    pub fn by_window_title(&self, window_title: &str) -> Option<Found> {
        let mut best: Option<Found> = None;
        for instance in self.instances.lock().unwrap().iter() {
            for window in &instance.windows {
                let Some(tab) = window.active_tab() else { continue };
                if title_matches(window_title, &tab.title)
                    && best.as_ref().is_none_or(|b| tab.title.chars().count() > b.2.title.chars().count())
                {
                    best = Some((instance.id.clone(), window.clone(), tab.clone()));
                }
            }
        }
        best
    }

    /// Вкладка запомненной сессии: та же вкладка браузера; после перезапуска браузера
    /// у вкладок новые номера — тогда единственная вкладка с тем же адресом, затем заголовком.
    pub fn find(&self, session: &Session) -> Option<Found> {
        let tabs: Vec<Found> = self
            .instances
            .lock()
            .unwrap()
            .iter()
            .flat_map(|i| {
                i.windows.iter().flat_map(move |w| w.tabs.iter().map(move |t| (i.id.clone(), w.clone(), t.clone())))
            })
            .collect();
        let instance = text(session, "instance");
        let same_browser: Vec<&Found> = tabs.iter().filter(|f| f.0 == instance).collect();
        let tab_id = session.get("tab").and_then(Value::as_i64);
        if let Some(found) = same_browser.iter().find(|f| Some(f.2.id) == tab_id) {
            return Some((*found).clone());
        }
        let all: Vec<&Found> = tabs.iter().collect();
        for pool in [&same_browser, &all] {
            for (key, value) in [("url", text(session, "url")), ("title", text(session, "name"))] {
                if value.is_empty() {
                    continue;
                }
                let matches: Vec<&&Found> =
                    pool.iter().filter(|f| if key == "url" { f.2.url == value } else { f.2.title == value }).collect();
                if matches.len() == 1 {
                    return Some((*matches[0]).clone());
                }
            }
        }
        None
    }
}

fn text<'a>(session: &'a Session, name: &str) -> &'a str {
    session.get(name).and_then(Value::as_str).unwrap_or("")
}

/// Окна рабочего стола, нужные вкладкам.
pub trait Desktop: Send + Sync {
    /// Окна от последнего активного к давним.
    fn windows(&self, include_minimized: bool) -> Vec<i64>;
    /// Обычное окно приложения (не своё, не панель задач, не диалог).
    fn is_app(&self, window: i64) -> bool;
    fn title(&self, window: i64) -> String;
    fn exe(&self, window: i64) -> String;
    /// false — Windows не дал вывести окно вперёд.
    fn show(&self, window: i64) -> bool;
}

pub trait Bridge: Send + Sync {
    /// Попросить расширение сделать вкладку активной; false — расширение не подключено.
    fn activate(&self, instance: &str, tab: i64, window: i64, focus: bool) -> bool;
}

pub fn session_id(instance: &str, tab: i64) -> String {
    let short: String = instance.chars().take(8).collect();
    format!("{short}:{tab}")
}

pub struct BrowserTabs {
    registry: Arc<TabRegistry>,
    bridge: Box<dyn Bridge>,
    desktop: Box<dyn Desktop>,
}

impl BrowserTabs {
    pub fn new(registry: Arc<TabRegistry>, bridge: Box<dyn Bridge>, desktop: Box<dyn Desktop>) -> Self {
        Self { registry, bridge, desktop }
    }

    fn describe(&self, instance: &str, window: &Window, tab: &Tab, exe: &str) -> Session {
        let browser = self.registry.browser(instance).unwrap_or_else(|| {
            let name = exe.rsplit(['\\', '/']).next().unwrap_or("");
            let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            if stem.is_empty() { "браузер".into() } else { stem.to_string() }
        });
        json!({
            "kind": KIND,
            "server": browser,
            "id": session_id(instance, tab.id),
            "instance": instance,
            "tab": tab.id,
            "window": window.id,
            "url": tab.url,
            "name": tab.title,
            "title": if tab.title.is_empty() { &tab.url } else { &tab.title },
            "app": browser,
            "icon": exe, // иконка браузера на кнопке
        })
        .as_object()
        .unwrap()
        .clone()
    }

    /// Окно Windows, которое сейчас показывает это окно браузера: по заголовку его
    /// активной вкладки (до переключения), свёрнутые окна тоже подходят.
    fn browser_window(&self, window: &Window, exe: &str) -> Option<i64> {
        let active = window.active_tab()?;
        let candidates: Vec<i64> = self
            .desktop
            .windows(true)
            .into_iter()
            .filter(|&w| self.desktop.is_app(w) && title_matches(&self.desktop.title(w), &active.title))
            .collect();
        let same_program =
            candidates.iter().copied().find(|&w| !exe.is_empty() && self.desktop.exe(w).eq_ignore_ascii_case(exe));
        same_program.or(candidates.first().copied())
    }
}

impl Backend for BrowserTabs {
    /// Окно браузера, о вкладках которого рассказало расширение.
    fn matches(&self, window: i64) -> bool {
        self.registry.connected()
            && self.desktop.is_app(window)
            && self.registry.by_window_title(&self.desktop.title(window)).is_some()
    }

    fn current(&self) -> Result<Session> {
        for window in self.desktop.windows(false) {
            if !self.desktop.is_app(window) {
                continue;
            }
            if let Some((instance, browser_window, tab)) = self.registry.by_window_title(&self.desktop.title(window)) {
                return Ok(self.describe(&instance, &browser_window, &tab, &self.desktop.exe(window)));
            }
        }
        Err(SessionError::Other("не найдено окно браузера с расширением Tapswitch".into()))
    }

    fn open(&self, session: &mut Session) -> Result<()> {
        let Some((instance, window, tab)) = self.registry.find(session) else {
            if !self.registry.connected() {
                return Err(SessionError::Lost("браузер закрыт или в нём не установлено расширение Tapswitch".into()));
            }
            return Err(SessionError::Lost(format!("вкладка «{}» закрыта", text(session, "name"))));
        };
        let exe = text(session, "icon").to_string();
        session.extend(self.describe(&instance, &window, &tab, &exe));
        let target = self.browser_window(&window, &exe);
        // Окно не нашлось (например, у вкладки пустой заголовок) — пусть расширение само
        // попросит Windows показать окно браузера.
        if !self.bridge.activate(&instance, tab.id, window.id, target.is_none()) {
            return Err(SessionError::Other("расширение браузера отключилось".into()));
        }
        if let Some(target) = target {
            if !self.desktop.show(target) {
                return Err(SessionError::Other("Windows не дал вывести на передний план окно браузера".into()));
            }
        }
        Ok(())
    }

    /// Вкладка доступна, пока она открыта в подключённом браузере; заголовок обновляется.
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        Some(
            sessions
                .iter()
                .map(|session| match self.registry.find(session) {
                    None => Probe { available: Some(false), info: None },
                    Some((instance, window, tab)) => Probe {
                        available: Some(true),
                        info: Some(self.describe(&instance, &window, &tab, text(session, "icon"))),
                    },
                })
                .collect(),
        )
    }

    fn focused(&self, window: i64, _bound: &[Session]) -> Option<Session> {
        let (instance, _, tab) = self.registry.by_window_title(&self.desktop.title(window))?;
        Some(json!({"kind": KIND, "id": session_id(&instance, tab.id)}).as_object().unwrap().clone())
    }
}

/// Настоящий рабочий стол Windows. Окна страниц самой программы — не сессии.
pub struct WindowsBrowserDesktop;

impl Desktop for WindowsBrowserDesktop {
    fn windows(&self, include_minimized: bool) -> Vec<i64> {
        win::top_level_windows(include_minimized)
    }
    fn is_app(&self, window: i64) -> bool {
        win::is_app_window(window) && !windows::is_own_page(&win::window_title(window))
    }
    fn title(&self, window: i64) -> String {
        win::window_title(window)
    }
    fn exe(&self, window: i64) -> String {
        win::process_path(window)
    }
    fn show(&self, window: i64) -> bool {
        win::bring_to_foreground(window)
    }
}

// --- сервер для расширения

/// Подключаться может только наше расширение: веб-страница подделать Origin не может.
pub fn allowed_origin(origin: &str, extension_ids: &[String]) -> bool {
    origin.strip_prefix("chrome-extension://").is_some_and(|id| extension_ids.iter().any(|e| e == id))
}

fn clean_id(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn clean_window(window: &Value) -> Option<Window> {
    let tabs = window.get("tabs")?.as_array()?;
    Some(Window {
        id: clean_id(window.get("id")?)?,
        focused: window["focused"].as_bool().unwrap_or(false),
        tabs: tabs
            .iter()
            .map(|tab| {
                Some(Tab {
                    id: clean_id(tab.get("id")?)?,
                    title: as_text(&tab["title"]),
                    url: as_text(&tab["url"]),
                    active: tab["active"].as_bool().unwrap_or(false),
                })
            })
            .collect::<Option<Vec<Tab>>>()?,
    })
}

/// Разбор сообщений расширения отдельно от сети: так он проверяется тестами.
pub struct Messages {
    registry: Arc<TabRegistry>,
    log: Box<dyn Fn(&str) + Send + Sync>,
}

impl Messages {
    pub fn new(registry: Arc<TabRegistry>, log: Box<dyn Fn(&str) + Send + Sync>) -> Self {
        Self { registry, log }
    }

    /// connection — какой браузер на том конце подключения (до hello — None).
    pub fn handle(&self, connection: &mut Option<String>, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else { return };
        match message["type"].as_str() {
            Some("hello") => {
                let instance = as_text(&message["instance"]);
                if instance.is_empty() {
                    return;
                }
                let browser = Some(as_text(&message["browser"])).filter(|b| !b.is_empty());
                let browser = browser.unwrap_or_else(|| "браузер".into());
                self.registry.connect(&instance, &browser);
                *connection = Some(instance);
                (self.log)(&format!("Расширение подключено: {browser}"));
            }
            Some("tabs") => {
                let Some(instance) = connection else { return };
                let Some(windows) = message["windows"].as_array() else { return };
                if let Some(windows) = windows.iter().map(clean_window).collect::<Option<Vec<_>>>() {
                    self.registry.set_windows(instance, windows);
                }
            }
            Some("activated") if message["ok"] != Value::Bool(true) => {
                (self.log)(&format!("Расширение не переключило вкладку: {}", as_text(&message["error"])));
            }
            _ => {}
        }
    }

    pub fn closed(&self, connection: &Option<String>) {
        if let Some(instance) = connection {
            let browser = self.registry.browser(instance).unwrap_or_default();
            self.registry.disconnect(instance);
            (self.log)(&format!("Расширение отключилось: {browser}"));
        }
    }
}

/// WebSocket-сервер на 127.0.0.1 для расширений браузеров.
pub struct BrowserBridge {
    messages: Messages,
    extension_ids: Vec<String>,
    /// Браузер → (номер подключения, очередь исходящих).
    clients: Mutex<HashMap<String, (u64, Sender<String>)>>,
    next_id: Mutex<u64>,
}

impl BrowserBridge {
    /// Запустить сервер; ошибка — порт занят.
    pub fn start(registry: Arc<TabRegistry>, port: u16, extension_ids: Vec<String>) -> std::io::Result<Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        let bridge = Arc::new(Self {
            messages: Messages::new(registry, Box::new(log::write)),
            extension_ids,
            clients: Mutex::default(),
            next_id: Mutex::new(0),
        });
        let server = bridge.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let server = server.clone();
                std::thread::spawn(move || server.serve(stream));
            }
        });
        Ok(bridge)
    }

    fn serve(&self, stream: TcpStream) {
        let check = |request: &Request, response: Response| -> std::result::Result<Response, ErrorResponse> {
            let origin = request.headers().get("origin").and_then(|v| v.to_str().ok()).unwrap_or("");
            if allowed_origin(origin, &self.extension_ids) {
                Ok(response)
            } else {
                let mut refusal = ErrorResponse::new(Some("forbidden origin".into()));
                *refusal.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
                Err(refusal)
            }
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let Ok(mut socket) = tungstenite::accept_hdr(stream, check) else { return };
        let _ = socket.get_ref().set_read_timeout(Some(POLL));
        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next
        };
        let mut connection: Option<String> = None;
        let mut outgoing: Option<Receiver<String>> = None;
        'session: loop {
            if let Some(queue) = &outgoing {
                loop {
                    match queue.try_recv() {
                        Ok(text) => {
                            if socket.send(Message::text(text)).is_err() {
                                break 'session;
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        // Тот же браузер подключился заново — это подключение лишнее.
                        Err(TryRecvError::Disconnected) => {
                            let _ = socket.close(None);
                            let _ = socket.flush();
                            break 'session;
                        }
                    }
                }
            }
            match socket.read() {
                Ok(Message::Text(text)) => {
                    let before = connection.clone();
                    self.messages.handle(&mut connection, text.as_str());
                    if let Some(instance) = connection.as_ref().filter(|i| Some(*i) != before.as_ref()) {
                        let (tx, rx) = channel();
                        outgoing = Some(rx);
                        // Прежнее подключение того же браузера закроется само: его очередь пропала.
                        self.clients.lock().unwrap().insert(instance.clone(), (id, tx));
                    }
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => break,
            }
        }
        if let Some(instance) = &connection {
            let mut clients = self.clients.lock().unwrap();
            if clients.get(instance).is_some_and(|(owner, _)| *owner == id) {
                clients.remove(instance);
                drop(clients);
                self.messages.closed(&connection);
            }
        }
    }
}

impl Bridge for BrowserBridge {
    fn activate(&self, instance: &str, tab: i64, window: i64, focus: bool) -> bool {
        let clients = self.clients.lock().unwrap();
        let Some((_, tx)) = clients.get(instance) else { return false };
        let message = json!({"type": "activate", "request": win::random_hex(16), "tab": tab, "window": window, "focus": focus});
        tx.send(message.to_string()).is_ok()
    }
}

/// Мост, которого нет: порт для расширения не открылся.
pub struct NoBridge;

impl Bridge for NoBridge {
    fn activate(&self, _: &str, _: i64, _: i64, _: bool) -> bool {
        false
    }
}

impl<T: Bridge + ?Sized> Bridge for Arc<T> {
    fn activate(&self, instance: &str, tab: i64, window: i64, focus: bool) -> bool {
        (**self).activate(instance, tab, window, focus)
    }
}

#[cfg(test)]
mod tests;
