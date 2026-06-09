//! Интеграционные тесты против реальных чертежей из `test_pdfs`.
//!
//! Требуют `pdfium.dll` в корне workspace и файлов в `test_pdfs`. При их
//! отсутствии (чужая машина) тесты помечаются пропущенными.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use pdfsmith_pdfium::{init, Document};

/// PDFium нельзя вызывать из нескольких потоков одновременно. Тестовый харнесс
/// гоняет тесты параллельно, поэтому сериализуем их через общий guard.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn light_pdf() -> PathBuf {
    workspace_root()
        .join("test_pdfs")
        .join("2025 СХЕМА Разводки 1-го кон. 21.08.pdf")
}

/// Готовит библиотеку и проверяет наличие файла; `None` → тест пропускается.
fn ensure(pdf: &PathBuf) -> bool {
    let root = workspace_root();
    let dll = root.join("pdfium.dll");
    if !dll.exists() || !pdf.exists() {
        eprintln!("ПРОПУСК: нет {} или {}", dll.display(), pdf.display());
        return false;
    }
    init(Some(&root)).expect("привязка к pdfium.dll");
    true
}

#[test]
fn opens_and_renders_a_real_schematic() {
    let _g = serial();
    let pdf = light_pdf();
    if !ensure(&pdf) {
        return;
    }

    let doc = Document::open(&pdf, None).expect("открытие PDF");
    assert!(doc.page_count() >= 1);

    let size = doc.page_size(0).expect("размер первой страницы");
    assert!(size.width_pt > 0.0 && size.height_pt > 0.0);

    let scale = 0.25;
    let img = doc.render_page_scaled(0, scale).expect("рендер страницы");
    assert!(img.width >= 1 && img.height >= 1);
    assert_eq!(img.rgba.len(), (img.width * img.height * 4) as usize);

    let expected_w = (size.width_pt * scale).round() as i64;
    assert!((img.width as i64 - expected_w).abs() <= 1);
}

/// Корректность матрицы тайла: рендер куска должен совпадать с обрезкой полного
/// рендера той же страницы в том же масштабе. Сглаживание выключено для
/// детерминированного сравнения.
#[test]
fn tile_matches_crop_of_full_render() {
    let _g = serial();
    let pdf = light_pdf();
    if !ensure(&pdf) {
        return;
    }

    let doc = Document::open(&pdf, None).expect("открытие PDF");
    let size = doc.page_size(0).expect("размер страницы");
    let scale = 1.0_f32;
    let w_full = (size.width_pt * scale).round() as i32;
    let h_full = (size.height_pt * scale).round() as i32;

    let page = doc.load_page(0).expect("загрузка страницы");
    let full = page
        .render_region(scale, 0, 0, w_full, h_full, 0, false)
        .expect("полный рендер");

    // Тайл в глубине страницы (не на краю), чтобы не задеть граничные эффекты.
    let (tw, th) = (96_i32, 96_i32);
    let tx = w_full / 3;
    let ty = h_full / 3;
    let tile = page
        .render_region(scale, tx, ty, tw, th, 0, false)
        .expect("рендер тайла");

    let mut exact = 0u64;
    let mut total = 0u64;
    let mut maxdiff = 0i32;
    for row in 0..th {
        for x in 0..tw {
            for ch in 0..4 {
                let ti = (((row * tw) + x) * 4 + ch) as usize;
                let fx = tx + x;
                let fy = ty + row;
                let fi = (((fy * w_full) + fx) * 4 + ch) as usize;
                let t = tile.rgba[ti] as i32;
                let f = full.rgba[fi] as i32;
                total += 1;
                if t == f {
                    exact += 1;
                }
                maxdiff = maxdiff.max((t - f).abs());
            }
        }
    }

    let frac = exact as f64 / total as f64;
    eprintln!("совпадение тайла с обрезкой: {:.4}, maxdiff {}", frac, maxdiff);
    // Неверная матрица дала бы ~случайное совпадение; верная — почти точное.
    assert!(
        frac > 0.98,
        "тайл не совпал с обрезкой полного рендера: {:.4} точных, maxdiff {}",
        frac,
        maxdiff
    );
}

/// Консистентность тайлинга при повороте 90°: тайл повёрнутой страницы должен
/// совпадать с обрезкой полного повёрнутого рендера.
#[test]
fn rotated_tile_matches_crop_of_rotated_full_render() {
    let _g = serial();
    let pdf = light_pdf();
    if !ensure(&pdf) {
        return;
    }

    let doc = Document::open(&pdf, None).expect("открытие PDF");
    let size = doc.page_size(0).expect("размер страницы");
    let scale = 1.0_f32;
    // При повороте 90° device-размеры меняются местами.
    let rot_w = (size.height_pt * scale).round() as i32;
    let rot_h = (size.width_pt * scale).round() as i32;

    let page = doc.load_page(0).expect("загрузка страницы");
    let full = page
        .render_region(scale, 0, 0, rot_w, rot_h, 1, false)
        .expect("полный повёрнутый рендер");

    let (tw, th) = (96_i32, 96_i32);
    let tx = rot_w / 3;
    let ty = rot_h / 3;
    let tile = page
        .render_region(scale, tx, ty, tw, th, 1, false)
        .expect("повёрнутый тайл");

    let mut exact = 0u64;
    let mut total = 0u64;
    for row in 0..th {
        for x in 0..tw {
            for ch in 0..4 {
                let ti = (((row * tw) + x) * 4 + ch) as usize;
                let fi = ((((ty + row) * rot_w) + (tx + x)) * 4 + ch) as usize;
                total += 1;
                if tile.rgba[ti] == full.rgba[fi] {
                    exact += 1;
                }
            }
        }
    }
    let frac = exact as f64 / total as f64;
    assert!(frac > 0.98, "повёрнутый тайл не совпал с обрезкой: {:.4}", frac);
}

/// Проверяет путь экспорта: рендер страницы в RGBA → кодирование PNG → файл →
/// перечитывание и сверка размеров. Это ядро функции «Экспорт в PNG».
#[test]
fn renders_page_to_valid_png() {
    let _g = serial();
    let pdf = light_pdf();
    if !ensure(&pdf) {
        return;
    }

    let doc = Document::open(&pdf, None).expect("открытие PDF");
    let scale = 0.5;
    let img = doc.render_page_scaled(0, scale).expect("рендер страницы");

    let out = std::env::temp_dir().join("pdfsmith-export-test.png");
    image::RgbaImage::from_raw(img.width, img.height, img.rgba)
        .expect("буфер RGBA корректен")
        .save(&out)
        .expect("сохранение PNG");

    let decoded = image::open(&out).expect("перечитывание PNG");
    assert_eq!(decoded.width(), img.width);
    assert_eq!(decoded.height(), img.height);
    let _ = std::fs::remove_file(&out);
}

/// Печатает число страниц во всех файлах `test_pdfs`. Диагностика.
#[test]
#[ignore = "диагностика, запускать вручную"]
fn list_page_counts() {
    let _g = serial();
    let dir = workspace_root().join("test_pdfs");
    if !workspace_root().join("pdfium.dll").exists() || !dir.exists() {
        eprintln!("ПРОПУСК");
        return;
    }
    init(Some(&workspace_root())).unwrap();
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "pdf").unwrap_or(false))
        .collect();
    entries.sort();
    for p in entries {
        match Document::open(&p, None) {
            Ok(d) => eprintln!("{:>3} стр.  {}", d.page_count(), p.file_name().unwrap().to_string_lossy()),
            Err(e) => eprintln!("ОШИБКА {e}: {}", p.display()),
        }
    }
}

/// Перф-смоук на самом тяжёлом чертеже. Запуск:
/// `cargo test -p pdfsmith-pdfium --release -- --ignored --nocapture`.
#[test]
#[ignore = "перф-замер, запускать вручную"]
fn render_timing_on_heaviest_schematic() {
    let _g = serial();
    let pdf = workspace_root().join("test_pdfs").join("2026  ЛИВНЕВка 28.02..pdf");
    if !ensure(&pdf) {
        return;
    }

    let t_open = Instant::now();
    let doc = Document::open(&pdf, None).expect("открытие PDF");
    eprintln!(
        "ливнёвка | страниц: {} | открытие: {:.1} ms",
        doc.page_count(),
        t_open.elapsed().as_secs_f64() * 1000.0
    );

    let size = doc.page_size(0).expect("размер");
    let t_load = Instant::now();
    let page = doc.load_page(0).expect("загрузка страницы");
    eprintln!("  загрузка страницы: {:.1} ms", t_load.elapsed().as_secs_f64() * 1000.0);

    // Полный рендер один раз (страница уже загружена).
    let w = (size.width_pt).round() as i32;
    let h = (size.height_pt).round() as i32;
    for i in 0..2 {
        let t = Instant::now();
        let _ = page.render_region(1.0, 0, 0, w, h, 0, true).expect("полный рендер");
        eprintln!("  полный рендер #{}: {:.1} ms", i + 1, t.elapsed().as_secs_f64() * 1000.0);
    }

    // Один тайл 512×512 при зуме 4× — стоимость интерактивного тайла.
    let t = Instant::now();
    let _ = page.render_region(4.0, w * 2, h * 2, 512, 512, 0, true).expect("рендер тайла");
    eprintln!(
        "  тайл 512×512 @scale 4.0: {:.1} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );
}
