//! Проверка обновлений: спрашиваем у GitHub последний релиз и сравниваем его
//! версию с текущей. Только проверка — ничего не скачивается и не ставится,
//! при находке показываем уведомление и пункт в меню трея.

use crate::shared::shared;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const API_HOST: &str = "api.github.com";
/// Проверяем не чаще раза в сутки — на GitHub API лимит и незачем шуметь.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// Задержка перед первой проверкой: не мешаем старту и загрузке модели.
const FIRST_DELAY: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub version: String,
    pub url: String,
}

static FOUND: OnceLock<Mutex<Option<Update>>> = OnceLock::new();

fn cell() -> &'static Mutex<Option<Update>> {
    FOUND.get_or_init(|| Mutex::new(None))
}

/// Найденное обновление (если проверка уже что-то нашла).
pub fn available() -> Option<Update> {
    cell().lock().unwrap().clone()
}

/// Фоновая проверка: сразу после старта и дальше раз в сутки.
/// Настройка `check_updates` читается перед каждой попыткой, так что выключение
/// в настройках действует без перезапуска.
pub fn watch() {
    // отладочная подмена: VI_FAKE_UPDATE=1.2.3 показывает уведомление сразу
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("VI_FAKE_UPDATE") {
        *cell().lock().unwrap() = Some(Update {
            version: v,
            url: "https://github.com/sashape/voice-inputter/releases/latest".into(),
        });
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(2));
            crate::ui::post_update();
        });
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(FIRST_DELAY);
        loop {
            if shared().config.lock().unwrap().check_updates {
                match check() {
                    Ok(Some(u)) => {
                        eprintln!("[update] доступна версия {}", u.version);
                        *cell().lock().unwrap() = Some(u);
                        crate::ui::post_update();
                    }
                    Ok(None) => eprintln!("[update] установлена последняя версия"),
                    Err(e) => eprintln!("[update] проверка не удалась: {e}"),
                }
            }
            std::thread::sleep(EVERY);
        }
    });
}

/// Одна проверка: `Ok(None)` — обновления нет.
pub fn check() -> Result<Option<Update>, String> {
    let repo = repo_slug().ok_or("в Cargo.toml нет ссылки на репозиторий")?;
    let body = crate::http::get_string(API_HOST, &format!("/repos/{repo}/releases/latest"), 512 * 1024)?;
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("ответ не разобран: {e}"))?;
    let tag = v["tag_name"].as_str().unwrap_or_default().to_string();
    if tag.is_empty() {
        return Err("в ответе нет tag_name".into());
    }
    let url = v["html_url"]
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("https://github.com/{repo}/releases/latest"));
    let remote = tag.trim_start_matches('v').to_string();
    Ok(if newer(&remote, env!("CARGO_PKG_VERSION")) {
        Some(Update { version: remote, url })
    } else {
        None
    })
}

/// «https://github.com/owner/repo» → «owner/repo».
fn repo_slug() -> Option<String> {
    let url = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let rest = url.strip_prefix("https://github.com/")?;
    (rest.matches('/').count() == 1).then(|| rest.to_string())
}

/// Сравнение версий по числовым частям: 0.10.0 новее 0.9.9, суффиксы вида
/// «-beta» отбрасываются.
fn newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split(['.', '-', '+'])
            .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>())
            .take_while(|p| !p.is_empty())
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let (a, b) = (parse(remote), parse(local));
    if a.is_empty() {
        return false;
    }
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(newer("0.9.1", "0.9.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.9.0"));
        assert!(!newer("0.9.0", "0.9.0"));
        assert!(!newer("0.8.9", "0.9.0"));
        assert!(!newer("", "0.9.0"));
        // суффиксы отбрасываем: 1.0.0-beta считаем как 1.0.0
        assert!(newer("1.0.0-beta", "0.9.0"));
        // «v» перед версией снимает вызывающий код
        assert!(newer("0.9.0", "0.8.0"));
    }

    #[test]
    fn slug_from_cargo_metadata() {
        assert_eq!(repo_slug().as_deref(), Some("sashape/voice-inputter"));
    }

    /// Живой запрос к GitHub API:
    /// `cargo test latest_release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn latest_release() {
        match check() {
            Ok(Some(u)) => println!("доступно {} → {}", u.version, u.url),
            Ok(None) => println!("текущая версия {} — последняя", env!("CARGO_PKG_VERSION")),
            Err(e) => panic!("проверка не удалась: {e}"),
        }
    }
}
