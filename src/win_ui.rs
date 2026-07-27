//! Общая кухня окон в стиле «Nocturne»: палитра, шрифты и вывод текста GDI,
//! поверхность layered-окна и примитивы отрисовки поверх tiny-skia.
//!
//! Тут живёт то, что нужно и окну настроек, и окну загрузки модели: формы
//! рисуем tiny-skia в пиксмап, текст — GDI ClearType поверх того же DIB,
//! затем `present` отдаёт всё окну через `UpdateLayeredWindow`.

#![allow(non_snake_case)]

use crate::paint::{blur, col, round_rect, Rgb};
use std::cell::RefCell;
use std::ffi::c_void;
use tiny_skia::{
    FillRule, FilterQuality, LineCap, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, POINT, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, CreateFontIndirectW, DeleteObject, EnumFontFamiliesExW, GetDC,
    GetTextExtentPoint32W, GetTextMetricsW, IntersectClipRect, ReleaseDC, SelectObject,
    SetTextCharacterExtra, SetTextColor, TextOutW, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BLENDFUNCTION, BI_RGB, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DIB_RGB_COLORS,
    HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, TEXTMETRICW,
};
use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

// ── палитра макета ────────────────────────────────────────────────────────
pub const TXT: Rgb = (233, 233, 237); // #e9e9ed
pub const TXT_DIM: Rgb = (165, 167, 189); // #a5a7bd
pub const TXT_MUTE: Rgb = (110, 112, 135); // #6e7087
pub const TXT_GREY: Rgb = (139, 141, 163); // #8b8da3
pub const VIOLET: Rgb = (139, 124, 247); // #8b7cf7
pub const MAGENTA: Rgb = (199, 125, 240); // #c77df0
pub const PINK: Rgb = (232, 121, 185); // #e879b9
pub const ICON: Rgb = (207, 200, 245); // #cfc8f5
pub const WHITE: Rgb = (255, 255, 255);
/// Фон полей и «утопленных» плашек: rgba(11,12,21,.6).
pub const SUNKEN: Rgb = (11, 12, 21);

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── пространство панели: логические px макета → физические ────────────────
#[derive(Clone, Copy)]
pub struct Sp {
    pub s: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Sp {
    pub fn x(&self, v: f32) -> f32 {
        self.ox + v * self.s
    }
    pub fn y(&self, v: f32) -> f32 {
        self.oy + v * self.s
    }
    pub fn l(&self, v: f32) -> f32 {
        v * self.s
    }
    pub fn rr(&self, x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
        round_rect(self.x(x), self.y(y), self.l(w), self.l(h), self.l(r))
    }
}

// ── примитивы отрисовки ───────────────────────────────────────────────────
pub fn fill(pm: &mut Pixmap, path: &tiny_skia::Path, shader: tiny_skia::Shader, mask: Option<&tiny_skia::Mask>) {
    let mut p = Paint::default();
    p.shader = shader;
    p.anti_alias = true;
    pm.fill_path(path, &p, FillRule::Winding, Transform::identity(), mask);
}

pub fn fill_c(pm: &mut Pixmap, path: &tiny_skia::Path, c: Rgb, a: f32, mask: Option<&tiny_skia::Mask>) {
    fill(pm, path, tiny_skia::Shader::SolidColor(col(c, a)), mask);
}

pub fn stroke_c(pm: &mut Pixmap, path: &tiny_skia::Path, c: Rgb, a: f32, w: f32, mask: Option<&tiny_skia::Mask>) {
    let mut p = Paint::default();
    p.set_color(col(c, a));
    p.anti_alias = true;
    let mut st = Stroke::default();
    st.width = w;
    st.line_cap = LineCap::Round;
    pm.stroke_path(path, &p, &st, Transform::identity(), mask);
}

/// Мягкая тень под элементом (аналог CSS box-shadow с blur/spread/offset).
/// Блюрим в четверть разрешения — вчетверо дешевле, разницы не видно.
pub fn shadow(pm: &mut Pixmap, sp: Sp, x: f32, y: f32, w: f32, h: f32, r: f32, c: Rgb, a: f32, blur_px: f32, dy: f32, spread: f32) {
    if a <= 0.004 {
        return;
    }
    let q = 0.25f32; // масштаб вспомогательного пиксмапа
    let m = (blur_px + spread.abs() + 4.0) * sp.s; // поле вокруг под размытие
    let (px0, py0) = (sp.x(x) - m, sp.y(y + dy) - m);
    let (pw, ph) = ((sp.l(w) + m * 2.0).ceil(), (sp.l(h) + m * 2.0).ceil());
    let (qw, qh) = (((pw * q) as u32).max(4), ((ph * q) as u32).max(4));
    let Some(mut tmp) = Pixmap::new(qw, qh) else { return };
    let rr = round_rect(
        (m - spread * sp.s) * q,
        (m - spread * sp.s) * q,
        (sp.l(w) + spread * sp.s * 2.0) * q,
        (sp.l(h) + spread * sp.s * 2.0) * q,
        (sp.l(r) + spread * sp.s) * q,
    );
    fill_c(&mut tmp, &rr, c, a, None);
    // CSS blur-radius B ≈ гаусс sigma B/2; тройной box ≈ окно 1.88*sigma
    let br = ((blur_px * sp.s * q * 0.94 - 0.5) as usize).max(1);
    blur(&mut tmp, br);
    pm.draw_pixmap(
        px0.round() as i32,
        py0.round() as i32,
        tmp.as_ref(),
        &PixmapPaint { quality: FilterQuality::Bilinear, ..Default::default() },
        Transform::from_scale(1.0 / q, 1.0 / q),
        None,
    );
}

/// Шеврон «m6 9 6 6 6-6» с поворотом (turn: 0 — вниз, -0.25 — вправо).
pub fn chevron(pm: &mut Pixmap, cx: f32, cy: f32, size: f32, c: Rgb, a: f32, turn: f32) {
    let g = size / 24.0;
    let ang = turn * std::f32::consts::TAU;
    let (si, co) = (ang.sin(), ang.cos());
    let pt = |x: f32, y: f32| {
        let (dx, dy) = ((x - 12.0) * g, (y - 12.0) * g);
        (cx + dx * co - dy * si, cy + dx * si + dy * co)
    };
    let mut pb = PathBuilder::new();
    let a0 = pt(6.0, 9.5);
    let a1 = pt(12.0, 15.5);
    let a2 = pt(18.0, 9.5);
    pb.move_to(a0.0, a0.1);
    pb.line_to(a1.0, a1.1);
    pb.line_to(a2.0, a2.1);
    if let Some(p) = pb.finish() {
        stroke_c(pm, &p, c, a, 2.0 * g, None);
    }
}

/// Иконка микрофона из макета (SVG viewBox 24, обводка 2.2).
pub fn mic_glyph(pm: &mut Pixmap, cx: f32, cy: f32, size: f32, c: Rgb, a: f32) {
    let g = size / 24.0;
    let vx = |v: f32| cx + (v - 12.0) * g;
    let vy = |v: f32| cy + (v - 12.0) * g;
    // капсула 9,3 6×12 r3 — обводкой
    let body = round_rect(vx(9.0), vy(3.0), 6.0 * g, 12.0 * g, 3.0 * g);
    stroke_c(pm, &body, c, a, 2.2 * g, None);
    // дуга M6 11 a6 6 0 0 0 12 0 — нижняя половина окружности r=6
    let (hx, hy, r) = (vx(12.0), vy(11.0), 6.0 * g);
    let k = 0.5523 * r;
    let mut arc = PathBuilder::new();
    arc.move_to(hx - r, hy);
    arc.cubic_to(hx - r, hy + k, hx - k, hy + r, hx, hy + r);
    arc.cubic_to(hx + k, hy + r, hx + r, hy + k, hx + r, hy);
    if let Some(p) = arc.finish() {
        stroke_c(pm, &p, c, a, 2.2 * g, None);
    }
    // ножка M12 17v4
    let mut st = PathBuilder::new();
    st.move_to(vx(12.0), vy(17.0));
    st.line_to(vx(12.0), vy(21.0));
    if let Some(p) = st.finish() {
        stroke_c(pm, &p, c, a, 2.2 * g, None);
    }
}

/// Ограничивает вывод GDI прямоугольником в координатах панели.
pub unsafe fn clip(dc: HDC, sp: Sp, x: f32, y: f32, w: f32, h: f32) {
    IntersectClipRect(
        dc,
        sp.x(x).floor() as i32,
        sp.y(y).floor() as i32,
        sp.x(x + w).ceil() as i32,
        sp.y(y + h).ceil() as i32,
    );
}

// ── шрифты ────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum F {
    Title = 0,
    Label,
    Hint,
    Input,
    Seg,
    Btn,
    BtnBold,
    Body,
}
const N_F: usize = 8;

pub struct Fonts {
    h: [HFONT; N_F],
    /// высота строки (tmHeight) в физических px
    th: [i32; N_F],
}

impl Fonts {
    pub fn new(dc: HDC, s: f32) -> Fonts {
        // 14px/500 заголовок, 12px/500 подписи, 11px хинты, 13.5px поля,
        // 12.5px/500 сегменты и хоткей, 13px/500 «Отмена», 13px/600 «Сохранить»
        let spec = [
            (14.0, 500), (12.0, 500), (11.0, 400), (13.5, 400),
            (12.5, 500), (13.0, 500), (13.0, 600), (13.0, 400),
        ];
        let mut h = [HFONT::default(); N_F];
        let mut th = [0i32; N_F];
        for i in 0..N_F {
            h[i] = make_font(spec[i].0 * s, spec[i].1);
            unsafe {
                let old = SelectObject(dc, HGDIOBJ(h[i].0));
                let mut tm = TEXTMETRICW::default();
                let _ = GetTextMetricsW(dc, &mut tm);
                th[i] = tm.tmHeight;
                SelectObject(dc, old);
            }
        }
        Fonts { h, th }
    }

    pub fn free(&mut self) {
        for f in self.h.iter_mut() {
            unsafe {
                let _ = DeleteObject(*f);
            }
            *f = HFONT::default();
        }
    }

    /// Ширина строки в физических px.
    pub fn width(&self, dc: HDC, f: F, t: &[u16]) -> f32 {
        if t.is_empty() {
            return 0.0;
        }
        unsafe {
            let old = SelectObject(dc, HGDIOBJ(self.h[f as usize].0));
            let mut sz = SIZE::default();
            let _ = GetTextExtentPoint32W(dc, t, &mut sz);
            SelectObject(dc, old);
            sz.cx as f32
        }
    }

    /// Рисует строку с центром по вертикали в `cy`; `spacing` — разрядка.
    pub unsafe fn text_out(&self, dc: HDC, f: F, t: &[u16], x: f32, cy: f32, c: Rgb, spacing: f32) {
        if t.is_empty() {
            return;
        }
        let old = SelectObject(dc, HGDIOBJ(self.h[f as usize].0));
        SetTextColor(dc, COLORREF((c.2 as u32) << 16 | (c.1 as u32) << 8 | c.0 as u32));
        SetTextCharacterExtra(dc, spacing.round() as i32);
        let y = cy - self.th[f as usize] as f32 / 2.0;
        let _ = TextOutW(dc, x.round() as i32, y.round() as i32, t);
        SetTextCharacterExtra(dc, 0);
        SelectObject(dc, old);
    }

    /// Обрезает строку многоточием под ширину `max` (физ. px).
    pub fn ellipsize(&self, dc: HDC, f: F, t: &[u16], max: f32, spacing: f32) -> Vec<u16> {
        let extra = |n: usize| spacing * n.saturating_sub(1) as f32;
        if self.width(dc, f, t) + extra(t.len()) <= max || t.len() < 2 {
            return t.to_vec();
        }
        let dots: Vec<u16> = "…".encode_utf16().collect();
        let dw = self.width(dc, f, &dots);
        let mut n = t.len();
        while n > 1 {
            n -= 1;
            if self.width(dc, f, &t[..n]) + extra(n) + dw <= max {
                break;
            }
        }
        let mut out = t[..n].to_vec();
        out.extend_from_slice(&dots);
        out
    }
}

fn make_font(px: f32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -(px.round().max(1.0) as i32),
        lfWeight: weight,
        lfCharSet: DEFAULT_CHARSET,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    for (i, c) in ui_face().encode_utf16().enumerate().take(31) {
        lf.lfFaceName[i] = c;
    }
    unsafe { CreateFontIndirectW(&lf) }
}

thread_local! {
    static FACE: RefCell<Option<&'static str>> = const { RefCell::new(None) };
}

/// Шрифт макета — Inter; если он не установлен, системный Segoe UI.
fn ui_face() -> &'static str {
    FACE.with(|f| {
        if let Some(v) = *f.borrow() {
            return v;
        }
        let v = if font_installed("Inter") { "Inter" } else { "Segoe UI" };
        *f.borrow_mut() = Some(v);
        v
    })
}

fn font_installed(name: &str) -> bool {
    unsafe extern "system" fn cb(_lf: *const LOGFONTW, _tm: *const TEXTMETRICW, _ty: u32, lp: LPARAM) -> i32 {
        *(lp.0 as *mut bool) = true;
        0
    }
    unsafe {
        let dc = GetDC(HWND::default());
        let mut lf = LOGFONTW {
            lfCharSet: DEFAULT_CHARSET,
            ..Default::default()
        };
        for (i, c) in name.encode_utf16().enumerate().take(31) {
            lf.lfFaceName[i] = c;
        }
        let mut found = false;
        EnumFontFamiliesExW(dc, &lf, Some(cb), LPARAM(&mut found as *mut bool as isize), 0);
        ReleaseDC(HWND::default(), dc);
        found
    }
}

// ── поверхность layered-окна ──────────────────────────────────────────────
/// Создаёт DIB нужного размера, делает его текущим в DC и удаляет прежний
/// (удалять до SelectObject нельзя — GDI не удаляет выбранный объект).
pub fn make_dib(dc: HDC, old: HBITMAP, w: i32, h: i32) -> (HBITMAP, *mut u32) {
    unsafe {
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w.max(1),
            biHeight: -h.max(1), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
            .expect("CreateDIBSection");
        SelectObject(dc, HGDIOBJ(dib.0));
        if !old.0.is_null() {
            let _ = DeleteObject(old);
        }
        (dib, bits as *mut u32)
    }
}

/// Переносит пиксмап в DIB, даёт нарисовать текст поверх (GDI) и отдаёт кадр
/// окну. Альфу после текста восстанавливаем: GDI обнуляет её в своих пикселях.
pub unsafe fn present(hwnd: HWND, dc: HDC, bits: *mut u32, w: i32, h: i32, pm: &Pixmap, draw_text: impl FnOnce()) {
    if bits.is_null() || w <= 0 || h <= 0 {
        return;
    }
    let n = (w * h) as usize;
    let src = pm.data();
    let dst = std::slice::from_raw_parts_mut(bits, n);
    for i in 0..n {
        let (r, g, b, a) = (
            src[i * 4] as u32,
            src[i * 4 + 1] as u32,
            src[i * 4 + 2] as u32,
            src[i * 4 + 3] as u32,
        );
        dst[i] = (a << 24) | (r << 16) | (g << 8) | b;
    }

    draw_text();

    let dst = std::slice::from_raw_parts_mut(bits, n);
    for i in 0..n {
        dst[i] = (dst[i] & 0x00FF_FFFF) | ((src[i * 4 + 3] as u32) << 24);
    }

    let screen = GetDC(HWND::default());
    let mut size = SIZE { cx: w, cy: h };
    let mut src_pt = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let _ = UpdateLayeredWindow(
        hwnd,
        screen,
        None,
        Some(&mut size),
        dc,
        Some(&mut src_pt),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
    ReleaseDC(HWND::default(), screen);
}
