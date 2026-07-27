//! Рендер волнового оверлея «Nocturne» на tiny-skia (честные градиенты,
//! AA, настоящий блюр для теней и свечения).
//!
//! Публичный API (`dims`, `animate`, `hit_test`, `Frame`) не зависит от
//! графики. `render` рисует кадр в tiny-skia Pixmap и отдаёт наружу RGBA
//! (прямая альфа); `ui.rs` конвертирует его в premultiplied BGRA для
//! UpdateLayeredWindow. Раскладка/hit-test — здесь же.

use std::cell::RefCell;
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, PathBuilder, Pixmap, PixmapPaint,
    Point, Shader, SpreadMode, Stroke, Transform,
};

// ── размеры окна и раскладка (логич. px) ──────────────────────────────────
pub const OV_W: i32 = 400;
pub const OV_H: i32 = 150;
pub const N_BARS: usize = 30;

const PILL_X: f32 = 50.0;
const PILL_Y: f32 = 46.0;
const PILL_W: f32 = 300.0;
const PILL_H: f32 = 60.0;
const PILL_R: f32 = 30.0;
const PILL_CY: f32 = PILL_Y + PILL_H / 2.0; // 76

const MIC_CX: f32 = 80.0;
const MIC_CY: f32 = PILL_CY;
const MIC_R: f32 = 24.0;

const WF_X0: f32 = 114.0;
const BAR_W: f32 = 3.5;
const BAR_SLOT: f32 = 7.5;
const WF_HALF: f32 = 22.0;

const CTRL_R: f32 = 17.0;
const CTRL_CY: f32 = 19.0;
const GEAR_CX: f32 = 179.0;
const CLOSE_CX: f32 = 221.0;

// ── палитра ────────────────────────────────────────────────────────────────
type Rgb = (u8, u8, u8);
const VIOLET: Rgb = (139, 124, 247);
const MAGENTA: Rgb = (199, 125, 240);
const PINK: Rgb = (232, 121, 185);
const CYAN: Rgb = (124, 196, 247);
const PALETTE: [Rgb; 4] = [VIOLET, MAGENTA, PINK, CYAN];
const AURORA: [Rgb; 4] = [VIOLET, PINK, CYAN, MAGENTA];
const ICON: Rgb = (207, 200, 245); // #cfc8f5

#[derive(PartialEq, Clone, Copy)]
pub enum Region {
    None,
    Pill,
    Mic,
    Gear,
    Close,
}

pub struct Frame<'a> {
    pub bars: &'a [f32],
    pub hovered: bool,
    pub recording: bool,
    pub t: f32,
}

/// Размер окна оверлея в физических пикселях при данном масштабе.
pub fn dims(s: f32) -> (i32, i32) {
    ((OV_W as f32 * s).round() as i32, (OV_H as f32 * s).round() as i32)
}

// ── анимация высот баров (запись модулируется голосом) ─────────────────────
pub fn animate(bars: &mut [f32], level: f32, t: f32, recording: bool) {
    let n = bars.len();
    if n == 0 {
        return;
    }
    let mid = (n as f32 - 1.0) / 2.0;
    for i in 0..n {
        let center = 1.0 - (i as f32 - mid).abs() / mid;
        let target = if recording {
            let s = i as f32 * 1.7;
            let env = 0.5 + 0.5 * (t * 1.7 + s * 2.3).sin();
            let fast = ((t * 6.0 + s).sin() * (t * 3.3 + s * 0.6).sin()).abs();
            let base = 0.2 + (0.3 + 0.6 * center) * (0.35 + 0.65 * fast) * env;
            (base * (0.5 + 0.8 * level)).clamp(0.06, 1.0)
        } else {
            let w1 = 0.5 + 0.5 * (t * 1.3 - i as f32 * 0.45).sin();
            let w2 = 0.5 + 0.5 * (t * 0.7 + i as f32 * 0.3).sin();
            let wave = w1 * 0.7 + w2 * 0.3;
            0.14 + 0.3 * wave * (0.5 + 0.5 * center)
        };
        let k = if recording { 0.35 } else { 0.1 };
        bars[i] += (target - bars[i]) * k;
    }
}

// ── hit-test ──────────────────────────────────────────────────────────────
pub fn hit_test(x: i32, y: i32, active: bool, s: f32) -> Region {
    let px = (x as f32 + 0.5) / s;
    let py = (y as f32 + 0.5) / s;
    if dist(px, py, MIC_CX, MIC_CY) <= MIC_R {
        return Region::Mic;
    }
    if active {
        if dist(px, py, GEAR_CX, CTRL_CY) <= CTRL_R {
            return Region::Gear;
        }
        if dist(px, py, CLOSE_CX, CTRL_CY) <= CTRL_R {
            return Region::Close;
        }
        return Region::Pill;
    }
    if sd_rrect(px - (PILL_X + PILL_W / 2.0), py - PILL_CY, PILL_W / 2.0, PILL_H / 2.0, PILL_R)
        <= 0.0
    {
        return Region::Pill;
    }
    Region::None
}

thread_local! {
    // кэш статичной тени (пилюля не двигается) — блюрим один раз на масштаб
    static SHADOW: RefCell<Option<(i32, i32, Pixmap)>> = const { RefCell::new(None) };
}

// ── отрисовка кадра ───────────────────────────────────────────────────────
pub fn render(buf: &mut [u8], f: &Frame, s: f32) {
    let (w, h) = dims(s);
    let mut pm = Pixmap::new(w as u32, h as u32).unwrap();

    draw_shadow(&mut pm, s);
    draw_pill(&mut pm, s);
    draw_aurora(&mut pm, s, f.t);
    draw_highlight(&mut pm, s);
    draw_mic(&mut pm, s, f.recording);
    draw_bars(&mut pm, f.bars, s);
    if f.hovered {
        draw_controls(&mut pm, s);
    }

    // premultiplied RGBA (tiny-skia) → прямая RGBA (ui.rs домножит сам)
    let src = pm.data();
    let px = (w * h) as usize;
    for i in 0..px {
        let a = src[i * 4 + 3] as u32;
        if a == 0 {
            buf[i * 4] = 0;
            buf[i * 4 + 1] = 0;
            buf[i * 4 + 2] = 0;
            buf[i * 4 + 3] = 0;
        } else {
            for k in 0..3 {
                buf[i * 4 + k] = ((src[i * 4 + k] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
            buf[i * 4 + 3] = a as u8;
        }
    }
}

fn draw_shadow(pm: &mut Pixmap, s: f32) {
    let (w, h) = (pm.width(), pm.height());
    let cached = SHADOW.with(|c| {
        let b = c.borrow();
        matches!(&*b, Some((cw, ch, _)) if *cw == w as i32 && *ch == h as i32)
    });
    if !cached {
        let mut shadow = Pixmap::new(w, h).unwrap();
        let mut p = Paint::default();
        p.set_color(Color::from_rgba8(0, 0, 0, 200));
        p.anti_alias = true;
        let path = round_rect(sc(PILL_X, s), sc(PILL_Y + 10.0, s), sc(PILL_W, s), sc(PILL_H, s), sc(PILL_R, s));
        shadow.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
        blur(&mut shadow, (9.0 * s) as usize);
        SHADOW.with(|c| *c.borrow_mut() = Some((w as i32, h as i32, shadow)));
    }
    SHADOW.with(|c| {
        if let Some((_, _, shadow)) = &*c.borrow() {
            pm.draw_pixmap(0, 0, shadow.as_ref(), &PixmapPaint { opacity: 0.85, ..Default::default() }, Transform::identity(), None);
        }
    });
}

fn draw_pill(pm: &mut Pixmap, s: f32) {
    let mut p = Paint::default();
    p.shader = lin(
        sc(PILL_X, s), sc(PILL_Y, s), sc(PILL_X, s), sc(PILL_Y + PILL_H, s),
        vec![(0.0, Color::from_rgba8(30, 32, 52, 240)), (1.0, Color::from_rgba8(19, 20, 34, 248))],
    );
    p.anti_alias = true;
    pm.fill_path(&pill_path(s), &p, FillRule::Winding, Transform::identity(), None);
}

fn draw_aurora(pm: &mut Pixmap, s: f32, t: f32) {
    let (w, h) = (pm.width(), pm.height());
    let mut mask = Mask::new(w, h).unwrap();
    mask.fill_path(&pill_path(s), FillRule::Winding, true, Transform::identity());
    // сдвиг фаз перелива по времени (эмуляция wf-aurora)
    let ph = (t * 0.06).rem_euclid(1.0);
    let at = |u: f32| col(aurora_at((u + ph).rem_euclid(1.0)), 0.14);
    let mut p = Paint::default();
    p.shader = lin(
        sc(PILL_X, s), sc(PILL_Y, s), sc(PILL_X + PILL_W, s), sc(PILL_Y, s),
        vec![(0.0, at(0.0)), (0.25, at(0.25)), (0.5, at(0.5)), (0.75, at(0.75)), (1.0, at(1.0))],
    );
    p.anti_alias = true;
    pm.fill_path(&pill_path(s), &p, FillRule::Winding, Transform::identity(), Some(&mask));
}

fn draw_highlight(pm: &mut Pixmap, s: f32) {
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(255, 255, 255, 18));
    p.anti_alias = true;
    let path = round_rect(sc(PILL_X + 12.0, s), sc(PILL_Y + 1.0, s), sc(PILL_W - 24.0, s), sc(1.6, s), sc(0.8, s));
    pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
}

fn draw_mic(pm: &mut Pixmap, s: f32, recording: bool) {
    let (mcx, mcy, mr) = (sc(MIC_CX, s), sc(MIC_CY, s), sc(MIC_R, s));
    let mic = round_rect(mcx - mr, mcy - mr, mr * 2.0, mr * 2.0, mr);
    // тёмный градиентный круг
    let mut mp = Paint::default();
    mp.shader = lin(mcx, mcy - mr, mcx, mcy + mr, vec![(0.0, Color::from_rgba8(35, 37, 60, 255)), (1.0, Color::from_rgba8(25, 27, 45, 255))]);
    mp.anti_alias = true;
    pm.fill_path(&mic, &mp, FillRule::Winding, Transform::identity(), None);

    if recording {
        // мягкое цветное свечение под кнопкой
        {
            let (w, h) = (pm.width(), pm.height());
            let mut glow = Pixmap::new(w, h).unwrap();
            let mut gp = Paint::default();
            gp.set_color(col(MAGENTA, 0.9));
            gp.anti_alias = true;
            glow.fill_path(&round_rect(mcx - mr * 0.8, mcy - mr * 0.4, mr * 1.6, mr * 1.4, mr * 0.6), &gp, FillRule::Winding, Transform::identity(), None);
            blur(&mut glow, (7.0 * s) as usize);
            pm.draw_pixmap(0, 0, glow.as_ref(), &PixmapPaint { opacity: 0.5, ..Default::default() }, Transform::identity(), None);
            // перерисуем тёмный круг поверх свечения (свечение только вокруг)
            pm.fill_path(&mic, &mp, FillRule::Winding, Transform::identity(), None);
        }
        // градиентная заливка круга (135°)
        let mut fp = Paint::default();
        fp.shader = lin(mcx - mr, mcy - mr, mcx + mr, mcy + mr, vec![(0.0, col(VIOLET, 1.0)), (0.45, col(MAGENTA, 1.0)), (1.0, col(PINK, 1.0))]);
        fp.anti_alias = true;
        pm.fill_path(&mic, &fp, FillRule::Winding, Transform::identity(), None);
        // квадрат-стоп (светлый градиент)
        let sh = sc(7.5, s);
        let mut sp = Paint::default();
        sp.shader = lin(mcx - sh, mcy - sh, mcx + sh, mcy + sh, vec![(0.0, col((233, 228, 255), 1.0)), (1.0, col((243, 196, 227), 1.0))]);
        sp.anti_alias = true;
        pm.fill_path(&round_rect(mcx - sh, mcy - sh, sh * 2.0, sh * 2.0, sc(5.0, s)), &sp, FillRule::Winding, Transform::identity(), None);
    } else {
        draw_mic_glyph(pm, mcx, mcy, s);
    }
}

/// Иконка микрофона по геометрии SVG (viewBox 24, масштаб g).
fn draw_mic_glyph(pm: &mut Pixmap, mcx: f32, mcy: f32, s: f32) {
    let g = 0.92;
    let vx = |v: f32| mcx + sc((v - 12.0) * g, s);
    let vy = |v: f32| mcy + sc((v - 12.0) * g, s);
    let mut ip = Paint::default();
    ip.set_color(Color::from_rgba8(207, 200, 245, 242));
    ip.anti_alias = true;
    // тело-капсула
    pm.fill_path(&round_rect(vx(9.0), vy(3.0), sc(6.0 * g, s), sc(12.0 * g, s), sc(3.0 * g, s)), &ip, FillRule::Winding, Transform::identity(), None);
    let mut stroke = Stroke::default();
    stroke.width = sc(1.8 * g, s);
    stroke.line_cap = tiny_skia::LineCap::Round;
    // держатель — нижний полукруг r=6g (огибает тело)
    let (hcx, hcy, r) = (mcx, vy(11.0), sc(6.0 * g, s));
    let k = 0.5523 * r;
    let mut arc = PathBuilder::new();
    arc.move_to(hcx - r, hcy);
    arc.cubic_to(hcx - r, hcy + k, hcx - k, hcy + r, hcx, hcy + r);
    arc.cubic_to(hcx + k, hcy + r, hcx + r, hcy + k, hcx + r, hcy);
    if let Some(path) = arc.finish() {
        pm.stroke_path(&path, &ip, &stroke, Transform::identity(), None);
    }
    // ножка + основание
    let mut st = PathBuilder::new();
    st.move_to(mcx, vy(17.0));
    st.line_to(mcx, vy(21.0));
    st.move_to(vx(9.0), vy(21.0));
    st.line_to(vx(15.0), vy(21.0));
    if let Some(path) = st.finish() {
        pm.stroke_path(&path, &ip, &stroke, Transform::identity(), None);
    }
}

fn draw_bars(pm: &mut Pixmap, bars: &[f32], s: f32) {
    let n = bars.len().max(1);
    let (w, h) = (pm.width(), pm.height());
    let mcy = sc(MIC_CY, s);
    // свечение — во вспомогательном пиксмапе, потом блюр
    let mut glow = Pixmap::new(w, h).unwrap();
    for (i, &lv) in bars.iter().enumerate() {
        let c = palette_at(i as f32 / (n as f32 - 1.0));
        let bh = (lv.clamp(0.0, 1.0) * WF_HALF * s).max(sc(1.4, s));
        let bx = sc(WF_X0 + i as f32 * BAR_SLOT, s);
        let mut p = Paint::default();
        p.set_color(col(c, 0.9));
        p.anti_alias = true;
        glow.fill_path(&round_rect(bx, mcy - bh, sc(BAR_W, s), bh * 2.0, sc(BAR_W / 2.0, s)), &p, FillRule::Winding, Transform::identity(), None);
    }
    blur(&mut glow, (3.0 * s) as usize);
    pm.draw_pixmap(0, 0, glow.as_ref(), &PixmapPaint { opacity: 0.55, ..Default::default() }, Transform::identity(), None);
    // сами бары (вертикальный градиент яркий→приглушённый)
    for (i, &lv) in bars.iter().enumerate() {
        let c = palette_at(i as f32 / (n as f32 - 1.0));
        let bh = (lv.clamp(0.0, 1.0) * WF_HALF * s).max(sc(1.4, s));
        let bx = sc(WF_X0 + i as f32 * BAR_SLOT, s);
        let mut p = Paint::default();
        p.shader = lin(bx, mcy - bh, bx, mcy + bh, vec![(0.0, col(c, 1.0)), (1.0, col(c, 0.6))]);
        p.anti_alias = true;
        pm.fill_path(&round_rect(bx, mcy - bh, sc(BAR_W, s), bh * 2.0, sc(BAR_W / 2.0, s)), &p, FillRule::Winding, Transform::identity(), None);
    }
}

fn draw_controls(pm: &mut Pixmap, s: f32) {
    for cx in [GEAR_CX, CLOSE_CX] {
        let (ccx, ccy, cr) = (sc(cx, s), sc(CTRL_CY, s), sc(CTRL_R, s));
        let mut p = Paint::default();
        p.shader = lin(ccx, ccy - cr, ccx, ccy + cr, vec![(0.0, Color::from_rgba8(35, 37, 60, 242)), (1.0, Color::from_rgba8(22, 24, 40, 242))]);
        p.anti_alias = true;
        pm.fill_path(&round_rect(ccx - cr, ccy - cr, cr * 2.0, cr * 2.0, cr), &p, FillRule::Winding, Transform::identity(), None);
    }
    // иконка ⚙️
    {
        let (gx, gy) = (sc(GEAR_CX, s), sc(CTRL_CY, s));
        let mut ip = Paint::default();
        ip.set_color(col(ICON, 0.95));
        ip.anti_alias = true;
        let mut stroke = Stroke::default();
        stroke.width = sc(2.0, s);
        let mut ring = PathBuilder::new();
        ring.push_circle(gx, gy, sc(4.6, s));
        if let Some(p) = ring.finish() {
            pm.stroke_path(&p, &ip, &stroke, Transform::identity(), None);
        }
        for kk in 0..8 {
            let a = kk as f32 * std::f32::consts::PI / 4.0;
            let mut dot = PathBuilder::new();
            dot.push_circle(gx + a.cos() * sc(7.4, s), gy + a.sin() * sc(7.4, s), sc(1.5, s));
            if let Some(p) = dot.finish() {
                pm.fill_path(&p, &ip, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
    // иконка ✕
    {
        let (xc, yc, r) = (sc(CLOSE_CX, s), sc(CTRL_CY, s), sc(5.0, s));
        let mut ip = Paint::default();
        ip.set_color(col(ICON, 0.95));
        ip.anti_alias = true;
        let mut stroke = Stroke::default();
        stroke.width = sc(1.8, s);
        stroke.line_cap = tiny_skia::LineCap::Round;
        let mut x = PathBuilder::new();
        x.move_to(xc - r, yc - r);
        x.line_to(xc + r, yc + r);
        x.move_to(xc + r, yc - r);
        x.line_to(xc - r, yc + r);
        if let Some(p) = x.finish() {
            pm.stroke_path(&p, &ip, &stroke, Transform::identity(), None);
        }
    }
}

// ── помощники ────────────────────────────────────────────────────────────
fn sc(v: f32, s: f32) -> f32 {
    v * s
}

fn pill_path(s: f32) -> tiny_skia::Path {
    round_rect(sc(PILL_X, s), sc(PILL_Y, s), sc(PILL_W, s), sc(PILL_H, s), sc(PILL_R, s))
}

fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

fn lin(x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Color)>) -> Shader<'static> {
    let gs = stops.into_iter().map(|(p, c)| GradientStop::new(p, c)).collect();
    LinearGradient::new(Point::from_xy(x0, y0), Point::from_xy(x1, y1), gs, SpreadMode::Pad, Transform::identity())
        .unwrap_or_else(|| Shader::SolidColor(Color::from_rgba8(0, 0, 0, 0)))
}

fn col(c: Rgb, a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, (a * 255.0) as u8)
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}

fn palette_at(u: f32) -> Rgb {
    let p = u.clamp(0.0, 1.0) * (PALETTE.len() as f32 - 1.0);
    let i = p.floor() as usize;
    lerp(PALETTE[i], PALETTE[(i + 1).min(PALETTE.len() - 1)], p - i as f32)
}

fn aurora_at(u: f32) -> Rgb {
    let m = AURORA.len();
    let p = u.rem_euclid(1.0) * m as f32;
    let i = p.floor() as usize % m;
    lerp(AURORA[i], AURORA[(i + 1) % m], p - p.floor())
}

fn dist(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
}

/// Знаковое расстояние до скруглённого прямоугольника (для hit-test).
fn sd_rrect(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - (hw - r);
    let qy = py.abs() - (hh - r);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

// быстрый разделимый box-blur (3 прохода ≈ гаусс) по premultiplied RGBA
fn blur(pm: &mut Pixmap, radius: usize) {
    if radius == 0 {
        return;
    }
    let (w, h) = (pm.width() as usize, pm.height() as usize);
    for _ in 0..3 {
        box_pass(pm.data_mut(), w, h, radius, true);
        box_pass(pm.data_mut(), w, h, radius, false);
    }
}

fn box_pass(data: &mut [u8], w: usize, h: usize, r: usize, horiz: bool) {
    let (n, m) = if horiz { (h, w) } else { (w, h) };
    if m == 0 {
        return;
    }
    let win = (2 * r + 1) as u32;
    let mut line = vec![0u8; m * 4];
    for a in 0..n {
        for b in 0..m {
            let idx = if horiz { (a * w + b) * 4 } else { (b * w + a) * 4 };
            line[b * 4..b * 4 + 4].copy_from_slice(&data[idx..idx + 4]);
        }
        for ch in 0..4 {
            let mut sum: u32 = line[ch] as u32 * (r as u32);
            for k in 0..=r.min(m - 1) {
                sum += line[k * 4 + ch] as u32;
            }
            for b in 0..m {
                let idx = if horiz { (a * w + b) * 4 } else { (b * w + a) * 4 };
                data[idx + ch] = (sum / win) as u8;
                let add = line[(b + r + 1).min(m - 1) * 4 + ch] as u32;
                let subv = line[b.saturating_sub(r) * 4 + ch] as u32;
                sum = sum + add - subv;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_bmp(path: &str, w: i32, h: i32, rgb: &[u8]) {
        let row = (w * 3 + 3) & !3;
        let size = 54 + row * h;
        let mut f = vec![0u8; size as usize];
        f[0] = b'B';
        f[1] = b'M';
        f[2..6].copy_from_slice(&(size as u32).to_le_bytes());
        f[10..14].copy_from_slice(&54u32.to_le_bytes());
        f[14..18].copy_from_slice(&40u32.to_le_bytes());
        f[18..22].copy_from_slice(&(w as u32).to_le_bytes());
        f[22..26].copy_from_slice(&(h as u32).to_le_bytes());
        f[26..28].copy_from_slice(&1u16.to_le_bytes());
        f[28..30].copy_from_slice(&24u16.to_le_bytes());
        for y in 0..h {
            let src_y = h - 1 - y;
            for x in 0..w {
                let sidx = ((src_y * w + x) * 3) as usize;
                let d = (54 + y * row + x * 3) as usize;
                f[d] = rgb[sidx + 2];
                f[d + 1] = rgb[sidx + 1];
                f[d + 2] = rgb[sidx];
            }
        }
        std::fs::write(path, f).unwrap();
    }

    #[test]
    #[ignore]
    fn render_preview() {
        let rec = std::env::var("PREVIEW_REC").map(|v| v != "0").unwrap_or(true);
        let mut bars = vec![0.06f32; N_BARS];
        for _ in 0..60 {
            animate(&mut bars, if rec { 0.7 } else { 0.05 }, 1.3, rec);
        }
        let scale = std::env::var("PREVIEW_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let (w, h) = dims(scale);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        render(&mut buf, &Frame { bars: &bars, hovered: true, recording: rec, t: 1.3 }, scale);
        let bg = (24u8, 26u8, 36u8);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for p in 0..(w * h) as usize {
            let a = buf[p * 4 + 3] as f32 / 255.0;
            for k in 0..3 {
                let sc = buf[p * 4 + k] as f32;
                let b = [bg.0, bg.1, bg.2][k] as f32;
                rgb[p * 3 + k] = (sc * a + b * (1.0 - a)) as u8;
            }
        }
        let out = std::env::temp_dir().join(format!("voice_inputter_render_{scale}_{rec}.bmp"));
        write_bmp(out.to_str().unwrap(), w, h, &rgb);
        println!("wrote {}", out.display());
    }
}
