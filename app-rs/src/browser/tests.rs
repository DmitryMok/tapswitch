//! Перенос `tests/test_browser.py`.

use super::*;
use crate::sessions::same_session;
use std::sync::Mutex;

const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
const OPERA: &str = r"C:\Users\u\AppData\Local\Programs\Opera\opera.exe";
const CHROME_ID: &str = "11111111-aaaa";
const OPERA_ID: &str = "22222222-bbbb";

fn session(value: Value) -> Session {
    value.as_object().unwrap().clone()
}

#[derive(Clone, Default)]
struct FakeDesktop {
    /// Окна от последнего активного: (hwnd, заголовок, программа).
    windows: Vec<(i64, String, String)>,
    shown: Arc<Mutex<Vec<i64>>>,
}

impl FakeDesktop {
    fn new(windows: &[(i64, &str, &str)]) -> Self {
        Self {
            windows: windows.iter().map(|(w, t, e)| (*w, t.to_string(), e.to_string())).collect(),
            shown: Arc::default(),
        }
    }
    fn get(&self, window: i64) -> Option<&(i64, String, String)> {
        self.windows.iter().find(|w| w.0 == window)
    }
}

impl Desktop for FakeDesktop {
    fn windows(&self, _: bool) -> Vec<i64> {
        self.windows.iter().map(|w| w.0).collect()
    }
    fn is_app(&self, _: i64) -> bool {
        true
    }
    fn title(&self, window: i64) -> String {
        self.get(window).map(|w| w.1.clone()).unwrap_or_default()
    }
    fn exe(&self, window: i64) -> String {
        self.get(window).map(|w| w.2.clone()).unwrap_or_default()
    }
    fn show(&self, window: i64) -> bool {
        self.shown.lock().unwrap().push(window);
        true
    }
}

#[derive(Clone, Default)]
struct FakeBridge {
    calls: Arc<Mutex<Vec<(String, i64, i64, bool)>>>,
}

impl Bridge for FakeBridge {
    fn activate(&self, instance: &str, tab: i64, window: i64, focus: bool) -> bool {
        self.calls.lock().unwrap().push((instance.into(), tab, window, focus));
        true
    }
}

fn tab(id: i64, title: &str, url: &str, active: bool) -> Tab {
    Tab { id, title: title.into(), url: url.into(), active }
}

fn window(id: i64, focused: bool, tabs: Vec<Tab>) -> Window {
    Window { id, focused, tabs }
}

struct Setup {
    tabs: BrowserTabs,
    registry: Arc<TabRegistry>,
    bridge: FakeBridge,
    desktop: FakeDesktop,
}

fn setup_with(desktop: Option<&[(i64, &str, &str)]>) -> Setup {
    let registry = Arc::new(TabRegistry::default());
    registry.connect(CHROME_ID, "Google Chrome");
    registry.set_windows(
        CHROME_ID,
        vec![
            window(
                1,
                true,
                vec![tab(10, "Почта", "https://mail.example/", true), tab(11, "Docs - План", "https://docs.example/plan", false)],
            ),
            window(2, false, vec![tab(20, "Docs", "https://docs.example/", true)]),
        ],
    );
    registry.connect(OPERA_ID, "Opera");
    registry.set_windows(OPERA_ID, vec![window(1, true, vec![tab(10, "Новости", "https://news.example/", true)])]);
    let default: &[(i64, &str, &str)] = &[
        (100, "Эмулятор панели", r"C:\python.exe"),
        (200, "Новости - Opera", OPERA),
        (300, "Почта - Google Chrome", CHROME),
        (400, "Docs - Google Chrome", CHROME),
    ];
    let desktop = FakeDesktop::new(desktop.unwrap_or(default));
    let bridge = FakeBridge::default();
    let tabs = BrowserTabs::new(registry.clone(), Box::new(bridge.clone()), Box::new(desktop.clone()));
    Setup { tabs, registry, bridge, desktop }
}

fn setup() -> Setup {
    setup_with(None)
}

#[test]
fn connected_browsers_are_listed_once_in_alphabetical_order() {
    let s = setup();
    assert_eq!(s.registry.browsers(), ["Google Chrome", "Opera"]);
    s.registry.connect("33333333-cccc", "Google Chrome");
    assert_eq!(s.registry.browsers(), ["Google Chrome", "Opera"]);
    s.registry.disconnect(CHROME_ID);
    assert_eq!(s.registry.browsers(), ["Google Chrome", "Opera"]);
    s.registry.disconnect("33333333-cccc");
    assert_eq!(s.registry.browsers(), ["Opera"]);
}

#[test]
fn title_matching() {
    for (window, tab, expected) in [
        ("Почта - Google Chrome", "Почта", true),
        ("Почта - Google Chrome - Работа", "Почта", true),
        ("YouTube - Воспроизведение аудио - Google Chrome", "YouTube", true),
        ("Почтальон - Google Chrome", "Почта", false),
        ("Почта", "Почта", true),
        ("Google Chrome", "", false),
    ] {
        assert_eq!(title_matches(window, tab), expected, "{window} / {tab}");
    }
}

#[test]
fn window_title_prefers_longest_tab_title() {
    let s = setup();
    s.registry.set_windows(
        CHROME_ID,
        vec![window(1, true, vec![tab(11, "Docs - План", "u1", true)]), window(2, false, vec![tab(20, "Docs", "u2", true)])],
    );
    assert_eq!(s.registry.by_window_title("Docs - План - Google Chrome").unwrap().2.id, 11);
    assert_eq!(s.registry.by_window_title("Docs - Google Chrome").unwrap().2.id, 20);
}

#[test]
fn matches_only_browser_windows_known_to_extension() {
    let s = setup();
    assert!(s.tabs.matches(300) && s.tabs.matches(200));
    assert!(!s.tabs.matches(100));
}

#[test]
fn matches_nothing_without_extension() {
    let s = setup();
    s.registry.disconnect(CHROME_ID);
    s.registry.disconnect(OPERA_ID);
    assert!(!s.tabs.matches(300));
}

#[test]
fn current_is_active_tab_of_last_active_browser_window() {
    let s = setup();
    assert_eq!(
        s.tabs.current().unwrap(),
        session(json!({
            "kind": "tab", "server": "Opera", "id": "22222222:10", "instance": OPERA_ID, "tab": 10,
            "window": 1, "url": "https://news.example/", "name": "Новости", "title": "Новости",
            "app": "Opera", "icon": OPERA,
        }))
    );
}

#[test]
fn current_without_browser_window_is_an_error() {
    let s = setup_with(Some(&[(100, "Эмулятор панели", "")]));
    assert!(s.tabs.current().unwrap_err().to_string().contains("расширением"));
}

#[test]
fn same_tab_number_in_two_browsers_is_different_session() {
    let s = setup();
    let opera = s.tabs.current().unwrap();
    let mut chrome = opera.clone();
    chrome.insert("id".into(), json!("11111111:10"));
    chrome.insert("instance".into(), json!(CHROME_ID));
    assert!(!same_session(Some(&opera), Some(&chrome)));
}

#[test]
fn open_activates_tab_and_shows_its_browser_window() {
    let s = setup();
    let mut found = session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 11, "name": "Docs - План", "icon": CHROME}));
    s.tabs.open(&mut found).unwrap();
    // Окно 1 Chrome сейчас показывает вкладку «Почта» — это окно 300.
    assert_eq!(*s.bridge.calls.lock().unwrap(), vec![(CHROME_ID.to_string(), 11, 1, false)]);
    assert_eq!(*s.desktop.shown.lock().unwrap(), vec![300]);
    assert_eq!(found["id"], "11111111:11");
    assert_eq!(found["url"], "https://docs.example/plan");
}

#[test]
fn open_asks_extension_to_focus_when_window_not_found() {
    let s = setup_with(Some(&[(100, "Эмулятор панели", "")]));
    s.tabs.open(&mut session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 20, "name": "Docs"}))).unwrap();
    assert_eq!(*s.bridge.calls.lock().unwrap(), vec![(CHROME_ID.to_string(), 20, 2, true)]);
    assert!(s.desktop.shown.lock().unwrap().is_empty());
}

#[test]
fn open_after_browser_restart_finds_tab_by_url() {
    let s = setup();
    s.registry.set_windows(
        CHROME_ID,
        vec![window(
            7,
            true,
            vec![tab(70, "Почта", "https://mail.example/", true), tab(71, "Docs - План (изменён)", "https://docs.example/plan", false)],
        )],
    );
    let mut found = session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 11, "name": "Docs - План",
                                   "url": "https://docs.example/plan"}));
    s.tabs.open(&mut found).unwrap();
    assert_eq!(*s.bridge.calls.lock().unwrap(), vec![(CHROME_ID.to_string(), 71, 7, false)]);
    assert_eq!(found["tab"], 71);
    assert_eq!(found["title"], "Docs - План (изменён)");
}

#[test]
fn open_closed_tab_is_lost() {
    let s = setup();
    let mut missing = session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 99, "name": "Нет такой", "url": "https://x/"}));
    match s.tabs.open(&mut missing) {
        Err(SessionError::Lost(text)) => assert!(text.contains("закрыта")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn open_without_extension_is_lost() {
    let s = setup();
    s.registry.disconnect(CHROME_ID);
    s.registry.disconnect(OPERA_ID);
    match s.tabs.open(&mut session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 10, "name": "Почта"}))) {
        Err(SessionError::Lost(text)) => assert!(text.contains("расширение")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn ambiguous_url_is_not_guessed() {
    let s = setup();
    s.registry.set_windows(
        CHROME_ID,
        vec![window(1, true, vec![tab(1, "A", "https://same/", true), tab(2, "B", "https://same/", false)])],
    );
    let result = s.tabs.open(&mut session(json!({"kind": "tab", "instance": CHROME_ID, "tab": 99, "url": "https://same/"})));
    assert!(matches!(result, Err(SessionError::Lost(_))));
}

#[test]
fn focused_is_active_tab_of_window() {
    let s = setup();
    assert_eq!(s.tabs.focused(300, &[]), Some(session(json!({"kind": "tab", "id": "11111111:10"}))));
    assert_eq!(s.tabs.focused(100, &[]), None);
}

#[test]
fn probe_updates_title_and_reports_closed_tabs() {
    let s = setup();
    s.registry.set_windows(OPERA_ID, vec![window(1, true, vec![tab(10, "Новости дня", "https://news.example/", true)])]);
    let results = s
        .tabs
        .probe(&[
            session(json!({"kind": "tab", "instance": OPERA_ID, "tab": 10, "icon": OPERA})),
            session(json!({"kind": "tab", "instance": OPERA_ID, "tab": 55})),
        ])
        .unwrap();
    assert_eq!(results[0].available, Some(true));
    assert_eq!(results[0].info.as_ref().unwrap()["title"], "Новости дня");
    assert_eq!(results[1], Probe { available: Some(false), info: None });
}

#[test]
fn origin_only_our_extension() {
    let ids = vec![EXTENSION_ID.to_string()];
    assert!(allowed_origin(&format!("chrome-extension://{EXTENSION_ID}"), &ids));
    assert!(!allowed_origin("chrome-extension://other", &ids));
    assert!(!allowed_origin("http://evil.example", &ids));
    assert!(!allowed_origin("", &ids));
}

#[test]
fn messages_fill_registry_and_forget_on_close() {
    let registry = Arc::new(TabRegistry::default());
    let logged = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = logged.clone();
    let messages = Messages::new(registry.clone(), Box::new(move |text| sink.lock().unwrap().push(text.into())));
    let mut connection = None;
    messages.handle(&mut connection, r#"{"type": "tabs", "windows": []}"#); // до hello — игнор
    assert!(!registry.connected());
    messages.handle(&mut connection, &json!({"type": "hello", "instance": CHROME_ID, "browser": "Google Chrome"}).to_string());
    messages.handle(
        &mut connection,
        &json!({"type": "tabs", "windows": [
            {"id": "3", "focused": true, "tabs": [{"id": 5, "title": "Почта", "url": "u", "active": true, "extra": 1}]},
        ]})
        .to_string(),
    );
    messages.handle(&mut connection, "not json");
    messages.handle(&mut connection, r#"{"type": "tabs", "windows": [{"id": 1}]}"#); // битый — игнор
    assert_eq!(
        registry.by_window_title("Почта - Google Chrome").unwrap().1,
        window(3, true, vec![tab(5, "Почта", "u", true)])
    );
    messages.closed(&connection);
    assert!(!registry.connected());
    assert_eq!(*logged.lock().unwrap(), vec!["Расширение подключено: Google Chrome", "Расширение отключилось: Google Chrome"]);
}
