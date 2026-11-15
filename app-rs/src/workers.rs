//! Постоянные вспомогательные процессы: PowerShell для UI Automation и Python в WSL для herdr
//! (перенос `workers.py`).
//!
//! Запуск PowerShell стоит ~0,2 с, загрузка UI Automation — ещё столько же, а `wsl.exe` —
//! ~0,13 с на каждый вызов. Процесс, который живёт всё время работы программы и принимает
//! запросы по одной строке JSON, отвечает за единицы-десятки миллисекунд.
//! Зависший или упавший процесс убивается; вызывающий код делает то же одиночным запуском.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::sessions::Output;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Одновременно: нажатие кнопки, рамка фокуса, фоновый опрос и чтение сессии для запоминания.
const POOL_SIZE: usize = 4;

const PS_HOST: &str = include_str!("../../app/agent_control/ps_host.ps1");
pub const TERMINAL_TABS: &str = include_str!("../../app/agent_control/terminal_tabs.ps1");

const WSL_SERVER: &str = r#"
import json, subprocess, sys
for line in sys.stdin:
    try:
        request = json.loads(line)
        done = subprocess.run(request["args"], capture_output=True, text=True, errors="replace",
                              timeout=request.get("timeout", 10))
        reply = {"code": done.returncode, "out": done.stdout, "err": done.stderr}
    except Exception as error:
        reply = {"code": 127, "out": "", "err": str(error)}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
"#;

/// Скрипт, встроенный в exe, — во временной папке (PowerShell запускает только файлы).
pub fn script(name: &str, content: &str) -> PathBuf {
    static WRITTEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let dir = std::env::temp_dir().join("agent_control_rs");
    let path = dir.join(name);
    let mut written = WRITTEN.lock().unwrap();
    if !written.iter().any(|n| n == name) {
        let _ = std::fs::create_dir_all(&dir);
        // BOM: Windows PowerShell 5.1 читает файл без него в кодировке ANSI.
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(content.as_bytes());
        let _ = std::fs::write(&path, bytes);
        written.push(name.to_string());
    }
    path
}

/// JSON только из ASCII: консольные кодировки процессов на него не влияют.
fn ascii_json(value: &Value) -> String {
    let mut out = String::new();
    for c in value.to_string().chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

struct LineWorker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Option<String>>,
}

impl LineWorker {
    fn start(args: &[String]) -> Result<Self, String> {
        let mut child = Command::new(&args[0])
            .args(&args[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("не запустился {}: {e}", args[0]))?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = Vec::new();
            while stdout.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                if tx.send(Some(String::from_utf8_lossy(&line).into_owned())).is_err() {
                    return;
                }
                line.clear();
            }
            let _ = tx.send(None); // процесс закончился
        });
        Ok(Self { child, stdin, lines })
    }

    fn request(&mut self, payload: &Value, timeout: Duration) -> Result<Value, String> {
        let line = format!("{}\n", ascii_json(payload));
        if let Err(e) = self.stdin.write_all(line.as_bytes()).and_then(|_| self.stdin.flush()) {
            return Err(format!("процесс не принял запрос: {e}"));
        }
        match self.lines.recv_timeout(timeout) {
            Ok(Some(line)) => serde_json::from_str(&line).map_err(|_| format!("непонятный ответ: {}", line.chars().take(200).collect::<String>())),
            Ok(None) | Err(RecvTimeoutError::Disconnected) => Err("процесс завершился".into()),
            Err(RecvTimeoutError::Timeout) => Err("процесс не ответил".into()),
        }
    }

    fn close(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Несколько одинаковых процессов: запросы из разных потоков не ждут друг друга.
struct Pool {
    args: Vec<String>,
    inner: Mutex<PoolState>,
}

#[derive(Default)]
struct PoolState {
    idle: Vec<LineWorker>,
    busy: usize,
    closed: bool,
}

impl Pool {
    fn call(&self, payload: &Value, timeout: Duration) -> Result<Value, String> {
        let worker = {
            let mut state = self.inner.lock().unwrap();
            if state.closed {
                return Err("программа закрывается".into());
            }
            let worker = state.idle.pop();
            if worker.is_none() && state.busy >= POOL_SIZE {
                return Err("все процессы заняты".into());
            }
            state.busy += 1;
            worker
        };
        let mut worker = match worker.map_or_else(|| LineWorker::start(&self.args), Ok) {
            Ok(worker) => worker,
            Err(e) => {
                self.inner.lock().unwrap().busy -= 1;
                return Err(e);
            }
        };
        let reply = worker.request(payload, timeout);
        let mut state = self.inner.lock().unwrap();
        state.busy -= 1;
        if reply.is_err() || state.closed {
            drop(state);
            worker.close();
        } else {
            state.idle.push(worker);
        }
        reply
    }

    fn close(&self) {
        let idle = {
            let mut state = self.inner.lock().unwrap();
            state.closed = true;
            std::mem::take(&mut state.idle)
        };
        idle.into_iter().for_each(LineWorker::close);
    }
}

static POOLS: OnceLock<Mutex<HashMap<&'static str, &'static Pool>>> = OnceLock::new();

fn pool(name: &'static str, args: impl FnOnce() -> Vec<String>) -> &'static Pool {
    let mut pools = POOLS.get_or_init(Default::default).lock().unwrap();
    pools.entry(name).or_insert_with(|| Box::leak(Box::new(Pool { args: args(), inner: Default::default() })))
}

pub fn close_all() {
    if let Some(pools) = POOLS.get() {
        pools.lock().unwrap().values().for_each(|pool| pool.close());
    }
}

fn powershell_args(script: &Path) -> Vec<String> {
    ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
        .iter()
        .map(|s| s.to_string())
        .chain([script.display().to_string()])
        .collect()
}

/// Выполнить скрипт в постоянном PowerShell: код выхода и вывод.
pub fn run_powershell(script: &Path, env: &[(&str, &str)], timeout: Duration) -> Result<(i32, String), String> {
    let pool = pool("powershell", || powershell_args(&self::script("ps_host.ps1", PS_HOST)));
    let env: serde_json::Map<String, Value> = env.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    let reply = pool.call(&json!({"script": script.display().to_string(), "env": env}), timeout)?;
    Ok((reply["code"].as_i64().unwrap_or(1) as i32, reply["out"].as_str().unwrap_or("").to_string()))
}

/// Выполнить команду в WSL через постоянный процесс (вместо `wsl.exe -e …`).
pub fn run_in_wsl(args: &[String], timeout: Duration) -> Result<Output, String> {
    let pool = pool("wsl", || ["wsl.exe", "-e", "python3", "-u", "-c", WSL_SERVER].iter().map(|s| s.to_string()).collect());
    let reply = pool.call(&json!({"args": args, "timeout": timeout.as_secs()}), timeout + Duration::from_secs(2))?;
    Ok(Output {
        code: reply["code"].as_i64().unwrap_or(127) as i32,
        stdout: reply["out"].as_str().unwrap_or("").to_string(),
        stderr: reply["err"].as_str().unwrap_or("").to_string(),
    })
}

/// Одиночный запуск без окна консоли, с ограничением времени.
pub fn run_once(args: &[String], env: &[(&str, &str)], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| e.to_string())?;
    let read = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("не ответил за {} с", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output {
        code: status.code().unwrap_or(-1),
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Скрипт PowerShell — в постоянном процессе, а если он недоступен — отдельным запуском.
pub fn powershell_script(script: &Path, env: &[(&str, &str)], timeout: Duration) -> Result<(i32, String), String> {
    match run_powershell(script, env, timeout) {
        Ok(reply) => Ok(reply),
        Err(_) => {
            let done = run_once(&powershell_args(script), env, timeout)?;
            let text = if done.code == 0 { done.stdout } else { done.stderr };
            Ok((done.code, text))
        }
    }
}

/// herdr в WSL — через постоянный процесс WSL (без ~0,13 с на запуск `wsl.exe`).
pub fn run_command(args: &[String]) -> Result<Output, String> {
    let timeout = Duration::from_secs(10);
    let first = args[0].to_ascii_lowercase();
    if args.len() > 2 && (first == "wsl.exe" || first == "wsl") && args[1] == "-e" {
        if let Ok(done) = run_in_wsl(&args[2..], timeout) {
            return Ok(done);
        }
    }
    run_once(args, &[], timeout)
}

/// Запустить процессы заранее, чтобы первое нажатие кнопки не ждало их запуска.
pub fn warm_up(wsl: bool) {
    // `& Out-Null` — ничего не делает.
    let _ = run_powershell(Path::new("Out-Null"), &[], Duration::from_secs(20));
    if wsl {
        let _ = run_in_wsl(&["true".to_string()], Duration::from_secs(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_ascii() {
        let value = json!({"a": "й😀 x"});
        let text = ascii_json(&value);
        assert!(text.is_ascii());
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), value);
    }

    #[test]
    fn powershell_host_runs_script_and_reports_exit_code() {
        let path = script("test_exit.ps1", "Write-Output \"привет $env:AC_TEST\"\nexit 3\n");
        let (code, out) = run_powershell(&path, &[("AC_TEST", "мир")], Duration::from_secs(30)).unwrap();
        assert_eq!((code, out.trim()), (3, "привет мир"));
        // Второй запрос — тем же процессом.
        let (code, _) = run_powershell(&path, &[("AC_TEST", "x")], Duration::from_secs(5)).unwrap();
        assert_eq!(code, 3);
    }
}
