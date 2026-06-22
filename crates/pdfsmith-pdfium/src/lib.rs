//! pdfsmith-pdfium — тонкая обёртка над PDFium через низкоуровневые биндинги
//! pdfium-render.
//!
//! High-level API pdfium-render всегда выделяет буфер размером со всю страницу,
//! что неприемлемо при глубоком зуме огромных чертежей. Поэтому мы работаем на
//! уровне сырых вызовов `FPDF_*` и рендерим произвольный прямоугольный кусок
//! страницы (`FPDF_RenderPageBitmapWithMatrix`) — основа тайлинга.
//!
//! PDFium не потокобезопасен (глобальное состояние), поэтому биндинги собраны с
//! фичей `thread_safe` (сериализующий мьютекс), а приложение держит весь рендер
//! в одном выделенном потоке.

use std::os::raw::c_int;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use pdfium_render::prelude::*;

pub mod text;

/// Формат битмапа BGRA, 8 бит на канал (значение PDFium `FPDFBitmap_BGRA`).
const FORMAT_BGRA: c_int = 4;

/// Ошибки PDF-бэкенда.
#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("не удалось загрузить библиотеку PDFium: {0}")]
    Bind(String),
    #[error("не удалось открыть документ: {0}")]
    Open(String),
    #[error("ошибка рендеринга: {0}")]
    Render(String),
}

/// Размер страницы в типографских пунктах (1 pt = 1/72 дюйма).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    pub width_pt: f32,
    pub height_pt: f32,
}

/// Готовое RGBA-изображение (8 бит на канал, альфа без предумножения).
pub struct RenderedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

static BINDINGS: OnceLock<Box<dyn PdfiumLibraryBindings>> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Инициализирует библиотеку PDFium (однократно, идемпотентно). Ищет
/// `pdfium.dll` в `dll_dir`, затем рядом с исполняемым файлом, затем в системе.
pub fn init(dll_dir: Option<&Path>) -> Result<(), PdfError> {
    let _guard = INIT_LOCK.lock().expect("INIT_LOCK отравлен");
    if BINDINGS.get().is_some() {
        return Ok(());
    }
    let bindings = bind(dll_dir)?;
    unsafe { bindings.FPDF_InitLibrary() };
    let _ = BINDINGS.set(bindings);
    Ok(())
}

fn bindings() -> &'static dyn PdfiumLibraryBindings {
    BINDINGS
        .get()
        .expect("pdfsmith_pdfium::init() должен быть вызван до использования")
        .as_ref()
}

fn bind(dll_dir: Option<&Path>) -> Result<Box<dyn PdfiumLibraryBindings>, PdfError> {
    if let Some(dir) = dll_dir {
        let name = Pdfium::pdfium_platform_library_name_at_path(dir);
        if let Ok(b) = Pdfium::bind_to_library(&name) {
            return Ok(b);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = Pdfium::pdfium_platform_library_name_at_path(dir);
            if let Ok(b) = Pdfium::bind_to_library(&name) {
                return Ok(b);
            }
        }
    }
    Pdfium::bind_to_system_library().map_err(|e| PdfError::Bind(e.to_string()))
}

/// Открытый PDF-документ. Держит размеры всех страниц (дёшево, без загрузки
/// самих страниц) и байты файла (PDFium ссылается на них, пока документ открыт).
pub struct Document {
    handle: FPDF_DOCUMENT,
    _bytes: Vec<u8>,
    page_sizes: Vec<PageSize>,
}

impl Document {
    /// Открывает документ из файла.
    pub fn open(path: &Path, password: Option<&str>) -> Result<Document, PdfError> {
        let bytes = std::fs::read(path).map_err(|e| PdfError::Open(e.to_string()))?;
        Document::from_bytes(bytes, password)
    }

    /// Открывает документ из байтов в памяти. PDFium берёт буфер по ссылке,
    /// поэтому он сохраняется внутри `Document`.
    pub fn from_bytes(bytes: Vec<u8>, password: Option<&str>) -> Result<Document, PdfError> {
        let b = bindings();
        let handle = unsafe { b.FPDF_LoadMemDocument64(bytes.as_slice(), password) };
        if handle.is_null() {
            return Err(PdfError::Open(
                "PDFium не смог открыть документ (повреждён или зашифрован)".into(),
            ));
        }

        let count = unsafe { b.FPDF_GetPageCount(handle) }.max(0) as usize;
        let mut page_sizes = Vec::with_capacity(count);
        for i in 0..count {
            let mut size = FS_SIZEF { width: 0.0, height: 0.0 };
            let ok = unsafe { b.FPDF_GetPageSizeByIndexF(handle, i as c_int, &mut size) };
            if b.is_true(ok) {
                page_sizes.push(PageSize { width_pt: size.width, height_pt: size.height });
            } else {
                page_sizes.push(PageSize { width_pt: 0.0, height_pt: 0.0 });
            }
        }

        Ok(Document { handle, _bytes: bytes, page_sizes })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn page_size(&self, index: usize) -> Option<PageSize> {
        self.page_sizes.get(index).copied()
    }

    /// Загружает страницу для повторного рендера. Загрузка разбирает content
    /// stream (дорого для плотных чертежей) — поэтому страницу держат загружённой
    /// и рендерят из неё много тайлов.
    pub fn load_page(&self, index: usize) -> Result<Page, PdfError> {
        let b = bindings();
        let handle = unsafe { b.FPDF_LoadPage(self.handle, index as c_int) };
        if handle.is_null() {
            return Err(PdfError::Render(format!("не удалось загрузить страницу {index}")));
        }
        let width_pt = unsafe { b.FPDF_GetPageWidthF(handle) };
        let height_pt = unsafe { b.FPDF_GetPageHeightF(handle) };
        Ok(Page { handle, width_pt, height_pt })
    }

    /// Удобный путь: рендер всей страницы целиком в указанном масштабе.
    pub fn render_page_scaled(&self, index: usize, scale: f32) -> Result<RenderedImage, PdfError> {
        let page = self.load_page(index)?;
        let w = ((page.width_pt * scale).round() as i32).max(1);
        let h = ((page.height_pt * scale).round() as i32).max(1);
        page.render_region(scale, 0, 0, w, h, 0, true)
    }
}

impl Drop for Document {
    fn drop(&mut self) {
        unsafe { bindings().FPDF_CloseDocument(self.handle) };
    }
}

/// Загруженная страница. Держится открытой ради дешёвого повторного рендера
/// тайлов.
pub struct Page {
    handle: FPDF_PAGE,
    width_pt: f32,
    height_pt: f32,
}

impl Page {
    pub fn width_pt(&self) -> f32 {
        self.width_pt
    }

    pub fn height_pt(&self) -> f32 {
        self.height_pt
    }

    /// Рендерит прямоугольный кусок страницы в RGBA с учётом поворота.
    ///
    /// Страница масштабируется в `scale` device-px на пункт; `(tx, ty)` — смещение
    /// левого-верхнего угла куска в (повёрнутой) device-системе, `(tw, th)` — его
    /// размер. `rotation` — число четвертей по часовой стрелке (0..3).
    ///
    /// Матрица отображает страницу в device-пространство `FPDF_RenderPageBitmapWithMatrix`.
    /// Чистый масштаб даёт верную ориентацию (как в high-level pdfium-render);
    /// повороты — производные от него.
    pub fn render_region(
        &self,
        scale: f32,
        tx: i32,
        ty: i32,
        tw: i32,
        th: i32,
        rotation: u8,
        antialias: bool,
    ) -> Result<RenderedImage, PdfError> {
        if tw <= 0 || th <= 0 {
            return Err(PdfError::Render("неположительный размер тайла".into()));
        }
        let b = bindings();

        let bitmap =
            unsafe { b.FPDFBitmap_CreateEx(tw, th, FORMAT_BGRA, std::ptr::null_mut(), 0) };
        if bitmap.is_null() {
            return Err(PdfError::Render("не удалось создать битмап".into()));
        }

        // Белый фон (страницы PDF прозрачны по умолчанию).
        unsafe { b.FPDFBitmap_FillRect(bitmap, 0, 0, tw, th, 0xFFFF_FFFF) };

        // Device-размер невёрнутой страницы.
        let sw = self.width_pt * scale;
        let sh = self.height_pt * scale;
        // [a, b, c, d, e, f] полной (повёрнутой) страницы; затем сдвиг тайла.
        let m = match rotation & 3 {
            0 => [scale, 0.0, 0.0, scale, 0.0, 0.0],
            1 => [0.0, scale, -scale, 0.0, sh, 0.0], // 90° по часовой
            2 => [-scale, 0.0, 0.0, -scale, sw, sh], // 180°
            _ => [0.0, -scale, scale, 0.0, 0.0, sw], // 270° по часовой
        };
        let matrix = FS_MATRIX {
            a: m[0],
            b: m[1],
            c: m[2],
            d: m[3],
            e: m[4] - tx as f32,
            f: m[5] - ty as f32,
        };
        let clip = FS_RECTF { left: 0.0, top: 0.0, right: tw as f32, bottom: th as f32 };

        let flags = if antialias { 0 } else { render_flags_no_aa() };
        unsafe {
            b.FPDF_RenderPageBitmapWithMatrix(bitmap, self.handle, &matrix, &clip, flags);
        }

        let stride = unsafe { b.FPDFBitmap_GetStride(bitmap) } as usize;
        let buf = unsafe { b.FPDFBitmap_GetBuffer(bitmap) } as *const u8;
        let rgba = if buf.is_null() {
            unsafe { b.FPDFBitmap_Destroy(bitmap) };
            return Err(PdfError::Render("пустой буфер битмапа".into()));
        } else {
            copy_bgra_to_rgba(buf, stride, tw as usize, th as usize)
        };

        unsafe { b.FPDFBitmap_Destroy(bitmap) };

        Ok(RenderedImage { width: tw as u32, height: th as u32, rgba })
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        unsafe { bindings().FPDF_ClosePage(self.handle) };
    }
}

/// Флаги PDFium для отключения сглаживания (текст/картинки/пути).
fn render_flags_no_aa() -> c_int {
    const FPDF_RENDER_NO_SMOOTHTEXT: c_int = 0x1000;
    const FPDF_RENDER_NO_SMOOTHIMAGE: c_int = 0x2000;
    const FPDF_RENDER_NO_SMOOTHPATH: c_int = 0x4000;
    FPDF_RENDER_NO_SMOOTHTEXT | FPDF_RENDER_NO_SMOOTHIMAGE | FPDF_RENDER_NO_SMOOTHPATH
}

/// Копирует пиксели BGRA (с учётом stride) в плотный буфер RGBA.
fn copy_bgra_to_rgba(buf: *const u8, stride: usize, width: usize, height: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; width * height * 4];
    for row in 0..height {
        let src_row = unsafe { std::slice::from_raw_parts(buf.add(row * stride), width * 4) };
        let dst_row = &mut rgba[row * width * 4..(row + 1) * width * 4];
        for x in 0..width {
            let s = &src_row[x * 4..x * 4 + 4];
            let d = &mut dst_row[x * 4..x * 4 + 4];
            d[0] = s[2]; // R <- B
            d[1] = s[1]; // G
            d[2] = s[0]; // B <- R
            d[3] = s[3]; // A
        }
    }
    rgba
}
