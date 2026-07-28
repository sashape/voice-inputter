//! Окно первого запуска: предлагает скачать модель распознавания и показывает
//! ход загрузки. Рисуется тем же способом, что и настройки (tiny-skia + GDI,
//! layered-окно), поэтому выглядит как часть того же интерфейса.

#![allow(non_snake_case)]

use crate::model::{self, Status};
use crate::paint::{col, lerp, lerp_f, lin};
use crate::win_ui::{
    fill, fill_c, make_dib, mic_glyph, present, stroke_c, wide, Fonts, Sp, F, MAGENTA, PINK, SUNKEN,
    TXT, TXT_DIM, TXT_MUTE, VIOLET, WHITE,
};
use std::cell::RefCell;
use std::ffi::c_void;
use std::time::{Duration, Instant};
use tiny_skia::{Color, Pixmap};

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW, MonitorFromPoint, ReleaseDC,
    SetBkMode, HBITMAP, HDC, MONITORINFO, MONITOR_DEFAULTTONEAREST, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE};
use windows::Win32::UI::WindowsAndMessaging::*;

// ── раскладка (логические px) ─────────────────────────────────────────────
const W: f32 = 420.0;
const PANEL_R: f32 = 20.0;
const PAD: f32 = 22.0;
const TITLE_H: f32 = 58.0;
const BAR_H: f32 = 8.0;
const BTN_H: f32 = 38.0;
const H: f32 = 214.0; // высота панели

const TIMER: usize = 11;
const WM_MOUSELEAVE: u32 = 0x02A3;

/// Кнопки окна: главная (Скачать/Повторить/Готово), второстепенная и ✕.
const B_MAIN: u8 = 0;
const B_ALT: u8 = 1;
const B_CLOSE: u8 = 2;

struct Dlg {
    hwnd: HWND,
    scale: f32,
    dc: HDC,
    dib: HBITMAP,
    bits: *mut u32,
    sw: i32,
    sh: i32,
    fonts: Fonts,
    hot: Option<u8>,
    press: Option<u8>,
    hovt: [f32; 3],
    t: f32,
    done_at: Option<Instant>,
    last: Instant,
    track_leave: bool,
}

thread_local! {
    static DLG: RefCell<Option<Box<Dlg>>> = const { RefCell::new(None) };
}

/// Доступ к состоянию окна (см. пояснение в settings.rs): try_borrow, чтобы
/// повторный вход из синхронного сообщения не приводил к панике.
fn with<R>(f: impl FnOnce(&mut Dlg) -> R) -> Option<R> {
    DLG.with(|d| d.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(|w| f(w))))
}

/// Открывает окно (или поднимает уже открытое).
pub fn open() {
    if let Some(h) = DLG.with(|d| d.borrow().as_ref().map(|w| w.hwnd)) {
        unsafe {
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    unsafe {
        let hmodule = GetModuleHandleW(None).expect("GetModuleHandleW");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hmodule.0);
        register_class(hinstance);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("VoiceInputterModel"),
            w!("Voice Inputter"),
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
        .expect("model window");

        let dpi = GetDpiForWindow(hwnd);
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let screen = GetDC(HWND::default());
        let dc = CreateCompatibleDC(screen);
        ReleaseDC(HWND::default(), screen);
        SetBkMode(dc, TRANSPARENT);

        let dlg = Box::new(Dlg {
            hwnd,
            scale,
            dc,
            dib: HBITMAP::default(),
            bits: std::ptr::null_mut(),
            sw: 0,
            sh: 0,
            fonts: Fonts::new(dc, scale),
            hot: None,
            press: None,
            hovt: [0.0; 3],
            t: 0.0,
            done_at: None,
            last: Instant::now(),
            track_leave: false,
        });
        DLG.with(|d| *d.borrow_mut() = Some(dlg));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let wa = work_area(pt);
        let (sw, sh) = ((W * scale).round() as i32, (H * scale).round() as i32);
        let x = wa.left + (wa.right - wa.left - sw) / 2;
        let y = wa.top + (wa.bottom - wa.top - sh) / 2;
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, sw, sh, SWP_NOACTIVATE);

        redraw();
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, TIMER, 16, None);
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

unsafe fn register_class(hinstance: windows::Win32::Foundation::HINSTANCE) {
    thread_local! {
        static DONE: RefCell<bool> = const { RefCell::new(false) };
    }
    if DONE.with(|d| *d.borrow()) {
        return;
    }
    let c = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: w!("VoiceInputterModel"),
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&c);
    DONE.with(|d| *d.borrow_mut() = true);
}

// ── тексты состояний ──────────────────────────────────────────────────────
/// Подписи под текущее состояние: заголовок, пояснение, главная кнопка.
fn labels(st: &Status) -> (String, String, Option<&'static str>) {
    match st {
        Status::Idle | Status::Cancelled => (
            "Нужна модель распознавания".into(),
            "Один раз скачаем ~23 МБ — дальше всё работает без интернета".into(),
            Some("Скачать"),
        ),
        Status::Downloading { got, total } => (
            "Загружаю модель…".into(),
            model::human(*got, *total),
            None,
        ),
        Status::Extracting => ("Распаковываю…".into(), "Почти готово".into(), None),
        Status::Done => (
            "Готово".into(),
            "Модель на месте — можно диктовать".into(),
            None,
        ),
        Status::Failed(e) => (
            "Не удалось скачать".into(),
            e.clone(),
            Some("Повторить"),
        ),
    }
}

/// Прямоугольники кнопок (главная, второстепенная) в логических px.
fn buttons(st: &Status, dc: HDC, fonts: &Fonts, scale: f32) -> Vec<(u8, f32, f32, f32, f32, String)> {
    let (_, _, main) = labels(st);
    let alt = match st {
        Status::Downloading { .. } | Status::Extracting => "Отмена",
        Status::Done => "Закрыть",
        _ => "Не сейчас",
    };
    let y = H - PAD - BTN_H;
    let mut out = Vec::new();
    let w_of = |s: &str, f: F, pad: f32| {
        let t: Vec<u16> = s.encode_utf16().collect();
        (fonts.width(dc, f, &t) / scale + pad).round()
    };
    let mut x = W - PAD;
    if let Some(m) = main {
        let bw = w_of(m, F::BtnBold, 44.0);
        x -= bw;
        out.push((B_MAIN, x, y, bw, BTN_H, m.to_string()));
        x -= 10.0;
    }
    let aw = w_of(alt, F::Btn, 36.0);
    out.push((B_ALT, x - aw, y, aw, BTN_H, alt.to_string()));
    out
}

// ── перерисовка ───────────────────────────────────────────────────────────
fn redraw() {
    with(|d| {
        let s = d.scale;
        let (sw, sh) = ((W * s).round() as i32, (H * s).round() as i32);
        if sw != d.sw || sh != d.sh {
            let (dib, bits) = make_dib(d.dc, d.dib, sw, sh);
            d.dib = dib;
            d.bits = bits;
            d.sw = sw;
            d.sh = sh;
            unsafe {
                let _ = SetWindowPos(d.hwnd, HWND::default(), 0, 0, sw, sh,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        d.draw();
    });
}

impl Dlg {
    fn draw(&mut self) {
        let s = self.scale;
        let sp = Sp { s, ox: 0.0, oy: 0.0 };
        let Some(mut pm) = Pixmap::new(self.sw.max(1) as u32, self.sh.max(1) as u32) else { return };
        let st = model::status();

        // корпус — как у настроек
        let panel = sp.rr(0.0, 0.0, W, H, PANEL_R);
        fill(&mut pm, &panel,
            lin(sp.x(0.0), sp.y(0.0), sp.x(0.0), sp.y(H),
                vec![(0.0, Color::from_rgba8(30, 32, 52, 247)), (1.0, Color::from_rgba8(19, 20, 34, 250))]),
            None);
        {
            let mut p = tiny_skia::Paint::default();
            p.shader = lin(sp.x(0.0), sp.y(0.0), sp.x(0.0), sp.y(H * 0.35),
                vec![(0.0, col(WHITE, 0.07)), (1.0, col(WHITE, 0.0))]);
            p.anti_alias = true;
            let mut stroke = tiny_skia::Stroke::default();
            stroke.width = s.max(1.0);
            pm.stroke_path(&panel, &p, &stroke, tiny_skia::Transform::identity(), None);
        }

        // шапка: кружок с микрофоном и ✕
        let badge = sp.rr(PAD, 17.0, 26.0, 26.0, 13.0);
        fill(&mut pm, &badge,
            lin(sp.x(PAD), sp.y(17.0), sp.x(PAD + 26.0), sp.y(43.0),
                vec![(0.0, col(VIOLET, 1.0)), (1.0, col(PINK, 1.0))]),
            None);
        mic_glyph(&mut pm, sp.x(PAD + 13.0), sp.y(30.0), sp.l(13.0), WHITE, 1.0);
        {
            let (cx, cy) = (sp.x(W - 18.0 - 14.0), sp.y(30.0));
            let hv = self.hovt[B_CLOSE as usize];
            if hv > 0.01 {
                let path = sp.rr(W - 18.0 - 28.0, 16.0, 28.0, 28.0, 14.0);
                fill_c(&mut pm, &path, WHITE, 0.07 * hv, None);
            }
            let r = sp.l(15.0) / 24.0 * 6.0;
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(cx - r, cy - r);
            pb.line_to(cx + r, cy + r);
            pb.move_to(cx + r, cy - r);
            pb.line_to(cx - r, cy + r);
            if let Some(p) = pb.finish() {
                stroke_c(&mut pm, &p, lerp((139, 141, 163), TXT, hv), 1.0, sp.l(15.0) / 24.0 * 1.9, None);
            }
        }

        // полоса прогресса
        let bar_y = TITLE_H + 62.0;
        let track = sp.rr(PAD, bar_y, W - PAD * 2.0, BAR_H, BAR_H / 2.0);
        fill_c(&mut pm, &track, SUNKEN, 0.6, None);
        let bw = W - PAD * 2.0;
        let grad = |a: f32, b: f32| {
            lin(sp.x(PAD), 0.0, sp.x(PAD + bw), 0.0,
                vec![(0.0, col(VIOLET, a)), (0.5, col(MAGENTA, a)), (1.0, col(PINK, b))])
        };
        match &st {
            Status::Downloading { got, total } => {
                let p = if *total > 0 { *got as f32 / *total as f32 } else { 0.0 };
                let fw = (bw * p.clamp(0.0, 1.0)).max(BAR_H);
                let path = sp.rr(PAD, bar_y, fw, BAR_H, BAR_H / 2.0);
                fill(&mut pm, &path, grad(1.0, 1.0), None);
            }
            Status::Extracting => {
                // неопределённый прогресс — бегущая полоска
                let seg = bw * 0.35;
                let x = PAD - seg + (bw + seg) * (self.t * 0.6).fract();
                let x0 = x.max(PAD);
                let x1 = (x + seg).min(PAD + bw);
                if x1 > x0 {
                    let path = sp.rr(x0, bar_y, x1 - x0, BAR_H, BAR_H / 2.0);
                    fill(&mut pm, &path, grad(0.9, 0.9), None);
                }
            }
            Status::Done => {
                let path = sp.rr(PAD, bar_y, bw, BAR_H, BAR_H / 2.0);
                fill(&mut pm, &path, grad(1.0, 1.0), None);
            }
            _ => {}
        }

        // кнопки
        let btns = buttons(&st, self.dc, &self.fonts, s);
        for (id, x, y, w, h, _) in &btns {
            let hv = self.hovt[*id as usize];
            if *id == B_MAIN {
                let b = lerp_f(1.0, 1.08, hv);
                let br = |c: (u8, u8, u8)| {
                    ((c.0 as f32 * b).min(255.0) as u8, (c.1 as f32 * b).min(255.0) as u8, (c.2 as f32 * b).min(255.0) as u8)
                };
                let path = sp.rr(*x, *y, *w, *h, 12.0);
                fill(&mut pm, &path,
                    lin(sp.x(*x), sp.y(*y), sp.x(*x + *w), sp.y(*y + *h),
                        vec![(0.0, col(br(VIOLET), 1.0)), (0.5, col(br(MAGENTA), 1.0)), (1.0, col(br(PINK), 1.0))]),
                    None);
            } else {
                let path = sp.rr(*x + 0.5, *y + 0.5, *w - 1.0, *h - 1.0, 12.0);
                stroke_c(&mut pm, &path, WHITE, lerp_f(0.1, 0.2, hv), sp.l(1.0).max(1.0), None);
            }
        }

        let (title, sub, _) = labels(&st);
        let fonts = &self.fonts;
        let dc = self.dc;
        let hovt = self.hovt;
        unsafe {
            present(self.hwnd, dc, self.bits, self.sw, self.sh, &pm, || {
                let head: Vec<u16> = "Voice Inputter".encode_utf16().collect();
                fonts.text_out(dc, F::Title, &head, sp.x(58.0), sp.y(30.0), TXT, sp.l(0.14));

                let t: Vec<u16> = title.encode_utf16().collect();
                fonts.text_out(dc, F::Body, &t, sp.x(PAD), sp.y(TITLE_H + 16.0), TXT, 0.0);

                let s2: Vec<u16> = sub.encode_utf16().collect();
                let s2 = fonts.ellipsize(dc, F::Hint, &s2, sp.l(W - PAD * 2.0), 0.0);
                fonts.text_out(dc, F::Hint, &s2, sp.x(PAD), sp.y(TITLE_H + 38.0), TXT_MUTE, 0.0);

                for (id, x, y, w, h, label) in &btns {
                    let t: Vec<u16> = label.encode_utf16().collect();
                    let (f, c) = if *id == B_MAIN {
                        (F::BtnBold, WHITE)
                    } else {
                        (F::Btn, lerp(TXT_DIM, TXT, hovt[*id as usize]))
                    };
                    let tw = fonts.width(dc, f, &t);
                    fonts.text_out(dc, f, &t, sp.x(x + w / 2.0) - tw / 2.0, sp.y(y + h / 2.0), c, 0.0);
                }
            });
        }
    }
}

// ── события ───────────────────────────────────────────────────────────────
fn hit(d: &Dlg, lx: f32, ly: f32) -> Option<u8> {
    if lx >= W - 18.0 - 28.0 && lx <= W - 18.0 && ly >= 16.0 && ly <= 44.0 {
        return Some(B_CLOSE);
    }
    let st = model::status();
    buttons(&st, d.dc, &d.fonts, d.scale)
        .into_iter()
        .find(|(_, x, y, w, h, _)| lx >= *x && lx <= x + w && ly >= *y && ly <= y + h)
        .map(|(id, ..)| id)
}

/// Действие кнопки; true — закрыть окно.
fn activate(id: u8) -> bool {
    match id {
        B_MAIN => {
            model::start();
            false
        }
        _ => {
            if model::is_running() {
                model::cancel();
            }
            true
        }
    }
}

fn tick() {
    let (need, close) = with(|d| {
        let now = Instant::now();
        let dt = (now - d.last).as_secs_f32().clamp(0.001, 0.1);
        d.last = now;
        d.t += dt;
        let st = model::status();
        let mut moving = matches!(st, Status::Downloading { .. } | Status::Extracting);
        for i in 0..3 {
            let target = (d.hot == Some(i as u8)) as u8 as f32;
            if (d.hovt[i] - target).abs() > 1e-3 {
                let step = dt / 0.2;
                d.hovt[i] += (target - d.hovt[i]).signum() * step.min((target - d.hovt[i]).abs());
                moving = true;
            }
        }
        // успех: показываем «Готово» пару секунд и закрываемся
        if st == Status::Done {
            let at = *d.done_at.get_or_insert(now);
            moving = true;
            if now.duration_since(at) > Duration::from_millis(1600) {
                return (false, true);
            }
        }
        (moving, false)
    })
    .unwrap_or((false, false));

    if close {
        with(|d| unsafe {
            let _ = DestroyWindow(d.hwnd);
        });
        return;
    }
    if need {
        redraw();
    }
}

/// Оконная процедура: паника внутри неё обрывала бы процесс (unwind
/// через FFI → abort), поэтому ловим её и продолжаем работать.
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wndproc_inner(hwnd, msg, wp, lp)
    }));
    match r {
        Ok(v) => v,
        Err(_) => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn wndproc_inner(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
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
                let r = with(|d| {
                    let lx = (lo(lp) - rc.left) as f32 / d.scale;
                    let ly = (hi(lp) - rc.top) as f32 / d.scale;
                    if hit(d, lx, ly).is_some() {
                        HTCLIENT as i32
                    } else if ly < TITLE_H {
                        HTCAPTION as i32
                    } else {
                        HTCLIENT as i32
                    }
                })
                .unwrap_or(HTCLIENT as i32);
                LRESULT(r as isize)
            }
            WM_MOUSEMOVE => {
                with(|d| {
                    let (lx, ly) = (lo(lp) as f32 / d.scale, hi(lp) as f32 / d.scale);
                    let h = hit(d, lx, ly);
                    if h != d.hot {
                        d.hot = h;
                    }
                    if !d.track_leave {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        d.track_leave = true;
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                with(|d| {
                    d.hot = None;
                    d.track_leave = false;
                });
                LRESULT(0)
            }
            WM_SETCURSOR => {
                let over = with(|d| {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let mut rc = RECT::default();
                    let _ = GetWindowRect(hwnd, &mut rc);
                    hit(d, (pt.x - rc.left) as f32 / d.scale, (pt.y - rc.top) as f32 / d.scale)
                })
                .flatten();
                if over.is_some() {
                    SetCursor(LoadCursorW(None, IDC_HAND).unwrap_or_default());
                    LRESULT(1)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_LBUTTONDOWN => {
                with(|d| {
                    let (lx, ly) = (lo(lp) as f32 / d.scale, hi(lp) as f32 / d.scale);
                    d.press = hit(d, lx, ly);
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let act = with(|d| {
                    let (lx, ly) = (lo(lp) as f32 / d.scale, hi(lp) as f32 / d.scale);
                    let h = hit(d, lx, ly);
                    let p = d.press.take();
                    if h.is_some() && h == p {
                        h
                    } else {
                        None
                    }
                })
                .flatten();
                if let Some(id) = act {
                    if activate(id) {
                        let _ = DestroyWindow(hwnd);
                    } else {
                        redraw();
                    }
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                // Esc — свернуть окно (загрузка продолжается в фоне)
                if wp.0 as u32 == 0x1B {
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                KillTimer(hwnd, TIMER).ok();
                DLG.with(|dd| {
                    if let Some(mut d) = dd.borrow_mut().take() {
                        d.fonts.free();
                        if !d.dib.0.is_null() {
                            let _ = DeleteObject(d.dib);
                        }
                        let _ = DeleteDC(d.dc);
                    }
                });
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

// заглушка, чтобы не тянуть c_void в сигнатуры выше
#[allow(dead_code)]
fn _unused(_: *mut c_void, _: Vec<u16>) {
    let _ = wide("");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win_ui::make_dib;

    /// Рендер всех состояний окна в PNG:
    /// `MODEL_OUT=... cargo test model_preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn model_preview() {
        let scale = std::env::var("MODEL_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let states = [
            Status::Idle,
            Status::Downloading { got: 12_400_000, total: 23_000_000 },
            Status::Extracting,
            Status::Done,
            Status::Failed("не подключиться к github.com".into()),
        ];
        unsafe {
            let screen = GetDC(HWND::default());
            let dc = CreateCompatibleDC(screen);
            ReleaseDC(HWND::default(), screen);
            SetBkMode(dc, TRANSPARENT);
            let (sw, sh) = ((W * scale).round() as i32, (H * scale).round() as i32);
            let (dib, bits) = make_dib(dc, HBITMAP::default(), sw, sh);

            let mut d = Box::new(Dlg {
                hwnd: HWND::default(),
                scale,
                dc,
                dib,
                bits,
                sw,
                sh,
                fonts: Fonts::new(dc, scale),
                hot: None,
                press: None,
                hovt: [0.0; 3],
                t: 0.6,
                done_at: None,
                last: Instant::now(),
                track_leave: false,
            });

            let n = states.len() as i32;
            let mut out = tiny_skia::Pixmap::new(sw as u32, (sh * n) as u32).unwrap();
            for (i, st) in states.iter().enumerate() {
                crate::model::set_for_test(st.clone());
                d.draw();
                let src = std::slice::from_raw_parts(d.bits, (sw * sh) as usize);
                let px = out.pixels_mut();
                for y in 0..sh {
                    for x in 0..sw {
                        let p = src[(y * sw + x) as usize];
                        let (a, r, g, b) = ((p >> 24) as u8, (p >> 16) as u8, (p >> 8) as u8, p as u8);
                        let bg = [11u8, 12, 21];
                        let inv = 255 - a as u32;
                        let mix = |c: u8, bgc: u8| (c as u32 + bgc as u32 * inv / 255).min(255) as u8;
                        let d2 = ((i as i32 * sh + y) * sw + x) as usize;
                        px[d2] = tiny_skia::PremultipliedColorU8::from_rgba(
                            mix(r, bg[0]), mix(g, bg[1]), mix(b, bg[2]), 255,
                        )
                        .unwrap();
                    }
                }
            }
            let path = std::path::Path::new(
                &std::env::var("MODEL_OUT").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into()),
            )
            .join(format!("model_states_{scale}.png"));
            out.save_png(&path).unwrap();
            println!("wrote {}", path.display());

            d.fonts.free();
            let _ = DeleteObject(dib);
            let _ = DeleteDC(dc);
        }
    }
}
