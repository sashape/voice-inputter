//! Софт-рендер волнового оверлея «Nocturne» (стеклянная пилюля +
//! разноцветные бары + градиентный микрофон + aurora-перелив) с
//! аналитическим антиалиасингом (SDF).
//!
//! Модуль не зависит от WinAPI: заполняет RGBA-буфер (прямая альфа), а `ui.rs`
//! конвертирует его в premultiplied BGRA и толкает в UpdateLayeredWindow.
//! Раскладка и hit-test — здесь же, чтобы отрисовка и попадания мыши совпадали.

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
const STOP_HALF: f32 = 7.5; // квадрат-стоп 15px

const WF_X0: f32 = 114.0;
const BAR_W: f32 = 3.5;
const BAR_SLOT: f32 = 7.5; // ширина 3.5 + зазор 4
const WF_HALF: f32 = 22.0; // половина высоты контейнера волны (44px)

const CTRL_R: f32 = 17.0;
const CTRL_CY: f32 = 19.0;
const GEAR_CX: f32 = 179.0;
const CLOSE_CX: f32 = 221.0;

// ── палитра ────────────────────────────────────────────────────────────────
type Rgb = (u8, u8, u8);
// живая мультипалитра баров: violet → magenta → pink → cyan
const VIOLET: Rgb = (139, 124, 247); // #8b7cf7
const MAGENTA: Rgb = (199, 125, 240); // #c77df0
const PINK: Rgb = (232, 121, 185); // #e879b9
const CYAN: Rgb = (124, 196, 247); // #7cc4f7
const PALETTE: [Rgb; 4] = [VIOLET, MAGENTA, PINK, CYAN];
// aurora-перелив внутри пилюли (циклический)
const AURORA: [Rgb; 4] = [VIOLET, PINK, CYAN, MAGENTA];

const PILL_TOP: Rgb = (30, 32, 52); // rgba(30,32,52,.92)
const PILL_BOT: Rgb = (19, 20, 34); // rgba(19,20,34,.96)
const MIC_TOP: Rgb = (35, 37, 60); // #23253c
const MIC_BOT: Rgb = (25, 27, 45); // #191b2d
const ICON: Rgb = (207, 200, 245); // #cfc8f5
const STOP_A: Rgb = (233, 228, 255); // #e9e4ff
const STOP_B: Rgb = (243, 196, 227); // #f3c4e3

#[derive(PartialEq, Clone, Copy)]
pub enum Region {
    None,
    Pill,
    Mic,
    Gear,
    Close,
}

pub struct Frame<'a> {
    pub bars: &'a [f32], // высоты 0..1
    pub hovered: bool,
    pub recording: bool,
    pub t: f32, // время (сек) для анимаций перелива/оттенка
}

/// Размер окна оверлея в физических пикселях при данном масштабе.
pub fn dims(s: f32) -> (i32, i32) {
    ((OV_W as f32 * s).round() as i32, (OV_H as f32 * s).round() as i32)
}

// ── анимация высот баров (из макета, для записи модулируется голосом) ───────
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
            // две перекрещённые бегущие волны — спокойный перелив
            let w1 = 0.5 + 0.5 * (t * 1.3 - i as f32 * 0.45).sin();
            let w2 = 0.5 + 0.5 * (t * 0.7 + i as f32 * 0.3).sin();
            let wave = w1 * 0.7 + w2 * 0.3;
            0.14 + 0.3 * wave * (0.5 + 0.5 * center)
        };
        let k = if recording { 0.35 } else { 0.1 };
        bars[i] += (target - bars[i]) * k;
    }
}

// ── hit-test (физ. координаты окна; s — масштаб) ──────────────────────────
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
        return Region::Pill; // всё окно интерактивно, пока активны
    }
    if sd_rrect(px - (PILL_X + PILL_W / 2.0), py - PILL_CY, PILL_W / 2.0, PILL_H / 2.0, PILL_R)
        <= 0.0
    {
        return Region::Pill;
    }
    Region::None
}

// ── отрисовка кадра ───────────────────────────────────────────────────────
pub fn render(buf: &mut [u8], f: &Frame, s: f32) {
    for b in buf.iter_mut() {
        *b = 0;
    }
    let (w, h) = dims(s);

    // мягкая тёмная тень под пилюлей
    soft_shadow(buf, w, h, s);

    // тело пилюли — «стекло» (полупрозрачный вертикальный градиент)
    fill_rrect(buf, w, h, PILL_X * s, PILL_Y * s, PILL_W * s, PILL_H * s, PILL_R * s, 0.94, |_, py| {
        lerp(PILL_TOP, PILL_BOT, ((py / s - PILL_Y) / PILL_H).clamp(0.0, 1.0))
    });

    // aurora-перелив внутри пилюли
    draw_aurora(buf, w, h, s, f.t);

    // верхний внутренний блик
    fill_rrect(
        buf, w, h,
        (PILL_X + 10.0) * s, (PILL_Y + 1.0) * s, (PILL_W - 20.0) * s, 1.4 * s,
        1.0, 0.07, |_, _| (255, 255, 255),
    );

    // микрофон-кнопка
    draw_mic(buf, w, h, s, f.recording, f.t);

    // бары — каждый со своим цветом из палитры + вертикальный градиент + свечение
    let n = f.bars.len().max(1);
    let hue = 0.06 * (f.t * 0.7).sin(); // лёгкий дрейф оттенка (аналог wf-hue)
    for (i, &lv) in f.bars.iter().enumerate() {
        let u = (i as f32 / (n as f32 - 1.0) + hue).clamp(0.0, 1.0);
        let c = palette_at(u);
        let cdim = mul(c, 0.55);
        let bx = (WF_X0 + i as f32 * BAR_SLOT) * s;
        let bh = (lv.clamp(0.0, 1.0) * WF_HALF).max(1.4) * s;
        let top = MIC_CY * s - bh;
        let bot = MIC_CY * s + bh;
        // свечение (мягкий более широкий полупрозрачный бар)
        fill_rrect(buf, w, h, bx - 1.6 * s, top - 1.6 * s, (BAR_W + 3.2) * s, (bot - top) + 3.2 * s, (BAR_W / 2.0 + 1.6) * s, 0.20, move |_, _| c);
        // сам бар: градиент сверху (яркий) вниз (приглушённый)
        fill_rrect(buf, w, h, bx, top, BAR_W * s, bot - top, (BAR_W / 2.0) * s, 1.0, move |_, py| {
            lerp(c, cdim, ((py - top) / (bot - top).max(1.0)).clamp(0.0, 1.0))
        });
    }

    // ховер-контролы
    if f.hovered {
        draw_ctrl_button(buf, w, h, GEAR_CX, s);
        draw_gear_icon(buf, w, h, GEAR_CX, CTRL_CY, s);
        draw_ctrl_button(buf, w, h, CLOSE_CX, s);
        draw_close_icon(buf, w, h, CLOSE_CX, CTRL_CY, s);
    }
}

/// Мягкая тёмная тень под пилюлей (смещена вниз).
fn soft_shadow(buf: &mut [u8], w: i32, h: i32, s: f32) {
    let cx = (PILL_X + PILL_W / 2.0) * s;
    let cy = (PILL_CY + 10.0) * s; // смещение вниз
    let (hw, hh, r) = (PILL_W / 2.0 * s, PILL_H / 2.0 * s, PILL_R * s);
    let blur = 18.0 * s;
    let x_min = (cx - hw - blur).floor() as i32;
    let x_max = (cx + hw + blur).ceil() as i32;
    let y_min = (cy - hh - blur).floor() as i32;
    let y_max = (cy + hh + blur).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let d = sd_rrect(x as f32 + 0.5 - cx, y as f32 + 0.5 - cy, hw, hh, r);
            if d > 0.0 {
                let a = (1.0 - d / blur).clamp(0.0, 1.0);
                put(buf, w, h, x, y, (0, 0, 0), a * a * 0.5);
            }
        }
    }
}

/// Анимированный мультицветный перелив внутри пилюли (низкая альфа).
fn draw_aurora(buf: &mut [u8], w: i32, h: i32, s: f32, t: f32) {
    let cx = (PILL_X + PILL_W / 2.0) * s;
    let (hw, hh, r) = (PILL_W / 2.0 * s, PILL_H / 2.0 * s, PILL_R * s);
    let x0 = (PILL_X * s) as i32;
    let x1 = ((PILL_X + PILL_W) * s) as i32;
    let y0 = (PILL_Y * s) as i32;
    let y1 = ((PILL_Y + PILL_H) * s) as i32;
    let shift = t * 0.06;
    for y in y0..y1 {
        for x in x0..x1 {
            let cov = (0.5 - sd_rrect(x as f32 + 0.5 - cx, y as f32 + 0.5 - PILL_CY * s, hw, hh, r))
                .clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let u = ((x - x0) as f32 / (x1 - x0) as f32 + shift).rem_euclid(1.0);
            put(buf, w, h, x, y, aurora_at(u), 0.14 * cov);
        }
    }
}

fn draw_mic(buf: &mut [u8], w: i32, h: i32, s: f32, recording: bool, _t: f32) {
    let (mcx, mcy, mr) = (MIC_CX * s, MIC_CY * s, MIC_R * s);
    // тёмный градиентный круг
    fill_circle_fn(buf, w, h, mcx, mcy, mr, 1.0, |_, py| {
        let t01 = ((py - (mcy - mr)) / (2.0 * mr)).clamp(0.0, 1.0);
        (lerp(MIC_TOP, MIC_BOT, t01), 1.0)
    });
    if recording {
        // градиентная заливка (135°): violet → magenta → pink
        fill_circle_fn(buf, w, h, mcx, mcy, mr, 1.0, |px, py| {
            let u = ((px - (mcx - mr)) + (py - (mcy - mr))) / (4.0 * mr);
            (mic_grad(u.clamp(0.0, 1.0)), 1.0)
        });
        // квадрат-стоп (светлый градиент) + свечение
        draw_glow(buf, w, h, (mcx, mcy), (STOP_HALF + 6.0) * s, STOP_HALF * s, PINK, 0.55);
        fill_rrect(
            buf, w, h,
            mcx - STOP_HALF * s, mcy - STOP_HALF * s, STOP_HALF * 2.0 * s, STOP_HALF * 2.0 * s,
            5.0 * s, 1.0, |px, _| {
                let u = ((px - (mcx - STOP_HALF * s)) / (STOP_HALF * 2.0 * s)).clamp(0.0, 1.0);
                lerp(STOP_A, STOP_B, u)
            },
        );
    } else {
        draw_mic_glyph(buf, w, h, mcx, mcy, s, ICON);
    }
}

/// Иконка микрофона (тело-капсула + U-держатель + ножка + основание).
fn draw_mic_glyph(buf: &mut [u8], w: i32, h: i32, mcx: f32, mcy: f32, s: f32, col: Rgb) {
    let a = 0.95;
    fill_rrect(buf, w, h, mcx - 4.2 * s, mcy - 10.5 * s, 8.4 * s, 13.0 * s, 4.2 * s, a, |_, _| col);
    stroke_arc(
        buf, w, h, mcx, mcy - 2.5 * s, 7.0 * s,
        0.45, std::f32::consts::PI - 0.45, 1.7 * s, col, a,
    );
    seg(buf, w, h, mcx, mcy + 4.5 * s, mcx, mcy + 12.0 * s, 1.7 * s, col, a);
    seg(buf, w, h, mcx - 5.5 * s, mcy + 12.0 * s, mcx + 5.5 * s, mcy + 12.0 * s, 1.7 * s, col, a);
}

fn draw_ctrl_button(buf: &mut [u8], w: i32, h: i32, cx: f32, s: f32) {
    let (ccx, ccy, cr) = (cx * s, CTRL_CY * s, CTRL_R * s);
    fill_circle_fn(buf, w, h, ccx, ccy, cr, 1.0, |_, py| {
        let t = ((py - (ccy - cr)) / (2.0 * cr)).clamp(0.0, 1.0);
        (lerp((35, 37, 60), (22, 24, 40), t), 0.95)
    });
    stroke_circle(buf, w, h, ccx, ccy, cr, 1.0 * s, (255, 255, 255), 0.07);
}

fn draw_gear_icon(buf: &mut [u8], w: i32, h: i32, cx: f32, cy: f32, s: f32) {
    let (gx, gy) = (cx * s, cy * s);
    stroke_circle(buf, w, h, gx, gy, 5.0 * s, 2.0 * s, ICON, 0.95);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::PI / 4.0;
        let tx = gx + a.cos() * 7.6 * s;
        let ty = gy + a.sin() * 7.6 * s;
        fill_circle_fn(buf, w, h, tx, ty, 1.5 * s, 1.0, |_, _| (ICON, 0.95));
    }
}

fn draw_close_icon(buf: &mut [u8], w: i32, h: i32, cx: f32, cy: f32, s: f32) {
    let (xc, yc, r) = (cx * s, cy * s, 5.0 * s);
    seg(buf, w, h, xc - r, yc - r, xc + r, yc + r, 1.8 * s, ICON, 0.95);
    seg(buf, w, h, xc + r, yc - r, xc - r, yc + r, 1.8 * s, ICON, 0.95);
}

// ── палитры/цвет ────────────────────────────────────────────────────────────
fn palette_at(u: f32) -> Rgb {
    let u = u.clamp(0.0, 1.0);
    let p = u * (PALETTE.len() as f32 - 1.0);
    let i = p.floor() as usize;
    let j = (i + 1).min(PALETTE.len() - 1);
    lerp(PALETTE[i], PALETTE[j], p - i as f32)
}

fn aurora_at(u: f32) -> Rgb {
    let u = u.rem_euclid(1.0);
    let m = AURORA.len();
    let p = u * m as f32;
    let i = p.floor() as usize % m;
    let j = (i + 1) % m;
    lerp(AURORA[i], AURORA[j], p - p.floor())
}

fn mic_grad(u: f32) -> Rgb {
    if u < 0.45 {
        lerp(VIOLET, MAGENTA, u / 0.45)
    } else {
        lerp(MAGENTA, PINK, (u - 0.45) / 0.55)
    }
}

fn mul(c: Rgb, f: f32) -> Rgb {
    (
        (c.0 as f32 * f) as u8,
        (c.1 as f32 * f) as u8,
        (c.2 as f32 * f) as u8,
    )
}

// ── низкоуровневые примитивы с AA (SDF + альфа-композитинг) ────────────────
fn put(buf: &mut [u8], w: i32, h: i32, x: i32, y: i32, c: Rgb, a: f32) {
    if x < 0 || y < 0 || x >= w || y >= h || a <= 0.0 {
        return;
    }
    let idx = ((y * w + x) * 4) as usize;
    let sa = a.clamp(0.0, 1.0);
    let da = buf[idx + 3] as f32 / 255.0;
    let na = sa + da * (1.0 - sa);
    if na <= 0.0 {
        return;
    }
    for k in 0..3 {
        let sc = [c.0, c.1, c.2][k] as f32;
        let dc = buf[idx + k] as f32;
        buf[idx + k] = ((sc * sa + dc * da * (1.0 - sa)) / na).round().clamp(0.0, 255.0) as u8;
    }
    buf[idx + 3] = (na * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn fill_rrect<F: Fn(f32, f32) -> Rgb>(
    buf: &mut [u8],
    w: i32,
    h: i32,
    x0: f32,
    y0: f32,
    rw: f32,
    rh: f32,
    r: f32,
    alpha: f32,
    color: F,
) {
    let cx = x0 + rw / 2.0;
    let cy = y0 + rh / 2.0;
    let (hw, hh) = (rw / 2.0, rh / 2.0);
    let x_min = (x0 - 1.0).floor() as i32;
    let x_max = (x0 + rw + 1.0).ceil() as i32;
    let y_min = (y0 - 1.0).floor() as i32;
    let y_max = (y0 + rh + 1.0).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = sd_rrect(px - cx, py - cy, hw, hh, r);
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                put(buf, w, h, x, y, color(px, py), cov * alpha);
            }
        }
    }
}

fn fill_circle_fn<F: Fn(f32, f32) -> (Rgb, f32)>(
    buf: &mut [u8],
    w: i32,
    h: i32,
    cx: f32,
    cy: f32,
    r: f32,
    alpha: f32,
    color: F,
) {
    let x_min = (cx - r - 1.0).floor() as i32;
    let x_max = (cx + r + 1.0).ceil() as i32;
    let y_min = (cy - r - 1.0).floor() as i32;
    let y_max = (cy + r + 1.0).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = dist(px, py, cx, cy) - r;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                let (c, a) = color(px, py);
                put(buf, w, h, x, y, c, cov * alpha * a);
            }
        }
    }
}

fn stroke_circle(buf: &mut [u8], w: i32, h: i32, cx: f32, cy: f32, r: f32, bw: f32, c: Rgb, alpha: f32) {
    let x_min = (cx - r - 1.0).floor() as i32;
    let x_max = (cx + r + 1.0).ceil() as i32;
    let y_min = (cy - r - 1.0).floor() as i32;
    let y_max = (cy + r + 1.0).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = (dist(px, py, cx, cy) - r).abs() - bw / 2.0;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                put(buf, w, h, x, y, c, cov * alpha);
            }
        }
    }
}

/// Дуга окружности: обводка только там, где угол atan2 ∈ [a0, a1].
#[allow(clippy::too_many_arguments)]
fn stroke_arc(
    buf: &mut [u8],
    w: i32,
    h: i32,
    cx: f32,
    cy: f32,
    r: f32,
    a0: f32,
    a1: f32,
    bw: f32,
    c: Rgb,
    alpha: f32,
) {
    let x_min = (cx - r - bw).floor() as i32;
    let x_max = (cx + r + bw).ceil() as i32;
    let y_min = (cy - r - bw).floor() as i32;
    let y_max = (cy + r + bw).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let ang = (py - cy).atan2(px - cx);
            if ang < a0 || ang > a1 {
                continue;
            }
            let d = (dist(px, py, cx, cy) - r).abs() - bw / 2.0;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                put(buf, w, h, x, y, c, cov * alpha);
            }
        }
    }
}

fn seg(buf: &mut [u8], w: i32, h: i32, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, c: Rgb, alpha: f32) {
    let x_min = (x0.min(x1) - width).floor() as i32;
    let x_max = (x0.max(x1) + width).ceil() as i32;
    let y_min = (y0.min(y1) - width).floor() as i32;
    let y_max = (y0.max(y1) + width).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = seg_dist(px, py, x0, y0, x1, y1) - width / 2.0;
            let cov = (0.5 - d).clamp(0.0, 1.0);
            if cov > 0.0 {
                put(buf, w, h, x, y, c, cov * alpha);
            }
        }
    }
}

/// Мягкое радиальное свечение (без реального блюра — гладкий спад альфы).
fn draw_glow(buf: &mut [u8], w: i32, h: i32, c: (f32, f32), outer: f32, inner: f32, col: Rgb, alpha: f32) {
    let x_min = (c.0 - outer - 1.0).floor() as i32;
    let x_max = (c.0 + outer + 1.0).ceil() as i32;
    let y_min = (c.1 - outer - 1.0).floor() as i32;
    let y_max = (c.1 + outer + 1.0).ceil() as i32;
    for y in y_min..y_max {
        for x in x_min..x_max {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let d = dist(px, py, c.0, c.1);
            let t = ((outer - d) / (outer - inner)).clamp(0.0, 1.0);
            let a = t * t * alpha;
            if a > 0.003 {
                put(buf, w, h, x, y, col, a);
            }
        }
    }
}

// ── математика ─────────────────────────────────────────────────────────────
fn dist(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt()
}

/// Знаковое расстояние до скруглённого прямоугольника (px,py — от центра).
fn sd_rrect(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - (hw - r);
    let qy = py.abs() - (hh - r);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

fn seg_dist(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= 0.0 {
        0.0
    } else {
        (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0)
    };
    dist(px, py, x0 + t * dx, y0 + t * dy)
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
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

    // Превью-рендер в BMP:
    //   PREVIEW_REC=0/1 cargo test render_preview -- --ignored --nocapture
    #[test]
    #[ignore]
    fn render_preview() {
        let rec = std::env::var("PREVIEW_REC").map(|v| v != "0").unwrap_or(true);
        let mut bars = vec![0.06f32; N_BARS];
        for _ in 0..60 {
            animate(&mut bars, if rec { 0.7 } else { 0.05 }, 1.3, rec);
        }
        let scale = std::env::var("PREVIEW_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1.0);
        let (w, h) = dims(scale);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        render(&mut buf, &Frame { bars: &bars, hovered: true, recording: rec, t: 1.3 }, scale);

        // композит поверх тёмного фона (имитация рабочего стола)
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
