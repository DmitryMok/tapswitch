//! Язык интерфейса: словари `app-rs/locales/<язык>.json`, встроенные в exe. Новый язык —
//! один новый файл. В коде — только ключи строк.
//!
//! Настройка `language` в state.json — код языка; без неё — русский.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::places::LOCALES;

pub const DEFAULT: &str = "ru";
/// Настройка из state.json.
pub const SETTING: &str = "language";

static CURRENT: RwLock<&'static str> = RwLock::new(DEFAULT);

fn dictionaries() -> &'static HashMap<&'static str, Map<String, Value>> {
    static PARSED: OnceLock<HashMap<&'static str, Map<String, Value>>> = OnceLock::new();
    PARSED.get_or_init(|| {
        LOCALES.iter().filter_map(|(code, text)| Some((*code, serde_json::from_str(text).ok()?))).collect()
    })
}

/// Коды языков, от которых есть словари, по алфавиту.
pub fn languages() -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = dictionaries().keys().copied().collect();
    codes.sort();
    codes
}

/// Словарь языка целиком — для страницы управления.
pub fn dictionary(code: &str) -> Option<&'static Map<String, Value>> {
    dictionaries().get(code)
}

fn known(code: &str) -> Option<&'static str> {
    dictionaries().keys().copied().find(|k| *k == code)
}

/// Язык по настройке; пустая, `auto` или неизвестная — русский (язык по умолчанию).
pub fn resolve(setting: &str) -> &'static str {
    known(setting).unwrap_or(DEFAULT)
}

pub fn set(code: &'static str) {
    *CURRENT.write().unwrap() = code;
}

pub fn current() -> &'static str {
    *CURRENT.read().unwrap()
}

/// Строка по ключу на текущем языке; нет перевода — на языке по умолчанию, затем сам ключ.
pub fn t(key: &str) -> String {
    [current(), DEFAULT]
        .iter()
        .filter_map(|code| dictionaries().get(code)?.get(key)?.as_str())
        .next()
        .unwrap_or(key)
        .to_string()
}

/// Строка с подстановками {0}, {1}, …
pub fn tf(key: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut text = t(key);
    for (index, arg) in args.iter().enumerate() {
        text = text.replace(&format!("{{{index}}}"), &arg.to_string());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_every_key() {
        let reference = dictionary(DEFAULT).unwrap();
        for code in languages() {
            let dictionary = dictionary(code).unwrap();
            let missing: Vec<&String> = reference.keys().filter(|k| !dictionary.contains_key(*k)).collect();
            let extra: Vec<&String> = dictionary.keys().filter(|k| !reference.contains_key(*k)).collect();
            assert!(missing.is_empty() && extra.is_empty(), "{code}: нет {missing:?}, лишние {extra:?}");
        }
        assert!(languages().contains(&"en"));
    }

    #[test]
    fn substitutes_arguments() {
        // Язык по умолчанию в тестах — русский.
        assert_eq!(tf("confirm_bind", &[&3, &"local_1"]), "Запомнить на кнопке 3 сессию «local_1»?");
        assert_eq!(t("no_such_key"), "no_such_key");
        assert_eq!(resolve("en"), "en");
    }
}
