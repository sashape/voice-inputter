//! Загрузка/сохранение настроек и поиск ресурсов (модель, DLL).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// Путь к папке модели Vosk.
    pub model_path: String,
    /// Имя выбранного микрофона (None = устройство по умолчанию).
    pub device_name: Option<String>,
    /// Слова-активаторы (имя ассистента), строчными.
    pub wake_words: Vec<String>,
    /// Слова, завершающие диктовку.
    pub stop_words: Vec<String>,
    /// Секунд тишины до авто-выключения (0 = никогда).
    pub silence_timeout: f32,
    /// Горячая клавиша, например "ctrl+alt+space", "ctrl+shift+d".
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    /// Доп. множитель размера оверлея поверх DPI (1.0 = как есть).
    #[serde(default = "default_scale")]
    pub overlay_scale: f32,
    /// Live-ввод: печатать слова сразу (true) или только после паузы (false).
    #[serde(default = "default_true")]
    pub live_typing: bool,
    /// Режим показа оверлея: "always" | "dictation" | "hidden".
    #[serde(default = "default_overlay_mode")]
    pub overlay_mode: String,
    /// Знаки препинания голосом: «запятая», «точка», «новая строка»…
    #[serde(default = "default_true")]
    pub punctuation: bool,
    /// Слово-приставка перед знаком («знак запятая»). Пусто — знак ставится
    /// по одному слову команды.
    #[serde(default)]
    pub punctuation_prefix: String,
    /// Свои команды пунктуации: слово → символ (дополняют встроенные).
    #[serde(default)]
    pub punctuation_words: BTreeMap<String, String>,
    /// Проверять обновления на GitHub (раз в сутки, только проверка).
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// Контекстное усиление wake/stop-слов (0 = выкл/greedy; >0 = beam-search).
    #[serde(default = "default_hotwords_score")]
    pub hotwords_score: f32,
    /// Пробел после каждой фразы.
    pub append_space: bool,
    /// Заглавная буква в начале фразы.
    pub capitalize: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model_path: "models/sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16".into(),
            device_name: None,
            wake_words: vec!["джарвис".into(), "компьютер".into()],
            stop_words: vec!["стоп".into(), "хватит".into(), "достаточно".into()],
            silence_timeout: 6.0,
            hotkey: default_hotkey(),
            overlay_scale: default_scale(),
            live_typing: true,
            overlay_mode: default_overlay_mode(),
            punctuation: true,
            punctuation_prefix: String::new(),
            punctuation_words: BTreeMap::new(),
            check_updates: true,
            hotwords_score: default_hotwords_score(),
            append_space: true,
            capitalize: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_overlay_mode() -> String {
    "always".into()
}

fn default_hotwords_score() -> f32 {
    2.0
}

fn default_hotkey() -> String {
    "ctrl+alt+space".into()
}

fn default_scale() -> f32 {
    1.0
}

/// Разбирает строку хоткея в (модификаторы, VK-код).
/// Поддержка: ctrl/alt/shift/win + буква/цифра/space/F1..F12.
pub fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;

    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    for part in s.split('+') {
        let p = part.trim().to_lowercase();
        match p.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            "space" => vk = Some(0x20),
            "" => {}
            other => {
                let bytes = other.as_bytes();
                if other.len() == 1 && bytes[0].is_ascii_alphabetic() {
                    vk = Some(bytes[0].to_ascii_uppercase() as u32);
                } else if other.len() == 1 && bytes[0].is_ascii_digit() {
                    vk = Some(bytes[0] as u32);
                } else if let Some(n) = other.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
                    if (1..=12).contains(&n) {
                        vk = Some(0x70 + n - 1); // VK_F1 = 0x70
                    }
                }
            }
        }
    }
    vk.map(|k| (mods, k))
}

/// Каталог, где лежит exe.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Куда писать данные (config.json, модель): рядом с exe, если туда можно
/// писать (портативная папка), иначе `%LOCALAPPDATA%\VoiceInputter` — так
/// приложение работает и из `Program Files`, где запись запрещена.
pub fn data_dir() -> PathBuf {
    let exe = exe_dir();
    if writable(&exe) {
        return exe;
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| exe.clone())
        .join("VoiceInputter");
    if std::fs::create_dir_all(&local).is_ok() {
        local
    } else {
        exe
    }
}

/// Проверяет запись пробным файлом — прав по ACL мало, важен реальный результат
/// (например, виртуализация записи в Program Files).
fn writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".vi-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// Ищет ресурс (модель/DLL): текущий каталог → рядом с exe → каталог данных.
pub fn resolve_resource(name: &str) -> Option<PathBuf> {
    let candidates = [PathBuf::from(name), exe_dir().join(name), data_dir().join(name)];
    candidates.into_iter().find(|p| p.exists())
}

impl Config {
    pub fn load() -> Config {
        // конфиг ищем в каталоге данных, затем рядом с exe и в текущем каталоге
        for p in [config_path(), exe_dir().join("config.json"), PathBuf::from("config.json")] {
            if let Ok(text) = std::fs::read_to_string(&p) {
                if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
                    return cfg;
                }
            }
        }
        Config::default()
    }

    pub fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), text);
        }
    }
}
