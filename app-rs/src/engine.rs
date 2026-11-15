//! Связывает панель, жесты, сценарии и действия (перенос `engine.py`).
//! Живёт в одном потоке; время передаётся снаружи, поэтому тесты не ждут таймеров.

use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use crate::actions::{self, Context};
use crate::gestures::{Emitted, Gesture, Handlers, Recognizer};
use crate::keystrokes;
use crate::look::{self, Look};
use crate::scenarios::Scenario;
use crate::sessions::{Probe, Session, Sessions, same_session};
use crate::state::State;
use crate::template::render;

/// Куда движок показывает вид кнопок: панель на телефоне или в браузере.
pub trait Device: Send + Sync {
    fn key_count(&self) -> usize;
    fn set_label(&self, key: usize, label: &str);
    /// Иконка по имени (обычно роль); пустое имя — без иконки. title — надпись над иконкой.
    fn set_icon(&self, key: usize, icon: &str, title: &str);
    /// Кнопка, чья сессия сейчас в фокусе на экране.
    fn set_active(&self, key: usize, active: bool);
    /// Состояние агента, покажет ли нажатие сессию, есть ли у кнопки сессия.
    fn set_state(&self, key: usize, status: Option<&str>, available: bool, bound: bool);
    /// Диалог подтверждения; новый вопрос заменяет прежний.
    fn ask_confirm(&self, key: usize, text: &str, state: Confirm);
    fn close_confirm(&self);
    /// Короткое уведомление на панели (например, запомнить не удалось).
    fn notify(&self, _text: &str) {}
    /// Показывать ли надпись над иконкой (роль).
    fn set_titles_visible(&self, _visible: bool) {}
    /// Сменился язык: заново отправить раскладку и кнопки.
    fn relayout(&self) {}
    /// Текущий профиль кнопок (с нуля) из `count`.
    fn set_profile(&self, _profile: usize, _count: usize) {}
    /// Адрес картинки иконки по имени — для миниатюр профилей на странице управления.
    fn icon_url(&self, _name: &str) -> String {
        String::new()
    }
    /// Свой вид кнопки: текстовая иконка, цвет плашки, мелкая строка под подписью
    /// (сочетание у кнопки-сочетания со своей подписью); пустое — нет.
    fn set_look(&self, _key: usize, _look: &KeyLook) {}
}

/// Состояние диалога подтверждения.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Confirm {
    /// Сессия ещё читается: «Запомнить» уже можно нажать.
    Reading,
    /// Сессия известна.
    Ready,
    /// Прочитать не удалось: только «ОК».
    Failed,
}

impl Confirm {
    pub fn name(self) -> &'static str {
        match self {
            Confirm::Reading => "reading",
            Confirm::Ready => "ready",
            Confirm::Failed => "failed",
        }
    }
}

/// Предложение запомнить сессию на кнопке. Сессия читается в фоне (`id` — номер чтения),
/// поэтому диалог показывается сразу, а согласие может прийти раньше сессии.
struct Proposal {
    key: usize,
    id: u64,
    session: Option<Session>,
    /// Пользователь уже согласился — запомнить, как только сессия прочитается.
    accepted: bool,
    /// Текст ошибки чтения: диалог показывает её.
    failed: Option<String>,
}

/// Вид кнопки сверх подписи и иконки — то, что показывает панель.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyLook {
    pub icon_text: String,
    pub color: String,
    pub hint: String,
}

/// Настройки из state.json, которые меняет страница управления.
pub const CONFIRM_BIND: &str = "confirm_bind";
pub const ICON_TITLES: &str = "icon_project";
/// Панель на телефоне (true) или на самой странице управления.
pub const PHONE_PANEL: &str = "phone_panel";
/// Профили кнопок включены; выключено — один набор, как раньше (без кнопок 1–6 и свайпа).
pub const PROFILES_ENABLED: &str = "profiles_enabled";

pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

type ShownState = (Option<String>, bool, bool);

pub struct Engine {
    scenarios: BTreeMap<String, Scenario>,
    default_scenario: Option<String>,
    state: State,
    sessions: Arc<dyn Sessions>,
    device: Arc<dyn Device>,
    log: Log,
    gestures: Recognizer,
    /// Пороги жестов — новый распознаватель при смене профиля.
    timings: (u64, u64),
    active: BTreeSet<usize>,
    focused: Option<Session>,
    /// Последний ответ опроса; нет ответа — доступна.
    available: HashMap<usize, bool>,
    /// Доступность кнопок неактивных профилей: при возврате к профилю закрытая кнопка
    /// сразу серая, а не «открытая» до следующего опроса.
    profile_available: HashMap<usize, HashMap<usize, bool>>,
    shown_states: HashMap<usize, ShownState>,
    /// Запоминать сессию только после подтверждения в диалоге.
    pub confirm_bind: bool,
    /// Ждёт подтверждения.
    proposal: Option<Proposal>,
    /// Номер последнего фонового чтения сессии.
    last_read: u64,
    /// Чтения, которые надо запустить в фоне (номера).
    reads: Vec<u64>,
    /// После жеста — сразу проверить фокус, не дожидаясь опроса.
    focus_requested: bool,
}

/// Что панель показывает сверх подписи и иконки. Сочетание мелко под подписью — только
/// у кнопки-сочетания со своей подписью (без подписи сочетание и есть подпись).
fn key_look(scenario: Option<&str>, values: &Map<String, Value>) -> KeyLook {
    let look = Look::of(values);
    let combo = (scenario == Some(keystrokes::SCENARIO))
        .then(|| values.get(keystrokes::VALUE).and_then(Value::as_str))
        .flatten()
        .filter(|_| look.caption.is_some());
    KeyLook {
        icon_text: look.icon_text().unwrap_or_default().to_string(),
        color: look.color.clone().unwrap_or_default(),
        hint: combo.unwrap_or_default().to_string(),
    }
}

/// Иконка кнопки: своя (значок библиотеки или текст) заменяет автоматическую.
fn shown_icon(icon: String, values: &Map<String, Value>) -> String {
    let look = Look::of(values);
    match (look.custom_icon(), look.icon_text()) {
        (Some(name), _) => name.to_string(),
        (None, Some(_)) => String::new(),
        // Окно, у которого не узнали программу (например, нет доступа к процессу), — общая иконка окна.
        _ if icon.is_empty() && is_window(values) => crate::windows::WINDOW_ICON.to_string(),
        _ => icon,
    }
}

fn is_window(values: &Map<String, Value>) -> bool {
    values
        .get("session")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == crate::windows::KIND)
}

/// Что движок знает о сценариях кнопок — для распознавателя жестов.
struct Lookup<'a> {
    scenarios: &'a BTreeMap<String, Scenario>,
    default_scenario: Option<&'a str>,
    state: &'a State,
}

impl Lookup<'_> {
    fn scenario(&self, key: usize) -> Option<&Scenario> {
        let id = self.state.scenario_of(key).or(self.default_scenario)?;
        self.scenarios.get(id)
    }
}

impl Handlers for Lookup<'_> {
    fn handles(&self, key: usize, gesture: Gesture) -> bool {
        self.scenario(key).is_some_and(|s| s.on.contains_key(&gesture))
    }
    fn eager(&self, key: usize) -> bool {
        self.scenario(key).is_some_and(|s| s.eager_click)
    }
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        device: Arc<dyn Device>,
        scenarios: BTreeMap<String, Scenario>,
        state: State,
        sessions: Arc<dyn Sessions>,
        log: Log,
        double_click_ms: u64,
        hold_ms: u64,
        default_scenario: Option<String>,
    ) -> Self {
        Self {
            scenarios,
            default_scenario,
            state,
            sessions,
            device,
            log,
            gestures: Recognizer::new(double_click_ms, hold_ms),
            timings: (double_click_ms, hold_ms),
            active: BTreeSet::new(),
            focused: None,
            available: HashMap::new(),
            profile_available: HashMap::new(),
            shown_states: HashMap::new(),
            confirm_bind: false,
            proposal: None,
            last_read: 0,
            reads: Vec::new(),
            focus_requested: false,
        }
    }

    fn scenario_of(&self, key: usize) -> Option<&Scenario> {
        let id = self.state.scenario_of(key).or(self.default_scenario.as_deref())?;
        self.scenarios.get(id)
    }

    fn key_count(&self) -> usize {
        self.device.key_count()
    }

    fn save(&self) {
        if let Err(error) = self.state.save() {
            (self.log)(&format!("Не удалось сохранить состояние кнопок: {error}"));
        }
    }

    /// Выбрать кнопке сценарий (None — сценарий по умолчанию).
    pub fn assign(&mut self, key: usize, scenario: Option<&str>) {
        if key >= self.key_count() || scenario.is_some_and(|id| !self.scenarios.contains_key(id)) {
            return;
        }
        self.state.set_scenario(key, scenario);
        self.save();
        self.render_key(key);
        self.show_focused(self.focused.clone());
    }

    /// Забыть сессию и другие значения кнопки; кнопка-сочетание становится обычной.
    /// Своя иконка сбрасывается (кнопка без назначения), подпись и цвет остаются;
    /// всё вместе убирает «Сбросить вид».
    pub fn clear(&mut self, key: usize) {
        if key >= self.key_count() {
            return;
        }
        let values = self.state.values_mut(key);
        look::keep_only(values);
        look::reset_icon(values);
        if self.state.scenario_of(key) == Some(keystrokes::SCENARIO) {
            self.state.set_scenario(key, None);
        }
        self.available.remove(&key);
        if self.proposal_key() == Some(key) {
            self.proposal = None;
            self.device.close_confirm();
        }
        self.save();
        (self.log)(&format!("Кнопка {}: привязка очищена", key + 1));
        self.render_key(key);
        self.show_focused(self.focused.clone());
    }

    /// Сменить язык: настройка (`auto` или код) и сценарии, уже прочитанные на новом языке.
    pub fn set_language(&mut self, setting: &str, scenarios: BTreeMap<String, Scenario>) {
        self.state.set_setting(crate::i18n::SETTING, json!(setting));
        self.save();
        self.scenarios = scenarios;
        self.show_proposal();
        self.device.relayout();
        self.render_all();
    }

    /// Сделать кнопку кнопкой-сочетанием: нажатие нажимает `combo` в активном окне
    /// (прежняя сессия кнопки забывается). None — снова обычная пустая кнопка.
    pub fn set_keys(&mut self, key: usize, combo: Option<&str>) -> Result<(), String> {
        let Some(combo) = combo else {
            self.clear(key);
            return Ok(());
        };
        if key >= self.key_count() {
            return Err("нет такой кнопки".into());
        }
        if !self.scenarios.contains_key(keystrokes::SCENARIO) {
            return Err(format!("нет сценария «{}»", keystrokes::SCENARIO));
        }
        keystrokes::parse(combo)?;
        if self.proposal_key() == Some(key) {
            self.cancel_proposal();
        }
        let values = self.state.values_mut(key);
        look::keep_only(values);
        values.insert(keystrokes::VALUE.into(), json!(combo));
        self.state.set_scenario(key, Some(keystrokes::SCENARIO));
        self.available.remove(&key);
        self.save();
        (self.log)(&format!("Кнопка {}: нажимает сочетание {combo}", key + 1));
        self.render_key(key);
        self.show_focused(self.focused.clone());
        Ok(())
    }

    /// Свой вид кнопки (подпись, иконка, цвет); пустой — автоматический («Сбросить вид»).
    pub fn set_look(&mut self, key: usize, look: Look) {
        if key >= self.key_count() {
            return;
        }
        look.store(self.state.values_mut(key));
        self.save();
        let reset = look == Look::default();
        (self.log)(&format!("Кнопка {}: {}", key + 1, if reset { "вид сброшен" } else { "вид изменён" }));
        self.render_key(key);
    }

    /// Кнопка-сочетание запомнила сессию (удержанием): теперь это кнопка сессии —
    /// сочетание забывается, сценарий — по умолчанию (или «auto», если его нет).
    fn settle_keys(&mut self, key: usize) {
        if self.state.scenario_of(key) != Some(keystrokes::SCENARIO) || self.session_of(key).is_none() {
            return;
        }
        let fallback = (self.default_scenario.is_none() && self.scenarios.contains_key("auto")).then_some("auto");
        self.state.values_mut(key).remove(keystrokes::VALUE);
        self.state.set_scenario(key, fallback);
        (self.log)(&format!("Кнопка {}: сочетание заменено сессией", key + 1));
    }

    /// Сочетание кнопки-сочетания.
    fn keys_of(&self, key: usize) -> Option<&str> {
        (self.state.scenario_of(key) == Some(keystrokes::SCENARIO))
            .then(|| self.state.values(key).get(keystrokes::VALUE).and_then(Value::as_str))
            .flatten()
            .filter(|c| !c.is_empty())
    }

    /// Сделать профиль кнопок текущим: панель показывает его кнопки, нажатия идут им.
    pub fn switch_profile(&mut self, profile: usize) {
        if self.proposal.is_some() {
            self.cancel_proposal(); // вопрос относился к кнопке прежнего профиля
        }
        let previous = self.state.profile();
        if !self.profiles_enabled() || !self.state.switch_profile(profile) {
            self.announce_profile();
            return;
        }
        // Незаконченные жесты относились к кнопкам прежнего профиля.
        self.gestures = Recognizer::new(self.timings.0, self.timings.1);
        let restored = self.profile_available.remove(&profile).unwrap_or_default();
        self.profile_available.insert(previous, std::mem::replace(&mut self.available, restored));
        self.save();
        self.render_all();
        self.show_focused(self.focused.clone());
        self.focus_requested = true;
        (self.log)(&format!("Профиль кнопок {}", profile + 1));
    }

    /// Миниатюра профиля: подпись, иконка и есть ли привязка у каждой кнопки.
    fn profile_preview(&self, profile: usize) -> Vec<Value> {
        let keys = self.state.profile_keys(profile);
        (0..self.key_count())
            .map(|key| {
                let entry = keys.get(&key.to_string());
                let empty = Map::new();
                let values = entry.and_then(|e| e.get("values")).and_then(Value::as_object).unwrap_or(&empty);
                let id = entry.and_then(|e| e.get("scenario")).and_then(Value::as_str).filter(|s| !s.is_empty());
                let scenario = id.or(self.default_scenario.as_deref()).and_then(|id| self.scenarios.get(id));
                let (label, icon) = scenario.map_or_else(Default::default, |s| {
                    (render(&s.label, values).trim().to_string(), render(&s.icon, values).trim().to_string())
                });
                let bound = ["session", keystrokes::VALUE].iter().any(|name| match values.get(*name) {
                    Some(Value::Object(o)) => !o.is_empty(),
                    Some(Value::String(t)) => !t.is_empty(),
                    _ => false,
                });
                let look = key_look(id, values);
                json!({
                    "label": label, "icon": self.device.icon_url(&shown_icon(icon, values)), "bound": bound,
                    "icon_text": look.icon_text, "color": look.color, "hint": look.hint,
                    // Свой вид — плитка не пустая, даже без сессии.
                    "styled": Look::of(values) != Look::default(),
                })
            })
            .collect()
    }

    /// Флажок со страницы управления; false — такой настройки нет.
    pub fn set_setting(&mut self, name: &str, value: bool) -> bool {
        match name {
            CONFIRM_BIND => self.confirm_bind = value,
            ICON_TITLES => self.device.set_titles_visible(value),
            PHONE_PANEL | PROFILES_ENABLED => {}
            _ => return false,
        }
        self.state.set_setting(name, json!(value));
        self.save();
        if name == PROFILES_ENABLED {
            // Кнопки текущего профиля остаются; остальные профили хранятся до включения.
            self.announce_profile();
        }
        true
    }

    fn profiles_enabled(&self) -> bool {
        self.state.setting(PROFILES_ENABLED).as_bool().unwrap_or(false)
    }

    /// Панели — текущий профиль; при выключенных профилях их 0 (панель без кнопок 1–6 и свайпа).
    fn announce_profile(&self) {
        let count = if self.profiles_enabled() { crate::state::PROFILES } else { 0 };
        self.device.set_profile(self.state.profile(), count);
    }

    /// Кнопки, сценарии и настройки — для страницы управления.
    pub fn overview(&self) -> Value {
        let keys: Vec<Value> = (0..self.key_count())
            .map(|key| {
                let session = self.session_of(key);
                let text = |name: &str| session.and_then(|s| s.get(name)).and_then(Value::as_str).filter(|t| !t.is_empty());
                let values = self.state.values(key);
                let label = self.scenario_of(key).map(|s| render(&s.label, values)).unwrap_or_default();
                // Автоматический вид — для превью в окне «Настроить кнопку».
                let mut plain = values.clone();
                plain.remove(look::CAPTION);
                let scenario = self.scenario_of(key);
                let auto_label = scenario.map(|s| render(&s.label, &plain)).unwrap_or_default();
                let auto_icon = scenario.map(|s| render(&s.icon, values).trim().to_string()).unwrap_or_default();
                json!({
                    "look": Look::of(values).to_json(),
                    "auto_label": auto_label.trim(),
                    "auto_icon": self.device.icon_url(&auto_icon),
                    // Своя иконка-картинка — для превью при открытии окна.
                    "custom_icon": Look::of(values).custom_icon().map(|icon| self.device.icon_url(icon)),
                    "key": key,
                    "scenario": self.state.scenario_of(key),
                    "label": label.trim(),
                    "session": text("title").or(text("name")).map(|t| t.replace('|', " · ")),
                    "app": match text("kind").unwrap_or("herdr") {
                        "herdr" => text("server").map(|server| format!("herdr · {server}")),
                        _ => text("app").or(text("server")).map(str::to_string),
                    },
                    "bound": session.is_some() || self.keys_of(key).is_some(),
                    "available": self.available.get(&key).copied().unwrap_or(true),
                    "keys": self.keys_of(key),
                })
            })
            .collect();
        let scenarios: Vec<Value> = self.scenarios.iter().map(|(id, s)| json!({"id": id, "name": s.name})).collect();
        let flag = |name: &str, default: bool| self.state.setting(name).as_bool().unwrap_or(default);
        let profiles: Option<Vec<Value>> = self
            .profiles_enabled()
            .then(|| (0..crate::state::PROFILES).map(|p| json!(self.profile_preview(p))).collect());
        json!({
            "keys": keys,
            "profile": self.state.profile(),
            "profiles": profiles,
            "scenarios": scenarios,
            "default_scenario": self.default_scenario,
            "settings": {
                CONFIRM_BIND: self.confirm_bind,
                ICON_TITLES: flag(ICON_TITLES, false),
                PHONE_PANEL: flag(PHONE_PANEL, true),
                PROFILES_ENABLED: self.profiles_enabled(),
                crate::i18n::SETTING: crate::i18n::current(),
            },
        })
    }

    pub fn render_all(&mut self) {
        self.announce_profile();
        for key in 0..self.key_count() {
            self.render_key(key);
        }
    }

    fn render_key(&mut self, key: usize) {
        let values = self.state.values(key);
        let (label, icon, title) = match self.scenario_of(key) {
            None => Default::default(),
            Some(scenario) => (
                render(&scenario.label, values),
                render(&scenario.icon, values).trim().to_string(),
                render(&scenario.icon_title, values).trim().to_string(),
            ),
        };
        let look = key_look(self.state.scenario_of(key), values);
        self.device.set_look(key, &look);
        self.device.set_label(key, &label);
        self.device.set_icon(key, &shown_icon(icon, values), &title);
        self.show_state(key);
    }

    fn session_of(&self, key: usize) -> Option<&Session> {
        self.state.values(key).get("session").and_then(Value::as_object).filter(|s| !s.is_empty())
    }

    /// Состояние агента и доступность сессии — устройству, только когда они изменились.
    fn show_state(&mut self, key: usize) {
        let session = self.scenario_of(key).and(self.session_of(key));
        let state: ShownState = match session {
            Some(session) => (
                session.get("agent_status").and_then(Value::as_str).map(str::to_string),
                self.available.get(&key).copied().unwrap_or(true),
                true,
            ),
            None => (None, true, self.keys_of(key).is_some()),
        };
        if self.shown_states.get(&key) != Some(&state) {
            self.device.set_state(key, state.0.as_deref(), state.1, state.2);
            self.shown_states.insert(key, state);
        }
    }

    // --- нажатия и время

    pub fn on_raw(&mut self, key: usize, pressed: bool, now: Instant) {
        if key >= self.key_count() {
            return;
        }
        let lookup = Lookup { scenarios: &self.scenarios, default_scenario: self.default_scenario.as_deref(), state: &self.state };
        let emitted = self.gestures.on_raw(key, pressed, now, &lookup);
        self.run_gestures(emitted);
    }

    pub fn tick(&mut self, now: Instant) {
        let emitted = self.gestures.tick(now);
        self.run_gestures(emitted);
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.gestures.next_deadline()
    }

    /// Проверить фокус сейчас: после жеста сессия, скорее всего, сменилась.
    pub fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_requested)
    }

    fn run_gestures(&mut self, emitted: Emitted) {
        for (key, gesture) in emitted {
            self.on_gesture(key, gesture);
        }
    }

    /// Кнопку начали перетаскивать: удержание не считается, сессия не перезаписывается.
    pub fn start_drag(&mut self, key: usize, now: Instant) {
        if key >= self.key_count() {
            return;
        }
        self.tick(now);
        let emitted = self.gestures.cancel(key);
        self.run_gestures(emitted);
    }

    /// Перетаскивание закончено: на другой кнопке — поменять их местами.
    pub fn drop(&mut self, key: usize, target: Option<usize>, now: Instant) {
        self.on_raw(key, false, now); // отпускание после отмены жестов не даёт
        let count = self.key_count();
        let Some(target) = target.filter(|&t| t != key && t < count && key < count) else { return };
        if self.proposal_key().is_some_and(|k| k == key || k == target) {
            self.cancel_proposal(); // кнопка переехала: предложение относилось к прежнему месту
        }
        self.state.swap(key, target);
        self.save();
        let (first, second) = (self.available.remove(&key), self.available.remove(&target));
        if let Some(value) = first {
            self.available.insert(target, value);
        }
        if let Some(value) = second {
            self.available.insert(key, value);
        }
        self.render_key(key);
        self.render_key(target);
        self.show_focused(self.focused.clone());
        (self.log)(&format!("Кнопки {} и {} поменялись местами", key + 1, target + 1));
    }

    /// Ответ в диалоге подтверждения: запомнить предложенную сессию или нет.
    /// Согласие до того, как сессия прочитана, — диалог закрывается, сессия запомнится по приходу.
    pub fn answer(&mut self, accepted: bool) {
        let Some(mut proposal) = self.proposal.take() else { return };
        if proposal.accepted {
            self.proposal = Some(proposal); // диалог уже закрыт, ждём сессию
            return;
        }
        self.device.close_confirm();
        let key = proposal.key;
        if proposal.failed.is_some() {
            return; // «ОК» под ошибкой
        }
        if !accepted || self.scenario_of(key).is_none() {
            (self.log)(&format!("Кнопка {}: запоминание сессии отменено", key + 1));
            return;
        }
        match proposal.session.take() {
            Some(session) => self.bind_proposed(key, session),
            None => {
                (self.log)(&format!("Кнопка {}: сессия запомнится, как только будет определена", key + 1));
                proposal.accepted = true;
                self.proposal = Some(proposal);
            }
        }
    }

    fn bind_proposed(&mut self, key: usize, session: Session) {
        if self.scenario_of(key).is_none() {
            return;
        }
        let log = self.log.clone();
        actions::bind(self.state.values_mut(key), key, session, log.as_ref());
        self.settle_keys(key);
        self.available.remove(&key);
        self.save();
        self.render_key(key);
        self.show_focused(self.focused.clone());
    }

    /// Отменить предложение, даже если согласие уже дано и сессия ещё читается.
    fn cancel_proposal(&mut self) {
        let Some(proposal) = self.proposal.take() else { return };
        if !proposal.accepted {
            self.device.close_confirm();
        }
        (self.log)(&format!("Кнопка {}: запоминание сессии отменено", proposal.key + 1));
    }

    fn proposal_key(&self) -> Option<usize> {
        self.proposal.as_ref().map(|p| p.key)
    }

    /// Диалог по состоянию предложения; согласие уже дано — диалога нет.
    fn show_proposal(&self) {
        let Some(proposal) = self.proposal.as_ref().filter(|p| !p.accepted) else { return };
        let key = proposal.key;
        let (text, state) = match (&proposal.session, &proposal.failed) {
            (_, Some(error)) => (crate::i18n::tf("bind_failed", &[&(key + 1), error]), Confirm::Failed),
            (Some(session), None) => (actions::describe_proposal(key, session), Confirm::Ready),
            (None, None) => (crate::i18n::tf("confirm_bind_reading", &[&(key + 1)]), Confirm::Reading),
        };
        self.device.ask_confirm(key, &text, state);
    }

    fn cancel_previous_proposal(&mut self) {
        if let Some(previous) = self.proposal.take() {
            (self.log)(&format!("Кнопка {}: запоминание отменено новым", previous.key + 1));
        }
    }

    /// Предложить уже прочитанную сессию (удержание читает её в начале жеста).
    fn propose(&mut self, key: usize, session: Session) {
        self.cancel_previous_proposal();
        self.last_read += 1;
        self.proposal = Some(Proposal { key, id: self.last_read, session: Some(session), accepted: false, failed: None });
        self.show_proposal();
    }

    /// Показать диалог сразу и прочитать выделенную сессию в фоне.
    fn propose_current(&mut self, key: usize) {
        self.cancel_previous_proposal();
        self.last_read += 1;
        self.proposal = Some(Proposal { key, id: self.last_read, session: None, accepted: false, failed: None });
        self.reads.push(self.last_read);
        self.show_proposal();
    }

    /// Чтения выделенной сессии, которые надо выполнить в фоне; ответ — `on_current`.
    pub fn take_reads(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.reads)
    }

    /// Ответ фонового чтения. Предложение, отменённое или заменённое за это время, ответ не трогает.
    pub fn on_current(&mut self, id: u64, result: Result<Session, String>) {
        let Some(mut proposal) = self.proposal.take_if(|p| p.id == id && p.session.is_none() && p.failed.is_none()) else {
            return;
        };
        let key = proposal.key;
        match result {
            Ok(session) if proposal.accepted => self.bind_proposed(key, session),
            Ok(session) => {
                (self.log)(&format!("Кнопка {}: подтвердите запоминание сессии {}", key + 1, actions::describe(&session)));
                proposal.session = Some(session);
                self.proposal = Some(proposal);
                self.show_proposal();
            }
            Err(error) => {
                let error = actions::remember_error(self.state.values(key), &error);
                (self.log)(&format!("Кнопка {}: не удалось запомнить текущую сессию: {error}", key + 1));
                if proposal.accepted {
                    self.device.notify(&crate::i18n::tf("bind_failed", &[&(key + 1), &error]));
                } else {
                    proposal.failed = Some(error);
                    self.proposal = Some(proposal);
                    self.show_proposal();
                }
            }
        }
    }

    // --- опросы

    /// Кнопки со сценарием и копии их сессий: копии можно отдать фоновому потоку.
    pub fn bound_keys(&self) -> Vec<(usize, Session)> {
        (0..self.key_count())
            .filter(|&key| self.scenario_of(key).is_some())
            .filter_map(|key| self.session_of(key).map(|s| (key, s.clone())))
            .collect()
    }

    /// Ответ фонового опроса: свежие сведения об агенте и доступность сессий.
    /// Кнопку, перезаписанную во время опроса, ответ не трогает.
    pub fn show_probes(&mut self, probes: Vec<(usize, Session, Probe)>) {
        let mut changed_any = false;
        for (key, probed, result) in probes {
            if key >= self.key_count() || self.scenario_of(key).is_none() || !same_session(self.session_of(key), Some(&probed)) {
                continue;
            }
            if let Some(available) = result.available {
                self.available.insert(key, available);
            }
            let session = self.state.values_mut(key).get_mut("session").and_then(Value::as_object_mut).unwrap();
            let changed: Map<String, Value> = result
                .info
                .unwrap_or_default()
                .into_iter()
                .filter(|(name, value)| session.get(name) != Some(value))
                .collect();
            if changed.is_empty() {
                self.show_state(key);
            } else {
                session.extend(changed);
                changed_any = true;
                self.render_key(key);
            }
        }
        if changed_any {
            self.save();
        }
    }

    /// Рамка у кнопок, чья сессия сейчас в фокусе на экране.
    pub fn show_focused(&mut self, focused: Option<Session>) {
        let active: BTreeSet<usize> = (0..self.key_count())
            .filter(|&key| self.scenario_of(key).is_some() && same_session(self.session_of(key), focused.as_ref()))
            .collect();
        for &key in active.symmetric_difference(&self.active) {
            self.device.set_active(key, active.contains(&key));
        }
        self.active = active;
        self.focused = focused;
    }

    // --- жест → действия сценария

    fn on_gesture(&mut self, key: usize, gesture: Gesture) {
        let Some(scenario) = self.scenario_of(key) else {
            (self.log)(&format!("Кнопка {}: {}, сценарий не выбран", key + 1, gesture.name()));
            return;
        };
        let Some(calls) = scenario.on.get(&gesture).filter(|c| !c.is_empty()).cloned() else { return };
        self.run_calls(key, gesture, &calls);
    }

    /// Крестик на кнопке, чья сессия или окно закрыты: забыть сессию, как двойной клик
    /// в автоматическом сценарии (в любом сценарии — двойной клик у них разный).
    pub fn forget_closed(&mut self, key: usize) {
        if key >= self.key_count() || self.scenario_of(key).is_none() {
            return;
        }
        // Панель могла показать крестик по устаревшим сведениям: сбрасываем только закрытую.
        if self.available.get(&key) != Some(&false) || self.session_of(key).is_none() {
            return;
        }
        if self.proposal_key() == Some(key) {
            self.cancel_proposal();
        }
        let call = crate::scenarios::ActionCall { name: "unbind_session".into(), params: Default::default() };
        self.run_calls(key, Gesture::DoubleClick, &[call]);
    }

    fn run_calls(&mut self, key: usize, gesture: Gesture, calls: &[crate::scenarios::ActionCall]) {
        let before = self.session_of(key).cloned();
        let (sessions, log) = (self.sessions.clone(), self.log.clone());
        let mut context = Context {
            key,
            gesture,
            values: self.state.values_mut(key),
            sessions: sessions.as_ref(),
            log: log.as_ref(),
            opened: None,
            confirm_bind: self.confirm_bind,
            proposed: None,
            read_later: false,
        };
        for call in calls {
            if let Err(error) = actions::run(&mut context, &call.name, &call.params) {
                log(&format!("Ошибка в «{}»: {error}", call.name));
                break;
            }
        }
        let (opened, proposed, read_later) = (context.opened.take(), context.proposed.take(), context.read_later);
        if let Some(session) = proposed {
            self.propose(key, session);
        } else if read_later {
            self.propose_current(key);
        }
        self.settle_keys(key);
        if !same_session(before.as_ref(), self.session_of(key)) {
            self.available.remove(&key); // новая сессия: доступность скажет следующий опрос
        }
        self.save();
        self.render_key(key);
        // Открытая сессия сейчас окажется в фокусе: рамка — сразу, опрос фокуса лишь подтвердит.
        // Иначе — пересчёт: кнопку могли перезаписать другой сессией.
        let focused = opened.or_else(|| self.focused.clone());
        self.show_focused(focused);
        self.focus_requested = true;
    }
}

#[cfg(test)]
mod tests;
