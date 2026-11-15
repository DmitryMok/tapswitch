//! Встроенная библиотека значков для кнопок: отобранные значки Lucide (лицензия ISC,
//! `assets/LUCIDE-LICENSE.txt`) с ключевыми словами для поиска на русском и английском.
//! Значение иконки кнопки — `lib:<имя>`; панель берёт картинку по адресу `/lib/<имя>.svg`.

use serde::Deserialize;
use std::sync::OnceLock;

const LIBRARY: &str = include_str!("../assets/library.json");

#[derive(Deserialize)]
pub struct Icon {
    pub name: String,
    pub ru: String,
    pub en: String,
    /// Содержимое `<svg>` без обёртки.
    svg: String,
}

#[derive(Deserialize)]
struct Library {
    icons: Vec<Icon>,
}

pub fn icons() -> &'static [Icon] {
    static PARSED: OnceLock<Vec<Icon>> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str::<Library>(LIBRARY).map(|l| l.icons).unwrap_or_default())
}

pub fn exists(name: &str) -> bool {
    icons().iter().any(|i| i.name == name)
}

/// Значок целиком: белые линии — на цветной плашке кнопки (в `<img>` currentColor не работает).
pub fn svg(name: &str) -> Option<String> {
    let icon = icons().iter().find(|i| i.name == name)?;
    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" \
         stroke=\"#ffffff\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">{}</svg>",
        icon.svg
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_is_embedded_and_complete() {
        let icons = icons();
        assert!(icons.len() > 200, "{}", icons.len());
        for icon in icons {
            assert!(!icon.ru.is_empty() && !icon.en.is_empty(), "{}", icon.name);
            assert!(icon.svg.starts_with('<') && !icon.svg.contains("<script"), "{}", icon.name);
            assert!(!icon.name.contains(['/', '\\', '.', ':']), "{}", icon.name);
        }
        for name in ["box", "save", "undo-2", "camera", "clapperboard", "mic", "terminal"] {
            assert!(exists(name), "{name}");
        }
        assert!(svg("box").unwrap().contains("stroke=\"#ffffff\""));
        assert!(svg("нет такого").is_none());
    }
}
