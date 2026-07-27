//! Софт-рендер волнового оверлея по макету (пилюля + микрофон + бары +
//! свечение + ховер-контролы) с аналитическим антиалиасингом (SDF).
//!
//! Модуль не зависит от WinAPI: заполняет RGBA-буфер (прямая альфа),
//! а `ui.rs` конвертирует его в premultiplied BGRA и толкает в
//! UpdateLayeredWindow. Раскладка и hit-test — здесь же, чтобы отрисовка
//! и попадания мыши всегда совпадали.

// ── размеры окна и раскладка (px) ─────────────────────────────────────────
pub const OV_W: i32 = 380;
pub const OV_H: i32 = 152;
pub const N_BARS: usize = 32;

const PILL_X: f32 = 30.0;
const PILL_Y: f32 = 60.0;
const PILL_W: f32 = 320.0;
const PILL_H: f32 = 72.0;
const PILL_R: f32 = 36.0;
const PILL_CY: f32 = PILL_Y + PILL_H / 2.0; // 96

const MIC_CX: f32 = 66.0;
const MIC_CY: f32 = PILL_CY;
const MIC_R: f32 = 26.0;
const STOP_HALF: f32 = 8.0; // квадрат-стоп 16px

const WF_X0: f32 = 108.0;
const BAR_W: f32 = 3.0;
const BAR_SLOT: f32 = 7.0; // ширина 3 + зазор 4
const WF_HALF: f32 = 22.0; // половина высоты контейнера волны (44px)

const CTRL_R: f32 = 17.0;
const CTRL_CY: f32 = 33.0;
const GEAR_CX: f32 = 169.0;
const CLOSE_CX: f32 = 211.0;

// ── палитра (blurple accent) ──────────────────────────────────────────────
type Rgb = (u8, u8, u8);
const ACCENT_200: Rgb = (231, 229, 254); // #e7e5fe
const ACCENT_300: Rgb = (210, 206, 253); // #d2cefd
const ACCENT_400: Rgb = (181, 171, 252); // #b5abfc
const ACCENT_500: Rgb = (150, 138, 224); // #968ae0
const ACCENT_700: Rgb = (93, 82, 148); // #5d5294
const PILL_TOP: Rgb = (42, 45, 68); // #2a2d44
const PILL_MID: Rgb = (28, 30, 48); // #1c1e30
const PILL_BOT: Rgb = (23, 25, 41); // #171929

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
}

/// Размер окна оверлея в физических пикселях при данном масштабе.
pub fn dims(s: f32) -> (i32, i32) {
    ((OV_W as f32 * s).round() as i32, (OV_H as f32 * s).round() as i32)
}

// ── анимация баров (из макета, модулируется реальным уровнем звука) ────────
pub fn animate(bars: &mut [f32], level: f32, t: f32) {
    let n = bars.len();
    if n == 0 {
        return;
    }
    let mid = (n as f32 - 1.0) / 2.0;
    for i in 0..n {
        let center = 1.0 - (i as f32 - mid).abs() / mid; // 0 у краёв .. 1 в центре
        let s = i as f32 * 0.663;
        let env = 0.5 + 0.5 * (t * 1.7 + s * 2.3).sin();
        let fast = ((t * 6.0 + s).sin() * (t * 3.3 + s * 0.6).sin()).abs();
        let base = 0.16 + (0.24 + 0.6 * center) * (0.35 + 0.65 * fast) * env;
        // реакция на голос: тихо → низкие бары, громко → высокие
        let target = (base * (0.45 + 1.1 * level)).clamp(0.06, 1.0);
        bars[i] += (target - bars[i]) * 0.35;
    }
}

// ── hit-test (физические координаты окна; s — масштаб) ────────────────────
pub fn hit_test(x: i32, y: i32, hovered: bool, s: f32) -> Region {
    // переводим в логические единицы и сравниваем с базовой раскладкой
    let px = (x as f32 + 0.5) / s;
    let py = (y as f32 + 0.5) / s;
    if dist(px, py, MIC_CX, MIC_CY) <= MIC_R {
        return Region::Mic;
    }
    if hovered {
        if dist(px, py, GEAR_CX, CTRL_CY) <= CTRL_R {
            return Region::Gear;
        }
        if dist(px, py, CLOSE_CX, CTRL_CY) <= CTRL_R {
            return Region::Close;
        }
    }
    if sd_rrect(px - (PILL_X + PILL_W / 2.0), py - PILL_CY, PILL_W / 2.0, PILL_H / 2.0, PILL_R)
        <= 0.0
    {
        return Region::Pill;
    }
    // «коридор» между пилюлей и контролами, чтобы курсор не терял hover
    if hovered && (150.0..=230.0).contains(&px) && (14.0..=PILL_Y).contains(&py) {
        return Region::Pill;
    }
    Region::None
}

// ── отрисовка кадра в RGBA-буфер (s — масштаб, физ. пиксели) ───────────────
pub fn render(buf: &mut [u8], f: &Frame, s: f32) {
    for b in buf.iter_mut() {
        *b = 0;
    }
    let (w, h) = dims(s);

    // тело пилюли — вертикальный градиент (без внешнего свечения/тени)
    fill_rrect(buf, w, h, PILL_X * s, PILL_Y * s, PILL_W * s, PILL_H * s, PILL_R * s, 1.0, |_, py| {
        pill_grad(((py / s - PILL_Y) / PILL_H).clamp(0.0, 1.0))
    });

    // верхний внутренний блик
    fill_rrect(
        buf,
        w,
        h,
        (PILL_X + 8.0) * s,
        (PILL_Y + 1.0) * s,
        (PILL_W - 16.0) * s,
        1.5 * s,
        1.0,
        0.05,
        |_, _| (255, 255, 255),
    );

    // рамка 2.5px accent-200 @ .7
    stroke_rrect(buf, w, h, PILL_X * s, PILL_Y * s, PILL_W * s, PILL_H * s, PILL_R * s, 2.5 * s, ACCENT_200, 0.70);

    // микрофон-кнопка
    draw_mic(buf, w, h, s);

    // бары
    for (i, &lv) in f.bars.iter().enumerate() {
        let bx = (WF_X0 + i as f32 * BAR_SLOT) * s;
        let bh = (lv.clamp(0.0, 1.0) * WF_HALF).max(1.6) * s;
        let top = MIC_CY * s - bh;
        let bot = MIC_CY * s + bh;
        fill_rrect(buf, w, h, bx, top, BAR_W * s, bot - top, (BAR_W / 2.0) * s, 1.0, move |_, py| {
            bar_grad((py - top) / (bot - top).max(1.0))
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

fn draw_mic(buf: &mut [u8], w: i32, h: i32, s: f32) {
    let (mcx, mcy, mr) = (MIC_CX * s, MIC_CY * s, MIC_R * s);
    // фон круга — радиальный фиолетовый
    fill_circle_fn(buf, w, h, mcx, mcy, mr, 1.0, |px, py| {
        let d = dist(px, py, mcx, mcy) / mr; // 0 центр .. 1 край
        (ACCENT_500, lerp_f(0.42, 0.14, d.clamp(0.0, 1.0)))
    });
    // рамка accent-400
    stroke_circle(buf, w, h, mcx, mcy, mr, 1.4 * s, ACCENT_400, 0.9);
    // квадрат-стоп accent-200 (свечение внутри круга — не выходит за пилюлю)
    draw_glow(buf, w, h, (mcx, mcy), (STOP_HALF + 6.0) * s, STOP_HALF * s, ACCENT_200, 0.5);
    fill_rrect(
        buf,
        w,
        h,
        mcx - STOP_HALF * s,
        mcy - STOP_HALF * s,
        STOP_HALF * 2.0 * s,
        STOP_HALF * 2.0 * s,
        5.0 * s,
        1.0,
        |_, _| ACCENT_200,
    );
}

fn draw_ctrl_button(buf: &mut [u8], w: i32, h: i32, cx: f32, s: f32) {
    let (ccx, ccy, cr) = (cx * s, CTRL_CY * s, CTRL_R * s);
    fill_circle_fn(buf, w, h, ccx, ccy, cr, 1.0, |_, py| {
        let t = ((py - (ccy - cr)) / (2.0 * cr)).clamp(0.0, 1.0);
        (lerp(PILL_TOP, PILL_MID, t), 1.0)
    });
    stroke_circle(buf, w, h, ccx, ccy, cr, 1.0 * s, (255, 255, 255), 0.10);
}

fn draw_gear_icon(buf: &mut [u8], w: i32, h: i32, cx: f32, cy: f32, s: f32) {
    let (gx, gy) = (cx * s, cy * s);
    stroke_circle(buf, w, h, gx, gy, 5.0 * s, 2.0 * s, ACCENT_300, 0.95);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::PI / 4.0;
        let tx = gx + a.cos() * 7.6 * s;
        let ty = gy + a.sin() * 7.6 * s;
        fill_circle_fn(buf, w, h, tx, ty, 1.5 * s, 1.0, |_, _| (ACCENT_300, 0.95));
    }
}

fn draw_close_icon(buf: &mut [u8], w: i32, h: i32, cx: f32, cy: f32, s: f32) {
    let (xc, yc, r) = (cx * s, cy * s, 5.0 * s);
    seg(buf, w, h, xc - r, yc - r, xc + r, yc + r, 1.8 * s, ACCENT_300, 0.95);
    seg(buf, w, h, xc + r, yc - r, xc - r, yc + r, 1.8 * s, ACCENT_300, 0.95);
}

// ── градиенты ──────────────────────────────────────────────────────────────
fn pill_grad(t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    if t < 0.52 {
        lerp(PILL_TOP, PILL_MID, t / 0.52)
    } else {
        lerp(PILL_MID, PILL_BOT, (t - 0.52) / 0.48)
    }
}

fn bar_grad(t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    if t < 0.55 {
        lerp(ACCENT_200, ACCENT_500, t / 0.55)
    } else {
        lerp(ACCENT_500, ACCENT_700, (t - 0.55) / 0.45)
    }
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

fn stroke_rrect(
    buf: &mut [u8],
    w: i32,
    h: i32,
    x0: f32,
    y0: f32,
    rw: f32,
    rh: f32,
    r: f32,
    bw: f32,
    c: Rgb,
    alpha: f32,
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
            let outer = (0.5 - d).clamp(0.0, 1.0);
            let inner = (0.5 - (d + bw)).clamp(0.0, 1.0);
            let band = outer - inner;
            if band > 0.0 {
                put(buf, w, h, x, y, c, band * alpha);
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
            let a = t * t * alpha; // мягкий спад
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

fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
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
            let src_y = h - 1 - y; // bottom-up
            for x in 0..w {
                let s = ((src_y * w + x) * 3) as usize;
                let d = (54 + y * row + x * 3) as usize;
                f[d] = rgb[s + 2]; // B
                f[d + 1] = rgb[s + 1]; // G
                f[d + 2] = rgb[s]; // R
            }
        }
        std::fs::write(path, f).unwrap();
    }

    // Превью-рендер в BMP для визуальной проверки:
    //   cargo test render_preview -- --ignored --nocapture
    #[test]
    #[ignore]
    fn render_preview() {
        let mut bars = vec![0.06f32; N_BARS];
        // прогреем анимацию, будто идёт диктовка со средним уровнем
        for _ in 0..40 {
            animate(&mut bars, 0.7, 1.3);
        }
        let scale = std::env::var("PREVIEW_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1.0);
        let (w, h) = dims(scale);
        let mut buf = vec![0u8; (w * h * 4) as usize];
        render(&mut buf, &Frame { bars: &bars, hovered: true }, scale);

        // композит поверх тёмного фона (имитация рабочего стола)
        let bg = (26u8, 28u8, 38u8);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for p in 0..(w * h) as usize {
            let a = buf[p * 4 + 3] as f32 / 255.0;
            for k in 0..3 {
                let s = buf[p * 4 + k] as f32;
                let b = [bg.0, bg.1, bg.2][k] as f32;
                rgb[p * 3 + k] = (s * a + b * (1.0 - a)) as u8;
            }
        }
        let out = std::env::temp_dir().join(format!("voice_inputter_render_{scale}.bmp"));
        write_bmp(out.to_str().unwrap(), w, h, &rgb);
        println!("wrote {}", out.display());
    }
}
