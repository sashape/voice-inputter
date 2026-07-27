//! Рендер волнового оверлея «Nocturne» на tiny-skia (честные градиенты,
//! AA, настоящий блюр для теней и свечения).
//!
//! Публичный API (`dims`, `animate`, `hit_test`, `Frame`) не зависит от
//! графики. `render` рисует кадр в tiny-skia Pixmap и отдаёт наружу RGBA
//! (прямая альфа); `ui.rs` конвертирует его в premultiplied BGRA для
//! UpdateLayeredWindow. Раскладка/hit-test — здесь же.

use crate::paint::{blur, col, lerp, lerp_f, lin, radial, round_rect, sc, sd_rrect, Rgb};
use std::cell::RefCell;
use tiny_skia::{
    Color, FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform,
};

pub use crate::paint::smooth_k;

// ── размеры окна и раскладка (логич. px) ──────────────────────────────────
pub const OV_W: i32 = 300;
pub const OV_H: i32 = 150;
pub const N_BARS: usize = 16;

const PILL_X: f32 = 50.0;
const PILL_Y: f32 = 46.0;
const PILL_W: f32 = 200.0; // короче: 16 баров
const PILL_H: f32 = 60.0;
const PILL_R: f32 = 30.0;
const PILL_CY: f32 = PILL_Y + PILL_H / 2.0; // 76

const MIC_CX: f32 = 80.0;
const MIC_CY: f32 = PILL_CY;
const MIC_R: f32 = 24.0;

const WF_X0: f32 = 114.0;
const BAR_W: f32 = 3.5;
const BAR_SLOT: f32 = 7.5; // как в исходном дизайне (по ширине)
const WF_HALF: f32 = MIC_R * 0.85; // потолок баров = 85% высоты кнопки

const CTRL_R: f32 = 17.0;
const CTRL_CY: f32 = 19.0;
const GEAR_CX: f32 = 129.0; // над центром укороченной пилюли (X=150)
const CLOSE_CX: f32 = 171.0;

// ── палитра ────────────────────────────────────────────────────────────────
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
    pub rec_t: f32,        // плавный переход покой↔запись (0..1)
    pub hover: [f32; 3],   // подсветка кнопок [mic, gear, close] (0..1)
    pub show_t: f32,       // появление/исчезание (0 скрыто → 1 показано)
    pub t: f32,
}

/// Размер окна оверлея в физических пикселях при данном масштабе.
pub fn dims(s: f32) -> (i32, i32) {
    ((OV_W as f32 * s).round() as i32, (OV_H as f32 * s).round() as i32)
}

// ── анимация высот баров (запись модулируется голосом) ─────────────────────
/// Обновляет высоты баров. `dt` — время с прошлого кадра (сек): сглаживание
/// привязано к нему, поэтому частота кадров (30/60 fps) не влияет на скорость.
pub fn animate(bars: &mut [f32], level: f32, t: f32, dt: f32, recording: bool) {
    let n = bars.len();
    if n == 0 {
        return;
    }
    let mid = (n as f32 - 1.0) / 2.0;
    for i in 0..n {
        let center = 1.0 - (i as f32 - mid).abs() / mid;
        let target = if recording {
            // Дёрганый «спектр» как раньше, но его включает ГОЛОС: нет голоса — замирает.
            let s = i as f32 * 1.7;
            let env = 0.5 + 0.5 * (t * 1.7 + s * 2.3).sin();
            let fast = ((t * 6.0 + s).sin() * (t * 3.3 + s * 0.6).sin()).abs();
            let burst = (0.35 + 0.65 * center) * (0.35 + 0.65 * fast) * env; // 0..~1 дёрганье
            // в тишине — спокойный низкий пол (лёгкое дыхание, без дёрганья)
            let calm = 0.09 + 0.025 * (0.5 + 0.5 * (t * 1.1 + s * 0.5).sin());
            (calm + level * 1.55 * burst).clamp(0.06, 1.0)
        } else {
            // две бегущие волны по ширине (~2 гребня на 16 баров), разная скорость — переливаются
            let w1 = 0.5 + 0.5 * (t * 1.3 - i as f32 * 0.84).sin();
            let w2 = 0.5 + 0.5 * (t * 0.9 - i as f32 * 0.84).sin();
            let wave = w1 * 0.6 + w2 * 0.4;
            // полный размах от минимума к максимуму (ещё −10% к высоте)
            0.126 + 0.378 * wave * (0.82 + 0.18 * center)
        };
        // запись — отзывчивее (за голосом), покой — плавнее
        let tau = if recording { 0.05 } else { 0.313 };
        bars[i] += (target - bars[i]) * smooth_k(dt, tau);
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
    // кэш свечения под кнопкой: форма не зависит от rec_t (меняется лишь opacity),
    // поэтому блюрим один раз на масштаб и блитим с прозрачностью 0.5*rec_t
    static MIC_GLOW: RefCell<Option<(i32, i32, Pixmap)>> = const { RefCell::new(None) };
}

// ── отрисовка кадра ───────────────────────────────────────────────────────
pub fn render(buf: &mut [u8], f: &Frame, s: f32) {
    let (w, h) = dims(s);
    let mut pm = Pixmap::new(w as u32, h as u32).unwrap();

    draw_shadow(&mut pm, s);
    draw_pill(&mut pm, s);
    draw_inner_glow(&mut pm, s, f.rec_t);
    draw_aurora(&mut pm, s, f.t);
    draw_mic(&mut pm, s, f.rec_t, f.hover[0]);
    draw_bars(&mut pm, f.bars, s);
    if f.hovered {
        draw_controls(&mut pm, s, f.hover[1], f.hover[2]);
    }

    // появление/исчезание: масштаб (0.92→1), сдвиг вверх (14px→0) и затухание всего кадра
    let display = if f.show_t < 0.999 {
        let show = f.show_t.clamp(0.0, 1.0);
        let mut out = Pixmap::new(w as u32, h as u32).unwrap();
        let scf = lerp_f(0.92, 1.0, show);
        let dy = (1.0 - show) * 14.0 * s;
        let (cx, cy) = (w as f32 * 0.5, h as f32 * 0.5);
        let tr = Transform::from_translate(cx, cy + dy)
            .pre_scale(scf, scf)
            .pre_translate(-cx, -cy);
        out.draw_pixmap(0, 0, pm.as_ref(), &PixmapPaint { opacity: show, ..Default::default() }, tr, None);
        out
    } else {
        pm
    };

    // premultiplied RGBA (tiny-skia) → прямая RGBA (ui.rs домножит сам)
    let src = display.data();
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

fn draw_mic(pm: &mut Pixmap, s: f32, rec_t: f32, hover: f32) {
    let (mcx, mcy, mr) = (sc(MIC_CX, s), sc(MIC_CY, s), sc(MIC_R, s));
    let mic = round_rect(mcx - mr, mcy - mr, mr * 2.0, mr * 2.0, mr);
    // тёмный градиентный круг (основа)
    let mut mp = Paint::default();
    mp.shader = lin(mcx, mcy - mr, mcx, mcy + mr, vec![(0.0, Color::from_rgba8(35, 37, 60, 255)), (1.0, Color::from_rgba8(25, 27, 45, 255))]);
    mp.anti_alias = true;
    pm.fill_path(&mic, &mp, FillRule::Winding, Transform::identity(), None);

    // мягкое цветное свечение под кнопкой — проявляется вместе с записью.
    // Форма постоянна → блюрим один раз на масштаб и кэшируем (см. MIC_GLOW).
    if rec_t > 0.01 {
        let (w, h) = (pm.width(), pm.height());
        let cached = MIC_GLOW.with(|c| matches!(&*c.borrow(), Some((cw, ch, _)) if *cw == w as i32 && *ch == h as i32));
        if !cached {
            let mut glow = Pixmap::new(w, h).unwrap();
            let mut gp = Paint::default();
            gp.set_color(col(MAGENTA, 0.9));
            gp.anti_alias = true;
            glow.fill_path(&round_rect(mcx - mr * 0.8, mcy - mr * 0.4, mr * 1.6, mr * 1.4, mr * 0.6), &gp, FillRule::Winding, Transform::identity(), None);
            blur(&mut glow, (7.0 * s) as usize);
            MIC_GLOW.with(|c| *c.borrow_mut() = Some((w as i32, h as i32, glow)));
        }
        MIC_GLOW.with(|c| {
            if let Some((_, _, glow)) = &*c.borrow() {
                pm.draw_pixmap(0, 0, glow.as_ref(), &PixmapPaint { opacity: 0.5 * rec_t, ..Default::default() }, Transform::identity(), None);
            }
        });
        // перерисуем тёмный круг поверх свечения (свечение только вокруг)
        pm.fill_path(&mic, &mp, FillRule::Winding, Transform::identity(), None);
        // градиентная заливка круга (135°) — по мере rec_t
        let mut fp = Paint::default();
        fp.shader = lin(mcx - mr, mcy - mr, mcx + mr, mcy + mr, vec![(0.0, col(VIOLET, rec_t)), (0.45, col(MAGENTA, rec_t)), (1.0, col(PINK, rec_t))]);
        fp.anti_alias = true;
        pm.fill_path(&mic, &fp, FillRule::Winding, Transform::identity(), None);
    }

    // подсветка при наведении — мягко высветляем круг
    if hover > 0.01 {
        let mut hp = Paint::default();
        hp.set_color(col((255, 255, 255), 0.12 * hover));
        hp.anti_alias = true;
        pm.fill_path(&mic, &hp, FillRule::Winding, Transform::identity(), None);
    }

    // иконка микрофона — уезжает (scale 1→0.5, rotate 0→-20°), гаснет
    if rec_t < 0.99 {
        let tr = rot_scale_at(lerp_f(0.0, -20.0, rec_t), lerp_f(1.0, 0.5, rec_t), mcx, mcy);
        draw_mic_glyph(pm, mcx, mcy, s, (1.0 - rec_t) * 0.95, tr);
    }
    // квадрат-стоп — приезжает (scale 0.4→1, rotate 40°→0), проявляется
    if rec_t > 0.01 {
        let tr = rot_scale_at(lerp_f(40.0, 0.0, rec_t), lerp_f(0.4, 1.0, rec_t), mcx, mcy);
        let sh = sc(7.5, s);
        let mut sp = Paint::default();
        sp.shader = lin(mcx - sh, mcy - sh, mcx + sh, mcy + sh, vec![(0.0, col((233, 228, 255), rec_t)), (1.0, col((243, 196, 227), rec_t))]);
        sp.anti_alias = true;
        pm.fill_path(&round_rect(mcx - sh, mcy - sh, sh * 2.0, sh * 2.0, sc(5.0, s)), &sp, FillRule::Winding, tr, None);
    }
}

/// Иконка микрофона по геометрии SVG (viewBox 24, масштаб g). `tr` — трансформ анимации.
fn draw_mic_glyph(pm: &mut Pixmap, mcx: f32, mcy: f32, s: f32, alpha: f32, tr: Transform) {
    let g = 0.92;
    let vx = |v: f32| mcx + sc((v - 12.0) * g, s);
    let vy = |v: f32| mcy + sc((v - 12.0) * g, s);
    let mut ip = Paint::default();
    ip.set_color(col(ICON, alpha));
    ip.anti_alias = true;
    // тело-капсула
    pm.fill_path(&round_rect(vx(9.0), vy(3.0), sc(6.0 * g, s), sc(12.0 * g, s), sc(3.0 * g, s)), &ip, FillRule::Winding, tr, None);
    let mut stroke = Stroke::default();
    stroke.width = sc(1.8 * g, s);
    stroke.line_cap = LineCap::Round;
    // держатель — нижний полукруг r=6g (огибает тело)
    let (hcx, hcy, r) = (mcx, vy(11.0), sc(6.0 * g, s));
    let k = 0.5523 * r;
    let mut arc = PathBuilder::new();
    arc.move_to(hcx - r, hcy);
    arc.cubic_to(hcx - r, hcy + k, hcx - k, hcy + r, hcx, hcy + r);
    arc.cubic_to(hcx + k, hcy + r, hcx + r, hcy + k, hcx + r, hcy);
    if let Some(path) = arc.finish() {
        pm.stroke_path(&path, &ip, &stroke, tr, None);
    }
    // ножка + основание
    let mut st = PathBuilder::new();
    st.move_to(mcx, vy(17.0));
    st.line_to(mcx, vy(21.0));
    st.move_to(vx(9.0), vy(21.0));
    st.line_to(vx(15.0), vy(21.0));
    if let Some(path) = st.finish() {
        pm.stroke_path(&path, &ip, &stroke, tr, None);
    }
}

/// Внутреннее свечение пилюли — два радиальных пятна, проявляются с записью.
fn draw_inner_glow(pm: &mut Pixmap, s: f32, rec_t: f32) {
    if rec_t <= 0.01 {
        return;
    }
    let mask = pill_mask(pm, s);
    let (px, py) = (sc(PILL_X, s), sc(PILL_Y, s));
    let (pw, ph) = (sc(PILL_W, s), sc(PILL_H, s));
    // фиолетовое пятно снизу-слева
    let mut p1 = Paint::default();
    p1.anti_alias = true;
    p1.shader = radial(px + pw * 0.30, py + ph * 1.10, pw * 0.6, vec![(0.0, col(VIOLET, 0.30 * rec_t)), (0.65, col(VIOLET, 0.0))]);
    pm.fill_path(&pill_path(s), &p1, FillRule::Winding, Transform::identity(), Some(&mask));
    // розовое пятно сверху-справа
    let mut p2 = Paint::default();
    p2.anti_alias = true;
    p2.shader = radial(px + pw * 0.72, py - ph * 0.10, pw * 0.6, vec![(0.0, col(PINK, 0.24 * rec_t)), (0.65, col(PINK, 0.0))]);
    pm.fill_path(&pill_path(s), &p2, FillRule::Winding, Transform::identity(), Some(&mask));
}

fn draw_bars(pm: &mut Pixmap, bars: &[f32], s: f32) {
    let n = bars.len().max(1);
    let (w, h) = (pm.width(), pm.height());
    let mcy = sc(MIC_CY, s);
    // бары могут «перерастать» пилюлю — клипаем их по её форме
    let mask = pill_mask(pm, s);
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
    pm.draw_pixmap(0, 0, glow.as_ref(), &PixmapPaint { opacity: 0.55, ..Default::default() }, Transform::identity(), Some(&mask));
    // сами бары (вертикальный градиент яркий→приглушённый)
    for (i, &lv) in bars.iter().enumerate() {
        let c = palette_at(i as f32 / (n as f32 - 1.0));
        let bh = (lv.clamp(0.0, 1.0) * WF_HALF * s).max(sc(1.4, s));
        let bx = sc(WF_X0 + i as f32 * BAR_SLOT, s);
        let mut p = Paint::default();
        p.shader = lin(bx, mcy - bh, bx, mcy + bh, vec![(0.0, col(c, 1.0)), (1.0, col(c, 0.6))]);
        p.anti_alias = true;
        pm.fill_path(&round_rect(bx, mcy - bh, sc(BAR_W, s), bh * 2.0, sc(BAR_W / 2.0, s)), &p, FillRule::Winding, Transform::identity(), Some(&mask));
    }
}

fn draw_controls(pm: &mut Pixmap, s: f32, hover_gear: f32, hover_close: f32) {
    for (cx, hv) in [(GEAR_CX, hover_gear), (CLOSE_CX, hover_close)] {
        let (ccx, ccy, cr) = (sc(cx, s), sc(CTRL_CY, s), sc(CTRL_R, s));
        let circle = round_rect(ccx - cr, ccy - cr, cr * 2.0, cr * 2.0, cr);
        let mut p = Paint::default();
        p.shader = lin(ccx, ccy - cr, ccx, ccy + cr, vec![(0.0, Color::from_rgba8(35, 37, 60, 242)), (1.0, Color::from_rgba8(22, 24, 40, 242))]);
        p.anti_alias = true;
        pm.fill_path(&circle, &p, FillRule::Winding, Transform::identity(), None);
        // подсветка при наведении
        if hv > 0.01 {
            let mut hp = Paint::default();
            hp.set_color(col((255, 255, 255), 0.14 * hv));
            hp.anti_alias = true;
            pm.fill_path(&circle, &hp, FillRule::Winding, Transform::identity(), None);
        }
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
fn pill_path(s: f32) -> tiny_skia::Path {
    round_rect(sc(PILL_X, s), sc(PILL_Y, s), sc(PILL_W, s), sc(PILL_H, s), sc(PILL_R, s))
}

/// Маска по форме пилюли (для клиппинга свечения внутрь).
fn pill_mask(pm: &Pixmap, s: f32) -> Mask {
    let mut mask = Mask::new(pm.width(), pm.height()).unwrap();
    mask.fill_path(&pill_path(s), FillRule::Winding, true, Transform::identity());
    mask
}

/// Поворот на `deg` + равномерный масштаб `sl` вокруг точки (cx, cy).
fn rot_scale_at(deg: f32, sl: f32, cx: f32, cy: f32) -> Transform {
    let r = deg.to_radians();
    let (c, si) = (r.cos() * sl, r.sin() * sl);
    // [c -si; si c] с последующим сдвигом, чтобы (cx,cy) остался на месте
    let (sx, kx, ky, sy) = (c, -si, si, c);
    let tx = cx - (sx * cx + kx * cy);
    let ty = cy - (ky * cx + sy * cy);
    Transform::from_row(sx, ky, kx, sy, tx, ty)
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
        // PREVIEW_REC: 0..1 — доля перехода (0 покой, 1 запись, дробь — середина)
        let rec_t = std::env::var("PREVIEW_REC").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0).clamp(0.0, 1.0);
        let rec = rec_t > 0.5;
        let tt = std::env::var("PREVIEW_T").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.3);
        let lvl = std::env::var("PREVIEW_LEVEL").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(if rec { 0.7 } else { 0.05 });
        let mut bars = vec![0.06f32; N_BARS];
        for _ in 0..60 {
            animate(&mut bars, lvl, tt, 0.016, rec);
        }
        let scale = std::env::var("PREVIEW_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let (w, h) = dims(scale);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let hover = std::env::var("PREVIEW_HOVER").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
        let show_t = std::env::var("PREVIEW_SHOW").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        render(&mut buf, &Frame { bars: &bars, hovered: true, rec_t, hover: [hover, 0.0, 0.0], show_t, t: tt }, scale);
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

    #[test]
    #[ignore]
    fn bench_render() {
        // замер стоимости одного кадра render() — самый тяжёлый путь: rec_t=1
        let scale = std::env::var("PREVIEW_SCALE").ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(1.0);
        let mut bars = vec![0.06f32; N_BARS];
        for _ in 0..60 {
            animate(&mut bars, 0.7, 1.3, 0.016, true);
        }
        let (w, h) = dims(scale);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        // прогрев (первый кадр строит thread_local кэш тени)
        for _ in 0..5 {
            render(&mut buf, &Frame { bars: &bars, hovered: true, rec_t: 1.0, hover: [0.0; 3], show_t: 1.0, t: 1.3 }, scale);
        }
        let n = 300;
        let start = std::time::Instant::now();
        for i in 0..n {
            let t = i as f32 * 0.033;
            render(&mut buf, &Frame { bars: &bars, hovered: true, rec_t: 1.0, hover: [0.0; 3], show_t: 1.0, t }, scale);
        }
        let per = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
        println!("scale={scale} {w}x{h}: {per:.3} ms/frame  (budget: 16.7ms@60fps, 33ms@30fps)");
    }
}
