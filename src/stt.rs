//! Потоковое распознавание речи через sherpa-onnx (OnlineRecognizer).
//!
//! Модель — streaming zipformer (transducer). Даёт растущую гипотезу
//! (`text()`) и признак конца фразы (`is_endpoint()`), что напрямую ложится
//! на live-ввод: текст печатается по мере роста, на endpoint — финализация.

use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig, OnlineStream};
use std::path::Path;

pub struct Recognizer {
    rec: OnlineRecognizer,
    stream: OnlineStream,
}

impl Recognizer {
    /// Создаёт распознаватель из папки модели (encoder/decoder/joiner + tokens
    /// определяются автоматически; предпочитаются int8-варианты).
    pub fn new(model_dir: &str, sample_rate: i32) -> Result<Self, String> {
        let dir = Path::new(model_dir);
        if !dir.is_dir() {
            return Err(format!("Папка модели не найдена: {model_dir}"));
        }
        let encoder = pick(dir, "encoder")?;
        let decoder = pick(dir, "decoder")?;
        let joiner = pick(dir, "joiner")?;
        let tokens = dir.join("tokens.txt");
        if !tokens.exists() {
            return Err(format!("В модели нет tokens.txt: {model_dir}"));
        }

        let mut cfg = OnlineRecognizerConfig::default();
        cfg.feat_config.sample_rate = sample_rate;
        cfg.feat_config.feature_dim = 80;
        cfg.model_config.transducer.encoder = Some(encoder);
        cfg.model_config.transducer.decoder = Some(decoder);
        cfg.model_config.transducer.joiner = Some(joiner);
        cfg.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        cfg.model_config.num_threads = 1;
        cfg.decoding_method = Some("greedy_search".into());
        // Детекция конца фразы (в Rust-обёртке дефолты нулевые — задаём сами).
        cfg.enable_endpoint = true;
        cfg.rule1_min_trailing_silence = 2.4; // тишина при пустой гипотезе
        cfg.rule2_min_trailing_silence = 0.8; // тишина после речи → конец фразы
        cfg.rule3_min_utterance_length = 30.0; // максимальная длина фразы, сек

        let rec = OnlineRecognizer::create(&cfg)
            .ok_or_else(|| "sherpa-onnx не смог создать распознаватель".to_string())?;
        let stream = rec.create_stream();
        Ok(Self { rec, stream })
    }

    /// Подать порцию сэмплов (16 кГц, mono, f32 [-1..1]) и продвинуть декодер.
    pub fn accept(&self, samples: &[f32]) {
        self.stream.accept_waveform(16000, samples);
        while self.rec.is_ready(&self.stream) {
            self.rec.decode(&self.stream);
        }
    }

    /// Текущая гипотеза (растёт по мере распознавания фразы).
    pub fn text(&self) -> String {
        self.rec
            .get_result(&self.stream)
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default()
    }

    /// Достигнут ли конец фразы (по тишине/длине).
    pub fn is_endpoint(&self) -> bool {
        self.rec.is_endpoint(&self.stream)
    }

    /// Сбросить состояние потока (начать новую фразу).
    pub fn reset(&self) {
        self.rec.reset(&self.stream);
    }
}

/// Ищет в папке .onnx-файл, содержащий `key`; предпочитает int8-вариант.
fn pick(dir: &Path, key: &str) -> Result<String, String> {
    let mut best: Option<std::path::PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
            if name.ends_with(".onnx") && name.contains(key) {
                let is_int8 = name.contains("int8");
                match &best {
                    None => best = Some(p),
                    Some(cur) => {
                        let cur_int8 = cur
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase().contains("int8"))
                            .unwrap_or(false);
                        // предпочитаем int8
                        if is_int8 && !cur_int8 {
                            best = Some(p);
                        }
                    }
                }
            }
        }
    }
    best.map(|p| p.to_string_lossy().into_owned())
        .ok_or_else(|| format!("В модели не найден файл *{key}*.onnx: {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sherpa_onnx::Wave;

    // Прогон тестового wav через модель — проверка распознавания без голоса:
    //   cargo test transcribe_sample -- --ignored --nocapture
    #[test]
    #[ignore]
    fn transcribe_sample() {
        let base = "models/sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16";
        let rec = Recognizer::new(base, 16000).expect("recognizer");
        let wav = format!("{base}/test_wavs/0.wav");
        let wave = Wave::read(&wav).expect("read wav");
        rec.stream.accept_waveform(wave.sample_rate(), wave.samples());
        rec.stream.input_finished();
        while rec.rec.is_ready(&rec.stream) {
            rec.rec.decode(&rec.stream);
        }
        let text = rec.text();
        println!("TRANSCRIPT[0.wav]: {text:?}");
        assert!(!text.trim().is_empty(), "пустой результат распознавания");
    }
}
