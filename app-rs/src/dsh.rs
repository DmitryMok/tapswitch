//! Сессии десктопного приложения DeepSeek Harness (dsh), перенос `dsh.py`.
//!
//! Список сессий читается из окна через UI Automation (`dsh_ui.ps1`): там видно, какая
//! сессия выбрана и какая работает. Ссылки на сессию у dsh нет, поэтому сессия
//! открывается выбором строки в списке. Строка узнаётся по рабочей области и названию;
//! id сессии (uuid) берётся из данных dsh на диске, когда его удаётся сопоставить, —
//! тогда переименование и одноимённые сессии не мешают.

use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use crate::sessions::{Backend, Probe, Result, Session, SessionError, describe_pane};
use crate::{win, workers};

pub const KIND: &str = "dsh";
const PROCESS: &str = "DeepSeek Harness.exe";
const WINDOW_CLASS: &str = "Chrome_WidgetWin_1";
const UNTITLED: &str = "Untitled";
const AGENT: &str = "dsh";
/// Список перерисовывается после щелчка.
const CLICK_SETTLE: Duration = Duration::from_millis(300);
const TIMEOUT: Duration = Duration::from_secs(10);
const SCRIPT: &str = include_str!("../../app/agent_control/dsh_ui.ps1");
const RUNNING: &str = "Running ";

/// Отрезать в конце возраст сессии: « now», « 2min», « 1mo», « 23 h».
fn strip_age(name: &str) -> &str {
    let trimmed = name.trim_end();
    let Some(space) = trimmed.rfind(char::is_whitespace) else { return name };
    let (head, word) = (&trimmed[..space], &trimmed[space..].trim_start());
    let is_age = |word: &str| {
        if word == "now" {
            return true;
        }
        let digits = word.chars().take_while(char::is_ascii_digit).count();
        let unit = word[digits..].trim_start();
        digits > 0 && ["s", "sec", "m", "min", "h", "d", "w", "mo", "y"].contains(&unit)
    };
    if is_age(word) {
        return head;
    }
    // Число и единица через пробел: «23 min».
    if ["s", "sec", "m", "min", "h", "d", "w", "mo", "y"].contains(word) {
        if let Some(space) = head.trim_end().rfind(char::is_whitespace) {
            let number = head.trim_end()[space..].trim_start();
            if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
                return &head[..space];
            }
        }
    }
    name
}

/// «Running проверь характиристи 2min» → («проверь характиристи», true).
pub fn parse_row_name(name: &str) -> (String, bool) {
    let running = name.starts_with(RUNNING);
    let name = if running { &name[RUNNING.len()..] } else { name };
    (strip_age(name).trim().to_string(), running)
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Row {
    /// workspace, session, more («Show N more sessions»).
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub expanded: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub index: usize,
    pub workspace: Option<String>,
    pub title: String,
    pub running: bool,
    /// Сколько строк с тем же названием выше в той же рабочей области.
    pub rank: usize,
}

pub fn session_rows(rows: &[Row]) -> Vec<SessionRow> {
    let mut found: Vec<SessionRow> = Vec::new();
    let mut workspace = None;
    for (index, row) in rows.iter().enumerate() {
        match row.kind.as_str() {
            "workspace" => workspace = Some(row.name.clone()),
            "session" => {
                let (title, running) = parse_row_name(&row.name);
                let rank = found.iter().filter(|s| s.workspace == workspace && s.title == title).count();
                found.push(SessionRow { index, workspace: workspace.clone(), title, running, rank });
            }
            _ => {}
        }
    }
    found
}

/// Строка сессии; если одноимённых меньше, чем ожидалось, — первая из них.
pub fn locate(rows: &[Row], workspace: Option<&str>, title: &str, rank: usize) -> Option<SessionRow> {
    let same: Vec<SessionRow> =
        session_rows(rows).into_iter().filter(|s| s.workspace.as_deref() == workspace && s.title == title).collect();
    let index = if rank < same.len() { rank } else { 0 };
    same.into_iter().nth(index)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DshRecord {
    pub id: String,
    pub workspace: String,
    pub path: String,
    /// None: dsh ещё не записал название на диск.
    pub title: Option<String>,
    pub blank: bool,
    pub recency: f64,
    /// Идёт ли ход агента — из файла сессии на диске. Окно dsh в фоне показывает
    /// устаревший список, а этот файл dsh перезаписывает на каждом шаге.
    /// None: файла нет или он как раз перезаписывается — тогда верить окну.
    pub running: Option<bool>,
}

/// Данные dsh на диске (`~/.dsh`): рабочие области, названия и id сессий.
/// Названия dsh записывает с задержкой, поэтому данные дополняют, а не заменяют окно.
pub struct DshStore {
    home: PathBuf,
}

fn mtime(meta: &std::fs::Metadata) -> f64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64())
}

impl DshStore {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    fn load(&self, name: &str) -> Value {
        // dsh не установлен или как раз перезаписывает файл — пусто.
        std::fs::read_to_string(self.home.join("storages").join(name))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    }

    /// Строки сессии из её собственного файла (`storages/session_projcache/sessions/<id>.json`,
    /// обновляется на каждом шаге) или Null.
    fn session_rows(&self, id: &str) -> Value {
        let path = self.home.join("storages").join("session_projcache").join("sessions").join(format!("{id}.json"));
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .map(|mut value| value["record"]["rows"].take())
            .unwrap_or(Value::Null)
    }

    /// Время последнего изменения файлов каждой сессии.
    fn recency(&self) -> HashMap<String, f64> {
        let mut times = HashMap::new();
        let Ok(projects) = std::fs::read_dir(self.home.join("sessions")) else { return times };
        for project in projects.flatten() {
            if !project.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(sessions) = std::fs::read_dir(project.path()) else { continue };
            for session in sessions.flatten() {
                let files = std::fs::read_dir(session.path())
                    .map(|entries| {
                        entries
                            .flatten()
                            .filter_map(|f| f.metadata().ok())
                            .filter(|m| m.is_file())
                            .map(|m| mtime(&m))
                            .fold(None, |best: Option<f64>, t| Some(best.map_or(t, |b| b.max(t))))
                    })
                    .ok()
                    .flatten();
                let time = files.or_else(|| session.metadata().ok().map(|m| mtime(&m))).unwrap_or(0.0);
                times.insert(session.file_name().to_string_lossy().into_owned(), time);
            }
        }
        times
    }

    /// Неархивные сессии, от недавних к давним.
    pub fn records(&self) -> Vec<DshRecord> {
        let layout = self.load("workspace.json");
        let archived: HashSet<&str> =
            layout["global"]["archivedSessionIds"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let cache = self.load("session_projcache.json");
        let cache = &cache["tables"]["sessions"];
        let recency = self.recency();
        let mut records = Vec::new();
        let Some(workspaces) = layout["tables"]["workspaces"].as_object() else { return records };
        for workspace in workspaces.values() {
            for id in workspace["sessionIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if archived.contains(id) {
                    continue;
                }
                // Свежий файл сессии важнее общего кэша: тот dsh обновляет редко.
                let own = self.session_rows(id);
                let rows = if own.is_object() { &own } else { &cache[id]["rows"] };
                let meta = &rows["sessionListMetadata"]["val"];
                let turn = &rows["turnBoundary"]["val"];
                let running = turn.is_object().then(|| !turn["openTurnStartSeq"].is_null());
                let prompted = meta["lastPromptAt"].as_f64().unwrap_or(0.0) / 1000.0;
                records.push(DshRecord {
                    id: id.to_string(),
                    workspace: workspace["title"].as_str().unwrap_or("").to_string(),
                    path: workspace["path"].as_str().unwrap_or("").to_string(),
                    title: rows["title"]["val"].as_str().map(str::to_string),
                    blank: meta["blank"].as_bool().unwrap_or(false),
                    recency: recency.get(id).copied().unwrap_or(prompted),
                    running,
                });
            }
        }
        // Устойчивая сортировка: при равном времени — порядок в workspace.json.
        records.sort_by(|a, b| b.recency.total_cmp(&a.recency));
        records
    }

    pub fn path_of(&self, workspace: Option<&str>) -> Option<String> {
        let layout = self.load("workspace.json");
        let workspaces = layout["tables"]["workspaces"].as_object()?;
        workspaces
            .values()
            .find(|w| w["title"].as_str().is_some() && w["title"].as_str() == workspace)
            .and_then(|w| w["path"].as_str().map(str::to_string))
    }
}

/// Сессия по строке окна: rank-я из одноимённых в рабочей области (самая свежая — первая).
pub fn resolve<'a>(records: &'a [DshRecord], workspace: Option<&str>, title: &str, rank: usize) -> Option<&'a DshRecord> {
    let own: Vec<&DshRecord> = records.iter().filter(|r| Some(r.workspace.as_str()) == workspace).collect();
    let named: Vec<&DshRecord> = if title == UNTITLED {
        own.iter().copied().filter(|r| r.blank || r.title.as_deref() == Some(UNTITLED)).collect()
    } else {
        own.iter().copied().filter(|r| r.title.as_deref() == Some(title)).collect()
    };
    if rank < named.len() {
        return Some(named[rank]);
    }
    // Название ещё не на диске.
    let unknown: Vec<&DshRecord> = own.into_iter().filter(|r| r.title.is_none() && !r.blank).collect();
    if unknown.len() == 1 { Some(unknown[0]) } else { None }
}

/// Запись о сессии кнопки: по id, а у кнопки без id (назначена, пока dsh не записал
/// название) — по рабочей области, названию и номеру среди одноимённых. Область могли
/// переименовать во «<проект> / <папка>» (плагин dsh workspace-parent-label) — тоже подходит.
pub fn record_of<'a>(records: &'a [DshRecord], session: &Session) -> Option<&'a DshRecord> {
    if let Some(id) = text(session, "id") {
        return records.iter().find(|r| r.id == id);
    }
    let title = text(session, "name")?;
    let workspace = text(session, "workspace");
    let rank = session.get("rank").and_then(Value::as_u64).unwrap_or(0) as usize;
    let named = |place: &dyn Fn(&DshRecord) -> bool| {
        records.iter().filter(|r| place(r) && r.title.as_deref() == Some(title)).nth(rank)
    };
    let Some(w) = workspace else { return named(&|_| true) };
    let renamed = format!(" / {w}");
    named(&|r| r.workspace == w).or_else(|| named(&|r| r.workspace.ends_with(&renamed)))
}

pub fn rank_of(records: &[DshRecord], record: &DshRecord) -> usize {
    records
        .iter()
        .filter(|r| r.workspace == record.workspace && r.title == record.title)
        .position(|r| r.id == record.id)
        .unwrap_or(0)
}

pub trait DshUi: Send + Sync {
    /// Главное окно dsh или None, если приложение не запущено.
    fn window(&self) -> Option<i64>;
    /// Строки списка сессий сверху вниз.
    fn read(&self, window: i64) -> Result<Vec<Row>>;
    /// select — выбрать строку сессии; click — щёлкнуть мышью по строке рабочей области
    /// или «Show N more sessions» (программное раскрытие роняет dsh).
    fn act(&self, window: i64, action: &str, row: usize, name: &str) -> Result<()>;
    /// Вывести окно на передний план.
    fn show(&self, window: i64);
    /// Окно этого приложения.
    fn is_dsh(&self, window: i64) -> bool;
}

pub fn is_dsh_window(window: i64) -> bool {
    win::class_name(window) == WINDOW_CLASS && {
        let path = win::process_path(window);
        path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case(PROCESS))
    }
}

fn failure(text: impl std::fmt::Display) -> SessionError {
    SessionError::Other(format!("Не удалось обратиться к DeepSeek Harness: {text}"))
}

pub struct WindowsDshUi {
    script: PathBuf,
}

impl WindowsDshUi {
    pub fn new() -> Self {
        Self { script: workers::script("dsh_ui.ps1", SCRIPT) }
    }

    fn run(&self, action: &str, window: i64, row: i64, name: &str) -> Result<String> {
        let (window, row) = (window.to_string(), row.to_string());
        let env = [
            ("AGENT_CONTROL_ACTION", action),
            ("AGENT_CONTROL_WINDOW", window.as_str()),
            ("AGENT_CONTROL_INDEX", row.as_str()),
            ("AGENT_CONTROL_NAME", name),
        ];
        let (code, out) = workers::powershell_script(&self.script, &env, TIMEOUT).map_err(failure)?;
        match code {
            0 => Ok(out),
            3 => Err(SessionError::Other(
                "список сессий DeepSeek Harness не найден или изменился: окно dsh должно быть видно на экране, повторите"
                    .into(),
            )),
            4 => Err(SessionError::Other(format!(
                "строку «{name}» в DeepSeek Harness не нажать: она прокручена за край списка или закрыта другим окном — откройте её вручную"
            ))),
            _ => Err(failure(out.trim().chars().take(300).collect::<String>())),
        }
    }
}

impl DshUi for WindowsDshUi {
    fn window(&self) -> Option<i64> {
        win::top_level_windows(true).into_iter().find(|&w| is_dsh_window(w))
    }

    fn read(&self, window: i64) -> Result<Vec<Row>> {
        let out = self.run("read", window, -1, "")?;
        let out = out.trim();
        if out.is_empty() {
            return Ok(vec![]);
        }
        serde_json::from_str(out).map_err(|e| failure(format!("непонятный ответ: {e}")))
    }

    fn act(&self, window: i64, action: &str, row: usize, name: &str) -> Result<()> {
        self.run(action, window, row as i64, name).map(drop)
    }

    fn show(&self, window: i64) {
        if !win::bring_to_foreground(window) {
            crate::log::write("Windows не дал вывести на передний план окно DeepSeek Harness");
        }
    }

    fn is_dsh(&self, window: i64) -> bool {
        is_dsh_window(window)
    }
}

/// Идёт ли работа: по файлу сессии на диске, если он прочитан, иначе по строке окна.
fn is_running(record: Option<&DshRecord>, row_running: bool) -> bool {
    record.and_then(|r| r.running).unwrap_or(row_running)
}

fn describe(session: &mut Session, path: Option<String>, running: bool) {
    // Известно только «идёт работа». Остановка может значить и «готов», и «ждёт ответа»,
    // и «сделал» — поэтому состояние неизвестно, без значков на кнопке.
    let pane = json!({"agent": AGENT, "agent_status": if running { "working" } else { "unknown" }, "cwd": path});
    session.extend(describe_pane(&pane, ""));
    let empty = session.get("title").is_none_or(|t| t.as_str().is_none_or(str::is_empty));
    if empty {
        let name = session.get("name").cloned().unwrap_or(Value::Null);
        session.insert("title".into(), name);
    }
}

fn text<'a>(session: &'a Session, name: &str) -> Option<&'a str> {
    session.get(name).and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub struct DshSessions {
    ui: Box<dyn DshUi>,
    store: DshStore,
    settle: Duration,
}

impl DshSessions {
    pub fn new(ui: Box<dyn DshUi>, store: DshStore) -> Self {
        Self { ui, store, settle: CLICK_SETTLE }
    }

    fn window(&self) -> Result<i64> {
        self.ui.window().ok_or_else(|| SessionError::Other("DeepSeek Harness не запущен".into()))
    }

    fn path(&self, record: Option<&DshRecord>, workspace: Option<&str>) -> Option<String> {
        record.map(|r| r.path.clone()).or_else(|| self.store.path_of(workspace))
    }

    /// Найти строку, при необходимости раскрыв рабочую область и «Show N more sessions».
    fn find(&self, window: i64, workspace: Option<&str>, title: &str, rank: usize) -> Result<Option<(SessionRow, Vec<Row>)>> {
        let mut rows = self.ui.read(window)?;
        for control in ["workspace", "more"] {
            if let Some(row) = locate(&rows, workspace, title, rank) {
                return Ok(Some((row, rows)));
            }
            let Some(target) = hidden_rows_control(&rows, workspace, control) else { continue };
            self.ui.act(window, "click", target, &rows[target].name)?;
            std::thread::sleep(self.settle);
            rows = self.ui.read(window)?;
        }
        Ok(locate(&rows, workspace, title, rank).map(|row| (row, rows)))
    }
}

fn hidden_rows_control(rows: &[Row], workspace: Option<&str>, control: &str) -> Option<usize> {
    let mut current: Option<&str> = None;
    for (index, row) in rows.iter().enumerate() {
        if row.kind == "workspace" {
            current = Some(&row.name);
            if control == "workspace" && current == workspace && row.expanded == Some(false) {
                return Some(index);
            }
        } else if control == "more" && row.kind == "more" && current == workspace {
            return Some(index);
        }
    }
    None
}

impl Backend for DshSessions {
    fn matches(&self, window: i64) -> bool {
        self.ui.is_dsh(window)
    }

    fn current(&self) -> Result<Session> {
        let window = self.window()?;
        let rows = self.ui.read(window)?;
        let Some(selected) = session_rows(&rows).into_iter().find(|s| rows[s.index].selected) else {
            return Err(SessionError::Other("в DeepSeek Harness не выбрана сессия".into()));
        };
        let records = self.store.records();
        let record = resolve(&records, selected.workspace.as_deref(), &selected.title, selected.rank);
        let mut session = json!({
            "kind": KIND,
            "server": AGENT,
            "id": record.map(|r| r.id.clone()),
            "workspace": selected.workspace,
            "name": selected.title,
            "rank": selected.rank,
        })
        .as_object()
        .unwrap()
        .clone();
        let path = self.path(record, selected.workspace.as_deref());
        describe(&mut session, path, is_running(record, selected.running));
        Ok(session)
    }

    fn focused(&self, _window: i64, _bound: &[Session]) -> Option<Session> {
        self.current().ok()
    }

    /// Доступна, пока запущен dsh и сессия не удалена. Работает ли — по файлу сессии на диске,
    /// а если его нет — по строке в списке окна. Перекрытое окно dsh список не отдаёт, а строка
    /// в свёрнутой области не видна: без файла состояние тогда неизвестно и не меняется.
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        let unavailable = || Probe { available: Some(false), info: None };
        let Some(window) = self.ui.window() else { return Some(sessions.iter().map(|_| unavailable()).collect()) };
        let records = self.store.records();
        let rows = self.ui.read(window).ok();
        let results = sessions
            .iter()
            .map(|session| {
                let record = record_of(&records, session);
                if text(session, "id").is_some() && record.is_none() && !records.is_empty() {
                    return unavailable();
                }
                let named = record.filter(|r| r.title.is_some());
                let (workspace, title, rank) = match named {
                    Some(r) => (Some(r.workspace.clone()), r.title.clone().unwrap(), rank_of(&records, r)),
                    None => (
                        text(session, "workspace").map(str::to_string),
                        text(session, "name").unwrap_or("").to_string(),
                        session.get("rank").and_then(Value::as_u64).unwrap_or(0) as usize,
                    ),
                };
                let row = rows.as_deref().and_then(|rows| locate(rows, workspace.as_deref(), &title, rank));
                let running = record.and_then(|r| r.running).or(row.map(|row| row.running));
                let info = running.map(|running| {
                    let mut info = Session::new();
                    info.insert("name".into(), session.get("name").cloned().unwrap_or(Value::Null));
                    describe(&mut info, self.path(record, workspace.as_deref()), running);
                    info.remove("name");
                    info
                });
                // Окно не прочитано и файла нет — ничего не известно, прежнее состояние остаётся.
                let available = (rows.is_some() || info.is_some()).then_some(true);
                Probe { available, info }
            })
            .collect();
        Some(results)
    }

    fn open(&self, session: &mut Session) -> Result<()> {
        let window = self.window()?;
        let records = self.store.records();
        let had_id = text(session, "id").is_some();
        let record = record_of(&records, session).cloned();
        if had_id && record.is_none() && !records.is_empty() {
            return Err(SessionError::Lost("сессии больше нет в DeepSeek Harness (удалена или в архиве)".into()));
        }
        let named = record.as_ref().filter(|r| r.title.is_some());
        if let Some(r) = named {
            session.insert("name".into(), json!(r.title)); // сессию могли переименовать
            session.insert("workspace".into(), json!(r.workspace));
            // Кнопка без id получает его: дальше переименования ей не мешают.
            session.insert("id".into(), json!(r.id));
        }
        let rank = match named {
            Some(r) => rank_of(&records, r),
            None => session.get("rank").and_then(Value::as_u64).unwrap_or(0) as usize,
        };
        let workspace = text(session, "workspace").map(str::to_string);
        let name = text(session, "name").unwrap_or("").to_string();
        self.ui.show(window);
        let Some((row, rows)) = self.find(window, workspace.as_deref(), &name, rank)? else {
            return Err(SessionError::Lost(format!("сессии «{name}» нет в списке DeepSeek Harness")));
        };
        self.ui.act(window, "select", row.index, &rows[row.index].name)?;
        let path = self.path(record.as_ref(), workspace.as_deref());
        describe(session, path, is_running(record.as_ref(), row.running));
        Ok(())
    }
}

/// `%USERPROFILE%\.dsh` → полный путь (переменные окружения в стиле Windows).
pub fn expand_env(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else { break };
        let name = &after[..end];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(value) if !name.is_empty() => out.push_str(&value),
            _ => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
