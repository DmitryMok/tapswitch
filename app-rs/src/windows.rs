//! Окна любых приложений как «сессии» кнопок: документ Word, PDF, браузер и т.п.
//! (перенос `apps.py`).
//!
//! Окно запоминается по дескриптору. После перезапуска приложения дескриптор другой,
//! поэтому окно ищется заново: тот же процесс и класс окна, затем совпадение заголовка;
//! у браузеров заголовок меняется с вкладкой — единственное окно программы и есть то самое.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::sessions::{Backend, Probe, Result, Session, SessionError};
use crate::win;

pub const KIND: &str = "window";
/// Иконка из `app/icons` для окна, у программы которого своей иконки нет.
pub const WINDOW_ICON: &str = "window";
/// Заголовки страниц самой программы в браузере.
/// Прежние названия — пока открыты вкладки, загруженные до переименования в Tapswitch.
const OWN_PAGES: &[&str] = &["Tapswitch", "Панель агентов", "Agent panel", "Agent Control"];

/// Классы окон, чей заголовок — открытая внутри вкладка или страница: браузеры на Chromium
/// и Firefox, программы на Electron (Obsidian, VS Code). Такое окно по заголовку не узнать.
const TABBED_CLASSES: &[&str] = &["Chrome_WidgetWin_1", "MozillaWindowClass"];

/// Панелью сейчас пользуются мышью на этом компьютере (страница в браузере, а не телефон).
static PANEL_ON_PC: AtomicBool = AtomicBool::new(false);

/// Отметить, откуда пользуются панелью; вызывает панель при подключении и отключении.
pub fn set_panel_on_pc(on: bool) {
    PANEL_ON_PC.store(on, Ordering::Relaxed);
}

/// Окно страницы самой программы, которое надо пропустить при выборе текущей сессии.
/// Пропускается, только пока панель на этом компьютере: тогда окно с ней — пульт, а текущее —
/// окно до него. Когда нажимают на телефоне, та же вкладка браузера — обычная сессия,
/// иначе «текущим» оказалось бы окно под браузером (например, терминал).
pub fn is_own_page(title: &str) -> bool {
    PANEL_ON_PC.load(Ordering::Relaxed) && is_own_title(title)
}

fn is_own_title(title: &str) -> bool {
    OWN_PAGES.iter().any(|page| title.starts_with(page))
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: i64,
    pub title: String,
    pub class: String,
    /// Полный путь к программе; пустой, если Windows его не отдал.
    pub exe: String,
}

pub trait Desktop: Send + Sync {
    /// Окно приложения, активное последним (не терминал и не окна других видов).
    fn last_app_window(&self) -> Option<i64>;
    /// Сведения об окне или None, если окна больше нет.
    fn info(&self, window: i64) -> Option<WindowInfo>;
    /// Все окна приложений, включая свёрнутые.
    fn app_windows(&self) -> Vec<i64>;
    /// Развернуть, если свёрнуто, и вывести вперёд; false — Windows не дал.
    fn show(&self, window: i64) -> bool;
    fn is_app(&self, window: i64) -> bool;
}

/// (документ, приложение) из заголовка «Отчёт.docx - Word»; без разделителя — весь заголовок и пусто.
pub fn split_title(title: &str) -> (String, String) {
    let title = title.trim();
    let chars: Vec<(usize, char)> = title.char_indices().collect();
    let mut parts = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < chars.len() {
        // Разделитель: пробелы, тире, пробелы.
        if chars[i].1.is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].1.is_whitespace() {
                j += 1;
            }
            if j < chars.len() && matches!(chars[j].1, '-' | '—' | '–') {
                let mut k = j + 1;
                while k < chars.len() && chars[k].1.is_whitespace() {
                    k += 1;
                }
                if k > j + 1 && k < chars.len() {
                    parts.push(&title[start..chars[i].0]);
                    start = chars[k].0;
                    i = k;
                    continue;
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    parts.push(&title[start..]);
    if parts.len() < 2 {
        return (title.to_string(), String::new());
    }
    let app = parts.pop().unwrap().to_string();
    (parts.join(" - "), app)
}

/// Документ без отметки несохранённых изменений («*», «●»).
fn plain_document(title: &str) -> String {
    let document = split_title(title).0;
    let marks = ['*', '●', '•'];
    let mut text = document.as_str();
    if let Some(rest) = text.strip_prefix(marks) {
        text = rest.trim_start();
    }
    if let Some(rest) = text.strip_suffix(marks) {
        text = rest.trim_end();
    }
    text.to_string()
}

/// Что окно показывает, кроме открытой вкладки: «Заметка - tutor - Obsidian 1.15.1» → «tutor».
fn place_of(title: &str) -> Option<String> {
    let (document, app) = split_title(title);
    if app.is_empty() {
        return None;
    }
    document.rsplit_once(" - ").map(|(_, place)| place.to_string())
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

pub fn describe_window(info: &WindowInfo) -> Session {
    let (document, app) = split_title(&info.title);
    let process = Some(file_name(&info.exe)).filter(|p| !p.is_empty()).unwrap_or("?");
    let stem = process.rsplit_once('.').map_or(process, |(stem, _)| stem);
    json!({
        "kind": KIND,
        "server": process,
        "id": info.hwnd,
        "window_class": info.class,
        "exe": info.exe,
        "name": info.title,
        "title": if document.is_empty() { info.title.clone() } else { document },
        "app": if app.is_empty() { stem.to_string() } else { app },
        "icon": info.exe, // иконка программы на кнопке
    })
    .as_object()
    .unwrap()
    .clone()
}

pub struct WindowSessions {
    desktop: Box<dyn Desktop>,
}

impl WindowSessions {
    pub fn new(desktop: Box<dyn Desktop>) -> Self {
        Self { desktop }
    }

    fn same_program(info: &WindowInfo, session: &Session) -> bool {
        let server = session.get("server").and_then(Value::as_str).unwrap_or("");
        file_name(&info.exe).eq_ignore_ascii_case(server)
            && session.get("window_class").and_then(Value::as_str) == Some(info.class.as_str())
    }

    fn find(&self, session: &Session) -> Option<WindowInfo> {
        let id = session.get("id").and_then(Value::as_i64).unwrap_or(0);
        if let Some(info) = self.desktop.info(id).filter(|i| Self::same_program(i, session)) {
            return Some(info); // заголовок мог смениться: в браузере — другая вкладка
        }
        let candidates: Vec<WindowInfo> = self
            .desktop
            .app_windows()
            .into_iter()
            .filter_map(|w| self.desktop.info(w))
            .filter(|i| Self::same_program(i, session))
            .collect();
        let name = session.get("name").and_then(Value::as_str).unwrap_or("");
        let rules: [&dyn Fn(&WindowInfo) -> bool; 2] =
            [&|i| i.title == name, &|i| plain_document(&i.title) == plain_document(name)];
        for rule in rules {
            let matches: Vec<&WindowInfo> = candidates.iter().filter(|i| rule(i)).collect();
            if matches.len() == 1 {
                return Some(matches[0].clone());
            }
        }
        // Документ без совпадения заголовка — другой документ; а окно браузера — то же окно
        // с другой вкладкой. У Obsidian в заголовке после заметки — хранилище: окно узнаётся по нему.
        let tabbed: Vec<&WindowInfo> = candidates.iter().filter(|i| TABBED_CLASSES.contains(&i.class.as_str())).collect();
        if let Some(place) = place_of(name) {
            let matches: Vec<&&WindowInfo> = tabbed.iter().filter(|i| place_of(&i.title).as_ref() == Some(&place)).collect();
            if matches.len() == 1 {
                return Some((*matches[0]).clone());
            }
        }
        match &candidates[..] {
            [only] if TABBED_CLASSES.contains(&only.class.as_str()) => Some(only.clone()),
            _ => None,
        }
    }
}

impl Backend for WindowSessions {
    fn matches(&self, window: i64) -> bool {
        self.desktop.is_app(window)
    }

    fn current(&self) -> Result<Session> {
        let info = self.desktop.last_app_window().and_then(|w| self.desktop.info(w));
        let Some(info) = info else {
            return Err(SessionError::Other("не найдено окно приложения: переключитесь в нужное окно".into()));
        };
        Ok(describe_window(&info))
    }

    fn open(&self, session: &mut Session) -> Result<()> {
        let Some(info) = self.find(session) else {
            let name = session.get("name").and_then(Value::as_str).unwrap_or("?");
            return Err(SessionError::Lost(format!("окно «{name}» закрыто")));
        };
        session.extend(describe_window(&info));
        if !self.desktop.show(info.hwnd) {
            return Err(SessionError::Other("Windows не дал вывести на передний план окно приложения".into()));
        }
        Ok(())
    }

    /// Окно доступно, пока его можно найти; состояния агента у окна нет. Найденное заново
    /// (после перезапуска программы) запоминается сразу: дальше его не надо искать.
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        Some(
            sessions
                .iter()
                .map(|session| match self.find(session) {
                    None => Probe { available: Some(false), info: None },
                    Some(info) => {
                        let moved = session.get("id").and_then(Value::as_i64) != Some(info.hwnd);
                        let info = moved.then(|| json!({"id": info.hwnd}).as_object().unwrap().clone());
                        Probe { available: Some(true), info }
                    }
                })
                .collect(),
        )
    }

    fn focused(&self, window: i64, _bound: &[Session]) -> Option<Session> {
        Some(json!({"kind": KIND, "id": window}).as_object().unwrap().clone())
    }
}

/// Настоящий рабочий стол: порядок окон и окно в фокусе — для выбора вида сессии.
pub struct WindowsScreen;

impl crate::sessions::Screen for WindowsScreen {
    fn top_level_windows(&self) -> Vec<i64> {
        win::top_level_windows(false)
    }
    fn foreground_window(&self) -> i64 {
        win::foreground_window()
    }
}

pub type Matcher = Box<dyn Fn(i64) -> bool + Send + Sync>;

/// Настоящий рабочий стол Windows.
pub struct WindowsDesktop {
    /// Распознаватели окон других видов (терминал и т.п.) — их окна не берутся.
    others: Vec<Matcher>,
}

impl WindowsDesktop {
    pub fn new(others: Vec<Matcher>) -> Self {
        Self { others }
    }
}

impl Desktop for WindowsDesktop {
    fn is_app(&self, window: i64) -> bool {
        if !win::is_app_window(window) || self.others.iter().any(|m| m(window)) {
            return false;
        }
        !is_own_page(&win::window_title(window))
    }

    fn last_app_window(&self) -> Option<i64> {
        win::top_level_windows(false).into_iter().find(|&w| self.is_app(w))
    }

    fn info(&self, window: i64) -> Option<WindowInfo> {
        win::is_window(window).then(|| WindowInfo {
            hwnd: window,
            title: win::window_title(window),
            class: win::class_name(window),
            exe: win::process_path(window),
        })
    }

    fn app_windows(&self) -> Vec<i64> {
        win::top_level_windows(true).into_iter().filter(|&w| self.is_app(w)).collect()
    }

    fn show(&self, window: i64) -> bool {
        win::bring_to_foreground(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::same_session;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    const WORD: &str = r"C:\Program Files\Microsoft Office\root\Office16\WINWORD.EXE";
    const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";

    #[derive(Default)]
    struct State {
        windows: BTreeMap<i64, WindowInfo>,
        last: Option<i64>,
        shown: Vec<i64>,
    }

    #[derive(Clone, Default)]
    struct FakeDesktop(Arc<Mutex<State>>);

    impl FakeDesktop {
        fn new(windows: &[WindowInfo]) -> Self {
            let desktop = Self::default();
            desktop.set(windows);
            desktop.0.lock().unwrap().last = windows.first().map(|w| w.hwnd);
            desktop
        }
        fn set(&self, windows: &[WindowInfo]) {
            self.0.lock().unwrap().windows = windows.iter().map(|w| (w.hwnd, w.clone())).collect();
        }
        fn shown(&self) -> Vec<i64> {
            self.0.lock().unwrap().shown.clone()
        }
    }

    impl Desktop for FakeDesktop {
        fn last_app_window(&self) -> Option<i64> {
            self.0.lock().unwrap().last
        }
        fn info(&self, window: i64) -> Option<WindowInfo> {
            self.0.lock().unwrap().windows.get(&window).cloned()
        }
        fn app_windows(&self) -> Vec<i64> {
            self.0.lock().unwrap().windows.keys().copied().collect()
        }
        fn show(&self, window: i64) -> bool {
            self.0.lock().unwrap().shown.push(window);
            true
        }
        fn is_app(&self, _window: i64) -> bool {
            true
        }
    }

    fn info(hwnd: i64, title: &str, class: &str, exe: &str) -> WindowInfo {
        WindowInfo { hwnd, title: title.into(), class: class.into(), exe: exe.into() }
    }

    fn word(hwnd: i64, title: &str) -> WindowInfo {
        info(hwnd, title, "OpusApp", WORD)
    }

    #[test]
    fn own_page_is_skipped_only_while_panel_is_on_this_computer() {
        // С телефона вкладка «Tapswitch» — обычное окно браузера, иначе «текущим»
        // становилось окно под ним (терминал с herdr).
        set_panel_on_pc(false);
        assert!(!is_own_page("Tapswitch - Google Chrome"));
        set_panel_on_pc(true);
        assert!(is_own_page("Tapswitch - Google Chrome"));
        assert!(is_own_page("Agent Control - Google Chrome"));
        assert!(is_own_page("Панель агентов - Google Chrome"));
        assert!(!is_own_page("GitHub - Google Chrome"));
        set_panel_on_pc(false);
    }

    #[test]
    fn splits_title() {
        let cases = [
            ("Отчёт.docx - Word", ("Отчёт.docx", "Word")),
            ("План - черновик - Word", ("План - черновик", "Word")),
            ("Новости — Google Chrome", ("Новости", "Google Chrome")),
            ("Калькулятор", ("Калькулятор", "")),
            ("a-b - c", ("a-b", "c")),
        ];
        for (title, (document, app)) in cases {
            assert_eq!(split_title(title), (document.to_string(), app.to_string()), "{title}");
        }
    }

    #[test]
    fn current_describes_last_active_window() {
        let session = WindowSessions::new(Box::new(FakeDesktop::new(&[word(10, "Отчёт.docx - Word")]))).current().unwrap();
        assert_eq!(
            Value::Object(session),
            json!({"kind": "window", "server": "WINWORD.EXE", "id": 10, "window_class": "OpusApp", "exe": WORD,
                   "name": "Отчёт.docx - Word", "title": "Отчёт.docx", "app": "Word", "icon": WORD})
        );
        let error = WindowSessions::new(Box::new(FakeDesktop::new(&[]))).current().unwrap_err();
        assert!(error.to_string().contains("окно приложения"));
    }

    #[test]
    fn open_shows_same_window_even_if_title_changed() {
        let desktop = FakeDesktop::new(&[info(5, "Почта — Google Chrome", "Chrome_WidgetWin_1", CHROME)]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let mut session = sessions.current().unwrap();
        desktop.set(&[info(5, "Новости — Google Chrome", "Chrome_WidgetWin_1", CHROME)]);
        sessions.open(&mut session).unwrap();
        assert_eq!(desktop.shown(), [5]);
        assert_eq!(session["title"], "Новости");
    }

    #[test]
    fn reopened_document_is_found_by_title_with_new_handle() {
        let desktop = FakeDesktop::new(&[word(10, "Отчёт.docx - Word"), word(11, "Другой.docx - Word")]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let mut session = sessions.current().unwrap();
        desktop.set(&[word(20, "Отчёт.docx* - Word"), word(11, "Другой.docx - Word")]);
        sessions.open(&mut session).unwrap();
        assert_eq!(desktop.shown(), [20]);
        assert_eq!(session["id"], 20);
    }

    #[test]
    fn handle_reused_by_other_program_and_ambiguous_titles_are_lost() {
        let desktop = FakeDesktop::new(&[word(10, "Отчёт.docx - Word")]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let session = sessions.current().unwrap();
        desktop.set(&[info(10, "Отчёт.docx - Word", "Notepad", r"C:\Windows\notepad.exe")]);
        let error = sessions.open(&mut session.clone()).unwrap_err();
        assert!(matches!(&error, SessionError::Lost(text) if text.contains("закрыто")));
        desktop.set(&[word(21, "Отчёт.docx - Word"), word(22, "Отчёт.docx - Word")]);
        assert!(matches!(sessions.open(&mut session.clone()), Err(SessionError::Lost(_))));
        assert!(desktop.shown().is_empty());
        assert_eq!(sessions.probe(&[session]).unwrap()[0].available, Some(false));
    }

    #[test]
    fn restarted_browser_is_its_only_window_whatever_the_title() {
        // После перезагрузки: другой дескриптор, а заголовок — другая открытая вкладка.
        let comet = r"C:\Users\u\AppData\Local\Perplexity\Comet\Application\comet.exe";
        let desktop = FakeDesktop::new(&[info(5, "New session - Claude – Comet", "Chrome_WidgetWin_1", comet)]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let session = sessions.current().unwrap();
        desktop.set(&[info(9, "Seltos - Comet", "Chrome_WidgetWin_1", comet)]);
        let probe = &sessions.probe(&[session.clone()]).unwrap()[0];
        assert_eq!(probe.available, Some(true));
        assert_eq!(probe.info.as_ref().unwrap()["id"], 9); // запомнить новое окно сразу
        // Найденное на прежнем месте ничего не меняет.
        let mut moved = session.clone();
        moved.insert("id".into(), json!(9));
        assert_eq!(sessions.probe(&[moved]).unwrap()[0].info, None);
        // Два окна браузера с чужими заголовками — какое из них, неизвестно.
        desktop.set(&[info(9, "Seltos - Comet", "Chrome_WidgetWin_1", comet), info(8, "Почта - Comet", "Chrome_WidgetWin_1", comet)]);
        assert_eq!(sessions.probe(&[session]).unwrap()[0].available, Some(false));
    }

    #[test]
    fn obsidian_window_is_found_by_vault_when_another_note_is_open() {
        let obsidian = r"C:\Users\u\AppData\Local\Programs\Obsidian\Obsidian.exe";
        let vault = |hwnd, title: &str| info(hwnd, title, "Chrome_WidgetWin_1", obsidian);
        let desktop = FakeDesktop::new(&[vault(1, "02 Kanban - tutor - Obsidian 1.14.5")]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let session = sessions.current().unwrap();
        desktop.set(&[
            vault(7, "Сценарий ролика - tutor - Obsidian 1.15.1"),
            vault(8, "02 Kanban - UavVisionLab - Obsidian 1.15.1"),
            vault(9, "02 Kanban - agent_control - Obsidian 1.15.1"),
        ]);
        let probe = &sessions.probe(&[session]).unwrap()[0];
        assert_eq!((probe.available, &probe.info.as_ref().unwrap()["id"]), (Some(true), &json!(7)));
        assert_eq!(place_of("Новости — Google Chrome"), None);
    }

    #[test]
    fn other_document_of_the_same_program_is_not_the_lost_one() {
        let desktop = FakeDesktop::new(&[word(10, "Отчёт.docx - Word")]);
        let sessions = WindowSessions::new(Box::new(desktop.clone()));
        let session = sessions.current().unwrap();
        desktop.set(&[word(20, "Другой.docx - Word")]);
        assert_eq!(sessions.probe(&[session]).unwrap()[0].available, Some(false));
    }

    #[test]
    fn focused_window_matches_bound_window() {
        let focused = WindowSessions::new(Box::new(FakeDesktop::new(&[]))).focused(10, &[]);
        let bound = json!({"kind": "window", "id": 10, "name": "x"}).as_object().unwrap().clone();
        let other = json!({"kind": "window", "id": 11}).as_object().unwrap().clone();
        assert!(same_session(Some(&bound), focused.as_ref()));
        assert!(!same_session(Some(&other), focused.as_ref()));
    }
}
