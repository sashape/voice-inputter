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
mod http;
mod icons;
mod model;
mod model_ui;
mod overlay;
mod paint;
mod punct;
mod settings;
mod shared;
mod startup;
mod stt;
mod typer;
mod ui;
mod update;
mod win_ui;

use config::Config;

/// Пишет панику в `crash.log` рядом с конфигом и показывает её пользователю:
/// в релизе консоли нет, иначе падение выглядит как «просто исчезло».
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bt = std::backtrace::Backtrace::force_capture();
        let text = format!(
            "=== {} (unix {secs}) v{}
{info}
{bt}
",
            "паника",
            env!("CARGO_PKG_VERSION")
        );
        let path = config::data_dir().join("crash.log");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = f.write_all(text.as_bytes());
        }
        eprintln!("{text}");
        // окно показываем только на первой панике: дальше пишем в лог молча,
        // иначе повторяющаяся ошибка завалит экран модальными окнами
        static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SHOWN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            ui::error_box(&format!(
                "Voice Inputter столкнулся с внутренней ошибкой и записал её в файл.

{info}

Подробности: {}",
                path.display()
            ));
        }
        prev(info);
    }));
}

fn main() {
    install_panic_hook();

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
