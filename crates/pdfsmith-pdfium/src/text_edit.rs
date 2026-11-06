//! Правка текста страницы, как в редакторах PDF: список текстовых объектов,
//! замена текста объекта, новый текст, перенос и удаление.
//!
//! Замена старается сохранить исходный шрифт: если все новые символы уже есть
//! в этом шрифте на странице, текст объекта просто переписывается. Иначе объект
//! заменяется новым — тем же кеглем, цветом и матрицей, но системным шрифтом
//! похожего начертания (встроенные шрифты почти всегда урезаны до использованных
//! глифов, и новая буква в них нарисовалась бы пустотой).

use std::collections::HashSet;
use std::os::raw::{c_char, c_int, c_uint, c_ushort};

use pdfium_render::prelude::*;

use crate::{bindings, Document, Page, PdfError, PdfRect, Rgba};

const FPDF_PAGEOBJ_TEXT: c_int = 1;
const FPDF_FONT_TRUETYPE: c_int = 2;
const TEXT_RENDER_INVISIBLE: c_int = 3;
// Флаги дескриптора шрифта (PDF 32000, 9.8.2).
const FLAG_FIXED_PITCH: c_int = 1;
const FLAG_SERIF: c_int = 1 << 1;
const FLAG_ITALIC: c_int = 1 << 6;

/// Текстовый объект страницы (верхнего уровня, без вложенных форм).
#[derive(Debug, Clone, PartialEq)]
pub struct TextObject {
    /// Индекс среди объектов страницы.
    pub index: usize,
    pub text: String,
    /// Рамка в PDF-пространстве.
    pub rect: PdfRect,
    /// Видимый кегль в пунктах (с учётом масштаба матрицы).
    pub size: f32,
    pub color: Rgba,
    /// Имя шрифта без префикса подмножества («ABCDEF+»).
    pub font: String,
    pub bold: bool,
    pub italic: bool,
}

/// Чем закончилась замена текста.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextChange {
    /// Текст переписан в исходном шрифте.
    KeptFont,
    /// Объект пересоздан системным шрифтом.
    NewFont,
    Removed,
}

/// Начертание системного шрифта: файлы обычного, жирного, курсива, жирного курсива.
struct Family {
    names: &'static [&'static str],
    files: [&'static str; 4],
}

const FAMILIES: &[Family] = &[
    Family { names: &["calibri"], files: ["calibri.ttf", "calibrib.ttf", "calibrii.ttf", "calibriz.ttf"] },
    Family { names: &["arial", "helvetica", "liberationsans", "dejavusans"], files: ["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf"] },
    Family { names: &["times", "liberationserif", "tinos", "ptserif"], files: ["times.ttf", "timesbd.ttf", "timesi.ttf", "timesbi.ttf"] },
    Family { names: &["courier", "liberationmono", "consolas"], files: ["cour.ttf", "courbd.ttf", "couri.ttf", "courbi.ttf"] },
    Family { names: &["verdana"], files: ["verdana.ttf", "verdanab.ttf", "verdanai.ttf", "verdanaz.ttf"] },
    Family { names: &["tahoma"], files: ["tahoma.ttf", "tahomabd.ttf", "tahoma.ttf", "tahomabd.ttf"] },
    Family { names: &["georgia"], files: ["georgia.ttf", "georgiab.ttf", "georgiai.ttf", "georgiaz.ttf"] },
    Family { names: &["segoeui", "segoe"], files: ["segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf"] },
    Family { names: &["trebuchet"], files: ["trebuc.ttf", "trebucbd.ttf", "trebucit.ttf", "trebucbi.ttf"] },
];
const ARIAL: usize = 1;
const TIMES: usize = 2;
const COURIER: usize = 3;

/// Файл системного шрифта для исходного: семейство по имени, иначе по флагам.
fn font_file(name: &str, flags: c_int, bold: bool, italic: bool) -> &'static str {
    let key: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let fam = FAMILIES
        .iter()
        .position(|f| f.names.iter().any(|n| key.contains(n)))
        .unwrap_or(if flags & FLAG_FIXED_PITCH != 0 {
            COURIER
        } else if flags & FLAG_SERIF != 0 {
            TIMES
        } else {
            ARIAL
        });
    FAMILIES[fam].files[bold as usize + 2 * italic as usize]
}

fn fonts_dir() -> std::path::PathBuf {
    let win = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    std::path::Path::new(&win).join("Fonts")
}

/// Имя шрифта без префикса подмножества.
fn strip_subset(name: &str) -> &str {
    match name.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()) => rest,
        _ => name,
    }
}

unsafe fn base_font_name(font: FPDF_FONT) -> String {
    let b = bindings();
    let n = b.FPDFFont_GetBaseFontName(font, std::ptr::null_mut(), 0);
    if n == 0 {
        return String::new();
    }
    let mut buf = vec![0u8; n];
    b.FPDFFont_GetBaseFontName(font, buf.as_mut_ptr() as *mut c_char, n);
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Жирность и курсив шрифта — по дескриптору и по имени.
unsafe fn style(font: FPDF_FONT, name: &str) -> (bool, bool, c_int) {
    let b = bindings();
    let lower = name.to_ascii_lowercase();
    let flags = b.FPDFFont_GetFlags(font);
    let weight = b.FPDFFont_GetWeight(font);
    let mut angle: c_int = 0;
    b.FPDFFont_GetItalicAngle(font, &mut angle);
    let bold = weight >= 600 || ["bold", "black", "heavy", "semibold", "demi"].iter().any(|w| lower.contains(w));
    let italic = angle != 0 || flags & FLAG_ITALIC != 0 || lower.contains("italic") || lower.contains("oblique");
    (bold, italic, flags.max(0))
}

unsafe fn object_text(obj: FPDF_PAGEOBJECT, tp: FPDF_TEXTPAGE) -> String {
    let b = bindings();
    let n = b.FPDFTextObj_GetText(obj, tp, std::ptr::null_mut(), 0);
    if n == 0 {
        return String::new();
    }
    let mut buf = vec![0 as c_ushort; n as usize / 2 + 1];
    b.FPDFTextObj_GetText(obj, tp, buf.as_mut_ptr(), (buf.len() * 2) as _);
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

unsafe fn fill_color(obj: FPDF_PAGEOBJECT) -> Rgba {
    let (mut r, mut g, mut bl, mut a) = (0 as c_uint, 0 as c_uint, 0 as c_uint, 255 as c_uint);
    bindings().FPDFPageObj_GetFillColor(obj, &mut r, &mut g, &mut bl, &mut a);
    Rgba(r as u8, g as u8, bl as u8, a as u8)
}

unsafe fn matrix(obj: FPDF_PAGEOBJECT) -> FS_MATRIX {
    let mut m = FS_MATRIX { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };
    bindings().FPDFPageObj_GetMatrix(obj, &mut m);
    m
}

unsafe fn raw_size(obj: FPDF_PAGEOBJECT) -> f32 {
    let mut s = 0.0f32;
    bindings().FPDFTextObj_GetFontSize(obj, &mut s);
    s
}

fn page_err(msg: &str) -> PdfError {
    PdfError::Render(msg.into())
}

impl Page {
    fn object(&self, index: usize) -> Option<FPDF_PAGEOBJECT> {
        let b = bindings();
        let n = unsafe { b.FPDFPage_CountObjects(self.handle) }.max(0) as usize;
        if index >= n {
            return None;
        }
        let obj = unsafe { b.FPDFPage_GetObject(self.handle, index as c_int) };
        (!obj.is_null()).then_some(obj)
    }

    /// Видимые текстовые объекты страницы.
    pub fn text_objects(&self) -> Vec<TextObject> {
        let b = bindings();
        let mut out = Vec::new();
        unsafe {
            let tp = b.FPDFText_LoadPage(self.handle);
            if tp.is_null() {
                return out;
            }
            let n = b.FPDFPage_CountObjects(self.handle).max(0);
            for i in 0..n {
                let obj = b.FPDFPage_GetObject(self.handle, i);
                if obj.is_null() || b.FPDFPageObj_GetType(obj) != FPDF_PAGEOBJ_TEXT {
                    continue;
                }
                if b.FPDFTextObj_GetTextRenderMode(obj) == TEXT_RENDER_INVISIBLE {
                    continue;
                }
                let text = object_text(obj, tp);
                if text.trim().is_empty() {
                    continue;
                }
                let (mut l, mut bt, mut r, mut t) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
                if !b.is_true(b.FPDFPageObj_GetBounds(obj, &mut l, &mut bt, &mut r, &mut t)) || r <= l || t <= bt {
                    continue;
                }
                let m = matrix(obj);
                let size = raw_size(obj) * (m.c * m.c + m.d * m.d).sqrt();
                let font = b.FPDFTextObj_GetFont(obj);
                let name = base_font_name(font);
                let (bold, italic, _) = style(font, &name);
                out.push(TextObject {
                    index: i as usize,
                    text,
                    rect: PdfRect { left: l, bottom: bt, right: r, top: t },
                    size,
                    color: fill_color(obj),
                    font: strip_subset(&name).to_string(),
                    bold,
                    italic,
                });
            }
            b.FPDFText_ClosePage(tp);
        }
        out
    }

    /// Сдвигает объект на (dx, dy) в PDF-пространстве.
    pub fn move_object(&self, index: usize, dx: f32, dy: f32) -> Result<(), PdfError> {
        let obj = self.object(index).ok_or_else(|| page_err("объект не найден — страница изменилась"))?;
        let b = bindings();
        unsafe {
            b.FPDFPageObj_Transform(obj, 1.0, 0.0, 0.0, 1.0, dx as f64, dy as f64);
            b.FPDFPage_GenerateContent(self.handle);
        }
        Ok(())
    }

    /// Символы, которые шрифт `font` уже рисует на этой странице.
    unsafe fn chars_of_font(&self, font: FPDF_FONT) -> HashSet<char> {
        let b = bindings();
        let mut set = HashSet::new();
        let tp = b.FPDFText_LoadPage(self.handle);
        if tp.is_null() {
            return set;
        }
        let n = b.FPDFPage_CountObjects(self.handle).max(0);
        for i in 0..n {
            let obj = b.FPDFPage_GetObject(self.handle, i);
            if !obj.is_null() && b.FPDFPageObj_GetType(obj) == FPDF_PAGEOBJ_TEXT && b.FPDFTextObj_GetFont(obj) == font {
                set.extend(object_text(obj, tp).chars());
            }
        }
        b.FPDFText_ClosePage(tp);
        set
    }

    unsafe fn text_at(&self, obj: FPDF_PAGEOBJECT) -> String {
        let b = bindings();
        let tp = b.FPDFText_LoadPage(self.handle);
        if tp.is_null() {
            return String::new();
        }
        let s = object_text(obj, tp);
        b.FPDFText_ClosePage(tp);
        s
    }
}

impl Document {
    /// Системный TrueType-шрифт, загруженный в документ (один раз на файл:
    /// PDFium встраивает его целиком).
    fn system_font(&self, file: &'static str) -> Result<FPDF_FONT, PdfError> {
        if let Some(&(_, f)) = self.fonts.borrow().iter().find(|(n, _)| *n == file) {
            return Ok(f);
        }
        let dir = fonts_dir();
        let (file, bytes) = match std::fs::read(dir.join(file)) {
            Ok(b) => (file, b),
            // Нет нужного начертания — обычный Arial лучше, чем ничего.
            Err(_) => ("arial.ttf", std::fs::read(dir.join("arial.ttf")).map_err(|e| page_err(&format!("шрифт {file}: {e}")))?),
        };
        if let Some(&(_, f)) = self.fonts.borrow().iter().find(|(n, _)| *n == file) {
            return Ok(f);
        }
        let font = unsafe {
            bindings().FPDFText_LoadFont(self.handle, bytes.as_ptr(), bytes.len() as u32, FPDF_FONT_TRUETYPE, 1)
        };
        if font.is_null() {
            return Err(page_err(&format!("PDFium не принял шрифт {file}")));
        }
        self.fonts.borrow_mut().push((file, font));
        Ok(font)
    }

    /// Заменяет текст объекта `index`. `expect` — текст, который видел
    /// пользователь: если объект уже другой, правка отклоняется.
    /// Пустой `new` удаляет объект.
    pub fn replace_text(&self, page: &Page, index: usize, expect: &str, new: &str) -> Result<TextChange, PdfError> {
        let b = bindings();
        let obj = page.object(index).ok_or_else(|| page_err("объект не найден — страница изменилась"))?;
        unsafe {
            if b.FPDFPageObj_GetType(obj) != FPDF_PAGEOBJ_TEXT || page.text_at(obj) != expect {
                return Err(page_err("текст на странице изменился — попробуйте ещё раз"));
            }
            if new.trim().is_empty() {
                b.FPDFPage_RemoveObject(page.handle, obj);
                b.FPDFPageObj_Destroy(obj);
                b.FPDFPage_GenerateContent(page.handle);
                return Ok(TextChange::Removed);
            }

            let font = b.FPDFTextObj_GetFont(obj);
            let name = base_font_name(font);
            let (bold, italic, flags) = style(font, &name);

            // Все символы уже рисуются этим шрифтом — пишем в нём же.
            let known = page.chars_of_font(font);
            if new.chars().all(|c| known.contains(&c)) && b.is_true(b.FPDFText_SetText_str(obj, new)) {
                if page.text_at(obj) == new {
                    b.FPDFPage_GenerateContent(page.handle);
                    return Ok(TextChange::KeptFont);
                }
            }

            let sys = self.system_font(font_file(strip_subset(&name), flags, bold, italic))?;
            let m = matrix(obj);
            let color = fill_color(obj);
            let mode = b.FPDFTextObj_GetTextRenderMode(obj);
            let fresh = b.FPDFPageObj_CreateTextObj(self.handle, sys, raw_size(obj));
            if fresh.is_null() {
                return Err(page_err("не удалось создать текстовый объект"));
            }
            b.FPDFText_SetText_str(fresh, new);
            b.FPDFPageObj_SetMatrix(fresh, &m);
            b.FPDFPageObj_SetFillColor(fresh, color.0 as c_uint, color.1 as c_uint, color.2 as c_uint, color.3 as c_uint);
            b.FPDFTextObj_SetTextRenderMode(fresh, mode);
            b.FPDFPage_RemoveObject(page.handle, obj);
            b.FPDFPageObj_Destroy(obj);
            if !b.is_true(b.FPDFPage_InsertObjectAtIndex(page.handle, fresh, index)) {
                b.FPDFPage_InsertObject(page.handle, fresh);
            }
            b.FPDFPage_GenerateContent(page.handle);
        }
        Ok(TextChange::NewFont)
    }

    /// Новый текст. `origin` — начало базовой линии в PDF-пространстве,
    /// `right`/`up` — единичные векторы строки и вверх (учитывают /Rotate,
    /// чтобы на экране текст стоял ровно).
    pub fn add_text(
        &self,
        page: &Page,
        origin: (f32, f32),
        right: (f32, f32),
        up: (f32, f32),
        text: &str,
        size: f32,
        color: Rgba,
    ) -> Result<(), PdfError> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let font = self.system_font("arial.ttf")?;
        let b = bindings();
        unsafe {
            let obj = b.FPDFPageObj_CreateTextObj(self.handle, font, size);
            if obj.is_null() {
                return Err(page_err("не удалось создать текстовый объект"));
            }
            b.FPDFText_SetText_str(obj, text);
            let m = FS_MATRIX { a: right.0, b: right.1, c: up.0, d: up.1, e: origin.0, f: origin.1 };
            b.FPDFPageObj_SetMatrix(obj, &m);
            b.FPDFPageObj_SetFillColor(obj, color.0 as c_uint, color.1 as c_uint, color.2 as c_uint, color.3 as c_uint);
            b.FPDFPage_InsertObject(page.handle, obj);
            b.FPDFPage_GenerateContent(page.handle);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_system_font_by_name_and_flags() {
        assert_eq!(font_file("Calibri-Bold", 0, true, false), "calibrib.ttf");
        assert_eq!(font_file("TimesNewRomanPS-ItalicMT", 0, false, true), "timesi.ttf");
        assert_eq!(font_file("Helvetica", 0, false, false), "arial.ttf");
        assert_eq!(font_file("Unknown", FLAG_SERIF, false, false), "times.ttf");
        assert_eq!(font_file("Unknown", FLAG_FIXED_PITCH, true, true), "courbi.ttf");
        assert_eq!(font_file("", 0, false, false), "arial.ttf");
    }

    #[test]
    fn strips_subset_prefix() {
        assert_eq!(strip_subset("ABCDEF+Calibri"), "Calibri");
        assert_eq!(strip_subset("Arial+Bold"), "Arial+Bold");
        assert_eq!(strip_subset("Arial"), "Arial");
    }
}
