//! Иконки приложения: трей (PNG из дизайна, вшиты в exe) и сборка `.ico`
//! для самого exe (иконка в проводнике и на панели задач).
//!
//! PNG лежат в `assets/icons` и включаются `include_bytes!`, поэтому exe
//! остаётся самодостаточным — внешних файлов не требуется.

use tiny_skia::Pixmap;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC,
};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};
use windows::core::w;

// Трей: белая волна в покое, цветная с точкой — во время диктовки.
// Берём крупный исходник и уменьшаем: 256→16 усреднением по площади выходит
// чище, чем мелкие ассеты (их пришлось бы растягивать после обрезки полей).
const TRAY: &[u8] = include_bytes!("../assets/icons/tray-256.png");
const TRAY_REC: &[u8] = include_bytes!("../assets/icons/tray-rec-256.png");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tray {
    /// ждём имя-активатор
    Idle,
    /// идёт диктовка
    Rec,
    /// прослушивание выключено
    Off,
}

/// Тёмные чернила для белого глифа, когда трей светлый (светлая тема Windows).
const INK: (u8, u8, u8) = (30, 32, 52);

/// Светлая ли панель задач (`SystemUsesLightTheme` в реестре).
pub fn light_taskbar() -> bool {
    let mut v = 0u32;
    let mut n = std::mem::size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut u32 as *mut std::ffi::c_void),
            Some(&mut n),
        )
        .is_ok()
            && v == 1
    }
}

/// Иконка трея нужного размера (16/20/24/32 px в зависимости от DPI).
pub fn tray_icon(state: Tray, size: i32) -> HICON {
    tray_icon_themed(state, size, light_taskbar())
}

/// То же, но с явной темой панели задач (нужно тесту-превью).
fn tray_icon_themed(state: Tray, size: i32, light: bool) -> HICON {
    let size = size.clamp(8, 256);
    // в исходных PNG вокруг волны широкие прозрачные поля — из-за них иконка
    // в трее выглядит мельче соседних, поэтому обрезаем их и вписываем глиф
    let rec = state == Tray::Rec;
    let (Ok(pm), Ok(other)) = (
        Pixmap::decode_png(if rec { TRAY_REC } else { TRAY }),
        Pixmap::decode_png(if rec { TRAY } else { TRAY_REC }),
    ) else {
        return HICON::default();
    };
    // кадрируем оба состояния по общей рамке — иначе волна прыгала бы в размере
    // при переключении (у rec-варианта в рамку входит ещё и точка записи)
    let b = union_box(content_box(&pm), content_box(&other));
    let mut pm = fit(&pm, b, size as u32);
    // белый глиф на светлом трее не виден — перекрашиваем в тёмные чернила
    // (иконка одноцветная, поэтому замена цвета точная); rec цветная — как есть
    if !rec && light {
        tint(&mut pm, INK);
    }
    // выключено — та же иконка, но приглушённая
    let alpha = if state == Tray::Off { 0.45 } else { 1.0 };
    icon_from_pixmap(&pm, alpha)
}

/// Перекрашивает одноцветный глиф, сохраняя альфу (данные premultiplied).
fn tint(pm: &mut Pixmap, c: (u8, u8, u8)) {
    let d = pm.data_mut();
    for i in (0..d.len()).step_by(4) {
        let a = d[i + 3] as u32;
        d[i] = (c.0 as u32 * a / 255) as u8;
        d[i + 1] = (c.1 as u32 * a / 255) as u8;
        d[i + 2] = (c.2 as u32 * a / 255) as u8;
    }
}

/// Прямоугольник непрозрачного содержимого (x0, y0, x1, y1 — включительно).
fn content_box(pm: &Pixmap) -> (u32, u32, u32, u32) {
    let (w, h) = (pm.width(), pm.height());
    let d = pm.data();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if d[((y * w + x) * 4 + 3) as usize] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        (0, 0, w - 1, h - 1)
    } else {
        (x0, y0, x1, y1)
    }
}

fn union_box(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// Вырезает рамку `b` и вписывает её в квадрат `size` с сохранением пропорций.
fn fit(pm: &Pixmap, b: (u32, u32, u32, u32), size: u32) -> Pixmap {
    let (bw, bh) = (b.2 - b.0 + 1, b.3 - b.1 + 1);
    let mut crop = Pixmap::new(bw, bh).unwrap();
    {
        let (src, dst) = (pm.data(), crop.data_mut());
        for y in 0..bh {
            let s = (((y + b.1) * pm.width() + b.0) * 4) as usize;
            let d = ((y * bw) * 4) as usize;
            dst[d..d + (bw * 4) as usize].copy_from_slice(&src[s..s + (bw * 4) as usize]);
        }
    }
    let k = (size as f32 / bw as f32).min(size as f32 / bh as f32);
    let (tw, th) = (((bw as f32 * k).round() as u32).max(1), ((bh as f32 * k).round() as u32).max(1));
    let scaled = resize(&crop, tw, th);
    let mut out = Pixmap::new(size, size).unwrap();
    let (ox, oy) = ((size - tw) / 2, (size - th) / 2);
    {
        let (src, dst) = (scaled.data(), out.data_mut());
        for y in 0..th {
            let s = ((y * tw) * 4) as usize;
            let d = (((y + oy) * size + ox) * 4) as usize;
            dst[d..d + (tw * 4) as usize].copy_from_slice(&src[s..s + (tw * 4) as usize]);
        }
    }
    out
}

/// Усреднение по площади: аккуратно уменьшает 256→24 без «каши» и алиасинга.
fn resize(src: &Pixmap, w: u32, h: u32) -> Pixmap {
    if src.width() == w && src.height() == h {
        return src.clone();
    }
    let mut out = Pixmap::new(w, h).unwrap();
    let (sw, sh) = (src.width() as f32, src.height() as f32);
    let sd = src.data();
    let dst = out.data_mut();
    for y in 0..h {
        let y0 = (y as f32 * sh / h as f32).floor() as u32;
        let y1 = (((y + 1) as f32 * sh / h as f32).ceil() as u32).min(src.height()).max(y0 + 1);
        for x in 0..w {
            let x0 = (x as f32 * sw / w as f32).floor() as u32;
            let x1 = (((x + 1) as f32 * sw / w as f32).ceil() as u32).min(src.width()).max(x0 + 1);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let i = ((sy * src.width() + sx) * 4) as usize;
                    for k in 0..4 {
                        acc[k] += sd[i + k] as u32;
                    }
                    n += 1;
                }
            }
            let d = ((y * w + x) * 4) as usize;
            for k in 0..4 {
                dst[d + k] = (acc[k] / n.max(1)) as u8;
            }
        }
    }
    out
}

/// HICON из пиксмапа (tiny-skia отдаёт premultiplied, иконке нужна прямая альфа).
fn icon_from_pixmap(pm: &Pixmap, alpha: f32) -> HICON {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    unsafe {
        let dc = CreateCompatibleDC(HDC::default());
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
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0)
        else {
            let _ = DeleteDC(dc);
            return HICON::default();
        };
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
        let src = pm.data();
        for i in 0..(w * h) as usize {
            let a = (src[i * 4 + 3] as f32 * alpha) as u32;
            let un = |c: u8| {
                let a0 = src[i * 4 + 3] as u32;
                if a0 == 0 {
                    0
                } else {
                    (c as u32 * 255 / a0).min(255)
                }
            };
            px[i] = (a << 24) | (un(src[i * 4]) << 16) | (un(src[i * 4 + 1]) << 8) | un(src[i * 4 + 2]);
        }
        // маска не используется (берём альфу цветного DIB), но нужна непустой
        let zeros = vec![0u8; ((w * h) as usize).max(64)];
        let mask = CreateBitmap(w, h, 1, 1, Some(zeros.as_ptr() as *const std::ffi::c_void));
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

// ── сборка assets/app.ico ─────────────────────────────────────────────────
/// Собирает многоразмерный `.ico` из PNG (мелкие размеры — BMP-записи,
/// 256 — как PNG внутри контейнера, так делает и Windows).
#[cfg(test)]
fn build_ico(sources: &[(&str, u32)], out: &std::path::Path) -> std::io::Result<()> {
    // (данные записи, ширина, высота)
    let mut entries: Vec<(Vec<u8>, u32)> = Vec::new();
    for &(path, size) in sources {
        let png = std::fs::read(path)?;
        if size == 256 {
            entries.push((png, size));
            continue;
        }
        let pm = Pixmap::decode_png(&png).expect("png");
        let pm = resize(&pm, size, size);
        entries.push((bmp_entry(&pm), size));
    }

    let mut out_buf = Vec::new();
    out_buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out_buf.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out_buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len() as u32;
    for (data, size) in &entries {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out_buf.push(dim); // ширина
        out_buf.push(dim); // высота
        out_buf.push(0); // палитра
        out_buf.push(0); // reserved
        out_buf.extend_from_slice(&1u16.to_le_bytes()); // planes
        out_buf.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out_buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out_buf.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (data, _) in &entries {
        out_buf.extend_from_slice(data);
    }
    std::fs::write(out, out_buf)
}

/// Запись .ico в формате BMP: заголовок с удвоенной высотой, пиксели снизу
/// вверх (BGRA, прямая альфа) и пустая AND-маска.
#[cfg(test)]
fn bmp_entry(pm: &Pixmap) -> Vec<u8> {
    let (w, h) = (pm.width(), pm.height());
    let mask_row = ((w + 31) / 32 * 4) as usize; // 1bpp, выравнивание до 4 байт
    let mut v = Vec::with_capacity(40 + (w * h * 4) as usize + mask_row * h as usize);
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&(w as i32).to_le_bytes());
    v.extend_from_slice(&((h * 2) as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    v.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    v.extend_from_slice(&[0u8; 16]); // разрешение и палитра
    let src = pm.data();
    for y in (0..h).rev() {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let a = src[i + 3] as u32;
            let un = |c: u8| if a == 0 { 0u8 } else { ((c as u32 * 255 / a).min(255)) as u8 };
            v.push(un(src[i + 2])); // B
            v.push(un(src[i + 1])); // G
            v.push(un(src[i])); // R
            v.push(a as u8);
        }
    }
    v.extend(std::iter::repeat(0u8).take(mask_row * h as usize));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Рисует настоящие HICON трея (все состояния и размеры) на тёмном фоне:
    /// `cargo test tray_preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn tray_preview() {
        use windows::Win32::Graphics::Gdi::SelectObject;
        use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL};

        let sizes = [16i32, 20, 24, 32];
        let states = [Tray::Idle, Tray::Rec, Tray::Off];
        let cell = 44i32;
        // слева тёмная панель задач, справа светлая
        let (w, h) = (cell * sizes.len() as i32 * 2, cell * states.len() as i32);
        unsafe {
            let dc = CreateCompatibleDC(HDC::default());
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let dib =
                CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0).unwrap();
            SelectObject(dc, windows::Win32::Graphics::Gdi::HGDIOBJ(dib.0));
            let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
            for y in 0..h {
                for x in 0..w {
                    // фон: слева тёмная панель задач, справа светлая
                    px[(y * w + x) as usize] =
                        if x < w / 2 { 0xFF20_222C } else { 0xFFF3_F3F3 };
                }
            }

            for (r, st) in states.iter().enumerate() {
                for (c, &size) in sizes.iter().enumerate() {
                    for (half, light) in [(0i32, false), (1, true)] {
                        let icon = tray_icon_themed(*st, size, light);
                        let x = half * w / 2 + c as i32 * cell + (cell - size) / 2;
                        let y = r as i32 * cell + (cell - size) / 2;
                        let _ = DrawIconEx(dc, x, y, icon, size, size, 0, None, DI_NORMAL);
                        let _ = DestroyIcon(icon);
                    }
                }
            }

            let mut pm = Pixmap::new(w as u32, h as u32).unwrap();
            let out = pm.pixels_mut();
            for i in 0..(w * h) as usize {
                let p = px[i];
                out[i] = tiny_skia::PremultipliedColorU8::from_rgba(
                    (p >> 16) as u8, (p >> 8) as u8, p as u8, 255,
                )
                .unwrap();
            }
            let path = std::path::Path::new(
                &std::env::var("TRAY_OUT").unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into()),
            )
            .join("tray_states.png");
            pm.save_png(&path).unwrap();
            println!("wrote {}", path.display());

            let _ = DeleteObject(dib);
            let _ = DeleteDC(dc);
        }
    }

    /// Пересобирает `assets/app.ico` из PNG дизайна:
    /// `cargo test build_app_ico -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn build_app_ico() {
        let src = |n: &str| format!("assets/icons/{n}");
        let sources: Vec<(String, u32)> = vec![
            (src("taskbar-32.png"), 16),
            (src("taskbar-32.png"), 24),
            (src("taskbar-32.png"), 32),
            (src("taskbar-48.png"), 48),
            (src("taskbar-256.png"), 64),
            (src("taskbar-256.png"), 128),
            (src("taskbar-256.png"), 256),
        ];
        let refs: Vec<(&str, u32)> = sources.iter().map(|(p, s)| (p.as_str(), *s)).collect();
        let out = std::path::Path::new("assets/app.ico");
        build_ico(&refs, out).unwrap();
        println!("wrote {} ({} байт)", out.display(), std::fs::metadata(out).unwrap().len());
    }
}
