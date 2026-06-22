//! Интеграционные тесты текстового слоя. Требуют `pdfium.dll` в корне и
//! файлов в `test_pdfs`; при отсутствии — тест помечается пропущенным.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use pdfsmith_pdfium::{init, Document};
use pdfsmith_pdfium::text::PageText;

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Инициализирует pdfium из корня; `false` → тест пропускается.
fn init_or_skip() -> bool {
    let root = workspace_root();
    if !root.join("pdfium.dll").exists() {
        eprintln!("ПРОПУСК: нет {}", root.join("pdfium.dll").display());
        return false;
    }
    init(Some(&root)).expect("привязка к pdfium.dll");
    true
}

/// Ищет первый PDF в `test_pdfs`, у которого на странице 0 есть текстовый слой.
/// Возвращает открытый документ, индекс страницы и извлечённый текст.
fn find_text_page() -> Option<(Document, usize, PageText)> {
    let dir = workspace_root().join("test_pdfs");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pdf").unwrap_or(false))
        .collect();
    files.sort();
    for f in files {
        if let Ok(doc) = Document::open(&f, None) {
            let pt_opt = doc
                .load_page(0)
                .ok()
                .and_then(|page| page.text().ok().map(|tp| tp.extract()));
            if let Some(pt) = pt_opt {
                if !pt.chars.is_empty() {
                    eprintln!("текстовый слой найден: {}", f.display());
                    return Some((doc, 0, pt));
                }
            }
        }
    }
    None
}

/// Диагностика: печатает число символов на стр. 0 каждого тестового PDF.
/// Запуск: `cargo test -p pdfsmith-pdfium --test text_layer text_layer_inventory -- --ignored --nocapture`
#[test]
#[ignore = "диагностика, запускать вручную"]
fn text_layer_inventory() {
    let _g = serial();
    if !init_or_skip() {
        return;
    }
    let dir = workspace_root().join("test_pdfs");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pdf").unwrap_or(false))
        .collect();
    files.sort();
    for f in files {
        match Document::open(&f, None) {
            Ok(doc) => {
                let n = doc
                    .load_page(0)
                    .ok()
                    .and_then(|p| p.text().ok().map(|tp| tp.char_count()))
                    .unwrap_or(-1);
                eprintln!("{:>6} симв. (стр.0)  {}", n, f.file_name().unwrap().to_string_lossy());
            }
            Err(e) => eprintln!("ОШИБКА {e}: {}", f.display()),
        }
    }
}

#[test]
fn extract_returns_aligned_chars_and_boxes() {
    let _g = serial();
    if !init_or_skip() {
        return;
    }
    let Some((doc, idx, pt)) = find_text_page() else {
        eprintln!("ПРОПУСК: ни в одном test_pdfs нет текстового слоя на стр.0");
        return;
    };
    // Длины символов и боксов строго равны.
    assert_eq!(pt.chars.len(), pt.boxes.len(), "chars и boxes должны быть одной длины");
    assert!(!pt.chars.is_empty());

    // Боксы лежат в пределах страницы (с допуском). Инженерные чертежи допускают
    // выход за MediaBox, поэтому допуск 200 pt (≈7 cm) — достаточно чтобы поймать
    // полностью сломанные координаты, но не ложные срабатывания на реальных файлах.
    let size = doc.page_size(idx).expect("размер страницы");
    eprintln!("размер страницы: {}×{} pt, символов: {}", size.width_pt, size.height_pt, pt.chars.len());
    for r in pt.boxes.iter().filter(|r| r.right > r.left || r.top > r.bottom) {
        assert!(r.left >= -200.0 && r.right <= size.width_pt + 200.0, "бокс по X вне страницы: {r:?}");
        assert!(r.bottom >= -200.0 && r.top <= size.height_pt + 200.0, "бокс по Y вне страницы: {r:?}");
    }
}
