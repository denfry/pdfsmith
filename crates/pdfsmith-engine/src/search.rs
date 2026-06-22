//! Поиск по извлечённому тексту страницы (чистый, без pdfium). Работает над
//! `&[char]` (как в `pdfsmith_pdfium::text::PageText.chars`); индексы совпадений —
//! это индексы символов, по которым берутся боксы для подсветки.

/// Совпадение: позиция и длина в символах.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMatch {
    pub start: usize,
    pub len: usize,
}

/// Приводит символ к нижнему регистру для нечувствительного сравнения
/// (берётся первый символ развёртки — достаточно для латиницы и кириллицы).
fn norm(c: char, match_case: bool) -> char {
    if match_case {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Все вхождения `query` в `text`. Перекрытия учитываются (шаг +1).
/// `whole_word` требует неалфанумерических границ слева и справа.
pub fn search_in_chars(text: &[char], query: &str, match_case: bool, whole_word: bool) -> Vec<TextMatch> {
    let q: Vec<char> = query.chars().map(|c| norm(c, match_case)).collect();
    let mut out = Vec::new();
    if q.is_empty() || q.len() > text.len() {
        return out;
    }
    let t: Vec<char> = text.iter().map(|&c| norm(c, match_case)).collect();
    let mut i = 0usize;
    while i + q.len() <= t.len() {
        if t[i..i + q.len()] == q[..] && (!whole_word || word_bounded(&t, i, q.len())) {
            out.push(TextMatch { start: i, len: q.len() });
        }
        i += 1;
    }
    out
}

fn word_bounded(t: &[char], start: usize, len: usize) -> bool {
    let before = start == 0 || !t[start - 1].is_alphanumeric();
    let after = start + len >= t.len() || !t[start + len].is_alphanumeric();
    before && after
}

/// Порядок обхода страниц при поиске: текущая первой, затем по кругу.
pub fn search_page_order(start: usize, total: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    (0..total).map(|i| (start + i) % total).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_substring() {
        let t = chars("Hello");
        assert_eq!(search_in_chars(&t, "ll", false, false), vec![TextMatch { start: 2, len: 2 }]);
    }

    #[test]
    fn case_insensitive_by_default() {
        let t = chars("Hello");
        assert_eq!(search_in_chars(&t, "hello", false, false), vec![TextMatch { start: 0, len: 5 }]);
    }

    #[test]
    fn case_sensitive_when_requested() {
        let t = chars("Hello");
        assert!(search_in_chars(&t, "hello", true, false).is_empty());
    }

    #[test]
    fn cyrillic_case_insensitive() {
        let t = chars("Привет Мир");
        assert_eq!(search_in_chars(&t, "мир", false, false), vec![TextMatch { start: 7, len: 3 }]);
    }

    #[test]
    fn whole_word_excludes_substring() {
        // "cat" встречается отдельным словом (4) и внутри "scatter" (9).
        let t = chars("the cat scatter");
        assert_eq!(search_in_chars(&t, "cat", false, true), vec![TextMatch { start: 4, len: 3 }]);
    }

    #[test]
    fn no_matches_and_empty_query() {
        let t = chars("Hello");
        assert!(search_in_chars(&t, "xyz", false, false).is_empty());
        assert!(search_in_chars(&t, "", false, false).is_empty());
    }

    #[test]
    fn overlapping_matches_are_found() {
        let t = chars("aaaa");
        assert_eq!(search_in_chars(&t, "aa", false, false).len(), 3);
    }

    #[test]
    fn page_order_wraps_from_current() {
        assert_eq!(search_page_order(2, 5), vec![2, 3, 4, 0, 1]);
        assert_eq!(search_page_order(0, 3), vec![0, 1, 2]);
        assert_eq!(search_page_order(0, 0), Vec::<usize>::new());
    }
}
