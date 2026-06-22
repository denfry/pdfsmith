# Текстовый слой (поиск + выделение/копирование) — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Добавить текстовый слой: поиск по документу (Ctrl+F) с подсветкой и переходом к совпадениям и выделение текста мышью с копированием.

**Architecture:** pdfium-крейт однократно извлекает текст страницы (символы + боксы в PDF-координатах) и кэширует его. Чистый движок (`pdfsmith-engine`) делает всю логику без зависимостей: перевод координат PDF→page-point (`geom`) и поиск по символам (`search`). Поток рендера (единственный владелец pdfium) гоняет инкрементальный поиск с отменой и шлёт в UI готовые прямоугольники в page-point пространстве; `main.rs` рисует подсветку/выделение и копирует через буфер обмена egui.

**Tech Stack:** Rust 2021, `pdfium-render 0.9.1` (низкоуровневые `FPDFText_*`), `eframe`/`egui 0.30` (wgpu), без новых зависимостей.

## Global Constraints

- Все вызовы `FPDF*`/`FPDFText_*` — **только** в рендер-потоке (`render_thread.rs`); pdfium не потокобезопасен.
- `pdfsmith-engine` остаётся **чистым**: без `egui`, без `pdfium`. Новые модули `geom`/`search` оперируют простыми типами (`f32`, `Vec`).
- **Никаких новых зависимостей.** Копирование в буфер — `ctx.copy_text(String)` (egui 0.30).
- Координатные соглашения: PDF — origin внизу-слева, ось Y вверх, пункты, без поворота. page-point — origin вверху-слева, ось Y вниз, пункты, размер = `page_pt()` (для поворотов 1/3 ширина/высота меняются местами). Повороты согласованы с матрицами `render_region` (rotation=1 — 90° по часовой). Экран = `view.offset + p_page_pt * view.zoom`.
- pdfium-зависимые тесты используют существующий паттерн `serial()` + `ensure()`/skip из `crates/pdfsmith-pdfium/tests/render_real_pdf.rs`; требуют `pdfium.dll` в корне workspace и файлов в `test_pdfs/`.
- Чистые тесты (`geom`, `search`) не требуют ни pdfium, ни файлов.
- Коммиты на русском, каждый завершается строкой `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Ветка работы: `feat/text-layer-search-select` (уже создана).

## File Structure

- Create `crates/pdfsmith-pdfium/src/text.rs` — `TextPage` (RAII над `FPDFText_LoadPage`), `PageText { chars, boxes }`, `RectPt`, `Page::text()`, `extract()`. Единственная задача: вытащить символы и их боксы из pdfium.
- Modify `crates/pdfsmith-pdfium/src/lib.rs` — объявить `pub mod text;`.
- Create `crates/pdfsmith-pdfium/tests/text_layer.rs` — интеграционные тесты извлечения + диагностика наличия текстового слоя.
- Create `crates/pdfsmith-engine/src/geom.rs` — `PdfRect`, `PagePtRect`, `pdf_point_to_page_pt`, `pdf_rect_to_page_pt`, `union`. Чистая геометрия.
- Create `crates/pdfsmith-engine/src/search.rs` — `TextMatch`, `search_in_chars`, `search_page_order`. Чистый поиск.
- Modify `crates/pdfsmith-engine/src/lib.rs` — объявить `pub mod geom;` и `pub mod search;`.
- Modify `crates/pdfsmith-app/src/render_thread.rs` — новые `Job::Search`/`Job::PageText`, события поиска/текста, кэш текста, `build_matches`, инкрементальный поиск.
- Modify `crates/pdfsmith-app/src/main.rs` — состояние и UI поиска, подсветка, переход; режим «Текст», выделение, копирование, курсор.

---

## Task 1: pdfium — извлечение текста (`text.rs`)

**Files:**
- Create: `crates/pdfsmith-pdfium/src/text.rs`
- Modify: `crates/pdfsmith-pdfium/src/lib.rs` (добавить `pub mod text;`)
- Test: `crates/pdfsmith-pdfium/tests/text_layer.rs`

**Interfaces:**
- Consumes: `crate::bindings()` (приватный, доступен дочернему модулю), `crate::Page` (поле `handle: FPDF_PAGE`), `crate::PdfError`.
- Produces:
  - `pub struct RectPt { pub left: f32, pub bottom: f32, pub right: f32, pub top: f32 }` (PDF-координаты).
  - `pub struct PageText { pub chars: Vec<char>, pub boxes: Vec<RectPt> }` (длины равны).
  - `pub struct TextPage<'a>` с методами `char_count(&self) -> i32`, `extract(&self) -> PageText`.
  - `impl Page { pub fn text(&self) -> Result<TextPage<'_>, PdfError> }`.

- [ ] **Step 1: Диагностика наличия текстового слоя (выясняем выполнимость на реальных файлах)**

Создать файл `crates/pdfsmith-pdfium/tests/text_layer.rs` со следующим содержимым (пока только диагностика и хелперы):

```rust
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
```

- [ ] **Step 2: Запустить диагностику, убедиться что код ещё не компилируется**

Run: `cargo test -p pdfsmith-pdfium --test text_layer text_layer_inventory -- --ignored --nocapture`
Expected: FAIL — компиляция падает («module `text` not found» / нет `Page::text`). Это ожидаемо: модуль ещё не написан.

> **КОНТРОЛЬНАЯ ТОЧКА выполнимости.** После Step 6 эта диагностика должна показать
> хотя бы один файл с `симв. (стр.0) > 0`. Если у ВСЕХ файлов `0` — у чертежей нет
> текстового слоя (текст «выжжен» в кривые), и подпроект №1 на этих файлах
> бесполезен без OCR (№6). В этом случае — ОСТАНОВИТЬСЯ и сообщить пользователю:
> возможно, порядок карты надо поменять (OCR раньше). Не продолжать вслепую.

- [ ] **Step 3: Написать модуль `text.rs`**

Создать `crates/pdfsmith-pdfium/src/text.rs`:

```rust
//! Текстовый слой: однократное извлечение символов и их боксов через
//! `FPDFText_*`. Координаты — в PDF-пространстве страницы (origin внизу-слева,
//! ось Y вверх, пункты). Перевод в экран — задача `pdfsmith_engine::geom`.
//!
//! Поиск и hit-test НЕ здесь: они реализованы как чистая логика над извлечённым
//! `PageText` в `pdfsmith-engine` (тестируются без pdfium, дружат с кэшем).

use std::marker::PhantomData;
use std::os::raw::c_double;

use pdfium_render::prelude::*;

use crate::{bindings, Page, PdfError};

/// Прямоугольник в PDF-координатах страницы (пункты, origin внизу-слева).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectPt {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Извлечённый текст страницы. Индексы `chars` и `boxes` совпадают с индексами
/// символов pdfium (для BMP-текста — кириллица/латиница — длины равны).
#[derive(Debug, Default, Clone)]
pub struct PageText {
    pub chars: Vec<char>,
    pub boxes: Vec<RectPt>,
}

/// Текстовая страница (RAII над `FPDFText_LoadPage`). Не должна пережить `Page`.
pub struct TextPage<'a> {
    handle: FPDF_TEXTPAGE,
    _page: PhantomData<&'a Page>,
}

impl Page {
    /// Открывает текстовый слой. Требует уже загруженной `Page`.
    pub fn text(&self) -> Result<TextPage<'_>, PdfError> {
        let b = bindings();
        let handle = unsafe { b.FPDFText_LoadPage(self.handle) };
        if handle.is_null() {
            return Err(PdfError::Render("не удалось открыть текстовый слой страницы".into()));
        }
        Ok(TextPage { handle, _page: PhantomData })
    }
}

impl<'a> TextPage<'a> {
    /// Число символов на странице (>= 0).
    pub fn char_count(&self) -> i32 {
        unsafe { bindings().FPDFText_CountChars(self.handle) }.max(0)
    }

    /// Извлекает символы и их боксы. `chars.len() == boxes.len()`.
    pub fn extract(&self) -> PageText {
        let b = bindings();
        let n = self.char_count();
        let mut chars = Vec::with_capacity(n as usize);
        let mut boxes = Vec::with_capacity(n as usize);
        for i in 0..n {
            let u = unsafe { b.FPDFText_GetUnicode(self.handle, i) };
            chars.push(char::from_u32(u).unwrap_or('\u{FFFD}'));
            boxes.push(self.char_box(i));
        }
        PageText { chars, boxes }
    }

    /// Бокс одного символа (PDF-координаты); при неудаче — нулевой прямоугольник,
    /// чтобы длины `chars`/`boxes` совпадали.
    fn char_box(&self, index: i32) -> RectPt {
        let b = bindings();
        let (mut l, mut r, mut bot, mut t) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
        let ok = unsafe { b.FPDFText_GetCharBox(self.handle, index, &mut l, &mut r, &mut bot, &mut t) };
        if b.is_true(ok) {
            RectPt { left: l as f32, bottom: bot as f32, right: r as f32, top: t as f32 }
        } else {
            RectPt { left: 0.0, bottom: 0.0, right: 0.0, top: 0.0 }
        }
    }
}

impl<'a> Drop for TextPage<'a> {
    fn drop(&mut self) {
        unsafe { bindings().FPDFText_ClosePage(self.handle) };
    }
}

// `c_double` импортирован для аргументов-указателей FPDFText_GetCharBox.
const _: fn() = || {
    let _x: c_double = 0.0;
};
```

> Примечание: последний `const _` — заглушка, гарантирующая использование импорта
> `c_double` (он фигурирует только в локальных переменных через вывод типа).
> Если компилятор не ругается на неиспользуемый импорт — этот блок можно убрать.
> Проще: заменить объявления на `let mut l: c_double = 0.0;` и удалить заглушку.
> Используйте явный тип у переменных и удалите `const _`-блок:
>
> ```rust
> let (mut l, mut r, mut bot, mut t): (c_double, c_double, c_double, c_double) =
>     (0.0, 0.0, 0.0, 0.0);
> ```

- [ ] **Step 4: Подключить модуль в `lib.rs`**

В `crates/pdfsmith-pdfium/src/lib.rs` после строки `use pdfium_render::prelude::*;` (строка 17) добавить объявление модуля:

```rust
pub mod text;
```

- [ ] **Step 5: Дописать в тест проверку извлечения**

Добавить в конец `crates/pdfsmith-pdfium/tests/text_layer.rs`:

```rust
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

    // Боксы лежат в пределах страницы (с допуском на округление).
    let size = doc.page_size(idx).expect("размер страницы");
    for r in pt.boxes.iter().filter(|r| r.right > r.left || r.top > r.bottom) {
        assert!(r.left >= -1.0 && r.right <= size.width_pt + 1.0, "бокс по X вне страницы: {r:?}");
        assert!(r.bottom >= -1.0 && r.top <= size.height_pt + 1.0, "бокс по Y вне страницы: {r:?}");
    }
}
```

- [ ] **Step 6: Запустить тесты извлечения и диагностику**

Run: `cargo test -p pdfsmith-pdfium --test text_layer -- --nocapture`
Expected: PASS (`extract_returns_aligned_chars_and_boxes`), либо корректный ПРОПУСК с понятным сообщением.

Run (выполнимость): `cargo test -p pdfsmith-pdfium --test text_layer text_layer_inventory -- --ignored --nocapture`
Expected: в выводе хотя бы один файл с `симв. (стр.0) > 0`. **Если везде 0 — см. контрольную точку в Step 2 и остановиться.**

- [ ] **Step 7: Commit**

```bash
git add crates/pdfsmith-pdfium/src/text.rs crates/pdfsmith-pdfium/src/lib.rs crates/pdfsmith-pdfium/tests/text_layer.rs
git commit -m "$(cat <<'EOF'
feat(pdfium): извлечение текстового слоя (FPDFText), символы + боксы

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: движок — координатный трансформ (`geom.rs`)

**Files:**
- Create: `crates/pdfsmith-engine/src/geom.rs`
- Modify: `crates/pdfsmith-engine/src/lib.rs` (добавить `pub mod geom;`)

**Interfaces:**
- Consumes: ничего (чистый модуль).
- Produces:
  - `pub struct PdfRect { pub left: f32, pub bottom: f32, pub right: f32, pub top: f32 }`
  - `pub struct PagePtRect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }`
  - `pub fn pdf_point_to_page_pt(x_pt: f32, y_pt: f32, page_w: f32, page_h: f32, rotation: u8) -> (f32, f32)`
  - `pub fn pdf_rect_to_page_pt(r: PdfRect, page_w: f32, page_h: f32, rotation: u8) -> PagePtRect`
  - `pub fn union(rects: &[PagePtRect]) -> Option<PagePtRect>`

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/pdfsmith-engine/src/geom.rs` сразу с тестами (реализация — следующим шагом):

```rust
//! Перевод координат текстового слоя PDF в "повёрнутое page-point" пространство.
//!
//! PDF: origin внизу-слева, ось Y вверх, пункты, БЕЗ поворота.
//! page-point: origin вверху-слева, ось Y вниз, пункты, размер = page_pt()
//! (для поворотов 1/3 ширина/высота меняются местами).
//! Повороты согласованы с матрицами `render_region` (rotation=1 — 90° по часовой).

/// Прямоугольник в PDF-координатах (пункты, origin внизу-слева).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfRect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Прямоугольник в page-point пространстве (пункты, origin вверху-слева, y вниз).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagePtRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Страница 200×100 pt. Бокс символа: left=10, bottom=20, right=30, top=40.
    const W: f32 = 200.0;
    const H: f32 = 100.0;
    fn r() -> PdfRect {
        PdfRect { left: 10.0, bottom: 20.0, right: 30.0, top: 40.0 }
    }

    #[test]
    fn rot0_flips_y_only() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 0),
            PagePtRect { x: 10.0, y: 60.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot1_is_90cw() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 1),
            PagePtRect { x: 20.0, y: 10.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot2_is_180() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 2),
            PagePtRect { x: 170.0, y: 20.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn rot3_is_270cw() {
        assert_eq!(
            pdf_rect_to_page_pt(r(), W, H, 3),
            PagePtRect { x: 60.0, y: 170.0, w: 20.0, h: 20.0 }
        );
    }

    #[test]
    fn point_corner_maps_per_rotation() {
        // PDF нижне-левый угол (0,0) при rot0 → верхне-левый по X, низ по Y.
        assert_eq!(pdf_point_to_page_pt(0.0, 0.0, W, H, 0), (0.0, 100.0));
        assert_eq!(pdf_point_to_page_pt(0.0, 0.0, W, H, 1), (0.0, 0.0));
    }

    #[test]
    fn union_covers_all() {
        let a = PagePtRect { x: 10.0, y: 60.0, w: 20.0, h: 20.0 };
        let b = PagePtRect { x: 30.0, y: 60.0, w: 20.0, h: 20.0 };
        assert_eq!(union(&[a, b]), Some(PagePtRect { x: 10.0, y: 60.0, w: 40.0, h: 20.0 }));
        assert_eq!(union(&[]), None);
    }
}
```

- [ ] **Step 2: Подключить модуль и запустить — тесты падают (нет функций)**

В `crates/pdfsmith-engine/src/lib.rs` добавить после `pub mod disk_cache;` (строка 7):

```rust
pub mod geom;
```

Run: `cargo test -p pdfsmith-engine geom`
Expected: FAIL — `cannot find function pdf_rect_to_page_pt` и т.д.

- [ ] **Step 3: Реализовать функции**

Добавить в `crates/pdfsmith-engine/src/geom.rs` перед `#[cfg(test)]`:

```rust
/// Переводит точку PDF (x_pt, y_pt) в page-point пространство для поворота.
/// `page_w`/`page_h` — размер НЕповёрнутой страницы в пунктах.
pub fn pdf_point_to_page_pt(x_pt: f32, y_pt: f32, page_w: f32, page_h: f32, rotation: u8) -> (f32, f32) {
    match rotation & 3 {
        0 => (x_pt, page_h - y_pt),
        1 => (y_pt, x_pt),
        2 => (page_w - x_pt, y_pt),
        _ => (page_h - y_pt, page_w - x_pt),
    }
}

/// Переводит PDF-прямоугольник в page-point прямоугольник для поворота.
/// Переводит два противоположных угла и нормализует (повороты на 90° меняют,
/// какой угол минимальный).
pub fn pdf_rect_to_page_pt(r: PdfRect, page_w: f32, page_h: f32, rotation: u8) -> PagePtRect {
    let (ax, ay) = pdf_point_to_page_pt(r.left, r.bottom, page_w, page_h, rotation);
    let (bx, by) = pdf_point_to_page_pt(r.right, r.top, page_w, page_h, rotation);
    PagePtRect {
        x: ax.min(bx),
        y: ay.min(by),
        w: (ax - bx).abs(),
        h: (ay - by).abs(),
    }
}

/// Объединяющий прямоугольник набора page-point прямоугольников.
pub fn union(rects: &[PagePtRect]) -> Option<PagePtRect> {
    let first = rects.first()?;
    let mut min_x = first.x;
    let mut min_y = first.y;
    let mut max_x = first.x + first.w;
    let mut max_y = first.y + first.h;
    for r in &rects[1..] {
        min_x = min_x.min(r.x);
        min_y = min_y.min(r.y);
        max_x = max_x.max(r.x + r.w);
        max_y = max_y.max(r.y + r.h);
    }
    Some(PagePtRect { x: min_x, y: min_y, w: max_x - min_x, h: max_y - min_y })
}
```

- [ ] **Step 4: Запустить тесты — проходят**

Run: `cargo test -p pdfsmith-engine geom`
Expected: PASS (6 тестов).

- [ ] **Step 5: Commit**

```bash
git add crates/pdfsmith-engine/src/geom.rs crates/pdfsmith-engine/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(engine): geom — перевод PDF-координат в page-point (все 4 поворота)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: движок — поиск по тексту (`search.rs`)

**Files:**
- Create: `crates/pdfsmith-engine/src/search.rs`
- Modify: `crates/pdfsmith-engine/src/lib.rs` (добавить `pub mod search;`)

**Interfaces:**
- Consumes: ничего.
- Produces:
  - `pub struct TextMatch { pub start: usize, pub len: usize }`
  - `pub fn search_in_chars(text: &[char], query: &str, match_case: bool, whole_word: bool) -> Vec<TextMatch>`
  - `pub fn search_page_order(start: usize, total: usize) -> Vec<usize>`

- [ ] **Step 1: Написать падающие тесты**

Создать `crates/pdfsmith-engine/src/search.rs`:

```rust
//! Поиск по извлечённому тексту страницы (чистый, без pdfium). Работает над
//! `&[char]` (как в `pdfsmith_pdfium::text::PageText.chars`); индексы совпадений —
//! это индексы символов, по которым берутся боксы для подсветки.

/// Совпадение: позиция и длина в символах.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMatch {
    pub start: usize,
    pub len: usize,
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
        // "cat" встречается отдельным словом (8) и внутри "scatter" (1).
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
```

- [ ] **Step 2: Подключить модуль и запустить — падает**

В `crates/pdfsmith-engine/src/lib.rs` добавить:

```rust
pub mod search;
```

Run: `cargo test -p pdfsmith-engine search`
Expected: FAIL — нет `search_in_chars`/`search_page_order`.

- [ ] **Step 3: Реализовать поиск**

Добавить в `crates/pdfsmith-engine/src/search.rs` перед `#[cfg(test)]`:

```rust
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
```

- [ ] **Step 4: Запустить тесты — проходят**

Run: `cargo test -p pdfsmith-engine search`
Expected: PASS (8 тестов).

- [ ] **Step 5: Commit**

```bash
git add crates/pdfsmith-engine/src/search.rs crates/pdfsmith-engine/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(engine): search — поиск по символам (регистр, целое слово, перекрытия)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: рендер-поток — задания поиска/текста, кэш, инкрементальный поиск

**Files:**
- Modify: `crates/pdfsmith-app/src/render_thread.rs`

**Interfaces:**
- Consumes: `pdfsmith_pdfium::text::{PageText, RectPt}` и `Page::text`; `pdfsmith_engine::geom::{PdfRect, pdf_rect_to_page_pt, union}`; `pdfsmith_engine::search::{search_in_chars, search_page_order}`.
- Produces (для UI в `main.rs`):
  - `pub struct FindOpts { pub match_case: bool, pub whole_word: bool }`
  - `pub struct MatchPt { pub rect: egui::Rect, pub start: i32, pub count: i32 }` (rect в page-point пространстве)
  - Варианты `Job::Search { query: String, opts: FindOpts, start_page: usize, rotation: u8, generation: u64 }` и `Job::PageText { page: usize, rotation: u8 }`.
  - Варианты `Event::SearchPage { generation: u64, page: usize, matches: Vec<MatchPt> }`, `Event::SearchProgress { generation: u64, scanned: usize, total: usize }`, `Event::SearchDone { generation: u64, total_matches: usize }`, `Event::PageText { page: usize, rotation: u8, chars: Vec<char>, boxes: Vec<egui::Rect> }`.
  - `pub(crate) fn build_matches(pt: &PageText, query: &str, opts: FindOpts, page_w: f32, page_h: f32, rotation: u8) -> Vec<MatchPt>`

- [ ] **Step 1: Тест на чистую функцию `build_matches` (без pdfium)**

`PageText` имеет публичные поля, поэтому его можно собрать в тесте без pdfium. Добавить в конец `crates/pdfsmith-app/src/render_thread.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pdfsmith_pdfium::text::{PageText, RectPt};

    #[test]
    fn build_matches_maps_query_to_pagepoint_rect() {
        // Страница 200×100. Символы "ab" с боксами рядом по горизонтали.
        let pt = PageText {
            chars: vec!['a', 'b'],
            boxes: vec![
                RectPt { left: 10.0, bottom: 20.0, right: 20.0, top: 40.0 },
                RectPt { left: 20.0, bottom: 20.0, right: 30.0, top: 40.0 },
            ],
        };
        let opts = FindOpts { match_case: false, whole_word: false };
        let m = build_matches(&pt, "ab", opts, 200.0, 100.0, 0);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].start, 0);
        assert_eq!(m[0].count, 2);
        // rot0: y = H - top .. H - bottom = 60..80; x = 10..30.
        assert_eq!(m[0].rect.min.x, 10.0);
        assert_eq!(m[0].rect.min.y, 60.0);
        assert_eq!(m[0].rect.width(), 20.0);
        assert_eq!(m[0].rect.height(), 20.0);
    }

    #[test]
    fn build_matches_empty_when_absent() {
        let pt = PageText {
            chars: vec!['a', 'b'],
            boxes: vec![
                RectPt { left: 10.0, bottom: 20.0, right: 20.0, top: 40.0 },
                RectPt { left: 20.0, bottom: 20.0, right: 30.0, top: 40.0 },
            ],
        };
        let opts = FindOpts { match_case: false, whole_word: false };
        assert!(build_matches(&pt, "zzz", opts, 200.0, 100.0, 0).is_empty());
    }
}
```

- [ ] **Step 2: Запустить — падает (нет типов/функции)**

Run: `cargo test -p pdfsmith-app build_matches`
Expected: FAIL — нет `FindOpts`/`MatchPt`/`build_matches`.

- [ ] **Step 3: Расширить импорты и типы в `render_thread.rs`**

В начале `crates/pdfsmith-app/src/render_thread.rs` после строки `use eframe::egui;` (строка 13) добавить:

```rust
use std::collections::HashMap;

use pdfsmith_engine::geom::{pdf_rect_to_page_pt, union, PdfRect};
use pdfsmith_engine::search::{search_in_chars, search_page_order};
use pdfsmith_pdfium::text::PageText;
```

Заменить блок `use pdfsmith_pdfium::{init, Document, Page, RenderedImage};` (строка 15) — он уже импортирует нужное; оставить как есть.

Добавить рядом с объявлением `pub enum Job` (перед ним) новые публичные типы:

```rust
/// Опции поиска, задаваемые из UI.
#[derive(Debug, Clone, Copy, Default)]
pub struct FindOpts {
    pub match_case: bool,
    pub whole_word: bool,
}

/// Одно совпадение в page-point пространстве (для подсветки и перехода).
#[derive(Debug, Clone, Copy)]
pub struct MatchPt {
    pub rect: egui::Rect,
    pub start: i32,
    pub count: i32,
}
```

- [ ] **Step 4: Добавить варианты `Job` и `Event`**

В `pub enum Job { ... }` добавить варианты (после `Tiles { ... }`):

```rust
    /// Инкрементальный поиск по документу. `generation` отсекает устаревшие
    /// результаты на стороне UI.
    Search {
        query: String,
        opts: FindOpts,
        start_page: usize,
        rotation: u8,
        generation: u64,
    },
    /// Извлечение текста страницы (символы + боксы в page-point) для выделения.
    PageText { page: usize, rotation: u8 },
```

В `pub enum Event { ... }` добавить варианты (после `Error(String)`):

```rust
    SearchPage { generation: u64, page: usize, matches: Vec<MatchPt> },
    SearchProgress { generation: u64, scanned: usize, total: usize },
    SearchDone { generation: u64, total_matches: usize },
    PageText { page: usize, rotation: u8, chars: Vec<char>, boxes: Vec<egui::Rect> },
```

- [ ] **Step 5: Реализовать `build_matches` и хелпер конверсии**

Добавить в `render_thread.rs` на уровне модуля (например, после функции `cache_root`):

```rust
/// Переводит PDF-бокс из `pdfsmith_pdfium` в тип геометрии движка.
fn to_pdf_rect(b: &pdfsmith_pdfium::text::RectPt) -> PdfRect {
    PdfRect { left: b.left, bottom: b.bottom, right: b.right, top: b.top }
}

/// Находит совпадения запроса в извлечённом тексте и переводит их в page-point
/// прямоугольники (по одному объединённому прямоугольнику на совпадение).
pub(crate) fn build_matches(
    pt: &PageText,
    query: &str,
    opts: FindOpts,
    page_w: f32,
    page_h: f32,
    rotation: u8,
) -> Vec<MatchPt> {
    let hits = search_in_chars(&pt.chars, query, opts.match_case, opts.whole_word);
    let mut out = Vec::with_capacity(hits.len());
    for m in hits {
        let end = m.start + m.len;
        let Some(slice) = pt.boxes.get(m.start..end) else { continue };
        let pp: Vec<_> = slice
            .iter()
            .map(|b| pdf_rect_to_page_pt(to_pdf_rect(b), page_w, page_h, rotation))
            .collect();
        if let Some(u) = union(&pp) {
            out.push(MatchPt {
                rect: egui::Rect::from_min_size(egui::pos2(u.x, u.y), egui::vec2(u.w, u.h)),
                start: m.start as i32,
                count: m.len as i32,
            });
        }
    }
    out
}
```

- [ ] **Step 6: Запустить юнит-тесты `build_matches` — проходят**

Run: `cargo test -p pdfsmith-app build_matches`
Expected: PASS (2 теста).

- [ ] **Step 7: Добавить в `Worker` кэш текста и обработку новых заданий**

В `struct Worker { ... }` добавить поле:

```rust
    text_cache: HashMap<usize, PageText>,
```

В инициализации `let mut w = Worker { ... };` (в функции `worker`) добавить `text_cache: HashMap::new(),`.

В `match job { ... }` главного цикла добавить ветки (рядом с существующими):

```rust
        Job::PageText { page, rotation } => w.page_text(page, rotation),
        Job::Search { query, opts, start_page, rotation, generation } => {
            pending = w.search(&job_rx, query, opts, start_page, rotation, generation);
        }
```

- [ ] **Step 8: Реализовать методы `Worker::page_text`, `Worker::ensure_text`, `Worker::search`**

Добавить в `impl Worker { ... }`:

```rust
    /// Гарантирует, что текст страницы извлечён и лежит в кэше.
    fn ensure_text(&mut self, page: usize) -> Result<(), String> {
        if self.text_cache.contains_key(&page) {
            return Ok(());
        }
        self.ensure_loaded(page)?;
        let pg = &self.loaded.as_ref().expect("страница загружена").1;
        let tp = pg.text().map_err(|e| e.to_string())?;
        let pt = tp.extract();
        self.text_cache.insert(page, pt);
        Ok(())
    }

    /// Извлекает текст страницы и шлёт боксы в page-point пространстве (для
    /// выделения/копирования в UI).
    fn page_text(&mut self, page: usize, rotation: u8) {
        let size = match self.doc.as_ref().and_then(|d| d.page_size(page)) {
            Some(s) => s,
            None => return,
        };
        if let Err(e) = self.ensure_text(page) {
            self.emit(Event::Error(e));
            return;
        }
        let pt = self.text_cache.get(&page).expect("в кэше");
        let chars = pt.chars.clone();
        let boxes: Vec<egui::Rect> = pt
            .boxes
            .iter()
            .map(|b| {
                let r = pdf_rect_to_page_pt(to_pdf_rect(b), size.width_pt, size.height_pt, rotation);
                egui::Rect::from_min_size(egui::pos2(r.x, r.y), egui::vec2(r.w, r.h))
            })
            .collect();
        self.emit(Event::PageText { page, rotation, chars, boxes });
    }

    /// Инкрементальный поиск: текущая страница первой, остальные по кругу.
    /// Между страницами проверяет отмену (новый Job). Возвращает `Some(job)`,
    /// если пришёл более свежий запрос.
    #[allow(clippy::too_many_arguments)]
    fn search(
        &mut self,
        job_rx: &Receiver<Job>,
        query: String,
        opts: FindOpts,
        start_page: usize,
        rotation: u8,
        generation: u64,
    ) -> Option<Job> {
        let total = self.doc.as_ref().map(|d| d.page_count()).unwrap_or(0);
        let order = search_page_order(start_page, total);
        let mut total_matches = 0usize;
        for (scanned, page) in order.into_iter().enumerate() {
            if let Ok(newer) = job_rx.try_recv() {
                return Some(newer);
            }
            let size = match self.doc.as_ref().and_then(|d| d.page_size(page)) {
                Some(s) => s,
                None => continue,
            };
            if let Err(e) = self.ensure_text(page) {
                self.emit(Event::Error(e));
                continue;
            }
            let pt = self.text_cache.get(&page).expect("в кэше");
            let matches = build_matches(pt, &query, opts, size.width_pt, size.height_pt, rotation);
            total_matches += matches.len();
            if !matches.is_empty() {
                self.emit(Event::SearchPage { generation, page, matches });
            }
            self.emit(Event::SearchProgress { generation, scanned: scanned + 1, total });
        }
        self.emit(Event::SearchDone { generation, total_matches });
        None
    }
```

Также: при открытии нового документа очищать кэш текста. В методе `Worker::open`, рядом с `self.loaded = None;` добавить:

```rust
        self.text_cache.clear();
```

- [ ] **Step 9: Сборка и регрессионные тесты**

Run: `cargo build -p pdfsmith-app`
Expected: успешная сборка (первая сборка долгая).

Run: `cargo test -p pdfsmith-app`
Expected: PASS (включая `build_matches*`).

Run: `cargo clippy -p pdfsmith-app`
Expected: без ошибок (предупреждения допустимы, новых грубых — нет).

- [ ] **Step 10: Commit**

```bash
git add crates/pdfsmith-app/src/render_thread.rs
git commit -m "$(cat <<'EOF'
feat(app): рендер-поток — инкрементальный поиск, кэш текста, PageText

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: UI — панель поиска, подсветка, переход

**Files:**
- Modify: `crates/pdfsmith-app/src/main.rs`

**Interfaces:**
- Consumes: `render_thread::{Job, Event, FindOpts, MatchPt}`.
- Produces: внутреннее состояние `ViewerApp` (`struct MatchHit`), без внешнего API.

- [ ] **Step 1: Подготовить запуск приложения для ручной проверки**

`cargo run` берёт exe из `target/debug`, а `pdfium.dll` лежит в корне — скопировать её рядом с бинарём (однократно):

```bash
cp pdfium.dll target/debug/pdfium.dll
```

Run (дымовая проверка текущей сборки): `cargo run -p pdfsmith-app -- "test_pdfs/2025 СХЕМА Разводки 1-го кон. 21.08.pdf"`
Expected: окно открывается, чертёж рендерится (как сейчас). Закрыть окно.

> Используйте файл, который в Task 1 (диагностика) показал `симв. > 0`. Если у
> «СХЕМА Разводки» текста нет — подставьте тот, где он есть.

- [ ] **Step 2: Добавить состояние поиска в `ViewerApp`**

В `crates/pdfsmith-app/src/main.rs` рядом с импортами добавить тип-хит и импорт:

```rust
use render_thread::{FindOpts, MatchPt};
```

(Строку `use render_thread::{spawn, Event, Job, RenderHandle, TILE};` дополнить так, чтобы импортировались также `FindOpts`, `MatchPt` — либо отдельной строкой выше.)

Перед `struct ViewerApp` добавить:

```rust
/// Совпадение поиска: страница + прямоугольник в page-point + диапазон символов.
#[derive(Clone, Copy)]
struct MatchHit {
    page: usize,
    rect: egui::Rect,
    #[allow(dead_code)]
    start: i32,
    #[allow(dead_code)]
    count: i32,
}
```

В `struct ViewerApp { ... }` добавить поля:

```rust
    search_open: bool,
    search_query: String,
    search_opts: FindOpts,
    search_generation: u64,
    search_hits: Vec<MatchHit>,
    search_active: Option<usize>,
    search_scanning: bool,
    search_scanned: usize,
    search_total: usize,
    search_focus: bool,
```

В конструкторе `ViewerApp { ... }` (в `fn new`) добавить инициализацию:

```rust
            search_open: false,
            search_query: String::new(),
            search_opts: FindOpts::default(),
            search_generation: 0,
            search_hits: Vec::new(),
            search_active: None,
            search_scanning: false,
            search_scanned: 0,
            search_total: 0,
            search_focus: false,
```

- [ ] **Step 3: Обрабатывать события поиска в `poll_events`**

В `fn poll_events`, в `match ev { ... }` добавить ветки (рядом с существующими):

```rust
                Event::SearchPage { generation, page, matches } => {
                    if generation == self.search_generation {
                        for m in matches {
                            self.search_hits.push(MatchHit {
                                page,
                                rect: m.rect,
                                start: m.start,
                                count: m.count,
                            });
                        }
                        if self.search_active.is_none() && !self.search_hits.is_empty() {
                            self.search_active = Some(0);
                            self.pending_jump = true;
                        }
                    }
                }
                Event::SearchProgress { generation, scanned, total } => {
                    if generation == self.search_generation {
                        self.search_scanned = scanned;
                        self.search_total = total;
                    }
                }
                Event::SearchDone { generation, .. } => {
                    if generation == self.search_generation {
                        self.search_scanning = false;
                    }
                }
                Event::PageText { .. } => { /* используется в Task 6 */ }
```

Добавить в `struct ViewerApp` ещё одно поле для отложенного перехода и инициализировать его `false` в конструкторе:

```rust
    pending_jump: bool,
```

- [ ] **Step 4: Метод запуска поиска и навигации**

Добавить в `impl ViewerApp` методы:

```rust
    /// Запускает новый поиск (сбрасывает прежние результаты).
    fn start_search(&mut self) {
        self.search_generation = self.search_generation.wrapping_add(1);
        self.search_hits.clear();
        self.search_active = None;
        self.search_scanned = 0;
        self.search_total = self.page_count();
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search_scanning = false;
            return;
        }
        self.search_scanning = true;
        let _ = self.handle.job_tx.send(Job::Search {
            query: q,
            opts: self.search_opts,
            start_page: self.page,
            rotation: self.rotation,
            generation: self.search_generation,
        });
    }

    /// Переходит к совпадению по индексу (со сменой страницы и центрированием).
    fn goto_hit(&mut self, idx: usize) {
        if idx >= self.search_hits.len() {
            return;
        }
        self.search_active = Some(idx);
        let hit = self.search_hits[idx];
        if hit.page != self.page {
            self.set_page(hit.page);
        }
        self.pending_jump = true;
    }

    /// Сдвигает активное совпадение на `delta` (с заворотом).
    fn step_hit(&mut self, delta: i32) {
        let n = self.search_hits.len();
        if n == 0 {
            return;
        }
        let cur = self.search_active.unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(n as i32) as usize;
        self.goto_hit(next);
    }

    /// Центрирует вид на прямоугольнике (page-point) активного совпадения.
    fn center_on_rect(&mut self, rect: egui::Rect, canvas: egui::Rect) {
        let center_pt = rect.center().to_vec2();
        self.view.offset = canvas.center().to_vec2() - center_pt * self.view.zoom;
    }
```

- [ ] **Step 5: Нарисовать панель поиска и подсветку**

Добавить в `impl ViewerApp` метод панели:

```rust
    fn search_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("🔍");
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .desired_width(220.0)
                    .hint_text("Поиск (Ctrl+F)"),
            );
            if self.search_focus {
                resp.request_focus();
                self.search_focus = false;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if resp.changed() {
                self.start_search();
            }
            if enter {
                // Enter — следующее совпадение (или новый поиск, если пусто).
                if self.search_hits.is_empty() {
                    self.start_search();
                } else {
                    let shift = ui.input(|i| i.modifiers.shift);
                    self.step_hit(if shift { -1 } else { 1 });
                }
                resp.request_focus();
            }
            if ui.button("▲").on_hover_text("Предыдущее (Shift+Enter)").clicked() {
                self.step_hit(-1);
            }
            if ui.button("▼").on_hover_text("Следующее (Enter)").clicked() {
                self.step_hit(1);
            }
            if ui.toggle_value(&mut self.search_opts.match_case, "Aa").on_hover_text("Учитывать регистр").changed() {
                self.start_search();
            }
            if ui.toggle_value(&mut self.search_opts.whole_word, "|w|").on_hover_text("Целое слово").changed() {
                self.start_search();
            }
            let label = if self.search_hits.is_empty() {
                if self.search_query.trim().is_empty() { String::new() } else { "нет совпадений".to_string() }
            } else {
                format!("{}/{}", self.search_active.map(|i| i + 1).unwrap_or(0), self.search_hits.len())
            };
            ui.label(label);
            if self.search_scanning {
                ui.spinner();
                ui.label(format!("{}/{}", self.search_scanned, self.search_total));
            }
            if ui.button("✕").on_hover_text("Закрыть (Esc)").clicked() {
                self.close_search();
            }
        });
    }

    fn close_search(&mut self) {
        self.search_open = false;
        self.search_hits.clear();
        self.search_active = None;
        self.search_scanning = false;
        self.search_generation = self.search_generation.wrapping_add(1); // отсечь хвост
    }

    /// Подсветка совпадений на текущей странице.
    fn draw_search_highlights(&self, painter: &egui::Painter) {
        for (i, hit) in self.search_hits.iter().enumerate() {
            if hit.page != self.page {
                continue;
            }
            let screen = egui::Rect::from_min_size(
                (self.view.offset + hit.rect.min.to_vec2() * self.view.zoom).to_pos2(),
                hit.rect.size() * self.view.zoom,
            );
            let active = self.search_active == Some(i);
            let color = if active {
                egui::Color32::from_rgba_unmultiplied(255, 165, 0, 140)
            } else {
                egui::Color32::from_rgba_unmultiplied(255, 230, 0, 70)
            };
            painter.rect_filled(screen, 1.0, color);
        }
    }
```

- [ ] **Step 6: Подключить панель, горячие клавиши и отложенный переход в `update`**

В `fn update` после строки `egui::TopBottomPanel::top("toolbar").show(...)` добавить условную панель поиска:

```rust
        if self.search_open {
            egui::TopBottomPanel::top("search").show(ctx, |ui| self.search_bar(ui));
        }
```

В `fn handle_keyboard`, внутри `ctx.input(|i| { ... })`, добавить открытие/закрытие:

```rust
            if i.modifiers.command && i.key_pressed(egui::Key::F) {
                self.search_open = true;
                self.search_focus = true;
            }
            if i.key_pressed(egui::Key::Escape) && self.search_open {
                // close_search вызывается ниже, вне замыкания ввода
                self.pending_close_search = true;
            }
```

Добавить в `struct ViewerApp` поле `pending_close_search: bool` (инициализировать `false`). После вызова `self.handle_keyboard(ctx);` в `update` обработать:

```rust
        if self.pending_close_search {
            self.pending_close_search = false;
            self.close_search();
        }
```

В `CentralPanel`, в самом конце замыкания (после `self.draw_minimap(...)`) добавить отрисовку подсветки и отложенный переход:

```rust
            self.draw_search_highlights(&painter);
            if self.pending_jump {
                self.pending_jump = false;
                if let Some(idx) = self.search_active {
                    if idx < self.search_hits.len() {
                        let rect = self.search_hits[idx].rect;
                        self.center_on_rect(rect, canvas);
                    }
                }
            }
```

- [ ] **Step 7: Перезапуск поиска при повороте**

В методе `rotate` (после `self.last_request = None;`) добавить переотправку поиска (боксы зависят от поворота):

```rust
        if self.search_open && !self.search_query.trim().is_empty() {
            self.start_search();
        }
```

- [ ] **Step 8: Сборка**

Run: `cargo build -p pdfsmith-app`
Expected: успешная сборка.

Run: `cargo clippy -p pdfsmith-app`
Expected: без ошибок.

- [ ] **Step 9: Ручная проверка**

Run: `cargo run -p pdfsmith-app -- "test_pdfs/<файл с текстом>.pdf"`

Проверить по чек-листу:
- Ctrl+F открывает панель, фокус в поле.
- Ввод запроса показывает «N/M» и подсвечивает совпадения жёлтым; активное — оранжевым.
- Enter/▼ — следующее, Shift+Enter/▲ — предыдущее; вид центрируется на активном (в т.ч. со сменой страницы на многостраничнике).
- Подсветка ложится ТОЧНО на текст при разном зуме и после поворотов `[`/`]`.
- Тумблеры «Aa» и «|w|» меняют результат.
- Esc закрывает панель и убирает подсветку.
- На файле без текста — «нет совпадений», без зависаний.

- [ ] **Step 10: Commit**

```bash
git add crates/pdfsmith-app/src/main.rs
git commit -m "$(cat <<'EOF'
feat(app): UI поиска — панель Ctrl+F, подсветка, переход к совпадению

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: UI — выделение текста, копирование, режим «Текст», курсор

**Files:**
- Modify: `crates/pdfsmith-app/src/main.rs`

**Interfaces:**
- Consumes: `Event::PageText { page, rotation, chars, boxes }` (Task 4), `Job::PageText`.
- Produces: внутреннее состояние выделения.

- [ ] **Step 1: Добавить режим инструмента и состояние выделения**

Перед `struct ViewerApp` добавить enum режима:

```rust
#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Hand,
    Text,
}
```

В `struct ViewerApp` добавить поля:

```rust
    tool: Tool,
    page_chars: Vec<char>,
    page_boxes: Vec<egui::Rect>,      // page-point боксы символов текущей страницы/поворота
    page_text_for: Option<(usize, u8)>,
    page_text_requested: Option<(usize, u8)>,
    sel_anchor: Option<usize>,
    sel_cursor: Option<usize>,
```

Инициализация в конструкторе:

```rust
            tool: Tool::Hand,
            page_chars: Vec::new(),
            page_boxes: Vec::new(),
            page_text_for: None,
            page_text_requested: None,
            sel_anchor: None,
            sel_cursor: None,
```

- [ ] **Step 2: Принять событие `PageText`**

Заменить заглушку `Event::PageText { .. } => { ... }` в `poll_events` на:

```rust
                Event::PageText { page, rotation, chars, boxes } => {
                    if page == self.page && rotation == self.rotation {
                        self.page_chars = chars;
                        self.page_boxes = boxes;
                        self.page_text_for = Some((page, rotation));
                    }
                }
```

- [ ] **Step 3: Запрос текста текущей страницы (по образцу `request_thumbnail`)**

Добавить в `impl ViewerApp`:

```rust
    /// Запрашивает текст текущей страницы/поворота для выделения, если ещё нет.
    fn request_page_text(&mut self) {
        if !self.opened {
            return;
        }
        let want = (self.page, self.rotation);
        if self.page_text_for == Some(want) || self.page_text_requested == Some(want) {
            return;
        }
        self.page_text_requested = Some(want);
        // сбросить прежнее выделение при смене страницы/поворота
        self.sel_anchor = None;
        self.sel_cursor = None;
        self.page_boxes.clear();
        self.page_chars.clear();
        self.page_text_for = None;
        let _ = self.handle.job_tx.send(Job::PageText { page: self.page, rotation: self.rotation });
    }
```

Вызвать его в `update` рядом с `self.request_thumbnail();`:

```rust
        self.request_page_text();
```

- [ ] **Step 4: Хелперы выделения (hit-test, диапазон, текст)**

Добавить в `impl ViewerApp`:

```rust
    /// Индекс символа под точкой в page-point координатах (внутри бокса, иначе
    /// ближайший по центру в пределах разумного допуска).
    fn char_at_pagept(&self, p: egui::Vec2) -> Option<usize> {
        let pt = p.to_pos2();
        // 1) точное попадание
        for (i, b) in self.page_boxes.iter().enumerate() {
            if b.contains(pt) {
                return Some(i);
            }
        }
        // 2) ближайший центр (для кликов между строк/символов)
        let mut best: Option<(usize, f32)> = None;
        for (i, b) in self.page_boxes.iter().enumerate() {
            let d = (b.center() - pt).length_sq();
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Текущий выделенный диапазон символов [min, max] включительно.
    fn selection_range(&self) -> Option<(usize, usize)> {
        match (self.sel_anchor, self.sel_cursor) {
            (Some(a), Some(c)) => Some((a.min(c), a.max(c))),
            _ => None,
        }
    }

    /// Собирает текст выделения для копирования.
    fn selection_text(&self) -> String {
        match self.selection_range() {
            Some((a, b)) if b < self.page_chars.len() => self.page_chars[a..=b].iter().collect(),
            _ => String::new(),
        }
    }
```

- [ ] **Step 5: Кнопка режима в тулбаре**

В `fn toolbar`, после блока поворота (после кнопок `⟲`/`⟳` и `ui.separator();`), добавить переключатель инструмента:

```rust
            ui.separator();
            ui.selectable_value(&mut self.tool, Tool::Hand, "✋").on_hover_text("Рука: панорама");
            ui.selectable_value(&mut self.tool, Tool::Text, "🆃").on_hover_text("Текст: выделение");
```

- [ ] **Step 6: Развести панораму и выделение в `handle_pan_zoom`**

Заменить тело `fn handle_pan_zoom` на разведение по режиму (зум колесом остаётся всегда):

```rust
    fn handle_pan_zoom(&mut self, ui: &egui::Ui, response: &egui::Response) {
        match self.tool {
            Tool::Hand => {
                if response.dragged() {
                    self.view.offset += response.drag_delta();
                }
            }
            Tool::Text => {
                if response.drag_started() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let p = pos.to_vec2() - self.view.offset;
                        let pp = p / self.view.zoom;
                        self.sel_anchor = self.char_at_pagept(pp);
                        self.sel_cursor = self.sel_anchor;
                    }
                }
                if response.dragged() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let p = pos.to_vec2() - self.view.offset;
                        let pp = p / self.view.zoom;
                        self.sel_cursor = self.char_at_pagept(pp);
                    }
                }
            }
        }
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            if let Some(cursor) = response.hover_pos() {
                self.zoom_around((scroll * 0.0015).exp(), cursor.to_vec2());
            }
        }
    }
```

- [ ] **Step 7: Рисование выделения, копирование, курсор**

Добавить метод отрисовки выделения в `impl ViewerApp`:

```rust
    fn draw_selection(&self, painter: &egui::Painter) {
        let Some((a, b)) = self.selection_range() else { return };
        let color = egui::Color32::from_rgba_unmultiplied(80, 160, 255, 90);
        for i in a..=b.min(self.page_boxes.len().saturating_sub(1)) {
            let r = self.page_boxes[i];
            if r.width() <= 0.0 && r.height() <= 0.0 {
                continue;
            }
            let screen = egui::Rect::from_min_size(
                (self.view.offset + r.min.to_vec2() * self.view.zoom).to_pos2(),
                r.size() * self.view.zoom,
            );
            painter.rect_filled(screen, 0.0, color);
        }
    }
```

В `CentralPanel` (после `self.draw_search_highlights(&painter);`) добавить:

```rust
            self.draw_selection(&painter);

            // Курсор «текст» в режиме выделения над страницей.
            if self.tool == Tool::Text && response.hovered() {
                ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Text);
            }

            // Копирование выделения.
            if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::C)) {
                let text = self.selection_text();
                if !text.is_empty() {
                    ui.ctx().copy_text(text);
                    self.status_msg = Some("Скопировано".into());
                }
            }

            // Контекстное меню «Копировать».
            response.context_menu(|ui| {
                let has = self.selection_range().is_some();
                if ui.add_enabled(has, egui::Button::new("Копировать")).clicked() {
                    let text = self.selection_text();
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                    }
                    ui.close_menu();
                }
            });
```

- [ ] **Step 8: Сборка и clippy**

Run: `cargo build -p pdfsmith-app`
Expected: успешная сборка.

Run: `cargo clippy -p pdfsmith-app`
Expected: без ошибок.

- [ ] **Step 9: Ручная проверка**

Run: `cargo run -p pdfsmith-app -- "test_pdfs/<файл с текстом>.pdf"`

Чек-лист:
- Переключение «✋»/«🆃» работает; в режиме «🆃» курсор над страницей — I-beam.
- В режиме «Текст» протягивание мышью выделяет текст (синяя заливка ложится на символы); в режиме «Рука» — панорама как прежде.
- Ctrl+C копирует выделенный текст (проверить вставкой в блокнот; кириллица копируется корректно).
- Контекстное меню (правый клик) → «Копировать».
- Колесо мыши масштабирует в обоих режимах.
- Смена страницы/поворота сбрасывает выделение; текст подгружается для новой страницы.

- [ ] **Step 10: Commit**

```bash
git add crates/pdfsmith-app/src/main.rs
git commit -m "$(cat <<'EOF'
feat(app): выделение текста и копирование, режим «Текст», I-beam курсор

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Финальная проверка подпроекта №1

- [ ] Все тесты: `cargo test` (workspace) — PASS или корректные пропуски.
- [ ] Критерии готовности из спека `2026-06-22-text-layer-search-select-design.md` выполнены (поиск N/M и переход; точность подсветки при зуме и всех поворотах; выделение+копирование, включая кириллицу; внятное поведение на странице без текста; неблокирующий поиск с прогрессом и отменой).
- [ ] Обновить статус в `docs/superpowers/2026-06-22-master-roadmap.md`: подпроект №1 → «готов», №2 → «следующий».
- [ ] Влить ветку `feat/text-layer-search-select` согласно навыку `superpowers:finishing-a-development-branch`.

## Self-Review (выполнено при написании плана)

- **Покрытие спека:** извлечение текста — Task 1; координатный трансформ (риск) — Task 2 (юнит-тесты всех поворотов) + проверка выравнивания в ручном чек-листе Task 5; поиск (инкрементальный, отмена, кэш) — Task 3 (логика) + Task 4 (поток); UI поиска/счётчик/переход — Task 5; выделение/копирование/курсор — Task 6; поведение на скане — Task 1 (контрольная точка) + чек-листы. Отличие от спека: поиск/hit-test реализованы как чистая Rust-логика над извлечённым текстом, а не через `FPDFText_FindStart` — это осознанное упрощение (тестируемость без pdfium, мгновенный повторный поиск, отсутствие повторной загрузки страниц); требования спека при этом выполнены.
- **Заглушки:** в коде шагов их нет; единственная «диагностика-заглушка» `const _` в Task 1 снабжена прямой заменой (явный тип переменных) — использовать замену.
- **Согласованность типов:** `FindOpts`/`MatchPt`/`PageText`/`RectPt`/`PdfRect`/`PagePtRect` и сигнатуры `build_matches`/`search_in_chars`/`pdf_rect_to_page_pt` совпадают между задачами 1–6.
