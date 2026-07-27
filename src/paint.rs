//! Общие примитивы отрисовки на tiny-skia: скруглённые прямоугольники,
//! градиенты, box-blur (тени и свечения), работа с цветом и easing.
//!
//! Используется и оверлеем, и окном настроек — оба порта одного дизайна.

use tiny_skia::{
    Color, GradientStop, LinearGradient, PathBuilder, Pixmap, Point, RadialGradient, Shader,
    SpreadMode, Transform,
};

pub type Rgb = (u8, u8, u8);

/// Логические px → физические.
pub fn sc(v: f32, s: f32) -> f32 {
    v * s
}

pub fn col(c: Rgb, a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn lerp_f(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

pub fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    (
        (a.0 as f32 + (b.0 as f32 - a.0 as f32) * t) as u8,
        (a.1 as f32 + (b.1 as f32 - a.1 as f32) * t) as u8,
        (a.2 as f32 + (b.2 as f32 - a.2 as f32) * t) as u8,
    )
}

pub fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
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

pub fn lin(x0: f32, y0: f32, x1: f32, y1: f32, stops: Vec<(f32, Color)>) -> Shader<'static> {
    let gs = stops.into_iter().map(|(p, c)| GradientStop::new(p, c)).collect();
    LinearGradient::new(
        Point::from_xy(x0, y0),
        Point::from_xy(x1, y1),
        gs,
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap_or_else(|| Shader::SolidColor(Color::from_rgba8(0, 0, 0, 0)))
}

pub fn radial(cx: f32, cy: f32, r: f32, stops: Vec<(f32, Color)>) -> Shader<'static> {
    let gs = stops.into_iter().map(|(p, c)| GradientStop::new(p, c)).collect();
    let c = Point::from_xy(cx, cy);
    RadialGradient::new(c, c, r, gs, SpreadMode::Pad, Transform::identity())
        .unwrap_or_else(|| Shader::SolidColor(Color::from_rgba8(0, 0, 0, 0)))
}

/// Знаковое расстояние до скруглённого прямоугольника (для hit-test).
pub fn sd_rrect(px: f32, py: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let qx = px.abs() - (hw - r);
    let qy = py.abs() - (hh - r);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - r
}

/// Коэффициент экспоненциального сглаживания, независимый от частоты кадров:
/// доля пути к цели за время `dt` при постоянной времени `tau`.
pub fn smooth_k(dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        return 1.0;
    }
    (1.0 - (-dt / tau).exp()).clamp(0.0, 1.0)
}

/// CSS `cubic-bezier(x1,y1,x2,y2)`: по прогрессу времени 0..1 даёт значение.
/// Ньютон по x, затем полином по y — как в браузере (допускает overshoot > 1).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    let d = |a: f32, b: f32, u: f32| {
        let v = 1.0 - u;
        3.0 * v * v * (a) + 6.0 * v * u * (b - a) + 3.0 * u * u * (1.0 - b)
    };
    let mut u = t;
    for _ in 0..6 {
        let dx = d(x1, x2, u);
        if dx.abs() < 1e-5 {
            break;
        }
        u -= (bez(x1, x2, u) - t) / dx;
        u = u.clamp(0.0, 1.0);
    }
    bez(y1, y2, u)
}

/// CSS `ease` = cubic-bezier(.25,.1,.25,1).
pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}

/// Быстрый разделимый box-blur (3 прохода ≈ гаусс) по premultiplied RGBA.
pub fn blur(pm: &mut Pixmap, radius: usize) {
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
