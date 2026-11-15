//! Обёртки над Windows API.

use std::net::UdpSocket;
use windows_sys::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::Foundation::{HWND, LPARAM};
use windows_sys::core::BOOL;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, ReleaseDC,
};
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::Shell::SHDefExtractIconW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, DestroyIcon, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow,
    GetIconInfo, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, HICON, ICONINFO, IsIconic,
    IsWindow, IsWindowVisible, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_RESTORE, SW_SHOWNORMAL,
    SetForegroundWindow, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn message_box(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

/// Сообщение-уведомление (не ошибка).
pub fn notice_box(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION) };
}

/// Открыть адрес в браузере по умолчанию.
pub fn open_url(url: &str) {
    let (verb, url) = (wide("open"), wide(url));
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL)
    };
}

/// Занять именованный мьютекс на всё время работы программы. false — его уже держит
/// другой процесс (вторая копия). Мьютекс освобождает Windows при выходе процесса.
pub fn claim_single_instance(name: &str) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name = wide(name);
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if handle.is_null() {
        return true; // не удалось создать — не мешать запуску
    }
    if exists {
        unsafe { CloseHandle(handle) };
        return false;
    }
    true // дескриптор не закрывается: мьютекс живёт, пока работает программа
}

/// `bytes` случайных байт из системного генератора — в шестнадцатеричном виде.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let status = unsafe {
        BCryptGenRandom(std::ptr::null_mut(), buffer.as_mut_ptr(), bytes as u32, BCRYPT_USE_SYSTEM_PREFERRED_RNG)
    };
    assert_eq!(status, 0, "BCryptGenRandom");
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

/// Адрес компьютера в локальной сети: через какой интерфейс ушёл бы пакет наружу.
/// UDP-«подключение» ничего не отправляет.
pub fn local_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.168.0.1:9")?;
            socket.local_addr()
        })
        .map(|address| address.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

// --- окна верхнего уровня (перенос `desktop.py`); окно — число, как в state.json

fn hwnd(window: i64) -> HWND {
    window as isize as HWND
}

/// Видимые окна: Windows перечисляет их от последнего активного к давним.
pub fn top_level_windows(include_minimized: bool) -> Vec<i64> {
    struct Found {
        windows: Vec<i64>,
        include_minimized: bool,
    }
    unsafe extern "system" fn visit(window: HWND, data: LPARAM) -> BOOL {
        let found = unsafe { &mut *(data as *mut Found) };
        if unsafe { IsWindowVisible(window) } != 0 && (found.include_minimized || unsafe { IsIconic(window) } == 0) {
            found.windows.push(window as isize as i64);
        }
        1
    }
    let mut found = Found { windows: Vec::new(), include_minimized };
    unsafe { EnumWindows(Some(visit), &mut found as *mut Found as LPARAM) };
    found.windows
}

pub fn class_name(window: i64) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(hwnd(window), buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

pub fn window_title(window: i64) -> String {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetWindowTextW(hwnd(window), buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

pub fn foreground_window() -> i64 {
    unsafe { GetForegroundWindow() as isize as i64 }
}

/// Вывести окно на передний план; false — Windows не дал.
pub fn bring_to_foreground(window: i64) -> bool {
    let target = hwnd(window);
    unsafe {
        if IsIconic(target) != 0 {
            ShowWindow(target, SW_RESTORE);
        }
        // Windows разрешает смену активного окна потоку, связанному с текущим активным.
        let this_thread = GetCurrentThreadId();
        let foreground_thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
        let attached = foreground_thread != 0
            && foreground_thread != this_thread
            && AttachThreadInput(this_thread, foreground_thread, 1) != 0;
        BringWindowToTop(target);
        SetForegroundWindow(target);
        if attached {
            AttachThreadInput(this_thread, foreground_thread, 0);
        }
        GetForegroundWindow() == target
    }
}

pub fn is_window(window: i64) -> bool {
    window != 0 && unsafe { IsWindow(hwnd(window)) } != 0
}

pub fn window_pid(window: i64) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd(window), &mut pid) };
    pid
}

/// Полный путь к исполняемому файлу процесса окна или пустая строка.
pub fn process_path(window: i64) -> String {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, window_pid(window));
        if process.is_null() {
            return String::new();
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size) != 0;
        CloseHandle(process);
        if ok { String::from_utf16_lossy(&buffer[..size as usize]) } else { String::new() }
    }
}

/// Скрытые системой окна (приложения Store в фоне, другие рабочие столы) видимы для API.
fn cloaked(window: i64) -> bool {
    let mut value = 0u32;
    let result = unsafe {
        DwmGetWindowAttribute(hwnd(window), DWMWA_CLOAKED as u32, &mut value as *mut u32 as *mut _, 4)
    };
    result == 0 && value != 0
}

/// Рабочий стол и панель задач — тоже видимые окна верхнего уровня, но не приложения.
const SHELL_CLASSES: &[&str] = &[
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
];

/// Обычное окно приложения: не своё, не панель задач, не всплывающее и не диалог.
pub fn is_app_window(window: i64) -> bool {
    let target = hwnd(window);
    if window == 0 || unsafe { IsWindowVisible(target) } == 0 || window_pid(window) == std::process::id() {
        return false;
    }
    if SHELL_CLASSES.contains(&class_name(window).as_str()) || window_title(window).trim().is_empty() {
        return false;
    }
    let style = unsafe { GetWindowLongW(target, GWL_EXSTYLE) } as u32;
    if style & WS_EX_TOOLWINDOW != 0 && style & WS_EX_APPWINDOW == 0 {
        return false;
    }
    if !unsafe { GetWindow(target, GW_OWNER) }.is_null() {
        return false;
    }
    !cloaked(window)
}

/// Иконка программы (exe) как RGBA `size`×`size`: SHDefExtractIcon сам подбирает и масштабирует
/// подходящий размер из ресурсов программы.
pub fn program_icon(path: &str, size: u32) -> Option<Vec<u8>> {
    let path = wide(path);
    unsafe {
        let mut icon: HICON = std::ptr::null_mut();
        if SHDefExtractIconW(path.as_ptr(), 0, 0, &mut icon, std::ptr::null_mut(), size) != 0 || icon.is_null() {
            return None;
        }
        let mut info: ICONINFO = std::mem::zeroed();
        let ok = GetIconInfo(icon, &mut info) != 0;
        let mut pixels = None;
        if ok {
            let read = |bitmap| {
                let mut header: BITMAPINFO = std::mem::zeroed();
                header.bmiHeader = BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: size as i32,
                    biHeight: -(size as i32), // сверху вниз
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                };
                let mut buffer = vec![0u8; (size * size * 4) as usize];
                let dc = GetDC(std::ptr::null_mut());
                let lines = GetDIBits(dc, bitmap, 0, size, buffer.as_mut_ptr() as *mut _, &mut header, DIB_RGB_COLORS);
                ReleaseDC(std::ptr::null_mut(), dc);
                (lines == size as i32).then_some(buffer)
            };
            if let Some(mut color) = read(info.hbmColor) {
                let has_alpha = color.chunks(4).any(|p| p[3] != 0);
                let mask = if has_alpha { None } else { read(info.hbmMask) };
                for (index, pixel) in color.chunks_mut(4).enumerate() {
                    pixel.swap(0, 2); // BGRA → RGBA
                    if let Some(mask) = &mask {
                        // Старые иконки без альфа-канала: прозрачность — по маске (белое — прозрачно).
                        pixel[3] = if mask[index * 4] == 0 { 255 } else { 0 };
                    }
                }
                pixels = Some(color);
            }
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
        }
        DestroyIcon(icon);
        pixels
    }
}

/// Местное время для журнала.
pub fn now() -> String {
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}
