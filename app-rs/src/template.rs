//! Подстановка значений кнопки в подпись: `{путь.к.значению|другой.путь|запасной текст}`
//! (перенос `render` из `actions.py`). Первое найденное значение, иначе последний вариант
//! как текст. Регулярных выражений нет — разбор вручную, ради размера exe.

use serde_json::{Map, Value};

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

fn is_path(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_path_char)
}

fn lookup<'a>(values: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut current = values.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    (!current.is_null()).then_some(current)
}

/// Значение как текст — так, как его показала бы Python-версия.
pub fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Разобрать `{…}` с начала `text`: варианты и длина в байтах; None — это не подстановка.
fn placeholder(text: &str) -> Option<(Vec<&str>, usize)> {
    let body = text.strip_prefix('{')?;
    let path_len = body.find(|c: char| !is_path_char(c)).unwrap_or(body.len());
    if path_len == 0 {
        return None;
    }
    let mut options = vec![&body[..path_len]];
    let mut rest = &body[path_len..];
    loop {
        if let Some(after) = rest.strip_prefix('}') {
            return Some((options, text.len() - after.len()));
        }
        let after = rest.strip_prefix('|')?;
        let end = after.find(['|', '}']).unwrap_or(after.len());
        options.push(&after[..end]);
        rest = &after[end..];
    }
}

pub fn render(template: &str, values: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match placeholder(rest) {
            Some((options, length)) => {
                let last = options.len() - 1;
                for (index, option) in options.iter().enumerate() {
                    if let Some(found) = is_path(option).then(|| lookup(values, option)).flatten() {
                        out.push_str(&text_of(found));
                        break;
                    }
                    if index == last && index > 0 {
                        out.push_str(option);
                    }
                }
                rest = &rest[length..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn renders() {
        let values = map(json!({"session": {"name": "demo"}}));
        assert_eq!(render("{session.name|нет}", &values), "demo");
        assert_eq!(render("{other.name|нет}", &values), "нет");
        assert_eq!(render("{missing}", &values), "");
    }

    #[test]
    fn tries_paths_in_turn_then_text() {
        let values = map(json!({"session": {"title": "demo", "project": null}, "text": "есть"}));
        assert_eq!(render("{session.project|session.title|Нет сессии}", &values), "demo");
        assert_eq!(render("{session.role|session.none|Нет сессии}", &values), "Нет сессии");
        assert_eq!(render("{missing|Нажми}", &values), "Нажми");
        assert_eq!(render("{text|Нажми}", &values), "есть");
        assert_eq!(render("{missing|}x{missing}", &values), "x");
    }

    #[test]
    fn leaves_non_placeholders_alone() {
        let values = map(json!({"key": 3}));
        assert_eq!(render("{ a } {} {key", &values), "{ a } {} {key");
        assert_eq!(render("Кнопка {key}: клик", &values), "Кнопка 3: клик");
    }
}
