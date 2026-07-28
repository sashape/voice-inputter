//! Знаки препинания голосом: слова-команды в распознанном тексте заменяются
//! на символы («привет запятая как дела вопрос» → «Привет, как дела?»).
//!
//! Работает поверх обычного текста, поэтому вписывается в живой ввод как есть:
//! правка гипотезы распознавателя доедет до печати через тот же diff.

use std::collections::BTreeMap;

/// Как символ примыкает к соседям.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// к предыдущему слову вплотную: . , ! ? : ; … » )
    Tail,
    /// к следующему слову вплотную: « (
    Open,
    /// с пробелами с обеих сторон: —
    Free,
    /// вплотную с обеих сторон: дефис, @, / — годится для «кто-то», «ivan@mail»
    Glue,
    /// перевод строки (число строк)
    Newline(u8),
}

/// Встроенная таблица команд. Переопределяется через `punctuation_words`
/// в config.json (слово → символ), там же добавляются свои.
const RULES: &[(&str, &str)] = &[
    ("точка", "."),
    ("запятая", ","),
    // Только явные формы: «вопрос», «восклицание», «абзац» — обычные слова,
    // и командами их делать нельзя, иначе «этот вопрос важен» превратится в «?».
    ("вопросительный знак", "?"),
    ("знак вопроса", "?"),
    ("восклицательный знак", "!"),
    ("знак восклицания", "!"),
    ("двоеточие", ":"),
    ("точка с запятой", ";"),
    ("многоточие", "…"),
    ("тире", "—"),
    ("дефис", "-"),
    ("открыть скобку", "("),
    ("скобка открывается", "("),
    ("закрыть скобку", ")"),
    ("скобка закрывается", ")"),
    ("открыть кавычки", "«"),
    ("кавычки открываются", "«"),
    ("закрыть кавычки", "»"),
    ("кавычки закрываются", "»"),
    ("новая строка", "\n"),
    ("с новой строки", "\n"),
    ("новый абзац", "\n\n"),
    ("с нового абзаца", "\n\n"),
];

/// Слова-команды — их же подсказываем распознавателю как hotwords.
pub fn command_words(extra: &BTreeMap<String, String>, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = RULES.iter().map(|(w, _)| w.to_string()).collect();
    v.extend(extra.keys().map(|k| k.to_lowercase()));
    let p = prefix.trim().to_lowercase();
    if !p.is_empty() {
        v.push(p);
    }
    v
}

fn kind_of(sym: &str) -> Kind {
    match sym {
        "\n" => Kind::Newline(1),
        "\n\n" => Kind::Newline(2),
        "«" | "(" | "[" | "{" => Kind::Open,
        "—" => Kind::Free,
        s if s.chars().all(|c| ".,!?:;…»)]}".contains(c)) => Kind::Tail,
        // одиночные «внутрисловные» символы (дефис, @, /) клеим без пробелов
        s if s.chars().count() == 1 => Kind::Glue,
        _ => Kind::Free,
    }
}

fn ends_sentence(sym: &str) -> bool {
    sym.chars().any(|c| ".!?…".contains(c))
}

/// Ищет команду по фразе (сначала в пользовательской таблице).
fn lookup<'a>(phrase: &str, extra: &'a BTreeMap<String, String>) -> Option<String> {
    if let Some(s) = extra.get(phrase) {
        return Some(s.clone());
    }
    RULES
        .iter()
        .find(|(w, _)| *w == phrase)
        .map(|(_, s)| s.to_string())
}

/// Заменяет слова-команды на знаки.
///
/// `cap` — ставить заглавную после точки. `prefix` — слово-приставка: если оно
/// задано, команда срабатывает только следом за ним («знак запятая»), а само
/// слово «запятая» остаётся обычным словом. Пусто — команды срабатывают сразу.
pub fn apply(text: &str, extra: &BTreeMap<String, String>, cap: bool, prefix: &str) -> String {
    let prefix = prefix.trim().to_lowercase();
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out = String::new();
    let mut need_space = false; // нужен ли пробел перед следующим словом
    let mut sentence_start = true; // следующее слово начинает предложение
    let mut i = 0;

    while i < tokens.len() {
        // с приставкой команду ищем после неё, без приставки — с текущего слова
        let (base, skip) = if prefix.is_empty() {
            (i, 0)
        } else if tokens[i].to_lowercase() == prefix {
            (i + 1, 1)
        } else {
            (usize::MAX, 0) // не приставка — это обычное слово
        };

        // самая длинная команда из 1–3 слов
        let mut hit = None;
        if base != usize::MAX {
            for len in (1..=3).rev() {
                if base + len > tokens.len() {
                    continue;
                }
                let phrase = tokens[base..base + len].join(" ").to_lowercase();
                if let Some(sym) = lookup(&phrase, extra) {
                    hit = Some((len + skip, sym));
                    break;
                }
            }
        }

        if let Some((len, sym)) = hit {
            match kind_of(&sym) {
                Kind::Tail => {
                    out.push_str(&sym);
                    need_space = true;
                }
                Kind::Open => {
                    if !out.is_empty() && need_space {
                        out.push(' ');
                    }
                    out.push_str(&sym);
                    need_space = false;
                }
                Kind::Free => {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&sym);
                    need_space = true;
                }
                Kind::Glue => {
                    out.push_str(&sym);
                    need_space = false;
                }
                Kind::Newline(n) => {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    for _ in 0..n {
                        out.push('\n');
                    }
                    need_space = false;
                }
            }
            if ends_sentence(&sym) || matches!(kind_of(&sym), Kind::Newline(_)) {
                sentence_start = true;
            }
            i += len;
            continue;
        }

        // обычное слово
        if need_space && !out.is_empty() {
            out.push(' ');
        }
        let w = tokens[i];
        if cap && sentence_start {
            let mut c = w.chars();
            if let Some(f) = c.next() {
                out.extend(f.to_uppercase());
                out.push_str(c.as_str());
            }
        } else {
            out.push_str(w);
        }
        need_space = true;
        sentence_start = false;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(s: &str) -> String {
        apply(s, &BTreeMap::new(), true, "")
    }

    #[test]
    fn basic_marks() {
        assert_eq!(go("привет запятая как дела знак вопроса"), "Привет, как дела?");
        assert_eq!(go("это точка новое предложение точка"), "Это. Новое предложение.");
        assert_eq!(go("ура восклицательный знак"), "Ура!");
    }

    #[test]
    fn glued_symbols() {
        // дефис соединяет слова, а тире стоит отдельно
        assert_eq!(apply("кто дефис то", &BTreeMap::new(), false, ""), "кто-то");
    }

    #[test]
    fn spacing_rules() {
        // тире — с пробелами, скобки и кавычки прилипают к своему слову
        assert_eq!(go("жизнь тире игра"), "Жизнь — игра");
        assert_eq!(go("тест открыть скобку раз закрыть скобку"), "Тест (раз)");
        assert_eq!(go("он сказал открыть кавычки да закрыть кавычки"), "Он сказал «да»");
    }

    #[test]
    fn newlines() {
        assert_eq!(go("первая строка новая строка вторая"), "Первая строка\nВторая");
        assert_eq!(go("раз новый абзац два"), "Раз\n\nДва");
    }

    #[test]
    fn longest_match_wins() {
        // «точка с запятой» не должна разобраться как «точка» + «с» + «запятая»
        assert_eq!(go("раз точка с запятой два"), "Раз; два");
    }

    #[test]
    fn custom_words_override() {
        let mut extra = BTreeMap::new();
        extra.insert("собака".to_string(), "@".to_string());
        assert_eq!(apply("ivan собака mail точка ru", &extra, false, ""), "ivan@mail. ru");
    }

    #[test]
    fn without_capitalization() {
        assert_eq!(apply("привет точка пока", &BTreeMap::new(), false, ""), "привет. пока");
    }

    #[test]
    fn prefix_gates_commands() {
        let e = BTreeMap::new();
        // с приставкой «знак» команда срабатывает только после неё
        assert_eq!(apply("привет знак запятая как дела", &e, true, "знак"), "Привет, как дела");
        // без приставки слово-команда остаётся обычным словом
        assert_eq!(apply("точка зрения важна", &e, true, "знак"), "Точка зрения важна");
        // приставка без команды после неё — тоже обычное слово
        assert_eq!(apply("дорожный знак стоит", &e, true, "знак"), "Дорожный знак стоит");
        // многословная команда после приставки
        assert_eq!(apply("раз знак новая строка два", &e, true, "знак"), "Раз\nДва");
    }

    #[test]
    fn plain_text_untouched() {
        assert_eq!(go("просто текст без команд"), "Просто текст без команд");
        // обычные слова командами не считаются — только явные формы знаков
        assert_eq!(go("этот вопрос важен"), "Этот вопрос важен");
        assert_eq!(go("новый абзац начинается"), "

Начинается");
        assert_eq!(go("абзац текста"), "Абзац текста");
        assert_eq!(go(""), "");
    }
}
