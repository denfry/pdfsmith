//! Тесты редактирования/конвертации против `test_pdfs/sample.pdf` (любой
//! многостраничный PDF с текстом). Требуют `pdfium.dll` в корне workspace.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use pdfsmith_pdfium::{init, Document, PdfRect, Rgba};

/// PDFium нельзя звать из двух потоков даже с мьютексом на каждом вызове —
/// последовательности вызовов должны быть атомарны. Сериализуем тесты.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn sample() -> Option<PathBuf> {
    let r = root();
    let pdf = r.join("test_pdfs").join("text.pdf");
    if !r.join("pdfium.dll").exists() || !pdf.exists() {
        eprintln!("ПРОПУСК: нет pdfium.dll или sample.pdf");
        return None;
    }
    init(Some(&r)).expect("pdfium");
    Some(pdf)
}

#[test]
fn edit_roundtrip() {
    let _g = serial();
    let Some(pdf) = sample() else { return };
    let mut doc = Document::open(&pdf, None).unwrap();
    let n = doc.page_count();
    assert!(n >= 2, "нужен многостраничный PDF");

    // Текст и поиск.
    let page = doc.load_page(0).unwrap();
    let text = page.text();
    let word = text.split_whitespace().find(|w| w.chars().count() >= 4).map(|s| s.to_string());
    if let Some(w) = &word {
        let hits = page.search(w);
        assert!(!hits.is_empty(), "поиск {w:?} не нашёл ничего");
        assert!(!hits[0].rects.is_empty());
    }

    // Координаты: угол страницы → PDF и обратно.
    let (px, py) = page.display_to_pdf(10.0, 10.0);
    let (dx, dy) = page.pdf_to_display(px, py);
    assert!((dx - 10.0).abs() < 0.5 && (dy - 10.0).abs() < 0.5, "{dx} {dy}");

    // Аннотации.
    let before = page.annotation_count();
    page.add_ink(&[(50.0, 50.0), (120.0, 90.0), (200.0, 60.0)], Rgba(220, 40, 40, 255), 3.0).unwrap();
    page.add_rect(PdfRect { left: 40.0, bottom: 40.0, right: 220.0, top: 120.0 }, Rgba(30, 90, 220, 255), 2.0).unwrap();
    page.add_highlight(PdfRect { left: 40.0, bottom: 300.0, right: 300.0, top: 330.0 }, Rgba(255, 220, 0, 255)).unwrap();
    page.add_note(100.0, 400.0, "Привет, заметка", Rgba(255, 200, 0, 255)).unwrap();
    assert_eq!(page.annotation_count(), before + 4);
    assert!(page.remove_last_annotation());
    assert_eq!(page.annotation_count(), before + 3);
    // Рендер с аннотациями не падает.
    let img = page.render_region(1.0, 0, 0, 200, 200, 0, true).unwrap();
    assert_eq!(img.rgba.len(), 200 * 200 * 4);
    drop(page);

    // Структура.
    doc.rotate_page(1, 1).unwrap();
    let s0 = doc.page_size(0).unwrap();
    let s1 = doc.page_size(1).unwrap();
    assert!((s1.width_pt - s0.height_pt).abs() < 1.0 || (s0.width_pt - s0.height_pt).abs() < 1.0);
    doc.delete_page(n - 1);
    assert_eq!(doc.page_count(), n - 1);
    doc.insert_blank(0, 300.0, 400.0).unwrap();
    assert_eq!(doc.page_count(), n);
    doc.move_page(0, n - 1).unwrap();
    assert!((doc.page_size(n - 1).unwrap().width_pt - 300.0).abs() < 0.5);

    // Сохранение и повторное открытие.
    let out = std::env::temp_dir().join("pdfsmith_edit_test.pdf");
    doc.save_to(&out).unwrap();
    let re = Document::open(&out, None).unwrap();
    assert_eq!(re.page_count(), n);
    assert_eq!(re.load_page(0).unwrap().annotation_count(), before + 3);

    // Извлечение и импорт.
    let ex = doc.extract(&[0, 1]).unwrap();
    assert_eq!(ex.page_count(), 2);
    let mut merged = Document::new().unwrap();
    merged.import_pages(&ex, &[], 0).unwrap();
    merged.import_pages(&re, &[0], 2).unwrap();
    assert_eq!(merged.page_count(), 3);
}

#[test]
fn image_and_text_pages() {
    let _g = serial();
    if sample().is_none() {
        return;
    }
    let mut doc = Document::new().unwrap();
    let (w, h) = (64u32, 32u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for p in rgba.chunks_mut(4) {
        p[0] = 200;
        p[1] = 30;
        p[2] = 30;
        p[3] = 255;
    }
    doc.add_image_page(&rgba, w, h, 96.0).unwrap();
    let s = doc.page_size(0).unwrap();
    assert!((s.width_pt - 48.0).abs() < 0.5 && (s.height_pt - 24.0).abs() < 0.5);
    let img = doc.render_page_scaled(0, 1.0).unwrap();
    assert!(img.rgba[0] > 150 && img.rgba[1] < 80, "картинка не отрисовалась: {:?}", &img.rgba[..4]);

    let font = std::fs::read(r"C:\Windows\Fonts\arial.ttf").unwrap();
    let long = "Кириллица и latin. ".repeat(400);
    doc.add_text_pages(&long, &font, 11.0).unwrap();
    assert!(doc.page_count() >= 3, "текст должен занять >1 страницы: {}", doc.page_count());
    let p = doc.load_page(1).unwrap();
    assert!(p.text().contains("Кириллица"));
    let out = std::env::temp_dir().join("pdfsmith_text_test.pdf");
    doc.save_to(&out).unwrap();
}
