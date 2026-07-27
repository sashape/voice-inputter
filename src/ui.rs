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
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DeleteDC, DeleteObject,
    GetDC, ReleaseDC, SelectObject, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DIB_RGB_COLORS, HBITMAP, HDC, HFONT, LOGFONTW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext, SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_CONTEXT_UNAWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, TrackMouseEvent, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_NOREPEAT,
    TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// ── константы ────────────────────────────────────────────────────────────
const WM_APP_TRAY: u32 = WM_APP + 1;
const WM_APP_STATE: u32 = WM_APP + 2;
const WM_MOUSELEAVE: u32 = 0x02A3;

const HOTKEY_ID: i32 = 1;
const OVERLAY_TIMER: usize = 1;

const ID_SETTINGS: usize = 1;
const ID_ENABLED: usize = 2;
const ID_DICTATE: usize = 3;
const ID_QUIT: usize = 4;

const ID_COMBO: i32 = 101;
const ID_EDIT: i32 = 102;
const ID_SAVE: i32 = 103;
const ID_CANCEL: i32 = 104;
const ID_STOP: i32 = 105;
const ID_HOTKEY: i32 = 106;
const ID_MODE: i32 = 107;
const ID_LIVE: i32 = 108;

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
    settings_hwnd: HWND,
    overlay: Option<Overlay>,
    bars: Vec<f32>,
    level_smooth: f32,
    anim_start: Option<Instant>,
    hovered: bool,
    hover_leave_at: Option<Instant>,
    tracking_leave: bool,
    last_dictating: bool,
    dict_ended_at: Option<Instant>,
    dismissed: bool,
    icons: [HICON; 3],
    stream: Option<cpal::Stream>,
    settings_combo: HWND,
    settings_edit: HWND,
    settings_stop: HWND,
    settings_hotkey: HWND,
    settings_mode: HWND,
    settings_live: HWND,
    settings_font: HFONT,
    settings_devices: Vec<String>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            hinstance: Default::default(),
            main: HWND::default(),
            overlay_hwnd: HWND::default(),
            settings_hwnd: HWND::default(),
            overlay: None,
            bars: vec![0.06; overlay::N_BARS],
            level_smooth: 0.0,
            anim_start: None,
            hovered: false,
            hover_leave_at: None,
            tracking_leave: false,
            last_dictating: false,
            dict_ended_at: None,
            dismissed: false,
            icons: [HICON::default(); 3],
            stream: None,
            settings_combo: HWND::default(),
            settings_edit: HWND::default(),
            settings_stop: HWND::default(),
            settings_hotkey: HWND::default(),
            settings_mode: HWND::default(),
            settings_live: HWND::default(),
            settings_font: HFONT::default(),
            settings_devices: Vec::new(),
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

        let isz = (16.0 * GetDpiForSystem() as f32 / 96.0).round() as i32;
        let icons = [
            make_icon((90, 150, 245), isz),  // idle
            make_icon((60, 200, 90), isz),   // dictating
            make_icon((140, 140, 140), isz), // disabled
        ];

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

        // непрерывный таймер анимации волны
        SetTimer(main, OVERLAY_TIMER, 33, None);

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

    let settings = WNDCLASSW {
        lpfnWndProc: Some(settings_wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterSettings"),
        hCursor: cursor,
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(
            (windows::Win32::Graphics::Gdi::COLOR_WINDOW.0 + 1) as isize as *mut c_void,
        ),
        ..Default::default()
    };
    RegisterClassW(&settings);
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
                    _ => {}
                }
                LRESULT(0)
            }
            WM_APP_STATE => {
                update_tray_icon(hwnd);
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

        let set_label = wide("Настройки…");
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, PCWSTR(set_label.as_ptr())).ok();
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
                UI.with_borrow_mut(|ui| {
                    ui.hovered = true;
                    ui.hover_leave_at = None;
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
            WM_MOUSELEAVE => {
                UI.with_borrow_mut(|ui| {
                    ui.hovered = false;
                    ui.hover_leave_at = Some(Instant::now());
                    ui.tracking_leave = false;
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
                        if is_dictating() {
                            crate::shared::send_worker(WorkerMsg::Toggle);
                        } else {
                            // в покое ✕ прячет пилюлю (в режиме «Только при диктовке»)
                            UI.with_borrow_mut(|ui| ui.dismissed = true);
                        }
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
            "always" => enabled,
            // dictation: диктовка + линжер idle-состояния + пока наведён курсор
            _ => dictating || (!ui.dismissed && (lingering || hover_active(ui))),
        };

        let visible = unsafe { IsWindowVisible(ui.overlay_hwnd).as_bool() };
        if should_show {
            if !visible {
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
                for b in ui.bars.iter_mut() {
                    *b = 0.06;
                }
                unsafe {
                    let _ = ShowWindow(ui.overlay_hwnd, SW_SHOWNOACTIVATE);
                }
            }
            let t = ui
                .anim_start
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            // при диктовке — реальный уровень; в покое (idle) — приглушённый
            let raw = current_level() as f32 / 1000.0;
            let level = if dictating { raw } else { raw * 0.35 };
            ui.level_smooth += (level - ui.level_smooth) * 0.25;
            let lvl = ui.level_smooth;
            overlay::animate(&mut ui.bars, lvl, t);
            draw_overlay(ui);
        } else if visible {
            ui.hovered = false;
            ui.hover_leave_at = None;
            ui.tracking_leave = false;
            unsafe {
                let _ = ShowWindow(ui.overlay_hwnd, SW_HIDE);
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
    overlay::render(
        &mut buf,
        &overlay::Frame {
            bars: &ui.bars,
            hovered: hover_active(ui),
            recording: is_dictating(),
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

// ── иконки трея ──────────────────────────────────────────────────────────
fn make_icon(color: (u8, u8, u8), size: i32) -> HICON {
    unsafe {
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dc = CreateCompatibleDC(HDC::default());
        let dib =
            CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0).unwrap();
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize);
        let c = (size as f32 - 1.0) / 2.0;
        let rad = size as f32 / 2.0 - 0.5;
        for y in 0..size {
            for x in 0..size {
                let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
                let idx = (y * size + x) as usize;
                if d <= rad {
                    px[idx] = 0xFF00_0000
                        | ((color.0 as u32) << 16)
                        | ((color.1 as u32) << 8)
                        | color.2 as u32;
                } else {
                    px[idx] = 0;
                }
            }
        }
        // белый значок микрофона поверх круга — чтобы иконку нельзя было
        // спутать с иконками других программ
        let s = size as f32;
        let bw = (s * 0.13).max(1.0); // полуширина тела микрофона
        let body_top = s * 0.24;
        let body_bot = s * 0.55;
        let stem_bot = s * 0.70;
        let base_half = s * 0.19;
        let stem_hw = (s * 0.055).max(0.6);
        for y in 0..size {
            for x in 0..size {
                let px_ = x as f32 + 0.5;
                let py_ = y as f32 + 0.5;
                let dx = px_ - c;
                let body = dx.abs() <= bw && py_ >= body_top && py_ <= body_bot;
                let stem = dx.abs() <= stem_hw && py_ > body_bot && py_ <= stem_bot;
                let base = (py_ - stem_bot).abs() <= (s * 0.05).max(0.6) && dx.abs() <= base_half;
                if body || stem || base {
                    px[(y * size + x) as usize] = 0xFFFF_FFFF; // белый
                }
            }
        }
        // маска 1bpp, все нули (используется альфа цветного DIB); буфер с запасом
        let zeros = vec![0u8; (size * size).max(64) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(zeros.as_ptr() as *const c_void));
        let ii = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: dib,
        };
        let icon = CreateIconIndirect(&ii).unwrap_or_default();
        let _ = DeleteObject(dib);
        let _ = DeleteObject(mask);
        let _ = DeleteDC(dc);
        icon
    }
}

// ── окно настроек ────────────────────────────────────────────────────────
fn make_ui_font() -> HFONT {
    unsafe {
        let mut lf = LOGFONTW {
            lfHeight: -15, // ~11pt при 96 DPI
            lfWeight: 400, // обычный
            lfCharSet: DEFAULT_CHARSET,
            lfQuality: CLEARTYPE_QUALITY,
            ..Default::default()
        };
        for (i, c) in "Segoe UI".encode_utf16().enumerate().take(31) {
            lf.lfFaceName[i] = c;
        }
        CreateFontIndirectW(&lf)
    }
}

fn open_settings() {
    UI.with_borrow(|ui| {
        if !ui.settings_hwnd.0.is_null() {
            unsafe {
                let _ = ShowWindow(ui.settings_hwnd, SW_SHOW);
                let _ = SetForegroundWindow(ui.settings_hwnd);
            }
            return;
        }
    });

    let hinstance = UI.with_borrow(|ui| ui.hinstance);
    unsafe {
        // Делаем окно настроек DPI-unaware: ОС сама масштабирует его целиком
        // (вместе со шрифтом) на HiDPI — без ручного пересчёта координат.
        let prev_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);

        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            w!("VoiceInputterSettings"),
            w!("Voice Inputter — Настройки"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            456,
            474,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("settings window");

        let font = make_ui_font();
        let cfg = shared().config.lock().unwrap().clone();
        let lm = 20i32; // левый отступ
        let cw = 412i32; // ширина контролов

        let mk = |class: PCWSTR, text: &Vec<u16>, style: WINDOW_STYLE, x, y, w_, h_, id: i32| -> HWND {
            let c = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                x,
                y,
                w_,
                h_,
                hwnd,
                HMENU(id as isize as *mut c_void),
                hinstance,
                None,
            )
            .unwrap();
            SendMessageW(c, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            c
        };

        let mut y = 16i32;
        mk(w!("STATIC"), &wide("Микрофон"), WINDOW_STYLE(0), lm, y, cw, 18, 0);
        y += 22;
        let combo = mk(
            w!("COMBOBOX"),
            &wide(""),
            WINDOW_STYLE((CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32) | WS_VSCROLL | WS_TABSTOP,
            lm, y, cw, 260, ID_COMBO,
        );
        y += 42;

        mk(w!("STATIC"), &wide("Имя-активатор (через запятую)"), WINDOW_STYLE(0), lm, y, cw, 18, 0);
        y += 22;
        let edit = mk(
            w!("EDIT"),
            &wide(&cfg.wake_words.join(", ")),
            WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_BORDER | WS_TABSTOP,
            lm, y, cw, 26, ID_EDIT,
        );
        y += 42;

        mk(w!("STATIC"), &wide("Стоп-слова (через запятую)"), WINDOW_STYLE(0), lm, y, cw, 18, 0);
        y += 22;
        let stop = mk(
            w!("EDIT"),
            &wide(&cfg.stop_words.join(", ")),
            WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_BORDER | WS_TABSTOP,
            lm, y, cw, 26, ID_STOP,
        );
        y += 42;

        mk(w!("STATIC"), &wide("Горячая клавиша (напр. ctrl+alt+j)"), WINDOW_STYLE(0), lm, y, cw, 18, 0);
        y += 22;
        let hotkey = mk(
            w!("EDIT"),
            &wide(&cfg.hotkey),
            WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_BORDER | WS_TABSTOP,
            lm, y, cw, 26, ID_HOTKEY,
        );
        y += 42;

        mk(w!("STATIC"), &wide("Показывать оверлей"), WINDOW_STYLE(0), lm, y, cw, 18, 0);
        y += 22;
        let mode = mk(
            w!("COMBOBOX"),
            &wide(""),
            WINDOW_STYLE((CBS_DROPDOWNLIST | CBS_HASSTRINGS) as u32) | WS_VSCROLL | WS_TABSTOP,
            lm, y, cw, 140, ID_MODE,
        );
        y += 42;

        let live = mk(
            w!("BUTTON"),
            &wide("Печатать сразу, по мере речи"),
            WINDOW_STYLE(BS_AUTOCHECKBOX as u32) | WS_TABSTOP,
            lm, y, cw, 24, ID_LIVE,
        );
        y += 42;

        mk(
            w!("BUTTON"),
            &wide("Сохранить"),
            WINDOW_STYLE(BS_DEFPUSHBUTTON as u32) | WS_TABSTOP,
            lm, y, 130, 34, ID_SAVE,
        );
        mk(
            w!("BUTTON"),
            &wide("Отмена"),
            WINDOW_STYLE(BS_PUSHBUTTON as u32) | WS_TABSTOP,
            lm + 146, y, 130, 34, ID_CANCEL,
        );

        // список микрофонов
        let devices = audio::list_input_devices();
        SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(wide("По умолчанию").as_ptr() as isize));
        let mut sel: usize = 0;
        for (i, d) in devices.iter().enumerate() {
            let wd = wide(d);
            SendMessageW(combo, CB_ADDSTRING, WPARAM(0), LPARAM(wd.as_ptr() as isize));
            if Some(d) == cfg.device_name.as_ref() {
                sel = i + 1;
            }
        }
        SendMessageW(combo, CB_SETCURSEL, WPARAM(sel), LPARAM(0));

        // режимы показа оверлея (подписи явные, чтобы не путать)
        for m in [
            "Всегда (не прячется на стоп)",
            "Только при диктовке (прячется в покое)",
            "Скрыт",
        ] {
            SendMessageW(mode, CB_ADDSTRING, WPARAM(0), LPARAM(wide(m).as_ptr() as isize));
        }
        let mode_idx = match cfg.overlay_mode.as_str() {
            "dictation" => 1,
            "hidden" => 2,
            _ => 0,
        };
        SendMessageW(mode, CB_SETCURSEL, WPARAM(mode_idx), LPARAM(0));

        // чекбокс live-ввода
        SendMessageW(live, BM_SETCHECK, WPARAM(if cfg.live_typing { 1 } else { 0 }), LPARAM(0));

        UI.with_borrow_mut(|ui| {
            ui.settings_hwnd = hwnd;
            ui.settings_combo = combo;
            ui.settings_edit = edit;
            ui.settings_stop = stop;
            ui.settings_hotkey = hotkey;
            ui.settings_mode = mode;
            ui.settings_live = live;
            ui.settings_font = font;
            ui.settings_devices = devices;
        });

        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = UpdateWindow(hwnd);

        // вернуть прежний контекст DPI для потока
        SetThreadDpiAwarenessContext(prev_dpi);
    }
}

extern "system" fn settings_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as i32;
                match id {
                    ID_SAVE => {
                        save_settings();
                        DestroyWindow(hwnd).ok();
                    }
                    ID_CANCEL => {
                        DestroyWindow(hwnd).ok();
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                DestroyWindow(hwnd).ok();
                LRESULT(0)
            }
            WM_DESTROY => {
                UI.with_borrow_mut(|ui| {
                    if !ui.settings_font.0.is_null() {
                        let _ = DeleteObject(ui.settings_font);
                        ui.settings_font = HFONT::default();
                    }
                    ui.settings_hwnd = HWND::default();
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn read_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; (len + 1) as usize];
    let got = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..got as usize])
}

unsafe fn read_words(h: HWND) -> Vec<String> {
    read_text(h)
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

unsafe fn reregister_hotkey(main: HWND, hk: &str) {
    let _ = UnregisterHotKey(main, HOTKEY_ID);
    if let Some((mods, vk)) = crate::config::parse_hotkey(hk) {
        let _ = RegisterHotKey(main, HOTKEY_ID, HOT_KEY_MODIFIERS(mods) | MOD_NOREPEAT, vk);
    }
}

fn save_settings() {
    let (combo, edit, stop, hotkey_h, mode, live, devices, main) = UI.with_borrow(|ui| {
        (
            ui.settings_combo,
            ui.settings_edit,
            ui.settings_stop,
            ui.settings_hotkey,
            ui.settings_mode,
            ui.settings_live,
            ui.settings_devices.clone(),
            ui.main,
        )
    });

    unsafe {
        let sel = SendMessageW(combo, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let device_name = if sel <= 0 {
            None
        } else {
            devices.get((sel - 1) as usize).cloned()
        };
        let wake_words = read_words(edit);
        let stop_words = read_words(stop);
        let hk = read_text(hotkey_h).trim().to_lowercase();
        let mi = SendMessageW(mode, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        let overlay_mode = match mi {
            1 => "dictation",
            2 => "hidden",
            _ => "always",
        }
        .to_string();
        let live_on = SendMessageW(live, BM_GETCHECK, WPARAM(0), LPARAM(0)).0 == 1;

        let (device_changed, hotkey_changed, words_changed);
        {
            let mut cfg = shared().config.lock().unwrap();
            device_changed = cfg.device_name != device_name;
            hotkey_changed = !hk.is_empty() && cfg.hotkey != hk;
            words_changed = (!wake_words.is_empty() && cfg.wake_words != wake_words)
                || (!stop_words.is_empty() && cfg.stop_words != stop_words);
            cfg.device_name = device_name.clone();
            if !wake_words.is_empty() {
                cfg.wake_words = wake_words;
            }
            if !stop_words.is_empty() {
                cfg.stop_words = stop_words;
            }
            if !hk.is_empty() {
                cfg.hotkey = hk.clone();
            }
            cfg.overlay_mode = overlay_mode;
            cfg.live_typing = live_on;
            cfg.save();
        }

        if device_changed {
            rebuild_stream(device_name.as_deref());
        }
        if hotkey_changed {
            reregister_hotkey(main, &hk);
        }
        // при смене слов пересоздаём распознаватель (обновить hotwords-буст)
        if words_changed {
            crate::shared::send_worker(WorkerMsg::Reload);
        } else {
            crate::shared::send_worker(WorkerMsg::Reset);
        }
    }
}
