//! Запуск вместе с Windows: значение в `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Включено ли — читается из реестра, а не из настроек: пользователь мог отключить
//! автозапуск в диспетчере задач.

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};

use crate::win::wide;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Tapswitch";
/// Имя пункта до переименования программы в Tapswitch.
const OLD_NAME: &str = "AgentControl";
/// При автозапуске страница не открывается — только значок в трее.
pub const FLAG: &str = "--no-browser";

fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {FLAG}", exe.display()))
}

fn read() -> Option<String> {
    read_value(NAME)
}

fn read_value(name: &str) -> Option<String> {
    let (key, name) = (wide(RUN), wide(name));
    let mut buffer = [0u16; 2048];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut _,
            &mut size,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buffer[..length]))
}

/// Включён ли автозапуск именно этого exe.
pub fn enabled() -> bool {
    match (read(), command()) {
        (Some(value), Some(own)) => value.eq_ignore_ascii_case(&own),
        _ => false,
    }
}

/// Пункт автозапуска прежней версии (Agent Control) — заменить своим, если он запускал
/// программу из этой же папки (иначе это другая копия, например проверочная, — не трогать).
/// true — перенесён.
pub fn migrate() -> bool {
    let Some(old) = read_value(OLD_NAME) else { return false };
    let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.to_path_buf())) else {
        return false;
    };
    let old_exe = std::path::PathBuf::from(old.trim().trim_start_matches('"').split('"').next().unwrap_or(""));
    let same_folder = old_exe.parent().is_some_and(|d| d.to_string_lossy().eq_ignore_ascii_case(&dir.to_string_lossy()));
    if !same_folder || set(true).is_err() {
        return false;
    }
    let (key, name) = (wide(RUN), wide(OLD_NAME));
    unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
    true
}

pub fn set(enable: bool) -> Result<(), String> {
    let (key, name) = (wide(RUN), wide(NAME));
    let status = if enable {
        let value = wide(&command().ok_or("не найден путь к программе")?);
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                value.as_ptr() as *const _,
                (value.len() * 2) as u32,
            )
        }
    } else {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
        if status == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND { ERROR_SUCCESS } else { status }
    };
    if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("ошибка реестра {status}")) }
}
