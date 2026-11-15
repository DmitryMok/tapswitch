//! Панель кнопок в браузере: вид кнопок и подключённый телефон.
//!
//! Протокол тот же, что у Python-версии (`web_panel.py`): страница присылает
//! down/up/drag/drop/answer/forget, обратно уходят layout, key, confirm, notice, replaced.
//! Подключено одно устройство: новое вытесняет прежнее. Нажатия уходят движку событиями.

use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use crate::app::Event;
use crate::engine::{Device, KeyLook};
use crate::log;
use crate::places::Icons;

#[derive(Clone)]
struct KeyView {
    label: String,
    icon: String,
    title: String,
    status: Option<String>,
    available: bool,
    bound: bool,
    active: bool,
    look: KeyLook,
}

/// Панель открыта на этом же компьютере (страница управления или вкладка браузера).
pub fn is_local_peer(peer: &str) -> bool {
    peer == "127.0.0.1" || peer == "::1"
}

struct Client {
    id: u64,
    /// Адрес устройства: телефон в сети или эта же машина (панель на странице управления).
    peer: String,
    /// Метка вкладки браузера: та же вкладка после обрыва переподключается без «replaced».
    page: String,
    tx: Sender<String>,
    /// Кнопки, которые держит этот телефон: при отключении их надо отпустить.
    pressed: BTreeSet<usize>,
}

pub struct Panel {
    rows: usize,
    cols: usize,
    hold_ms: u64,
    icons: Arc<Icons>,
    /// Надпись над иконкой (роль) — настройка «Добавить роль на иконку».
    show_titles: AtomicBool,
    keys: Mutex<Vec<KeyView>>,
    confirm: Mutex<Option<Value>>,
    /// Текущий профиль кнопок — новому подключению сразу после раскладки.
    profile: Mutex<Option<Value>>,
    client: Mutex<Option<Client>>,
    next_id: AtomicU64,
    events: Mutex<Sender<Event>>,
    /// Иконки программ с кнопок: id в адресе → путь к exe и готовый PNG.
    programs: Mutex<HashMap<String, (PathBuf, Option<Vec<u8>>)>>,
}

/// Размер иконки программы для страницы: на телефоне кнопка крупная.
const PROGRAM_ICON: u32 = 64;

/// Программа, чья иконка нужна, или None, если это не полный путь к существующему .exe.
fn program_file(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(name.trim());
    let exe = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    (exe && path.is_absolute() && path.is_file()).then_some(path)
}

/// Похоже на путь к программе (`C:\…\name.exe`), даже если такого файла нет.
fn looks_like_program(name: &str) -> bool {
    let path = std::path::Path::new(name.trim());
    path.is_absolute() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

/// Короткий id пути для адреса картинки (FNV-1a, 64 бита).
fn program_id(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().to_lowercase();
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("{hash:016x}")
}

impl Panel {
    pub fn new(rows: usize, cols: usize, hold_ms: u64, icons: Arc<Icons>, show_titles: bool, events: Sender<Event>) -> Self {
        let view = KeyView {
            label: String::new(),
            icon: String::new(),
            title: String::new(),
            status: None,
            available: true,
            bound: false,
            active: false,
            look: KeyLook::default(),
        };
        Self {
            rows,
            cols,
            hold_ms,
            icons,
            show_titles: AtomicBool::new(show_titles),
            keys: Mutex::new(vec![view; rows * cols]),
            confirm: Mutex::new(None),
            profile: Mutex::new(None),
            client: Mutex::new(None),
            next_id: AtomicU64::new(1),
            events: Mutex::new(events),
            programs: Mutex::default(),
        }
    }

    fn key_message(&self, index: usize, view: &KeyView) -> String {
        let title = if self.show_titles.load(Ordering::Relaxed) { view.title.as_str() } else { "" };
        json!({
            "type": "key",
            "key": index,
            "label": view.label,
            "icon": view.icon,
            "title": title,
            "active": view.active,
            "status": view.status,
            "available": view.available,
            "bound": view.bound,
            // Свой вид кнопки (Rust-версия): текстовая иконка, цвет плашки, строка под подписью.
            "icon_text": view.look.icon_text,
            "color": view.look.color,
            "hint": view.look.hint,
        })
        .to_string()
    }

    fn send(&self, message: String) {
        if let Some(client) = self.client.lock().unwrap().as_ref() {
            let _ = client.tx.send(message);
        }
    }

    fn update(&self, key: usize, change: impl FnOnce(&mut KeyView)) {
        let message = {
            let mut keys = self.keys.lock().unwrap();
            let Some(view) = keys.get_mut(key) else { return };
            change(view);
            self.key_message(key, view)
        };
        self.send(message);
    }

    fn emit(&self, event: Event) {
        let _ = self.events.lock().unwrap().send(event);
    }

    /// Новое подключение: прежнее получает «replaced» и закрывается, новому уходит всё состояние.
    /// Если прежнее — та же вкладка (`page`), это переподключение после обрыва: прежнее
    /// закрывается молча, иначе запоздавшая старая попытка вытеснила бы новую.
    pub fn connect(&self, peer: &str, page: &str) -> (u64, Receiver<String>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(self.layout());
        if let Some(profile) = self.profile.lock().unwrap().as_ref() {
            let _ = tx.send(profile.to_string());
        }
        for (index, view) in self.keys.lock().unwrap().iter().enumerate() {
            let _ = tx.send(self.key_message(index, view));
        }
        if let Some(confirm) = self.confirm.lock().unwrap().as_ref() {
            let _ = tx.send(confirm.to_string());
        }
        let client = Client { id, peer: peer.to_string(), page: page.to_string(), tx, pressed: BTreeSet::new() };
        let previous = self.client.lock().unwrap().replace(client);
        if let Some(previous) = previous {
            let same_page = !page.is_empty() && previous.page == page;
            if !same_page {
                let _ = previous.tx.send(json!({"type": "replaced"}).to_string());
                log::write("Панель открыта на другом устройстве, прежнее отключено");
            }
            self.release_all(previous.pressed);
        }
        crate::windows::set_panel_on_pc(is_local_peer(peer));
        crate::tray::phone_changed(true);
        log::write(&format!("Телефон подключён: {peer}"));
        (id, rx)
    }

    fn layout(&self) -> String {
        json!({
            "type": "layout", "rows": self.rows, "cols": self.cols, "hold_ms": self.hold_ms,
            "lang": crate::i18n::current(), "version": crate::server::panel_version(),
        })
        .to_string()
    }

    /// Раскладка: строки и столбцы (горизонтальная, как на ПК).
    pub fn size(&self) -> (usize, usize) {
        (self.rows, self.cols)
    }

    pub fn key_count(&self) -> usize {
        self.rows * self.cols
    }

    /// Адрес подключённого устройства или None.
    pub fn connected_peer(&self) -> Option<String> {
        self.client.lock().unwrap().as_ref().map(|c| c.peer.clone())
    }

    /// Отпустить кнопки, которые держал телефон, иначе удержание зависнет.
    fn release_all(&self, pressed: BTreeSet<usize>) {
        for key in pressed {
            self.emit(Event::Raw(key, false));
        }
    }

    pub fn disconnect(&self, id: u64) {
        let mut client = self.client.lock().unwrap();
        if client.as_ref().is_some_and(|c| c.id == id) {
            let pressed = client.take().unwrap().pressed;
            drop(client);
            self.release_all(pressed);
            crate::windows::set_panel_on_pc(false);
            crate::tray::phone_changed(false);
            log::write("Телефон отключился");
        }
    }

    /// Сообщение от страницы — событием движку.
    pub fn on_message(&self, id: u64, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else { return };
        let kind = message["type"].as_str().unwrap_or("");
        let count = self.rows * self.cols;
        let mut client = self.client.lock().unwrap();
        let Some(client) = client.as_mut().filter(|c| c.id == id) else { return };
        // Проверка связи: страница шлёт ping раз в пару секунд и без ответа переподключается.
        if kind == "ping" {
            let _ = client.tx.send(json!({"type": "pong"}).to_string());
            return;
        }
        if kind == "answer" {
            if self.confirm.lock().unwrap().is_some() {
                self.emit(Event::Answer(message["ok"] == true));
            }
            return;
        }
        if kind == "profile" {
            if let Some(profile) = message["profile"].as_u64().filter(|&p| (p as usize) < crate::state::PROFILES) {
                self.emit(Event::Profile(profile as usize));
            }
            return;
        }
        let Some(key) = message["key"].as_u64().map(|k| k as usize).filter(|&k| k < count) else { return };
        let pressed = client.pressed.contains(&key);
        match kind {
            "down" if !pressed => {
                client.pressed.insert(key);
                self.emit(Event::Raw(key, true));
            }
            "up" if pressed => {
                client.pressed.remove(&key);
                self.emit(Event::Raw(key, false));
            }
            "drag" if pressed => self.emit(Event::Drag(key)),
            "forget" => self.emit(Event::Forget(key)),
            "drop" if pressed => {
                client.pressed.remove(&key);
                let target = message["target"].as_u64().map(|t| t as usize).filter(|&t| t < count);
                self.emit(Event::Drop(key, target));
            }
            _ => {}
        }
    }

    /// Адрес картинки для страницы: SVG роли или PNG иконки программы (у окна приложения).
    fn icon_address(&self, name: &str) -> String {
        if let Some(icon) = name.strip_prefix(crate::look::LIB) {
            return if crate::library::exists(icon) { format!("/lib/{icon}.svg") } else { String::new() };
        }
        if let Some(hash) = name.strip_prefix(crate::look::IMG) {
            return if crate::look::is_image_hash(hash) { format!("/usericon/{hash}.png") } else { String::new() };
        }
        if let Some(name) = self.icons.find(name) {
            return format!("/icons/{name}.svg");
        }
        let Some(path) = program_file(name) else {
            // Путь к программе, но файла нет (удалена, нет доступа) — общая иконка окна.
            return if looks_like_program(name) { self.window_icon() } else { String::new() };
        };
        let id = program_id(&path);
        // Отдаются только иконки программ с кнопок.
        self.programs.lock().unwrap().entry(id.clone()).or_insert((path, None));
        format!("/appicon/{id}.png")
    }

    fn window_icon(&self) -> String {
        self.icons.find(crate::windows::WINDOW_ICON).map(|name| format!("/icons/{name}.svg")).unwrap_or_default()
    }

    /// Иконка программы по id из адреса: тип и содержимое; None — такой программы на кнопках
    /// не было. Если Windows не отдал иконку программы — общая иконка окна (SVG).
    pub fn program_icon(&self, id: &str) -> Option<(&'static str, Vec<u8>)> {
        let path = {
            let programs = self.programs.lock().unwrap();
            let (path, png) = programs.get(id)?;
            if let Some(png) = png {
                return Some(("image/png", png.clone()));
            }
            path.clone()
        };
        let Some(rgba) = crate::win::program_icon(&path.to_string_lossy(), PROGRAM_ICON) else {
            let svg = self.icons.read(crate::windows::WINDOW_ICON)?;
            return Some(("image/svg+xml", svg.into_owned()));
        };
        let png = crate::png::encode(PROGRAM_ICON, PROGRAM_ICON, &rgba);
        self.programs.lock().unwrap().insert(id.to_string(), (path, Some(png.clone())));
        Some(("image/png", png))
    }
}

impl Device for Panel {
    fn key_count(&self) -> usize {
        self.rows * self.cols
    }

    fn set_label(&self, key: usize, label: &str) {
        self.update(key, |view| view.label = label.to_string());
    }

    fn set_icon(&self, key: usize, icon: &str, title: &str) {
        let url = self.icon_address(icon);
        self.update(key, |view| {
            view.icon = url;
            view.title = title.to_string();
        });
    }

    fn set_look(&self, key: usize, look: &KeyLook) {
        // Вид меняется редко, а показ кнопки — при каждом обновлении: лишнее не отправлять.
        if self.keys.lock().unwrap().get(key).is_some_and(|view| view.look == *look) {
            return;
        }
        self.update(key, |view| view.look = look.clone());
    }

    fn set_active(&self, key: usize, active: bool) {
        self.update(key, |view| view.active = active);
    }

    fn set_state(&self, key: usize, status: Option<&str>, available: bool, bound: bool) {
        self.update(key, |view| {
            view.status = status.map(str::to_string);
            view.available = available;
            view.bound = bound;
        });
    }

    fn ask_confirm(&self, key: usize, text: &str, state: crate::engine::Confirm) {
        let message = json!({"type": "confirm", "key": key, "text": text, "state": state.name()});
        *self.confirm.lock().unwrap() = Some(message.clone());
        self.send(message.to_string());
    }

    fn close_confirm(&self) {
        *self.confirm.lock().unwrap() = None;
        self.send(json!({"type": "confirm", "key": null, "text": ""}).to_string());
    }

    fn notify(&self, text: &str) {
        self.send(json!({"type": "notice", "text": text}).to_string());
    }

    fn set_profile(&self, profile: usize, count: usize) {
        let message = json!({"type": "profile", "profile": profile, "count": count});
        *self.profile.lock().unwrap() = Some(message.clone());
        self.send(message.to_string());
    }

    fn icon_url(&self, name: &str) -> String {
        self.icon_address(name)
    }

    fn relayout(&self) {
        self.send(self.layout());
        self.set_titles_visible(self.show_titles.load(Ordering::Relaxed));
    }

    fn set_titles_visible(&self, visible: bool) {
        self.show_titles.store(visible, Ordering::Relaxed);
        let messages: Vec<String> =
            self.keys.lock().unwrap().iter().enumerate().map(|(index, view)| self.key_message(index, view)).collect();
        for message in messages {
            self.send(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> Panel {
        let icons = Arc::new(Icons::new(PathBuf::from("нет такой папки")));
        Panel::new(2, 4, 500, icons, false, mpsc::channel().0)
    }

    #[test]
    fn missing_program_falls_back_to_window_icon() {
        let panel = panel();
        assert_eq!(panel.icon_address(r"C:\Нет такой папки\app.exe"), "/icons/window.svg");
        assert_eq!(panel.icon_address("app.exe"), ""); // не путь к программе
        assert_eq!(panel.icon_address(""), "");
    }

    fn types(rx: &Receiver<String>) -> Vec<String> {
        rx.try_iter().map(|t| serde_json::from_str::<Value>(&t).unwrap()["type"].as_str().unwrap_or("").to_string()).collect()
    }

    #[test]
    fn answers_ping_with_pong() {
        let panel = panel();
        let (id, rx) = panel.connect("192.168.137.2", "tab1");
        types(&rx);
        panel.on_message(id, r#"{"type":"ping"}"#);
        assert_eq!(types(&rx), ["pong"]);
    }

    #[test]
    fn same_tab_reconnects_without_replaced() {
        let panel = panel();
        let (_, first) = panel.connect("192.168.137.2", "tab1");
        let (_, second) = panel.connect("192.168.137.2", "tab1");
        assert!(!types(&first).contains(&"replaced".to_string()));
        assert!(first.try_recv().is_err_and(|e| e == mpsc::TryRecvError::Disconnected));
        types(&second);
        let (_, _third) = panel.connect("127.0.0.1", "tab2");
        assert_eq!(types(&second), ["replaced"]);
        // Страница без метки (старая) — вытесняет как раньше.
        let (_, fourth) = panel.connect("192.168.137.2", "");
        types(&fourth);
        let (_, _fifth) = panel.connect("192.168.137.2", "");
        assert_eq!(types(&fourth), ["replaced"]);
    }
}
