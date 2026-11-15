//! Журнал в файл (окна консоли у релизной сборки нет).
//!
//! Размер ограничен: файл больше `LIMIT` переименовывается в `*.old.log` (прежний старый
//! заменяется) и запись идёт в новый — на диске не больше двух файлов по `LIMIT`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::Mutex;

static FILE: Mutex<Option<Writer>> = Mutex::new(None);
/// Предел размера одного файла журнала.
const LIMIT: u64 = 50 * 1024 * 1024;
/// Последние строки — для журнала на странице управления.
static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
const RECENT_LINES: usize = 300;

static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Где будет журнал. Файл открывается позже (`open`): вторая копия программы, которая сразу
/// завершается, не должна стирать журнал работающей.
pub fn prepare(path: &Path) {
    *PATH.lock().unwrap() = Some(path.to_path_buf());
}

/// Открыть журнал с чистого листа (чтобы файл не рос бесконечно); повторно — ничего не делает.
pub fn open() {
    let mut file = FILE.lock().unwrap();
    if file.is_some() {
        return;
    }
    let Some(path) = PATH.lock().unwrap().clone() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    *file = Writer::open(&path);
}

struct Writer {
    file: File,
    path: PathBuf,
    size: u64,
}

impl Writer {
    fn open(path: &Path) -> Option<Self> {
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(path).ok()?;
        Some(Self { file, path: path.to_path_buf(), size: 0 })
    }

    fn write(&mut self, line: &[u8], limit: u64) {
        if self.size > 0 && self.size + line.len() as u64 > limit {
            self.rotate();
        }
        if self.file.write_all(line).is_ok() {
            self.size += line.len() as u64;
        }
    }

    /// Текущий файл — в старый (прежний старый заменяется), запись — в новый. Если старый
    /// занят другой программой и переименовать не вышло, текущий начинается заново:
    /// лучше потерять журнал, чем дать ему расти без предела.
    fn rotate(&mut self) {
        let _ = std::fs::rename(&self.path, old_path(&self.path));
        if let Ok(file) = OpenOptions::new().create(true).write(true).truncate(true).open(&self.path) {
            self.file = file;
        }
        self.size = 0;
    }
}

/// `tapswitch.log` → `tapswitch.old.log`.
fn old_path(path: &Path) -> PathBuf {
    path.with_extension("old.log")
}

pub fn write(text: &str) {
    let line = format!("[{}] {text}\n", crate::win::now());
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    if let Some(file) = FILE.lock().unwrap().as_mut() {
        file.write(line.as_bytes(), LIMIT);
    }
    let mut recent = RECENT.lock().unwrap();
    if recent.len() == RECENT_LINES {
        recent.pop_front();
    }
    recent.push_back(line.trim_end().to_string());
}

/// Последние строки журнала, от старых к новым.
pub fn recent() -> Vec<String> {
    RECENT.lock().unwrap().iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_two_files_within_limit() {
        let dir = std::env::temp_dir().join(format!("agent_control_log_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.log");
        let old = dir.join("test.old.log");
        let mut writer = Writer::open(&path).unwrap();
        let read = |p: &Path| std::fs::read_to_string(p).unwrap_or_default();
        for line in ["aaaa\n", "bbbb\n"] {
            writer.write(line.as_bytes(), 12);
        }
        assert_eq!(read(&path), "aaaa\nbbbb\n");
        assert!(!old.exists());
        writer.write(b"cccc\n", 12); // не влезает — текущий уходит в старый
        assert_eq!((read(&old), read(&path)), ("aaaa\nbbbb\n".into(), "cccc\n".into()));
        writer.write(b"dddd\n", 12);
        writer.write(b"eeee\n", 12); // ещё раз — прежний старый заменяется
        assert_eq!((read(&old), read(&path)), ("cccc\ndddd\n".into(), "eeee\n".into()));
        writer.write(b"a line longer than the limit\n", 12);
        writer.write(b"ffff\n", 12); // длинная строка не теряется и не зацикливает
        assert_eq!((read(&old), read(&path)), ("a line longer than the limit\n".into(), "ffff\n".into()));
        drop(writer);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
