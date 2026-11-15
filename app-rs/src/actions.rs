//! Действия сценариев (перенос `actions.py`). Новая возможность — новая ветка в `run`
//! и её параметры в `params_of`.

use serde_json::{Map, Value, json};

use crate::gestures::Gesture;
use crate::sessions::{Result, Session, SessionError, Sessions, readiness};
use crate::template;

/// Сессия потеряна, следующее нажатие запоминает выделенную.
pub const WAITING: &str = "_waiting";
const WAITING_MARK: &str = "⚠ Выделите сессию и нажмите: ";
/// Сессия, прочитанная в начале удержания; запоминается при отпускании.
const HELD: &str = "_held";
const RECORDING: &str = "_recording";

/// Параметры действия (все обязательные) или None, если такого действия нет.
pub fn params_of(action: &str) -> Option<&'static [&'static str]> {
    Some(match action {
        "log" => &["message"],
        "set_value" => &["name", "value"],
        "clear_value" => &["name"],
        "bind_current_session" | "read_current_session" | "bind_held_session" | "forget_held_session"
        | "unbind_session" | "open_session" | "switch_session" => &[],
        // Распознавание речи в Rust-версию не переносится: сценарий загружается, действие — нет.
        "start_recording" | "stop_recording" | "cancel_recording" => &[],
        "send_keys" => &[],
        _ => return None,
    })
}

pub struct Context<'a> {
    pub key: usize,
    pub gesture: Gesture,
    pub values: &'a mut Map<String, Value>,
    pub sessions: &'a dyn Sessions,
    pub log: &'a dyn Fn(&str),
    /// Сессия, которую действие успешно открыло.
    pub opened: Option<Session>,
    /// Запоминать сессию только после подтверждения в диалоге.
    pub confirm_bind: bool,
    /// Сессия, которую надо запомнить после подтверждения.
    pub proposed: Option<Session>,
    /// Показать диалог подтверждения сразу, а выделенную сессию прочитать в фоне.
    pub read_later: bool,
}

impl Context<'_> {
    fn render(&self, template: &str) -> String {
        let mut values = self.values.clone();
        values.insert("key".into(), json!(self.key + 1));
        values.insert("gesture".into(), json!(self.gesture.name()));
        template::render(template, &values)
    }

    fn session(&self) -> Option<&Session> {
        self.values.get("session").and_then(Value::as_object).filter(|s| !s.is_empty())
    }
}

pub fn run(ctx: &mut Context, name: &str, params: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let param = |name: &str| params.get(name).map(String::as_str).unwrap_or("");
    match name {
        "log" => (ctx.log)(&ctx.render(param("message"))),
        "set_value" => {
            let value = ctx.render(param("value"));
            ctx.values.insert(param("name").into(), json!(value));
        }
        "clear_value" => {
            ctx.values.remove(param("name"));
        }
        "bind_current_session" => bind_current_session(ctx)?,
        "read_current_session" => {
            // Начало удержания: прочитать выделенную сессию, пока она точно та, что на экране.
            ctx.values.remove(HELD);
            let session = ctx.sessions.current()?;
            ctx.values.insert(HELD.into(), Value::Object(session));
        }
        "bind_held_session" => {
            // Прочитать не удалось — ошибка уже в журнале, прежняя запись остаётся.
            if let Some(Value::Object(session)) = ctx.values.remove(HELD) {
                bind_or_propose(ctx, session);
            }
        }
        "forget_held_session" => {
            ctx.values.remove(HELD);
        }
        "unbind_session" => {
            // Кнопка снова пустая и следующим кликом запомнит выделенную сессию.
            let session = ctx.values.remove("session");
            ctx.values.remove(WAITING);
            if session.is_some() {
                crate::look::reset_icon(ctx.values); // назначение сменилось — иконка автоматическая
            }
            match session.as_ref().and_then(Value::as_object) {
                Some(session) => (ctx.log)(&format!(
                    "Кнопка {}: привязка к сессии «{}» очищена",
                    ctx.key + 1,
                    template::text_of(session.get("name").unwrap_or(&Value::Null))
                )),
                None => (ctx.log)(&format!("Кнопка {}: привязки не было", ctx.key + 1)),
            }
        }
        "open_session" => open_session(ctx)?,
        "switch_session" => switch_session(ctx)?,
        "start_recording" => {
            (ctx.log)(&format!("Кнопка {}: распознавание речи есть только в Python-версии", ctx.key + 1))
        }
        "send_keys" => {
            // Сочетание кнопки-сочетания — нажать в активном окне.
            let combo = ctx.values.get(crate::keystrokes::VALUE).and_then(Value::as_str).unwrap_or("").to_string();
            if combo.is_empty() {
                (ctx.log)(&format!("Кнопка {}: сочетание не назначено", ctx.key + 1));
            } else {
                crate::keystrokes::send(&combo).map_err(SessionError::Other)?;
                (ctx.log)(&format!("Кнопка {}: нажато {combo}", ctx.key + 1));
            }
        }
        "stop_recording" | "cancel_recording" => {
            ctx.values.remove(RECORDING);
        }
        _ => (ctx.log)(&format!("Неизвестное действие «{name}»")),
    }
    Ok(())
}

pub fn bind(values: &mut Map<String, Value>, key: usize, session: Session, log: &dyn Fn(&str)) {
    log(&format!("Кнопка {} назначена на сессию {}", key + 1, describe(&session)));
    values.insert("session".into(), Value::Object(session));
    values.remove(WAITING);
    // Сменилось назначение — своя иконка (например, микрофон) больше не подходит:
    // возвращаем автоматическую, цвет плашки и подпись оставляем.
    crate::look::reset_icon(values);
}

/// Запомнить сразу или, если включено подтверждение, предложить (диалог покажет движок).
fn bind_or_propose(ctx: &mut Context, session: Session) {
    if ctx.confirm_bind {
        (ctx.log)(&format!("Кнопка {}: подтвердите запоминание сессии {}", ctx.key + 1, describe(&session)));
        ctx.proposed = Some(session);
    } else {
        bind(ctx.values, ctx.key, session, ctx.log);
    }
}

/// Текст диалога подтверждения.
pub fn describe_proposal(key: usize, session: &Session) -> String {
    let field = |name: &str| session.get(name).and_then(Value::as_str).filter(|s| !s.is_empty());
    let title = field("title").or_else(|| field("name")).unwrap_or("?");
    crate::i18n::tf("confirm_bind", &[&(key + 1), &title])
}

fn bind_current_session(ctx: &mut Context) -> Result<()> {
    if ctx.confirm_bind {
        // Чтение сессии заметно долгое: диалог — сразу, сессия — вдогонку (это сделает движок).
        (ctx.log)(&format!("Кнопка {}: определяю выделенную сессию для запоминания", ctx.key + 1));
        ctx.read_later = true;
        return Ok(());
    }
    let session = ctx.sessions.current()?; // при ошибке прежняя запись остаётся
    bind_or_propose(ctx, session);
    Ok(())
}

/// Причина, по которой не удалось запомнить; кнопка, потерявшая сессию, подсказывает, что делать дальше.
pub fn remember_error(values: &Map<String, Value>, error: &dyn std::fmt::Display) -> String {
    let hint = if values.contains_key(WAITING) { "; кнопка ждёт: выделите сессию и нажмите ещё раз" } else { "" };
    format!("{error}{hint}")
}

fn open_session(ctx: &mut Context) -> Result<()> {
    let Some(Value::Object(session)) = ctx.values.get_mut("session").filter(|s| s.as_object().is_some_and(|o| !o.is_empty())) else {
        (ctx.log)("Сессия не назначена: сначала запомните текущую сессию");
        return Ok(());
    };
    ctx.sessions.open(session)?;
    let session = session.clone();
    (ctx.log)(&format!("Открыта сессия {}", describe(&session)));
    ctx.opened = Some(session);
    Ok(())
}

/// Открыть запомненную сессию; на пустой кнопке или после потери — запомнить текущую.
fn switch_session(ctx: &mut Context) -> Result<()> {
    if let Some(session) = ctx.session().filter(|_| !ctx.values.contains_key(WAITING)) {
        let name = template::text_of(session.get("name").unwrap_or(&Value::Null));
        match open_session(ctx) {
            Err(SessionError::Lost(error)) => {
                ctx.values.insert(WAITING.into(), json!(WAITING_MARK));
                (ctx.log)(&format!(
                    "Сессия «{name}» не найдена ({error}): выделите нужную сессию и нажмите кнопку ещё раз"
                ));
                return Ok(());
            }
            other => return other,
        }
    }
    bind_current_session(ctx).map_err(|error| SessionError::Other(format!("не удалось запомнить текущую сессию: {}", remember_error(ctx.values, &error))))
}

pub fn describe(session: &Session) -> String {
    let field = |name: &str| session.get(name).filter(|v| !v.is_null() && *v != "").map(template::text_of);
    let address: Vec<String> = [field("server"), field("id")].into_iter().flatten().collect();
    let name = template::text_of(session.get("name").unwrap_or(&Value::Null));
    let place = format!("«{name}» ({})", address.join(", "));
    if !session.contains_key("agent_mark") {
        return place; // сервер не сообщил сведений о панели
    }
    let role = field("role").map(|r| format!(", роль {r}")).unwrap_or_default();
    format!("{place}{role}: {}", readiness(session))
}
