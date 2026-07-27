//! Рабочий поток: потоковое распознавание (sherpa-onnx) + машина состояний
//! (ожидание имени ↔ диктовка). Ввод — live: растущая гипотеза печатается
//! сразу, на конце фразы (endpoint) — пробел и сброс.

use crate::config::{resolve_resource, Config};
use crate::shared::{shared, WorkerMsg};
use crate::stt::Recognizer;
use crate::{typer, ui};
use crossbeam_channel::Receiver;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Instant;

#[derive(PartialEq)]
enum State {
    Idle,
    Dictating,
}

/// Что сейчас напечатано для текущей (незавершённой) фразы.
struct Live {
    emitted: String,
}

impl Live {
    fn new() -> Self {
        Live {
            emitted: String::new(),
        }
    }

    /// Привести напечатанное к `target`: стереть расхождение и допечатать хвост.
    fn reconcile(&mut self, target: &str) {
        let (back, suffix) = diff(&self.emitted, target);
        if back > 0 {
            typer::backspace(back);
        }
        if !suffix.is_empty() {
            typer::type_text(&suffix);
        }
        self.emitted = target.to_string();
    }

    /// Завершить фразу: пробел после текста и сброс накопителя.
    fn finalize(&mut self, cfg: &Config) {
        if !self.emitted.is_empty() {
            if cfg.append_space {
                typer::type_text(" ");
            }
            self.emitted.clear();
        }
    }

    fn reset(&mut self) {
        self.emitted.clear();
    }
}

pub fn run(rx: Receiver<WorkerMsg>) {
    let cfg0 = shared().config.lock().unwrap().clone();

    let model_path = resolve_resource(&cfg0.model_path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| cfg0.model_path.clone());

    let stt = match Recognizer::new(&model_path, 16000) {
        Ok(r) => r,
        Err(e) => {
            ui::error_box(&format!(
                "Не удалось загрузить модель распознавания.\n\n{e}\n\nПроверьте model_path в config.json (папка модели sherpa-onnx)."
            ));
            return;
        }
    };
    eprintln!("[engine] модель sherpa загружена: {model_path}");

    let mut state = State::Idle;
    let mut enabled = true;
    let mut last_speech = Instant::now();
    let mut live = Live::new();

    for msg in rx.iter() {
        match msg {
            WorkerMsg::Shutdown => break,
            WorkerMsg::SetEnabled(v) => {
                enabled = v;
                shared().enabled.store(v, SeqCst);
                if !v && state == State::Dictating {
                    stop_dictation(&cfg0, &stt, &mut state, &mut live);
                }
                stt.reset();
                live.reset();
                ui::post_state();
            }
            WorkerMsg::Reset => {
                stt.reset();
                live.reset();
            }
            WorkerMsg::Toggle => {
                if state == State::Dictating {
                    stop_dictation(&cfg0, &stt, &mut state, &mut live);
                } else {
                    start_dictation(&stt, &mut state, &mut live);
                    last_speech = Instant::now();
                }
            }
            WorkerMsg::Audio(samples) => {
                if !enabled {
                    continue;
                }
                let cfg = shared().config.lock().unwrap().clone();

                stt.accept(&samples);
                let text = stt.text();
                let endpoint = stt.is_endpoint();

                match state {
                    State::Idle => {
                        if !text.is_empty() && contains_word(&text, &cfg.wake_words).is_some() {
                            start_dictation(&stt, &mut state, &mut live);
                            last_speech = Instant::now();
                        } else if endpoint {
                            stt.reset(); // отбросить накопленный шум
                        }
                    }
                    State::Dictating => {
                        if !text.is_empty() {
                            last_speech = Instant::now();
                        }
                        if cfg.live_typing {
                            live.reconcile(&shape(&cfg, &text));
                        }
                        if endpoint {
                            if !cfg.live_typing {
                                live.reconcile(&shape(&cfg, &text));
                            }
                            if contains_word(&text, &cfg.stop_words).is_some() {
                                stop_dictation(&cfg, &stt, &mut state, &mut live);
                            } else {
                                live.finalize(&cfg); // пробел за завершённой фразой
                                stt.reset(); // следующая фраза с чистого листа
                            }
                        }
                    }
                }

                // авто-выключение диктовки по длительной тишине
                if state == State::Dictating
                    && cfg.silence_timeout > 0.0
                    && last_speech.elapsed().as_secs_f32() > cfg.silence_timeout
                {
                    stop_dictation(&cfg, &stt, &mut state, &mut live);
                }
            }
        }
    }
    eprintln!("[engine] остановлен");
}

fn start_dictation(stt: &Recognizer, state: &mut State, live: &mut Live) {
    if *state == State::Dictating {
        return;
    }
    stt.reset();
    live.reset();
    *state = State::Dictating;
    shared().dictating.store(true, SeqCst);
    eprintln!("[engine] ▶ диктовка ВКЛ");
    ui::post_state();
}

fn stop_dictation(cfg: &Config, stt: &Recognizer, state: &mut State, live: &mut Live) {
    if *state == State::Idle {
        return;
    }
    live.finalize(cfg); // дописать пробел за незавершённой фразой
    stt.reset();
    *state = State::Idle;
    shared().dictating.store(false, SeqCst);
    eprintln!("[engine] ■ диктовка ВЫКЛ");
    ui::post_state();
}

/// Готовит текст к вводу: убирает управляющие слова, тримит, ставит заглавную.
fn shape(cfg: &Config, raw: &str) -> String {
    let mut ctrl = cfg.stop_words.clone();
    ctrl.extend(cfg.wake_words.clone());
    let cleaned = strip_words(raw, &ctrl);
    if cleaned.is_empty() {
        return String::new();
    }
    if cfg.capitalize {
        let mut chars = cleaned.chars();
        if let Some(first) = chars.next() {
            return first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    cleaned
}

/// Возвращает найденное управляющее слово, если оно есть в тексте.
fn contains_word(text: &str, words: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let tokens: std::collections::HashSet<&str> = lower.split_whitespace().collect();
    for w in words {
        let w = w.to_lowercase();
        if w.contains(' ') {
            if lower.contains(&w) {
                return Some(w);
            }
        } else if tokens.contains(w.as_str()) {
            return Some(w);
        }
    }
    None
}

/// Убирает из текста управляющие слова.
fn strip_words(text: &str, words: &[String]) -> String {
    let mut out = text.to_lowercase();
    let mut sorted: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    sorted.sort_by_key(|w| std::cmp::Reverse(w.len()));
    for w in sorted {
        out = out.replace(&w, " ");
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Разница между напечатанным и целевым текстом: сколько символов стереть
/// (Backspace) и какой хвост допечатать. Диф по символам (Unicode).
fn diff(emitted: &str, target: &str) -> (usize, String) {
    let common = emitted
        .chars()
        .zip(target.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let back = emitted.chars().count() - common;
    let suffix: String = target.chars().skip(common).collect();
    (back, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(screen: &mut String, emitted: &mut String, target: &str) {
        let (back, suffix) = diff(emitted, target);
        for _ in 0..back {
            screen.pop();
        }
        screen.push_str(&suffix);
        *emitted = target.to_string();
    }

    #[test]
    fn streaming_with_revision() {
        let mut screen = String::new();
        let mut em = String::new();
        for t in ["При", "Привет", "Привет ми", "Привет мир"] {
            feed(&mut screen, &mut em, t);
        }
        assert_eq!(screen, "Привет мир");
        feed(&mut screen, &mut em, "Привет мир кот");
        feed(&mut screen, &mut em, "Привет мир как");
        assert_eq!(screen, "Привет мир как");
    }

    #[test]
    fn shape_strips_and_capitalizes() {
        let cfg = crate::config::Config::default();
        assert_eq!(shape(&cfg, "джарвис привет мир"), "Привет мир");
        assert_eq!(shape(&cfg, "это всё стоп"), "Это всё");
        assert_eq!(shape(&cfg, "стоп"), "");
    }
}
