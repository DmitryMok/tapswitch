//! Tapswitch: значок в трее и сервер панели кнопок для телефона и браузера.
//!
//! Rust-версия (см. «03 Work/Архитектурная концепция.md»): форматы `config.yaml`,
//! `scenarios/*.yaml` и `state.json` и страница панели — общие с Python-версией.
//! Где лежат данные — см. `places.rs`.
//! Жесты на кнопках выполняют сценарии; сессии — herdr, DeepSeek Harness, вкладки браузера
//! и окна любых приложений.

// Без окна консоли в релизной сборке; в отладочной консоль остаётся для журнала.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod autostart;
mod browser;
mod config;
mod dsh;
mod opencode;
mod engine;
mod gestures;
mod i18n;
mod keystrokes;
mod library;
mod log;
mod look;
mod panel;
mod png;
mod places;
mod qr;
mod scenarios;
mod server;
mod sessions;
mod state;
mod template;
mod terminal;
mod tray;
mod win;
mod windows;
mod workers;

use std::net::TcpListener;
use std::sync::Arc;

const TITLE: &str = "Tapswitch";

/// На порту уже работает эта же программа (вторая копия)?
fn running_copy(port: u16) -> bool {
    use std::io::{Read, Write};
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    let request = format!("GET /api/ping HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    let mut answer = String::new();
    stream.write_all(request.as_bytes()).is_ok()
        && stream.read_to_string(&mut answer).is_ok()
        && answer.ends_with(server::PING_REPLY)
}

/// Имя мьютекса одной копии: своё для каждой папки данных — две копии с одними данными
/// портили бы state.json, а проверочная копия с другой папкой (AGENT_CONTROL_APP) не мешает.
fn instance_name(data: &std::path::Path) -> String {
    let text = data.to_string_lossy().to_lowercase();
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    format!("Local\\AgentControl_{hash:016x}")
}

/// Язык интерфейса из state.json — чтобы сообщение второй копии было на языке программы.
fn state_language(path: &std::path::Path) -> &'static str {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let data: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    i18n::resolve(data["settings"][i18n::SETTING].as_str().unwrap_or(i18n::DEFAULT))
}

/// Вторая копия: сказать, что программа уже работает, и выйти. Страницу не открываем:
/// она забирает панель у телефона и показывает там «панель открыта на другом устройстве».
/// При автозапуске (`--no-browser`) сообщение не показываем, чтобы не мешать входу в Windows.
fn already_running(places: &places::Places, no_browser: bool) {
    if !no_browser {
        i18n::set(state_language(&places.state()));
        win::notice_box(TITLE, &i18n::t("already_running"));
    }
}

fn fail(text: &str) {
    log::open();
    log::write(text);
    win::message_box(TITLE, text);
}

fn make_sessions(config: &config::Config) -> (Arc<dyn sessions::Sessions>, Option<Arc<browser::TabRegistry>>) {
    let servers = config
        .servers
        .iter()
        .map(|s| sessions::HerdrServer::new(&s.id, s.command.clone(), Box::new(workers::run_command), s.detects_agents))
        .collect();
    let herdr = sessions::HerdrSessions::new(servers, Box::new(terminal::WindowsTerminal::new()));
    let mut backends: Vec<(&'static str, Box<dyn sessions::Backend>)> = vec![("herdr", Box::new(herdr))];
    let mut others: Vec<windows::Matcher> = vec![Box::new(terminal::is_terminal_window)];
    let mut browser_registry = None;
    if let Some(data) = &config.dsh_data {
        let store = dsh::DshStore::new(dsh::expand_env(data).into());
        backends.push((dsh::KIND, Box::new(dsh::DshSessions::new(Box::new(dsh::WindowsDshUi::new()), store))));
        others.push(Box::new(dsh::is_dsh_window));
    }
    if let Some(data) = &config.opencode_data {
        let store = opencode::OpencodeStore::new(dsh::expand_env(data).into());
        let ui = opencode::WindowsOpencodeUi::new();
        backends.push((opencode::KIND, Box::new(opencode::OpencodeSessions::new(Box::new(ui), store))));
        others.push(Box::new(opencode::is_opencode_window));
    }
    if let Some(settings) = &config.browser {
        let registry = Arc::new(browser::TabRegistry::default());
        browser_registry = Some(registry.clone());
        let ids = std::iter::once(browser::EXTENSION_ID.to_string()).chain(settings.extension_ids.iter().cloned()).collect();
        let bridge: Box<dyn browser::Bridge> = match browser::BrowserBridge::start(registry.clone(), settings.port, ids) {
            Ok(bridge) => Box::new(bridge),
            Err(error) => {
                log::write(&format!("Вкладки браузера недоступны: порт {}: {error}", settings.port));
                Box::new(browser::NoBridge)
            }
        };
        let tabs = Arc::new(browser::BrowserTabs::new(registry, bridge, Box::new(browser::WindowsBrowserDesktop)));
        let matcher = tabs.clone();
        others.push(Box::new(move |window| sessions::Backend::matches(&*matcher, window)));
        backends.push((browser::KIND, Box::new(tabs)));
    }
    // Любое другое окно — окно приложения; проверяется последним.
    let apps = windows::WindowSessions::new(Box::new(windows::WindowsDesktop::new(others)));
    backends.push((windows::KIND, Box::new(apps)));
    (Arc::new(sessions::Router::new(backends, Box::new(windows::WindowsScreen))), browser_registry)
}

fn main() {
    i18n::set(i18n::DEFAULT);
    let places = match places::Places::find() {
        Ok(places) => places,
        Err(error) => return fail(&i18n::tf("error_data_folder", &[&error])),
    };
    log::prepare(&places.log());

    let config = match config::load(&places.config) {
        Ok(config) => config,
        Err(error) => return fail(&i18n::tf("error_config", &[&places.config.display(), &error])),
    };
    let no_browser = std::env::args().any(|arg| arg == autostart::FLAG);
    // Одна копия на папку данных — даже если обе запущены одновременно или порт сменили
    // в настройках (тогда прежняя копия ещё работает на старом порту).
    if !win::claim_single_instance(&instance_name(&places.data)) {
        return already_running(&places, no_browser);
    }
    let state_path = places.state();
    let mut state = match state::State::load(&state_path) {
        Ok(state) => state,
        Err(error) => return fail(&i18n::tf("error_state", &[&state_path.display(), &error])),
    };
    i18n::set(i18n::resolve(state.setting(i18n::SETTING).as_str().unwrap_or(i18n::DEFAULT)));
    let key = match state.web_key() {
        Ok(key) => key,
        Err(error) => return fail(&i18n::tf("error_key", &[&error])),
    };

    let page_url = format!("http://127.0.0.1:{}/", config.port);
    let listener = match TcpListener::bind(("0.0.0.0", config.port)) {
        Ok(listener) => listener,
        // Вторая копия: сказать, что программа уже запущена, и открыть её страницу
        // (её журнал не трогать).
        Err(_) if running_copy(config.port) => {
            return already_running(&places, no_browser);
        }
        Err(error) => {
            return fail(&i18n::tf("error_port", &[&config.port, &error]));
        }
    };

    log::open();
    log::write(&format!("Запуск, {}", places.describe()));
    if autostart::migrate() {
        log::write("Автозапуск перенесён с Agent Control на Tapswitch");
    }

    let (scenarios, errors) = scenarios::load_all(&places.scenarios);
    for error in errors {
        log::write(&format!("Сценарий не загружен: {error}"));
    }
    let icons = Arc::new(places::Icons::new(places.icons.clone()));
    let (events_tx, events) = std::sync::mpsc::channel();
    let show_titles = state.setting(engine::ICON_TITLES).as_bool().unwrap_or(false);
    let confirm_bind = state.setting(engine::CONFIRM_BIND).as_bool().unwrap_or(false);
    let panel = Arc::new(panel::Panel::new(
        config.rows,
        config.cols,
        config.hold_ms,
        icons.clone(),
        show_titles,
        events_tx.clone(),
    ));
    let (sessions, browser_registry) = make_sessions(&config);
    let mut engine = engine::Engine::new(
        panel.clone(),
        scenarios,
        state,
        sessions.clone(),
        Arc::new(log::write),
        config.double_click_ms,
        config.hold_ms,
        config.default_scenario.clone(),
    );
    engine.confirm_bind = confirm_bind;
    engine.render_all();
    let polls = app::Polls {
        focus_ms: config.focus_poll_ms,
        status_ms: config.status_poll_ms,
        warm_up_wsl: config.servers.iter().any(|s| s.command[0].to_ascii_lowercase().starts_with("wsl")),
    };
    let page_events = events_tx.clone();
    app::spawn(engine, events_tx, events, sessions, polls);
    let setup = server::Setup {
        port: config.port,
        host: config.host.clone(),
        config: places.config.clone(),
        scenarios: places.scenarios.clone(),
        user_icons: places.user_icons(),
        browsers: browser_registry,
        events: page_events,
    };
    log::write(&format!("Панель для телефона: {}", server::mobile_url(&config.host, config.port, &key)));
    server::spawn(listener, panel, key, icons, setup);

    // --no-browser: не открывать страницу при запуске (автозапуск и проверки).
    if !no_browser {
        win::open_url(&page_url);
    }
    tray::run(TITLE, page_url);
    workers::close_all();
    log::write("Выход");
}
