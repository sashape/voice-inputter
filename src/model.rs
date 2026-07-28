//! Модель распознавания: где она лежит, есть ли она и как её докачать.
//!
//! Скачиваем по HTTPS через WinHTTP (системный клиент — ни новых крейтов, ни
//! DLL), распаковываем `.tar.bz2` системным `tar.exe` (есть в Windows 10 1803+).
//! Всё идёт в фоновом потоке, состояние читает окно прогресса.

use crate::config::{data_dir, resolve_resource};
use crate::shared::{send_worker, shared, WorkerMsg};

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

const HOST: &str = "github.com";
const PATH_PREFIX: &str = "/k2-fsa/sherpa-onnx/releases/download/asr-models/";

/// Ход установки модели (читает окно прогресса).
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// ещё не начинали
    Idle,
    Downloading { got: u64, total: u64 },
    Extracting,
    Done,
    Failed(String),
    Cancelled,
}

static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
static CANCEL: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

fn cell() -> &'static Mutex<Status> {
    STATUS.get_or_init(|| Mutex::new(Status::Idle))
}

pub fn status() -> Status {
    cell().lock().unwrap().clone()
}

fn set(s: Status) {
    *cell().lock().unwrap() = s;
}

/// Подменяет состояние — только для превью окна загрузки в тестах.
#[cfg(test)]
pub fn set_for_test(s: Status) {
    set(s);
}

/// Имя папки модели из конфига («models/foo» → «foo»).
pub fn model_name() -> String {
    let p = shared().config.lock().unwrap().model_path.clone();
    p.replace('\\', "/")
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(&p)
        .to_string()
}

/// Куда распаковываем: `<каталог данных>/models`.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

/// Модель на месте (папка найдена и в ней есть tokens.txt).
pub fn installed() -> bool {
    let path = shared().config.lock().unwrap().model_path.clone();
    resolve_resource(&path)
        .map(|p| p.join("tokens.txt").exists())
        .unwrap_or(false)
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

pub fn cancel() {
    CANCEL.store(true, Ordering::SeqCst);
}

/// Запускает загрузку в фоне (повторный вызов во время работы игнорируется).
pub fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    CANCEL.store(false, Ordering::SeqCst);
    set(Status::Downloading { got: 0, total: 0 });
    std::thread::spawn(|| {
        let r = install();
        RUNNING.store(false, Ordering::SeqCst);
        match r {
            Ok(()) => {
                set(Status::Done);
                // движок ждёт модель — пусть пересоберёт распознаватель
                send_worker(WorkerMsg::Reload);
            }
            Err(e) if CANCEL.load(Ordering::SeqCst) => {
                eprintln!("[model] загрузка отменена ({e})");
                set(Status::Cancelled);
            }
            Err(e) => {
                eprintln!("[model] ошибка: {e}");
                set(Status::Failed(e));
            }
        }
    });
}

fn install() -> Result<(), String> {
    let name = model_name();
    let dir = models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("не создать {}: {e}", dir.display()))?;

    let archive = format!("{name}.tar.bz2");
    let tmp = std::env::temp_dir().join(&archive);
    let path = format!("{PATH_PREFIX}{archive}");
    eprintln!("[model] качаю https://{HOST}{path}");
    download(&path, &tmp)?;

    set(Status::Extracting);
    extract(&tmp, &dir)?;
    let _ = std::fs::remove_file(&tmp);

    if !dir.join(&name).join("tokens.txt").exists() {
        return Err(format!("в архиве нет папки {name} с tokens.txt"));
    }
    eprintln!("[model] готово: {}", dir.join(&name).display());
    Ok(())
}

/// Скачивание с прогрессом; отмена проверяется на каждом куске.
fn download(path: &str, out: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::File::create(out).map_err(|e| format!("не создать файл: {e}"))?;
    let mut failed: Option<String> = None;
    let mut sink = |chunk: &[u8], got: u64, total: u64| {
        if CANCEL.load(Ordering::SeqCst) {
            return false;
        }
        if let Err(e) = file.write_all(chunk) {
            failed = Some(format!("не записать файл: {e}"));
            return false;
        }
        set(Status::Downloading { got, total });
        true
    };
    let r = crate::http::get(HOST, path, &mut sink);
    drop(file);
    if let Some(e) = failed {
        let _ = std::fs::remove_file(out);
        return Err(e);
    }
    if let Err(e) = r {
        let _ = std::fs::remove_file(out);
        return Err(e);
    }
    Ok(())
}

/// Распаковка `.tar.bz2` системным tar.exe (bsdtar, Windows 10 1803+).
fn extract(archive: &std::path::Path, dir: &std::path::Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("tar.exe")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("не запустить tar.exe: {e} (нужна Windows 10 1803 или новее)"))?;
    if !out.status.success() {
        return Err(format!(
            "tar не смог распаковать архив: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Подпись вида «12,4 / 23,0 МБ».
pub fn human(got: u64, total: u64) -> String {
    let mb = |v: u64| v as f64 / (1024.0 * 1024.0);
    if total > 0 {
        format!("{:.1} / {:.1} МБ", mb(got), mb(total)).replace('.', ",")
    } else {
        format!("{:.1} МБ", mb(got)).replace('.', ",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Проверяет вызов системного tar.exe: собираем маленький .tar.bz2 и
    /// распаковываем его тем же кодом, что и модель.
    #[test]
    #[ignore]
    fn extract_roundtrip() {
        let tmp = std::env::temp_dir().join("vi-extract-test");
        let src = tmp.join("src").join("mymodel");
        let out = tmp.join("out");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(src.join("tokens.txt"), b"<blk>
").unwrap();

        let archive = tmp.join("mymodel.tar.bz2");
        let st = std::process::Command::new("tar.exe")
            .arg("-cjf")
            .arg(&archive)
            .arg("-C")
            .arg(src.parent().unwrap())
            .arg("mymodel")
            .status()
            .expect("tar.exe должен быть в Windows 10 1803+");
        assert!(st.success(), "не собрался тестовый архив");

        extract(&archive, &out).unwrap();
        assert!(out.join("mymodel").join("tokens.txt").exists(), "tokens.txt не распакован");

        // битый архив должен давать понятную ошибку, а не панику
        let bad = tmp.join("bad.tar.bz2");
        std::fs::write(&bad, b"not an archive").unwrap();
        let e = extract(&bad, &out).unwrap_err();
        assert!(e.contains("tar"), "странная ошибка: {e}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Проверяет живой путь WinHTTP (включая редирект GitHub на objects.*):
    /// качаем архив модели, после 2 МБ отменяем.
    /// `cargo test download_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn download_probe() {
        CANCEL.store(false, Ordering::SeqCst);
        let out = std::env::temp_dir().join("vi-download-probe.bin");
        let name = "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16.tar.bz2";
        let path = format!("{PATH_PREFIX}{name}");
        let dst = out.clone();
        let t = std::thread::spawn(move || download(&path, &dst));

        let start = std::time::Instant::now();
        let mut seen_total = 0u64;
        while start.elapsed() < std::time::Duration::from_secs(60) {
            if let Status::Downloading { got, total } = status() {
                if got > 2 * 1024 * 1024 {
                    seen_total = total;
                    println!("получено {}, отменяю", human(got, total));
                    cancel();
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let r = t.join().unwrap();
        assert!(seen_total > 10 * 1024 * 1024, "Content-Length не пришёл: {seen_total}");
        assert!(r.is_err(), "загрузка должна была прерваться по отмене");
        assert!(!out.exists(), "временный файл не убран после отмены");
        CANCEL.store(false, Ordering::SeqCst);
    }
}
