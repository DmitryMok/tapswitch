//! Значок в трее и цикл сообщений Windows (работает в главном потоке до «Выход»).
//! Иконки — из ресурсов exe (см. build.rs): 1 — обычная, 2 — с зелёной точкой, телефон подключён.

use std::sync::atomic::{AtomicU32, Ordering};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, GetSystemMetrics, MSG, PostThreadMessageW, SM_CXSMICON, TranslateMessage, WM_APP,
    WM_QUIT,
};

use crate::{autostart, i18n, log, win};

/// Сообщение потоку трея: телефон подключился (wParam = 1) или отключился.
const WM_PHONE: u32 = WM_APP + 1;
/// Сообщение потоку трея: автозапуск включили или выключили на странице управления.
const WM_AUTOSTART: u32 = WM_APP + 2;
/// Сообщение потоку трея: сменился язык интерфейса.
const WM_LANGUAGE: u32 = WM_APP + 3;
static CONNECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Перевести меню и подсказку на новый язык (из любого потока).
pub fn language_changed() {
    let thread = THREAD.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe { PostThreadMessageW(thread, WM_LANGUAGE, 0, 0) };
    }
}
static THREAD: AtomicU32 = AtomicU32::new(0);

/// Сообщить трею, подключён ли телефон (из любого потока).
pub fn phone_changed(connected: bool) {
    let thread = THREAD.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe { PostThreadMessageW(thread, WM_PHONE, connected as usize, 0) };
    }
}

/// Обновить флажок «Запускать вместе с Windows» в меню (из любого потока).
pub fn autostart_changed() {
    let thread = THREAD.load(Ordering::Relaxed);
    if thread != 0 {
        unsafe { PostThreadMessageW(thread, WM_AUTOSTART, 0, 0) };
    }
}

fn icon(connected: bool) -> Option<Icon> {
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
    Icon::from_resource(if connected { 2 } else { 1 }, Some((size, size))).ok()
}

fn tooltip(title: &str, connected: bool) -> String {
    format!("{title} — {}", i18n::t(if connected { "tray_phone_on" } else { "tray_phone_off" }))
}

/// page_url — страница управления; щелчок по значку открывает её.
pub fn run(title: &str, page_url: String) {
    let open = MenuItem::new(i18n::t("tray_open"), true, None);
    let startup = CheckMenuItem::new(i18n::t("tray_autostart"), true, autostart::enabled(), None);
    let quit = MenuItem::new(i18n::t("tray_quit"), true, None);
    let menu = Menu::new();
    menu.append_items(&[&open, &startup, &PredefinedMenuItem::separator(), &quit]).expect("меню трея");

    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip(tooltip(title, false));
    if let Some(icon) = icon(false) {
        builder = builder.with_icon(icon);
    }
    let tray = builder.build().expect("значок в трее");

    // Обработчики вызываются из оконной процедуры; выход — сообщением потоку цикла.
    let thread = unsafe { GetCurrentThreadId() };
    THREAD.store(thread, Ordering::Relaxed);
    let (open_id, startup_id, quit_id) = (open.id().clone(), startup.id().clone(), quit.id().clone());
    let url = page_url.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == open_id {
            win::open_url(&url);
        } else if event.id == startup_id {
            // Пункт уже переключился сам; состояние берётся из реестра.
            if let Err(error) = autostart::set(!autostart::enabled()) {
                log::write(&format!("Автозапуск не изменён: {error}"));
            }
            autostart_changed();
        } else if event.id == quit_id {
            unsafe { PostThreadMessageW(thread, WM_QUIT, 0, 0) };
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            win::open_url(&page_url);
        }
    }));

    unsafe {
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            if message.hwnd.is_null() && message.message == WM_PHONE {
                let connected = message.wParam != 0;
                CONNECTED.store(connected, Ordering::Relaxed);
                let _ = tray.set_icon(icon(connected));
                let _ = tray.set_tooltip(Some(tooltip(title, connected)));
                continue;
            }
            if message.hwnd.is_null() && message.message == WM_LANGUAGE {
                open.set_text(i18n::t("tray_open"));
                startup.set_text(i18n::t("tray_autostart"));
                quit.set_text(i18n::t("tray_quit"));
                let _ = tray.set_tooltip(Some(tooltip(title, CONNECTED.load(Ordering::Relaxed))));
                continue;
            }
            if message.hwnd.is_null() && message.message == WM_AUTOSTART {
                startup.set_checked(autostart::enabled());
                continue;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    THREAD.store(0, Ordering::Relaxed);
}
