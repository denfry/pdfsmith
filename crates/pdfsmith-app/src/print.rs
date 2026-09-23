//! Печать: выбор страниц, раскладка страницы на листе и вывод на принтер.
//!
//! Страница растеризуется PDFium с разрешением принтера (не выше `MAX_DPI`)
//! полосами и передаётся драйверу через GDI `StretchDIBits`. Растровый путь
//! одинаково предсказуем на любом драйвере, включая «Microsoft Print to PDF»,
//! а полосы держат память в пределах `BAND_BYTES` даже для чертежей A0.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use pdfsmith_pdfium::RenderedImage;

/// Выше этого разрешения растр не поднимаем: разницы на бумаге не видно,
/// а объём задания в спулере растёт квадратично.
const MAX_DPI: f32 = 600.0;
/// Размер одной полосы растра в байтах.
const BAND_BYTES: usize = 16 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageSet {
    All,
    Current,
    Range,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Subset {
    All,
    Odd,
    Even,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Scaling {
    /// Вписать в печатную область.
    Fit,
    /// 100 %, лишнее обрезается.
    Actual,
    /// 100 %, но крупные страницы уменьшить до печатной области.
    Shrink,
    /// Свой масштаб, проценты.
    Custom(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    Auto,
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Duplex {
    Simplex,
    LongEdge,
    ShortEdge,
}

/// Прямоугольник в пунктах: левый-верхний угол и размер.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PtRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Лист бумаги принтера в пунктах (origin — левый-верхний угол листа).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Paper {
    pub width: f32,
    pub height: f32,
    /// Печатная область (без полей, которые принтер физически не пропечатывает).
    pub area: PtRect,
    pub dpi_x: f32,
    pub dpi_y: f32,
}

impl Paper {
    /// A4 без полей — для предпросмотра, пока принтер не выбран.
    pub fn a4() -> Paper {
        Paper { width: 595.0, height: 842.0, area: PtRect { x: 0.0, y: 0.0, w: 595.0, h: 842.0 }, dpi_x: 300.0, dpi_y: 300.0 }
    }
}

/// Где и как страница ложится на лист.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Placement {
    /// Поворот страницы в четвертях по часовой.
    pub rotation: u8,
    /// Прямоугольник страницы на листе (может выходить за печатную область).
    pub rect: PtRect,
}

/// Раскладка страницы `page` (пт, с учётом её /Rotate) на лист. При
/// `auto_rotate` альбомная страница на книжном листе (и наоборот)
/// поворачивается на 90°. Страница центрируется в печатной области.
pub fn place(page: (f32, f32), paper: &Paper, scaling: Scaling, auto_rotate: bool) -> Placement {
    let (mut pw, mut ph) = (page.0.max(1.0), page.1.max(1.0));
    let a = paper.area;
    let mut rotation = 0;
    if auto_rotate && (pw > ph) != (a.w > a.h) && (pw - ph).abs() > 1.0 {
        rotation = 1;
        std::mem::swap(&mut pw, &mut ph);
    }
    let fit = (a.w / pw).min(a.h / ph);
    let s = match scaling {
        Scaling::Fit => fit,
        Scaling::Actual => 1.0,
        Scaling::Shrink => fit.min(1.0),
        Scaling::Custom(pct) => (pct / 100.0).clamp(0.01, 10.0),
    };
    let (w, h) = (pw * s, ph * s);
    Placement { rotation, rect: PtRect { x: a.x + (a.w - w) * 0.5, y: a.y + (a.h - h) * 0.5, w, h } }
}

/// Разбор диапазона «1-3, 5, 8-» (номера с 1) в индексы страниц с 0.
/// «-3» — с первой по третью, «8-» — с восьмой до конца, «5-3» — в обратном порядке.
pub fn parse_ranges(text: &str, n: usize) -> Result<Vec<usize>, String> {
    let num = |s: &str| -> Result<usize, String> {
        let v: usize = s.trim().parse().map_err(|_| format!("«{}» — не номер страницы", s.trim()))?;
        if v == 0 || v > n {
            return Err(format!("страницы {v} нет (всего {n})"));
        }
        Ok(v)
    };
    let mut out = Vec::new();
    for part in text.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let part = part.replace(['–', '—'], "-");
        match part.split_once('-') {
            None => out.push(num(&part)? - 1),
            Some((a, b)) => {
                let a = if a.trim().is_empty() { 1 } else { num(a)? };
                let b = if b.trim().is_empty() { n } else { num(b)? };
                if a <= b {
                    out.extend((a..=b).map(|i| i - 1));
                } else {
                    out.extend((b..=a).rev().map(|i| i - 1));
                }
            }
        }
    }
    if out.is_empty() {
        return Err("укажите страницы, например 1-3, 5".into());
    }
    Ok(out)
}

/// Итоговый список страниц к печати.
pub fn select_pages(
    set: PageSet,
    current: usize,
    range: &str,
    subset: Subset,
    reverse: bool,
    n: usize,
) -> Result<Vec<usize>, String> {
    if n == 0 {
        return Err("документ пуст".into());
    }
    let mut pages = match set {
        PageSet::All => (0..n).collect(),
        PageSet::Current => vec![current.min(n - 1)],
        PageSet::Range => parse_ranges(range, n)?,
    };
    // Чётность — по номеру страницы в документе, как в Acrobat.
    match subset {
        Subset::All => {}
        Subset::Odd => pages.retain(|p| p % 2 == 0),
        Subset::Even => pages.retain(|p| p % 2 == 1),
    }
    if reverse {
        pages.reverse();
    }
    if pages.is_empty() {
        return Err("под условия не попала ни одна страница".into());
    }
    Ok(pages)
}

/// Что и как печатать. Ориентация листа, бумага и двусторонняя печать уже
/// записаны в `devmode`; `auto_rotate` — ориентация «Авто».
#[derive(Clone)]
pub struct PrintJob {
    pub printer: String,
    pub devmode: Option<DevMode>,
    pub doc_name: String,
    pub pages: Vec<usize>,
    pub copies: u32,
    pub collate: bool,
    pub scaling: Scaling,
    pub auto_rotate: bool,
    pub grayscale: bool,
    pub annotations: bool,
    /// Печать в файл (для виртуальных принтеров вроде «Microsoft Print to PDF»).
    pub output: Option<PathBuf>,
}

impl PrintJob {
    /// Порядок листов: с разбором по копиям — 1,2,3,1,2,3; без — 1,1,2,2,3,3.
    pub fn sheets(&self) -> Vec<usize> {
        let copies = self.copies.max(1) as usize;
        if self.collate {
            (0..copies).flat_map(|_| self.pages.iter().copied()).collect()
        } else {
            self.pages.iter().flat_map(|&p| std::iter::repeat(p).take(copies)).collect()
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum PrintError {
    Cancelled,
    Failed(String),
}

/// Рендер куска страницы: (страница, px на пт, tx, ty, tw, th, поворот).
pub type RenderFn<'a> = dyn FnMut(usize, f32, i32, i32, i32, i32, u8) -> Result<RenderedImage, String> + 'a;

/// Пересчёт RGBA → BGRA (порядок GDI) на месте, по желанию — в оттенки серого.
fn to_bgra(rgba: &mut [u8], grayscale: bool) {
    for px in rgba.chunks_exact_mut(4) {
        if grayscale {
            let y = ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8;
            px[0] = y;
            px[1] = y;
            px[2] = y;
        } else {
            px.swap(0, 2);
        }
    }
}

/// Полоса растра: прямоугольник в пикселях растра страницы (x, y, w, h) и
/// приёмник в пикселях устройства относительно печатной области (x, y, w, h).
type Band = ([i32; 4], [i32; 4]);

/// Полосы растра размещённой страницы и масштаб растра (px на пт листа).
/// Всё вне печатной области отсекается ещё до рендера.
fn bands(place: &Placement, paper: &Paper) -> (f32, Vec<Band>) {
    let rdpi = paper.dpi_x.min(paper.dpi_y).min(MAX_DPI);
    let k = rdpi / 72.0;
    let (r, a) = (place.rect, paper.area);
    let ix0 = r.x.max(a.x);
    let iy0 = r.y.max(a.y);
    let ix1 = (r.x + r.w).min(a.x + a.w);
    let iy1 = (r.y + r.h).min(a.y + a.h);
    if ix1 <= ix0 || iy1 <= iy0 {
        return (k, Vec::new());
    }
    let tx = ((ix0 - r.x) * k).floor() as i32;
    let ty = ((iy0 - r.y) * k).floor() as i32;
    let tw = (((ix1 - r.x) * k).ceil() as i32 - tx).max(1);
    let th = (((iy1 - r.y) * k).ceil() as i32 - ty).max(1);
    let dev_x = |px: i32| ((r.x + px as f32 / k - a.x) * paper.dpi_x / 72.0).round() as i32;
    let dev_y = |py: i32| ((r.y + py as f32 / k - a.y) * paper.dpi_y / 72.0).round() as i32;
    let band_h = (BAND_BYTES / (tw as usize * 4)).max(16) as i32;
    let mut out = Vec::new();
    let mut y = ty;
    while y < ty + th {
        let h = band_h.min(ty + th - y);
        let (dx0, dx1) = (dev_x(tx), dev_x(tx + tw));
        let (dy0, dy1) = (dev_y(y), dev_y(y + h));
        out.push(([tx, y, tw, h], [dx0, dy0, (dx1 - dx0).max(1), (dy1 - dy0).max(1)]));
        y += h;
    }
    (k, out)
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{HANDLE, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateDCW, CreateICW, DeleteDC, GetDeviceCaps, SetStretchBltMode, StretchDIBits, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DEVMODEW, DIB_RGB_COLORS, DM_COLLATE, DM_COLOR, DM_COPIES, DM_DUPLEX,
        DM_IN_BUFFER, DM_IN_PROMPT, DM_ORIENTATION, DM_OUT_BUFFER, DM_PAPERSIZE, HALFTONE, HDC, SRCCOPY,
    };
    use windows_sys::Win32::Graphics::Printing::{
        ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
        PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_INFO_4W,
    };
    use windows_sys::Win32::Storage::Xps::{
        AbortDoc, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW, StartPage, DC_COLORDEVICE, DC_DUPLEX, DC_PAPERNAMES, DC_PAPERS, DOCINFOW,
    };

    const HORZRES: i32 = 8;
    const VERTRES: i32 = 10;
    const LOGPIXELSX: i32 = 88;
    const LOGPIXELSY: i32 = 90;
    const PHYSICALWIDTH: i32 = 110;
    const PHYSICALHEIGHT: i32 = 111;
    const PHYSICALOFFSETX: i32 = 112;
    const PHYSICALOFFSETY: i32 = 113;
    const IDOK: i32 = 1;
    const ERROR_CANCELLED: i32 = 1223;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        unsafe {
            let len = (0..).take_while(|&i| *p.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        }
    }

    /// Настройки драйвера принтера (DEVMODE с приватным хвостом драйвера).
    /// Буфер из `u64` — ради выравнивания полей структуры.
    #[derive(Clone)]
    pub struct DevMode(Vec<u64>);

    impl DevMode {
        fn with_bytes(n: usize) -> DevMode {
            DevMode(vec![0u64; n.div_ceil(8).max(std::mem::size_of::<DEVMODEW>().div_ceil(8))])
        }
        fn ptr(&self) -> *const DEVMODEW {
            self.0.as_ptr() as *const DEVMODEW
        }
        fn ptr_mut(&mut self) -> *mut DEVMODEW {
            self.0.as_mut_ptr() as *mut DEVMODEW
        }
        fn dm(&self) -> &DEVMODEW {
            unsafe { &*self.ptr() }
        }
        fn dm_mut(&mut self) -> &mut DEVMODEW {
            unsafe { &mut *self.ptr_mut() }
        }

        pub fn orientation(&self) -> Orientation {
            match unsafe { self.dm().Anonymous1.Anonymous1.dmOrientation } {
                2 => Orientation::Landscape,
                _ => Orientation::Portrait,
            }
        }
        pub fn set_orientation(&mut self, o: Orientation) {
            let v = if o == Orientation::Landscape { 2 } else { 1 };
            let dm = self.dm_mut();
            dm.dmFields |= DM_ORIENTATION;
            dm.Anonymous1.Anonymous1.dmOrientation = v;
        }
        pub fn paper(&self) -> i16 {
            unsafe { self.dm().Anonymous1.Anonymous1.dmPaperSize }
        }
        pub fn set_paper(&mut self, id: i16) {
            let dm = self.dm_mut();
            dm.dmFields |= DM_PAPERSIZE;
            dm.Anonymous1.Anonymous1.dmPaperSize = id;
        }
        pub fn duplex(&self) -> Duplex {
            match self.dm().dmDuplex {
                2 => Duplex::LongEdge,
                3 => Duplex::ShortEdge,
                _ => Duplex::Simplex,
            }
        }
        pub fn set_duplex(&mut self, d: Duplex) {
            let dm = self.dm_mut();
            dm.dmFields |= DM_DUPLEX;
            // DMDUP_VERTICAL — переплёт по длинному краю книжного листа.
            dm.dmDuplex = match d {
                Duplex::Simplex => 1,
                Duplex::LongEdge => 2,
                Duplex::ShortEdge => 3,
            };
        }
        pub fn grayscale(&self) -> bool {
            self.dm().dmFields & DM_COLOR != 0 && self.dm().dmColor == 1
        }
        pub fn set_grayscale(&mut self, gray: bool) {
            let dm = self.dm_mut();
            dm.dmFields |= DM_COLOR;
            dm.dmColor = if gray { 1 } else { 2 };
        }
        /// Копии печатаем сами (надёжнее, чем полагаться на драйвер).
        fn single_copy(&mut self) {
            let dm = self.dm_mut();
            dm.dmFields |= DM_COPIES | DM_COLLATE;
            dm.Anonymous1.Anonymous1.dmCopies = 1;
            dm.dmCollate = 0;
        }
    }

    /// Возможности принтера для диалога.
    #[derive(Clone, Default)]
    pub struct PrinterCaps {
        /// (DMPAPER_*, название) — как их называет драйвер.
        pub papers: Vec<(i16, String)>,
        pub duplex: bool,
        pub color: bool,
    }

    /// Установленные принтеры (локальные и подключённые сетевые).
    pub fn list_printers() -> Vec<String> {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        unsafe {
            EnumPrintersW(flags, null(), 4, null_mut(), 0, &mut needed, &mut count);
            if needed == 0 {
                return Vec::new();
            }
            let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
            if EnumPrintersW(flags, null(), 4, buf.as_mut_ptr() as *mut u8, needed, &mut needed, &mut count) == 0 {
                return Vec::new();
            }
            let infos = std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, count as usize);
            infos.iter().map(|i| from_wide(i.pPrinterName)).filter(|s| !s.is_empty()).collect()
        }
    }

    pub fn default_printer() -> Option<String> {
        let mut len = 0u32;
        unsafe {
            GetDefaultPrinterW(null_mut(), &mut len);
            if len == 0 {
                return None;
            }
            let mut buf = vec![0u16; len as usize];
            if GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) == 0 {
                return None;
            }
            Some(from_wide(buf.as_ptr()))
        }
    }

    struct Printer(HANDLE);

    impl Printer {
        fn open(name: &str) -> Option<Printer> {
            let w = wide(name);
            let mut h: HANDLE = null_mut();
            (unsafe { OpenPrinterW(w.as_ptr(), &mut h, null()) } != 0 && !h.is_null()).then_some(Printer(h))
        }
    }

    impl Drop for Printer {
        fn drop(&mut self) {
            unsafe { ClosePrinter(self.0) };
        }
    }

    /// Настройки драйвера по умолчанию для принтера.
    pub fn default_devmode(name: &str) -> Option<DevMode> {
        let p = Printer::open(name)?;
        let w = wide(name);
        unsafe {
            let size = DocumentPropertiesW(null_mut(), p.0, w.as_ptr(), null_mut(), null(), 0);
            if size <= 0 {
                return None;
            }
            let mut dm = DevMode::with_bytes(size as usize);
            let r = DocumentPropertiesW(null_mut(), p.0, w.as_ptr(), dm.ptr_mut(), null(), DM_OUT_BUFFER);
            (r == IDOK).then_some(dm)
        }
    }

    /// Родное окно «Свойства принтера» (бумага, лоток, качество, двусторонняя
    /// печать…). Модально к активному окну приложения. `true` — нажали «ОК».
    pub fn properties_dialog(name: &str, dm: &mut DevMode) -> bool {
        let Some(p) = Printer::open(name) else { return false };
        let w = wide(name);
        let hwnd: HWND = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() };
        unsafe {
            let size = DocumentPropertiesW(hwnd, p.0, w.as_ptr(), null_mut(), null(), 0);
            if size <= 0 {
                return false;
            }
            let mut out = DevMode::with_bytes(size as usize);
            let r = DocumentPropertiesW(hwnd, p.0, w.as_ptr(), out.ptr_mut(), dm.ptr(), DM_IN_BUFFER | DM_IN_PROMPT | DM_OUT_BUFFER);
            if r == IDOK {
                *dm = out;
                true
            } else {
                false
            }
        }
    }

    pub fn printer_caps(name: &str, dm: Option<&DevMode>) -> PrinterCaps {
        let w = wide(name);
        let dmp = dm.map(|d| d.ptr()).unwrap_or(null());
        unsafe {
            let n = DeviceCapabilitiesW(w.as_ptr(), null(), DC_PAPERS, null_mut(), dmp);
            let mut papers = Vec::new();
            if n > 0 && DeviceCapabilitiesW(w.as_ptr(), null(), DC_PAPERNAMES, null_mut(), dmp) == n {
                let mut ids = vec![0u16; n as usize];
                let mut names = vec![0u16; n as usize * 64];
                DeviceCapabilitiesW(w.as_ptr(), null(), DC_PAPERS, ids.as_mut_ptr(), dmp);
                DeviceCapabilitiesW(w.as_ptr(), null(), DC_PAPERNAMES, names.as_mut_ptr(), dmp);
                for (i, id) in ids.iter().enumerate() {
                    let chunk = &names[i * 64..(i + 1) * 64];
                    let end = chunk.iter().position(|&c| c == 0).unwrap_or(64);
                    let name = String::from_utf16_lossy(&chunk[..end]).trim().to_string();
                    if !name.is_empty() {
                        papers.push((*id as i16, name));
                    }
                }
            }
            PrinterCaps {
                papers,
                duplex: DeviceCapabilitiesW(w.as_ptr(), null(), DC_DUPLEX, null_mut(), dmp) == 1,
                color: DeviceCapabilitiesW(w.as_ptr(), null(), DC_COLORDEVICE, null_mut(), dmp) == 1,
            }
        }
    }

    fn paper_of(dc: HDC) -> Option<Paper> {
        unsafe {
            let dpi_x = GetDeviceCaps(dc, LOGPIXELSX) as f32;
            let dpi_y = GetDeviceCaps(dc, LOGPIXELSY) as f32;
            if dpi_x <= 0.0 || dpi_y <= 0.0 {
                return None;
            }
            let pt_x = |v: i32| v as f32 * 72.0 / dpi_x;
            let pt_y = |v: i32| v as f32 * 72.0 / dpi_y;
            let width = pt_x(GetDeviceCaps(dc, PHYSICALWIDTH));
            let height = pt_y(GetDeviceCaps(dc, PHYSICALHEIGHT));
            let area = PtRect {
                x: pt_x(GetDeviceCaps(dc, PHYSICALOFFSETX)),
                y: pt_y(GetDeviceCaps(dc, PHYSICALOFFSETY)),
                w: pt_x(GetDeviceCaps(dc, HORZRES)),
                h: pt_y(GetDeviceCaps(dc, VERTRES)),
            };
            (width > 0.0 && height > 0.0 && area.w > 0.0 && area.h > 0.0).then_some(Paper { width, height, area, dpi_x, dpi_y })
        }
    }

    /// Лист и печатная область для текущих настроек драйвера.
    pub fn paper_info(name: &str, dm: Option<&DevMode>) -> Option<Paper> {
        let w = wide(name);
        unsafe {
            let dc = CreateICW(null(), w.as_ptr(), null(), dm.map(|d| d.ptr()).unwrap_or(null()));
            if dc.is_null() {
                return None;
            }
            let p = paper_of(dc);
            DeleteDC(dc);
            p
        }
    }

    struct Dc(HDC);

    impl Drop for Dc {
        fn drop(&mut self) {
            unsafe { DeleteDC(self.0) };
        }
    }

    /// Печатает задание. `sizes` — размеры страниц документа в пт,
    /// `progress(лист, всего)`, `cancel` проверяется между полосами.
    /// Возвращает число напечатанных листов.
    pub fn print(
        job: &PrintJob,
        sizes: &[(f32, f32)],
        render: &mut RenderFn,
        progress: &mut dyn FnMut(usize, usize),
        cancel: &AtomicBool,
    ) -> Result<usize, PrintError> {
        let fail = |msg: &str| PrintError::Failed(msg.to_string());
        let mut dm = job.devmode.clone().or_else(|| default_devmode(&job.printer));
        if let Some(d) = dm.as_mut() {
            d.single_copy();
            if job.grayscale {
                d.set_grayscale(true);
            }
        }
        let name = wide(&job.printer);
        let raw = unsafe { CreateDCW(null(), name.as_ptr(), null(), dm.as_ref().map(|d| d.ptr()).unwrap_or(null())) };
        if raw.is_null() {
            return Err(fail("не удалось открыть принтер"));
        }
        let dc = Dc(raw);
        let paper = paper_of(dc.0).ok_or_else(|| fail("принтер не сообщил размер бумаги"))?;
        let doc_name = wide(&job.doc_name);
        let output = job.output.as_ref().map(|p| wide(&p.to_string_lossy()));
        let info = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: doc_name.as_ptr(),
            lpszOutput: output.as_ref().map(|o| o.as_ptr()).unwrap_or(null()),
            lpszDatatype: null(),
            fwType: 0,
        };
        if unsafe { StartDocW(dc.0, &info) } <= 0 {
            // «Печать в PDF»: пользователь закрыл окно выбора файла.
            return Err(match std::io::Error::last_os_error().raw_os_error() {
                Some(ERROR_CANCELLED) => PrintError::Cancelled,
                _ => fail("принтер отказался начинать печать"),
            });
        }
        unsafe { SetStretchBltMode(dc.0, HALFTONE) };

        let sheets = job.sheets();
        let abort = |e: PrintError| {
            unsafe { AbortDoc(dc.0) };
            e
        };
        for (i, &page) in sheets.iter().enumerate() {
            progress(i, sheets.len());
            if cancel.load(Ordering::Relaxed) {
                return Err(abort(PrintError::Cancelled));
            }
            let size = sizes.get(page).copied().ok_or_else(|| abort(fail("нет такой страницы")))?;
            let place = place(size, &paper, job.scaling, job.auto_rotate);
            let (k, bands) = bands(&place, &paper);
            // Масштаб рендера: px растра на пункт страницы.
            let scale = k * place.rect.w / if place.rotation & 1 == 1 { size.1 } else { size.0 };
            if unsafe { StartPage(dc.0) } <= 0 {
                return Err(abort(fail("ошибка драйвера принтера (StartPage)")));
            }
            for (src, dst) in bands {
                if cancel.load(Ordering::Relaxed) {
                    return Err(abort(PrintError::Cancelled));
                }
                let mut img = render(page, scale, src[0], src[1], src[2], src[3], place.rotation).map_err(|e| abort(PrintError::Failed(e)))?;
                to_bgra(&mut img.rgba, job.grayscale);
                let bmi = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: img.width as i32,
                        biHeight: -(img.height as i32), // сверху вниз
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        biSizeImage: 0,
                        biXPelsPerMeter: 0,
                        biYPelsPerMeter: 0,
                        biClrUsed: 0,
                        biClrImportant: 0,
                    },
                    bmiColors: [unsafe { std::mem::zeroed() }],
                };
                let ok = unsafe {
                    StretchDIBits(
                        dc.0,
                        dst[0],
                        dst[1],
                        dst[2],
                        dst[3],
                        0,
                        0,
                        img.width as i32,
                        img.height as i32,
                        img.rgba.as_ptr() as *const c_void,
                        &bmi,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    )
                };
                if ok == 0 {
                    return Err(abort(fail("драйвер принтера не принял изображение страницы")));
                }
            }
            if unsafe { EndPage(dc.0) } <= 0 {
                return Err(abort(fail("ошибка драйвера принтера (EndPage)")));
            }
        }
        if unsafe { EndDoc(dc.0) } <= 0 {
            return Err(fail("ошибка драйвера принтера (EndDoc)"));
        }
        progress(sheets.len(), sheets.len());
        Ok(sheets.len())
    }
}

#[cfg(not(windows))]
pub use stub::*;

/// Вне Windows печати нет: диалог покажет пустой список принтеров.
#[cfg(not(windows))]
mod stub {
    use super::*;

    #[derive(Clone)]
    pub struct DevMode;

    impl DevMode {
        pub fn orientation(&self) -> Orientation {
            Orientation::Portrait
        }
        pub fn set_orientation(&mut self, _: Orientation) {}
        pub fn paper(&self) -> i16 {
            0
        }
        pub fn set_paper(&mut self, _: i16) {}
        pub fn duplex(&self) -> Duplex {
            Duplex::Simplex
        }
        pub fn set_duplex(&mut self, _: Duplex) {}
        pub fn grayscale(&self) -> bool {
            false
        }
        pub fn set_grayscale(&mut self, _: bool) {}
    }

    #[derive(Clone, Default)]
    pub struct PrinterCaps {
        pub papers: Vec<(i16, String)>,
        pub duplex: bool,
        pub color: bool,
    }

    pub fn list_printers() -> Vec<String> {
        Vec::new()
    }
    pub fn default_printer() -> Option<String> {
        None
    }
    pub fn default_devmode(_: &str) -> Option<DevMode> {
        None
    }
    pub fn properties_dialog(_: &str, _: &mut DevMode) -> bool {
        false
    }
    pub fn printer_caps(_: &str, _: Option<&DevMode>) -> PrinterCaps {
        PrinterCaps::default()
    }
    pub fn paper_info(_: &str, _: Option<&DevMode>) -> Option<Paper> {
        None
    }
    pub fn print(
        _: &PrintJob,
        _: &[(f32, f32)],
        _: &mut RenderFn,
        _: &mut dyn FnMut(usize, usize),
        _: &AtomicBool,
    ) -> Result<usize, PrintError> {
        Err(PrintError::Failed("печать поддерживается только в Windows".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse_singles_spans_and_open_ends() {
        assert_eq!(parse_ranges("1-3, 5", 10).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(parse_ranges(" 8- ", 10).unwrap(), vec![7, 8, 9]);
        assert_eq!(parse_ranges("-2;4", 10).unwrap(), vec![0, 1, 3]);
        assert_eq!(parse_ranges("5–3", 10).unwrap(), vec![4, 3, 2]);
        assert!(parse_ranges("0", 10).is_err());
        assert!(parse_ranges("11", 10).is_err());
        assert!(parse_ranges("abc", 10).is_err());
        assert!(parse_ranges(" , ", 10).is_err());
    }

    #[test]
    fn select_applies_subset_by_page_number_and_reverse() {
        let odd = select_pages(PageSet::All, 0, "", Subset::Odd, false, 5).unwrap();
        assert_eq!(odd, vec![0, 2, 4]); // стр. 1, 3, 5
        let even = select_pages(PageSet::Range, 0, "2-5", Subset::Even, true, 5).unwrap();
        assert_eq!(even, vec![3, 1]); // стр. 4, 2
        assert_eq!(select_pages(PageSet::Current, 7, "", Subset::All, false, 5).unwrap(), vec![4]);
        assert!(select_pages(PageSet::Current, 0, "", Subset::Even, false, 5).is_err()); // стр. 1 нечётная
    }

    #[test]
    fn collate_orders_sheets() {
        let mut job = PrintJob {
            printer: String::new(),
            devmode: None,
            doc_name: String::new(),
            pages: vec![0, 1],
            copies: 2,
            collate: true,
            scaling: Scaling::Fit,
            auto_rotate: true,
            grayscale: false,
            annotations: true,
            output: None,
        };
        assert_eq!(job.sheets(), vec![0, 1, 0, 1]);
        job.collate = false;
        assert_eq!(job.sheets(), vec![0, 0, 1, 1]);
    }

    fn letterish() -> Paper {
        // 600 × 800 pt, поля по 20 pt.
        Paper { width: 600.0, height: 800.0, area: PtRect { x: 20.0, y: 20.0, w: 560.0, h: 760.0 }, dpi_x: 600.0, dpi_y: 600.0 }
    }

    #[test]
    fn fit_centers_in_printable_area() {
        let p = place((280.0, 380.0), &letterish(), Scaling::Fit, true);
        assert_eq!(p.rotation, 0);
        assert!((p.rect.w - 560.0).abs() < 0.01 && (p.rect.h - 760.0).abs() < 0.01);
        assert!((p.rect.x - 20.0).abs() < 0.01 && (p.rect.y - 20.0).abs() < 0.01);
    }

    #[test]
    fn auto_rotate_turns_landscape_page_on_portrait_paper() {
        let p = place((800.0, 400.0), &letterish(), Scaling::Fit, true);
        assert_eq!(p.rotation, 1);
        assert!(p.rect.h > p.rect.w);
        let q = place((800.0, 400.0), &letterish(), Scaling::Fit, false);
        assert_eq!(q.rotation, 0);
        assert!(q.rect.w > q.rect.h);
    }

    #[test]
    fn shrink_keeps_small_pages_actual_size() {
        let small = place((100.0, 100.0), &letterish(), Scaling::Shrink, true);
        assert!((small.rect.w - 100.0).abs() < 0.01);
        let big = place((2000.0, 3000.0), &letterish(), Scaling::Shrink, true);
        assert!(big.rect.w <= 560.01 && big.rect.h <= 760.01);
        let pct = place((100.0, 100.0), &letterish(), Scaling::Custom(50.0), true);
        assert!((pct.rect.w - 50.0).abs() < 0.01);
    }

    #[test]
    fn bands_cover_visible_part_without_gaps() {
        let paper = letterish();
        // Фактический размер огромной страницы: видна только печатная область.
        let p = place((2000.0, 3000.0), &paper, Scaling::Actual, false);
        let (k, bands) = bands(&p, &paper);
        assert!((k - 600.0 / 72.0).abs() < 1e-4);
        assert!(bands.len() > 1, "должно быть несколько полос");
        let dev_w = (560.0f32 * 600.0 / 72.0).round() as i32;
        let dev_h = (760.0f32 * 600.0 / 72.0).round() as i32;
        let mut y = bands[0].1[1];
        for (src, dst) in &bands {
            assert!(src[2] as usize * src[3] as usize * 4 <= BAND_BYTES + src[2] as usize * 64);
            assert_eq!(dst[1], y, "дыра между полосами");
            y = dst[1] + dst[3];
            assert!((dst[2] - dev_w).abs() <= 2);
        }
        assert!((bands[0].1[1]).abs() <= 1 && (y - dev_h).abs() <= 2, "полосы не покрывают область: {y} vs {dev_h}");
        assert!(bands[0].1[0].abs() <= 1);
    }

    /// Сквозная печать через «Microsoft Print to PDF» в файл: реальный драйвер,
    /// спулер и GDI. На физические принтеры ничего не уходит.
    #[cfg(windows)]
    #[test]
    fn print_to_pdf_driver_end_to_end() {
        use pdfsmith_pdfium::{init, Document, RenderOpts};
        const VIRTUAL: &str = "Microsoft Print to PDF";
        let _serial = crate::render_thread::pdfium_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let src = root.join("test_pdfs").join("text.pdf");
        if !root.join("pdfium.dll").exists() || !src.exists() || !list_printers().iter().any(|p| p == VIRTUAL) {
            eprintln!("ПРОПУСК: нет pdfium.dll, test_pdfs/text.pdf или «{VIRTUAL}»");
            return;
        }
        init(Some(&root)).unwrap();

        let dm = default_devmode(VIRTUAL).expect("DEVMODE виртуального принтера");
        let caps = printer_caps(VIRTUAL, Some(&dm));
        assert!(!caps.papers.is_empty(), "драйвер не отдал список бумаги");
        let paper = paper_info(VIRTUAL, Some(&dm)).expect("размер листа");
        assert!(paper.width > 500.0 && paper.height > 500.0, "{paper:?}");

        let doc = Document::open(&src, None).unwrap();
        let sizes: Vec<(f32, f32)> = (0..doc.page_count()).map(|i| doc.page_size(i).map(|s| (s.width_pt, s.height_pt)).unwrap()).collect();
        let out = std::env::temp_dir().join(format!("pdfsmith-print-{}.pdf", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let job = PrintJob {
            printer: VIRTUAL.into(),
            devmode: Some(dm),
            doc_name: "pdfsmith test".into(),
            pages: vec![1, 0],
            copies: 1,
            collate: true,
            scaling: Scaling::Fit,
            auto_rotate: true,
            grayscale: false,
            annotations: true,
            output: Some(out.clone()),
        };
        let mut render = |page: usize, scale: f32, x: i32, y: i32, w: i32, h: i32, rot: u8| {
            doc.load_page(page).unwrap().render_region_opts(scale, x, y, w, h, rot, RenderOpts { printing: true, ..RenderOpts::default() }).map_err(|e| e.to_string())
        };
        let mut steps = Vec::new();
        let n = print(&job, &sizes, &mut render, &mut |i, n| steps.push((i, n)), &AtomicBool::new(false)).unwrap();
        assert_eq!(n, 2);
        assert_eq!(steps.last(), Some(&(2, 2)));

        // Спулер дописывает файл асинхронно.
        let t0 = std::time::Instant::now();
        let printed = loop {
            if let Ok(d) = Document::open(&out, None) {
                break d;
            }
            assert!(t0.elapsed().as_secs() < 60, "PDF от драйвера так и не появился: {}", out.display());
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        assert_eq!(printed.page_count(), 2);
        let img = printed.render_page_scaled(0, 1.0).unwrap();
        let dark = img.rgba.chunks(4).filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100).count();
        assert!(dark > 2000, "напечатанная страница пустая: тёмных пикселей {dark}");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn to_bgra_swaps_and_grays() {
        let mut px = vec![10, 20, 30, 255];
        to_bgra(&mut px, false);
        assert_eq!(px, vec![30, 20, 10, 255]);
        let mut g = vec![255, 0, 0, 255];
        to_bgra(&mut g, true);
        assert_eq!(g[0], g[1]);
        assert_eq!(g[1], g[2]);
    }
}
