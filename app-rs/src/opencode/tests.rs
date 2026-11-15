use super::*;
use std::sync::{Arc, Mutex};

const TUTOR: &str = "ses_e2bf9cbf0ffemcxCU1v04BDGik";
const CHECK: &str = "ses_e2beb3e35ffe0ynJQRuiR26eFC";

fn key(id: &str) -> String {
    format!("sidecar\n/server/c2lkZWNhcg/session/{id}")
}

/// Файл окна, как его пишет OpenCode: значения — JSON строкой.
fn window_file(tabs: &[(&str, &str, &str)], active: Option<&str>) -> Value {
    let list: Vec<Value> = tabs.iter().map(|(id, _, _)| json!({"type": "session", "server": "sidecar", "sessionId": id})).collect();
    let mut info = serde_json::Map::new();
    for (id, title, directory) in tabs {
        info.insert(key(id), json!({"title": title, "directory": directory}));
    }
    let recent = match active {
        Some(id) => json!({"key": key(id)}),
        None => json!({"key": "sidecar\n/home"}),
    };
    json!({
        "tabs": Value::Array(list).to_string(),
        "tabs.recent": recent.to_string(),
        "tabs.info": Value::Object(info).to_string(),
        "tabs.closed": "[]",
    })
}

const TABS: &[(&str, &str, &str)] = &[
    (TUTOR, "Курс AI Tutor: пошаговое сопровождение", r"C:\Projects\my_project"),
    (CHECK, "Проверка задач", r"C:\Projects\my_project\04 Roles\tester"),
];

#[test]
fn parse_window_reads_tabs_titles_and_active_tab() {
    let tabs = parse_window(&window_file(TABS, Some(CHECK)));
    assert_eq!(tabs.active.as_deref(), Some(CHECK));
    assert_eq!(tabs.tabs.len(), 2);
    assert_eq!(tabs.tabs[1].title.as_deref(), Some("Проверка задач"));
    assert_eq!(tabs.tabs[0].directory.as_deref(), Some(r"C:\Projects\my_project"));
    assert_eq!(parse_window(&window_file(TABS, None)).active, None);
    assert_eq!(parse_window(&json!({})), WindowTabs::default());
}

struct TempHome(PathBuf);

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn store(tabs: &[(&str, &str, &str)], active: Option<&str>) -> (TempHome, OpencodeStore) {
    static NEXT: Mutex<u32> = Mutex::new(0);
    let mut next = NEXT.lock().unwrap();
    *next += 1;
    let dir = std::env::temp_dir().join(format!("agent_control_opencode_test_{}_{}", std::process::id(), *next));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("opencode.window.a1347120.dat"), window_file(tabs, active).to_string()).unwrap();
    std::fs::write(dir.join("opencode.global.dat"), "{}").unwrap();
    (TempHome(dir.clone()), OpencodeStore::new(dir))
}

#[derive(Clone, Default)]
struct FakeUi {
    opened: Arc<Mutex<Vec<String>>>,
    shown: Arc<Mutex<usize>>,
    stopped: bool,
    covered: bool,
}

impl OpencodeUi for FakeUi {
    fn window(&self) -> Option<i64> {
        (!self.stopped).then_some(7)
    }
    fn show(&self, _: i64) {
        *self.shown.lock().unwrap() += 1;
    }
    fn open_tab(&self, _: i64, id: &str) -> Result<()> {
        if self.covered {
            return Err(SessionError::Other("окно перекрыто".into()));
        }
        self.opened.lock().unwrap().push(id.to_string());
        Ok(())
    }
    fn is_opencode(&self, window: i64) -> bool {
        window == 7
    }
}

fn sessions(ui: &FakeUi, store: OpencodeStore) -> OpencodeSessions {
    OpencodeSessions::new(Box::new(ui.clone()), store)
}

fn session(value: Value) -> Session {
    value.as_object().unwrap().clone()
}

#[test]
fn current_is_active_tab_with_id_project_and_title() {
    let (_home, store) = store(TABS, Some(CHECK));
    let found = sessions(&FakeUi::default(), store).current().unwrap();
    assert_eq!(found["kind"], "opencode");
    assert_eq!(found["id"], CHECK);
    assert_eq!(found["name"], "Проверка задач");
    assert_eq!(found["project"], "my_project");
    assert_eq!(found["role"], "tester");
    assert_eq!((&found["agent"], &found["ready"]), (&json!("opencode"), &json!(false)));
}

#[test]
fn current_without_session_tab_or_app_is_an_error() {
    let (_home, store1) = store(TABS, None);
    assert!(sessions(&FakeUi::default(), store1).current().unwrap_err().to_string().contains("не открыта"));
    let (_home, store2) = store(TABS, Some(CHECK));
    let ui = FakeUi { stopped: true, ..Default::default() };
    assert!(sessions(&ui, store2).current().unwrap_err().to_string().contains("не запущен"));
}

#[test]
fn open_shows_window_and_switches_to_tab_by_id() {
    let (_home, store) = store(TABS, Some(CHECK));
    let ui = FakeUi::default();
    let mut bound = session(json!({"kind": "opencode", "id": TUTOR, "name": "старое название"}));
    sessions(&ui, store).open(&mut bound).unwrap();
    assert_eq!(*ui.opened.lock().unwrap(), vec![TUTOR.to_string()]);
    assert_eq!(*ui.shown.lock().unwrap(), 1);
    assert_eq!(bound["name"], "Курс AI Tutor: пошаговое сопровождение"); // переименованная вкладка
}

#[test]
fn open_closed_tab_is_lost_and_does_not_touch_window() {
    let (_home, store) = store(&TABS[..1], Some(TUTOR));
    let ui = FakeUi::default();
    let mut bound = session(json!({"kind": "opencode", "id": CHECK, "name": "Проверка задач"}));
    let error = sessions(&ui, store).open(&mut bound).unwrap_err();
    assert!(matches!(error, SessionError::Lost(_)), "{error}");
    assert_eq!(*ui.shown.lock().unwrap(), 0);
}

#[test]
fn open_when_tab_strip_unreadable_is_temporary() {
    let (_home, store) = store(TABS, Some(CHECK));
    let ui = FakeUi { covered: true, ..Default::default() };
    let mut bound = session(json!({"kind": "opencode", "id": CHECK}));
    assert!(matches!(sessions(&ui, store).open(&mut bound), Err(SessionError::Other(_))));
}

#[test]
fn probe_marks_closed_tabs_and_stopped_app_unavailable() {
    let (_home, store1) = store(&TABS[..1], Some(TUTOR));
    let bound = [
        session(json!({"kind": "opencode", "id": TUTOR, "name": "Курс"})),
        session(json!({"kind": "opencode", "id": CHECK, "name": "Проверка задач"})),
    ];
    let found = sessions(&FakeUi::default(), store1).probe(&bound).unwrap();
    assert_eq!((found[0].available, found[1].available), (Some(true), Some(false)));
    assert_eq!(found[0].info.as_ref().unwrap()["name"], "Курс AI Tutor: пошаговое сопровождение");
    let (_home, store2) = store(TABS, Some(TUTOR));
    let ui = FakeUi { stopped: true, ..Default::default() };
    let found = sessions(&ui, store2).probe(&bound).unwrap();
    assert!(found.iter().all(|p| p.available == Some(false)));
}
