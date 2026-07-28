//! Окно настроек «Nocturne» — порт дизайна Settings.html.
//!
//! Рисуем всё сами: формы, градиенты и тени — tiny-skia, текст — GDI
//! (ClearType) поверх того же DIB, вывод — layered-окно (UpdateLayeredWindow),
//! поэтому скругление 20px и мягкая тень выглядят как в макете.
//!
//! Масштабирование: окно Per-Monitor-v2, вся раскладка задана в логических px
//! макета и умножается на `scale = DPI / 96`; при `WM_DPICHANGED` шрифты и
//! поверхность пересоздаются, окно перерисовывается — без размытия.

#![allow(non_snake_case)]

use crate::paint::{col, cubic_bezier, ease, lerp, lerp_f, lin, round_rect, sd_rrect, Rgb};
use crate::win_ui::{
    chevron, clip, fill, fill_c, make_dib, mic_glyph, present, shadow, stroke_c, wide, Fonts, Sp,
    F, ICON, MAGENTA, PINK, SUNKEN, TXT, TXT_DIM, TXT_GREY, TXT_MUTE, VIOLET, WHITE,
};
use crate::shared::shared;
use std::cell::RefCell;
use std::ffi::c_void;
use std::time::Instant;
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

use windows::core::w;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint,
    ReleaseDC, SelectClipRgn, SetBkMode, UpdateWindow, HBITMAP, HDC, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
    VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT,
    VK_LWIN, VK_MENU, VK_RETURN, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

// ── раскладка макета (логические px) ─────────────────────────────────────
const W: f32 = 420.0; // ширина окна
const PANEL_R: f32 = 20.0;
const PAD: f32 = 22.0; // боковые поля контента
const CW: f32 = W - PAD * 2.0; // ширина контролов, 376
const TITLE_H: f32 = 58.0; // 16 + 28 + 14
const GAP: f32 = 18.0; // расстояние между группами
const GAP_S: f32 = 7.0; // подпись → контрол
const FIELD_H: f32 = 40.0;
const FIELD_R: f32 = 12.0;
const L_LABEL: f32 = 15.0; // высота строки подписи (12px)
const L_HINT: f32 = 14.0; // 11px
const L_BODY: f32 = 16.0; // 13px
const BTN_H: f32 = 38.0;
const SEG_H: f32 = 32.0;
const SW_W: f32 = 44.0; // переключатель
const SW_H: f32 = 26.0;
const MORE_GAP: f32 = 14.0; // расстояние между строками «Дополнительно»

// поля вокруг панели: тень окна не рисуем, поэтому окно = сама панель
const SH_L: f32 = 0.0;
const SH_T: f32 = 0.0;
const SH_B: f32 = 0.0;

const TIMER: usize = 7;
const WM_MOUSELEAVE: u32 = 0x02A3;

// ── элементы окна ─────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Id {
    Close = 0,
    Device,
    Wake,
    Stop,
    Hotkey,
    Mode0,
    Mode1,
    Mode2,
    Live,
    More,
    Silence,
    OvScale,
    Hotwords,
    Space,
    Caps,
    Startup,
    Updates,
    Cancel,
    Save,
}
const N_ID: usize = 19;
const ALL_IDS: [Id; N_ID] = [
    Id::Close, Id::Device, Id::Wake, Id::Stop, Id::Hotkey, Id::Mode0, Id::Mode1, Id::Mode2,
    Id::Live, Id::More, Id::Silence, Id::OvScale, Id::Hotwords, Id::Space, Id::Caps, Id::Startup,
    Id::Updates, Id::Cancel, Id::Save,
];

#[derive(Clone, Copy, PartialEq)]
enum K {
    Icon,
    Edit,
    Select,
    Seg,
    Toggle,
    Btn,
    Slider,
    More,
}

#[derive(Clone, Copy)]
struct Item {
    id: Id,
    k: K,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    /// элемент лежит в раскрывающемся блоке «Дополнительно» (клипается)
    more: bool,
}

impl Item {
    fn hit(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
    fn cy(&self) -> f32 {
        self.y + self.h / 2.0
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Al {
    L,
    C,
}

struct Lab {
    s: String,
    f: F,
    x: f32,
    cy: f32,
    al: Al,
    c: Rgb,
    /// разрядка (letter-spacing) в логических px
    sp: f32,
    max_w: f32,
    more: bool,
}

// ── однострочное поле ввода ───────────────────────────────────────────────
#[derive(Default)]
struct Edit {
    t: Vec<u16>,
    caret: usize,
    anchor: usize,
    scroll: f32, // сдвиг текста влево (лог. px)
    numeric: bool,
}

impl Edit {
    fn new(s: &str, numeric: bool) -> Edit {
        let t: Vec<u16> = s.encode_utf16().collect();
        let n = t.len();
        Edit { t, caret: n, anchor: n, scroll: 0.0, numeric }
    }
    fn text(&self) -> String {
        String::from_utf16_lossy(&self.t)
    }
    fn sel(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    fn has_sel(&self) -> bool {
        self.caret != self.anchor
    }
    fn del_sel(&mut self) -> bool {
        let (a, b) = self.sel();
        if a == b {
            return false;
        }
        self.t.drain(a..b);
        self.caret = a;
        self.anchor = a;
        true
    }
    fn insert(&mut self, ch: u16) {
        if self.numeric {
            let c = char::from_u32(ch as u32).unwrap_or('\0');
            if !(c.is_ascii_digit() || c == '.' || c == ',') {
                return;
            }
        }
        self.del_sel();
        let ch = if self.numeric && ch == ',' as u16 { '.' as u16 } else { ch };
        self.t.insert(self.caret, ch);
        self.caret += 1;
        self.anchor = self.caret;
    }
    fn insert_str(&mut self, s: &str) {
        for ch in s.encode_utf16() {
            if ch >= 0x20 {
                self.insert(ch);
            }
        }
    }
    fn backspace(&mut self) {
        if self.del_sel() || self.caret == 0 {
            return;
        }
        self.caret -= 1;
        self.t.remove(self.caret);
        self.anchor = self.caret;
    }
    fn delete(&mut self) {
        if self.del_sel() || self.caret >= self.t.len() {
            return;
        }
        self.t.remove(self.caret);
    }
    fn move_to(&mut self, pos: usize, keep_sel: bool) {
        self.caret = pos.min(self.t.len());
        if !keep_sel {
            self.anchor = self.caret;
        }
    }
    fn word_left(&self) -> usize {
        let mut i = self.caret;
        while i > 0 && is_sep(self.t[i - 1]) {
            i -= 1;
        }
        while i > 0 && !is_sep(self.t[i - 1]) {
            i -= 1;
        }
        i
    }
    fn word_right(&self) -> usize {
        let mut i = self.caret;
        let n = self.t.len();
        while i < n && !is_sep(self.t[i]) {
            i += 1;
        }
        while i < n && is_sep(self.t[i]) {
            i += 1;
        }
        i
    }
    fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.t.len();
    }
    fn sel_text(&self) -> String {
        let (a, b) = self.sel();
        String::from_utf16_lossy(&self.t[a..b])
    }
}

fn is_sep(c: u16) -> bool {
    matches!(char::from_u32(c as u32), Some(' ') | Some(',') | Some(';') | Some('\t'))
}

// ── состояние окна ────────────────────────────────────────────────────────
struct Win {
    hwnd: HWND,
    scale: f32,
    // поверхность
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    sw: i32,
    sh: i32,
    fonts: Fonts,
    // модель
    devices: Vec<String>,
    device: usize, // 0 = «По умолчанию»
    wake: Edit,
    stop: Edit,
    silence: Edit,
    hotwords: Edit,
    hotkey: String,
    capturing: bool,
    mode: usize,
    live: bool,
    space: bool,
    caps: bool,
    /// автозапуск при входе — хранится не в config.json, а в реестре
    startup: bool,
    updates: bool,
    ov_scale: f32,
    // вид
    expanded: bool,
    expand_t: f32,
    focus: Option<Id>,
    hot: Option<Id>,
    press: Option<Id>,
    drag_edit: bool,
    drag_slider: bool,
    track_leave: bool,
    anim: [f32; N_ID],
    hovt: [f32; N_ID],
    caret_on: bool,
    caret_at: Instant,
    last: Instant,
    dirty: bool,
    panel_h: f32,
    // выпадающий список микрофонов (своё layered-окно)
    popup: HWND,
    popup_on: bool,
    popup_hot: i32,
    popup_top: usize,
    pdc: HDC,
    pdib: HBITMAP,
    pbits: *mut u32,
    pw: i32,
    ph: i32,
}

thread_local! {
    static ST: RefCell<Option<Box<Win>>> = const { RefCell::new(None) };
}

fn with<R>(f: impl FnOnce(&mut Win) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().as_mut().map(|w| f(w)))
}

/// Собирает состояние окна из конфига (вынесено, чтобы им же пользовался
/// тест-превью рендера).
fn make_win(
    hwnd: HWND,
    popup: HWND,
    dc: HDC,
    pdc: HDC,
    scale: f32,
    cfg: &crate::config::Config,
    devices: Vec<String>,
    device: usize,
) -> Box<Win> {
    Box::new(Win {
        hwnd,
        scale,
        dc,
        dib: HBITMAP::default(),
        bits: std::ptr::null_mut(),
        sw: 0,
        sh: 0,
        fonts: Fonts::new(dc, scale),
        devices,
        device,
        wake: Edit::new(&cfg.wake_words.join(", "), false),
        stop: Edit::new(&cfg.stop_words.join(", "), false),
        silence: Edit::new(&fmt_num(cfg.silence_timeout), true),
        hotwords: Edit::new(&fmt_num(cfg.hotwords_score), true),
        hotkey: cfg.hotkey.clone(),
        capturing: false,
        mode: match cfg.overlay_mode.as_str() {
            "dictation" => 1,
            "hidden" => 2,
            _ => 0,
        },
        live: cfg.live_typing,
        space: cfg.append_space,
        caps: cfg.capitalize,
        startup: crate::startup::enabled(),
        updates: cfg.check_updates,
        ov_scale: cfg.overlay_scale.clamp(0.6, 2.0),
        expanded: false,
        expand_t: 0.0,
        focus: None,
        hot: None,
        press: None,
        drag_edit: false,
        drag_slider: false,
        track_leave: false,
        anim: [0.0; N_ID],
        hovt: [0.0; N_ID],
        caret_on: true,
        caret_at: Instant::now(),
        last: Instant::now(),
        dirty: true,
        panel_h: 600.0,
        popup,
        popup_on: false,
        popup_hot: -1,
        popup_top: 0,
        pdc,
        pdib: HBITMAP::default(),
        pbits: std::ptr::null_mut(),
        pw: 0,
        ph: 0,
    })
}

// ── открытие ──────────────────────────────────────────────────────────────
pub fn open() {
    if let Some(h) = ST.with(|s| s.borrow().as_ref().map(|w| w.hwnd)) {
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
        return;
    }

    let cfg = shared().config.lock().unwrap().clone();
    let devices = crate::audio::list_input_devices();
    let device = cfg
        .device_name
        .as_ref()
        .and_then(|d| devices.iter().position(|x| x == d).map(|i| i + 1))
        .unwrap_or(0);

    unsafe {
        let hmodule = GetModuleHandleW(None).expect("GetModuleHandleW");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hmodule.0);
        register_classes(hinstance);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("VoiceInputterSettings"),
            w!("Voice Inputter — Настройки"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("settings window");

        let popup = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("VoiceInputterSettingsPopup"),
            w!(""),
            WS_POPUP,
            0,
            0,
            10,
            10,
            hwnd,
            HMENU::default(),
            hinstance,
            None,
        )
        .expect("settings popup");

        let dpi = GetDpiForWindow(hwnd);
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };

        let screen = GetDC(HWND::default());
        let dc = CreateCompatibleDC(screen);
        let pdc = CreateCompatibleDC(screen);
        ReleaseDC(HWND::default(), screen);
        SetBkMode(dc, TRANSPARENT);
        SetBkMode(pdc, TRANSPARENT);

        let win = make_win(hwnd, popup, dc, pdc, scale, &cfg, devices, device);
        ST.with(|s| *s.borrow_mut() = Some(win));

        // сегмент режима и переключатели сразу в своём состоянии (без анимации)
        with(|w| {
            for (i, id) in [Id::Mode0, Id::Mode1, Id::Mode2].iter().enumerate() {
                w.anim[*id as usize] = (w.mode == i) as u8 as f32;
            }
            w.anim[Id::Live as usize] = w.live as u8 as f32;
            w.anim[Id::Space as usize] = w.space as u8 as f32;
            w.anim[Id::Caps as usize] = w.caps as u8 as f32;
            w.anim[Id::Startup as usize] = w.startup as u8 as f32;
            w.anim[Id::Updates as usize] = w.updates as u8 as f32;
        });

        // разместить по центру рабочей области монитора под курсором
        with(|w| {
            let l = w.layout();
            w.panel_h = l.h;
        });
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let wa = work_area(pt);
        let (sw, sh) = with(|w| surface_size(w)).unwrap_or((400, 700));
        let x = wa.left + (wa.right - wa.left - sw) / 2;
        let y = wa.top + (wa.bottom - wa.top - sh) / 2;
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, sw, sh, SWP_NOACTIVATE);

        redraw();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let _ = UpdateWindow(hwnd);
        SetTimer(hwnd, TIMER, 16, None);
    }
}

fn fmt_num(v: f32) -> String {
    if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

fn work_area(pt: POINT) -> RECT {
    unsafe {
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            mi.rcWork
        } else {
            RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
        }
    }
}

unsafe fn register_classes(hinstance: windows::Win32::Foundation::HINSTANCE) {
    thread_local! {
        static DONE: RefCell<bool> = const { RefCell::new(false) };
    }
    if DONE.with(|d| *d.borrow()) {
        return;
    }
    let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
    let c = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterSettings"),
        hCursor: cursor,
        ..Default::default()
    };
    RegisterClassW(&c);
    let p = WNDCLASSW {
        lpfnWndProc: Some(popup_wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterSettingsPopup"),
        hCursor: cursor,
        ..Default::default()
    };
    RegisterClassW(&p);
    DONE.with(|d| *d.borrow_mut() = true);
}

// ── раскладка ─────────────────────────────────────────────────────────────
struct Layout {
    items: Vec<Item>,
    labs: Vec<Lab>,
    divs: Vec<f32>,   // y горизонтальных разделителей
    h: f32,           // высота панели
    band: (f32, f32), // видимая полоса блока «Дополнительно» (y0, y1)
}

impl Win {
    fn seg_label(&self, i: usize) -> &'static str {
        ["Всегда", "При диктовке", "Никогда"][i]
    }

    fn layout(&self) -> Layout {
        let mut items = Vec::with_capacity(20);
        let mut labs = Vec::with_capacity(24);
        macro_rules! lab {
            ($s:expr, $f:expr, $x:expr, $cy:expr, $al:expr, $c:expr, $sp:expr, $max:expr) => {
                labs.push(Lab { s: $s.to_string(), f: $f, x: $x, cy: $cy, al: $al, c: $c, sp: $sp, max_w: $max, more: false })
            };
        }

        // ── шапка
        items.push(Item { id: Id::Close, k: K::Icon, x: W - 18.0 - 28.0, y: 16.0, w: 28.0, h: 28.0, more: false });
        lab!("Voice Inputter — Настройки", F::Title, 58.0, 30.0, Al::L, TXT, 0.14, 260.0);

        let mut y = TITLE_H + 6.0;

        // ── микрофон
        lab!("Микрофон", F::Label, PAD, y + L_LABEL / 2.0, Al::L, TXT_DIM, 0.36, CW);
        y += L_LABEL + GAP_S;
        items.push(Item { id: Id::Device, k: K::Select, x: PAD, y, w: CW, h: FIELD_H, more: false });
        let dev = if self.device == 0 {
            "По умолчанию".to_string()
        } else {
            self.devices.get(self.device - 1).cloned().unwrap_or_default()
        };
        lab!(&dev, F::Input, PAD + 14.0, y + FIELD_H / 2.0, Al::L, TXT, 0.0, CW - 14.0 - 36.0);
        y += FIELD_H + GAP;

        // ── имя-активатор
        lab!("Имя-активатор", F::Label, PAD, y + L_LABEL / 2.0, Al::L, TXT_DIM, 0.36, CW);
        y += L_LABEL + GAP_S;
        items.push(Item { id: Id::Wake, k: K::Edit, x: PAD, y, w: CW, h: FIELD_H, more: false });
        y += FIELD_H + GAP_S;
        lab!("Несколько слов — через запятую", F::Hint, PAD, y + L_HINT / 2.0, Al::L, TXT_MUTE, 0.0, CW);
        y += L_HINT + GAP;

        // ── стоп-слова
        lab!("Стоп-слова", F::Label, PAD, y + L_LABEL / 2.0, Al::L, TXT_DIM, 0.36, CW);
        y += L_LABEL + GAP_S;
        items.push(Item { id: Id::Stop, k: K::Edit, x: PAD, y, w: CW, h: FIELD_H, more: false });
        y += FIELD_H + GAP;

        // ── горячая клавиша
        let hk_txt = if self.capturing { "Нажмите клавиши…".to_string() } else { hotkey_label(&self.hotkey) };
        let hk_w = (self.text_w(F::Seg, &hk_txt) / self.scale + 28.0).max(86.0);
        let row_h = 36.0f32;
        items.push(Item { id: Id::Hotkey, k: K::Btn, x: W - PAD - hk_w, y, w: hk_w, h: row_h, more: false });
        lab!("Горячая клавиша", F::Label, PAD, y + row_h / 2.0 - 9.0, Al::L, TXT_DIM, 0.36, 240.0);
        lab!("Нажмите и введите сочетание", F::Hint, PAD, y + row_h / 2.0 + 8.0, Al::L, TXT_MUTE, 0.0, 240.0);
        let hk_col = if self.capturing { WHITE } else { ICON };
        lab!(&hk_txt, F::Seg, W - PAD - hk_w / 2.0, y + row_h / 2.0, Al::C, hk_col, 0.36, hk_w);
        y += row_h + GAP;

        // ── разделитель
        let div1 = y;
        y += 1.0 + GAP;

        // ── режим оверлея
        lab!("Показывать оверлей", F::Label, PAD, y + L_LABEL / 2.0, Al::L, TXT_DIM, 0.36, CW);
        y += L_LABEL + 8.0;
        let seg_y = y + 4.0;
        let seg_w = (CW - 8.0 - 8.0) / 3.0; // паддинг 4 + 2 зазора по 4
        for (i, id) in [Id::Mode0, Id::Mode1, Id::Mode2].iter().enumerate() {
            let x = PAD + 4.0 + i as f32 * (seg_w + 4.0);
            items.push(Item { id: *id, k: K::Seg, x, y: seg_y, w: seg_w, h: SEG_H, more: false });
            let t = self.anim[*id as usize];
            lab!(self.seg_label(i), F::Seg, x + seg_w / 2.0, seg_y + SEG_H / 2.0, Al::C, lerp(TXT_GREY, WHITE, t), 0.0, seg_w);
        }
        y += SEG_H + 8.0 + GAP;

        // ── печатать сразу
        let live_h = 33.0f32;
        items.push(Item { id: Id::Live, k: K::Toggle, x: W - PAD - SW_W, y: y + (live_h - SW_H) / 2.0, w: SW_W, h: SW_H, more: false });
        lab!("Печатать сразу, по мере речи", F::Body, PAD, y + 8.0, Al::L, TXT, 0.0, 290.0);
        lab!("Текст появляется во время диктовки, а не после", F::Hint, PAD, y + 8.0 + L_BODY / 2.0 + 3.0 + L_HINT / 2.0, Al::L, TXT_MUTE, 0.0, 290.0);
        y += live_h + GAP;

        // ── разделитель + «Дополнительно»
        let div2 = y;
        y += 1.0 + GAP;
        let more_row = 18.0f32;
        items.push(Item { id: Id::More, k: K::More, x: PAD, y, w: CW, h: more_row, more: false });
        lab!("Дополнительно", F::Label, PAD + 20.0, y + more_row / 2.0, Al::L, lerp(TXT_DIM, TXT, self.anim[Id::More as usize]), 0.36, 260.0);
        y += more_row;

        // ── содержимое блока «Дополнительно» (клипается по expand_t)
        let inner_top = y + MORE_GAP;
        let mut my = inner_top;
        {
            let num_row = |items: &mut Vec<Item>, labs: &mut Vec<Lab>, id: Id, title: &str, hint: &str, y: f32| -> f32 {
                let h = 36.0f32;
                let fw = 96.0f32;
                items.push(Item { id, k: K::Edit, x: W - PAD - fw, y, w: fw, h, more: true });
                labs.push(Lab { s: title.into(), f: F::Body, x: PAD, cy: y + h / 2.0 - 8.0, al: Al::L, c: TXT, sp: 0.0, max_w: 240.0, more: true });
                labs.push(Lab { s: hint.into(), f: F::Hint, x: PAD, cy: y + h / 2.0 + 9.0, al: Al::L, c: TXT_MUTE, sp: 0.0, max_w: 240.0, more: true });
                y + h + MORE_GAP
            };
            my = num_row(&mut items, &mut labs, Id::Silence, "Пауза до авто-стопа", "секунд тишины; 0 — не выключать", my);
            my = num_row(&mut items, &mut labs, Id::Hotwords, "Усиление слов-команд", "0 — выкл, 2 — по умолчанию", my);
        }
        // размер оверлея — слайдер
        {
            let h = 36.0f32;
            let slw = 150.0f32;
            items.push(Item { id: Id::OvScale, k: K::Slider, x: W - PAD - slw, y: my, w: slw, h, more: true });
            labs.push(Lab { s: "Размер оверлея".into(), f: F::Body, x: PAD, cy: my + h / 2.0 - 8.0, al: Al::L, c: TXT, sp: 0.0, max_w: 200.0, more: true });
            labs.push(Lab { s: format!("×{:.2}", self.ov_scale), f: F::Hint, x: PAD, cy: my + h / 2.0 + 9.0, al: Al::L, c: TXT_MUTE, sp: 0.0, max_w: 200.0, more: true });
            my += h + MORE_GAP;
        }
        // два переключателя
        for (id, title) in [
            (Id::Space, "Пробел после фразы"),
            (Id::Caps, "Заглавная в начале фразы"),
            (Id::Startup, "Запускать при входе в Windows"),
            (Id::Updates, "Проверять обновления"),
        ] {
            let h = SW_H;
            items.push(Item { id, k: K::Toggle, x: W - PAD - SW_W, y: my, w: SW_W, h, more: true });
            labs.push(Lab { s: title.into(), f: F::Body, x: PAD, cy: my + h / 2.0, al: Al::L, c: TXT, sp: 0.0, max_w: 280.0, more: true });
            my += h + MORE_GAP;
        }
        let more_full = my - inner_top; // полная высота содержимого блока
        let more_h = more_full * ease(self.expand_t);
        y += more_h;
        let band = (inner_top, inner_top + more_h);

        // ── кнопки
        y += GAP + 4.0;
        let save_w = (self.text_w(F::BtnBold, "Сохранить") / self.scale + 44.0).round();
        let cancel_w = (self.text_w(F::Btn, "Отмена") / self.scale + 36.0).round();
        let save_x = W - PAD - save_w;
        let cancel_x = save_x - 10.0 - cancel_w;
        items.push(Item { id: Id::Cancel, k: K::Btn, x: cancel_x, y, w: cancel_w, h: BTN_H, more: false });
        items.push(Item { id: Id::Save, k: K::Btn, x: save_x, y, w: save_w, h: BTN_H, more: false });
        let cancel_c = lerp(TXT_DIM, TXT, self.anim[Id::Cancel as usize]);
        lab!("Отмена", F::Btn, cancel_x + cancel_w / 2.0, y + BTN_H / 2.0, Al::C, cancel_c, 0.0, cancel_w);
        lab!("Сохранить", F::BtnBold, save_x + save_w / 2.0, y + BTN_H / 2.0, Al::C, WHITE, 0.0, save_w);
        y += BTN_H + PAD;

        Layout { items, labs, divs: vec![div1, div2], h: y, band }
    }

    /// Ширина текста в физических px.
    fn text_w(&self, f: F, s: &str) -> f32 {
        let t: Vec<u16> = s.encode_utf16().collect();
        self.fonts.width(self.dc, f, &t)
    }

    fn edit(&self, id: Id) -> Option<&Edit> {
        match id {
            Id::Wake => Some(&self.wake),
            Id::Stop => Some(&self.stop),
            Id::Silence => Some(&self.silence),
            Id::Hotwords => Some(&self.hotwords),
            _ => None,
        }
    }

    fn edit_mut(&mut self, id: Id) -> Option<&mut Edit> {
        match id {
            Id::Wake => Some(&mut self.wake),
            Id::Stop => Some(&mut self.stop),
            Id::Silence => Some(&mut self.silence),
            Id::Hotwords => Some(&mut self.hotwords),
            _ => None,
        }
    }
}

/// «ctrl+alt+j» → «Ctrl + Alt + J».
fn hotkey_label(hk: &str) -> String {
    hk.split('+')
        .map(|p| {
            let p = p.trim();
            match p {
                "ctrl" | "control" => "Ctrl".into(),
                "alt" => "Alt".into(),
                "shift" => "Shift".into(),
                "win" | "super" | "meta" => "Win".into(),
                "space" => "Space".into(),
                other => {
                    let mut c = other.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => String::new(),
                    }
                }
            }
        })
        .filter(|s: &String| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" + ")
}

fn surface_size(w: &Win) -> (i32, i32) {
    let s = w.scale;
    (
        ((W + SH_L * 2.0) * s).round() as i32,
        ((w.panel_h + SH_T + SH_B) * s).round() as i32,
    )
}

impl Win {
    /// Ширина строки в физических px.
    fn tw(&self, f: F, t: &[u16]) -> f32 {
        self.fonts.width(self.dc, f, t)
    }

    /// Позиция каретки (в символах) по смещению от начала текста, физ. px.
    fn caret_from_x(&self, f: F, t: &[u16], x: f32) -> usize {
        let mut best = 0usize;
        let mut prev = 0.0f32;
        for i in 1..=t.len() {
            let w = self.tw(f, &t[..i]);
            if x < (prev + w) / 2.0 {
                return best;
            }
            best = i;
            prev = w;
        }
        best
    }

    /// База текста поля: числовые поля центрируем, остальные — слева.
    fn edit_text_x(&self, it: &Item, e: &Edit) -> f32 {
        let pad = if e.numeric { 12.0 } else { 14.0 };
        if e.numeric {
            let tw = self.tw(F::Input, &e.t) / self.scale;
            if tw < it.w - pad * 2.0 {
                return it.x + (it.w - tw) / 2.0;
            }
        }
        it.x + pad - e.scroll
    }

    /// Держим каретку в видимой части поля.
    fn sync_scroll(&mut self, l: &Layout) {
        let Some(id) = self.focus else { return };
        let Some(it) = l.items.iter().find(|i| i.id == id && i.k == K::Edit).copied() else { return };
        let scale = self.scale;
        let Some(e) = self.edit(id) else { return };
        if e.numeric {
            return;
        }
        let inner = it.w - 28.0;
        let caret = self.tw(F::Input, &e.t[..e.caret]) / scale;
        let full = self.tw(F::Input, &e.t) / scale;
        let mut s = e.scroll;
        if caret - s > inner {
            s = caret - inner;
        }
        if caret - s < 0.0 {
            s = caret;
        }
        s = s.min((full - inner).max(0.0)).max(0.0);
        if let Some(e) = self.edit_mut(id) {
            e.scroll = s;
        }
    }

    fn draw(&mut self, l: &Layout) {
        let s = self.scale;
        let sp = Sp { s, ox: SH_L * s, oy: SH_T * s };
        let Some(mut pm) = Pixmap::new(self.sw.max(1) as u32, self.sh.max(1) as u32) else { return };

        // тень окна не рисуем: на живом фоне мягкий градиент тени выглядел
        // грязным ореолом, окно ограничено собственным скруглённым краем

        // корпус: linear-gradient(180deg, rgba(30,32,52,.97), rgba(19,20,34,.98))
        let panel = sp.rr(0.0, 0.0, W, l.h, PANEL_R);
        fill(
            &mut pm,
            &panel,
            lin(sp.x(0.0), sp.y(0.0), sp.x(0.0), sp.y(l.h),
                vec![(0.0, Color::from_rgba8(30, 32, 52, 247)), (1.0, Color::from_rgba8(19, 20, 34, 250))]),
            None,
        );
        // inset 0 1px 0 rgba(255,255,255,.07) — светлая линия по верхней кромке
        {
            let mut p = Paint::default();
            p.shader = lin(sp.x(0.0), sp.y(0.0), sp.x(0.0), sp.y(l.h * 0.35),
                vec![(0.0, col(WHITE, 0.07)), (1.0, col(WHITE, 0.0))]);
            p.anti_alias = true;
            let mut st = Stroke::default();
            st.width = s.max(1.0);
            pm.stroke_path(&panel, &p, &st, Transform::identity(), None);
        }

        // маска блока «Дополнительно» (полоса раскрытия)
        let band_h = l.band.1 - l.band.0;
        let band_mask = if band_h > 0.5 {
            let mut m = tiny_skia::Mask::new(pm.width(), pm.height()).unwrap();
            m.fill_path(
                &round_rect(sp.x(0.0), sp.y(l.band.0), sp.l(W), sp.l(band_h), 0.0),
                FillRule::Winding,
                false,
                Transform::identity(),
            );
            Some(m)
        } else {
            None
        };

        // шапка: кружок-микрофон
        let badge = sp.rr(PAD, 17.0, 26.0, 26.0, 13.0);
        fill(&mut pm, &badge,
            lin(sp.x(PAD), sp.y(17.0), sp.x(PAD + 26.0), sp.y(43.0),
                vec![(0.0, col(VIOLET, 1.0)), (1.0, col(PINK, 1.0))]),
            None);
        mic_glyph(&mut pm, sp.x(PAD + 13.0), sp.y(30.0), sp.l(13.0), WHITE, 1.0);

        // разделители: linear-gradient(90deg, transparent, rgba(255,255,255,.08) 20%..80%, transparent)
        for &dy in &l.divs {
            let path = round_rect(sp.x(PAD), sp.y(dy), sp.l(CW), s.max(1.0), 0.0);
            fill(&mut pm, &path,
                lin(sp.x(PAD), 0.0, sp.x(PAD + CW), 0.0,
                    vec![(0.0, col(WHITE, 0.0)), (0.2, col(WHITE, 0.08)), (0.8, col(WHITE, 0.08)), (1.0, col(WHITE, 0.0))]),
                None);
        }

        for it in &l.items {
            if it.more && (it.y + it.h <= l.band.0 || it.y >= l.band.1) {
                continue;
            }
            let mask = if it.more { band_mask.as_ref() } else { None };
            let hov = self.hov(it.id);
            let act = self.anim[it.id as usize];
            let focused = self.focus == Some(it.id);
            match it.k {
                K::Icon => self.draw_close(&mut pm, sp, it, hov),
                K::Edit | K::Select => {
                    let f = if focused { 1.0 } else { 0.0 };
                    self.draw_field(&mut pm, sp, it, f, hov, mask);
                    if it.k == K::Select {
                        chevron(&mut pm, sp.x(it.x + it.w - 20.0), sp.y(it.cy()), sp.l(14.0), TXT_GREY, 1.0, 0.0);
                    } else if focused {
                        self.draw_caret_sel(&mut pm, sp, it, mask);
                    }
                }
                K::Seg => self.draw_seg(&mut pm, sp, it, act, hov, focused),
                K::Toggle => self.draw_toggle(&mut pm, sp, it, act, focused, mask),
                K::Slider => self.draw_slider(&mut pm, sp, it, focused, mask),
                K::More => {
                    let c = lerp(TXT_DIM, TXT, hov);
                    chevron(&mut pm, sp.x(it.x + 7.0), sp.y(it.cy()), sp.l(14.0), c, 1.0,
                        lerp_f(-0.25, 0.0, ease(self.expand_t)));
                    if focused {
                        self.focus_ring(&mut pm, sp, it.x - 4.0, it.y - 2.0, it.w + 8.0, it.h + 4.0, 8.0);
                    }
                }
                K::Btn => self.draw_button(&mut pm, sp, it, hov, focused),
            }
        }

        self.blit(&pm, l, sp);
    }

    fn hov(&self, id: Id) -> f32 {
        self.hovt[id as usize]
    }

    fn focus_ring(&self, pm: &mut Pixmap, sp: Sp, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let path = sp.rr(x, y, w, h, r);
        stroke_c(pm, &path, VIOLET, 0.75, sp.l(1.6), None);
    }

    fn draw_close(&self, pm: &mut Pixmap, sp: Sp, it: &Item, hov: f32) {
        if hov > 0.01 {
            let path = sp.rr(it.x, it.y, it.w, it.h, it.h / 2.0);
            fill_c(pm, &path, WHITE, 0.07 * hov, None);
        }
        let c = lerp(TXT_GREY, TXT, hov);
        let (cx, cy) = (sp.x(it.x + it.w / 2.0), sp.y(it.cy()));
        let r = sp.l(15.0) / 24.0 * 6.0;
        let mut pb = PathBuilder::new();
        pb.move_to(cx - r, cy - r);
        pb.line_to(cx + r, cy + r);
        pb.move_to(cx + r, cy - r);
        pb.line_to(cx - r, cy + r);
        if let Some(p) = pb.finish() {
            stroke_c(pm, &p, c, 1.0, sp.l(15.0) / 24.0 * 1.9, None);
        }
    }

    fn draw_field(&self, pm: &mut Pixmap, sp: Sp, it: &Item, focus: f32, hov: f32, mask: Option<&tiny_skia::Mask>) {
        if focus > 0.01 {
            // box-shadow: 0 0 0 3px rgba(139,124,247,.18)
            let ring = sp.rr(it.x - 3.0, it.y - 3.0, it.w + 6.0, it.h + 6.0, FIELD_R + 3.0);
            fill_c(pm, &ring, VIOLET, 0.18 * focus, mask);
        }
        let path = sp.rr(it.x, it.y, it.w, it.h, FIELD_R);
        fill_c(pm, &path, SUNKEN, 0.6, mask);
        let bw = sp.l(1.0).max(1.0);
        let inner = sp.rr(it.x + 0.5, it.y + 0.5, it.w - 1.0, it.h - 1.0, FIELD_R - 0.5);
        let border = if focus > 0.01 {
            (lerp(WHITE, VIOLET, focus), lerp_f(0.08, 0.6, focus))
        } else {
            (WHITE, lerp_f(0.08, 0.14, hov))
        };
        stroke_c(pm, &inner, border.0, border.1, bw, mask);
    }

    /// Выделение и каретка в поле с фокусом.
    fn draw_caret_sel(&self, pm: &mut Pixmap, sp: Sp, it: &Item, mask: Option<&tiny_skia::Mask>) {
        let Some(e) = self.edit(it.id) else { return };
        let base = self.edit_text_x(it, e);
        let (a, b) = e.sel();
        let x0 = base + self.tw(F::Input, &e.t[..a]) / self.scale;
        let x1 = base + self.tw(F::Input, &e.t[..b]) / self.scale;
        let (ty, th) = (it.cy() - 9.0, 18.0);
        if b > a {
            let l = x0.max(it.x + 4.0);
            let r = x1.min(it.x + it.w - 4.0);
            if r > l {
                let path = sp.rr(l, ty, r - l, th, 3.0);
                fill_c(pm, &path, VIOLET, 0.35, mask);
            }
        } else if self.caret_on {
            let cx = base + self.tw(F::Input, &e.t[..e.caret]) / self.scale;
            if cx >= it.x + 3.0 && cx <= it.x + it.w - 3.0 {
                let path = sp.rr(cx - 0.7, ty, 1.4, th, 0.7);
                fill_c(pm, &path, TXT, 0.95, mask);
            }
        }
    }

    fn draw_seg(&self, pm: &mut Pixmap, sp: Sp, it: &Item, act: f32, hov: f32, focused: bool) {
        // контейнер рисуем один раз — на первом сегменте
        if it.id == Id::Mode0 {
            let path = sp.rr(it.x - 4.0, it.y - 4.0, CW, it.h + 8.0, FIELD_R);
            fill_c(pm, &path, SUNKEN, 0.6, None);
        }
        if act > 0.01 {
            shadow(pm, sp, it.x, it.y, it.w, it.h, 9.0, VIOLET, 0.5 * act, 10.0, 2.0, -2.0);
            let path = sp.rr(it.x, it.y, it.w, it.h, 9.0);
            fill(pm, &path,
                lin(sp.x(it.x), sp.y(it.y), sp.x(it.x + it.w), sp.y(it.y + it.h),
                    vec![(0.0, col(VIOLET, 0.85 * act)), (1.0, col(MAGENTA, 0.75 * act))]),
                None);
        } else if hov > 0.01 {
            let path = sp.rr(it.x, it.y, it.w, it.h, 9.0);
            fill_c(pm, &path, WHITE, 0.05 * hov, None);
        }
        if focused {
            self.focus_ring(pm, sp, it.x, it.y, it.w, it.h, 9.0);
        }
    }

    fn draw_toggle(&self, pm: &mut Pixmap, sp: Sp, it: &Item, on: f32, focused: bool, mask: Option<&tiny_skia::Mask>) {
        let track = sp.rr(it.x, it.y, it.w, it.h, it.h / 2.0);
        fill_c(pm, &track, WHITE, 0.12, mask);
        if on > 0.01 {
            fill(pm, &track,
                lin(sp.x(it.x), sp.y(it.y), sp.x(it.x + it.w), sp.y(it.y + it.h),
                    vec![(0.0, col(VIOLET, on)), (1.0, col(PINK, on))]),
                mask);
        }
        if focused {
            self.focus_ring(pm, sp, it.x - 3.0, it.y - 3.0, it.w + 6.0, it.h + 6.0, it.h / 2.0 + 3.0);
        }
        // ручка: transform .3s cubic-bezier(.34,1.4,.64,1) — с лёгким перелётом
        let k = cubic_bezier(0.34, 1.4, 0.64, 1.0, on);
        let kx = it.x + 3.0 + 18.0 * k;
        shadow(pm, sp, kx, it.y + 3.0, 20.0, 20.0, 10.0, (0, 0, 0), 0.4, 6.0, 2.0, 0.0);
        let knob = sp.rr(kx, it.y + 3.0, 20.0, 20.0, 10.0);
        fill_c(pm, &knob, WHITE, 1.0, mask);
    }

    fn draw_slider(&self, pm: &mut Pixmap, sp: Sp, it: &Item, focused: bool, mask: Option<&tiny_skia::Mask>) {
        let cy = it.cy();
        let (x0, x1) = (it.x + 8.0, it.x + it.w - 8.0);
        let t = ((self.ov_scale - 0.6) / 1.4).clamp(0.0, 1.0);
        let kx = x0 + (x1 - x0) * t;
        let track = sp.rr(it.x, cy - 2.0, it.w, 4.0, 2.0);
        fill_c(pm, &track, WHITE, 0.12, mask);
        let fillp = sp.rr(it.x, cy - 2.0, (kx - it.x).max(0.0), 4.0, 2.0);
        fill(pm, &fillp,
            lin(sp.x(it.x), 0.0, sp.x(it.x + it.w), 0.0,
                vec![(0.0, col(VIOLET, 1.0)), (1.0, col(PINK, 1.0))]),
            mask);
        shadow(pm, sp, kx - 8.0, cy - 8.0, 16.0, 16.0, 8.0, (0, 0, 0), 0.4, 6.0, 2.0, 0.0);
        let knob = sp.rr(kx - 8.0, cy - 8.0, 16.0, 16.0, 8.0);
        fill_c(pm, &knob, WHITE, 1.0, mask);
        if focused {
            self.focus_ring(pm, sp, it.x - 3.0, cy - 11.0, it.w + 6.0, 22.0, 11.0);
        }
    }

    fn draw_button(&self, pm: &mut Pixmap, sp: Sp, it: &Item, hov: f32, focused: bool) {
        match it.id {
            Id::Save => {
                shadow(pm, sp, it.x, it.y, it.w, it.h, FIELD_R, MAGENTA, lerp_f(0.7, 0.9, hov), lerp_f(20.0, 26.0, hov), 6.0, -8.0);
                let b = lerp_f(1.0, 1.08, hov);
                let br = |c: Rgb| ((c.0 as f32 * b).min(255.0) as u8, (c.1 as f32 * b).min(255.0) as u8, (c.2 as f32 * b).min(255.0) as u8);
                let path = sp.rr(it.x, it.y, it.w, it.h, FIELD_R);
                fill(pm, &path,
                    lin(sp.x(it.x), sp.y(it.y), sp.x(it.x + it.w), sp.y(it.y + it.h),
                        vec![(0.0, col(br(VIOLET), 1.0)), (0.5, col(br(MAGENTA), 1.0)), (1.0, col(br(PINK), 1.0))]),
                    None);
            }
            Id::Cancel => {
                let path = sp.rr(it.x + 0.5, it.y + 0.5, it.w - 1.0, it.h - 1.0, FIELD_R);
                stroke_c(pm, &path, WHITE, lerp_f(0.1, 0.2, hov), sp.l(1.0).max(1.0), None);
            }
            _ => {
                // кнопка горячей клавиши
                let cap = self.capturing;
                if cap {
                    let ring = sp.rr(it.x - 3.0, it.y - 3.0, it.w + 6.0, it.h + 6.0, 13.0);
                    fill_c(pm, &ring, VIOLET, 0.22, None);
                }
                let path = sp.rr(it.x, it.y, it.w, it.h, 10.0);
                if cap {
                    fill(pm, &path,
                        lin(sp.x(it.x), sp.y(it.y), sp.x(it.x + it.w), sp.y(it.y + it.h),
                            vec![(0.0, col(VIOLET, 0.5)), (1.0, col(PINK, 0.35))]),
                        None);
                } else {
                    fill_c(pm, &path, SUNKEN, 0.6, None);
                }
                let inner = sp.rr(it.x + 0.5, it.y + 0.5, it.w - 1.0, it.h - 1.0, 10.0);
                let (bc, ba) = if cap { (VIOLET, 0.8) } else { (WHITE, lerp_f(0.1, 0.2, hov)) };
                stroke_c(pm, &inner, bc, ba, sp.l(1.0).max(1.0), None);
            }
        }
        if focused {
            self.focus_ring(pm, sp, it.x - 3.0, it.y - 3.0, it.w + 6.0, it.h + 6.0, FIELD_R + 3.0);
        }
    }

    /// Текст рисуется GDI поверх готового фона (внутри панели альфа = 255).
    fn blit(&mut self, pm: &Pixmap, l: &Layout, sp: Sp) {
        unsafe {
            present(self.hwnd, self.dc, self.bits, self.sw, self.sh, pm, || {
                self.draw_text(l, sp)
            });
        }
    }

    unsafe fn draw_text(&self, l: &Layout, sp: Sp) {
        let dc = self.dc;
        for lb in &l.labs {
            if lb.more {
                let band_h = l.band.1 - l.band.0;
                if band_h <= 0.5 || lb.cy < l.band.0 - 12.0 || lb.cy > l.band.1 + 12.0 {
                    continue;
                }
                clip(dc, sp, 0.0, l.band.0, W, band_h);
            }
            let t: Vec<u16> = lb.s.encode_utf16().collect();
            let t = self.ellipsize(lb.f, &t, sp.l(lb.max_w), sp.l(lb.sp));
            let tw = self.tw(lb.f, &t) + sp.l(lb.sp) * t.len().saturating_sub(1) as f32;
            let x = match lb.al {
                Al::L => sp.x(lb.x),
                Al::C => sp.x(lb.x) - tw / 2.0,
            };
            self.text_out(dc, lb.f, &t, x, sp.y(lb.cy), lb.c, sp.l(lb.sp));
            if lb.more {
                let _ = SelectClipRgn(dc, None);
            }
        }
        // текст в полях ввода
        for it in l.items.iter().filter(|i| i.k == K::Edit) {
            let Some(e) = self.edit(it.id) else { continue };
            if it.more {
                let band_h = l.band.1 - l.band.0;
                if band_h <= 0.5 || it.y + it.h <= l.band.0 || it.y >= l.band.1 {
                    continue;
                }
                let top = it.y.max(l.band.0);
                clip(dc, sp, it.x + 2.0, top, it.w - 4.0, (it.y + it.h).min(l.band.1) - top);
            } else {
                clip(dc, sp, it.x + 2.0, it.y, it.w - 4.0, it.h);
            }
            let x = sp.x(self.edit_text_x(it, e));
            self.text_out(dc, F::Input, &e.t, x, sp.y(it.cy()), TXT, 0.0);
            let _ = SelectClipRgn(dc, None);
        }
    }

    unsafe fn text_out(&self, dc: HDC, f: F, t: &[u16], x: f32, cy: f32, c: Rgb, spacing: f32) {
        self.fonts.text_out(dc, f, t, x, cy, c, spacing);
    }

    /// Обрезает строку многоточием под ширину `max` (физ. px).
    fn ellipsize(&self, f: F, t: &[u16], max: f32, spacing: f32) -> Vec<u16> {
        self.fonts.ellipsize(self.dc, f, t, max, spacing)
    }
}

// ── поверхность и цикл перерисовки ────────────────────────────────────────
fn redraw() {
    with(|w| {
        let l = w.layout();
        let resized = (l.h - w.panel_h).abs() > 0.01;
        w.panel_h = l.h;
        let (sw, sh) = surface_size(w);
        if resized || sw != w.sw || sh != w.sh {
            unsafe {
                let (dib, bits) = make_dib(w.dc, w.dib, sw, sh);
                w.dib = dib;
                w.bits = bits;
                w.sw = sw;
                w.sh = sh;
                let _ = SetWindowPos(w.hwnd, HWND::default(), 0, 0, sw, sh,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        w.sync_scroll(&l);
        w.draw(&l);
    });
}

/// Шаг анимации к цели за время `dur`; возвращает true, пока идёт движение.
fn step(cur: &mut f32, target: f32, dur: f32, dt: f32) -> bool {
    if (*cur - target).abs() < 1e-3 {
        *cur = target;
        return false;
    }
    let d = dt / dur.max(0.001);
    if *cur < target {
        *cur = (*cur + d).min(target);
    } else {
        *cur = (*cur - d).max(target);
    }
    true
}

fn tick() {
    let need = with(|w| {
        let now = Instant::now();
        let dt = (now - w.last).as_secs_f32().clamp(0.001, 0.1);
        w.last = now;
        let mut moving = false;

        for id in ALL_IDS {
            let i = id as usize;
            let target = match id {
                Id::Mode0 => (w.mode == 0) as u8 as f32,
                Id::Mode1 => (w.mode == 1) as u8 as f32,
                Id::Mode2 => (w.mode == 2) as u8 as f32,
                Id::Live => w.live as u8 as f32,
                Id::Space => w.space as u8 as f32,
                Id::Caps => w.caps as u8 as f32,
                Id::Startup => w.startup as u8 as f32,
                Id::Updates => w.updates as u8 as f32,
                _ => 0.0,
            };
            let dur = match id {
                Id::Live | Id::Space | Id::Caps | Id::Startup | Id::Updates => 0.30,
                _ => 0.25,
            };
            moving |= step(&mut w.anim[i], target, dur, dt);
            let ht = (w.hot == Some(id)) as u8 as f32;
            moving |= step(&mut w.hovt[i], ht, 0.20, dt);
        }
        moving |= step(&mut w.expand_t, w.expanded as u8 as f32, 0.28, dt);

        // мигание каретки в поле с фокусом
        let in_edit = w.focus.map(|f| w.edit(f).is_some()).unwrap_or(false);
        if in_edit && !w.drag_edit {
            let period = std::time::Duration::from_millis(530);
            if w.caret_at.elapsed() >= period {
                w.caret_on = !w.caret_on;
                w.caret_at = now;
                moving = true;
            }
        } else if !w.caret_on {
            w.caret_on = true;
            moving = true;
        }

        let need = moving || w.dirty;
        w.dirty = false;
        need
    })
    .unwrap_or(false);
    if need {
        redraw();
    }
}

// ── попадание курсора ─────────────────────────────────────────────────────
/// Экранные/клиентские физ. px → логические координаты панели.
fn to_panel(w: &Win, x: i32, y: i32) -> (f32, f32) {
    (x as f32 / w.scale - SH_L, y as f32 / w.scale - SH_T)
}

fn hit(w: &Win, lx: f32, ly: f32) -> Option<Item> {
    let l = w.layout();
    l.items
        .iter()
        .rev()
        .find(|it| {
            if it.more && (it.y + it.h <= l.band.0 || it.y >= l.band.1) {
                return false;
            }
            it.hit(lx, ly)
        })
        .copied()
}

fn focusables(l: &Layout) -> Vec<Id> {
    l.items
        .iter()
        .filter(|it| it.k != K::Icon && !(it.more && it.y >= l.band.1))
        .map(|it| it.id)
        .collect()
}

// ── действия ──────────────────────────────────────────────────────────────
/// Что делать вызывающему ПОСЛЕ выхода из заимствования состояния: применять
/// настройки внутри `with(..)` нельзя — `apply` может показать модальное окно,
/// его цикл сообщений вернётся сюда и повторно займёт RefCell.
enum Act {
    None,
    Close,
    Save { cfg: crate::config::Config, startup: bool },
}

fn activate(w: &mut Win, id: Id) -> Act {
    match id {
        Id::Close | Id::Cancel => return Act::Close,
        Id::Save => return Act::Save { cfg: build_config(w), startup: w.startup },
        Id::Mode0 => w.mode = 0,
        Id::Mode1 => w.mode = 1,
        Id::Mode2 => w.mode = 2,
        Id::Live => w.live = !w.live,
        Id::Space => w.space = !w.space,
        Id::Caps => w.caps = !w.caps,
        Id::Startup => w.startup = !w.startup,
        Id::Updates => w.updates = !w.updates,
        Id::More => w.expanded = !w.expanded,
        Id::Hotkey => start_capture(w),
        Id::Device => popup_toggle(w),
        _ => {}
    }
    w.dirty = true;
    Act::None
}

/// Применить действие, накопленное обработчиком (уже вне заимствования).
fn finish(hwnd: HWND, act: Act) {
    match act {
        Act::None => {}
        Act::Close => close_window(hwnd),
        Act::Save { cfg, startup } => {
            crate::startup::set(startup);
            crate::ui::apply(cfg);
            close_window(hwnd);
        }
    }
}

fn start_capture(w: &mut Win) {
    if w.capturing {
        return;
    }
    w.capturing = true;
    w.focus = Some(Id::Hotkey);
    // пока ловим сочетание, глобальный хоткей снимаем — иначе он не дойдёт до окна
    crate::ui::pause_hotkey();
}

fn stop_capture(w: &mut Win) {
    if !w.capturing {
        return;
    }
    w.capturing = false;
    crate::ui::resume_hotkey();
}

fn split_words(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

/// Собирает конфиг из состояния окна (без побочных эффектов).
fn build_config(w: &Win) -> crate::config::Config {
    let mut cfg = shared().config.lock().unwrap().clone();
    cfg.device_name = if w.device == 0 {
        None
    } else {
        w.devices.get(w.device - 1).cloned()
    };
    let wake = split_words(&w.wake.text());
    if !wake.is_empty() {
        cfg.wake_words = wake;
    }
    let stop = split_words(&w.stop.text());
    if !stop.is_empty() {
        cfg.stop_words = stop;
    }
    if !w.hotkey.trim().is_empty() {
        cfg.hotkey = w.hotkey.trim().to_lowercase();
    }
    cfg.overlay_mode = ["always", "dictation", "hidden"][w.mode.min(2)].to_string();
    cfg.live_typing = w.live;
    cfg.append_space = w.space;
    cfg.capitalize = w.caps;
    cfg.overlay_scale = w.ov_scale;
    cfg.check_updates = w.updates;
    if let Ok(v) = w.silence.text().trim().parse::<f32>() {
        cfg.silence_timeout = v.clamp(0.0, 600.0);
    }
    if let Ok(v) = w.hotwords.text().trim().parse::<f32>() {
        cfg.hotwords_score = v.clamp(0.0, 20.0);
    }
    cfg
}

fn close_window(hwnd: HWND) {
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

// ── выпадающий список микрофонов ──────────────────────────────────────────
const P_ROW: f32 = 34.0;
const P_PAD: f32 = 6.0;
const P_MAX: usize = 7;
const P_SH: f32 = 16.0; // поле под тень слева/справа/сверху
const P_SHB: f32 = 30.0;

fn popup_rows(w: &Win) -> usize {
    w.devices.len() + 1
}

fn popup_toggle(w: &mut Win) {
    if w.popup_on {
        popup_close(w);
    } else {
        popup_show(w);
    }
}

fn popup_show(w: &mut Win) {
    let rows = popup_rows(w).min(P_MAX);
    let ph = P_PAD * 2.0 + rows as f32 * P_ROW;
    let s = w.scale;
    let sw = ((CW + P_SH * 2.0) * s).round() as i32;
    let sh = ((ph + P_SH + P_SHB) * s).round() as i32;
    unsafe {
        let (dib, bits) = make_dib(w.pdc, w.pdib, sw, sh);
        w.pdib = dib;
        w.pbits = bits;
        w.pw = sw;
        w.ph = sh;

        // под полем выбора, в экранных координатах
        let mut rc = RECT::default();
        let _ = GetWindowRect(w.hwnd, &mut rc);
        let l = w.layout();
        let sel = l.items.iter().find(|i| i.id == Id::Device).copied();
        let (fx, fy, fh) = sel.map(|i| (i.x, i.y, i.h)).unwrap_or((PAD, 100.0, FIELD_H));
        let x = rc.left + ((SH_L + fx - P_SH) * s).round() as i32;
        let mut y = rc.top + ((SH_T + fy + fh + 6.0 - P_SH) * s).round() as i32;
        let wa = work_area(POINT { x: x + sw / 2, y });
        if y + sh > wa.bottom {
            // не влезает снизу — показываем над полем
            y = rc.top + ((SH_T + fy - 6.0 - P_SH) * s).round() as i32 - sh + ((P_SH + P_SHB) * s) as i32;
        }
        let _ = SetWindowPos(w.popup, HWND_TOPMOST, x, y, sw, sh, SWP_NOACTIVATE);
    }
    w.popup_on = true;
    // прокрутка так, чтобы выбранный пункт был виден
    let rows_all = popup_rows(w);
    w.popup_top = if w.device + 1 > P_MAX { (w.device + 1 - P_MAX).min(rows_all.saturating_sub(P_MAX)) } else { 0 };
    w.popup_hot = w.device as i32;
    popup_draw(w);
    unsafe {
        let _ = ShowWindow(w.popup, SW_SHOWNOACTIVATE);
    }
    w.dirty = true;
}

fn popup_close(w: &mut Win) {
    if !w.popup_on {
        return;
    }
    w.popup_on = false;
    unsafe {
        let _ = ShowWindow(w.popup, SW_HIDE);
    }
    w.dirty = true;
}

fn popup_item(w: &Win, i: usize) -> String {
    if i == 0 {
        "По умолчанию".to_string()
    } else {
        w.devices.get(i - 1).cloned().unwrap_or_default()
    }
}

fn popup_draw(w: &mut Win) {
    if w.pbits.is_null() {
        return;
    }
    let s = w.scale;
    let sp = Sp { s, ox: P_SH * s, oy: P_SH * s };
    let rows = popup_rows(w).min(P_MAX);
    let ph = P_PAD * 2.0 + rows as f32 * P_ROW;
    let Some(mut pm) = Pixmap::new(w.pw.max(1) as u32, w.ph.max(1) as u32) else { return };

    shadow(&mut pm, sp, 0.0, 0.0, CW, ph, FIELD_R, (0, 0, 0), 0.75, 40.0, 14.0, -10.0);
    let panel = sp.rr(0.0, 0.0, CW, ph, FIELD_R);
    fill(&mut pm, &panel,
        lin(sp.x(0.0), sp.y(0.0), sp.x(0.0), sp.y(ph),
            vec![(0.0, Color::from_rgba8(30, 32, 52, 250)), (1.0, Color::from_rgba8(19, 20, 34, 252))]),
        None);
    let border = sp.rr(0.5, 0.5, CW - 1.0, ph - 1.0, FIELD_R);
    stroke_c(&mut pm, &border, WHITE, 0.08, sp.l(1.0).max(1.0), None);

    let total = popup_rows(w);
    for r in 0..rows {
        let i = w.popup_top + r;
        if i >= total {
            break;
        }
        let y = P_PAD + r as f32 * P_ROW;
        if w.popup_hot == i as i32 {
            let path = sp.rr(4.0, y, CW - 8.0, P_ROW, 8.0);
            fill_c(&mut pm, &path, WHITE, 0.06, None);
        }
        if w.device == i {
            // галочка выбранного пункта
            let (cx, cy) = (sp.x(CW - 22.0), sp.y(y + P_ROW / 2.0));
            let g = sp.l(14.0) / 24.0;
            let mut pb = PathBuilder::new();
            pb.move_to(cx + (5.0 - 12.0) * g, cy + (12.5 - 12.0) * g);
            pb.line_to(cx + (10.0 - 12.0) * g, cy + (17.0 - 12.0) * g);
            pb.line_to(cx + (19.0 - 12.0) * g, cy + (7.0 - 12.0) * g);
            if let Some(p) = pb.finish() {
                stroke_c(&mut pm, &p, VIOLET, 1.0, 2.2 * g, None);
            }
        }
    }

    // текст пунктов
    unsafe {
        present(w.popup, w.pdc, w.pbits, w.pw, w.ph, &pm, || {
            for r in 0..rows {
                let i = w.popup_top + r;
                if i >= total {
                    break;
                }
                let y = P_PAD + r as f32 * P_ROW;
                let txt: Vec<u16> = popup_item(w, i).encode_utf16().collect();
                let txt = w.ellipsize(F::Input, &txt, sp.l(CW - 46.0), 0.0);
                let c = if w.device == i { TXT } else { lerp(TXT, TXT_DIM, 0.5) };
                clip(w.pdc, sp, 0.0, y, CW, P_ROW);
                w.text_out(w.pdc, F::Input, &txt, sp.x(14.0), sp.y(y + P_ROW / 2.0), c, 0.0);
                let _ = SelectClipRgn(w.pdc, None);
            }
        });
    }
}

/// Номер пункта под курсором в координатах окна-списка (физ. px).
fn popup_row_at(w: &Win, x: i32, y: i32) -> i32 {
    let s = w.scale;
    let lx = x as f32 / s - P_SH;
    let ly = y as f32 / s - P_SH;
    if lx < 0.0 || lx > CW {
        return -1;
    }
    let rows = popup_rows(w).min(P_MAX);
    let r = ((ly - P_PAD) / P_ROW).floor();
    if r < 0.0 || r >= rows as f32 {
        return -1;
    }
    let i = w.popup_top + r as usize;
    if i >= popup_rows(w) {
        -1
    } else {
        i as i32
    }
}

fn popup_pick(w: &mut Win, i: usize) {
    if i < popup_rows(w) {
        w.device = i;
    }
    popup_close(w);
}

fn popup_scroll(w: &mut Win, delta: i32) {
    let total = popup_rows(w);
    if total <= P_MAX {
        return;
    }
    let max_top = total - P_MAX;
    let t = w.popup_top as i32 - delta;
    w.popup_top = t.clamp(0, max_top as i32) as usize;
    popup_draw(w);
}

extern "system" fn popup_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEMOVE => {
                let (x, y) = (lo(lp), hi(lp));
                with(|w| {
                    let r = popup_row_at(w, x, y);
                    if r != w.popup_hot {
                        w.popup_hot = r;
                        popup_draw(w);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = (lo(lp), hi(lp));
                with(|w| {
                    let r = popup_row_at(w, x, y);
                    if r >= 0 {
                        popup_pick(w, r as usize);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let d = ((wp.0 >> 16) as i16) as i32 / 120;
                with(|w| popup_scroll(w, d));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn lo(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as i16 as i32
}
fn hi(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as i16 as i32
}

// ── буфер обмена ──────────────────────────────────────────────────────────
unsafe fn clipboard_get() -> Option<String> {
    if OpenClipboard(HWND::default()).is_err() {
        return None;
    }
    let mut out = None;
    if let Ok(h) = GetClipboardData(13u32) {
        // CF_UNICODETEXT
        let p = GlobalLock(HGLOBAL(h.0 as *mut c_void)) as *const u16;
        if !p.is_null() {
            let mut n = 0usize;
            while *p.add(n) != 0 && n < 1 << 20 {
                n += 1;
            }
            out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(p, n)));
            let _ = GlobalUnlock(HGLOBAL(h.0 as *mut c_void));
        }
    }
    let _ = CloseClipboard();
    out
}

unsafe fn clipboard_set(s: &str) {
    if OpenClipboard(HWND::default()).is_err() {
        return;
    }
    let _ = EmptyClipboard();
    let t = wide(s);
    if let Ok(h) = GlobalAlloc(GMEM_MOVEABLE, t.len() * 2) {
        let p = GlobalLock(h) as *mut u16;
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(t.as_ptr(), p, t.len());
            let _ = GlobalUnlock(h);
            let _ = SetClipboardData(13u32, HANDLE(h.0));
        }
    }
    let _ = CloseClipboard();
}

fn key_down(vk: u32) -> bool {
    unsafe { (GetKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

// ── оконная процедура ─────────────────────────────────────────────────────
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                if wp.0 == TIMER {
                    tick();
                }
                LRESULT(0)
            }
            WM_NCHITTEST => {
                let mut rc = RECT::default();
                let _ = GetWindowRect(hwnd, &mut rc);
                let (sx, sy) = (lo(lp) - rc.left, hi(lp) - rc.top);
                let r = with(|w| {
                    let (lx, ly) = to_panel(w, sx, sy);
                    let inside = sd_rrect(lx - W / 2.0, ly - w.panel_h / 2.0, W / 2.0, w.panel_h / 2.0, PANEL_R) <= 0.0;
                    if !inside {
                        return HTTRANSPARENT;
                    }
                    let over_item = hit(w, lx, ly).is_some();
                    if ly < TITLE_H && !over_item {
                        HTCAPTION as i32
                    } else {
                        HTCLIENT as i32
                    }
                })
                .unwrap_or(HTTRANSPARENT);
                LRESULT(r as isize)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (lo(lp), hi(lp));
                with(|w| {
                    let (lx, ly) = to_panel(w, x, y);
                    if w.drag_edit {
                        if let Some(id) = w.focus {
                            let l = w.layout();
                            if let Some(it) = l.items.iter().find(|i| i.id == id).copied() {
                                let base = w.edit(id).map(|e| w.edit_text_x(&it, e)).unwrap_or(0.0);
                                let rel = (lx - base) * w.scale;
                                let t = w.edit(id).map(|e| e.t.clone()).unwrap_or_default();
                                let pos = w.caret_from_x(F::Input, &t, rel);
                                if let Some(e) = w.edit_mut(id) {
                                    e.caret = pos;
                                }
                                w.dirty = true;
                            }
                        }
                    } else if w.drag_slider {
                        let l = w.layout();
                        if let Some(it) = l.items.iter().find(|i| i.id == Id::OvScale).copied() {
                            set_slider(w, &it, lx);
                        }
                    } else {
                        let h = hit(w, lx, ly).map(|i| i.id);
                        if h != w.hot {
                            w.hot = h;
                            w.dirty = true;
                        }
                        if !w.track_leave {
                            let mut tme = TRACKMOUSEEVENT {
                                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                                dwFlags: TME_LEAVE,
                                hwndTrack: hwnd,
                                dwHoverTime: 0,
                            };
                            let _ = TrackMouseEvent(&mut tme);
                            w.track_leave = true;
                        }
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                with(|w| {
                    w.track_leave = false;
                    if w.hot.is_some() {
                        w.hot = None;
                        w.dirty = true;
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = (lo(lp), hi(lp));
                with(|w| {
                    let (lx, ly) = to_panel(w, x, y);
                    let it = hit(w, lx, ly);
                    if w.popup_on && it.map(|i| i.id) != Some(Id::Device) {
                        popup_close(w);
                    }
                    if w.capturing && it.map(|i| i.id) != Some(Id::Hotkey) {
                        stop_capture(w);
                    }
                    match it {
                        Some(it) if it.k == K::Edit => {
                            w.focus = Some(it.id);
                            let base = w.edit(it.id).map(|e| w.edit_text_x(&it, e)).unwrap_or(0.0);
                            let rel = (lx - base) * w.scale;
                            let t = w.edit(it.id).map(|e| e.t.clone()).unwrap_or_default();
                            let pos = w.caret_from_x(F::Input, &t, rel);
                            let dbl = msg == WM_LBUTTONDBLCLK;
                            if let Some(e) = w.edit_mut(it.id) {
                                if dbl {
                                    e.select_all();
                                } else {
                                    e.caret = pos;
                                    e.anchor = pos;
                                }
                            }
                            w.drag_edit = !dbl;
                            w.caret_on = true;
                            w.caret_at = Instant::now();
                            SetCapture(hwnd);
                        }
                        Some(it) if it.k == K::Slider => {
                            w.focus = Some(it.id);
                            set_slider(w, &it, lx);
                            w.drag_slider = true;
                            SetCapture(hwnd);
                        }
                        Some(it) => {
                            if it.k != K::Icon {
                                w.focus = Some(it.id);
                            }
                            w.press = Some(it.id);
                        }
                        None => {
                            w.focus = None;
                            w.press = None;
                        }
                    }
                    w.dirty = true;
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = (lo(lp), hi(lp));
                let act = with(|w| {
                    let _ = ReleaseCapture();
                    w.drag_edit = false;
                    w.drag_slider = false;
                    let (lx, ly) = to_panel(w, x, y);
                    let it = hit(w, lx, ly);
                    let press = w.press.take();
                    w.dirty = true;
                    match (it, press) {
                        (Some(it), Some(p)) if it.id == p => activate(w, p),
                        _ => Act::None,
                    }
                })
                .unwrap_or(Act::None);
                finish(hwnd, act);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let d = ((wp.0 >> 16) as i16) as i32 / 120;
                with(|w| {
                    if w.popup_on {
                        popup_scroll(w, d);
                    }
                });
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let over = with(|w| {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let mut rc = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut rc);
                    let (lx, ly) = to_panel(w, pt.x - rc.left, pt.y - rc.top);
                    hit(w, lx, ly).map(|i| i.k)
                })
                .flatten();
                match over {
                    Some(K::Edit) => {
                        SetCursor(LoadCursorW(None, IDC_IBEAM).unwrap_or_default());
                        LRESULT(1)
                    }
                    Some(_) => {
                        SetCursor(LoadCursorW(None, IDC_HAND).unwrap_or_default());
                        LRESULT(1)
                    }
                    None => DefWindowProcW(hwnd, msg, wp, lp),
                }
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let act = with(|w| on_key(w, wp.0 as u32)).unwrap_or(Act::None);
                finish(hwnd, act);
                LRESULT(0)
            }
            WM_CHAR => {
                with(|w| {
                    let ch = wp.0 as u16;
                    if ch < 0x20 || key_down(VK_CONTROL.0 as u32) || w.capturing {
                        return;
                    }
                    if let Some(id) = w.focus {
                        if w.edit(id).is_some() {
                            if let Some(e) = w.edit_mut(id) {
                                e.insert(ch);
                            }
                            w.caret_on = true;
                            w.caret_at = Instant::now();
                            w.dirty = true;
                        }
                    }
                });
                LRESULT(0)
            }
            WM_ACTIVATE => {
                if (wp.0 & 0xFFFF) == 0 {
                    with(|w| {
                        popup_close(w);
                        stop_capture(w);
                    });
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wp.0 & 0xFFFF) as u32;
                let rc = *(lp.0 as *const RECT);
                with(|w| {
                    w.scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
                    w.fonts.free();
                    w.fonts = Fonts::new(w.dc, w.scale);
                    w.sw = 0; // заставит пересоздать DIB нужного размера
                    let _ = SetWindowPos(w.hwnd, HWND::default(), rc.left, rc.top, 10, 10,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE);
                    if w.popup_on {
                        popup_close(w);
                    }
                    w.dirty = true;
                });
                redraw();
                LRESULT(0)
            }
            WM_CLOSE => {
                close_window(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                KillTimer(hwnd, TIMER).ok();
                ST.with(|s| {
                    if let Some(w) = s.borrow_mut().take() {
                        let mut w = w;
                        if w.capturing {
                            crate::ui::resume_hotkey();
                        }
                        w.fonts.free();
                        if !w.dib.0.is_null() {
                            let _ = DeleteObject(w.dib);
                        }
                        if !w.pdib.0.is_null() {
                            let _ = DeleteObject(w.pdib);
                        }
                        let _ = DeleteDC(w.dc);
                        let _ = DeleteDC(w.pdc);
                        if !w.popup.0.is_null() {
                            let _ = DestroyWindow(w.popup);
                        }
                    }
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn set_slider(w: &mut Win, it: &Item, lx: f32) {
    let (x0, x1) = (it.x + 8.0, it.x + it.w - 8.0);
    let t = ((lx - x0) / (x1 - x0)).clamp(0.0, 1.0);
    // шаг 0.05 в диапазоне 0.6…2.0
    let v = 0.6 + (t * 1.4 / 0.05).round() * 0.05;
    w.ov_scale = (v * 100.0).round() / 100.0;
    w.dirty = true;
}

/// Обработка клавиш; результат применяется вне заимствования состояния.
fn on_key(w: &mut Win, vk: u32) -> Act {
    let shift = key_down(VK_SHIFT.0 as u32);
    let ctrl = key_down(VK_CONTROL.0 as u32);
    w.dirty = true;

    if w.capturing {
        return capture_key(w, vk);
    }

    match VIRTUAL_KEY(vk as u16) {
        VK_ESCAPE => {
            if w.popup_on {
                popup_close(w);
                return Act::None;
            }
            return Act::Close;
        }
        VK_RETURN => {
            if w.popup_on {
                let i = w.popup_hot.max(0) as usize;
                popup_pick(w, i);
                return Act::None;
            }
            if let Some(id) = w.focus {
                if matches!(id, Id::More | Id::Device | Id::Hotkey) {
                    return activate(w, id);
                }
            }
            return Act::Save { cfg: build_config(w), startup: w.startup };
        }
        VK_TAB => {
            let l = w.layout();
            let f = focusables(&l);
            if f.is_empty() {
                return Act::None;
            }
            let cur = w.focus.and_then(|c| f.iter().position(|&i| i == c));
            let n = f.len();
            let next = match cur {
                Some(i) if shift => (i + n - 1) % n,
                Some(i) => (i + 1) % n,
                None if shift => n - 1,
                None => 0,
            };
            w.focus = Some(f[next]);
            w.caret_on = true;
            w.caret_at = Instant::now();
            if let Some(e) = w.edit_mut(f[next]) {
                e.select_all();
            }
            return Act::None;
        }
        VK_SPACE => {
            if let Some(id) = w.focus {
                if w.edit(id).is_none() {
                    return activate(w, id);
                }
            }
            return Act::None;
        }
        VK_UP | VK_DOWN if w.popup_on => {
            let total = popup_rows(w) as i32;
            let d = if VIRTUAL_KEY(vk as u16) == VK_UP { -1 } else { 1 };
            w.popup_hot = (w.popup_hot + d).clamp(0, total - 1);
            let top = w.popup_top as i32;
            if w.popup_hot < top {
                w.popup_top = w.popup_hot as usize;
            } else if w.popup_hot >= top + P_MAX as i32 {
                w.popup_top = (w.popup_hot - P_MAX as i32 + 1) as usize;
            }
            popup_draw(w);
            return Act::None;
        }
        VK_DOWN if w.focus == Some(Id::Device) => {
            popup_show(w);
            return Act::None;
        }
        _ => {}
    }

    let Some(id) = w.focus else { return Act::None };

    // поля ввода
    if w.edit(id).is_some() {
        let (t_len, caret) = w.edit(id).map(|e| (e.t.len(), e.caret)).unwrap_or((0, 0));
        let (wl, wr) = w.edit(id).map(|e| (e.word_left(), e.word_right())).unwrap_or((0, 0));
        let sel_text = w.edit(id).map(|e| e.sel_text()).unwrap_or_default();
        let has_sel = w.edit(id).map(|e| e.has_sel()).unwrap_or(false);
        let paste = ctrl && vk == 0x56; // V
        let clip_txt = if paste { unsafe { clipboard_get() } } else { None };
        w.caret_on = true;
        w.caret_at = Instant::now();
        let Some(e) = w.edit_mut(id) else { return Act::None };
        match (VIRTUAL_KEY(vk as u16), vk) {
            (VK_LEFT, _) => {
                let p = if ctrl { wl } else { caret.saturating_sub(1) };
                e.move_to(p, shift);
            }
            (VK_RIGHT, _) => {
                let p = if ctrl { wr } else { (caret + 1).min(t_len) };
                e.move_to(p, shift);
            }
            (VK_HOME, _) => e.move_to(0, shift),
            (VK_END, _) => e.move_to(t_len, shift),
            (VK_BACK, _) => e.backspace(),
            (VK_DELETE, _) if !ctrl => {
                if shift && has_sel {
                    unsafe { clipboard_set(&sel_text) };
                }
                e.delete()
            }
            (_, 0x41) if ctrl => e.select_all(), // A
            (_, 0x43) if ctrl => unsafe { clipboard_set(&sel_text) }, // C
            (_, 0x58) if ctrl => {
                // X
                unsafe { clipboard_set(&sel_text) };
                e.del_sel();
            }
            (_, 0x56) if ctrl => {
                if let Some(s) = clip_txt {
                    e.insert_str(s.split(['\r', '\n']).next().unwrap_or(""));
                }
            }
            _ => {}
        }
        return Act::None;
    }

    // сегменты и слайдер стрелками
    match (VIRTUAL_KEY(vk as u16), id) {
        (VK_LEFT, Id::Mode0 | Id::Mode1 | Id::Mode2) => {
            w.mode = w.mode.saturating_sub(1);
            w.focus = Some([Id::Mode0, Id::Mode1, Id::Mode2][w.mode]);
        }
        (VK_RIGHT, Id::Mode0 | Id::Mode1 | Id::Mode2) => {
            w.mode = (w.mode + 1).min(2);
            w.focus = Some([Id::Mode0, Id::Mode1, Id::Mode2][w.mode]);
        }
        (VK_LEFT, Id::OvScale) => w.ov_scale = ((w.ov_scale - 0.05) * 100.0).round() / 100.0,
        (VK_RIGHT, Id::OvScale) => w.ov_scale = ((w.ov_scale + 0.05) * 100.0).round() / 100.0,
        _ => {}
    }
    w.ov_scale = w.ov_scale.clamp(0.6, 2.0);
    Act::None
}

/// Захват сочетания клавиш (Esc отменяет захват, окно не закрывает).
fn capture_key(w: &mut Win, vk: u32) -> Act {
    let v = VIRTUAL_KEY(vk as u16);
    if v == VK_ESCAPE {
        stop_capture(w);
        return Act::None;
    }
    // модификаторы сами по себе игнорируем — ждём основную клавишу
    if matches!(v, VK_CONTROL | VK_SHIFT | VK_MENU | VK_LWIN | VK_RWIN)
        || vk == 0xA0 || vk == 0xA1 || vk == 0xA2 || vk == 0xA3 || vk == 0xA4 || vk == 0xA5
    {
        return Act::None;
    }
    let key = match vk {
        0x41..=0x5A => Some(((vk as u8) as char).to_ascii_lowercase().to_string()),
        0x30..=0x39 => Some(((vk as u8) as char).to_string()),
        0x20 => Some("space".to_string()),
        0x70..=0x7B => Some(format!("f{}", vk - 0x70 + 1)),
        _ => None,
    };
    let Some(key) = key else { return Act::None };
    let mut parts = Vec::new();
    if key_down(VK_CONTROL.0 as u32) {
        parts.push("ctrl".to_string());
    }
    if key_down(VK_MENU.0 as u32) {
        parts.push("alt".to_string());
    }
    if key_down(VK_SHIFT.0 as u32) {
        parts.push("shift".to_string());
    }
    if key_down(VK_LWIN.0 as u32) || key_down(VK_RWIN.0 as u32) {
        parts.push("win".to_string());
    }
    if parts.is_empty() {
        return Act::None; // без модификатора глобальный хоткей бесполезен
    }
    parts.push(key);
    w.hotkey = parts.join("+");
    stop_capture(w);
    Act::None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Открывает настоящее окно настроек и крутит цикл сообщений
    /// (`SETTINGS_SECS=30 cargo test settings_live -- --ignored --nocapture`) —
    /// для проверки живого поведения: ховеры, ввод, раскрытие блока.
    #[test]
    #[ignore]
    fn settings_live() {
        let secs = std::env::var("SETTINGS_SECS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(20);
        crate::shared::init(crate::config::Config::default());
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        open();
        let end = Instant::now() + std::time::Duration::from_secs(secs);
        let mut msg = MSG::default();
        while Instant::now() < end {
            unsafe {
                while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
    }

    /// Рендер окна в PNG для визуальной проверки макета:
    /// `SETTINGS_SCALE=1.5 SETTINGS_MORE=1 cargo test settings_preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn settings_preview() {
        let scale = std::env::var("SETTINGS_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let more = std::env::var("SETTINGS_MORE").map(|v| v == "1").unwrap_or(false);
        unsafe {
            let screen = GetDC(HWND::default());
            let dc = CreateCompatibleDC(screen);
            let pdc = CreateCompatibleDC(screen);
            ReleaseDC(HWND::default(), screen);
            SetBkMode(dc, TRANSPARENT);
            SetBkMode(pdc, TRANSPARENT);

            let mut cfg = crate::config::Config::default();
            cfg.hotkey = "ctrl+alt+j".into();
            cfg.overlay_mode = "dictation".into();
            let devices = vec![
                "Микрофон (USB PnP Audio Device)".to_string(),
                "Микрофон гарнитуры (Bluetooth)".to_string(),
            ];
            let mut w = make_win(HWND::default(), HWND::default(), dc, pdc, scale, &cfg, devices, 1);
            w.expanded = more;
            w.expand_t = more as u8 as f32;
            for (i, id) in [Id::Mode0, Id::Mode1, Id::Mode2].iter().enumerate() {
                w.anim[*id as usize] = (w.mode == i) as u8 as f32;
            }
            w.anim[Id::Live as usize] = w.live as u8 as f32;
            w.anim[Id::Space as usize] = w.space as u8 as f32;
            w.anim[Id::Caps as usize] = w.caps as u8 as f32;
            w.anim[Id::Startup as usize] = w.startup as u8 as f32;
            w.anim[Id::Updates as usize] = w.updates as u8 as f32;

            let l = w.layout();
            w.panel_h = l.h;
            let (sw, sh) = surface_size(&w);
            let (dib, bits) = make_dib(dc, HBITMAP::default(), sw, sh);
            w.dib = dib;
            w.bits = bits;
            w.sw = sw;
            w.sh = sh;
            let l = w.layout();
            w.draw(&l);

            // premultiplied BGRA → RGBA поверх фона макета (#0b0c15)
            let n = (sw * sh) as usize;
            let src = std::slice::from_raw_parts(w.bits, n);
            let mut pm = Pixmap::new(sw as u32, sh as u32).unwrap();
            let out = pm.pixels_mut();
            for i in 0..n {
                let p = src[i];
                let (a, r, g, b) = ((p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8, p as u8);
                let bg = [11u8, 12, 21];
                let inv = 255 - a as u32;
                let mix = |c: u8, bgc: u8| (c as u32 + bgc as u32 * inv / 255).min(255) as u8;
                out[i] = tiny_skia::PremultipliedColorU8::from_rgba(mix(r, bg[0]), mix(g, bg[1]), mix(b, bg[2]), 255).unwrap();
            }
            let name = format!("settings_{}_{}.png", scale, if more { "more" } else { "base" });
            let path = std::path::Path::new(&std::env::var("SETTINGS_OUT").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into())).join(name);
            pm.save_png(&path).unwrap();
            println!("wrote {}", path.display());

            w.fonts.free();
            let _ = DeleteObject(dib);
            let _ = DeleteDC(dc);
            let _ = DeleteDC(pdc);
        }
    }
}
