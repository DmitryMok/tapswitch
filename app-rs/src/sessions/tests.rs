//! Тесты сессий herdr — перенос из `app/tests/test_core.py`.

use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn obj(value: Value) -> Session {
    value.as_object().unwrap().clone()
}

fn completed(payload: Value, code: i32) -> std::result::Result<Output, String> {
    Ok(Output { code, stdout: payload.to_string(), stderr: String::new() })
}

type Calls = Arc<Mutex<Vec<Vec<String>>>>;

/// panes: идентификатор панели -> видимый текст; первая панель в фокусе.
fn fake_server(id: &str, panes: &[(&str, &str)], label: &str, fail: Option<&str>) -> (HerdrServer, Calls) {
    let workspace = panes[0].0.split(':').next().unwrap().to_string();
    let listing: Vec<Value> = panes
        .iter()
        .enumerate()
        .map(|(i, (pane, _))| {
            json!({"pane_id": pane, "tab_id": format!("{workspace}:t1"), "workspace_id": workspace, "focused": i == 0})
        })
        .collect();
    let texts: HashMap<String, String> = panes.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect();
    let calls: Calls = Arc::default();
    let (record, label, fail) = (calls.clone(), label.to_string(), fail.map(str::to_string));
    let run: Runner = Box::new(move |args: &[String]| {
        let command: Vec<String> = args[1..].to_vec();
        record.lock().unwrap().push(command.clone());
        let head: Vec<&str> = command.iter().take(2).map(String::as_str).collect();
        if let Some(code) = &fail {
            return completed(json!({"error": {"code": code, "message": "no"}}), 1);
        }
        match head[..] {
            ["api", "snapshot"] => completed(json!({"result": {"snapshot": {"workspaces": [], "tabs": [], "layouts": []}}}), 0),
            ["pane", "list"] => completed(json!({"result": {"panes": listing}}), 0),
            ["pane", "layout"] => completed(json!({"result": {"layout": {
                "tab_id": format!("{workspace}:t1"),
                "workspace_id": workspace,
                "focused_pane_id": listing[0]["pane_id"],
                "panes": listing.iter().enumerate().map(|(i, pane)| json!({
                    "pane_id": pane["pane_id"],
                    "focused": i == 0,
                    "rect": {"x": i as i64 * 90, "y": 0, "width": 90, "height": 50},
                })).collect::<Vec<_>>(),
            }}}), 0),
            ["pane", "read"] => {
                assert_eq!(command[3..], ["--source", "visible"]);
                Ok(Output { code: 0, stdout: texts[&command[2]].clone(), stderr: String::new() })
            }
            ["workspace", "list"] => {
                completed(json!({"result": {"workspaces": [{"workspace_id": workspace, "label": label}]}}), 0)
            }
            ["agent", "focus"] => completed(json!({"error": {"code": "agent_not_found", "message": "no"}}), 1),
            _ => completed(json!({"result": {}}), 0),
        }
    });
    (HerdrServer::new(id, vec!["herdr".into()], run, true), calls)
}

#[derive(Default)]
struct FakeTerminal {
    text: String,
    shown: Mutex<Vec<(i64, Option<String>)>>,
}

impl Terminal for Arc<FakeTerminal> {
    fn active_tab(&self) -> Result<TerminalTab> {
        Ok(TerminalTab { window: 77, tab: Some("tab-1".into()), text: self.text.clone(), chrome: None })
    }
    fn read_selected(&self, _window: i64) -> Result<TerminalTab> {
        unimplemented!()
    }
    fn list_tabs(&self) -> Result<Vec<TabRef>> {
        unimplemented!()
    }
    fn select_tab(&self, _window: i64, _tab: Option<&str>) -> Result<()> {
        unimplemented!()
    }
    fn show_tab(&self, window: i64, tab: Option<&str>) -> Result<()> {
        self.shown.lock().unwrap().push((window, tab.map(str::to_string)));
        Ok(())
    }
    fn selected_tab(&self, _window: i64) -> Result<Option<String>> {
        Ok(None)
    }
    fn window_tabs(&self, _window: i64) -> Result<Option<Vec<String>>> {
        Ok(Some(vec![]))
    }
    fn is_terminal(&self, window: i64) -> bool {
        window == 77
    }
}

const WINDOWS_TEXT: &str = "первая строка windows\nвторая строка windows\nтретья строка windows";
const WSL_TEXT: &str = "первая строка из wsl\nвторая строка из wsl\nтретья строка из wsl";

fn two_servers(screen: &str, wsl_fail: Option<&str>) -> (HerdrSessions, Arc<FakeTerminal>) {
    let (windows, _) =
        fake_server("windows", &[("w1:p1", "почти пустая панель в фокусе"), ("w1:p2", WINDOWS_TEXT)], "win-label", None);
    let (wsl, _) = fake_server("wsl", &[("w2:p1", WSL_TEXT)], "wsl-label", wsl_fail);
    let terminal = Arc::new(FakeTerminal { text: screen.into(), ..Default::default() });
    (HerdrSessions::new(vec![windows, wsl], Box::new(terminal.clone())), terminal)
}

fn candidate(server: &str, pane: &str, text: &str, focused: bool) -> Candidate {
    Candidate { server_id: server.into(), pane: json!({"pane_id": pane, "focused": focused}), lines: significant_lines(text) }
}

#[test]
fn significant_lines_ignores_short_and_keeps_line_starts() {
    let lines = significant_lines("короткая\n  длинная строка для сравнения с экраном ");
    assert_eq!(lines, HashSet::from(["длинная строка для с".to_string()]));
}

#[test]
fn visible_pane_rules() {
    let narrow = "первая строка windows│\nвторая строка windows│";
    let long = "первая строка windows и много текста\nвторая строка windows и много текста";
    let candidates = [candidate("a", "p1", long, false)];
    assert_eq!(find_visible_pane(narrow, &candidates).unwrap().pane["pane_id"], "p1");

    // Случай с реальных данных: 4 строки из 29 совпали с чужой вкладкой.
    let lines: Vec<String> = (0..29).map(|i| format!("{i:02} строка большой панели")).collect();
    let big = [candidate("wsl", "p1", &lines.join("\n"), false)];
    assert!(find_visible_pane(&lines[..4].join("\n"), &big).is_none());

    let near_empty = [candidate("a", "p1", "одна строка на всю панель", false)];
    assert!(find_visible_pane("одна строка на всю панель", &near_empty).is_none());

    let two = [candidate("a", "p1", WINDOWS_TEXT, false), candidate("b", "p1", WINDOWS_TEXT, false)];
    assert!(find_visible_pane(WINDOWS_TEXT, &two).is_none());

    let other = "другая панель строка раз\nдругая панель строка два\nдругая панель строка три";
    let split = [
        candidate("a", "bigger", &format!("{other}\nдругая панель строка ещё"), false),
        candidate("a", "focused", WINDOWS_TEXT, true),
    ];
    let screen = format!("{WINDOWS_TEXT}\n{other}");
    assert_eq!(find_visible_pane(&screen, &split).unwrap().pane["pane_id"], "focused");
}

#[test]
fn current_finds_pane_by_text_on_either_server() {
    // Несколько окон одного herdr показывают разное: фокус сервера не указывает на экран.
    let session = two_servers(WINDOWS_TEXT, None).0.current().unwrap();
    assert_eq!((&session["server"], &session["id"], &session["name"]), (&json!("windows"), &json!("w1:p2"), &json!("win-label")));
    assert_eq!((&session["terminal_window"], &session["terminal_tab"]), (&json!(77), &json!("tab-1")));
    assert_eq!(two_servers(WSL_TEXT, None).0.current().unwrap()["server"], "wsl");
    let error = two_servers("обычный терминал без herdr на экране", None).0.current().unwrap_err();
    assert!(error.to_string().contains("не удалось уверенно"));
    // Остановленный сервер не мешает другому.
    assert_eq!(two_servers(WINDOWS_TEXT, Some("server_down")).0.current().unwrap()["server"], "windows");
}

const BG: i64 = 789516;
const SELECTED_ROW: i64 = 3022366;
const ACTIVE_TAB: i64 = 16430217;
const IDLE_TAB: i64 = 4469297;

/// Активная в тестах рабочая область — w2.
fn snapshot(workspaces: &[&str], tabs: &[&str]) -> Value {
    json!({
        "workspaces": workspaces.iter().enumerate()
            .map(|(i, label)| json!({"workspace_id": format!("w{}", i + 1), "label": label, "number": i + 1}))
            .collect::<Vec<_>>(),
        "tabs": tabs.iter().enumerate()
            .map(|(i, label)| json!({"tab_id": format!("w2:t{}", i + 1), "workspace_id": "w2", "label": label}))
            .collect::<Vec<_>>(),
        "layouts": (1..=tabs.len())
            .map(|i| json!({"tab_id": format!("w2:t{i}"), "focused_pane_id": format!("w2:p{i}")}))
            .collect::<Vec<_>>(),
    })
}

fn chrome(rows: &[(&str, i64)], tabs: &[(&str, i64)], collapsed: bool) -> Chrome {
    let entries = |items: &[(&str, i64)]| {
        items.iter().map(|(label, bg)| ChromeEntry { label: label.to_string(), background: Some(*bg) }).collect()
    };
    Chrome { collapsed, sidebar_background: Some(BG), workspaces: entries(rows), tabs: entries(tabs) }
}

fn servers() -> Vec<(String, Value)> {
    vec![
        ("windows".into(), snapshot(&["lawyer", "agent_control"], &["1", "2"])),
        ("wsl".into(), snapshot(&["lawyer", "UVL", "agent_task"], &["cmt", "dev"])),
    ]
}

const WINDOWS_ROWS: &[(&str, i64)] = &[("lawyer", BG), ("agent_control", SELECTED_ROW)];

fn wsl_screen() -> Chrome {
    chrome(&[("lawyer", BG), ("UVL", SELECTED_ROW), ("agent_task", BG)], &[("cmt", ACTIVE_TAB), ("dev", IDLE_TAB)], false)
}

fn windows_screen() -> Chrome {
    chrome(WINDOWS_ROWS, &[("1", ACTIVE_TAB), ("2", IDLE_TAB)], false)
}

#[test]
fn chrome_identifies_pane() {
    let screen = chrome(WINDOWS_ROWS, &[("1", IDLE_TAB), ("2", ACTIVE_TAB)], false);
    assert_eq!(
        find_pane_by_chrome(Some(&screen), &servers()),
        Some(("windows".into(), json!({"pane_id": "w2:p2", "tab_id": "w2:t2", "workspace_id": "w2"})))
    );
    // Сервер различается, хотя одна область называется одинаково.
    assert_eq!(find_pane_by_chrome(Some(&wsl_screen()), &servers()).unwrap().0, "wsl");
    // Свёрнутая боковая панель — по номерам областей.
    let collapsed = chrome(&[("1", BG), ("2", SELECTED_ROW), ("3", BG)], &[("cmt", IDLE_TAB), ("dev", ACTIVE_TAB)], true);
    let (server, pane) = find_pane_by_chrome(Some(&collapsed), &servers()).unwrap();
    assert_eq!((server.as_str(), &pane["pane_id"]), ("wsl", &json!("w2:p2")));
    // Одинаковые номера — различаются вкладками.
    let same_numbers = vec![
        ("windows".to_string(), snapshot(&["a", "b"], &["1", "2"])),
        ("wsl".to_string(), snapshot(&["c", "d"], &["cmt", "dev"])),
    ];
    let screen = chrome(&[("1", BG), ("2", SELECTED_ROW)], &[("1", ACTIVE_TAB), ("2", IDLE_TAB)], true);
    assert_eq!(find_pane_by_chrome(Some(&screen), &same_numbers).unwrap().0, "windows");
}

#[test]
fn chrome_refuses_to_guess() {
    let screens = [
        chrome(&[("чужая", BG), ("область", SELECTED_ROW)], &[("1", ACTIVE_TAB), ("2", IDLE_TAB)], false),
        chrome(&[("lawyer", BG), ("agent_control", BG)], &[("1", ACTIVE_TAB), ("2", IDLE_TAB)], false),
        chrome(WINDOWS_ROWS, &[("1", IDLE_TAB), ("2", IDLE_TAB)], false),
        chrome(WINDOWS_ROWS, &[("1", ACTIVE_TAB), ("9", IDLE_TAB)], false),
        chrome(WINDOWS_ROWS, &[("1", ACTIVE_TAB)], false),
    ];
    assert!(find_pane_by_chrome(None, &servers()).is_none());
    for screen in &screens {
        assert!(find_pane_by_chrome(Some(screen), &servers()).is_none(), "{screen:?}");
    }
    let same = vec![("a".to_string(), snapshot(&["x", "y"], &["1", "2"])), ("b".to_string(), snapshot(&["x", "y"], &["1", "2"]))];
    let screen = chrome(&[("x", BG), ("y", SELECTED_ROW)], &[("1", ACTIVE_TAB), ("2", IDLE_TAB)], false);
    assert!(find_pane_by_chrome(Some(&screen), &same).is_none());
}

fn moved_session() -> Session {
    obj(json!({
        "server": "wsl", "id": "w2:p1", "tab": "w2:t1", "workspace": "w2", "name": "UVL",
        "terminal_window": 1, "terminal_tab": "старый адрес",
    }))
}

fn with_terminal(mut session: Session) -> Session {
    session.insert("terminal_window".into(), json!(77));
    session.insert("terminal_tab".into(), json!("tab-1"));
    session
}

fn calls(calls: &Calls) -> Vec<Vec<String>> {
    calls.lock().unwrap().clone()
}

fn strings(items: &[&[&str]]) -> Vec<Vec<String>> {
    items.iter().map(|c| c.iter().map(|s| s.to_string()).collect()).collect()
}

#[test]
fn open_uses_remembered_server_and_tab() {
    let (windows, windows_calls) = fake_server("windows", &[("w1:p1", WINDOWS_TEXT)], "demo", None);
    let (wsl, wsl_calls) = fake_server("wsl", &[("w2:p1", WSL_TEXT)], "demo", None);
    let terminal = Arc::new(FakeTerminal::default());
    let mut session = with_terminal(moved_session());
    HerdrSessions::new(vec![windows, wsl], Box::new(terminal.clone())).open(&mut session).unwrap();
    assert!(calls(&windows_calls).is_empty());
    assert_eq!(
        calls(&wsl_calls),
        strings(&[
            &["agent", "focus", "w2:p1"],
            &["pane", "list"],
            &["workspace", "focus", "w2"],
            &["tab", "focus", "w2:t1"],
            &["pane", "layout", "--pane", "w2:p1"],
        ])
    );
    assert_eq!(*terminal.shown.lock().unwrap(), [(77, Some("tab-1".to_string()))]);
}

#[test]
fn open_with_agent_is_one_herdr_call() {
    let record: Calls = Arc::default();
    let sink = record.clone();
    let run: Runner = Box::new(move |args: &[String]| {
        sink.lock().unwrap().push(args[1..].to_vec());
        completed(json!({"result": {}}), 0)
    });
    let server = HerdrServer::new("wsl", vec!["herdr".into()], run, true);
    let mut session = with_terminal(moved_session());
    HerdrSessions::new(vec![server], Box::new(Arc::new(FakeTerminal::default()))).open(&mut session).unwrap();
    assert_eq!(calls(&record), strings(&[&["agent", "focus", "w2:p1"]]));
}

#[test]
fn server_without_agents_skips_agent_focus() {
    let (mut windows, record) = fake_server("windows", &[("w1:p1", WINDOWS_TEXT)], "demo", None);
    windows.detects_agents = false;
    let mut session = with_terminal(moved_session());
    session.insert("server".into(), json!("windows"));
    session.insert("id".into(), json!("w1:p1"));
    HerdrSessions::new(vec![windows], Box::new(Arc::new(FakeTerminal::default()))).open(&mut session).unwrap();
    assert_eq!(
        calls(&record),
        strings(&[
            &["pane", "list"],
            &["workspace", "focus", "w1"],
            &["tab", "focus", "w1:t1"],
            &["pane", "layout", "--pane", "w1:p1"],
        ])
    );
}

#[test]
fn focus_path_finds_route_between_neighboring_panes() {
    let layout = json!({"panes": [
        {"pane_id": "w1:p1", "rect": {"x": 0, "y": 0, "width": 60, "height": 50}},
        {"pane_id": "w1:p2", "rect": {"x": 60, "y": 0, "width": 60, "height": 50}},
        {"pane_id": "w1:p3", "rect": {"x": 120, "y": 0, "width": 60, "height": 50}},
    ]});
    let rects = pane_rects(&layout);
    assert_eq!(focus_path(&rects, "w1:p1", "w1:p3"), Some(vec!["right", "right"]));
    assert_eq!(focus_path(&rects, "w1:p3", "w1:p1"), Some(vec!["left", "left"]));
    assert_eq!(focus_path(&rects, "w1:p1", "w1:p1"), Some(vec![]));
    assert!(focus_path(&rects, "w1:p1", "w9:p9").is_none());
}

#[test]
fn open_moves_focus_to_remembered_pane_inside_tab_without_agents() {
    let focused_layout = json!({
        "tab_id": "w1:t1", "workspace_id": "w1", "focused_pane_id": "w1:p1",
        "panes": [
            {"pane_id": "w1:p1", "focused": true, "rect": {"x": 0, "y": 0, "width": 90, "height": 50}},
            {"pane_id": "w1:p2", "focused": false, "rect": {"x": 90, "y": 0, "width": 90, "height": 50}},
        ]
    });
    let record: Calls = Arc::default();
    let sink = record.clone();
    let run: Runner = Box::new(move |args: &[String]| {
        let command = &args[1..];
        sink.lock().unwrap().push(command.to_vec());
        let head: Vec<&str> = command.iter().take(2).map(String::as_str).collect();
        match head[..] {
            ["pane", "list"] => completed(json!({"result": {"panes": [
                {"pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "focused": false},
                {"pane_id": "w1:p2", "tab_id": "w1:t1", "workspace_id": "w1", "focused": true},
            ]}}), 0),
            ["pane", "layout"] => completed(json!({"result": {"layout": {
                "tab_id": "w1:t1", "workspace_id": "w1", "focused_pane_id": "w1:p2",
                "panes": [
                    {"pane_id": "w1:p1", "focused": false, "rect": {"x": 0, "y": 0, "width": 90, "height": 50}},
                    {"pane_id": "w1:p2", "focused": true, "rect": {"x": 90, "y": 0, "width": 90, "height": 50}},
                ]
            }}}), 0),
            ["pane", "focus"] => {
                assert_eq!(command[2..], ["--direction", "left"]);
                completed(json!({"result": {"focus": {
                    "changed": true, "source_pane_id": "w1:p2", "focused_pane_id": "w1:p1",
                    "layout": focused_layout,
                }}}), 0)
            }
            _ => completed(json!({"result": {}}), 0),
        }
    });
    // herdr в PowerShell не распознаёт агентов: панель выбирается только вкладкой, а её
    // фокус остаётся на соседней панели, пока не проведём его по соседям.
    let server = HerdrServer::new("windows", vec!["herdr".into()], run, false);
    let mut session = obj(json!({
        "server": "windows", "id": "w1:p1", "tab": "w1:t1", "workspace": "w1", "name": "product",
        "terminal_window": 77, "terminal_tab": "tab-1",
    }));
    HerdrSessions::new(vec![server], Box::new(Arc::new(FakeTerminal::default()))).open(&mut session).unwrap();
    assert_eq!(
        calls(&record),
        strings(&[
            &["pane", "list"],
            &["workspace", "focus", "w1"],
            &["tab", "focus", "w1:t1"],
            &["pane", "layout", "--pane", "w1:p1"],
            &["pane", "focus", "--direction", "left"],
        ])
    );
}

#[test]
fn open_reports_deleted_pane_as_lost_and_old_records_as_lost() {
    let (wsl, record) = fake_server("wsl", &[("w2:p2", WSL_TEXT)], "demo", None);
    let terminal = Arc::new(FakeTerminal::default());
    let sessions = HerdrSessions::new(vec![wsl], Box::new(terminal.clone()));
    let error = sessions.open(&mut with_terminal(moved_session())).unwrap_err();
    assert!(matches!(&error, SessionError::Lost(text) if text.contains("w2:p1")));
    assert_eq!(calls(&record), strings(&[&["agent", "focus", "w2:p1"], &["pane", "list"]]));
    // Вкладка сервера показывается одновременно с поиском панели: там и выбирать новую сессию.
    assert_eq!(*terminal.shown.lock().unwrap(), [(77, Some("tab-1".to_string()))]);

    let old = obj(json!({"id": "w2:p1", "tab": "w2:t1", "workspace": "w2", "name": "x"}));
    let error = sessions.open(&mut old.clone()).unwrap_err();
    assert!(matches!(&error, SessionError::Lost(text) if text.contains("устарела")));
}

#[test]
fn open_with_stopped_server_is_temporary_error() {
    let (wsl, _) = fake_server("wsl", &[("w2:p1", WSL_TEXT)], "demo", Some("server_unavailable"));
    let sessions = HerdrSessions::new(vec![wsl], Box::new(Arc::new(FakeTerminal::default())));
    let error = sessions.open(&mut with_terminal(moved_session())).unwrap_err();
    assert!(matches!(error, SessionError::Other(_)));
}

#[test]
fn open_refreshes_agent_and_role_of_session() {
    let run: Runner = Box::new(|args: &[String]| {
        if args[1..3] == ["pane", "list"] {
            let pane = json!({"pane_id": "w2:p1", "agent": "claude", "agent_status": "working",
                              "cwd": "/mnt/c/projects_wiki/UavVisionLab/06 Roles/dev"});
            return completed(json!({"result": {"panes": [pane]}}), 0);
        }
        completed(json!({"result": {}}), 0)
    });
    let server = HerdrServer::new("wsl", vec!["herdr".into()], run, false); // путь через pane list
    let terminal = Arc::new(FakeTerminal::default());
    let mut session = with_terminal(moved_session());
    HerdrSessions::new(vec![server], Box::new(terminal.clone())).open(&mut session).unwrap();
    assert_eq!((&session["title"], &session["agent_mark"], &session["ready"]), (&json!("UavVisionLab|dev"), &json!("… claude"), &json!(false)));
    assert_eq!(session["terminal_tab"], "tab-1");
}

/// Терминал, где старого адреса вкладки больше нет.
struct MovedTabTerminal {
    screens: BTreeMap<(i64, Option<String>), Option<Chrome>>,
    selected: Mutex<HashMap<i64, Option<String>>>,
    shown: Mutex<Vec<(i64, Option<String>)>>,
    selections: Mutex<Vec<(i64, Option<String>)>>,
}

impl Terminal for Arc<MovedTabTerminal> {
    fn active_tab(&self) -> Result<TerminalTab> {
        self.read_selected(5)
    }
    fn read_selected(&self, window: i64) -> Result<TerminalTab> {
        let tab = self.selected.lock().unwrap()[&window].clone();
        let chrome = self.screens[&(window, tab.clone())].clone();
        Ok(TerminalTab { window, tab, text: String::new(), chrome })
    }
    fn list_tabs(&self) -> Result<Vec<TabRef>> {
        let selected = self.selected.lock().unwrap();
        Ok(self
            .screens
            .iter()
            .map(|((w, t), chrome)| TabRef {
                window: *w,
                tab: t.clone(),
                selected: selected[w] == *t,
                title: if chrome.is_some() { "herdr" } else { "Windows PowerShell" }.into(),
            })
            .collect())
    }
    fn select_tab(&self, window: i64, tab: Option<&str>) -> Result<()> {
        self.selections.lock().unwrap().push((window, tab.map(str::to_string)));
        self.selected.lock().unwrap().insert(window, tab.map(str::to_string));
        Ok(())
    }
    fn show_tab(&self, window: i64, tab: Option<&str>) -> Result<()> {
        let key = (window, tab.map(str::to_string));
        if !self.screens.contains_key(&key) {
            return Err(SessionError::TabNotFound("нет".into()));
        }
        self.shown.lock().unwrap().push(key.clone());
        self.selected.lock().unwrap().insert(window, key.1);
        Ok(())
    }
    fn selected_tab(&self, window: i64) -> Result<Option<String>> {
        Ok(self.selected.lock().unwrap()[&window].clone())
    }
    /// Окна, которого нет среди вкладок, больше нет.
    fn window_tabs(&self, window: i64) -> Result<Option<Vec<String>>> {
        let tabs: Vec<String> = self.screens.keys().filter(|(w, _)| *w == window).filter_map(|(_, t)| t.clone()).collect();
        Ok(self.screens.keys().any(|(w, _)| *w == window).then_some(tabs))
    }
    fn is_terminal(&self, window: i64) -> bool {
        window == 77
    }
}

fn snapshot_server(id: &str, snap: Value) -> HerdrServer {
    let run: Runner = Box::new(move |args: &[String]| match (args[1].as_str(), args[2].as_str()) {
        ("api", "snapshot") => completed(json!({"result": {"snapshot": snap}}), 0),
        ("workspace", "list") => completed(json!({"result": {"workspaces": snap["workspaces"]}}), 0),
        ("pane", "list") => {
            let panes: Vec<Value> =
                snap["layouts"].as_array().unwrap().iter().map(|l| json!({"pane_id": l["focused_pane_id"]})).collect();
            completed(json!({"result": {"panes": panes}}), 0)
        }
        _ => completed(json!({"result": {}}), 0),
    });
    HerdrServer::new(id, vec!["herdr".into()], run, true)
}

type Screens<'a> = &'a [((i64, Option<&'a str>), Option<Chrome>)];

fn make_moved(screens: Screens, selected: &[(i64, Option<&str>)]) -> (HerdrSessions, Arc<MovedTabTerminal>) {
    let terminal = Arc::new(MovedTabTerminal {
        screens: screens.iter().map(|((w, t), c)| ((*w, t.map(str::to_string)), c.clone())).collect(),
        selected: Mutex::new(selected.iter().map(|(w, t)| (*w, t.map(str::to_string))).collect()),
        shown: Mutex::default(),
        selections: Mutex::default(),
    });
    let servers = servers().into_iter().map(|(id, snap)| snapshot_server(&id, snap)).collect();
    (HerdrSessions::new(servers, Box::new(terminal.clone())), terminal)
}

fn tab(window: i64, tab: Option<&str>) -> (i64, Option<String>) {
    (window, tab.map(str::to_string))
}

#[test]
fn open_finds_tab_moved_to_own_window_without_touching_other_tabs() {
    let (sessions, terminal) = make_moved(
        &[((1, Some("a")), Some(windows_screen())), ((1, Some("b")), None), ((2, Some("new")), Some(wsl_screen()))],
        &[(1, Some("a")), (2, Some("new"))],
    );
    let mut session = moved_session();
    sessions.open(&mut session).unwrap();
    assert_eq!(*terminal.shown.lock().unwrap(), [tab(2, Some("new"))]);
    assert!(terminal.selections.lock().unwrap().is_empty());
    assert_eq!((&session["terminal_window"], &session["terminal_tab"]), (&json!(2), &json!("new")));
}

#[test]
fn open_finds_window_merged_back_as_unselected_tab() {
    let (sessions, terminal) = make_moved(
        &[((1, Some("plain")), None), ((1, Some("merged")), Some(wsl_screen())), ((3, Some("other")), None)],
        &[(1, Some("plain")), (3, Some("other"))],
    );
    let mut session = moved_session();
    sessions.open(&mut session).unwrap();
    assert_eq!(*terminal.shown.lock().unwrap(), [tab(1, Some("merged"))]);
    assert_eq!(session["terminal_tab"], "merged");
}

#[test]
fn failed_search_restores_selected_tabs_and_reports_error() {
    let (sessions, terminal) = make_moved(
        &[((1, Some("a")), Some(windows_screen())), ((1, Some("b")), None), ((2, Some("c")), None), ((2, Some("d")), None)],
        &[(1, Some("a")), (2, Some("c"))],
    );
    let error = sessions.open(&mut moved_session()).unwrap_err().to_string();
    assert!(error.contains("Не найдена вкладка") && !error.contains("полноэкранн"));
    let selected = terminal.selected.lock().unwrap().clone();
    assert_eq!((selected[&1].as_deref(), selected[&2].as_deref()), (Some("a"), Some("c")));
    assert!(terminal.shown.lock().unwrap().is_empty());
}

#[test]
fn search_restores_tabs_it_tried_in_other_windows() {
    let (sessions, terminal) = make_moved(
        &[((1, Some("a")), None), ((1, Some("b")), None), ((2, Some("c")), None), ((2, Some("d")), Some(wsl_screen()))],
        &[(1, Some("a")), (2, Some("c"))],
    );
    sessions.open(&mut moved_session()).unwrap();
    let selected = terminal.selected.lock().unwrap().clone();
    assert_eq!((selected[&1].as_deref(), selected[&2].as_deref()), (Some("a"), Some("d")));
}

/// После перезагрузки: панель в herdr жива, а окна терминала с запомненным номером нет.
#[test]
fn probe_finds_selected_herdr_tab_after_reboot_without_switching() {
    let (sessions, terminal) = make_moved(
        &[((2, Some("ps")), None), ((2, Some("new")), Some(wsl_screen()))],
        &[(2, Some("new"))],
    );
    let probe = &sessions.probe(&[moved_session()])[0];
    assert_eq!(probe.available, Some(true));
    let info = probe.info.as_ref().unwrap();
    assert_eq!((&info["terminal_window"], &info["terminal_tab"]), (&json!(2), &json!("new")));
    assert!(terminal.selections.lock().unwrap().is_empty() && terminal.shown.lock().unwrap().is_empty());
}

#[test]
fn probe_counts_unselected_herdr_tab_as_reachable_by_press() {
    let (sessions, terminal) = make_moved(
        &[((2, Some("ps")), None), ((2, Some("new")), Some(wsl_screen()))],
        &[(2, Some("ps"))],
    );
    let probe = &sessions.probe(&[moved_session()])[0];
    assert_eq!(probe.available, Some(true));
    assert!(!probe.info.as_ref().unwrap().contains_key("terminal_window")); // адрес не проверен
    assert!(terminal.selections.lock().unwrap().is_empty());
}

#[test]
fn probe_without_herdr_tab_is_unavailable() {
    let (sessions, _) = make_moved(&[((2, Some("ps")), None)], &[(2, Some("ps"))]);
    assert_eq!(sessions.probe(&[moved_session()])[0].available, Some(false));
    // Вкладка herdr есть, но она известна как вкладка другого сервера.
    let (sessions, _) = make_moved(
        &[((1, Some("a")), None), ((1, Some("b")), Some(windows_screen()))],
        &[(1, Some("a"))],
    );
    let windows = obj(json!({"server": "windows", "id": "w2:p1", "terminal_window": 1, "terminal_tab": "b"}));
    let probes = sessions.probe(&[windows, moved_session()]);
    assert_eq!((probes[0].available, probes[1].available), (Some(true), Some(false)));
}

#[test]
fn full_screen_windows() {
    // Запомненная в полноэкранном окне сессия — без адреса вкладки.
    let (sessions, _) = make_moved(&[((5, None), Some(wsl_screen()))], &[(5, None)]);
    let session = sessions.current().unwrap();
    assert_eq!((&session["server"], &session["terminal_window"], &session["terminal_tab"]), (&json!("wsl"), &json!(5), &Value::Null));
    let mut session = moved_session();
    session.insert("terminal_window".into(), json!(5));
    session.insert("terminal_tab".into(), Value::Null);
    let (sessions, terminal) = make_moved(&[((5, None), Some(wsl_screen()))], &[(5, None)]);
    sessions.open(&mut session).unwrap();
    assert_eq!(*terminal.shown.lock().unwrap(), [tab(5, None)]);

    // Адрес вкладки сохраняется до выхода из полноэкранного режима.
    let (sessions, terminal) = make_moved(&[((1, None), Some(wsl_screen()))], &[(1, None)]);
    let mut session = moved_session();
    sessions.open(&mut session).unwrap();
    assert_eq!(*terminal.shown.lock().unwrap(), [tab(1, None)]);
    assert_eq!(session["terminal_tab"], "старый адрес");

    let (sessions, terminal) = make_moved(&[((1, None), Some(windows_screen()))], &[(1, None)]);
    let error = sessions.open(&mut moved_session()).unwrap_err().to_string();
    assert!(error.contains("выйдите из полноэкранного режима"));
    assert!(terminal.shown.lock().unwrap().is_empty());
}

#[test]
fn project_and_role_from_session_folder() {
    let cases = [
        (Some("/mnt/c/projects_wiki/UavVisionLab/06 Roles/dev"), (Some("UavVisionLab"), Some("dev"))),
        (Some("/mnt/c/projects_wiki/UavVisionLab/06 Roles/dev/notes"), (Some("UavVisionLab"), Some("dev"))),
        (Some("/mnt/c/projects_wiki/UavVisionLab"), (Some("UavVisionLab"), Some("core"))),
        (Some(r"C:\projects_wiki\tutor\06 Roles\product"), (Some("tutor"), Some("product"))),
        (Some(r"C:\Projects\exp-26\agent_control"), (Some("agent_control"), Some("core"))),
        (Some("/mnt/c/projects_wiki/UavVisionLab/06 Roles"), (Some("06 Roles"), Some("core"))),
        (None, (None, None)),
    ];
    for (cwd, (project, role)) in cases {
        let found = project_and_role(cwd);
        assert_eq!((found.0.as_deref(), found.1.as_deref()), (project, role), "{cwd:?}");
    }
}

#[test]
fn agent_readiness() {
    let cases = [
        (Some("claude"), "idle", true, "✓ claude", "claude готов к промпту"),
        (Some("opencode"), "done", true, "✓ opencode", "готов к промпту"),
        (Some("claude"), "working", false, "… claude", "claude занят"),
        (Some("claude"), "blocked", false, "⚠ claude", "ждёт подтверждения"),
        (None, "unknown", false, "нет агента", "не определён"),
    ];
    for (agent, status, ready, mark, text) in cases {
        let info = describe_pane(&json!({"pane_id": "w1:p1", "agent": agent, "agent_status": status}), "нет агента");
        assert_eq!((&info["ready"], &info["agent_mark"]), (&json!(ready), &json!(mark)));
        assert!(readiness(&info).contains(text));
    }
}

#[test]
fn server_without_agent_detection_marks_agent_unknown() {
    let (server, _) = fake_server("windows", &[("w1:p1", WINDOWS_TEXT)], "demo", None);
    let server = HerdrServer { detects_agents: false, ..server };
    let terminal = Arc::new(FakeTerminal { text: WINDOWS_TEXT.into(), ..Default::default() });
    let session = HerdrSessions::new(vec![server], Box::new(terminal)).current().unwrap();
    assert_eq!(session["agent_mark"], "неизвестен");
}

#[test]
fn same_session_rules() {
    let a = obj(json!({"server": "wsl", "id": "w2:p1", "name": "x"}));
    let b = obj(json!({"kind": "herdr", "server": "wsl", "id": "w2:p1"}));
    let c = obj(json!({"server": "windows", "id": "w2:p1"}));
    assert!(same_session(Some(&a), Some(&b)));
    assert!(!same_session(Some(&a), Some(&c)));
    assert!(!same_session(Some(&a), None));
    let app1 = obj(json!({"kind": "app", "id": "1", "name": "x"}));
    let app2 = obj(json!({"kind": "app", "id": "1", "name": "y"}));
    assert!(same_session(Some(&app1), Some(&app2)));
    assert!(!same_session(Some(&app1), Some(&a)));
}

struct FakeScreen {
    windows: Vec<i64>,
    foreground: i64,
}

impl Screen for FakeScreen {
    fn top_level_windows(&self) -> Vec<i64> {
        self.windows.clone()
    }
    fn foreground_window(&self) -> i64 {
        self.foreground
    }
}

/// Вид «окно приложения» для проверки выбора вида: окна 100+ — приложения.
struct FakeApps;

impl Backend for FakeApps {
    fn matches(&self, window: i64) -> bool {
        window >= 100
    }
    fn current(&self) -> Result<Session> {
        Ok(obj(json!({"kind": "window", "id": 100, "name": "Блокнот"})))
    }
    fn open(&self, _session: &mut Session) -> Result<()> {
        Ok(())
    }
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        Some(sessions.iter().map(|_| Probe { available: Some(true), info: None }).collect())
    }
    fn focused(&self, window: i64, _bound: &[Session]) -> Option<Session> {
        Some(obj(json!({"kind": "window", "id": window})))
    }
}

fn router(windows: Vec<i64>, foreground: i64) -> Router {
    let (wsl, _) = fake_server("wsl", &[("w2:p1", WSL_TEXT)], "demo", None);
    let terminal = Arc::new(FakeTerminal { text: WSL_TEXT.into(), ..Default::default() });
    let herdr: Box<dyn Backend> = Box::new(HerdrSessions::new(vec![wsl], Box::new(terminal)));
    Router::new(vec![("herdr", herdr), ("window", Box::new(FakeApps))], Box::new(FakeScreen { windows, foreground }))
}

#[test]
fn router_takes_current_session_from_last_active_kind() {
    assert_eq!(router(vec![5, 77, 100], 0).current().unwrap()["server"], "wsl");
    let session = router(vec![5, 100, 77], 0).current().unwrap();
    assert_eq!((&session["kind"], &session["name"]), (&json!("window"), &json!("Блокнот")));
    // Ни одного известного окна — herdr, как в Python-версии.
    assert_eq!(router(vec![5], 0).current().unwrap()["kind"], "herdr");
}

#[test]
fn router_routes_open_probe_and_focus_by_kind() {
    let router = router(vec![], 100);
    let mut tab = obj(json!({"kind": "tab", "id": 1, "name": "Почта"}));
    let error = router.open(&mut tab).unwrap_err();
    assert!(matches!(&error, SessionError::Other(text) if text.contains("пока не открывает")));
    let window = obj(json!({"kind": "window", "id": 100, "name": "Блокнот"}));
    let probes = router.probe(&[tab, with_terminal(moved_session()), window.clone()]);
    assert_eq!(probes[0].available, Some(false));
    assert_eq!(probes[1].info.as_ref().unwrap()["agent_mark"], "нет агента");
    assert_eq!(probes[2].available, Some(true));
    assert!(same_session(router.focused(&[window.clone()]).as_ref(), Some(&window)));
    assert!(router.focused(&[with_terminal(moved_session())]).is_none());
}
