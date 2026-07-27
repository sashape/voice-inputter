//! Автозапуск при входе в Windows — значение в `HKCU\...\CurrentVersion\Run`.
//! Пользовательская ветка: прав администратора не требует.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

const KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: PCWSTR = w!("VoiceInputter");

/// Команда запуска: путь к exe в кавычках (в пути бывают пробелы).
fn command() -> Vec<u16> {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// Прописан ли автозапуск (значение есть и указывает на текущий exe).
pub fn enabled() -> bool {
    let mut buf = [0u16; 1024];
    let mut n = (buf.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            KEY,
            NAME,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
            Some(&mut n),
        )
        .is_ok()
    };
    if !ok {
        return false;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let cur = String::from_utf16_lossy(&buf[..len]);
    let want = String::from_utf16_lossy(&command()[..command().len() - 1]);
    // путь мог измениться (папку перенесли) — тогда считаем, что автозапуск
    // включён, но при сохранении настроек значение перезапишется на актуальное
    !cur.is_empty() && (cur == want || cur.to_lowercase().contains("voice-inputter"))
}

/// Включает или выключает автозапуск.
pub fn set(on: bool) {
    unsafe {
        if on {
            let cmd = command();
            let bytes = (cmd.len() * 2) as u32;
            let e = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                KEY,
                NAME,
                REG_SZ.0,
                Some(cmd.as_ptr() as *const std::ffi::c_void),
                bytes,
            );
            if e.is_err() {
                eprintln!("[startup] не удалось включить автозапуск: {e:?}");
            }
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, KEY, NAME);
        }
    }
}
