//! Кнопка-сочетание: нажатие кнопки панели нажимает в активном окне сочетание клавиш
//! («Win+H» — голосовой ввод Windows, «Ctrl+Shift+Esc» — диспетчер задач и т.п.).
//! Клавиши нажимаются через SendInput, как с настоящей клавиатуры, поэтому работают
//! и сочетания с Win, которые Windows не даёт программам перехватывать.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    SendInput,
};

/// Сценарий кнопки-сочетания (`app/scenarios/keys.yaml`).
pub const SCENARIO: &str = "keys";
/// Значение кнопки с сочетанием.
pub const VALUE: &str = "keys";

const CTRL: u16 = 0x11;
const ALT: u16 = 0x12;
const SHIFT: u16 = 0x10;
const WIN: u16 = 0x5B;

#[derive(Debug, PartialEq)]
pub struct Combo {
    /// Модификаторы в порядке нажатия.
    pub modifiers: Vec<u16>,
    /// Код клавиши.
    pub key: u16,
}

/// «Win+H» → модификаторы и клавиша. Регистр и пробелы не важны.
pub fn parse(combo: &str) -> Result<Combo, String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().filter(|(k, _)| !k.is_empty()).ok_or("пустое сочетание")?;
    let mut modifiers = Vec::new();
    for part in mods {
        let code = match part.to_ascii_lowercase().as_str() {
            "ctrl" => CTRL,
            "alt" => ALT,
            "shift" => SHIFT,
            "win" => WIN,
            _ => return Err(format!("неизвестная клавиша «{part}»")),
        };
        if !modifiers.contains(&code) {
            modifiers.push(code);
        }
    }
    let upper = key.to_ascii_uppercase();
    let function = upper.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=24).contains(n));
    let code = match upper.as_str() {
        _ if function.is_some() => 0x70 + function.unwrap() - 1,
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) => k.as_bytes()[0] as u16,
        k if k.len() == 4 && k.starts_with("NUM") && k.as_bytes()[3].is_ascii_digit() => 0x60 + (k.as_bytes()[3] - b'0') as u16,
        "BACKSPACE" => 0x08,
        "TAB" => 0x09,
        "ENTER" => 0x0D,
        "ESCAPE" | "ESC" => 0x1B,
        "SPACE" => 0x20,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "END" => 0x23,
        "HOME" => 0x24,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "PRINTSCREEN" => 0x2C,
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "VOLUMEMUTE" => 0xAD,
        "VOLUMEDOWN" => 0xAE,
        "VOLUMEUP" => 0xAF,
        "MEDIANEXT" => 0xB0,
        "MEDIAPREV" => 0xB1,
        "MEDIASTOP" => 0xB2,
        "MEDIAPLAYPAUSE" => 0xB3,
        _ => return Err(format!("клавишу «{key}» нажать нельзя")),
    };
    Ok(Combo { modifiers, key: code })
}

/// Клавиши «расширенной» части клавиатуры: без флага Windows путает их с цифровым блоком.
fn extended(code: u16) -> bool {
    matches!(code, 0x21..=0x28 | 0x2C..=0x2E | WIN | 0xAD..=0xB3)
}

fn input(code: u16, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if extended(code) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: code,
                wScan: unsafe { MapVirtualKeyW(code as u32, MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Нажатия и отпускания по порядку: модификаторы, клавиша, отпускание в обратном порядке.
fn inputs(combo: &Combo) -> Vec<(u16, bool)> {
    let mut list: Vec<(u16, bool)> = combo.modifiers.iter().map(|&m| (m, false)).collect();
    list.push((combo.key, false));
    list.push((combo.key, true));
    list.extend(combo.modifiers.iter().rev().map(|&m| (m, true)));
    list
}

/// Нажать сочетание в активном окне.
pub fn send(text: &str) -> Result<(), String> {
    let combo = parse(text)?;
    let events = inputs(&combo);
    if cfg!(test) {
        return Ok(()); // тесты не нажимают настоящие клавиши
    }
    let list: Vec<INPUT> = events.into_iter().map(|(code, up)| input(code, up)).collect();
    let sent = unsafe { SendInput(list.len() as u32, list.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != list.len() {
        return Err("Windows не принял нажатие (например, активно окно программы, запущенной от администратора)".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        assert_eq!(parse("Win+H"), Ok(Combo { modifiers: vec![WIN], key: 0x48 }));
        assert_eq!(parse("ctrl + shift + Esc"), Ok(Combo { modifiers: vec![CTRL, SHIFT], key: 0x1B }));
        assert_eq!(parse("F13"), Ok(Combo { modifiers: vec![], key: 0x7C }));
        assert_eq!(parse("Ctrl+Num7").unwrap().key, 0x67);
        assert_eq!(parse("VolumeUp").unwrap().key, 0xAF);
        assert!(parse("").is_err());
        assert!(parse("Ctrl+").is_err());
        assert!(parse("Hyper+K").is_err());
        assert!(parse("Ctrl+Pause").is_err());
    }

    #[test]
    fn modifiers_are_released_in_reverse_order() {
        let combo = parse("Ctrl+Alt+Delete").unwrap();
        assert_eq!(
            inputs(&combo),
            vec![(CTRL, false), (ALT, false), (0x2E, false), (0x2E, true), (ALT, true), (CTRL, true)]
        );
        assert!(extended(0x2E) && extended(WIN) && !extended(0x48));
    }
}
