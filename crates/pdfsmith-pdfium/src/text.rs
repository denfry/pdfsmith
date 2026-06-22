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
        let (mut l, mut r, mut bot, mut t): (c_double, c_double, c_double, c_double) =
            (0.0, 0.0, 0.0, 0.0);
        let ok = unsafe {
            b.FPDFText_GetCharBox(self.handle, index, &mut l, &mut r, &mut bot, &mut t)
        };
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
