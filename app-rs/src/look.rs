//! Свой вид кнопки: подпись, цвет плашки и иконка. Хранится в значениях кнопки
//! (`state.json`, `keys.<n>.values`) рядом с сессией или сочетанием, поэтому переезжает
//! при перетаскивании и живёт в своём профиле. Подпись и цвет «Очистить» не трогает;
//! своя иконка сбрасывается, когда кнопка меняет назначение, — её убирает `reset_icon`.

use serde_json::{Map, Value, json};

/// Подпись вместо автоматической; сценарии подставляют её через `{caption|…}`.
pub const CAPTION: &str = "caption";
/// Иконка с типом: `text:R`; дальше — `lib:…`, `img:…`, `app:…`.
pub const ICON: &str = "icon";
pub const COLOR: &str = "color";
const NAMES: [&str; 3] = [CAPTION, ICON, COLOR];

/// Цвета плашки; сами оттенки — на странице панели.
pub const COLORS: [&str; 9] = ["red", "orange", "yellow", "green", "teal", "blue", "purple", "pink", "brown"];
const TEXT: &str = "text:";
/// Значок встроенной библиотеки (`library.rs`).
pub const LIB: &str = "lib:";
/// Своя картинка: PNG 128×128 в папке `usericons`, имя — хэш содержимого.
pub const IMG: &str = "img:";
/// Иконка программы: полный путь к .exe.
pub const APP: &str = "app:";

/// Имя своей картинки — 16 шестнадцатеричных цифр (хэш содержимого).
pub fn is_image_hash(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Похоже на полный путь к .exe (есть ли файл — проверяет сервер).
pub fn is_program_path(text: &str) -> bool {
    let path = std::path::Path::new(text);
    let windows_absolute = text.as_bytes().get(1..3) == Some(b":\\".as_slice()) || text.starts_with("\\\\");
    (path.is_absolute() || windows_absolute)
        && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        && !text.chars().any(char::is_control)
}
const MAX_CAPTION: usize = 40;
/// Символов (char) в текстовой иконке: 1–5 знаков, а эмодзи бывают из нескольких char.
/// Сколько именно знаков видно, считает страница (по графемам) — здесь только защита от длины.
const MAX_TEXT: usize = 16;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    pub caption: Option<String>,
    pub icon: Option<String>,
    pub color: Option<String>,
}

fn clean(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

impl Look {
    /// Проверить присланное страницей; пустые поля — автоматический вид.
    pub fn parse(caption: Option<&str>, icon: Option<&str>, color: Option<&str>) -> Result<Self, String> {
        let look = Self { caption: clean(caption), icon: clean(icon), color: clean(color) };
        if let Some(caption) = &look.caption {
            if caption.chars().count() > MAX_CAPTION {
                return Err(format!("подпись длиннее {MAX_CAPTION} символов"));
            }
            if caption.chars().any(char::is_control) {
                return Err("в подписи служебные символы".into());
            }
        }
        if let Some(icon) = &look.icon {
            if let Some(name) = icon.strip_prefix(LIB) {
                if !crate::library::exists(name) {
                    return Err("нет такого значка в библиотеке".into());
                }
            } else if let Some(hash) = icon.strip_prefix(IMG) {
                if !is_image_hash(hash) {
                    return Err("неверное имя картинки".into());
                }
            } else if let Some(path) = icon.strip_prefix(APP) {
                if !is_program_path(path) {
                    return Err("нужен полный путь к .exe".into());
                }
            } else {
                let text = icon.strip_prefix(TEXT).map(str::trim).ok_or("неизвестный тип иконки")?;
                if text.is_empty() || text.chars().count() > MAX_TEXT || text.chars().any(char::is_control) {
                    return Err("текст иконки — 1–5 символов".into());
                }
            }
        }
        if look.color.as_deref().is_some_and(|c| !COLORS.contains(&c)) {
            return Err("нет такого цвета".into());
        }
        Ok(look)
    }

    pub fn of(values: &Map<String, Value>) -> Self {
        let get = |name| clean(values.get(name).and_then(Value::as_str));
        Self { caption: get(CAPTION), icon: get(ICON), color: get(COLOR) }
    }

    /// Записать в значения кнопки; пустые поля убираются.
    pub fn store(&self, values: &mut Map<String, Value>) {
        for (name, value) in NAMES.iter().zip([&self.caption, &self.icon, &self.color]) {
            match value {
                Some(value) => values.insert(name.to_string(), json!(value)),
                None => values.remove(*name),
            };
        }
    }

    /// Текст иконки, если иконка текстовая.
    pub fn icon_text(&self) -> Option<&str> {
        self.icon.as_deref()?.strip_prefix(TEXT).map(str::trim).filter(|t| !t.is_empty())
    }

    /// Своя иконка-картинка как имя иконки для панели: `lib:box`, `img:<хэш>` или путь к exe
    /// (панель сама превращает его в адрес картинки); текстовая или неизвестная — None.
    pub fn custom_icon(&self) -> Option<&str> {
        let icon = self.icon.as_deref()?;
        if let Some(name) = icon.strip_prefix(LIB) {
            return crate::library::exists(name).then_some(icon);
        }
        if let Some(hash) = icon.strip_prefix(IMG) {
            return is_image_hash(hash).then_some(icon);
        }
        icon.strip_prefix(APP).filter(|path| is_program_path(path))
    }

    pub fn to_json(&self) -> Value {
        json!({CAPTION: self.caption, ICON: self.icon, COLOR: self.color})
    }
}

/// Забыть всё, кроме вида кнопки.
pub fn keep_only(values: &mut Map<String, Value>) {
    values.retain(|name, _| NAMES.contains(&name.as_str()));
}

/// Забыть свою иконку: кнопка сменила назначение (запомнила сессию или приложение,
/// очищена) — иконка снова автоматическая. Подпись и цвет остаются.
pub fn reset_icon(values: &mut Map<String, Value>) {
    values.remove(ICON);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        let look = Look::parse(Some(" Рендер "), Some("text:R"), Some("blue")).unwrap();
        assert_eq!(look.caption.as_deref(), Some("Рендер"));
        assert_eq!(look.icon_text(), Some("R"));
        assert_eq!(Look::parse(Some(""), Some(" "), None).unwrap(), Look::default());
        assert!(Look::parse(None, Some("text:👍🏽"), None).is_ok());
        assert!(Look::parse(None, None, Some("black")).is_err());
        assert_eq!(Look::parse(None, Some("lib:box"), None).unwrap().custom_icon(), Some("lib:box"));
        assert!(Look::parse(None, Some("lib:нет-такого"), None).is_err());
        let image = Look::parse(None, Some("img:0123456789abcdef"), None).unwrap();
        assert_eq!(image.custom_icon(), Some("img:0123456789abcdef"));
        for bad in ["img:abc", "img:0123456789ABCDEF", "img:../../x.png", "app:blender.exe", "app:C:\\x\\a.txt", "app:"] {
            assert!(Look::parse(None, Some(bad), None).is_err(), "{bad}");
        }
        let program = Look::parse(None, Some("app:C:\\Program Files\\Blender\\blender.exe"), None).unwrap();
        assert_eq!(program.custom_icon(), Some("C:\\Program Files\\Blender\\blender.exe"));
        assert!(Look::parse(None, Some("text:"), None).is_err());
        assert!(Look::parse(Some(&"x".repeat(41)), None, None).is_err());
        assert!(Look::parse(Some("a\nb"), None, None).is_err());
    }

    #[test]
    fn stores_and_keeps_only_look() {
        let mut values = json!({"session": {"name": "a"}, "keys": "Win+H"}).as_object().unwrap().clone();
        let look = Look::parse(Some("G"), None, Some("red")).unwrap();
        look.store(&mut values);
        assert_eq!(Look::of(&values), look);
        keep_only(&mut values);
        assert_eq!(Value::Object(values.clone()), json!({"caption": "G", "color": "red"}));
        Look::default().store(&mut values);
        assert!(values.is_empty());
    }
}
