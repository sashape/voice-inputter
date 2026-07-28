//! Весь нативный WinAPI-слой: трей с меню, оверлей-волна (topmost,
//! no-activate — кликабелен, но фокус не крадётся), окно настроек,
//! глобальный хоткей и цикл сообщений.
//!
//! Всё выполняется в главном потоке; ресурсы, привязанные к потоку,
//! лежат в thread_local `UI`.

#![allow(non_snake_case)]

use crate::overlay::{self, Region};
use crate::shared::{
    current_level, is_dictating, is_enabled, pack_hwnd, shared, unpack_hwnd, WorkerMsg,
};
use crate::audio;
use crate::icons::Tray;
use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, COLORREF, ERROR_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE,
    WPARAM,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// ── константы ────────────────────────────────────────────────────────────
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_STATE: u32 = WM_APP + 2;
const WM_APP_UPDATE: u32 = WM_APP + 3;
/// Клик по всплывающему уведомлению трея.
const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;
const WM_MOUSELEAVE: u32 = 0x02A3;

const HOTKEY_ID: i32 = 1;
const OVERLAY_TIMER: usize = 1;
// интервал таймера оверлея: частый пока виден (плавно), редкий в покое (экономия)
const TIMER_FAST: u32 = 8; // ~120 fps под timeBeginPeriod(1)
const TIMER_IDLE: u32 = 40; // достаточно, чтобы вовремя поймать начало показа

const ID_SETTINGS: usize = 1;
const ID_ENABLED: usize = 2;
const ID_DICTATE: usize = 3;
const ID_QUIT: usize = 4;
const ID_MODEL: usize = 5;
const ID_UPDATE: usize = 6;

use overlay::{OV_H, OV_W};

// ── UI-состояние (только главный поток) ──────────────────────────────────
struct Overlay {
    mem_dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    x: i32,
    y: i32,
    scale: f32,
}

struct Ui {
    hinstance: windows::Win32::Foundation::HINSTANCE,
    main: HWND,
    overlay_hwnd: HWND,
    overlay: Option<Overlay>,
    bars: Vec<f32>,
    level_smooth: f32,
    rec_t: f32,
    show_t: f32,
    hires: bool, // включён ли высокоточный таймер (пока оверлей на экране)
    anim_start: Option<Instant>,
    last_tick: Option<Instant>,
    hovered: bool,
    hover_leave_at: Option<Instant>,
    tracking_leave: bool,
    hover_region: Region,   // кнопка под курсором (для курсора-руки и подсветки)
    hover_int: [f32; 3],    // сглаженная подсветка [mic, gear, close]
    last_dictating: bool,
    dict_ended_at: Option<Instant>,
    dismissed: bool,
    icons: [HICON; 3],
    stream: Option<cpal::Stream>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            hinstance: Default::default(),
            main: HWND::default(),
            overlay_hwnd: HWND::default(),
            overlay: None,
            bars: vec![0.06; overlay::N_BARS],
            level_smooth: 0.0,
            rec_t: 0.0,
            show_t: 0.0,
            hires: false,
            anim_start: None,
            last_tick: None,
            hovered: false,
            hover_leave_at: None,
            tracking_leave: false,
            hover_region: Region::None,
            hover_int: [0.0; 3],
            last_dictating: false,
            dict_ended_at: None,
            dismissed: false,
            icons: [HICON::default(); 3],
            stream: None,
        }
    }
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── публичное API для других потоков ─────────────────────────────────────
/// Уведомить UI об изменении состояния (вызывается из аудио-потока).
pub fn post_state() {
    let s = shared();
    let h = unpack_hwnd(&s.main_hwnd, &s.main_hwnd_hi);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(
                HWND(h as *mut c_void),
                WM_APP_STATE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

/// Уведомить UI о найденном обновлении (вызывается из потока проверки).
pub fn post_update() {
    let s = shared();
    let h = unpack_hwnd(&s.main_hwnd, &s.main_hwnd_hi);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(HWND(h as *mut c_void), WM_APP_UPDATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// true, если наш экземпляр уже запущен (named mutex в сессии пользователя).
pub fn already_running() -> bool {
    unsafe {
        // handle намеренно «утекает» — мьютекс живёт до конца процесса
        if CreateMutexW(None, false, w!("VoiceInputterSingleInstance")).is_err() {
            return false;
        }
        GetLastError() == ERROR_ALREADY_EXISTS
    }
}

pub fn error_box(msg: &str) {
    let text = wide(msg);
    let title = wide("Voice Inputter");
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

// ── запуск ───────────────────────────────────────────────────────────────
pub fn run() {
    unsafe {
        // Per-Monitor v2: рисуем в физических пикселях → чёткий оверлей на HiDPI.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let hmodule = GetModuleHandleW(None).expect("GetModuleHandleW");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hmodule.0);

        register_classes(hinstance);

        // скрытое окно-приёмник сообщений
        let main = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("VoiceInputterMain"),
            w!("Voice Inputter"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("main window");

        // оверлей (скрыт)
        let overlay_hwnd = CreateWindowExW(
            // без WS_EX_TRANSPARENT: оверлей кликабелен (mic/⚙️/✕);
            // WS_EX_NOACTIVATE гарантирует, что клики не крадут фокус.
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("VoiceInputterOverlay"),
            w!(""),
            WS_POPUP,
            0,
            0,
            OV_W,
            OV_H,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("overlay window");

        let icons = build_tray_icons();

        let init_scale = overlay_scale_for(overlay_hwnd);
        let overlay = create_overlay_gdi(init_scale);

        UI.with_borrow_mut(|ui| {
            ui.hinstance = hinstance;
            ui.main = main;
            ui.overlay_hwnd = overlay_hwnd;
            ui.icons = icons;
            ui.overlay = Some(overlay);
        });

        // сохранить main hwnd для других потоков
        let s = shared();
        pack_hwnd(&s.main_hwnd, &s.main_hwnd_hi, main.0 as isize);
        pack_hwnd(&s.overlay_hwnd, &s.overlay_hwnd_hi, overlay_hwnd.0 as isize);

        add_tray(main, icons[0]);

        let hk = shared().config.lock().unwrap().hotkey.clone();
        if let Some((mods, vk)) = crate::config::parse_hotkey(&hk) {
            let modifiers = HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT;
            if let Err(e) = RegisterHotKey(main, HOTKEY_ID, modifiers, vk) {
                eprintln!(
                    "[ui] хоткей «{hk}» занят другим приложением ({e}). \
                     Смените hotkey в config.json. Голос и трей работают."
                );
            } else {
                eprintln!("[ui] хоткей активен: {hk}");
            }
        } else {
            eprintln!("[ui] не удалось разобрать хоткей «{hk}»");
        }

        // запустить захват звука
        let device = shared().config.lock().unwrap().device_name.clone();
        rebuild_stream(device.as_deref());

        // таймер анимации: старт в «покойном» режиме (оверлей скрыт),
        // ускоряется до TIMER_FAST + timeBeginPeriod, когда становится виден
        SetTimer(main, OVERLAY_TIMER, TIMER_IDLE, None);

        // проверка обновлений (первая — через полминуты после старта)
        crate::update::watch();

        // первый запуск без модели — предлагаем скачать её сразу
        if !crate::model::installed() {
            crate::model_ui::open();
        }

        eprintln!("[ui] запущено. Иконка в трее.");

        // цикл сообщений
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // очистка
        let _ = UnregisterHotKey(main, HOTKEY_ID);
        remove_tray(main);
        crate::shared::send_worker(WorkerMsg::Shutdown);
    }
}

fn rebuild_stream(device: Option<&str>) {
    let tx = {
        let guard = shared().worker_tx.lock().unwrap();
        guard.clone()
    };
    let Some(tx) = tx else { return };
    UI.with_borrow_mut(|ui| {
        ui.stream = None; // сначала закрыть старый
        match audio::build_stream(device, tx) {
            Ok(s) => ui.stream = Some(s),
            Err(e) => {
                eprintln!("[ui] аудио: {e}");
                error_box(&format!("Не удалось открыть микрофон:\n{e}"));
            }
        }
    });
}

// ── регистрация классов окон ─────────────────────────────────────────────
unsafe fn register_classes(hinstance: windows::Win32::Foundation::HINSTANCE) {
    let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();

    let main = WNDCLASSW {
        lpfnWndProc: Some(main_wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterMain"),
        hCursor: cursor,
        ..Default::default()
    };
    RegisterClassW(&main);

    let overlay = WNDCLASSW {
        lpfnWndProc: Some(overlay_wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterOverlay"),
        hCursor: cursor,
        ..Default::default()
    };
    RegisterClassW(&overlay);
}

// ── главное окно ─────────────────────────────────────────────────────────
extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_TRAY => {
                let event = (lp.0 & 0xFFFF) as u32;
                match event {
                    WM_RBUTTONUP | WM_CONTEXTMENU => show_tray_menu(hwnd),
                    WM_LBUTTONUP => crate::shared::send_worker(WorkerMsg::Toggle),
                    NIN_BALLOONUSERCLICK => open_release_page(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_STATE => {
                update_tray_icon(hwnd);
                LRESULT(0)
            }
            WM_APP_UPDATE => {
                if let Some(u) = crate::update::available() {
                    show_balloon(
                        hwnd,
                        "Доступно обновление",
                        &format!("Версия {} — откройте страницу релиза", u.version),
                    );
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                if wp.0 as i32 == HOTKEY_ID {
                    crate::shared::send_worker(WorkerMsg::Toggle);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                if wp.0 == OVERLAY_TIMER {
                    tick_overlay();
                }
                LRESULT(0)
            }
            // смена темы Windows приходит как WM_SETTINGCHANGE("ImmersiveColorSet")
            WM_SETTINGCHANGE => {
                if wp.0 == 0 && lp.0 != 0 {
                    let name = PCWSTR(lp.0 as *const u16).to_string().unwrap_or_default();
                    if name == "ImmersiveColorSet" {
                        refresh_tray_icons(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                // команды из трей-меню приходят сюда через WM_COMMAND не идут
                // (используем TPM_RETURNCMD), но оставим на всякий случай
                LRESULT(0)
            }
            WM_DESTROY => {
                KillTimer(hwnd, OVERLAY_TIMER).ok();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

// ── трей ─────────────────────────────────────────────────────────────────
/// Иконки трея под текущий DPI и тему Windows (порядок: покой, диктовка, выкл).
fn build_tray_icons() -> [HICON; 3] {
    let isz = unsafe { (16.0 * GetDpiForSystem() as f32 / 96.0).round() as i32 };
    [Tray::Idle, Tray::Rec, Tray::Off].map(|s| crate::icons::tray_icon(s, isz))
}

/// Пересоздать иконки трея — например, когда Windows переключила светлую/тёмную
/// тему: на светлой панели задач белый глиф не виден, нужен тёмный.
fn refresh_tray_icons(hwnd: HWND) {
    let icons = build_tray_icons();
    let old = UI.with_borrow_mut(|ui| std::mem::replace(&mut ui.icons, icons));
    update_tray_icon(hwnd);
    for i in old {
        unsafe {
            let _ = DestroyIcon(i);
        }
    }
}

fn tray_data(hwnd: HWND, icon: HICON) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    let tip = wide("Voice Inputter — голосовой ввод");
    for (i, &c) in tip.iter().take(nid.szTip.len()).enumerate() {
        nid.szTip[i] = c;
    }
    nid
}

/// Всплывающее уведомление у иконки в трее.
fn show_balloon(hwnd: HWND, title: &str, text: &str) {
    unsafe {
        let mut nid = tray_data(hwnd, HICON::default());
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_INFO;
        let put = |dst: &mut [u16], src: &str| {
            let w = wide(src);
            for (i, &c) in w.iter().take(dst.len()).enumerate() {
                dst[i] = c;
            }
        };
        put(&mut nid.szInfoTitle, title);
        put(&mut nid.szInfo, text);
        if !Shell_NotifyIconW(NIM_MODIFY, &nid).as_bool() {
            eprintln!("[ui] уведомление трея не показано (Shell_NotifyIcon отказал)");
        }
    }
}

/// Открывает страницу релиза в браузере.
fn open_release_page() {
    let Some(u) = crate::update::available() else { return };
    let url = wide(&u.url);
    unsafe {
        ShellExecuteW(HWND::default(), w!("open"), PCWSTR(url.as_ptr()), PCWSTR::null(),
            PCWSTR::null(), SW_SHOWNORMAL);
    }
}

fn add_tray(hwnd: HWND, icon: HICON) {
    unsafe {
        let nid = tray_data(hwnd, icon);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

fn remove_tray(hwnd: HWND) {
    unsafe {
        let nid = tray_data(hwnd, HICON::default());
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn update_tray_icon(hwnd: HWND) {
    let idx = if !is_enabled() {
        2
    } else if is_dictating() {
        1
    } else {
        0
    };
    UI.with_borrow(|ui| unsafe {
        let mut nid = tray_data(hwnd, ui.icons[idx]);
        nid.uFlags = NIF_ICON;
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    });
}

fn show_tray_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu().unwrap();
        let dict_label = if is_dictating() {
            wide("Остановить диктовку")
        } else {
            wide("Начать диктовку")
        };
        AppendMenuW(menu, MF_STRING, ID_DICTATE, PCWSTR(dict_label.as_ptr())).ok();
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();

        let en_flags = if is_enabled() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let en_label = wide("Прослушивание микрофона");
        AppendMenuW(menu, en_flags, ID_ENABLED, PCWSTR(en_label.as_ptr())).ok();

        // пункт появляется, только когда проверка нашла новую версию
        let upd = crate::update::available();
        let upd_label = upd
            .as_ref()
            .map(|u| wide(&format!("Обновление {} — открыть…", u.version)))
            .unwrap_or_default();
        if upd.is_some() {
            AppendMenuW(menu, MF_STRING, ID_UPDATE, PCWSTR(upd_label.as_ptr())).ok();
            AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
        }

        let set_label = wide("Настройки…");
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, PCWSTR(set_label.as_ptr())).ok();

        // пункт появляется, только если модель ещё не установлена
        let model_label = wide("Загрузить модель…");
        if !crate::model::installed() {
            AppendMenuW(menu, MF_STRING, ID_MODEL, PCWSTR(model_label.as_ptr())).ok();
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()).ok();
        let quit_label = wide("Выход");
        AppendMenuW(menu, MF_STRING, ID_QUIT, PCWSTR(quit_label.as_ptr())).ok();

        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok();
        // требуется, чтобы меню корректно закрывалось
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        DestroyMenu(menu).ok();

        match cmd.0 as usize {
            ID_DICTATE => crate::shared::send_worker(WorkerMsg::Toggle),
            ID_ENABLED => crate::shared::send_worker(WorkerMsg::SetEnabled(!is_enabled())),
            ID_SETTINGS => open_settings(),
            ID_MODEL => crate::model_ui::open(),
            ID_UPDATE => open_release_page(),
            ID_QUIT => {
                DestroyWindow(hwnd).ok();
            }
            _ => {}
        }
    }
}

// ── оверлей: волна ───────────────────────────────────────────────────────
const HOVER_GRACE: std::time::Duration = std::time::Duration::from_millis(450);
/// Сколько показывать idle-состояние после стопа в режиме «Только при диктовке».
const OVERLAY_LINGER: std::time::Duration = std::time::Duration::from_secs(8);

/// Активен ли ховер (наведение или ещё в grace-периоде после ухода).
fn hover_active(ui: &Ui) -> bool {
    ui.hovered
        || ui
            .hover_leave_at
            .map(|t| t.elapsed() < HOVER_GRACE)
            .unwrap_or(false)
}

extern "system" fn overlay_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCHITTEST => {
                let sx = (lp.0 & 0xFFFF) as i16 as i32;
                let sy = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
                let mut rc = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rc);
                let (active, scale) = UI.with_borrow(|ui| {
                    (hover_active(ui), ui.overlay.as_ref().map(|o| o.scale).unwrap_or(1.0))
                });
                let region = overlay::hit_test(sx - rc.left, sy - rc.top, active, scale);
                if region == Region::None {
                    LRESULT(HTTRANSPARENT as isize)
                } else {
                    LRESULT(HTCLIENT as isize)
                }
            }
            WM_MOUSEMOVE => {
                let x = (lp.0 & 0xFFFF) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
                UI.with_borrow_mut(|ui| {
                    ui.hovered = true;
                    ui.hover_leave_at = None;
                    let scale = ui.overlay.as_ref().map(|o| o.scale).unwrap_or(1.0);
                    ui.hover_region = overlay::hit_test(x, y, hover_active(ui), scale);
                    if !ui.tracking_leave {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        ui.tracking_leave = true;
                    }
                });
                LRESULT(0)
            }
            WM_SETCURSOR => {
                // рука над кнопками, стрелка в остальных местах пилюли
                let over_button = UI.with_borrow(|ui| {
                    matches!(ui.hover_region, Region::Mic | Region::Gear | Region::Close)
                });
                if over_button {
                    let hand = LoadCursorW(None, IDC_HAND).unwrap_or_default();
                    SetCursor(hand);
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_MOUSELEAVE => {
                UI.with_borrow_mut(|ui| {
                    ui.hovered = false;
                    ui.hover_leave_at = Some(Instant::now());
                    ui.tracking_leave = false;
                    ui.hover_region = Region::None;
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lp.0 & 0xFFFF) as i16 as i32;
                let y = ((lp.0 >> 16) & 0xFFFF) as i16 as i32;
                let (active, scale) = UI.with_borrow(|ui| {
                    (hover_active(ui), ui.overlay.as_ref().map(|o| o.scale).unwrap_or(1.0))
                });
                match overlay::hit_test(x, y, active, scale) {
                    Region::Mic => crate::shared::send_worker(WorkerMsg::Toggle),
                    Region::Close => {
                        // ✕ скрывает пилюлю в любом режиме; если шла диктовка — останавливает
                        if is_dictating() {
                            crate::shared::send_worker(WorkerMsg::Toggle);
                        }
                        UI.with_borrow_mut(|ui| ui.dismissed = true);
                    }
                    Region::Gear => open_settings(),
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Итоговый масштаб оверлея: DPI монитора × ручной множитель из конфига.
fn overlay_scale_for(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi };
    let user = shared().config.lock().unwrap().overlay_scale.clamp(0.5, 4.0);
    (dpi as f32 / 96.0) * user
}

fn create_overlay_gdi(scale: f32) -> Overlay {
    unsafe {
        let (w, h) = overlay::dims(scale);
        let screen = GetDC(HWND::default());
        let mem_dc = CreateCompatibleDC(screen);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
            .expect("CreateDIBSection");
        SelectObject(mem_dc, dib);
        ReleaseDC(HWND::default(), screen);
        Overlay {
            mem_dc,
            dib,
            bits: bits as *mut u32,
            x: 0,
            y: 0,
            scale,
        }
    }
}

fn workarea() -> RECT {
    let mut rc = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut rc as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok();
    }
    rc
}

fn tick_overlay() {
    let dictating = is_dictating();
    let (mode, enabled) = {
        let cfg = shared().config.lock().unwrap();
        (cfg.overlay_mode.clone(), is_enabled())
    };

    UI.with_borrow_mut(|ui| {
        // отслеживаем момент остановки диктовки (для «линжера» в режиме dictation)
        if dictating != ui.last_dictating {
            if dictating {
                ui.dismissed = false; // новая диктовка снимает «скрыто»
                ui.dict_ended_at = None;
            } else {
                ui.dict_ended_at = Some(Instant::now());
            }
            ui.last_dictating = dictating;
        }
        let lingering = ui
            .dict_ended_at
            .map(|t| t.elapsed() < OVERLAY_LINGER)
            .unwrap_or(false);

        let should_show = match mode.as_str() {
            "hidden" => false,
            // always: всегда видна, но ✕ прячет до следующей диктовки
            "always" => enabled && !ui.dismissed,
            // dictation: диктовка + линжер idle-состояния + пока наведён курсор
            _ => dictating || (!ui.dismissed && (lingering || hover_active(ui))),
        };

        // dt с прошлого кадра (клампим, чтобы пауза таймера не давала скачок)
        let now = Instant::now();
        let dt = ui.last_tick.map(|p| (now - p).as_secs_f32()).unwrap_or(0.016).clamp(0.001, 0.05);
        ui.last_tick = Some(now);

        let visible = unsafe { IsWindowVisible(ui.overlay_hwnd).as_bool() };

        // высокоточный частый таймер только пока оверлей на экране (в т.ч. при затухании);
        // в покое — грубый таймер и обычная гранулярность (экономим CPU/батарею)
        let on_screen = should_show || visible;
        if on_screen && !ui.hires {
            unsafe {
                timeBeginPeriod(1);
                SetTimer(ui.main, OVERLAY_TIMER, TIMER_FAST, None);
            }
            ui.hires = true;
        } else if !on_screen && ui.hires {
            unsafe {
                SetTimer(ui.main, OVERLAY_TIMER, TIMER_IDLE, None);
                let _ = timeEndPeriod(1);
            }
            ui.hires = false;
        }

        // первичный показ: настраиваем позицию/масштаб и показываем окно (show_t поедет вверх)
        if should_show && !visible {
            // масштаб мог измениться (DPI/конфиг) — пересоздаём DIB
            let want = overlay_scale_for(ui.overlay_hwnd);
            let cur = ui.overlay.as_ref().map(|o| o.scale).unwrap_or(1.0);
            if (want - cur).abs() > 0.01 {
                rebuild_overlay_gdi(ui, want);
            }
            let scale = ui.overlay.as_ref().map(|o| o.scale).unwrap_or(1.0);
            let (ovw, ovh) = overlay::dims(scale);
            let wa = workarea();
            let x = wa.left + (wa.right - wa.left - ovw) / 2;
            let y = wa.bottom - ovh - (24.0 * scale) as i32;
            if let Some(ov) = ui.overlay.as_mut() {
                ov.x = x;
                ov.y = y;
            }
            ui.anim_start = Some(Instant::now());
            ui.show_t = 0.0;
            for b in ui.bars.iter_mut() {
                *b = 0.06;
            }
            unsafe {
                let _ = ShowWindow(ui.overlay_hwnd, SW_SHOWNOACTIVATE);
            }
        }

        // рисуем, пока окно на экране (включая фазу затухания)
        if on_screen {
            // плавное появление/исчезание (быстрее вдвое: ~.25s)
            let show_target = if should_show { 1.0 } else { 0.0 };
            ui.show_t += (show_target - ui.show_t) * overlay::smooth_k(dt, 0.08);

            let t = ui
                .anim_start
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            // уровень микрофона: быстрая атака (бары резко отзываются на речь), мягкий спад
            let raw = current_level() as f32 / 1000.0;
            let tau_lvl = if raw > ui.level_smooth { 0.04 } else { 0.16 };
            ui.level_smooth += (raw - ui.level_smooth) * overlay::smooth_k(dt, tau_lvl);
            let lvl = ui.level_smooth;
            // плавный переход микрофон↔стоп и внутреннее свечение (как в html: ~.3s)
            let rec_target = if dictating { 1.0 } else { 0.0 };
            ui.rec_t += (rec_target - ui.rec_t) * overlay::smooth_k(dt, 0.1);
            // плавная подсветка кнопки под курсором (~.2s)
            let hr = ui.hover_region;
            let targets = [
                (hr == Region::Mic) as u8 as f32,
                (hr == Region::Gear) as u8 as f32,
                (hr == Region::Close) as u8 as f32,
            ];
            let kh = overlay::smooth_k(dt, 0.08);
            for i in 0..3 {
                ui.hover_int[i] += (targets[i] - ui.hover_int[i]) * kh;
            }
            overlay::animate(&mut ui.bars, lvl, t, dt, dictating);
            draw_overlay(ui);

            // полностью исчезло — прячем окно
            if !should_show && ui.show_t < 0.02 {
                ui.show_t = 0.0;
                ui.hovered = false;
                ui.hover_leave_at = None;
                ui.tracking_leave = false;
                ui.hover_region = Region::None;
                unsafe {
                    let _ = ShowWindow(ui.overlay_hwnd, SW_HIDE);
                }
            }
        }
    });
}

fn rebuild_overlay_gdi(ui: &mut Ui, scale: f32) {
    if let Some(old) = ui.overlay.take() {
        unsafe {
            let _ = DeleteDC(old.mem_dc);
            let _ = DeleteObject(old.dib);
        }
    }
    let mut ov = create_overlay_gdi(scale);
    // сохранить прежнюю позицию (будет пересчитана при показе)
    ov.x = 0;
    ov.y = 0;
    ui.overlay = Some(ov);
}

fn draw_overlay(ui: &Ui) {
    let Some(ov) = ui.overlay.as_ref() else {
        return;
    };
    let scale = ov.scale;
    let (dw, dh) = overlay::dims(scale);
    let (w, h) = (dw as usize, dh as usize);
    // straight-alpha RGBA буфер — рисуем по макету в отдельном модуле
    let mut buf = vec![0u8; w * h * 4];
    let t = ui.anim_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
    overlay::render(
        &mut buf,
        &overlay::Frame {
            bars: &ui.bars,
            hovered: hover_active(ui),
            rec_t: ui.rec_t,
            hover: ui.hover_int,
            show_t: ui.show_t,
            t,
        },
        scale,
    );

    // конвертация в premultiplied BGRA
    unsafe {
        let dst = std::slice::from_raw_parts_mut(ov.bits, w * h);
        for p in 0..w * h {
            let r = buf[p * 4] as u32;
            let g = buf[p * 4 + 1] as u32;
            let b = buf[p * 4 + 2] as u32;
            let a = buf[p * 4 + 3] as u32;
            let rp = r * a / 255;
            let gp = g * a / 255;
            let bp = b * a / 255;
            dst[p] = (a << 24) | (rp << 16) | (gp << 8) | bp;
        }

        let screen = GetDC(HWND::default());
        let mut pt_dst = POINT { x: ov.x, y: ov.y };
        let mut sz = SIZE { cx: dw, cy: dh };
        let mut pt_src = POINT { x: 0, y: 0 };
        let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
            BlendOp: windows::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: windows::Win32::Graphics::Gdi::AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            ui.overlay_hwnd,
            screen,
            Some(&mut pt_dst),
            Some(&mut sz),
            ov.mem_dc,
            Some(&mut pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(HWND::default(), screen);
    }
}

// ── окно настроек ────────────────────────────────────────────────────────
/// Открыть окно настроек (рисуется в `settings.rs`).
fn open_settings() {
    crate::settings::open();
}

/// Снять глобальный хоткей на время захвата нового сочетания в настройках.
pub fn pause_hotkey() {
    UI.with_borrow(|ui| unsafe {
        let _ = UnregisterHotKey(ui.main, HOTKEY_ID);
    });
}

/// Вернуть глобальный хоткей из конфига.
pub fn resume_hotkey() {
    let hk = shared().config.lock().unwrap().hotkey.clone();
    UI.with_borrow(|ui| unsafe {
        reregister_hotkey(ui.main, &hk);
    });
}

/// Применить настройки из окна: сохранить конфиг и подхватить изменения
/// (микрофон, хоткей, слова-команды) без перезапуска.
pub fn apply(new: crate::config::Config) {
    let (device_changed, hotkey_changed, words_changed, device_name, hotkey);
    {
        let mut cfg = shared().config.lock().unwrap();
        device_changed = cfg.device_name != new.device_name;
        hotkey_changed = cfg.hotkey != new.hotkey;
        words_changed = cfg.wake_words != new.wake_words
            || cfg.stop_words != new.stop_words
            || cfg.hotwords_score != new.hotwords_score;
        device_name = new.device_name.clone();
        hotkey = new.hotkey.clone();
        *cfg = new;
        cfg.save();
    }

    if device_changed {
        rebuild_stream(device_name.as_deref());
    }
    if hotkey_changed {
        unsafe { reregister_hotkey(UI.with_borrow(|ui| ui.main), &hotkey) };
    }
    // при смене слов пересоздаём распознаватель (обновить hotwords-буст)
    if words_changed {
        crate::shared::send_worker(WorkerMsg::Reload);
    } else {
        crate::shared::send_worker(WorkerMsg::Reset);
    }
}

unsafe fn reregister_hotkey(main: HWND, hk: &str) {
    let _ = UnregisterHotKey(main, HOTKEY_ID);
    if let Some((mods, vk)) = crate::config::parse_hotkey(hk) {
        let _ = RegisterHotKey(main, HOTKEY_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk);
    }
}

