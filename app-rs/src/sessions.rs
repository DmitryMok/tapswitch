//! Сессии агентов: панели на нескольких серверах herdr, открытых во вкладках терминала
//! (перенос `sessions.py`). Сессия — объект JSON, как в `state.json`.

use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub type Session = Map<String, Value>;

const MIN_LINE_LENGTH: usize = 12; // короткие строки встречаются где угодно
const LINE_PREFIX: usize = 20;
const MIN_HITS: usize = 2;
const MIN_COVERAGE: f64 = 0.6;
const READ_WORKERS: usize = 16;

const CORE_ROLE: &str = "core";
/// Сколько помнить, где нашлись вкладки herdr с пропавшим адресом: опрос идёт каждые
/// пару секунд, а чтение вкладок — запуск скрипта.
const RELOCATE_TTL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq)]
pub enum SessionError {
    Other(String),
    /// Сессии больше нет: её надо запомнить заново. Остальные ошибки считаются временными.
    Lost(String),
    TabNotFound(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            SessionError::Other(text) | SessionError::Lost(text) | SessionError::TabNotFound(text) => f.write_str(text),
        }
    }
}

pub type Result<T> = std::result::Result<T, SessionError>;

fn error<T>(text: impl Into<String>) -> Result<T> {
    Err(SessionError::Other(text.into()))
}

/// Состояние запомненной сессии: доступна ли (None — неизвестно) и свежие сведения о ней.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub available: Option<bool>,
    pub info: Option<Session>,
}

/// Все виды сессий вместе: что выделено сейчас, открыть, проверить, что в фокусе.
pub trait Sessions: Send + Sync {
    fn current(&self) -> Result<Session>;
    fn open(&self, session: &mut Session) -> Result<()>;
    fn probe(&self, sessions: &[Session]) -> Vec<Probe>;
    /// Сессия в окне, на котором сейчас фокус; ищется только среди запомненных.
    fn focused(&self, bound: &[Session]) -> Option<Session>;
}

fn str_of<'a>(session: &'a Session, name: &str) -> Option<&'a str> {
    session.get(name).and_then(Value::as_str)
}

fn kind_of(session: &Session) -> &str {
    str_of(session, "kind").unwrap_or("herdr")
}

/// Непустое значение (как проверка истинности в Python).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

/// Одна ли это сессия: herdr — сервер и панель, другие виды — id, а без id — область и название.
pub fn same_session(first: Option<&Session>, second: Option<&Session>) -> bool {
    let (Some(first), Some(second)) = (first, second) else { return false };
    if first.is_empty() || second.is_empty() {
        return false;
    }
    let kind = kind_of(first);
    if kind != kind_of(second) {
        return false;
    }
    let get = |s: &Session, name: &str| s.get(name).cloned().unwrap_or(Value::Null);
    if kind == "herdr" {
        return truthy(first.get("id")) && get(first, "server") == get(second, "server") && get(first, "id") == get(second, "id");
    }
    if truthy(first.get("id")) && truthy(second.get("id")) {
        return first["id"] == second["id"];
    }
    get(first, "workspace") == get(second, "workspace") && get(first, "name") == get(second, "name")
}

// --- поиск панели на экране по тексту

pub struct Candidate {
    pub server_id: String,
    pub pane: Value,
    pub lines: HashSet<String>,
}

/// Начала строк: в узком окне те же строки обрезаны по ширине.
pub fn significant_lines(text: &str) -> HashSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.chars().count() >= MIN_LINE_LENGTH)
        .map(|line| line.chars().take(LINE_PREFIX).collect())
        .collect()
}

/// Найти панель, видимую на экране; при неявном совпадении вернуть None.
pub fn find_visible_pane<'a>(screen: &str, candidates: &'a [Candidate]) -> Option<&'a Candidate> {
    let visible: Vec<(usize, &Candidate)> = candidates
        .iter()
        .filter_map(|candidate| {
            let hits = candidate.lines.iter().filter(|line| screen.contains(line.as_str())).count();
            (hits >= MIN_HITS && hits as f64 >= MIN_COVERAGE * candidate.lines.len() as f64).then_some((hits, candidate))
        })
        .collect();
    if visible.iter().map(|(_, c)| c.server_id.as_str()).collect::<HashSet<_>>().len() != 1 {
        return None;
    }
    // На вкладке с разделением видно несколько панелей: важнее та, что в фокусе.
    let focused: Vec<_> = visible.iter().filter(|(_, c)| c.pane["focused"] == true).collect();
    let pool: Vec<_> = if focused.is_empty() { visible.iter().collect() } else { focused };
    // max() в Python берёт первый из равных — здесь так же.
    pool.into_iter().fold(None, |best: Option<&(usize, &Candidate)>, item| match best {
        Some(b) if b.0 >= item.0 => Some(b),
        _ => Some(item),
    })
    .map(|(_, candidate)| *candidate)
}

/// `…/UavVisionLab/06 Roles/dev` — проект UavVisionLab, роль dev; иначе роль core,
/// а проект — последняя папка пути.
pub fn project_and_role(cwd: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else { return (None, None) };
    let windows = cwd.contains('\\') || cwd.chars().take(3).any(|c| c == ':');
    let parts: Vec<&str> = if windows {
        let mut parts: Vec<&str> = cwd.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
        if parts.first().is_some_and(|p| p.ends_with(':')) {
            parts.remove(0); // диск — часть корня, не папка
        }
        parts
    } else {
        cwd.split('/').filter(|p| !p.is_empty()).collect()
    };
    let roles_folder = |part: &str| {
        part.strip_suffix(" Roles").is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    for (index, part) in parts.iter().enumerate() {
        if roles_folder(part) && index > 0 && index < parts.len() - 1 {
            return (Some(parts[index - 1].to_string()), Some(parts[index + 1].to_string()));
        }
    }
    (parts.last().map(|p| p.to_string()), Some(CORE_ROLE.into()))
}

fn agent_mark(status: &str) -> &'static str {
    match status {
        "idle" | "done" => "✓",
        "working" => "…",
        "blocked" => "⚠",
        _ => "?",
    }
}

/// Сведения о панели, которые меняются со временем: агент, его состояние, папка.
/// no_agent — подпись, если herdr не назвал агента.
pub fn describe_pane(pane: &Value, no_agent: &str) -> Session {
    let agent = pane["agent"].as_str().filter(|a| !a.is_empty());
    let status = pane["agent_status"].as_str().filter(|s| !s.is_empty()).unwrap_or("unknown");
    let (project, role) = project_and_role(pane["cwd"].as_str());
    let title = project.as_ref().map(|p| format!("{p}|{}", role.as_deref().unwrap_or("")));
    let info = json!({
        "cwd": pane["cwd"],
        "project": project,
        "role": role,
        "title": title,
        "agent": agent,
        "agent_status": agent.map(|_| status),
        "ready": agent.is_some() && matches!(status, "idle" | "done"),
        "agent_mark": match agent {
            Some(agent) => format!("{} {agent}", agent_mark(status)),
            None => no_agent.to_string(),
        },
    });
    info.as_object().unwrap().clone()
}

pub fn readiness(session: &Session) -> String {
    let Some(agent) = str_of(session, "agent").filter(|a| !a.is_empty()) else {
        return "агент herdr не определён".into();
    };
    match str_of(session, "agent_status") {
        Some("idle") => format!("{agent} готов к промпту"),
        Some("done") => format!("{agent} закончил и готов к промпту"),
        Some("working") => format!("{agent} занят"),
        Some("blocked") => format!("{agent} ждёт подтверждения"),
        _ => format!("{agent}, состояние неизвестно"),
    }
}

// --- поиск панели по интерфейсу herdr на экране

#[derive(Deserialize, Clone, Debug)]
pub struct ChromeEntry {
    pub label: String,
    pub background: Option<i64>,
}

/// Боковая панель и строка вкладок herdr, прочитанные с экрана (см. terminal_tabs.ps1).
#[derive(Deserialize, Clone, Debug)]
pub struct Chrome {
    #[serde(default)]
    pub collapsed: bool,
    pub sidebar_background: Option<i64>,
    #[serde(default)]
    pub workspaces: Vec<ChromeEntry>,
    #[serde(default)]
    pub tabs: Vec<ChromeEntry>,
}

/// Цвета зависят от темы, поэтому активная строка та, чей фон отличается от фона панели.
fn highlighted_workspace(chrome: &Chrome) -> Option<&str> {
    if chrome.workspaces.len() == 1 {
        return Some(&chrome.workspaces[0].label);
    }
    let marked: Vec<&str> = chrome
        .workspaces
        .iter()
        .filter(|e| e.background != chrome.sidebar_background)
        .map(|e| e.label.as_str())
        .collect();
    (marked.len() == 1).then(|| marked[0])
}

fn color_distance(first: i64, second: i64) -> i64 {
    [0, 8, 16].iter().map(|shift| ((first >> shift & 255) - (second >> shift & 255)).abs()).sum()
}

/// Активная вкладка сильнее других отличается от фона интерфейса.
fn highlighted_tab(chrome: &Chrome) -> Option<&str> {
    if chrome.tabs.len() == 1 {
        return Some(&chrome.tabs[0].label);
    }
    let background = chrome.sidebar_background.unwrap_or(0);
    let mut ranked: Vec<&ChromeEntry> = chrome.tabs.iter().collect();
    // Устойчивая сортировка по убыванию — как sorted(reverse=True) в Python.
    ranked.sort_by_key(|e| std::cmp::Reverse(color_distance(e.background.unwrap_or(0), background)));
    if ranked.len() < 2 || ranked[0].background == ranked[1].background {
        return None;
    }
    Some(&ranked[0].label)
}

fn label_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Экран должен совпасть с сервером целиком: рабочие области и вкладки активной из них.
fn match_server_chrome(chrome: &Chrome, snapshot: &Value, active_workspace: &str, active_tab: &str) -> Option<Value> {
    let key = if chrome.collapsed { "number" } else { "label" };
    let workspaces: HashMap<String, &Value> =
        snapshot["workspaces"].as_array()?.iter().map(|w| (label_text(&w[key]), w)).collect();
    let on_screen: BTreeSet<&str> = chrome.workspaces.iter().map(|e| e.label.as_str()).collect();
    if on_screen != workspaces.keys().map(String::as_str).collect() {
        return None;
    }
    let workspace_id = &workspaces[active_workspace]["workspace_id"];
    let tabs: HashMap<String, &Value> = snapshot["tabs"]
        .as_array()?
        .iter()
        .filter(|t| &t["workspace_id"] == workspace_id)
        .map(|t| (label_text(&t["label"]), t))
        .collect();
    let on_screen: BTreeSet<&str> = chrome.tabs.iter().map(|e| e.label.as_str()).collect();
    if on_screen != tabs.keys().map(String::as_str).collect() {
        return None;
    }
    let tab_id = &tabs[active_tab]["tab_id"];
    let pane_id = snapshot["layouts"].as_array()?.iter().find(|l| &l["tab_id"] == tab_id)?["focused_pane_id"].clone();
    if pane_id.is_null() {
        return None;
    }
    Some(json!({"pane_id": pane_id, "tab_id": tab_id, "workspace_id": workspace_id}))
}

/// Найти панель по интерфейсу herdr на экране: текст в самой панели не нужен.
pub fn find_pane_by_chrome(chrome: Option<&Chrome>, snapshots: &[(String, Value)]) -> Option<(String, Value)> {
    let chrome = chrome.filter(|c| !c.workspaces.is_empty() && !c.tabs.is_empty())?;
    let active_workspace = highlighted_workspace(chrome)?;
    let active_tab = highlighted_tab(chrome)?;
    let mut matches = snapshots.iter().filter_map(|(server_id, snapshot)| {
        match_server_chrome(chrome, snapshot, active_workspace, active_tab).map(|pane| (server_id.clone(), pane))
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

// --- терминал

pub struct TerminalTab {
    pub window: i64,
    /// None: окно на весь экран скрывает вкладки.
    pub tab: Option<String>,
    pub text: String,
    pub chrome: Option<Chrome>,
}

pub struct TabRef {
    pub window: i64,
    pub tab: Option<String>,
    pub selected: bool,
    /// Заголовок вкладки; у вкладки с herdr — «herdr». Пустой у полноэкранного окна.
    pub title: String,
}

fn looks_like_herdr(title: &str) -> bool {
    title.to_lowercase().contains("herdr")
}

/// Где сейчас herdr, чья вкладка по запомненному адресу пропала (например, после
/// перезагрузки у окон терминала другие номера).
#[derive(Clone, Debug, PartialEq)]
enum Located {
    /// Выбранная вкладка показывает этот herdr: новый адрес для кнопки.
    At(i64, Option<String>),
    /// Есть невыбранная вкладка herdr, какой сервер в ней — без переключения не прочитать:
    /// нажатие найдёт её перебором вкладок.
    Hidden,
}

struct Relocation {
    at: Instant,
    servers: BTreeSet<String>,
    found: HashMap<String, Located>,
}

pub trait Terminal: Send + Sync {
    /// Выбранная вкладка последнего активного окна терминала.
    fn active_tab(&self) -> Result<TerminalTab>;
    /// Выбранная вкладка окна: читать можно только её.
    fn read_selected(&self, window: i64) -> Result<TerminalTab>;
    /// Все вкладки всех окон терминала.
    fn list_tabs(&self) -> Result<Vec<TabRef>>;
    /// Выбрать вкладку, не выводя окно вперёд.
    fn select_tab(&self, window: i64, tab: Option<&str>) -> Result<()>;
    /// Выбрать вкладку, если она известна, и вывести её окно вперёд.
    fn show_tab(&self, window: i64, tab: Option<&str>) -> Result<()>;
    /// Выбранная вкладка окна без чтения её текста.
    fn selected_tab(&self, window: i64) -> Result<Option<String>>;
    /// Вкладки окна; None — окна больше нет, пустой список — окно на весь экран.
    fn window_tabs(&self, window: i64) -> Result<Option<Vec<String>>>;
    /// Окно терминала.
    fn is_terminal(&self, window: i64) -> bool;
}

// --- сервер herdr

pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub type Runner = Box<dyn Fn(&[String]) -> std::result::Result<Output, String> + Send + Sync>;

pub struct HerdrServer {
    pub id: String,
    pub detects_agents: bool,
    /// Без распознавания агентов «нет агента» неправда: агент может быть, он просто неизвестен.
    command: Vec<String>,
    run: Runner,
}

/// Место панели на сетке вкладки: по прямоугольникам видно, кто кому сосед.
#[derive(Clone, Copy)]
struct Rect {
    x: i64,
    y: i64,
    width: i64,
    height: i64,
}

fn pane_rects(layout: &Value) -> Vec<(String, Rect)> {
    layout["panes"]
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter_map(|pane| {
                    let rect = &pane["rect"];
                    Some((
                        pane["pane_id"].as_str()?.to_string(),
                        Rect {
                            x: rect["x"].as_i64()?,
                            y: rect["y"].as_i64()?,
                            width: rect["width"].as_i64()?,
                            height: rect["height"].as_i64()?,
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Соседние панели по сторонам: прямоугольники касаются друг друга.
fn pane_neighbors<'a>(rects: &'a [(String, Rect)], pane_id: &str) -> Vec<(&'static str, &'a str)> {
    let Some((_, a)) = rects.iter().find(|(id, _)| id == pane_id) else { return Vec::new() };
    let mut found = Vec::new();
    for (id, b) in rects {
        if id == pane_id {
            continue;
        }
        let vertical = a.y < b.y + b.height && b.y < a.y + a.height;
        let horizontal = a.x < b.x + b.width && b.x < a.x + a.width;
        if vertical && a.x + a.width == b.x {
            found.push(("right", id.as_str()));
        } else if vertical && b.x + b.width == a.x {
            found.push(("left", id.as_str()));
        } else if horizontal && a.y + a.height == b.y {
            found.push(("down", id.as_str()));
        } else if horizontal && b.y + b.height == a.y {
            found.push(("up", id.as_str()));
        }
    }
    found
}

/// Путь фокуса от одной панели к другой по соседям: направления для `pane focus`.
fn focus_path(rects: &[(String, Rect)], from: &str, to: &str) -> Option<Vec<&'static str>> {
    let mut queue = VecDeque::from([(from.to_string(), Vec::new())]);
    let mut seen = HashSet::from([from.to_string()]);
    while let Some((pane, path)) = queue.pop_front() {
        if pane == to {
            return Some(path);
        }
        for (direction, neighbor) in pane_neighbors(rects, &pane) {
            if !seen.insert(neighbor.to_string()) {
                continue;
            }
            let mut next = path.clone();
            next.push(direction);
            queue.push_back((neighbor.to_string(), next));
        }
    }
    None
}

impl HerdrServer {
    pub fn new(id: &str, command: Vec<String>, run: Runner, detects_agents: bool) -> Self {
        Self { id: id.into(), detects_agents, command, run }
    }

    /// Подпись, если herdr не назвал агента; herdr в PowerShell агентов не распознаёт.
    fn no_agent(&self) -> String {
        crate::i18n::t(if self.detects_agents { "no_agent" } else { "unknown_agent" })
    }

    fn execute(&self, args: &[&str]) -> Result<Output> {
        let full: Vec<String> = self.command.iter().cloned().chain(args.iter().map(|a| a.to_string())).collect();
        (self.run)(&full).map_err(|e| SessionError::Other(format!("herdr ({}) не запустился: {e}", self.id)))
    }

    /// `pane read` выводит простой текст, а не JSON.
    fn read_pane(&self, pane_id: &str) -> Result<String> {
        let done = self.execute(&["pane", "read", pane_id, "--source", "visible"])?;
        if done.code != 0 {
            return error(format!("herdr ({}) не прочитал панель {pane_id}", self.id));
        }
        Ok(done.stdout)
    }

    fn call(&self, args: &[&str]) -> Result<Value> {
        let done = self.execute(args)?;
        let output = if done.stdout.trim().is_empty() { done.stderr.trim() } else { done.stdout.trim() };
        let data: Value = serde_json::from_str(output).unwrap_or(Value::Null);
        if let Some(err) = data.get("error") {
            let field = |name: &str| err.get(name).map(label_text).unwrap_or_else(|| "None".into());
            return error(format!("herdr ({}): {}: {}", self.id, field("code"), field("message")));
        }
        if done.code != 0 {
            let shown = if output.is_empty() { done.code.to_string() } else { output.to_string() };
            return error(format!("herdr ({}) {}: {shown}", self.id, args.join(" ")));
        }
        Ok(data.get("result").cloned().unwrap_or_else(|| json!({})))
    }

    pub fn snapshot(&self) -> Result<Value> {
        Ok(self.call(&["api", "snapshot"])?["snapshot"].clone())
    }

    pub fn panes(&self) -> Result<Vec<Value>> {
        Ok(self.call(&["pane", "list"])?["panes"].as_array().cloned().unwrap_or_default())
    }

    /// Все панели сервера с их видимым текстом.
    pub fn candidates(&self) -> Result<Vec<Candidate>> {
        let panes = self.panes()?;
        let mut texts: Vec<Result<String>> = Vec::with_capacity(panes.len());
        for chunk in panes.chunks(READ_WORKERS) {
            std::thread::scope(|scope| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|pane| scope.spawn(move || self.read_pane(pane["pane_id"].as_str().unwrap_or(""))))
                    .collect();
                texts.extend(handles.into_iter().map(|h| h.join().unwrap_or_else(|_| error("сбой чтения панели"))));
            });
        }
        panes
            .into_iter()
            .zip(texts)
            .map(|(pane, text)| Ok(Candidate { server_id: self.id.clone(), pane, lines: significant_lines(&text?) }))
            .collect()
    }

    /// Панель из списка или None, если её нет. `agent focus` отвечает agent_not_found
    /// и для удалённой панели, и для панели без агента, поэтому нужен список.
    pub fn pane(&self, pane_id: &str) -> Result<Option<Value>> {
        Ok(self.panes()?.into_iter().find(|p| p["pane_id"] == pane_id))
    }

    pub fn label_of(&self, workspace_id: &str, fallback: &str) -> Result<String> {
        let result = self.call(&["workspace", "list"])?;
        let found = result["workspaces"]
            .as_array()
            .and_then(|list| list.iter().find(|w| w["workspace_id"] == workspace_id))
            .map(|w| label_text(&w["label"]));
        Ok(found.unwrap_or_else(|| fallback.to_string()))
    }

    /// Показать панель в herdr. Сначала одной командой через агента; панель без агента
    /// (а на сервере, не распознающем агентов, — всегда) — через вкладку и соседние панели.
    /// Панели нет — Lost.
    pub fn focus_pane(&self, session: &mut Session) -> Result<()> {
        let id = str_of(session, "id").unwrap_or("").to_string();
        if self.detects_agents {
            match self.call(&["agent", "focus", &id]) {
                Ok(_) => return Ok(()),
                Err(e) if e.to_string().contains("agent_not_found") => {}
                Err(e) => return Err(e),
            }
        }
        let Some(pane) = self.pane(&id)? else {
            return Err(SessionError::Lost(format!("панели {id} больше нет в herdr ({})", self.id)));
        };
        session.extend(describe_pane(&pane, &self.no_agent()));
        fill_title(session);
        let pick = |field: &str, saved: &str| {
            pane[field].as_str().filter(|s| !s.is_empty()).or_else(|| str_of(session, saved)).unwrap_or("").to_string()
        };
        let (workspace, tab) = (pick("workspace_id", "workspace"), pick("tab_id", "tab"));
        self.call(&["workspace", "focus", &workspace])?;
        self.call(&["tab", "focus", &tab])?;
        // `tab focus` оставляет в фокусе ту панель вкладки, которая была в ней последней:
        // без распознавания агентов herdr сам нужную панель не выберет. Доводим фокус
        // по соседям, иначе рамка на телефоне останется у соседней сессии.
        let _ = self.focus_pane_exact(&id);
        Ok(())
    }

    /// Выбрать конкретную панель внутри вкладки, ведя фокус по соседним панелям.
    /// Ошибки не мешают показу вкладки: без точного фокуса она всё равно открыта.
    fn focus_pane_exact(&self, pane_id: &str) -> Result<()> {
        let mut layout = self.call(&["pane", "layout", "--pane", pane_id])?["layout"].clone();
        let mut focused = layout["focused_pane_id"].as_str().unwrap_or("").to_string();
        // Шагов не больше, чем панелей: защита от зацикливания, если herdr уведёт не туда.
        let limit = layout["panes"].as_array().map_or(0, Vec::len) + 1;
        for _ in 0..limit {
            if focused == pane_id {
                return Ok(());
            }
            let rects = pane_rects(&layout);
            let Some(direction) = focus_path(&rects, &focused, pane_id).and_then(|path| path.first().copied()) else {
                return Ok(());
            };
            let answer = self.call(&["pane", "focus", "--direction", direction])?;
            let focus = &answer["focus"];
            let Some(next) = focus["focused_pane_id"].as_str().filter(|next| *next != focused) else {
                return Ok(()); // фокус не сдвинулся — дальше некуда
            };
            focused = next.to_string();
            if focus["layout"].is_object() {
                layout = focus["layout"].clone();
            }
        }
        Ok(())
    }
}

/// Название — проект и роль, а без папки — имя сессии.
fn fill_title(session: &mut Session) {
    if !truthy(session.get("title")) {
        let name = session.get("name").cloned().unwrap_or(Value::Null);
        session.insert("title".into(), name);
    }
}

pub struct HerdrSessions {
    servers: Vec<HerdrServer>,
    terminal: Box<dyn Terminal>,
    relocation: Mutex<Option<Relocation>>,
}

impl HerdrSessions {
    pub fn new(servers: Vec<HerdrServer>, terminal: Box<dyn Terminal>) -> Self {
        Self { servers, terminal, relocation: Mutex::default() }
    }

    fn server(&self, id: Option<&str>) -> Option<&HerdrServer> {
        self.servers.iter().find(|s| Some(s.id.as_str()) == id)
    }

    pub fn current(&self) -> Result<Session> {
        let active = self.terminal.active_tab()?;
        let found = self.find_by_chrome(&active).or_else(|| self.find_by_text(&active));
        let Some((server_id, pane)) = found else {
            return error(
                "В выбранной вкладке терминала не удалось уверенно определить панель herdr: \
                 откройте вкладку с herdr; если боковая панель herdr свёрнута, \
                 разверните её или дождитесь текста на экране",
            );
        };
        let server = self.server(Some(&server_id)).unwrap();
        let pane_id = pane["pane_id"].as_str().unwrap_or("").to_string();
        let workspace = pane["workspace_id"].as_str().unwrap_or("").to_string();
        let mut session: Session = json!({
            "server": server_id,
            "id": pane_id,
            "tab": pane["tab_id"],
            "workspace": workspace,
            "name": server.label_of(&workspace, &pane_id)?,
            "terminal_window": active.window,
            "terminal_tab": active.tab,
        })
        .as_object()
        .unwrap()
        .clone();
        let fresh = server.pane(&pane_id)?.unwrap_or(pane);
        session.extend(describe_pane(&fresh, &server.no_agent()));
        fill_title(&mut session);
        Ok(session)
    }

    /// Панель в фокусе у herdr, показанного в этом окне терминала. Какой сервер в окне —
    /// по запомненным сессиям; если их серверов в окне несколько — по выбранной вкладке.
    pub fn focused(&self, window: i64, bound: &[Session]) -> Result<Option<Session>> {
        let mine: Vec<&Session> = bound
            .iter()
            .filter(|s| s.get("terminal_window").and_then(Value::as_i64) == Some(window))
            .filter(|s| self.server(str_of(s, "server")).is_some())
            .collect();
        let mut servers: BTreeSet<&str> = mine.iter().filter_map(|s| str_of(s, "server")).collect();
        if servers.len() > 1 {
            let tab = self.terminal.selected_tab(window)?;
            servers = mine
                .iter()
                .filter(|s| s.get("terminal_tab").and_then(Value::as_str) == tab.as_deref())
                .filter_map(|s| str_of(s, "server"))
                .collect();
        }
        if servers.len() != 1 {
            return Ok(None);
        }
        let server_id = servers.into_iter().next().unwrap();
        let snapshot = self.server(Some(server_id)).unwrap().snapshot()?;
        Ok(snapshot["focused_pane_id"]
            .as_str()
            .filter(|p| !p.is_empty())
            .map(|pane| json!({"kind": "herdr", "server": server_id, "id": pane}).as_object().unwrap().clone()))
    }

    /// Доступна ли сессия и что с её агентом. Недоступна — если нажатие кнопки её не покажет:
    /// нет сервера или панели, либо вкладки терминала с этим сервером нет. Если вкладка
    /// по запомненному адресу пропала, она ищется заново без переключения вкладок,
    /// и найденный адрес уходит в сведения о сессии.
    pub fn probe(&self, sessions: &[Session]) -> Vec<Probe> {
        let mut listed: HashMap<&str, Option<HashMap<String, Value>>> = HashMap::new();
        for session in sessions {
            let Some(server) = self.server(str_of(session, "server")) else { continue };
            listed.entry(server.id.as_str()).or_insert_with(|| {
                server.panes().ok().map(|panes| {
                    panes.into_iter().map(|p| (p["pane_id"].as_str().unwrap_or("").to_string(), p)).collect()
                })
            });
        }
        let pane_of = |session: &Session| -> Option<(&HerdrServer, &Value)> {
            let server = self.server(str_of(session, "server"))?;
            let pane = listed.get(server.id.as_str())?.as_ref()?.get(str_of(session, "id")?)?;
            session.contains_key("terminal_tab").then_some((server, pane))
        };
        let shown = self.shown_servers(sessions);
        let lost: BTreeSet<&str> = sessions
            .iter()
            .filter_map(|s| pane_of(s).map(|(server, _)| server.id.as_str()))
            .filter(|server| !shown.contains(server))
            .collect();
        let relocated = if lost.is_empty() {
            HashMap::new()
        } else {
            // Вкладки, которые и так известны как вкладки других серверов.
            let known: HashSet<(i64, Option<String>)> = sessions
                .iter()
                .filter(|s| str_of(s, "server").is_some_and(|server| shown.contains(server)))
                .map(|s| {
                    let window = s.get("terminal_window").and_then(Value::as_i64).unwrap_or(0);
                    (window, str_of(s, "terminal_tab").map(str::to_string))
                })
                .collect();
            self.relocate(&lost, &known)
        };
        sessions
            .iter()
            .map(|session| {
                let Some((server, pane)) = pane_of(session) else {
                    return Probe { available: Some(false), info: None };
                };
                let mut info = describe_pane(pane, &server.no_agent());
                if !truthy(info.get("title")) {
                    info.insert("title".into(), session.get("name").cloned().unwrap_or(Value::Null));
                }
                let available = shown.contains(server.id.as_str())
                    || match relocated.get(&server.id) {
                        Some(Located::At(window, tab)) => {
                            info.insert("terminal_window".into(), json!(window));
                            info.insert("terminal_tab".into(), json!(tab));
                            true
                        }
                        Some(Located::Hidden) => true,
                        None => false,
                    };
                Probe { available: Some(available), info: Some(info) }
            })
            .collect()
    }

    /// Найти вкладки серверов, чей запомненный адрес пропал, не переключая вкладок:
    /// прочитать можно только выбранные. `known` — вкладки других серверов.
    fn relocate(&self, lost: &BTreeSet<&str>, known: &HashSet<(i64, Option<String>)>) -> HashMap<String, Located> {
        let wanted: BTreeSet<String> = lost.iter().map(|s| s.to_string()).collect();
        let mut cache = self.relocation.lock().unwrap();
        if let Some(last) = cache.as_ref().filter(|r| r.at.elapsed() < RELOCATE_TTL && wanted.is_subset(&r.servers)) {
            return last.found.clone();
        }
        let found = self.search_selected_tabs(&wanted, known);
        *cache = Some(Relocation { at: Instant::now(), servers: wanted, found: found.clone() });
        found
    }

    fn search_selected_tabs(&self, wanted: &BTreeSet<String>, known: &HashSet<(i64, Option<String>)>) -> HashMap<String, Located> {
        let mut found = HashMap::new();
        let Ok(tabs) = self.terminal.list_tabs() else { return found };
        let snapshots = self.snapshots();
        let mut hidden = false;
        for tab in &tabs {
            let full_screen = tab.tab.is_none();
            if !full_screen && (!looks_like_herdr(&tab.title) || known.contains(&(tab.window, tab.tab.clone()))) {
                continue;
            }
            if !tab.selected {
                hidden = true;
                continue;
            }
            let Ok(screen) = self.terminal.read_selected(tab.window) else { continue };
            match self.shown_server(&screen, &snapshots) {
                Some(id) if wanted.contains(&id) => {
                    found.entry(id).or_insert(Located::At(screen.window, screen.tab.clone()));
                }
                Some(_) => {}
                // Полноэкранное окно показывает что-то другое; вкладка herdr без узнаваемого
                // интерфейса — может быть любым сервером.
                None => hidden |= !full_screen,
            }
        }
        if hidden {
            for server in wanted {
                found.entry(server.clone()).or_insert(Located::Hidden);
            }
        }
        found
    }

    /// Серверы, вкладка которых ещё открыта в терминале, — по вкладкам запомненных сессий.
    /// Вкладку, которую никто не запоминал, искать здесь слишком дорого: это переключение вкладок.
    fn shown_servers<'a>(&self, sessions: &'a [Session]) -> HashSet<&'a str> {
        let mut shown = HashSet::new();
        let mut tabs_of: HashMap<i64, Option<Vec<String>>> = HashMap::new();
        for session in sessions {
            let Some(server) = str_of(session, "server") else { continue };
            let window = session.get("terminal_window").and_then(Value::as_i64).unwrap_or(0);
            if window == 0 || shown.contains(server) {
                continue;
            }
            // Не прочиталось — считать, что вкладка есть.
            let tabs = tabs_of.entry(window).or_insert_with(|| self.terminal.window_tabs(window).unwrap_or(Some(vec![])));
            let tab = session.get("terminal_tab").and_then(Value::as_str);
            if let Some(tabs) = tabs {
                if tab.is_none() || tabs.is_empty() || tabs.iter().any(|t| Some(t.as_str()) == tab) {
                    shown.insert(server);
                }
            }
        }
        shown
    }

    fn snapshots(&self) -> Vec<(String, Value)> {
        // Остановленный сервер не мешает другому.
        self.servers.iter().filter_map(|s| s.snapshot().ok().map(|snap| (s.id.clone(), snap))).collect()
    }

    fn find_by_chrome(&self, active: &TerminalTab) -> Option<(String, Value)> {
        find_pane_by_chrome(active.chrome.as_ref(), &self.snapshots())
    }

    fn find_by_text(&self, active: &TerminalTab) -> Option<(String, Value)> {
        let candidates: Vec<Candidate> = self.servers.iter().filter_map(|s| s.candidates().ok()).flatten().collect();
        find_visible_pane(&active.text, &candidates).map(|c| (c.server_id.clone(), c.pane.clone()))
    }

    pub fn open(&self, session: &mut Session) -> Result<()> {
        let server = self.server(str_of(session, "server"));
        let Some(server) = server.filter(|_| session.contains_key("terminal_tab")) else {
            return Err(SessionError::Lost("запись о сессии устарела".into()));
        };
        // Фокус в herdr и вкладка терминала друг от друга не зависят: одновременно.
        let mut focus_copy = session.clone();
        let (focused, shown) = std::thread::scope(|scope| {
            let handle = scope.spawn(|| server.focus_pane(&mut focus_copy));
            let shown = self.show_terminal(session);
            (handle.join().unwrap_or_else(|_| error("сбой фокуса herdr")), shown)
        });
        for (name, value) in focus_copy {
            if name != "terminal_window" && name != "terminal_tab" {
                session.insert(name, value);
            }
        }
        // Панели нет — важнее, чем ошибка вкладки.
        focused.and(shown)
    }

    fn show_terminal(&self, session: &mut Session) -> Result<()> {
        let window = session.get("terminal_window").and_then(Value::as_i64).unwrap_or(0);
        let tab = session.get("terminal_tab").and_then(Value::as_str).map(str::to_string);
        if let Some(tab) = &tab {
            match self.terminal.show_tab(window, Some(tab)) {
                Ok(()) => return Ok(()),
                // Вкладку перенесли в другое окно или окно развернули на весь экран.
                Err(SessionError::TabNotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        let server = str_of(session, "server").unwrap_or("").to_string();
        let found = self.locate_server_tab(&server, window)?;
        self.terminal.show_tab(found.window, found.tab.as_deref())?;
        // Адрес вкладки сохраняется, пока окно на весь экран: после выхода он снова пригодится.
        if found.tab.is_some() || found.window != window {
            session.insert("terminal_window".into(), json!(found.window));
            session.insert("terminal_tab".into(), json!(found.tab));
        }
        Ok(())
    }

    /// Какой сервер herdr показан на выбранной вкладке.
    fn shown_server(&self, tab: &TerminalTab, snapshots: &[(String, Value)]) -> Option<String> {
        let mut found = find_pane_by_chrome(tab.chrome.as_ref(), snapshots);
        if found.is_none() && tab.chrome.is_some() {
            found = self.find_by_text(tab); // интерфейс herdr есть, но распознать его не вышло
        }
        found.map(|(id, _)| id)
    }

    /// Найти вкладку с нужным herdr. Прочитать можно только выбранную вкладку окна,
    /// поэтому сначала проверяются выбранные, а остальные выбираются по очереди.
    fn locate_server_tab(&self, server_id: &str, preferred_window: i64) -> Result<TabRef> {
        let mut tabs = self.terminal.list_tabs()?;
        let snapshots = self.snapshots();
        let originally: HashMap<i64, Option<String>> =
            tabs.iter().filter(|t| t.selected).map(|t| (t.window, t.tab.clone())).collect();
        tabs.sort_by_key(|t| (!t.selected, t.window != preferred_window));
        let mut found = None;
        let mut switched = BTreeSet::new();
        let mut failure = None;
        for (index, tab) in tabs.iter().enumerate() {
            if !tab.selected {
                if let Err(e) = self.terminal.select_tab(tab.window, tab.tab.as_deref()) {
                    failure = Some(e);
                    break;
                }
                switched.insert(tab.window);
            }
            match self.terminal.read_selected(tab.window) {
                Ok(screen) if self.shown_server(&screen, &snapshots).as_deref() == Some(server_id) => {
                    found = Some(index);
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        let found_window = found.map(|i| tabs[i].window);
        for window in switched {
            if found_window != Some(window) {
                if let Some(tab) = originally.get(&window) {
                    let _ = self.terminal.select_tab(window, tab.as_deref());
                }
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
        match found {
            Some(index) => Ok(tabs.swap_remove(index)),
            None if tabs.iter().any(|t| t.tab.is_none()) => error(format!(
                "Вкладка с herdr ({server_id}) не найдена: окно на весь экран показывает \
                 другую вкладку и скрывает остальные. Переключитесь на вкладку с herdr \
                 или выйдите из полноэкранного режима"
            )),
            None => error(format!("Не найдена вкладка терминала с herdr ({server_id})")),
        }
    }
}

/// Один вид сессий: herdr в терминале, окна приложений и т.д.
pub trait Backend: Send + Sync {
    /// Окно этого вида.
    fn matches(&self, window: i64) -> bool;
    /// Сессия, в которой пользователь был последний раз.
    fn current(&self) -> Result<Session>;
    fn open(&self, session: &mut Session) -> Result<()>;
    /// Состояние сессий по порядку; None — вид не умеет проверять.
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>>;
    /// Сессия в окне `window`, на котором фокус; ищется среди запомненных этого вида.
    fn focused(&self, window: i64, bound: &[Session]) -> Option<Session>;
}

impl<T: Backend + ?Sized> Backend for std::sync::Arc<T> {
    fn matches(&self, window: i64) -> bool {
        (**self).matches(window)
    }
    fn current(&self) -> Result<Session> {
        (**self).current()
    }
    fn open(&self, session: &mut Session) -> Result<()> {
        (**self).open(session)
    }
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        (**self).probe(sessions)
    }
    fn focused(&self, window: i64, bound: &[Session]) -> Option<Session> {
        (**self).focused(window, bound)
    }
}

impl Backend for HerdrSessions {
    fn matches(&self, window: i64) -> bool {
        self.terminal.is_terminal(window)
    }
    fn current(&self) -> Result<Session> {
        HerdrSessions::current(self)
    }
    fn open(&self, session: &mut Session) -> Result<()> {
        HerdrSessions::open(self, session)
    }
    fn probe(&self, sessions: &[Session]) -> Option<Vec<Probe>> {
        Some(HerdrSessions::probe(self, sessions))
    }
    fn focused(&self, window: i64, bound: &[Session]) -> Option<Session> {
        HerdrSessions::focused(self, window, bound).ok().flatten()
    }
}

/// Окна рабочего стола, которые нужны выбору вида: от последнего активного и окно в фокусе.
pub trait Screen: Send + Sync {
    fn top_level_windows(&self) -> Vec<i64>;
    fn foreground_window(&self) -> i64;
}

/// Несколько видов сессий (перенос `SessionRouter`). Текущая сессия берётся из приложения,
/// активного последним; запомненная открывается тем видом, которым её запомнили.
/// У записей без поля `kind` вид — herdr.
pub struct Router {
    /// Виды по порядку проверки окна: последним — окно любого приложения.
    backends: Vec<(&'static str, Box<dyn Backend>)>,
    screen: Box<dyn Screen>,
}

impl Router {
    pub fn new(backends: Vec<(&'static str, Box<dyn Backend>)>, screen: Box<dyn Screen>) -> Self {
        Self { backends, screen }
    }

    fn backend(&self, kind: &str) -> Option<&dyn Backend> {
        self.backends.iter().find(|(k, _)| *k == kind).map(|(_, b)| b.as_ref())
    }

    /// Вид окна, активного последним среди известных.
    fn last_active(&self) -> Option<&'static str> {
        self.screen
            .top_level_windows()
            .into_iter()
            .find_map(|window| self.backends.iter().find(|(_, b)| b.matches(window)).map(|(k, _)| *k))
    }
}

impl Sessions for Router {
    fn current(&self) -> Result<Session> {
        let kind = self.last_active().unwrap_or("herdr");
        let Some(backend) = self.backend(kind) else { return error("вид сессии «herdr» не настроен") };
        let mut session = backend.current()?;
        session.entry("kind").or_insert_with(|| json!(kind));
        Ok(session)
    }

    fn open(&self, session: &mut Session) -> Result<()> {
        let kind = kind_of(session).to_string();
        match self.backend(&kind) {
            Some(backend) => backend.open(session),
            None => error(format!("сессии вида «{kind}» Rust-версия пока не открывает")),
        }
    }

    /// Вид, который не умеет проверять, оставляет состояние неизвестным; неизвестный вид —
    /// недоступен (нажатие его не откроет).
    fn probe(&self, sessions: &[Session]) -> Vec<Probe> {
        let mut results = vec![Probe::default(); sessions.len()];
        let mut by_kind: Vec<(String, Vec<usize>)> = Vec::new();
        for (index, session) in sessions.iter().enumerate() {
            let kind = kind_of(session);
            match by_kind.iter_mut().find(|(k, _)| k == kind) {
                Some((_, indexes)) => indexes.push(index),
                None => by_kind.push((kind.to_string(), vec![index])),
            }
        }
        for (kind, indexes) in by_kind {
            let Some(backend) = self.backend(&kind) else {
                for index in indexes {
                    results[index].available = Some(false);
                }
                continue;
            };
            let mine: Vec<Session> = indexes.iter().map(|&i| sessions[i].clone()).collect();
            if let Some(found) = backend.probe(&mine) {
                for (index, probe) in indexes.into_iter().zip(found) {
                    results[index] = probe;
                }
            }
        }
        results
    }

    fn focused(&self, bound: &[Session]) -> Option<Session> {
        let window = self.screen.foreground_window();
        if window == 0 {
            return None;
        }
        let (kind, backend) = self.backends.iter().find(|(_, b)| b.matches(window))?;
        let mine: Vec<Session> = bound.iter().filter(|s| kind_of(s) == *kind).cloned().collect();
        if mine.is_empty() {
            return None;
        }
        backend.focused(window, &mine)
    }
}

#[cfg(test)]
mod tests;
