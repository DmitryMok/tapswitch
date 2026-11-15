//! Тесты движка — перенос сценарных тестов из `app/tests/test_core.py`.

use super::*;
use crate::scenarios::load_scenarios;
use crate::sessions::{Result as SessionResult, SessionError};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Default)]
struct FakeDevice {
    count: usize,
    labels: Mutex<Vec<(usize, String)>>,
    icons: Mutex<Vec<(String, String)>>,
    active: Mutex<Vec<(usize, bool)>>,
    asked: Mutex<Vec<Option<(usize, String)>>>,
    looks: Mutex<Vec<(usize, KeyLook)>>,
    confirm_states: Mutex<Vec<Confirm>>,
    notices: Mutex<Vec<String>>,
    available: Mutex<Vec<(usize, bool)>>,
}

impl Device for FakeDevice {
    fn key_count(&self) -> usize {
        self.count
    }
    fn set_label(&self, key: usize, label: &str) {
        self.labels.lock().unwrap().push((key, label.into()));
    }
    fn set_icon(&self, _key: usize, icon: &str, title: &str) {
        self.icons.lock().unwrap().push((icon.into(), title.into()));
    }
    fn set_active(&self, key: usize, active: bool) {
        self.active.lock().unwrap().push((key, active));
    }
    fn set_state(&self, key: usize, _status: Option<&str>, available: bool, _bound: bool) {
        self.available.lock().unwrap().push((key, available));
    }
    fn ask_confirm(&self, key: usize, text: &str, state: Confirm) {
        self.asked.lock().unwrap().push(Some((key, text.into())));
        self.confirm_states.lock().unwrap().push(state);
    }
    fn notify(&self, text: &str) {
        self.notices.lock().unwrap().push(text.into());
    }
    fn close_confirm(&self) {
        self.asked.lock().unwrap().push(None);
    }
    fn set_look(&self, key: usize, look: &KeyLook) {
        self.looks.lock().unwrap().push((key, look.clone()));
    }
}

#[derive(Default)]
struct FakeState {
    opened: Vec<String>,
    /// Ошибка определения текущей сессии.
    error: Option<String>,
    /// Ошибка при открытии.
    open_error: Option<SessionError>,
    name: String,
    role: Option<String>,
    /// Текущая сессия — окно приложения с этой иконкой (пустая — программу не узнали).
    window_icon: Option<String>,
}

#[derive(Default)]
struct FakeSessions(Mutex<FakeState>);

impl Sessions for FakeSessions {
    fn current(&self) -> SessionResult<Session> {
        let state = self.0.lock().unwrap();
        if let Some(error) = &state.error {
            return Err(SessionError::Other(error.clone()));
        }
        let mut session = json!({
            "server": "windows", "id": format!("w1:{}", state.name), "tab": "w1:t1", "workspace": "w1",
            "name": state.name, "title": state.name, "agent_mark": "✓ claude",
        });
        if let Some(icon) = &state.window_icon {
            let mut window = json!({"kind": "window", "server": "app.exe", "id": 7, "name": state.name, "title": state.name});
            if !icon.is_empty() {
                window["icon"] = json!(icon);
            }
            return Ok(window.as_object().unwrap().clone());
        }
        if let Some(role) = &state.role {
            session["role"] = json!(role);
            session["project"] = json!("UavVisionLab");
        }
        Ok(session.as_object().unwrap().clone())
    }
    fn open(&self, session: &mut Session) -> SessionResult<()> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = &state.open_error {
            return Err(error.clone());
        }
        state.opened.push(session["name"].as_str().unwrap().to_string());
        Ok(())
    }
    fn probe(&self, sessions: &[Session]) -> Vec<Probe> {
        vec![Probe::default(); sessions.len()]
    }
    fn focused(&self, _bound: &[Session]) -> Option<Session> {
        None
    }
}

struct Run {
    engine: Engine,
    device: Arc<FakeDevice>,
    sessions: Arc<FakeSessions>,
    log: Arc<Mutex<Vec<String>>>,
    path: PathBuf,
    dir: PathBuf,
    start: Instant,
    now: u64,
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn shipped() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../app/scenarios")
}

impl Run {
    fn new(cols: usize) -> Self {
        Self::with_default(cols, None)
    }

    fn with_default(cols: usize, default: Option<&str>) -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("agent_control_engine_{}_{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        let device = Arc::new(FakeDevice { count: cols, ..Default::default() });
        let sessions = Arc::new(FakeSessions::default());
        sessions.0.lock().unwrap().name = "demo".into();
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let (scenarios, _) = load_scenarios(&shipped());
        let engine = Engine::new(
            device.clone(),
            scenarios,
            State::load(&path).unwrap(),
            sessions.clone(),
            Arc::new(move |text: &str| sink.lock().unwrap().push(text.to_string())),
            300,
            500,
            default.map(str::to_string),
        );
        Self { engine, device, sessions, log, path, dir, start: Instant::now(), now: 0 }
    }

    fn at(&self) -> Instant {
        self.start + Duration::from_millis(self.now)
    }
    fn advance(&mut self, ms: u64) {
        self.now += ms;
        let now = self.at();
        self.engine.tick(now);
    }
    fn down(&mut self, key: usize) {
        let now = self.at();
        self.engine.on_raw(key, true, now);
    }
    fn up(&mut self, key: usize) {
        let now = self.at();
        self.engine.on_raw(key, false, now);
    }
    fn press(&mut self, key: usize) {
        self.down(key);
        self.advance(50);
        self.up(key);
    }
    fn click(&mut self, key: usize) {
        self.press(key);
        self.advance(300);
    }
    fn hold(&mut self, key: usize) {
        self.down(key);
        self.advance(500);
        self.up(key);
    }
    fn double_press(&mut self) {
        self.press(0);
        self.advance(50);
        self.press(0);
    }
    fn drag(&mut self, key: usize, target: Option<usize>) {
        self.down(key);
        self.advance(500);
        let now = self.at();
        self.engine.start_drag(key, now);
        self.engine.drop(key, target, now);
    }
    fn label(&self, key: usize) -> String {
        self.device.labels.lock().unwrap().iter().rev().find(|(k, _)| *k == key).unwrap().1.clone()
    }
    fn look(&self, key: usize) -> KeyLook {
        self.device.looks.lock().unwrap().iter().rev().find(|(k, _)| *k == key).unwrap().1.clone()
    }
    /// Выполнить фоновые чтения выделенной сессии, как это делает цикл программы.
    fn read(&mut self) {
        for id in self.engine.take_reads() {
            let result = self.sessions.current().map_err(|e| e.to_string());
            self.engine.on_current(id, result);
        }
    }
    fn last_asked(&self) -> Option<(usize, String)> {
        self.device.asked.lock().unwrap().last().cloned().flatten()
    }
    fn last_confirm_state(&self) -> Confirm {
        *self.device.confirm_states.lock().unwrap().last().unwrap()
    }
    fn last_icon(&self) -> String {
        self.device.icons.lock().unwrap().last().unwrap().0.clone()
    }
    fn last_log(&self) -> String {
        self.log.lock().unwrap().last().cloned().unwrap_or_default()
    }
    fn opened(&self) -> Vec<String> {
        self.sessions.0.lock().unwrap().opened.clone()
    }
    fn set_name(&self, name: &str) {
        self.sessions.0.lock().unwrap().name = name.into();
    }
    fn set_error(&self, error: Option<&str>) {
        self.sessions.0.lock().unwrap().error = error.map(str::to_string);
    }
    fn saved(&self) -> State {
        State::load(&self.path).unwrap()
    }
    fn saved_name(&self, key: usize) -> Option<String> {
        self.saved().values(key).get("session").map(|s| s["name"].as_str().unwrap().to_string())
    }
}

#[test]
fn click_on_empty_key_remembers_then_opens_and_persists() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    assert!(run.label(0).starts_with("Нет сессии"));
    run.click(0);
    assert!(run.opened().is_empty());
    assert!(run.label(0).starts_with("demo"));
    run.click(0);
    assert_eq!(run.opened(), ["demo"]);
    assert_eq!(run.saved().scenario_of(0), Some("auto"));
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
}

#[test]
fn failed_remember_on_empty_key_keeps_it_empty() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.set_error(Some("не удалось определить панель"));
    run.click(0);
    assert!(run.label(0).starts_with("Нет сессии"));
    assert!(run.last_log().contains("не удалось запомнить текущую сессию: не удалось определить панель"));
}

#[test]
fn auto_label_shows_project_and_role_icon() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    assert_eq!(run.device.icons.lock().unwrap().last().unwrap(), &(String::new(), String::new()));
    run.sessions.0.lock().unwrap().role = Some("dev".into());
    run.click(0);
    assert_eq!(run.label(0), "UavVisionLab ✓ claude");
    assert_eq!(run.device.icons.lock().unwrap().last().unwrap(), &("dev".to_string(), "dev".to_string()));
    run.engine.assign(0, None);
    assert_eq!(run.device.icons.lock().unwrap().last().unwrap(), &(String::new(), String::new()));
}

#[test]
fn open_frames_key_at_once_without_waiting_for_focus_poll() {
    let mut run = Run::new(2);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    assert!(run.device.active.lock().unwrap().is_empty());
    run.press(0); // открыть: клик при отпускании
    assert_eq!(run.opened(), ["demo"]);
    assert_eq!(*run.device.active.lock().unwrap(), [(0, true)]);
    assert!(run.engine.take_focus_request());
}

#[test]
fn hold_reads_session_at_start_and_binds_on_release() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.set_name("other");
    run.down(0);
    run.advance(500);
    run.set_name("third"); // к отпусканию выделена другая — запоминается прочитанная в начале
    assert!(run.label(0).starts_with("demo"));
    run.up(0);
    assert!(run.label(0).starts_with("other"));
    assert!(run.opened().is_empty());
}

#[test]
fn drag_keeps_session_and_drop_swaps_keys() {
    let mut run = Run::new(3);
    run.engine.assign(0, Some("auto"));
    run.engine.assign(1, Some("dictation"));
    run.click(0);
    run.set_name("other");
    run.drag(0, Some(1));
    run.advance(1000);
    assert!(run.opened().is_empty());
    let saved = run.saved();
    assert_eq!(saved.scenario_of(1), Some("auto"));
    assert_eq!(run.saved_name(1).as_deref(), Some("demo"));
    assert_eq!(saved.scenario_of(0), Some("dictation"));
    assert!(!saved.values(0).contains_key("session"));
    assert!(run.last_log().contains("поменялись местами"));

    run.drag(1, None); // мимо кнопок — ничего
    run.drag(1, Some(1)); // на себя же — ничего
    assert_eq!(run.saved().scenario_of(1), Some("auto"));
    run.click(1);
    assert_eq!(run.opened(), ["demo"]);
}

#[test]
fn confirmation_needed_to_remember_session() {
    let mut run = Run::new(1);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.click(0); // пустая кнопка: вопрос сразу, сессия читается в фоне
    assert_eq!(*run.device.asked.lock().unwrap(), [Some((0, "Запомнить на кнопке 1 выделенную сессию?".to_string()))]);
    assert_eq!(run.last_confirm_state(), Confirm::Reading);
    run.read();
    assert_eq!(run.last_asked(), Some((0, "Запомнить на кнопке 1 сессию «demo»?".to_string())));
    assert_eq!(run.last_confirm_state(), Confirm::Ready);
    assert_eq!(run.saved_name(0), None);
    run.engine.answer(true);
    assert_eq!(run.device.asked.lock().unwrap().last().unwrap(), &None);
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
    assert!(run.label(0).starts_with("demo"));

    run.set_name("other");
    run.hold(0); // перезапись удержанием — тоже с вопросом
    assert_eq!(
        run.device.asked.lock().unwrap().last().unwrap(),
        &Some((0, "Запомнить на кнопке 1 сессию «other»?".to_string()))
    );
    run.engine.answer(false);
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
    assert!(run.last_log().contains("отменено"));
    run.engine.answer(true); // повторный ответ без вопроса — ничего
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
}

#[test]
fn new_question_replaces_old_and_moving_key_cancels_it() {
    let mut run = Run::new(2);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.engine.assign(1, Some("auto"));
    run.click(0);
    run.set_name("other");
    run.click(1);
    run.read(); // ответ на первый вопрос устарел
    run.engine.answer(true);
    assert_eq!(run.saved_name(0), None);
    assert_eq!(run.saved_name(1).as_deref(), Some("other"));

    run.click(0);
    run.read();
    run.drag(0, Some(1));
    assert_eq!(run.device.asked.lock().unwrap().last().unwrap(), &None);
    let log = run.log.lock().unwrap().clone();
    assert!(log[log.len() - 2].contains("отменено"));
    run.engine.answer(true);
    assert_eq!(run.saved_name(0).as_deref(), Some("other"));
    assert_eq!(run.saved_name(1), None);
}

#[test]
fn double_click_clears_binding() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.set_name("other");
    run.double_press();
    run.advance(1000);
    assert_eq!(run.opened(), ["demo"]); // клик сразу, без ожидания второго нажатия; затем очистка
    assert!(run.label(0).starts_with("Нет сессии"));
    assert!(run.last_log().contains("привязка к сессии «demo» очищена"));
    assert_eq!(run.saved_name(0), None);
    run.click(0); // пустая кнопка снова запоминает выделенную
    assert!(run.label(0).starts_with("other"));
}

#[test]
fn failed_hold_keeps_previous_session() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.set_error(Some("панель не определилась"));
    run.hold(0);
    assert!(run.label(0).starts_with("demo"));
    assert!(run.last_log().contains("панель не определилась"));
}

#[test]
fn lost_session_waits_for_selection_and_next_click_remembers_it() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.sessions.0.lock().unwrap().open_error = Some(SessionError::Lost("панели больше нет".into()));
    run.click(0);
    assert!(run.label(0).starts_with("⚠ Выделите сессию и нажмите"));
    assert!(run.last_log().contains("выделите нужную сессию и нажмите кнопку ещё раз"));
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&run.path).unwrap()).unwrap();
    assert!(saved["keys"]["0"]["values"].get("_waiting").is_none());

    run.set_error(Some("панель не определилась"));
    run.click(0);
    assert!(run.label(0).starts_with('⚠'));
    assert!(run.last_log().contains("панель не определилась") && run.last_log().contains("кнопка ждёт"));

    run.set_error(None);
    run.set_name("new");
    run.click(0);
    assert!(run.label(0).starts_with("new"));
    assert!(run.opened().is_empty());
}

#[test]
fn temporary_error_keeps_session_and_does_not_wait() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.sessions.0.lock().unwrap().open_error = Some(SessionError::Other("herdr (windows) не запустился".into()));
    run.click(0);
    assert!(run.label(0).starts_with("demo"));
    assert!(run.last_log().contains("не запустился"));
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
}

#[test]
fn recording_mark_is_shown_during_hold_and_never_saved() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("herdr_session"));
    run.down(0);
    run.advance(500);
    assert_eq!(run.label(0), "● Нет сессии");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&run.path).unwrap()).unwrap();
    assert!(saved["keys"]["0"]["values"].get("_recording").is_none());
    run.up(0);
    assert_eq!(run.label(0), "Нет сессии");
}

#[test]
fn remember_scenario_binds_on_click_and_opens_on_double_click() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("remember_session"));
    assert_eq!(run.label(0), "Запомнить");
    run.click(0);
    assert_eq!(run.label(0), "demo");
    assert!(run.opened().is_empty());
    run.double_press();
    assert_eq!(run.opened(), ["demo"]);
    run.engine.assign(0, Some("journal"));
    assert_eq!(run.label(0), "Журнал");
    run.engine.assign(0, Some("herdr_session"));
    assert_eq!(run.label(0), "demo");
}

#[test]
fn dictation_is_not_supported_and_does_not_break() {
    let mut run = Run::new(1);
    run.engine.assign(0, Some("dictation"));
    assert_eq!(run.label(0), "Диктовка");
    run.hold(0);
    assert_eq!(run.label(0), "Диктовка");
    assert!(run.log.lock().unwrap().iter().any(|l| l.contains("только в Python-версии")));
    run.press(0);
    assert!(run.last_log().contains("держите"));
}

#[test]
fn key_without_own_scenario_uses_default() {
    let mut run = Run::with_default(2, Some("auto"));
    run.engine.render_all();
    assert!(run.label(1).starts_with("Нет сессии"));
    run.click(1);
    assert!(run.label(1).starts_with("demo"));
    run.engine.assign(1, Some("journal"));
    assert_eq!(run.label(1), "Журнал");
}

#[test]
fn probes_refresh_agent_and_skip_rebound_keys() {
    let mut run = Run::new(2);
    run.engine.assign(0, Some("auto"));
    run.click(0);
    let bound = run.engine.bound_keys();
    assert_eq!(bound.len(), 1);
    let info = json!({"agent": "claude", "agent_status": "working", "agent_mark": "… claude"});
    let probe = Probe { available: Some(true), info: Some(info.as_object().unwrap().clone()) };
    run.engine.show_probes(vec![(0, bound[0].1.clone(), probe.clone())]);
    assert_eq!(run.label(0), "demo … claude");
    assert_eq!(run.saved().values(0)["session"]["agent_status"], "working");

    let stale = json!({"server": "windows", "id": "w1:old"}).as_object().unwrap().clone();
    let info = json!({"agent_mark": "✓ claude"});
    run.engine.show_probes(vec![(0, stale, Probe { available: Some(false), info: Some(info.as_object().unwrap().clone()) })]);
    assert_eq!(run.label(0), "demo … claude");
}

#[test]
fn key_combination_button_presses_combo_and_clears_back_to_default() {
    let mut run = Run::with_default(2, Some("auto"));
    run.engine.assign(0, Some("auto"));
    run.click(0); // кнопка запомнила сессию — сочетание её заменит
    assert!(run.engine.set_keys(0, Some("Ctrl+Pause")).is_err());
    run.engine.set_keys(0, Some("Win+H")).unwrap();
    assert_eq!(run.label(0), "Win+H");
    assert_eq!(run.saved().scenario_of(0), Some("keys"));
    assert_eq!(run.saved_name(0), None);
    assert_eq!(run.engine.overview()["keys"][0]["keys"], "Win+H");
    assert_eq!(run.engine.overview()["keys"][0]["bound"], true);
    run.press(0);
    assert_eq!(run.last_log(), "Кнопка 1: нажато Win+H"); // сразу, без ожидания второго нажатия
    assert!(run.opened().is_empty());
    // Сочетание переезжает вместе с кнопкой.
    run.drag(0, Some(1));
    assert_eq!(run.label(1), "Win+H");
    // «Очистить» — снова обычная кнопка со сценарием по умолчанию.
    run.engine.clear(1);
    assert_eq!(run.saved().scenario_of(1), None);
    assert!(run.label(1).starts_with("Нет сессии"));
    run.engine.set_keys(1, Some("F13")).unwrap();
    run.engine.set_keys(1, None).unwrap();
    assert_eq!(run.saved().scenario_of(1), None);
}

#[test]
fn profiles_switch_whole_set_of_buttons() {
    let mut run = Run::with_default(2, Some("auto"));
    run.click(0); // профиль 1: кнопка 1 запомнила demo
    assert!(run.label(0).starts_with("demo"));
    // Профили выключены (по умолчанию): переключения нет, миниатюр нет.
    run.engine.switch_profile(1);
    assert_eq!(run.saved().profile(), 0);
    assert!(run.engine.overview()["profiles"].is_null());
    assert!(run.engine.set_setting("profiles_enabled", true));
    run.down(1); // незаконченный жест прежнего профиля
    run.engine.switch_profile(1);
    run.up(1);
    run.advance(1000);
    assert!(run.label(0).starts_with("Нет сессии"));
    assert_eq!(run.saved().profile(), 1);
    let overview = run.engine.overview();
    assert_eq!(overview["profile"], 1);
    assert_eq!(overview["profiles"].as_array().unwrap().len(), 6);
    assert!(overview["profiles"][0][0]["label"].as_str().unwrap().starts_with("demo"));
    assert_eq!(overview["profiles"][0][0]["bound"], true);
    assert_eq!(overview["profiles"][1][0]["bound"], false);
    run.set_name("other");
    run.click(0); // профиль 2: своя сессия на той же кнопке
    assert!(run.label(0).starts_with("other"));
    run.engine.switch_profile(0);
    assert!(run.label(0).starts_with("demo"));
    run.engine.switch_profile(1);
    assert!(run.label(0).starts_with("other"));
    // Выключили — остаётся текущий набор, остальные хранятся до включения.
    run.engine.set_setting("profiles_enabled", false);
    assert!(run.label(0).starts_with("other"));
    assert_eq!(run.saved().setting("profiles_enabled"), false);
    run.engine.set_setting("profiles_enabled", true);
    run.engine.switch_profile(0);
    assert!(run.label(0).starts_with("demo"));
}

fn look(caption: Option<&str>, icon: Option<&str>, color: Option<&str>) -> Look {
    Look::parse(caption, icon, color).unwrap()
}

#[test]
fn own_look_replaces_caption_and_icon_but_keeps_agent_mark() {
    let mut run = Run::with_default(2, Some("auto"));
    run.sessions.0.lock().unwrap().role = Some("coder".into());
    run.click(0);
    assert_eq!(run.label(0), "UavVisionLab ✓ claude");
    assert_eq!(run.last_icon(), "coder");
    run.engine.set_look(0, look(Some("Рендер"), Some("text:R"), Some("blue")));
    assert_eq!(run.label(0), "Рендер ✓ claude"); // отметка состояния агента остаётся
    assert_eq!(run.last_icon(), ""); // своя иконка заменяет иконку роли
    assert_eq!(run.look(0), KeyLook { icon_text: "R".into(), color: "blue".into(), hint: String::new() });
    let saved = run.saved();
    assert_eq!(saved.values(0)["caption"], "Рендер");
    assert_eq!(saved.values(0)["icon"], "text:R");
    let key = &run.engine.overview()["keys"][0];
    assert_eq!(key["look"], json!({"caption": "Рендер", "icon": "text:R", "color": "blue"}));
    assert_eq!(key["auto_label"], "UavVisionLab ✓ claude");
    // Вид переезжает вместе с кнопкой.
    run.drag(0, Some(1));
    assert_eq!(run.label(1), "Рендер ✓ claude");
    assert_eq!(run.look(1).color, "blue");
    assert_eq!(run.look(0), KeyLook::default());
    // «Очистить» забывает сессию и свою иконку (назначение сменилось), подпись и цвет остаются;
    // «Сбросить вид» — отдельно.
    run.engine.clear(1);
    assert_eq!(run.label(1), "Рендер ");
    assert_eq!(run.saved_name(1), None);
    assert_eq!(run.saved().values(1)["color"], "blue");
    assert!(run.saved().values(1).get("icon").is_none());
    run.engine.set_look(1, Look::default());
    assert!(run.label(1).starts_with("Нет сессии"));
    assert_eq!(run.look(1), KeyLook::default());
    assert!(run.saved().values(1).is_empty());
}

#[test]
fn new_session_resets_custom_icon_but_keeps_color_and_caption() {
    let mut run = Run::with_default(1, Some("auto"));
    run.sessions.0.lock().unwrap().role = Some("core".into());
    run.engine.set_look(0, look(Some("Голос"), Some("lib:mic"), Some("purple")));
    assert_eq!(run.last_icon(), "lib:mic");
    run.click(0); // пустая кнопка запомнила сессию
    let saved = run.saved();
    assert!(saved.values(0).get("icon").is_none()); // микрофон не подходит роли core
    assert_eq!(saved.values(0)["color"], "purple");
    assert_eq!(saved.values(0)["caption"], "Голос");
    assert_eq!(run.last_icon(), "core"); // снова автоматическая иконка роли
}

#[test]
fn key_combination_button_shows_combo_under_own_caption() {
    let mut run = Run::with_default(2, Some("auto"));
    run.engine.set_look(0, look(None, Some("text:🎬"), None));
    run.engine.set_keys(0, Some("F12")).unwrap(); // вид не теряется
    assert_eq!(run.label(0), "F12");
    assert_eq!(run.look(0).hint, ""); // без своей подписи сочетание и есть подпись
    assert_eq!(run.look(0).icon_text, "🎬");
    run.engine.set_look(0, look(Some("Рендер"), Some("text:🎬"), None));
    assert_eq!(run.label(0), "Рендер");
    assert_eq!(run.look(0).hint, "F12");
    // Убрать сочетание: кнопка снова обычная, вид остаётся.
    run.engine.set_keys(0, None).unwrap();
    assert_eq!(run.saved().scenario_of(0), None);
    assert_eq!(run.label(0), "Рендер ");
    assert_eq!(run.look(0).hint, "");
}

#[test]
fn look_lives_in_its_profile_and_shows_on_thumbnails() {
    let mut run = Run::with_default(2, Some("auto"));
    assert!(run.engine.set_setting("profiles_enabled", true));
    run.engine.set_look(1, look(Some("Blender"), Some("text:B"), Some("orange")));
    run.engine.switch_profile(1);
    assert_eq!(run.look(1), KeyLook::default());
    assert!(run.label(1).starts_with("Нет сессии"));
    let thumb = &run.engine.overview()["profiles"][0][1];
    assert_eq!((thumb["icon_text"].as_str(), thumb["color"].as_str()), (Some("B"), Some("orange")));
    assert_eq!(thumb["styled"], true);
    assert_eq!(thumb["bound"], false);
    assert!(thumb["label"].as_str().unwrap().starts_with("Blender"));
    run.engine.switch_profile(0);
    assert_eq!(run.look(1).icon_text, "B");
}

#[test]
fn hold_on_key_combination_button_remembers_session() {
    let mut run = Run::with_default(2, Some("auto"));
    run.engine.set_keys(0, Some("Win+H")).unwrap();
    run.engine.set_look(0, look(None, Some("text:V"), Some("blue")));
    run.hold(0);
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
    assert_eq!(run.saved().scenario_of(0), None); // снова кнопка сессии (сценарий по умолчанию)
    assert!(run.saved().values(0).get("keys").is_none());
    // Сессия заменила назначение: иконка автоматическая, цвет остался.
    assert!(run.saved().values(0).get("icon").is_none());
    assert_eq!(run.saved().values(0)["color"], "blue");
    assert_eq!(run.look(0).icon_text, "");
    assert!(run.label(0).starts_with("demo"));
    run.click(0);
    assert_eq!(run.opened(), ["demo"]); // нажатие открывает сессию, а не жмёт сочетание
    // Перетаскивание кнопки-сочетания удержанием сессию не запоминает.
    run.engine.set_keys(1, Some("F12")).unwrap();
    run.drag(1, None);
    assert_eq!(run.saved().scenario_of(1), Some("keys"));
    assert_eq!(run.saved_name(1), None);
}

#[test]
fn hold_on_key_combination_button_with_confirmation() {
    let mut run = Run::new(2);
    run.engine.confirm_bind = true;
    run.engine.set_keys(0, Some("F12")).unwrap();
    run.hold(0);
    assert_eq!(run.saved().scenario_of(0), Some("keys")); // ждёт ответа
    run.engine.answer(true);
    assert_eq!(run.saved().scenario_of(0), Some("auto")); // сценария по умолчанию нет — auto
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
}

#[test]
fn library_icon_replaces_automatic_icon() {
    let mut run = Run::with_default(2, Some("auto"));
    run.sessions.0.lock().unwrap().role = Some("coder".into());
    run.click(0);
    assert_eq!(run.last_icon(), "coder");
    run.engine.set_look(0, look(None, Some("lib:clapperboard"), Some("orange")));
    assert_eq!(run.last_icon(), "lib:clapperboard"); // панель превратит в /lib/clapperboard.svg
    assert_eq!(run.look(0).icon_text, "");
    assert!(run.label(0).starts_with("UavVisionLab")); // подпись прежняя
    run.engine.set_look(0, Look::default());
    assert_eq!(run.last_icon(), "coder");
}

#[test]
fn own_image_and_program_icons_replace_automatic_icon() {
    let mut run = Run::with_default(2, Some("auto"));
    run.engine.set_look(0, look(None, Some("img:0123456789abcdef"), None));
    assert_eq!(run.last_icon(), "img:0123456789abcdef"); // панель: /usericon/<хэш>.png
    let blender = r"C:\Program Files\Blender Foundation\Blender\blender.exe";
    run.engine.set_look(0, look(Some("Blender"), Some(&format!("app:{blender}")), Some("orange")));
    assert_eq!(run.last_icon(), blender); // панель: /appicon/<id>.png
    assert_eq!(run.saved().values(0)["icon"], format!("app:{blender}"));
    assert!(run.label(0).starts_with("Blender"));
}

#[test]
fn window_without_program_icon_gets_generic_window_icon() {
    let mut run = Run::with_default(2, Some("auto"));
    run.sessions.0.lock().unwrap().window_icon = Some(String::new());
    run.click(0);
    assert_eq!(run.last_icon(), crate::windows::WINDOW_ICON);
    let program = r"C:\Program Files\App\app.exe";
    run.sessions.0.lock().unwrap().window_icon = Some(program.into());
    run.click(1);
    assert_eq!(run.last_icon(), program); // иконку программы (или запасную) выберет панель
}

#[test]
fn accept_before_session_is_read_remembers_it_when_read() {
    let mut run = Run::new(1);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.engine.answer(true); // не дождавшись сессии
    assert_eq!(run.last_asked(), None); // диалог закрыт
    assert_eq!(run.saved_name(0), None);
    run.engine.answer(true); // повторное «да» ничего не ломает
    run.read();
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
    assert!(run.label(0).starts_with("demo"));
    assert_eq!(run.device.asked.lock().unwrap().iter().flatten().count(), 1); // второго вопроса нет
}

#[test]
fn failed_read_is_shown_in_dialog_or_as_notice() {
    let mut run = Run::new(1);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.sessions.0.lock().unwrap().error = Some("нет терминала".into());
    run.click(0);
    run.read(); // диалог ещё открыт — ошибка в нём
    assert_eq!(run.last_asked(), Some((0, "Не удалось запомнить сессию на кнопке 1: нет терминала".to_string())));
    assert_eq!(run.last_confirm_state(), Confirm::Failed);
    run.engine.answer(true); // «ОК»
    assert_eq!(run.last_asked(), None);
    assert_eq!(run.saved_name(0), None);
    assert!(run.device.notices.lock().unwrap().is_empty());

    run.click(0);
    run.engine.answer(true); // согласился раньше — уведомление
    run.read();
    assert_eq!(*run.device.notices.lock().unwrap(), ["Не удалось запомнить сессию на кнопке 1: нет терминала"]);
    assert_eq!(run.saved_name(0), None);
    assert!(run.last_log().contains("не удалось запомнить текущую сессию: нет терминала"));
}

#[test]
fn late_session_after_cancel_or_clear_is_ignored() {
    let mut run = Run::new(1);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.click(0);
    run.engine.answer(false);
    run.read();
    assert_eq!(run.saved_name(0), None);
    assert_eq!(run.last_asked(), None);

    run.click(0);
    run.engine.answer(true);
    run.engine.clear(0); // кнопку очистили, пока сессия читалась
    run.read();
    assert_eq!(run.saved_name(0), None);
}

#[test]
fn moving_key_cancels_accepted_but_unread_session() {
    let mut run = Run::new(2);
    run.engine.confirm_bind = true;
    run.engine.assign(0, Some("auto"));
    run.engine.assign(1, Some("auto"));
    run.click(0);
    run.engine.answer(true);
    run.drag(0, Some(1)); // запоминание относилось к прежнему месту кнопки
    run.read();
    assert_eq!(run.saved_name(0), None);
    assert_eq!(run.saved_name(1), None);
}

#[test]
fn cross_on_closed_key_forgets_session_in_any_scenario() {
    let mut run = Run::new(2);
    run.engine.assign(0, Some("auto"));
    run.engine.assign(1, Some("remember_session")); // двойной клик у него открывает сессию
    run.click(0);
    run.click(1);
    run.engine.forget_closed(0); // сессия доступна — крестика не было, ничего
    assert_eq!(run.saved_name(0).as_deref(), Some("demo"));
    let closed = |run: &Run, key: usize| {
        let session = run.saved().values(key)["session"].as_object().unwrap().clone();
        (key, session, Probe { available: Some(false), info: None })
    };
    let probes = vec![closed(&run, 0), closed(&run, 1)];
    run.engine.show_probes(probes);
    run.engine.forget_closed(0);
    run.engine.forget_closed(1);
    assert_eq!(run.saved_name(0), None);
    assert_eq!(run.saved_name(1), None);
    assert!(run.opened().is_empty());
    assert!(run.last_log().contains("привязка к сессии «demo» очищена"));
    assert!(run.label(0).contains("Нет сессии"));
}

#[test]
fn closed_key_stays_gray_after_switching_profiles_back() {
    let mut run = Run::new(2);
    assert!(run.engine.set_setting("profiles_enabled", true));
    run.engine.assign(0, Some("auto"));
    run.click(0);
    let session = run.saved().values(0)["session"].as_object().unwrap().clone();
    run.engine.show_probes(vec![(0, session, Probe { available: Some(false), info: None })]);
    run.engine.switch_profile(1);
    run.engine.switch_profile(0);
    let shown = run.device.available.lock().unwrap().iter().rev().find(|(k, _)| *k == 0).copied();
    assert_eq!(shown, Some((0, false))); // сразу серая, без ожидания опроса
    run.engine.forget_closed(0); // и крестик работает сразу
    assert_eq!(run.saved_name(0), None);
}
