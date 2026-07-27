//! Voice Inputter — локальный голосовой ввод для Windows на Rust.
//!
//! Слушает микрофон (cpal) → распознаёт потоково (sherpa-onnx, streaming
//! zipformer) → печатает текст в активное окно (SendInput). Активация:
//! имя-активатор, хоткей или иконка в трее. Во время диктовки показывает
//! волновой оверлей, не забирая фокус (курсор остаётся на месте).

// В релизе — без консольного окна; в отладке оставляем консоль для логов.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod config;
mod engine;
mod overlay;
mod shared;
mod stt;
mod typer;
mod ui;

use config::Config;

fn main() {
    // не даём запустить второй экземпляр (иначе в трее две одинаковые иконки)
    if ui::already_running() {
        ui::error_box("Voice Inputter уже запущен (см. иконку в трее).");
        return;
    }

    let cfg = Config::load();
    shared::init(cfg);

    // канал к рабочему потоку
    let (tx, rx) = crossbeam_channel::unbounded();
    *shared::shared().worker_tx.lock().unwrap() = Some(tx);

    // рабочий поток: распознавание + машина состояний
    let worker = std::thread::spawn(move || engine::run(rx));

    // UI + захват звука + цикл сообщений (блокирует до выхода)
    ui::run();

    // корректное завершение
    shared::send_worker(shared::WorkerMsg::Shutdown);
    let _ = worker.join();
}
