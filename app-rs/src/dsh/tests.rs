//! Перенос `tests/test_dsh.py`.

use super::*;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[test]
fn row_names() {
    for (name, title, running) in [
        ("проверь характиристи now", "проверь характиристи", false),
        ("Running проверь характиристи 2min", "проверь характиристи", true),
        ("ау, ты здесь? 1mo", "ау, ты здесь?", false),
        ("Untitled 23min", "Untitled", false),
        ("Untitled 23 min", "Untitled", false),
        ("Задача на 5 минут", "Задача на 5 минут", false),
    ] {
        assert_eq!(parse_row_name(name), (title.to_string(), running), "{name}");
    }
}

fn row(kind: &str, name: &str, selected: bool, expanded: Option<bool>) -> Row {
    Row { kind: kind.into(), name: name.into(), selected, expanded }
}

/// Список сессий как в окне dsh; more_hidden — сессии за «Show N more sessions».
fn screen(selected: Option<&str>, running: &[&str], collapsed: &[&str], more_hidden: &[&str]) -> Vec<Row> {
    let layout: [(&str, &[&str]); 2] = [
        ("for_agents", &["проверь характиристи", "Проверка связи", "ты здесь?", "ты здесь?"]),
        ("architect", &["вопрос про передачу в"]),
    ];
    let mut rows = Vec::new();
    for (workspace, titles) in layout {
        let folded = collapsed.contains(&workspace);
        rows.push(row("workspace", workspace, false, Some(!folded)));
        if folded {
            continue;
        }
        for title in titles {
            if more_hidden.contains(title) {
                continue;
            }
            let prefix = if running.contains(title) { "Running " } else { "" };
            rows.push(row("session", &format!("{prefix}{title} 5min"), selected == Some(*title), None));
        }
        if !more_hidden.is_empty() && workspace == "for_agents" {
            rows.push(row("more", &format!("Show {} more sessions", more_hidden.len()), false, None));
        }
    }
    rows
}

fn plain() -> Vec<Row> {
    screen(None, &[], &[], &[])
}

#[test]
fn session_rows_know_workspace_and_rank_of_same_titles() {
    let rows = session_rows(&plain());
    let same: Vec<(Option<&str>, usize)> =
        rows.iter().filter(|r| r.title == "ты здесь?").map(|r| (r.workspace.as_deref(), r.rank)).collect();
    assert_eq!(same, vec![(Some("for_agents"), 0), (Some("for_agents"), 1)]);
    assert_eq!(rows.last().unwrap().workspace.as_deref(), Some("architect"));
}

#[test]
fn locate_falls_back_to_first_of_same_titles() {
    let rows = plain();
    assert_eq!(locate(&rows, Some("for_agents"), "ты здесь?", 1).unwrap().rank, 1);
    assert_eq!(locate(&rows, Some("for_agents"), "ты здесь?", 5).unwrap().rank, 0);
    assert!(locate(&rows, Some("architect"), "ты здесь?", 0).is_none());
}

struct TempHome(PathBuf);

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_home() -> TempHome {
    static NEXT: Mutex<u32> = Mutex::new(0);
    let mut next = NEXT.lock().unwrap();
    *next += 1;
    let dir = std::env::temp_dir().join(format!("agent_control_dsh_test_{}_{}", std::process::id(), *next));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TempHome(dir)
}

type Entry<'a> = (&'a str, &'a str, Option<&'a str>, bool, u64);

/// sessions: (id, рабочая область, название или None, blank, mtime).
fn write_store(home: &TempHome, sessions: &[Entry], archived: &[&str]) -> DshStore {
    let mut workspaces = serde_json::Map::new();
    let mut cache = serde_json::Map::new();
    for (id, workspace, title, blank, time) in sessions {
        let entry = workspaces.entry(workspace.to_string()).or_insert_with(
            || json!({"title": workspace, "path": format!("C:\\Projects\\{workspace}"), "sessionIds": []}),
        );
        entry["sessionIds"].as_array_mut().unwrap().push(json!(id));
        let mut rows = json!({"sessionListMetadata": {"val": {"blank": blank}}});
        if let Some(title) = title {
            rows["title"] = json!({"val": title});
        }
        cache.insert(id.to_string(), json!({"rows": rows}));
        let folder = home.0.join("sessions").join(format!("--{}--", workspace.replace(" / ", "-"))).join(id);
        std::fs::create_dir_all(&folder).unwrap();
        let data = folder.join("session.v4.jsonl.zstd");
        let file = std::fs::File::create(&data).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 + time)).unwrap();
    }
    let storages = home.0.join("storages");
    std::fs::create_dir_all(&storages).unwrap();
    let layout = json!({"global": {"archivedSessionIds": archived}, "tables": {"workspaces": workspaces}});
    std::fs::write(storages.join("workspace.json"), layout.to_string()).unwrap();
    std::fs::write(storages.join("session_projcache.json"), json!({"tables": {"sessions": cache}}).to_string()).unwrap();
    DshStore::new(home.0.clone())
}

const SESSIONS: &[Entry] = &[
    ("session-check", "for_agents", Some("проверь характиристи"), false, 300),
    ("session-ping", "for_agents", None, false, 250), // название ещё не записано на диск
    ("session-new", "for_agents", Some("ты здесь?"), false, 200),
    ("session-old", "for_agents", Some("ты здесь?"), false, 100),
    ("session-arch", "architect", Some("вопрос про передачу в"), false, 50),
];

#[test]
fn resolve_orders_same_titles_by_recency_and_skips_archived() {
    let home = temp_home();
    let mut all = SESSIONS.to_vec();
    all.push(("session-gone", "architect", Some("вопрос про передачу в"), false, 90));
    let records = write_store(&home, &all, &["session-gone"]).records();
    let id = |w: &str, t: &str, r: usize| resolve(&records, Some(w), t, r).map(|r| r.id.clone());
    assert_eq!(id("for_agents", "ты здесь?", 0).as_deref(), Some("session-new"));
    assert_eq!(id("for_agents", "ты здесь?", 1).as_deref(), Some("session-old"));
    assert_eq!(id("architect", "вопрос про передачу в", 0).as_deref(), Some("session-arch"));
    // Названия нет на диске: единственная сессия без названия.
    assert_eq!(id("for_agents", "Проверка связи", 0).as_deref(), Some("session-ping"));
}

#[test]
fn store_without_dsh_data_is_empty() {
    let home = temp_home();
    assert!(DshStore::new(home.0.join("missing")).records().is_empty());
}

#[derive(Default)]
struct UiState {
    /// Окно перекрыто: список не читается.
    covered: bool,
    rows: Vec<Row>,
    actions: Vec<(String, String)>,
    shown: usize,
}

#[derive(Clone)]
struct FakeUi {
    state: Arc<Mutex<UiState>>,
    window: Option<i64>,
}

impl FakeUi {
    fn new(rows: Vec<Row>) -> Self {
        Self { state: Arc::new(Mutex::new(UiState { rows, ..Default::default() })), window: Some(7) }
    }
    fn actions(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().actions.clone()
    }
    fn fail_read(&self) {
        self.state.lock().unwrap().covered = true;
    }
}

impl DshUi for FakeUi {
    fn window(&self) -> Option<i64> {
        self.window
    }
    fn read(&self, _: i64) -> Result<Vec<Row>> {
        let state = self.state.lock().unwrap();
        if state.covered {
            return Err(SessionError::Other("окно перекрыто".into()));
        }
        Ok(state.rows.clone())
    }
    fn act(&self, _: i64, action: &str, row: usize, name: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.rows[row].name, name);
        state.actions.push((action.into(), name.into()));
        if action == "click" {
            state.rows = plain(); // раскрыть рабочую область или «Show N more sessions»
        }
        Ok(())
    }
    fn show(&self, _: i64) {
        self.state.lock().unwrap().shown += 1;
    }
    fn is_dsh(&self, window: i64) -> bool {
        Some(window) == self.window
    }
}

fn sessions(ui: &FakeUi, store: DshStore) -> DshSessions {
    let mut sessions = DshSessions::new(Box::new(ui.clone()), store);
    sessions.settle = Duration::ZERO;
    sessions
}

fn session(value: Value) -> Session {
    value.as_object().unwrap().clone()
}

fn action(kind: &str, name: &str) -> (String, String) {
    (kind.into(), name.into())
}

#[test]
fn current_takes_selected_session_with_id_role_and_status() {
    let home = temp_home();
    let store = write_store(&home, SESSIONS, &[]);
    let ui = FakeUi::new(screen(Some("проверь характиристи"), &["проверь характиристи"], &[], &[]));
    let found = sessions(&ui, store).current().unwrap();
    assert_eq!(found["kind"], "dsh");
    assert_eq!(found["id"], "session-check");
    assert_eq!(found["name"], "проверь характиристи");
    assert_eq!(found["title"], "for_agents|core");
    assert_eq!((&found["agent"], &found["agent_status"], &found["ready"]), (&json!("dsh"), &json!("working"), &json!(false)));
}

#[test]
fn current_without_selection_or_window_is_an_error() {
    let home = temp_home();
    let error = sessions(&FakeUi::new(plain()), write_store(&home, SESSIONS, &[])).current().unwrap_err();
    assert!(error.to_string().contains("не выбрана"));
    let mut ui = FakeUi::new(plain());
    ui.window = None;
    let error = sessions(&ui, write_store(&home, SESSIONS, &[])).current().unwrap_err();
    assert!(error.to_string().contains("не запущен"));
}

#[test]
fn open_selects_second_of_same_titles_by_id() {
    let home = temp_home();
    let ui = FakeUi::new(plain());
    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": "session-old", "workspace": "for_agents",
                                   "name": "ты здесь?", "rank": 0}));
    sessions(&ui, write_store(&home, SESSIONS, &[])).open(&mut found).unwrap();
    let rows = plain();
    let second = session_rows(&rows).into_iter().filter(|r| r.title == "ты здесь?").nth(1).unwrap();
    assert_eq!(ui.actions(), vec![action("select", &rows[second.index].name)]);
    assert_eq!(ui.state.lock().unwrap().shown, 1);
}

#[test]
fn open_follows_rename() {
    let home = temp_home();
    let ui = FakeUi::new(plain());
    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": "session-check", "workspace": "for_agents",
                                   "name": "старое название", "rank": 0}));
    sessions(&ui, write_store(&home, SESSIONS, &[])).open(&mut found).unwrap();
    assert_eq!(found["name"], "проверь характиристи");
    assert_eq!(ui.actions(), vec![action("select", "проверь характиристи 5min")]);
}

#[test]
fn open_clicks_collapsed_workspace_and_hidden_sessions() {
    let home = temp_home();
    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": "session-arch", "workspace": "architect",
                                   "name": "вопрос про передачу в", "rank": 0}));
    let ui = FakeUi::new(screen(None, &[], &["architect"], &[]));
    sessions(&ui, write_store(&home, SESSIONS, &[])).open(&mut found).unwrap();
    assert_eq!(ui.actions(), vec![action("click", "architect"), action("select", "вопрос про передачу в 5min")]);

    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": "session-old", "workspace": "for_agents",
                                   "name": "ты здесь?", "rank": 1}));
    let ui = FakeUi::new(screen(None, &[], &[], &["ты здесь?"]));
    sessions(&ui, write_store(&home, SESSIONS, &[])).open(&mut found).unwrap();
    let actions = ui.actions();
    assert_eq!(actions.iter().map(|a| a.0.as_str()).collect::<Vec<_>>(), vec!["click", "select"]);
    assert!(actions[0].1.starts_with("Show "));
}

#[test]
fn open_reports_deleted_or_archived_session_as_lost() {
    let home = temp_home();
    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": "session-deleted", "workspace": "for_agents",
                                   "name": "ты здесь?", "rank": 0}));
    let result = sessions(&FakeUi::new(plain()), write_store(&home, SESSIONS, &[])).open(&mut found);
    assert!(matches!(result, Err(SessionError::Lost(_))));
}

#[test]
fn open_without_id_uses_first_row_with_title() {
    let home = temp_home();
    let ui = FakeUi::new(plain());
    let dsh = sessions(&ui, write_store(&home, SESSIONS, &[]));
    let mut found = session(json!({"kind": "dsh", "server": "dsh", "id": null, "workspace": "for_agents",
                                   "name": "Проверка связи", "rank": 0}));
    dsh.open(&mut found).unwrap();
    assert_eq!(ui.actions(), vec![action("select", "Проверка связи 5min")]);
    found.insert("name".into(), json!("нет такой"));
    assert!(matches!(dsh.open(&mut found), Err(SessionError::Lost(_))));
}

#[test]
fn probe_reports_running_and_deleted_sessions() {
    let home = temp_home();
    let ui = FakeUi::new(screen(None, &["проверь характиристи"], &["architect"], &[]));
    let dsh = sessions(&ui, write_store(&home, SESSIONS, &[]));
    let results = dsh
        .probe(&[
            session(json!({"kind": "dsh", "id": "session-check", "workspace": "for_agents", "name": "x", "rank": 0})),
            session(json!({"kind": "dsh", "id": "session-arch", "workspace": "architect", "name": "вопрос про передачу в"})),
            session(json!({"kind": "dsh", "id": "session-deleted", "workspace": "for_agents", "name": "ты здесь?"})),
        ])
        .unwrap();
    let info = results[0].info.as_ref().unwrap();
    assert_eq!((results[0].available, &info["agent_status"]), (Some(true), &json!("working")));
    assert!(!info.contains_key("name"));
    // Свёрнутая рабочая область: доступна, состояние неизвестно.
    assert_eq!(results[1], Probe { available: Some(true), info: None });
    assert_eq!(results[2], Probe { available: Some(false), info: None });
}

/// Файл сессии, который dsh перезаписывает на каждом шаге: открыт ли ход агента.
fn write_turn(home: &TempHome, id: &str, title: &str, open: bool) {
    let dir = home.0.join("storages").join("session_projcache").join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let rows = json!({
        "title": {"val": title},
        "sessionListMetadata": {"val": {"blank": false}},
        "turnBoundary": {"val": {"openTurnStartSeq": if open { json!(968) } else { Value::Null }}},
    });
    std::fs::write(dir.join(format!("{id}.json")), json!({"record": {"rows": rows}}).to_string()).unwrap();
}

#[test]
fn probe_trusts_session_file_over_stale_window() {
    let home = temp_home();
    // Окно dsh в фоне всё ещё показывает «Running», а ход уже закончен — и наоборот.
    let ui = FakeUi::new(screen(None, &["проверь характиристи", "ты здесь?"], &[], &[]));
    let store = write_store(&home, SESSIONS, &[]);
    write_turn(&home, "session-check", "проверь характиристи", false);
    write_turn(&home, "session-arch", "вопрос про передачу в", true);
    std::fs::write(home.0.join("storages").join("session_projcache").join("sessions").join("session-new.json"), "{обрыв").unwrap();
    let dsh = sessions(&ui, store);
    let status = |id: &str, workspace: &str, name: &str| {
        let probe = dsh.probe(&[session(json!({"kind": "dsh", "id": id, "workspace": workspace, "name": name}))]).unwrap();
        probe[0].info.as_ref().unwrap()["agent_status"].clone()
    };
    assert_eq!(status("session-check", "for_agents", "проверь характиристи"), json!("unknown"));
    assert_eq!(status("session-arch", "architect", "вопрос про передачу в"), json!("working"));
    // Файл недочитан (dsh как раз пишет) — состояние по окну.
    assert_eq!(status("session-new", "for_agents", "ты здесь?"), json!("working"));
}

#[test]
fn button_without_id_finds_its_session_and_gets_id() {
    let home = temp_home();
    // Кнопку назначили, пока у сессии не было названия на диске: id нет. Потом плагин dsh
    // переименовал область «architect» в «UavVisionLab / architect».
    let renamed = [("session-arch", "UavVisionLab / architect", Some("вопрос про передачу в"), false, 50)];
    let store = write_store(&home, &renamed, &[]);
    write_turn(&home, "session-arch", "вопрос про передачу в", true);
    let rows = vec![
        Row { kind: "workspace".into(), name: "UavVisionLab / architect".into(), selected: false, expanded: Some(true) },
        Row { kind: "session".into(), name: "вопрос про передачу в 2mo".into(), selected: false, expanded: None },
    ];
    let ui = FakeUi::new(rows);
    let dsh = sessions(&ui, store);
    let mut button = session(json!({"kind": "dsh", "workspace": "architect", "name": "вопрос про передачу в", "rank": 0}));
    let probe = dsh.probe(std::slice::from_ref(&button)).unwrap();
    assert_eq!(probe[0].info.as_ref().unwrap()["agent_status"], json!("working"));
    dsh.open(&mut button).unwrap();
    assert_eq!(button["id"], json!("session-arch"));
    assert_eq!(button["workspace"], json!("UavVisionLab / architect"));
}

#[test]
fn probe_uses_session_file_when_window_is_covered() {
    let home = temp_home();
    let store = write_store(&home, SESSIONS, &[]);
    write_turn(&home, "session-check", "проверь характиристи", false);
    // Перекрытое окно dsh список не отдаёт.
    let ui = FakeUi::new(screen(None, &["проверь характиристи"], &[], &[]));
    ui.fail_read();
    let dsh = sessions(&ui, store);
    let probe = dsh
        .probe(&[
            session(json!({"kind": "dsh", "id": "session-check", "workspace": "for_agents", "name": "проверь характиристи"})),
            session(json!({"kind": "dsh", "id": "session-arch", "workspace": "architect", "name": "вопрос про передачу в"})),
        ])
        .unwrap();
    assert_eq!(probe[0].available, Some(true));
    assert_eq!(probe[0].info.as_ref().unwrap()["agent_status"], json!("unknown"));
    // Файла сессии нет — без изменений.
    assert_eq!(probe[1], Probe { available: None, info: None });
}

#[test]
fn env_variables_expand() {
    // SAFETY: тест один пишет эту переменную.
    unsafe { std::env::set_var("AGENT_CONTROL_TEST_HOME", r"C:\Users\u") };
    assert_eq!(expand_env(r"%AGENT_CONTROL_TEST_HOME%\.dsh"), r"C:\Users\u\.dsh");
    assert_eq!(expand_env(r"%NO_SUCH_VARIABLE_X%\a"), r"%NO_SUCH_VARIABLE_X%\a");
    assert_eq!(expand_env("100%"), "100%");
}
