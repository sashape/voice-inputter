//! Глобальное разделяемое состояние между UI-потоком и аудио-потоком.

use crate::config::Config;
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32};
use std::sync::{Mutex, OnceLock};

/// Сообщения рабочему (аудио/STT) потоку.
pub enum WorkerMsg {
    /// Порция сэмплов 16 кГц mono f32 [-1..1] из микрофона.
    Audio(Vec<f32>),
    /// Переключить диктовку (хоткей/трей).
    Toggle,
    /// Включить/выключить прослушивание микрофона.
    SetEnabled(bool),
    /// Сбросить накопленный контекст распознавания.
    Reset,
    /// Пересоздать распознаватель (после смены wake/stop-слов в настройках).
    Reload,
    /// Завершить поток.
    Shutdown,
}

pub struct Shared {
    /// Текущий уровень звука 0..1000 (для волны в оверлее).
    pub level: AtomicU32,
    /// Идёт ли сейчас диктовка.
    pub dictating: AtomicBool,
    /// Включено ли прослушивание.
    pub enabled: AtomicBool,
    /// HWND скрытого окна-приёмника сообщений (isize).
    pub main_hwnd: AtomicI32,
    /// HWND оверлея (isize, 0 = ещё не создан).
    pub main_hwnd_hi: AtomicI32,
    pub overlay_hwnd: AtomicI32,
    pub overlay_hwnd_hi: AtomicI32,
    /// Канал к рабочему потоку.
    pub worker_tx: Mutex<Option<Sender<WorkerMsg>>>,
    /// Текущая конфигурация.
    pub config: Mutex<Config>,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

pub fn init(config: Config) {
    let _ = SHARED.set(Shared {
        level: AtomicU32::new(0),
        dictating: AtomicBool::new(false),
        enabled: AtomicBool::new(true),
        main_hwnd: AtomicI32::new(0),
        main_hwnd_hi: AtomicI32::new(0),
        overlay_hwnd: AtomicI32::new(0),
        overlay_hwnd_hi: AtomicI32::new(0),
        worker_tx: Mutex::new(None),
        config: Mutex::new(config),
    });
}

pub fn shared() -> &'static Shared {
    SHARED.get().expect("shared::init не вызван")
}

// HWND — это i64 на x64. Храним в двух AtomicI32 (lo/hi), чтобы не тянуть
// платформозависимые типы; помощники ниже упаковывают/распаковывают.
pub fn pack_hwnd(lo: &AtomicI32, hi: &AtomicI32, value: isize) {
    use std::sync::atomic::Ordering::SeqCst;
    lo.store((value as i64 & 0xFFFF_FFFF) as i32, SeqCst);
    hi.store(((value as i64 >> 32) & 0xFFFF_FFFF) as i32, SeqCst);
}

pub fn unpack_hwnd(lo: &AtomicI32, hi: &AtomicI32) -> isize {
    use std::sync::atomic::Ordering::SeqCst;
    let l = lo.load(SeqCst) as u32 as i64;
    let h = hi.load(SeqCst) as u32 as i64;
    ((h << 32) | l) as isize
}

pub fn send_worker(msg: WorkerMsg) {
    if let Some(tx) = shared().worker_tx.lock().unwrap().as_ref() {
        let _ = tx.send(msg);
    }
}

/// Обновить текущий уровень звука (0..1000) — читается волной оверлея.
pub fn send_level(level: u32) {
    shared()
        .level
        .store(level, std::sync::atomic::Ordering::Relaxed);
}

pub fn current_level() -> u32 {
    shared().level.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_dictating() -> bool {
    shared()
        .dictating
        .load(std::sync::atomic::Ordering::SeqCst)
}

pub fn is_enabled() -> bool {
    shared().enabled.load(std::sync::atomic::Ordering::SeqCst)
}
