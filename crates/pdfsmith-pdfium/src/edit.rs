//! Редактирование и конвертация: операции над структурой документа
//! (удаление/перемещение/импорт страниц, поворот), аннотации, извлечение и
//! поиск текста, сохранение, создание документов из картинок и текста.
//!
//! Всё — сырые вызовы `FPDF_*`; вызывать только из рендер-потока.

use std::ffi::c_void;
use std::io::Write;
use std::os::raw::{c_int, c_ulong, c_ushort};
use std::path::Path;

use pdfium_render::prelude::*;

use crate::{bindings, Document, Page, PdfError, FORMAT_BGRA};

const FPDF_ANNOT_TEXT: c_int = 1;
const FPDF_ANNOT_SQUARE: c_int = 5;
const FPDF_ANNOT_HIGHLIGHT: c_int = 9;
const FPDF_ANNOT_INK: c_int = 15;
const COLORTYPE_COLOR: FPDFANNOT_COLORTYPE = 0;
const COLORTYPE_INTERIOR: FPDFANNOT_COLORTYPE = 1;
const FPDF_FONT_TRUETYPE: c_int = 2;
const FPDF_NO_INCREMENTAL: FPDF_DWORD = 2;
/// Флаг аннотации «печатать» (/F bit 3) — иначе многие просмотрщики её прячут.
const ANNOT_FLAG_PRINT: c_int = 4;

/// Прямоугольник в пунктах PDF-пространства (origin — левый нижний угол).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PdfRect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

/// Цвет RGBA 0..255.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

/// Одно вхождение при поиске: индекс первого символа, прямоугольники, контекст.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub char_index: i32,
    pub rects: Vec<PdfRect>,
    pub snippet: String,
}

// ---------------------------------------------------------------------------
// FPDF_FILEWRITE — свой repr(C)-двойник структуры PDFium с хвостом для writer.

#[repr(C)]
struct FileWrite<'a> {
    version: c_int,
    write_block: Option<unsafe extern "C" fn(*mut FileWrite, *const c_void, c_ulong) -> c_int>,
    writer: &'a mut dyn Write,
    ok: bool,
}

unsafe extern "C" fn write_block(this: *mut FileWrite, buf: *const c_void, size: c_ulong) -> c_int {
    let this = &mut *this;
    let data = std::slice::from_raw_parts(buf as *const u8, size as usize);
    match this.writer.write_all(data) {
        Ok(()) => 1,
        Err(_) => {
            this.ok = false;
            0
        }
    }
}

fn utf16_to_string(buf: &[c_ushort]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

impl Document {
    /// Пустой документ.
    pub fn new() -> Result<Document, PdfError> {
        let handle = unsafe { bindings().FPDF_CreateNewDocument() };
        if handle.is_null() {
            return Err(PdfError::Open("FPDF_CreateNewDocument вернул NULL".into()));
        }
        Ok(Document { handle, _bytes: Vec::new(), page_sizes: Vec::new() })
    }

    /// Перечитывает размеры страниц после структурных изменений.
    pub fn refresh_sizes(&mut self) {
        let b = bindings();
        let count = unsafe { b.FPDF_GetPageCount(self.handle) }.max(0) as usize;
        let mut sizes = Vec::with_capacity(count);
        for i in 0..count {
            let mut size = FS_SIZEF { width: 0.0, height: 0.0 };
            let ok = unsafe { b.FPDF_GetPageSizeByIndexF(self.handle, i as c_int, &mut size) };
            sizes.push(crate::PageSize {
                width_pt: if b.is_true(ok) { size.width } else { 0.0 },
                height_pt: if b.is_true(ok) { size.height } else { 0.0 },
            });
        }
        self.page_sizes = sizes;
    }

    pub fn delete_page(&mut self, index: usize) {
        unsafe { bindings().FPDFPage_Delete(self.handle, index as c_int) };
        self.refresh_sizes();
    }

    /// Переносит страницу `from` на позицию `to` (индексы до операции).
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<(), PdfError> {
        let b = bindings();
        let idx = [from as c_int];
        let ok = unsafe { b.FPDF_MovePages(self.handle, idx.as_ptr(), 1, to as c_int) };
        if !b.is_true(ok) {
            return Err(PdfError::Render("FPDF_MovePages отказал".into()));
        }
        self.refresh_sizes();
        Ok(())
    }

    /// Поворачивает страницу на `quarters` четвертей по часовой (записывается в /Rotate).
    pub fn rotate_page(&mut self, index: usize, quarters: i32) -> Result<(), PdfError> {
        let b = bindings();
        let page = unsafe { b.FPDF_LoadPage(self.handle, index as c_int) };
        if page.is_null() {
            return Err(PdfError::Render(format!("страница {index} не загрузилась")));
        }
        unsafe {
            let cur = b.FPDFPage_GetRotation(page);
            b.FPDFPage_SetRotation(page, (cur + quarters).rem_euclid(4));
            b.FPDF_ClosePage(page);
        }
        self.refresh_sizes();
        Ok(())
    }

    /// Вставляет пустую страницу указанного размера на позицию `at`.
    pub fn insert_blank(&mut self, at: usize, width_pt: f32, height_pt: f32) -> Result<(), PdfError> {
        let b = bindings();
        let page = unsafe {
            b.FPDFPage_New(self.handle, at as c_int, width_pt as f64, height_pt as f64)
        };
        if page.is_null() {
            return Err(PdfError::Render("FPDFPage_New вернул NULL".into()));
        }
        unsafe { b.FPDF_ClosePage(page) };
        self.refresh_sizes();
        Ok(())
    }

    /// Импортирует страницы `indices` (или все, если пусто) из `src` на позицию `at`.
    pub fn import_pages(&mut self, src: &Document, indices: &[usize], at: usize) -> Result<(), PdfError> {
        let b = bindings();
        let idx: Vec<c_int> = if indices.is_empty() {
            (0..src.page_count() as c_int).collect()
        } else {
            indices.iter().map(|&i| i as c_int).collect()
        };
        let ok = unsafe {
            b.FPDF_ImportPagesByIndex(
                self.handle,
                src.handle,
                idx.as_ptr(),
                idx.len() as c_ulong,
                at as c_int,
            )
        };
        if !b.is_true(ok) {
            return Err(PdfError::Render("FPDF_ImportPagesByIndex отказал".into()));
        }
        self.refresh_sizes();
        Ok(())
    }

    /// Новый документ только из указанных страниц.
    pub fn extract(&self, indices: &[usize]) -> Result<Document, PdfError> {
        let mut out = Document::new()?;
        out.import_pages(self, indices, 0)?;
        Ok(out)
    }

    /// Сохраняет копию документа (полная перезапись, не инкрементально).
    pub fn save_to(&self, path: &Path) -> Result<(), PdfError> {
        let mut bytes = Vec::new();
        {
            let mut fw = FileWrite {
                version: 1,
                write_block: Some(write_block),
                writer: &mut bytes,
                ok: true,
            };
            let b = bindings();
            let ok = unsafe {
                b.FPDF_SaveAsCopy(
                    self.handle,
                    &mut fw as *mut FileWrite as *mut FPDF_FILEWRITE,
                    FPDF_NO_INCREMENTAL,
                )
            };
            if !b.is_true(ok) || !fw.ok {
                return Err(PdfError::Render("FPDF_SaveAsCopy отказал".into()));
            }
        }
        // Пишем во временный файл рядом и переименовываем — исходник не
        // повреждается при сбое посреди записи.
        let tmp = path.with_extension("pdf.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| PdfError::Render(format!("запись: {e}")))?;
        std::fs::rename(&tmp, path).map_err(|e| PdfError::Render(format!("переименование: {e}")))?;
        Ok(())
    }

    /// Добавляет страницу с картинкой (RGBA), масштаб `dpi` пикселей на дюйм.
    pub fn add_image_page(&mut self, rgba: &[u8], width: u32, height: u32, dpi: f32) -> Result<(), PdfError> {
        let b = bindings();
        let w_pt = width as f32 * 72.0 / dpi;
        let h_pt = height as f32 * 72.0 / dpi;
        let at = self.page_count();
        let page = unsafe { b.FPDFPage_New(self.handle, at as c_int, w_pt as f64, h_pt as f64) };
        if page.is_null() {
            return Err(PdfError::Render("FPDFPage_New вернул NULL".into()));
        }
        let result = (|| -> Result<(), PdfError> {
            let bitmap = unsafe {
                b.FPDFBitmap_CreateEx(width as c_int, height as c_int, FORMAT_BGRA, std::ptr::null_mut(), 0)
            };
            if bitmap.is_null() {
                return Err(PdfError::Render("битмап для картинки".into()));
            }
            unsafe {
                let stride = b.FPDFBitmap_GetStride(bitmap) as usize;
                let buf = b.FPDFBitmap_GetBuffer(bitmap) as *mut u8;
                for y in 0..height as usize {
                    let dst = std::slice::from_raw_parts_mut(buf.add(y * stride), width as usize * 4);
                    let src = &rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
                    for x in 0..width as usize {
                        // Предумножаем альфу на белый: PDFium ждёт непрозрачный BGRA.
                        let a = src[x * 4 + 3] as u32;
                        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
                        dst[x * 4] = blend(src[x * 4 + 2]);
                        dst[x * 4 + 1] = blend(src[x * 4 + 1]);
                        dst[x * 4 + 2] = blend(src[x * 4]);
                        dst[x * 4 + 3] = 255;
                    }
                }
                let obj = b.FPDFPageObj_NewImageObj(self.handle);
                let mut pg = page;
                let ok = b.FPDFImageObj_SetBitmap(&mut pg, 1, obj, bitmap);
                b.FPDFBitmap_Destroy(bitmap);
                if !b.is_true(ok) {
                    b.FPDFPageObj_Destroy(obj);
                    return Err(PdfError::Render("FPDFImageObj_SetBitmap отказал".into()));
                }
                let m = FS_MATRIX { a: w_pt, b: 0.0, c: 0.0, d: h_pt, e: 0.0, f: 0.0 };
                b.FPDFPageObj_SetMatrix(obj, &m);
                b.FPDFPage_InsertObject(page, obj);
                b.FPDFPage_GenerateContent(page);
            }
            Ok(())
        })();
        unsafe { b.FPDF_ClosePage(page) };
        self.refresh_sizes();
        result
    }

    /// Добавляет страницы A4 с текстом: перенос по словам, TrueType-шрифт
    /// (`font_bytes`, например Arial — нужен для кириллицы).
    pub fn add_text_pages(&mut self, text: &str, font_bytes: &[u8], font_size: f32) -> Result<(), PdfError> {
        let b = bindings();
        let font = unsafe {
            b.FPDFText_LoadFont(self.handle, font_bytes.as_ptr(), font_bytes.len() as u32, FPDF_FONT_TRUETYPE, 1)
        };
        if font.is_null() {
            return Err(PdfError::Render("не удалось загрузить шрифт".into()));
        }
        const PAGE_W: f32 = 595.0;
        const PAGE_H: f32 = 842.0;
        const MARGIN: f32 = 56.0;
        let line_h = font_size * 1.35;
        let max_w = PAGE_W - 2.0 * MARGIN;

        // Ширина строки — через временный текстовый объект и его bounds.
        let measure = |s: &str| -> f32 {
            unsafe {
                let obj = b.FPDFPageObj_CreateTextObj(self.handle, font, font_size);
                if obj.is_null() {
                    return s.chars().count() as f32 * font_size * 0.5;
                }
                b.FPDFText_SetText_str(obj, s);
                let (mut l, mut bt, mut r, mut t) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                b.FPDFPageObj_GetBounds(obj, &mut l, &mut bt, &mut r, &mut t);
                b.FPDFPageObj_Destroy(obj);
                r - l
            }
        };

        let mut lines: Vec<String> = Vec::new();
        for para in text.lines() {
            let para = para.trim_end();
            if para.is_empty() {
                lines.push(String::new());
                continue;
            }
            let mut cur = String::new();
            for word in para.split_whitespace() {
                let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                if measure(&cand) <= max_w || cur.is_empty() {
                    cur = cand;
                } else {
                    lines.push(std::mem::replace(&mut cur, word.to_string()));
                }
            }
            lines.push(cur);
        }
        if lines.is_empty() {
            lines.push(String::new());
        }

        let per_page = ((PAGE_H - 2.0 * MARGIN) / line_h).floor().max(1.0) as usize;
        for chunk in lines.chunks(per_page) {
            let at = self.page_count();
            let page = unsafe { b.FPDFPage_New(self.handle, at as c_int, PAGE_W as f64, PAGE_H as f64) };
            if page.is_null() {
                unsafe { b.FPDFFont_Close(font) };
                return Err(PdfError::Render("FPDFPage_New вернул NULL".into()));
            }
            let mut y = PAGE_H - MARGIN - font_size;
            for line in chunk {
                if !line.is_empty() {
                    unsafe {
                        let obj = b.FPDFPageObj_CreateTextObj(self.handle, font, font_size);
                        b.FPDFText_SetText_str(obj, line);
                        b.FPDFPageObj_Transform(obj, 1.0, 0.0, 0.0, 1.0, MARGIN as f64, y as f64);
                        b.FPDFPage_InsertObject(page, obj);
                    }
                }
                y -= line_h;
            }
            unsafe {
                b.FPDFPage_GenerateContent(page);
                b.FPDF_ClosePage(page);
            }
            self.refresh_sizes();
        }
        unsafe { b.FPDFFont_Close(font) };
        Ok(())
    }
}

impl Page {
    /// Переводит координаты «отображаемой» страницы (origin слева-сверху,
    /// пункты, как в раскладке просмотрщика при повороте 0) в PDF-пространство.
    pub fn display_to_pdf(&self, x: f32, y: f32) -> (f32, f32) {
        // Размеры в десятых долях пункта — целочисленный API, нужна точность.
        const K: f32 = 10.0;
        let b = bindings();
        let (mut px, mut py) = (0.0f64, 0.0f64);
        unsafe {
            b.FPDF_DeviceToPage(
                self.handle,
                0,
                0,
                (self.width_pt * K).round() as c_int,
                (self.height_pt * K).round() as c_int,
                0,
                (x * K).round() as c_int,
                (y * K).round() as c_int,
                &mut px,
                &mut py,
            );
        }
        (px as f32, py as f32)
    }

    /// Обратное преобразование: PDF-пространство → отображаемые пункты.
    pub fn pdf_to_display(&self, x: f32, y: f32) -> (f32, f32) {
        const K: f32 = 10.0;
        let b = bindings();
        let (mut dx, mut dy) = (0 as c_int, 0 as c_int);
        unsafe {
            b.FPDF_PageToDevice(
                self.handle,
                0,
                0,
                (self.width_pt * K).round() as c_int,
                (self.height_pt * K).round() as c_int,
                0,
                x as f64,
                y as f64,
                &mut dx,
                &mut dy,
            );
        }
        (dx as f32 / K, dy as f32 / K)
    }

    /// Весь текст страницы.
    pub fn text(&self) -> String {
        let b = bindings();
        unsafe {
            let tp = b.FPDFText_LoadPage(self.handle);
            if tp.is_null() {
                return String::new();
            }
            let n = b.FPDFText_CountChars(tp).max(0);
            let mut buf = vec![0 as c_ushort; n as usize + 1];
            b.FPDFText_GetText(tp, 0, n, buf.as_mut_ptr());
            b.FPDFText_ClosePage(tp);
            utf16_to_string(&buf)
        }
    }

    /// Текст внутри прямоугольника (PDF-пространство).
    pub fn text_in_rect(&self, r: PdfRect) -> String {
        let b = bindings();
        unsafe {
            let tp = b.FPDFText_LoadPage(self.handle);
            if tp.is_null() {
                return String::new();
            }
            let (l, t, rr, bt) = (r.left as f64, r.top as f64, r.right as f64, r.bottom as f64);
            let n = b.FPDFText_GetBoundedText(tp, l, t, rr, bt, std::ptr::null_mut(), 0);
            let mut buf = vec![0 as c_ushort; n.max(0) as usize + 1];
            b.FPDFText_GetBoundedText(tp, l, t, rr, bt, buf.as_mut_ptr(), buf.len() as c_int);
            b.FPDFText_ClosePage(tp);
            utf16_to_string(&buf)
        }
    }

    /// Ищет все вхождения `query` (без учёта регистра).
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let b = bindings();
        let mut hits = Vec::new();
        if query.is_empty() {
            return hits;
        }
        unsafe {
            let tp = b.FPDFText_LoadPage(self.handle);
            if tp.is_null() {
                return hits;
            }
            let total = b.FPDFText_CountChars(tp).max(0);
            let sh = b.FPDFText_FindStart_str(tp, query, 0, 0);
            if !sh.is_null() {
                while b.is_true(b.FPDFText_FindNext(sh)) {
                    let idx = b.FPDFText_GetSchResultIndex(sh);
                    let cnt = b.FPDFText_GetSchCount(sh);
                    let nrects = b.FPDFText_CountRects(tp, idx, cnt);
                    let mut rects = Vec::new();
                    for i in 0..nrects {
                        let (mut l, mut t, mut r, mut bt) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                        if b.is_true(b.FPDFText_GetRect(tp, i, &mut l, &mut t, &mut r, &mut bt)) {
                            rects.push(PdfRect {
                                left: l as f32,
                                bottom: bt as f32,
                                right: r as f32,
                                top: t as f32,
                            });
                        }
                    }
                    let s = (idx - 28).max(0);
                    let e = (idx + cnt + 28).min(total);
                    let mut buf = vec![0 as c_ushort; (e - s) as usize + 1];
                    b.FPDFText_GetText(tp, s, e - s, buf.as_mut_ptr());
                    let snippet = utf16_to_string(&buf)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ");
                    hits.push(SearchHit { char_index: idx, rects, snippet });
                    if hits.len() > 2000 {
                        break;
                    }
                }
                b.FPDFText_FindClose(sh);
            }
            b.FPDFText_ClosePage(tp);
        }
        hits
    }

    pub fn annotation_count(&self) -> usize {
        unsafe { bindings().FPDFPage_GetAnnotCount(self.handle) }.max(0) as usize
    }

    /// Удаляет последнюю аннотацию (простой undo).
    pub fn remove_last_annotation(&self) -> bool {
        let b = bindings();
        let n = self.annotation_count();
        if n == 0 {
            return false;
        }
        b.is_true(unsafe { b.FPDFPage_RemoveAnnot(self.handle, (n - 1) as c_int) })
    }

    /// Рукописный штрих (Ink), точки в PDF-пространстве.
    pub fn add_ink(&self, points: &[(f32, f32)], color: Rgba, width: f32) -> Result<(), PdfError> {
        if points.len() < 2 {
            return Ok(());
        }
        let b = bindings();
        unsafe {
            let a = b.FPDFPage_CreateAnnot(self.handle, FPDF_ANNOT_INK);
            if a.is_null() {
                return Err(PdfError::Render("FPDFPage_CreateAnnot(Ink)".into()));
            }
            let pts: Vec<FS_POINTF> = points.iter().map(|&(x, y)| FS_POINTF { x, y }).collect();
            b.FPDFAnnot_AddInkStroke(a, pts.as_ptr(), pts.len());
            b.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, color.0 as u32, color.1 as u32, color.2 as u32, color.3 as u32);
            b.FPDFAnnot_SetBorder(a, 0.0, 0.0, width);
            let rect = bounds(points, width);
            b.FPDFAnnot_SetRect(a, &rect);
            b.FPDFAnnot_SetFlags(a, ANNOT_FLAG_PRINT);
            b.FPDFPage_CloseAnnot(a);
        }
        Ok(())
    }

    /// Прямоугольная рамка (Square).
    pub fn add_rect(&self, r: PdfRect, color: Rgba, width: f32) -> Result<(), PdfError> {
        let b = bindings();
        unsafe {
            let a = b.FPDFPage_CreateAnnot(self.handle, FPDF_ANNOT_SQUARE);
            if a.is_null() {
                return Err(PdfError::Render("FPDFPage_CreateAnnot(Square)".into()));
            }
            b.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, color.0 as u32, color.1 as u32, color.2 as u32, color.3 as u32);
            b.FPDFAnnot_SetBorder(a, 0.0, 0.0, width);
            b.FPDFAnnot_SetRect(a, &to_fs(r));
            b.FPDFAnnot_SetFlags(a, ANNOT_FLAG_PRINT);
            b.FPDFPage_CloseAnnot(a);
        }
        Ok(())
    }

    /// Полупрозрачное выделение (Highlight) по прямоугольнику.
    pub fn add_highlight(&self, r: PdfRect, color: Rgba) -> Result<(), PdfError> {
        let b = bindings();
        unsafe {
            let a = b.FPDFPage_CreateAnnot(self.handle, FPDF_ANNOT_HIGHLIGHT);
            if a.is_null() {
                return Err(PdfError::Render("FPDFPage_CreateAnnot(Highlight)".into()));
            }
            b.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, color.0 as u32, color.1 as u32, color.2 as u32, color.3 as u32);
            let q = FS_QUADPOINTSF {
                x1: r.left,
                y1: r.top,
                x2: r.right,
                y2: r.top,
                x3: r.left,
                y3: r.bottom,
                x4: r.right,
                y4: r.bottom,
            };
            b.FPDFAnnot_AppendAttachmentPoints(a, &q);
            b.FPDFAnnot_SetRect(a, &to_fs(r));
            b.FPDFAnnot_SetFlags(a, ANNOT_FLAG_PRINT);
            b.FPDFPage_CloseAnnot(a);
        }
        Ok(())
    }

    /// Заметка (Text/«стикер») в точке с содержимым.
    pub fn add_note(&self, x: f32, y: f32, contents: &str, color: Rgba) -> Result<(), PdfError> {
        let b = bindings();
        unsafe {
            let a = b.FPDFPage_CreateAnnot(self.handle, FPDF_ANNOT_TEXT);
            if a.is_null() {
                return Err(PdfError::Render("FPDFPage_CreateAnnot(Text)".into()));
            }
            b.FPDFAnnot_SetColor(a, COLORTYPE_COLOR, color.0 as u32, color.1 as u32, color.2 as u32, color.3 as u32);
            b.FPDFAnnot_SetColor(a, COLORTYPE_INTERIOR, color.0 as u32, color.1 as u32, color.2 as u32, color.3 as u32);
            let r = PdfRect { left: x, bottom: y - 20.0, right: x + 20.0, top: y };
            b.FPDFAnnot_SetRect(a, &to_fs(r));
            b.FPDFAnnot_SetStringValue_str(a, "Contents", contents);
            b.FPDFAnnot_SetStringValue_str(a, "T", "pdfsmith");
            b.FPDFAnnot_SetFlags(a, ANNOT_FLAG_PRINT);
            b.FPDFPage_CloseAnnot(a);
        }
        Ok(())
    }
}

fn to_fs(r: PdfRect) -> FS_RECTF {
    FS_RECTF { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
}

fn bounds(points: &[(f32, f32)], pad: f32) -> FS_RECTF {
    let mut r = FS_RECTF { left: f32::MAX, top: f32::MIN, right: f32::MIN, bottom: f32::MAX };
    for &(x, y) in points {
        r.left = r.left.min(x);
        r.right = r.right.max(x);
        r.bottom = r.bottom.min(y);
        r.top = r.top.max(y);
    }
    r.left -= pad;
    r.right += pad;
    r.bottom -= pad;
    r.top += pad;
    r
}
