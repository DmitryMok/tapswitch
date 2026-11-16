//! HTTP и WebSocket на одном порту, как у Python-версии.
//! Каждое подключение — свой поток: подключений единицы, а кода так меньше всего.

use std::io::{ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::Message;

use crate::app::{Event, PageCommand};
use crate::panel::Panel;
use crate::places::Icons;
use crate::{autostart, config, i18n, keystrokes, log, qr, scenarios, win};

const PANEL_HTML: &[u8] = include_bytes!("../../app/agent_control/web/panel.html");

/// Версия страницы панели (FNV-1a от её текста): открытая на телефоне старая страница
/// сравнивает её с прежней и перезагружается — иначе после обновления программы телефон
/// продолжал бы работать со старой страницей (она лишь переподключается).
pub fn panel_version() -> String {
    let hash = PANEL_HTML.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("{hash:016x}")
}
/// Страница управления — только для этого компьютера.
const SETUP_HTML: &[u8] = include_bytes!("../web/setup.html");
/// Ответ на /api/ping: так вторая копия узнаёт, что порт занят этой же программой.
pub const PING_REPLY: &str = "agent_control";
const MAX_BODY: usize = 64 * 1024;
/// Своя картинка кнопки: страница присылает PNG до 256×256 (обычно 128×128, десятки КБ).
const MAX_IMAGE: usize = 512 * 1024;
const MAX_IMAGE_SIDE: u32 = 256;
// Беззвучное видео в цвет фона: пока оно играет, телефон не гасит экран
// (Wake Lock API браузер даёт только по HTTPS, а панель открывается по HTTP).
const KEEPAWAKE_MP4: &[u8] = include_bytes!("../../app/agent_control/web/keepawake.mp4");
const KEEPAWAKE_WEBM: &[u8] = include_bytes!("../../app/agent_control/web/keepawake.webm");

/// Значок вкладки браузера и знак для страниц — тот же рисунок, что у exe и трея.
const FAVICON: &[u8] = include_bytes!("../assets/app.ico");
const ICON_SVG: &[u8] = include_bytes!("../assets/icon.svg");

const MAX_HEADER: usize = 8192;
/// Как часто поток WebSocket проверяет исходящие сообщения, пока страница молчит.
const POLL: Duration = Duration::from_millis(50);
/// Телефон, который шлёт ping (раз в 2 с), молчит дольше — связь потеряна: считать отключённым,
/// не дожидаясь, пока это заметит TCP (без отправки данных — никогда).
const PHONE_SILENCE: Duration = Duration::from_secs(10);

/// Что нужно странице управления.
pub struct Setup {
    pub port: u16,
    /// Адрес для телефона из настроек; None — определяется сам.
    pub host: Option<String>,
    pub config: PathBuf,
    /// Папка своих сценариев: при смене языка сценарии читаются заново.
    pub scenarios: PathBuf,
    /// Папка своих картинок кнопок.
    pub user_icons: PathBuf,
    /// Подключения расширений браузеров; None — в настройках нет раздела browser.
    pub browsers: Option<Arc<crate::browser::TabRegistry>>,
    pub events: Sender<Event>,
}

struct Context {
    panel: Arc<Panel>,
    key: String,
    icons: Arc<Icons>,
    setup: Setup,
    events: Mutex<Sender<Event>>,
}

pub fn spawn(listener: TcpListener, panel: Arc<Panel>, key: String, icons: Arc<Icons>, setup: Setup) {
    let events = Mutex::new(setup.events.clone());
    let context = Arc::new(Context { panel, key, icons, setup, events });
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let context = context.clone();
            std::thread::spawn(move || handle(stream, &context));
        }
    });
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> &str {
        self.headers.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v.as_str())
    }

    fn param(&self, name: &str) -> String {
        self.query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| percent_decode(v))
            .unwrap_or_default()
    }
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = || u8::from_str_radix(std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?, 16).ok();
        match bytes[i] {
            b'%' if hex().is_some() => {
                out.push(hex().unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Прочитать заголовок запроса, не забирая его из сокета: при WebSocket его разберёт tungstenite.
fn peek_request(stream: &TcpStream) -> Option<Request> {
    let mut buffer = [0u8; MAX_HEADER];
    let head = loop {
        let size = stream.peek(&mut buffer).ok()?;
        if size == 0 {
            return None;
        }
        if let Some(end) = buffer[..size].windows(4).position(|w| w == b"\r\n\r\n") {
            break String::from_utf8_lossy(&buffer[..end]).into_owned();
        }
        if size == MAX_HEADER {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    Some(Request { method, path: percent_decode(path), query: query.to_string(), headers })
}

fn handle(mut stream: TcpStream, context: &Context) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some(request) = peek_request(&stream) else { return };
    let authorized = request.param("key") == context.key;
    let path = request.path.as_str();
    if path == "/" || path.starts_with("/api/") {
        drain_head(&mut stream);
        if !is_local(&stream, &request, context.setup.port) {
            let text = "Страница управления открывается только на этом компьютере";
            return reply(&mut stream, 403, "text/plain; charset=utf-8", text.as_bytes(), false, "");
        }
        return setup_page(&mut stream, &request, context);
    }
    if request.method != "GET" {
        return reply(&mut stream, 405, "text/plain", b"method not allowed", false, "");
    }
    if path == "/ws" && request.header("upgrade").eq_ignore_ascii_case("websocket") {
        if !authorized {
            return reply(&mut stream, 403, "text/plain", b"wrong key", false, "");
        }
        return websocket(stream, context, &request.param("page"));
    }
    // Заголовок прочитан через peek — забрать его из сокета перед ответом.
    drain_head(&mut stream);
    match path {
        "/mobile" if authorized => reply(&mut stream, 200, "text/html; charset=utf-8", PANEL_HTML, false, ""),
        "/mobile" => reply(&mut stream, 403, "text/plain; charset=utf-8", "Неверный ключ".as_bytes(), false, ""),
        "/favicon.ico" => reply(&mut stream, 200, "image/x-icon", FAVICON, true, ""),
        "/icon.svg" => reply(&mut stream, 200, "image/svg+xml", ICON_SVG, true, ""),
        "/keepawake.mp4" => media(&mut stream, &request, "video/mp4", KEEPAWAKE_MP4),
        "/keepawake.webm" => media(&mut stream, &request, "video/webm", KEEPAWAKE_WEBM),
        _ if path.starts_with("/appicon/") && path.ends_with(".png") => {
            let id = &path["/appicon/".len()..path.len() - ".png".len()];
            match context.panel.program_icon(id) {
                Some((kind, body)) => reply(&mut stream, 200, kind, &body, true, ""),
                None => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
            }
        }
        // Живые иконки окон с кнопок: ключ меняется вместе с картинкой, поэтому кэшируются.
        _ if path.starts_with("/winicon/") && path.ends_with(".png") => {
            match crate::live_icon::png(&path["/winicon/".len()..path.len() - ".png".len()]) {
                Some(body) => reply(&mut stream, 200, "image/png", &body, true, ""),
                None => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
            }
        }
        // Значки библиотеки для кнопок — и телефону, и странице управления.
        _ if path.starts_with("/lib/") && path.ends_with(".svg") => {
            match crate::library::svg(&path["/lib/".len()..path.len() - ".svg".len()]) {
                Some(body) => reply(&mut stream, 200, "image/svg+xml", body.as_bytes(), true, ""),
                None => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
            }
        }
        // Свои картинки кнопок: имя — только хэш, путь собрать из него нельзя.
        _ if path.starts_with("/usericon/") && path.ends_with(".png") => {
            let hash = &path["/usericon/".len()..path.len() - ".png".len()];
            let body = crate::look::is_image_hash(hash)
                .then(|| std::fs::read(context.setup.user_icons.join(format!("{hash}.png"))).ok())
                .flatten();
            match body {
                Some(body) => reply(&mut stream, 200, "image/png", &body, true, ""),
                None => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
            }
        }
        _ if path.starts_with("/icons/") && path.ends_with(".svg") => {
            let name = &path["/icons/".len()..path.len() - ".svg".len()];
            match context.icons.read(name) {
                Some(body) => reply(&mut stream, 200, "image/svg+xml", &body, true, ""),
                None => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
            }
        }
        _ => reply(&mut stream, 404, "text/plain", b"not found", false, ""),
    }
}

/// Запрос с этой машины и к адресу 127.0.0.1/localhost: чужой сайт в браузере не сможет
/// подменить адрес (DNS rebinding), а телефон в сети страницу управления не откроет.
fn is_local(stream: &TcpStream, request: &Request, port: u16) -> bool {
    let loopback = stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());
    let host = request.header("host");
    loopback && [format!("127.0.0.1:{port}"), format!("localhost:{port}")].iter().any(|h| h == host)
}

fn read_body(stream: &mut TcpStream, request: &Request) -> Option<Value> {
    use std::io::Read;
    let length: usize = request.header("content-length").parse().ok()?;
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn read_raw(stream: &mut TcpStream, request: &Request, limit: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let length: usize = request.header("content-length").parse().ok()?;
    if length > limit {
        return None;
    }
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    Some(body)
}

/// Ширина и высота PNG из заголовка или None, если это не PNG.
fn png_size(data: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 24 || !data.starts_with(SIGNATURE) || &data[12..16] != b"IHDR" {
        return None;
    }
    let number = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
    Some((number(16), number(20)))
}

/// Сохранить свою картинку кнопки; имя — хэш содержимого (одинаковая хранится один раз).
/// Принимается только PNG: картинку уменьшает и переводит в PNG страница, программе
/// не нужно разбирать форматы, а SVG приходит уже растром.
fn save_user_icon(folder: &std::path::Path, png: &[u8]) -> Result<String, String> {
    let (width, height) = png_size(png).ok_or("нужна картинка PNG")?;
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err(format!("картинка больше {MAX_IMAGE_SIDE}×{MAX_IMAGE_SIDE}"));
    }
    let hash = png.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let hash = format!("{hash:016x}");
    let path = folder.join(format!("{hash}.png"));
    if !path.is_file() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
        std::fs::write(&path, png).map_err(|e| e.to_string())?;
        log::write(&format!("Своя картинка кнопки сохранена: {hash}.png"));
    }
    Ok(hash)
}

/// Открытые окна программ (по одному на программу): название, путь к exe и адрес иконки.
fn program_windows(context: &Context) -> Vec<Value> {
    let mut seen = std::collections::HashSet::new();
    win::top_level_windows(true)
        .into_iter()
        .filter(|&window| win::is_app_window(window))
        .filter_map(|window| {
            let path = win::process_path(window);
            if path.is_empty() || !seen.insert(path.to_lowercase()) {
                return None;
            }
            let name = std::path::Path::new(&path).file_stem()?.to_string_lossy().into_owned();
            let url = crate::engine::Device::icon_url(context.panel.as_ref(), &path);
            Some(json!({"title": win::window_title(window), "name": name, "path": path, "icon": url}))
        })
        .collect()
}

/// Иконка программы годится: файл .exe есть.
fn check_program(path: &str) -> Result<(), String> {
    if crate::look::is_program_path(path) && std::path::Path::new(path).is_file() {
        Ok(())
    } else {
        Err("нет такой программы: нужен полный путь к .exe".into())
    }
}

fn json_reply(stream: &mut TcpStream, value: &Value) {
    reply(stream, 200, "application/json; charset=utf-8", value.to_string().as_bytes(), false, "");
}

/// Адрес панели для телефона.
pub fn mobile_url(host: &Option<String>, port: u16, key: &str) -> String {
    let host = host.clone().unwrap_or_else(win::local_ip);
    format!("http://{host}:{port}/mobile?key={key}")
}

fn ask_engine(context: &Context, command: PageCommand) -> Value {
    let (tx, rx) = std::sync::mpsc::channel();
    if context.events.lock().unwrap().send(Event::Page(command, tx)).is_err() {
        return json!({"ok": false, "error": "программа завершается"});
    }
    // Движок может быть занят открытием сессии — ждать недолго.
    rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| json!({"ok": false, "error": "программа занята, повторите"}))
}

fn setup_page(stream: &mut TcpStream, request: &Request, context: &Context) {
    let setup = &context.setup;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => reply(stream, 200, "text/html; charset=utf-8", SETUP_HTML, false, ""),
        ("GET", "/api/ping") => reply(stream, 200, "text/plain", PING_REPLY.as_bytes(), false, ""),
        ("GET", "/api/qr.svg") => match qr::svg(&mobile_url(&setup.host, setup.port, &context.key)) {
            Some(svg) => reply(stream, 200, "image/svg+xml", svg.as_bytes(), false, ""),
            None => reply(stream, 404, "text/plain", b"too long", false, ""),
        },
        // Список значков библиотеки с ключевыми словами — для поиска в окне «Настроить кнопку».
        ("GET", "/api/library") => {
            let list: Vec<Value> =
                crate::library::icons().iter().map(|i| json!({"name": i.name, "ru": i.ru, "en": i.en})).collect();
            json_reply(stream, &json!(list))
        }
        ("GET", "/api/log") => json_reply(stream, &json!(log::recent())),
        ("GET", "/api/strings") => {
            let code = request.param("lang");
            let code = if i18n::dictionary(&code).is_some() { code.as_str() } else { i18n::current() };
            json_reply(stream, &json!(i18n::dictionary(code)))
        }
        ("GET", "/api/state") => {
            let mut state = ask_engine(context, PageCommand::Overview);
            let peer = context.panel.connected_peer();
            let saved = config::web_settings(&setup.config);
            state["phone"] = json!({
                "connected": peer.is_some(),
                "local": peer.as_deref().is_some_and(crate::panel::is_local_peer),
                "peer": peer,
            });
            let browsers = setup.browsers.as_ref().map_or_else(Vec::new, |registry| registry.browsers());
            state["browser"] = json!({"connected": !browsers.is_empty(), "browsers": browsers});
            state["url"] = json!(mobile_url(&setup.host, setup.port, &context.key));
            state["local_url"] = json!(format!("/mobile?key={}", context.key));
            let (rows, cols) = context.panel.size();
            state["layout"] = json!({"rows": rows, "cols": cols});
            state["autostart"] = json!(autostart::enabled());
            let languages: Vec<Value> = i18n::languages()
                .into_iter()
                .map(|code| {
                    let dictionary = i18n::dictionary(code);
                    json!({
                        "code": code,
                        "name": dictionary.and_then(|d| d.get("language_name")),
                        "short": dictionary.and_then(|d| d.get("language_short")),
                    })
                })
                .collect();
            state["language"] = json!({"current": i18n::current(), "languages": languages});
            state["advanced"] = json!({
                "port": setup.port,
                "host": setup.host,
                "detected_host": win::local_ip(),
                "saved_port": saved.0,
                "saved_host": saved.1,
                "folder": program_folder().display().to_string(),
                "version": env!("CARGO_PKG_VERSION"),
            });
            json_reply(stream, &state)
        }
        // Открытые окна программ — выбрать иконку программы для кнопки.
        ("GET", "/api/windows") => json_reply(stream, &json!(program_windows(context))),
        ("POST", "/api/icon") => {
            let body = read_raw(stream, request, MAX_IMAGE);
            if request.header("x-agent-control") != "1" {
                return reply(stream, 403, "text/plain", b"forbidden", false, "");
            }
            let answer = match body {
                None => json!({"ok": false, "error": "картинка больше 512 КБ"}),
                Some(png) => match save_user_icon(&context.setup.user_icons, &png) {
                    Ok(hash) => json!({"ok": true, "hash": hash, "url": format!("/usericon/{hash}.png")}),
                    Err(error) => json!({"ok": false, "error": error}),
                },
            };
            json_reply(stream, &answer)
        }
        ("POST", "/api/action") => {
            let body = read_body(stream, request);
            // Свой заголовок: браузер не даст чужому сайту отправить его без разрешения CORS.
            if request.header("x-agent-control") != "1" {
                return reply(stream, 403, "text/plain", b"forbidden", false, "");
            }
            let Some(body) = body else {
                return reply(stream, 400, "text/plain", b"bad request", false, "");
            };
            json_reply(stream, &action(context, &body))
        }
        _ => reply(stream, 404, "text/plain", b"not found", false, ""),
    }
}

/// Папка, где лежит программа (exe).
fn program_folder() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)).unwrap_or_default()
}

fn action(context: &Context, body: &Value) -> Value {
    let key = body["key"].as_u64().map(|k| k as usize);
    let failed = |error: String| json!({"ok": false, "error": error});
    match body["action"].as_str().unwrap_or("") {
        "scenario" if key.is_some() => {
            let scenario = body["scenario"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
            ask_engine(context, PageCommand::SetScenario(key.unwrap(), scenario))
        }
        "clear" if key.is_some() => ask_engine(context, PageCommand::Clear(key.unwrap())),
        "setting" => {
            let (Some(name), Some(value)) = (body["name"].as_str(), body["value"].as_bool()) else {
                return failed("нет имени или значения".into());
            };
            ask_engine(context, PageCommand::Setting(name.to_string(), value))
        }
        "autostart" => match autostart::set(body["value"].as_bool().unwrap_or(false)) {
            Ok(()) => {
                crate::tray::autostart_changed();
                log::write(if autostart::enabled() { "Автозапуск включён" } else { "Автозапуск выключен" });
                json!({"ok": true})
            }
            Err(error) => failed(error),
        },
        "web" => {
            let port = body["port"].as_u64();
            if port.is_some_and(|p| !(1024..=65535).contains(&p)) {
                return failed("порт — число от 1024 до 65535".into());
            }
            let host = body["host"].as_str().map(str::trim);
            match config::save_web(&context.setup.config, port.map(|p| p as u16), host) {
                Ok(()) => {
                    log::write("Настройки панели сохранены, применятся после перезапуска программы");
                    json!({"ok": true})
                }
                Err(error) => failed(error),
            }
        }
        "language" => {
            let setting = body["value"].as_str().unwrap_or(i18n::DEFAULT);
            if i18n::dictionary(setting).is_none() {
                return failed("нет такого языка".into());
            }
            i18n::set(i18n::resolve(setting));
            let (scenarios, errors) = scenarios::load_all(&context.setup.scenarios);
            for error in errors {
                log::write(&format!("Сценарий не загружен: {error}"));
            }
            crate::tray::language_changed();
            ask_engine(context, PageCommand::Language(setting.to_string(), scenarios))
        }
        "open_folder" => {
            win::open_url(&program_folder().display().to_string());
            json!({"ok": true})
        }
        "profile" => match body["profile"].as_u64().filter(|&p| (p as usize) < crate::state::PROFILES) {
            Some(profile) => ask_engine(context, PageCommand::Profile(profile as usize)),
            None => failed("нет такого профиля".into()),
        },
        "keys" if key.is_some_and(|k| k < context.panel.key_count()) => {
            let combo = body["combo"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
            if let Some(Err(error)) = combo.as_deref().map(keystrokes::parse) {
                return failed(error);
            }
            ask_engine(context, PageCommand::Keys(key.unwrap(), combo))
        }
        "appearance" if key.is_some_and(|k| k < context.panel.key_count()) => {
            let field = |name: &str| body[name].as_str();
            let look = crate::look::Look::parse(field("caption"), field("icon"), field("color")).and_then(|look| {
                let icon = look.icon.as_deref().unwrap_or("");
                if let Some(path) = icon.strip_prefix(crate::look::APP) {
                    check_program(path)?;
                }
                if let Some(hash) = icon.strip_prefix(crate::look::IMG) {
                    if !context.setup.user_icons.join(format!("{hash}.png")).is_file() {
                        return Err("картинка не найдена — загрузите её снова".into());
                    }
                }
                Ok(look)
            });
            match look {
                Ok(look) => ask_engine(context, PageCommand::Look(key.unwrap(), look)),
                Err(error) => failed(error),
            }
        }
        // Путь к .exe, введённый вручную: годится ли и адрес его иконки.
        "program" => {
            let path = body["path"].as_str().unwrap_or("").trim().trim_matches('"').to_string();
            match check_program(&path) {
                Ok(()) => json!({"ok": true, "path": path, "icon": crate::engine::Device::icon_url(context.panel.as_ref(), &path)}),
                Err(error) => failed(error),
            }
        }
        _ => failed("неизвестное действие".into()),
    }
}

fn drain_head(stream: &mut TcpStream) {
    use std::io::Read;
    let mut buffer = [0u8; MAX_HEADER];
    let Ok(size) = stream.peek(&mut buffer) else { return };
    if let Some(end) = buffer[..size].windows(4).position(|w| w == b"\r\n\r\n") {
        let _ = stream.read_exact(&mut buffer[..end + 4]);
    }
}

/// Браузер запрашивает видео по частям (Range) и без ответа 206 может считать его
/// непрокручиваемым потоком — тогда повтор по кругу срывается и экран гаснет.
fn media(stream: &mut TcpStream, request: &Request, content_type: &str, body: &[u8]) {
    match byte_range(request.header("range"), body.len()) {
        Some((start, end)) => {
            let extra = format!("Content-Range: bytes {start}-{end}/{}\r\n", body.len());
            reply(stream, 206, content_type, &body[start..=end], true, &extra)
        }
        None => reply(stream, 200, content_type, body, true, ""),
    }
}

/// «bytes=a-b» / «bytes=a-» / «bytes=-n» → (начало, конец включительно);
/// None — заголовка нет или он непонятен (тогда отдаётся весь файл).
fn byte_range(header: &str, size: usize) -> Option<(usize, usize)> {
    let spec = header.strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (first, last) = spec.trim().split_once('-')?;
    let (start, end) = if first.is_empty() {
        (size.saturating_sub(last.parse().ok()?), size - 1)
    } else {
        let start: usize = first.parse().ok()?;
        let end = if last.is_empty() { size - 1 } else { last.parse::<usize>().ok()?.min(size - 1) };
        (start, end)
    };
    (start <= end).then_some((start, end))
}

fn reply(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8], cache: bool, extra: &str) {
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let (ranges, cache_control) = if cache { ("Accept-Ranges: bytes\r\n", "max-age=3600") } else { ("", "no-store") };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n{ranges}{extra}Cache-Control: {cache_control}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(body));
}

fn websocket(stream: TcpStream, context: &Context, page: &str) {
    let peer = stream.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
    let _ = stream.set_read_timeout(None);
    let mut socket = match tungstenite::accept(stream) {
        Ok(socket) => socket,
        Err(error) => return log::write(&format!("WebSocket: {error}")),
    };
    // Короткий таймаут чтения: поток по очереди читает страницу и отправляет ей обновления.
    let _ = socket.get_ref().set_read_timeout(Some(POLL));
    let (id, outgoing) = context.panel.connect(&peer, page);
    // Панель на этом же ПК (вкладка в фоне шлёт ping редко) по молчанию не отключается.
    let watch = !crate::panel::is_local_peer(&peer);
    let mut pings = false;
    let mut heard = Instant::now();
    'session: loop {
        if watch && pings && heard.elapsed() > PHONE_SILENCE {
            log::write(&format!("Телефон не отвечает {} с", PHONE_SILENCE.as_secs()));
            break;
        }
        loop {
            match outgoing.try_recv() {
                Ok(text) => {
                    if socket.send(Message::text(text)).is_err() {
                        break 'session;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Подключение вытеснено другим устройством.
                Err(TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    break 'session;
                }
            }
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                heard = Instant::now();
                pings |= is_ping(text.as_str());
                context.panel.on_message(id, text.as_str());
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => heard = Instant::now(),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    context.panel.disconnect(id);
}

/// Сообщение проверки связи от страницы: значит, страница шлёт его регулярно.
fn is_ping(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|m| m["type"] == "ping")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_ping() {
        assert!(is_ping(r#"{"type":"ping"}"#));
        assert!(!is_ping(r#"{"type":"down","key":1}"#));
        assert!(!is_ping("ping"));
    }

    #[test]
    fn byte_ranges() {
        assert_eq!(byte_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(byte_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(byte_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(byte_range("bytes=0-5000", 1000), Some((0, 999)));
        assert_eq!(byte_range("bytes=900-100", 1000), None);
        assert_eq!(byte_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(byte_range("", 1000), None);
    }

    #[test]
    fn user_icons_are_png_named_by_hash() {
        let dir = std::env::temp_dir().join(format!("agent_control_usericons_{}", std::process::id()));
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend(128u32.to_be_bytes());
        png.extend(128u32.to_be_bytes());
        png.extend(b"rest of file");
        let hash = save_user_icon(&dir, &png).unwrap();
        assert!(crate::look::is_image_hash(&hash));
        assert_eq!(save_user_icon(&dir, &png).unwrap(), hash); // та же картинка — то же имя
        assert_eq!(std::fs::read(dir.join(format!("{hash}.png"))).unwrap(), png);
        let mut big = png.clone();
        big[16..20].copy_from_slice(&1000u32.to_be_bytes());
        assert!(save_user_icon(&dir, &big).is_err());
        assert!(save_user_icon(&dir, b"<svg onload=alert(1)></svg>").is_err());
        assert!(save_user_icon(&dir, b"").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_percent() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%D0%B9"), "й");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
