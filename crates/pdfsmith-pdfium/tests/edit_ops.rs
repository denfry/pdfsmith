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

/// Регрессия «после рисования страница белая»: штрих → закрыть страницу →
/// загрузить заново → рендер должен содержать и исходный текст, и штрих.
#[test]
fn ink_survives_page_reload_and_keeps_content() {
    let _g = serial();
    let Some(pdf) = sample() else { return };
    let doc = Document::open(&pdf, None).unwrap();
    let count_dark = |img: &pdfsmith_pdfium::RenderedImage| img.rgba.chunks(4).filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128).count();
    let count_red = |img: &pdfsmith_pdfium::RenderedImage| img.rgba.chunks(4).filter(|p| p[0] > 150 && p[1] < 90 && p[2] < 90).count();

    let page = doc.load_page(0).unwrap();
    let (w, h) = ((page.width_pt()) as i32, (page.height_pt()) as i32);
    let before = page.render_region(1.0, 0, 0, w, h, 0, true).unwrap();
    let pts: Vec<(f32, f32)> = [(60.0, 60.0), (160.0, 120.0), (260.0, 80.0)]
        .iter()
        .map(|&(x, y)| page.display_to_pdf(x, y))
        .collect();
    page.add_ink(&pts, Rgba(220, 30, 30, 255), 3.0).unwrap();
    drop(page);

    let page = doc.load_page(0).unwrap();
    let after = page.render_region(1.0, 0, 0, w, h, 0, true).unwrap();
    eprintln!("dark before={} after={} red after={}", count_dark(&before), count_dark(&after), count_red(&after));
    assert!(count_dark(&after) * 2 > count_dark(&before), "исходное содержимое пропало после штриха");
    assert!(count_red(&after) > 50, "штрих не отрисовался");
}

#[test]
fn text_objects_edit_add_move_remove_survive_save() {
    use pdfsmith_pdfium::TextChange;
    let _g = serial();
    let Some(pdf) = sample() else { return };
    let doc = Document::open(&pdf, None).unwrap();
    let page = doc.load_page(0).unwrap();
    let objs = page.text_objects();
    assert!(!objs.is_empty(), "в text.pdf должны быть текстовые объекты");
    for o in &objs {
        eprintln!("#{} {:?} {:.1}pt {} b={} i={}", o.index, o.text, o.size, o.font, o.bold, o.italic);
    }

    // Перестановка своих же символов — остаётся исходный шрифт.
    let a = objs.iter().find(|o| o.text.chars().count() >= 4).unwrap().clone();
    let rev: String = a.text.chars().rev().collect();
    assert_eq!(doc.replace_text(&page, a.index, &a.text, &rev).unwrap(), TextChange::KeptFont);

    // Новые символы — системный шрифт, тот же объект по индексу.
    let objs = page.text_objects();
    let b = objs.iter().find(|o| o.index != a.index && o.text.chars().count() >= 2).unwrap().clone();
    let fresh = "Ёжик ЪЩ 0123 qwz";
    assert!(matches!(doc.replace_text(&page, b.index, &b.text, fresh).unwrap(), TextChange::NewFont | TextChange::KeptFont));
    let after = page.text_objects();
    let nb = after.iter().find(|o| o.text == fresh).expect("новый текст на месте");
    assert!((nb.rect.left - b.rect.left).abs() < 2.0, "{} vs {}", nb.rect.left, b.rect.left);
    assert!((nb.size - b.size).abs() < 0.1);

    // Устаревший expect отклоняется.
    assert!(doc.replace_text(&page, nb.index, "не то", "x").is_err());

    // Перенос.
    page.move_object(nb.index, 10.0, -5.0).unwrap();
    let moved = page.text_objects().into_iter().find(|o| o.text == fresh).unwrap();
    assert!((moved.rect.left - nb.rect.left - 10.0).abs() < 0.5);

    // Новый текст и удаление.
    doc.add_text(&page, (72.0, 72.0), (1.0, 0.0), (0.0, 1.0), "Добавлено pdfsmith", 14.0, Rgba(200, 0, 0, 255)).unwrap();
    let c = objs.iter().find(|o| o.index != a.index && o.index != b.index).unwrap().clone();
    let c_now = page.text_objects().into_iter().find(|o| o.text == c.text).unwrap();
    assert_eq!(doc.replace_text(&page, c_now.index, &c_now.text, "  ").unwrap(), TextChange::Removed);

    // Всё переживает сохранение.
    let bytes = doc.to_bytes().unwrap();
    drop(page);
    drop(doc);
    let back = Document::from_bytes(bytes, None).unwrap();
    let texts: Vec<String> = back.load_page(0).unwrap().text_objects().into_iter().map(|o| o.text).collect();
    assert!(texts.iter().any(|t| t.trim() == rev.trim()), "{texts:?}");
    assert!(texts.iter().any(|t| t == fresh));
    assert!(texts.iter().any(|t| t == "Добавлено pdfsmith"));
    let all = back.load_page(0).unwrap().text();
    assert!(all.contains("Добавлено pdfsmith"));
}
