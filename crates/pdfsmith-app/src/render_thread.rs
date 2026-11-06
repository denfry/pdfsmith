//! Постоянный фоновый поток PDFium.
//!
//! PDFium живёт целиком в одном потоке (его нельзя звать из нескольких).
//! Поток открывает документ, держит LRU загруженных страниц (загрузка дорога —
//! платится редко), рендерит тайлы (сначала пробуя дисковый кэш), выполняет
//! редактирование, поиск, сохранение и конвертацию. Между тайлами проверяет,
//! не пришёл ли более свежий запрос (отмена устаревших задач).

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use eframe::egui;
use pdfsmith_engine::disk_cache::{self, file_id, DiskCache, Tile};
use pdfsmith_pdfium::{init, Document, Page, PdfRect, RenderOpts, RenderedImage, Rgba, TextChange};
use pdfsmith_sign::{sign_pdf, store, CertInfo, SignError, SignField, SignInfo};

use crate::office;
use crate::tables;
use crate::print::{self, PrintError, PrintJob};
use crate::print_ui::PREVIEW_PX;

/// Сторона тайла в device-пикселях. На тяжёлых чертежах PDFium платит за
/// обход всех объектов страницы при каждом вызове, поэтому 512 вдвое
/// дешевле 256 на ту же площадь (см. docs/perf-findings.md).
pub const TILE: u32 = 512;
/// Длинная сторона миниатюры (боковая панель) в пикселях.
pub const THUMB_PX: f32 = 220.0;
/// Сколько загруженных страниц держим одновременно.
const LOADED_PAGES: usize = 4;
/// Сколько снимков документа держать для отмены правок текста.
const SNAPSHOTS: usize = 15;
/// Сентинел LOD для миниатюр в дисковом кэше.
const THUMB_LOD: i32 = i32::MIN + 1;
/// Версия формата кэша: меняется при изменении рендера (например, аннотации).
const CACHE_VERSION: &str = "v4";
/// Тайл быстрее этого порога дешевле перерисовать, чем писать и читать с диска.
const DISK_WORTH_MS: u128 = 8;
/// Предел размера дискового кэша (чистится в фоне при запуске).
const CACHE_MAX_BYTES: u64 = 1 << 30;

/// Прямоугольник в «отображаемых» пунктах страницы: origin слева-сверху,
/// координаты уже с учётом поворота просмотра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DispRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Тайлы одной страницы в запросе.
pub struct PageTiles {
    pub page: usize,
    /// Размер повёрнутой страницы в пунктах.
    pub page_pt: (f32, f32),
    /// Координаты тайлов (col, row) в порядке приоритета.
    pub tiles: Vec<(u32, u32)>,
}

pub enum EditOp {
    DeletePage(usize),
    RotatePage { page: usize, quarters: i32 },
    MovePage { from: usize, to: usize },
    InsertBlank { at: usize },
    InsertPdf { path: PathBuf, at: usize },
    InsertImages { paths: Vec<PathBuf>, at: usize },
    Ink { page: usize, points: Vec<(f32, f32)>, color: Rgba, width: f32 },
    Rect { page: usize, rect: DispRect, color: Rgba, width: f32 },
    Highlight { page: usize, rect: DispRect, color: Rgba },
    Note { page: usize, x: f32, y: f32, text: String, color: Rgba },
    UndoAnnotation(usize),
    /// Заменить текст объекта `index`; `expect` — текст, который видел пользователь.
    /// Пустой `text` удаляет объект.
    ReplaceText { page: usize, index: usize, expect: String, text: String },
    /// Новый текст: (x, y) — левый верхний угол в display pt текущего поворота.
    AddText { page: usize, x: f32, y: f32, text: String, size: f32, color: Rgba },
    /// Сдвиг объекта на (dx, dy) display pt текущего поворота.
    MoveObject { page: usize, index: usize, dx: f32, dy: f32 },
    /// Отменить последнюю правку (пометку или правку текста); если истории
    /// нет — последнюю аннотацию страницы `page`.
    Undo { page: usize },
}

/// Текстовый объект для редактора: рамка в display pt при повороте 0.
#[derive(Clone, Debug)]
pub struct TextBox {
    pub index: usize,
    pub text: String,
    pub rect: DispRect,
    pub size: f32,
    pub color: Rgba,
}

pub enum ImageFormat {
    Png,
    Jpeg,
}

pub enum ConvertOp {
    PdfToImages { src: PathBuf, out_dir: PathBuf, format: ImageFormat, dpi: f32 },
    PdfToText { src: PathBuf, out: PathBuf },
    ImagesToPdf { srcs: Vec<PathBuf>, out: PathBuf },
    TextToPdf { src: PathBuf, out: PathBuf },
    /// PDF и документы Office — в один PDF (Office-файлы сначала конвертируются).
    Merge { srcs: Vec<PathBuf>, out: PathBuf },
    Split { src: PathBuf, out_dir: PathBuf },
    /// Word/Excel/PowerPoint → PDF; один файл — в `out`, несколько — в папку `out`.
    OfficeToPdf { srcs: Vec<PathBuf>, out: PathBuf },
    /// Документ Office открыть в просмотрщике: конвертировать во временный PDF.
    OpenOffice { src: PathBuf },
    PdfToWord { src: PathBuf, out: PathBuf },
    PdfToExcel { src: PathBuf, out: PathBuf },
    PdfToPowerPoint { src: PathBuf, out: PathBuf },
}

/// Подпись ЭП: видимый штамп в `rect` на странице `page`, результат — в `out`.
pub struct SignJob {
    pub page: usize,
    /// Поворот просмотра, в котором нарисован `rect`.
    pub rotation: u8,
    pub rect: DispRect,
    pub cert: CertInfo,
    pub reason: String,
    pub location: String,
    pub out: PathBuf,
}

/// Команда из UI в рендер-поток.
pub enum Job {
    Open(PathBuf),
    /// `gen` — поколение документа в UI; события с чужим поколением отбрасываются.
    Tiles { gen: u32, rotation: u8, lod: i32, lod_scale: f32, pages: Vec<PageTiles> },
    Thumbnail { gen: u32, page: usize, rotation: u8 },
    ExportPng { path: PathBuf, page: usize, rotation: u8 },
    Search(String),
    /// Текст внутри прямоугольника → в буфер обмена.
    CopyText { page: usize, rotation: u8, rect: DispRect },
    /// Редактирование; `rotation` — поворот просмотра для перевода координат.
    Edit { op: EditOp, rotation: u8 },
    Save(PathBuf),
    Convert(ConvertOp),
    /// Растр страницы для предпросмотра печати.
    PrintPreview { page: usize, annotations: bool },
    /// Печать; `cancel` взводит UI.
    Print { job: PrintJob, cancel: Arc<AtomicBool> },
    Sign(Box<SignJob>),
    /// Текстовые объекты страницы для режима правки текста.
    TextObjects { gen: u32, page: usize },
}

/// Одно вхождение поиска: страница, прямоугольники (display pt при повороте 0).
pub struct SearchHit {
    pub page: usize,
    pub rects: Vec<DispRect>,
    pub snippet: String,
}

/// Событие из рендер-потока в UI.
pub enum Event {
    Opened { path: PathBuf, page_sizes: Vec<(f32, f32)> },
    /// Структура документа изменилась: сбросить все кэши.
    Changed { page_sizes: Vec<(f32, f32)> },
    /// Изменились аннотации одной страницы: перерисовать только её.
    PageChanged { page: usize },
    PageLoading(bool),
    Tile { gen: u32, page: usize, rotation: u8, lod: i32, col: u32, row: u32, width: u32, height: u32, rgba: Vec<u8> },
    Thumbnail { gen: u32, page: usize, rotation: u8, width: u32, height: u32, rgba: Vec<u8> },
    Saved(PathBuf),
    SearchResults { query: String, hits: Vec<SearchHit> },
    TextCopied(String),
    Progress { msg: String, frac: f32 },
    Done(String),
    Error(String),
    PrintPreview { page: usize, annotations: bool, width: u32, height: u32, rgba: Vec<u8> },
    /// Печать закончилась: число отправленных листов или причина.
    PrintFinished(Result<usize, PrintError>),
    /// Документ Office сконвертирован для просмотра: временный `pdf` из `source`.
    OfficeOpened { pdf: PathBuf, source: PathBuf },
    TextObjects { gen: u32, page: usize, boxes: Vec<TextBox> },
}

pub struct RenderHandle {
    pub job_tx: Sender<Job>,
    pub event_rx: Receiver<Event>,
}

/// Запускает рендер-поток. `dll_dir` — где искать `pdfium.dll`.
pub fn spawn(ctx: egui::Context, dll_dir: Option<PathBuf>) -> RenderHandle {
    let (job_tx, job_rx) = channel::<Job>();
    let (event_tx, event_rx) = channel::<Event>();
    thread::Builder::new()
        .name("pdfium".into())
        .spawn(move || worker(job_rx, event_tx, ctx, dll_dir))
        .expect("рендер-поток");
    // Уборка дискового кэша — в фоне, запуск не ждёт.
    let _ = thread::Builder::new().name("cache-prune".into()).spawn(|| {
        let n = disk_cache::prune(&cache_root(), &format!("-{CACHE_VERSION}"), CACHE_MAX_BYTES, std::time::Duration::from_secs(3600));
        if n > 0 {
            log::info!("дисковый кэш: удалено каталогов {n}");
        }
    });
    RenderHandle { job_tx, event_rx }
}

/// Корень дискового кэша: `%LOCALAPPDATA%\pdfsmith\cache` либо temp.
fn cache_root() -> PathBuf {
    // Тесты не засоряют пользовательский кэш.
    if cfg!(test) {
        return std::env::temp_dir().join("pdfsmith-test-cache");
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local).join("pdfsmith").join("cache")
    } else {
        std::env::temp_dir().join("pdfsmith-cache")
    }
}

/// Перевод точки из повёрнутого отображения в отображение при повороте 0.
/// `(w, h)` — размер неповёрнутой страницы.
pub fn unrotate(x: f32, y: f32, rotation: u8, (w, h): (f32, f32)) -> (f32, f32) {
    match rotation & 3 {
        0 => (x, y),
        1 => (y, h - x),
        2 => (w - x, h - y),
        _ => (w - y, x),
    }
}

/// Обратно: из отображения при повороте 0 в повёрнутое.
pub fn rotate_pt(x: f32, y: f32, rotation: u8, (w, h): (f32, f32)) -> (f32, f32) {
    match rotation & 3 {
        0 => (x, y),
        1 => (h - y, x),
        2 => (w - x, h - y),
        _ => (y, w - x),
    }
}

/// Шаг отмены.
enum UndoStep {
    /// Добавлена аннотация на страницу.
    Annot(usize),
    /// Правка содержимого: байты документа до неё.
    Content(Vec<u8>),
}

struct Worker {
    event_tx: Sender<Event>,
    ctx: egui::Context,
    doc: Option<Document>,
    path: Option<PathBuf>,
    disk: Option<DiskCache>,
    /// LRU загруженных страниц: последняя использованная — в конце.
    loaded: Vec<(usize, Page)>,
    dirty: bool,
    /// Страницы с несохранёнными аннотациями: дисковый кэш для них устарел.
    dirty_pages: HashSet<usize>,
    /// История правок для Ctrl+Z (только успешные правки).
    history: Vec<UndoStep>,
}

fn worker(job_rx: Receiver<Job>, event_tx: Sender<Event>, ctx: egui::Context, dll_dir: Option<PathBuf>) {
    log::info!("рендер-поток: init pdfium, dll_dir={dll_dir:?}");
    if let Err(e) = init(dll_dir.as_deref()) {
        log::error!("init pdfium провалился: {e}");
        let _ = event_tx.send(Event::Error(e.to_string()));
        ctx.request_repaint();
        return;
    }

    let mut w = Worker { event_tx, ctx, doc: None, path: None, disk: None, loaded: Vec::new(), dirty: false, dirty_pages: HashSet::new(), history: Vec::new() };
    // Очередь задач, снятых с канала раньше времени (см. схлопывание тайлов).
    let mut pending: VecDeque<Job> = VecDeque::new();

    loop {
        let job = match pending.pop_front() {
            Some(j) => j,
            None => match job_rx.recv() {
                Ok(j) => j,
                Err(_) => return, // UI закрылся
            },
        };
        // Миниатюра — фоновая и непрерываемая: пропускаем вперёд всё, что уже
        // лежит в канале (тайлы видимой области, правки).
        let mut job = job;
        while matches!(job, Job::Thumbnail { .. }) {
            match job_rx.try_recv() {
                Ok(next) => {
                    pending.push_back(job);
                    job = next;
                }
                Err(_) => break,
            }
        }
        // Схлопываем очередь тайлов: из нескольких запросов тайлов интересен
        // только самый свежий, а остальные задачи выполняются раньше него.
        let job = if matches!(job, Job::Tiles { .. }) {
            let mut latest = job;
            let mut others = Vec::new();
            while let Ok(next) = job_rx.try_recv() {
                if matches!(next, Job::Tiles { .. }) {
                    latest = next;
                } else {
                    others.push(next);
                }
            }
            if others.is_empty() {
                latest
            } else {
                let first = others.remove(0);
                for j in others {
                    pending.push_back(j);
                }
                pending.push_back(latest);
                first
            }
        } else {
            job
        };

        match job {
            Job::Open(path) => w.open(path),
            Job::ExportPng { path, page, rotation } => w.export_png(path, page, rotation),
            Job::Thumbnail { gen, page, rotation } => w.thumbnail(gen, page, rotation),
            Job::Tiles { gen, rotation, lod, lod_scale, pages } => {
                w.render_tiles(&job_rx, &mut pending, gen, rotation, lod, lod_scale, pages);
            }
            Job::Search(q) => w.search(q),
            Job::CopyText { page, rotation, rect } => w.copy_text(page, rotation, rect),
            Job::Edit { op, rotation } => {
                if let Err(e) = w.edit(op, rotation) {
                    w.emit(Event::Error(e));
                }
            }
            Job::Save(path) => w.save(path),
            Job::Convert(op) => {
                if let Err(e) = w.convert(op) {
                    w.emit(Event::Error(e));
                }
            }
            Job::PrintPreview { page, annotations } => w.print_preview(page, annotations),
            Job::Print { job, cancel } => w.print(job, &cancel),
            Job::Sign(job) => {
                if let Err(e) = w.sign(*job) {
                    w.emit(Event::Error(e));
                }
            }
            Job::TextObjects { gen, page } => w.text_objects(gen, page),
        }
    }
}

impl Worker {
    fn emit(&self, ev: Event) {
        let _ = self.event_tx.send(ev);
        self.ctx.request_repaint();
    }

    fn progress(&self, msg: impl Into<String>, frac: f32) {
        self.emit(Event::Progress { msg: msg.into(), frac });
    }

    fn sizes(&self) -> Vec<(f32, f32)> {
        let Some(d) = &self.doc else { return Vec::new() };
        (0..d.page_count())
            .map(|i| d.page_size(i).map(|s| (s.width_pt, s.height_pt)).unwrap_or((1.0, 1.0)))
            .collect()
    }

    /// Гарантирует, что страница загружена; возвращает индекс в LRU.
    fn ensure_loaded(&mut self, page: usize) -> Result<usize, String> {
        if let Some(pos) = self.loaded.iter().position(|(p, _)| *p == page) {
            if pos != self.loaded.len() - 1 {
                let item = self.loaded.remove(pos);
                self.loaded.push(item);
            }
            return Ok(self.loaded.len() - 1);
        }
        let doc = self.doc.as_ref().ok_or("документ не открыт")?;
        self.emit(Event::PageLoading(true));
        let pg = doc.load_page(page).map_err(|e| e.to_string());
        self.emit(Event::PageLoading(false));
        let pg = pg?;
        if self.loaded.len() >= LOADED_PAGES {
            self.loaded.remove(0);
        }
        self.loaded.push((page, pg));
        Ok(self.loaded.len() - 1)
    }

    fn page(&mut self, page: usize) -> Result<&Page, String> {
        let i = self.ensure_loaded(page)?;
        Ok(&self.loaded[i].1)
    }

    /// Доступ к странице для фоновых задач (миниатюры, поиск): берёт уже
    /// загруженную, иначе грузит временно, не вытесняя из LRU страницы,
    /// которые сейчас смотрит пользователь.
    fn with_page<R>(&self, page: usize, f: impl FnOnce(&Page) -> R) -> Result<R, String> {
        if let Some((_, pg)) = self.loaded.iter().find(|(p, _)| *p == page) {
            return Ok(f(pg));
        }
        let doc = self.doc.as_ref().ok_or("документ не открыт")?;
        let pg = doc.load_page(page).map_err(|e| e.to_string())?;
        Ok(f(&pg))
    }

    /// Дисковый кэш страницы, если он ещё соответствует её содержимому.
    fn disk_for(&self, page: usize) -> Option<&DiskCache> {
        if self.dirty_pages.contains(&page) {
            None
        } else {
            self.disk.as_ref()
        }
    }

    /// После структурной правки: страницы перезагрузить, кэш тайлов недействителен.
    fn mark_changed(&mut self) {
        self.loaded.clear();
        self.disk = None;
        self.dirty_pages.clear();
        self.dirty = true;
        let sizes = self.sizes();
        self.emit(Event::Changed { page_sizes: sizes });
    }

    /// Рендерит страницу целиком в масштабе с поворотом.
    fn render_full(&mut self, page: usize, rotation: u8, scale: f32) -> Result<RenderedImage, String> {
        let size = self.doc.as_ref().and_then(|d| d.page_size(page)).ok_or("нет такой страницы")?;
        let (ew, eh) = if rotation & 1 == 1 { (size.height_pt, size.width_pt) } else { (size.width_pt, size.height_pt) };
        let w = ((ew * scale).round() as i32).max(1);
        let h = ((eh * scale).round() as i32).max(1);
        self.page(page)?.render_region(scale, 0, 0, w, h, rotation, true).map_err(|e| e.to_string())
    }

    fn export_png(&mut self, path: PathBuf, page: usize, rotation: u8) {
        let Some(size) = self.doc.as_ref().and_then(|d| d.page_size(page)) else {
            self.emit(Event::Error("нет такой страницы".into()));
            return;
        };
        let longest = size.width_pt.max(size.height_pt).max(1.0);
        let scale = (200.0 / 72.0_f32).min(10000.0 / longest);
        match self.render_full(page, rotation, scale) {
            Ok(img) => match save_image(&img, &path, &ImageFormat::Png) {
                Ok(()) => self.emit(Event::Done(format!("Сохранено: {}", path.display()))),
                Err(e) => self.emit(Event::Error(e)),
            },
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    fn thumbnail(&mut self, gen: u32, page: usize, rotation: u8) {
        if let Some(t) = self.disk_for(page).and_then(|d| d.get(page, rotation, THUMB_LOD, 0, 0)) {
            self.emit(Event::Thumbnail { gen, page, rotation, width: t.width, height: t.height, rgba: t.rgba });
            return;
        }
        let Some(size) = self.doc.as_ref().and_then(|d| d.page_size(page)) else { return };
        let longest = size.width_pt.max(size.height_pt).max(1.0);
        let scale = THUMB_PX / longest;
        let (ew, eh) = if rotation & 1 == 1 { (size.height_pt, size.width_pt) } else { (size.width_pt, size.height_pt) };
        let w = ((ew * scale).round() as i32).max(1);
        let h = ((eh * scale).round() as i32).max(1);
        let res = self
            .with_page(page, |pg| pg.render_region(scale, 0, 0, w, h, rotation, true).map_err(|e| e.to_string()))
            .and_then(|r| r);
        match res {
            Ok(img) => {
                if let Some(d) = self.disk_for(page) {
                    d.put(page, rotation, THUMB_LOD, 0, 0, &Tile { width: img.width, height: img.height, rgba: img.rgba.clone() });
                }
                self.emit(Event::Thumbnail { gen, page, rotation, width: img.width, height: img.height, rgba: img.rgba });
            }
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    fn open(&mut self, path: PathBuf) {
        log::info!("открываю: {}", path.display());
        match Document::open(&path, None) {
            Ok(d) => {
                let id = format!("{}-{CACHE_VERSION}", file_id(&path));
                let disk = DiskCache::new(&cache_root(), &id);
                log::info!("документ открыт: страниц={}, диск-кэш={}", d.page_count(), disk.is_enabled());
                // Сначала закрываем страницы старого документа, потом сам документ.
                self.loaded.clear();
                self.dirty_pages.clear();
                self.history.clear();
                self.disk = Some(disk);
                self.doc = Some(d);
                self.dirty = false;
                self.path = Some(path.clone());
                let sizes = self.sizes();
                self.emit(Event::Opened { path, page_sizes: sizes });
            }
            Err(e) => {
                log::error!("открытие провалилось: {e}");
                self.emit(Event::Error(e.to_string()));
            }
        }
    }

    /// Рендерит тайлы, между тайлами проверяя входящие задания:
    /// - новый запрос тайлов вытесняет текущий (он уже включает всё недостающее);
    /// - миниатюры откладываются до конца — они фоновые;
    /// - прочее (правка, сохранение, поиск…) выполняется сразу, а недорисованный
    ///   остаток возвращается в очередь следом. Выбрасывать его нельзя: UI не
    ///   перезапросит тайлы, пока запрос не изменится, и страница останется белой.
    #[allow(clippy::too_many_arguments)]
    fn render_tiles(
        &mut self,
        job_rx: &Receiver<Job>,
        pending: &mut VecDeque<Job>,
        gen: u32,
        rotation: u8,
        lod: i32,
        lod_scale: f32,
        pages: Vec<PageTiles>,
    ) {
        let mut queue: VecDeque<PageTiles> = pages.into();
        let mut deferred: Vec<Job> = Vec::new();
        while let Some(mut pt) = queue.pop_front() {
            let page = pt.page;
            let page_w_px = (pt.page_pt.0 * lod_scale).round() as i32;
            let page_h_px = (pt.page_pt.1 * lod_scale).round() as i32;
            let mut i = 0;
            while i < pt.tiles.len() {
                match job_rx.try_recv() {
                    Ok(newer @ Job::Tiles { .. }) => {
                        pending.push_back(newer);
                        pending.extend(deferred);
                        return;
                    }
                    Ok(thumb @ Job::Thumbnail { .. }) => {
                        deferred.push(thumb);
                        continue;
                    }
                    Ok(other) => {
                        pt.tiles.drain(..i);
                        queue.push_front(pt);
                        pending.push_back(other);
                        pending.push_back(Job::Tiles { gen, rotation, lod, lod_scale, pages: queue.into() });
                        pending.extend(deferred);
                        return;
                    }
                    Err(_) => {}
                }
                let (col, row) = pt.tiles[i];
                i += 1;
                if let Some(t) = self.disk_for(page).and_then(|d| d.get(page, rotation, lod, col, row)) {
                    self.emit(Event::Tile { gen, page, rotation, lod, col, row, width: t.width, height: t.height, rgba: t.rgba });
                    continue;
                }
                let tx = (col * TILE) as i32;
                let ty = (row * TILE) as i32;
                let tw = (TILE as i32).min(page_w_px - tx);
                let th = (TILE as i32).min(page_h_px - ty);
                if tw <= 0 || th <= 0 {
                    continue;
                }
                let started = std::time::Instant::now();
                let res = match self.page(page) {
                    Ok(pg) => pg.render_region(lod_scale, tx, ty, tw, th, rotation, true).map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                let worth_caching = started.elapsed().as_millis() >= DISK_WORTH_MS;
                match res {
                    Ok(img) => {
                        if let Some(d) = self.disk_for(page).filter(|_| worth_caching) {
                            d.put(page, rotation, lod, col, row, &Tile { width: img.width, height: img.height, rgba: img.rgba.clone() });
                        }
                        self.emit(Event::Tile { gen, page, rotation, lod, col, row, width: img.width, height: img.height, rgba: img.rgba });
                    }
                    Err(e) => {
                        self.emit(Event::Error(e));
                        pending.extend(deferred);
                        return;
                    }
                }
            }
        }
        pending.extend(deferred);
    }

    fn search(&mut self, query: String) {
        let n = self.doc.as_ref().map(|d| d.page_count()).unwrap_or(0);
        let mut hits = Vec::new();
        for i in 0..n {
            let size = self.doc.as_ref().and_then(|d| d.page_size(i)).unwrap();
            self.progress(format!("Поиск: страница {}/{n}", i + 1), i as f32 / n.max(1) as f32);
            let _ = size;
            let page_hits = self.with_page(i, |pg| {
                pg.search(&query)
                    .into_iter()
                    .map(|h| {
                        let rects = h
                            .rects
                            .iter()
                            .map(|r| {
                                let (x0, y0) = pg.pdf_to_display(r.left, r.top);
                                let (x1, y1) = pg.pdf_to_display(r.right, r.bottom);
                                DispRect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) }
                            })
                            .collect();
                        SearchHit { page: i, rects, snippet: h.snippet }
                    })
                    .collect::<Vec<_>>()
            });
            let Ok(page_hits) = page_hits else { continue };
            hits.extend(page_hits);
            if hits.len() > 5000 {
                break;
            }
        }
        self.emit(Event::SearchResults { query, hits });
    }

    /// Прямоугольник display(повёрнутый) → PDF-пространство.
    fn disp_rect_to_pdf(&mut self, page: usize, rotation: u8, r: DispRect) -> Result<PdfRect, String> {
        let size = self.doc.as_ref().and_then(|d| d.page_size(page)).ok_or("нет страницы")?;
        let wh = (size.width_pt, size.height_pt);
        let pg = self.page(page)?;
        let a = unrotate(r.x0, r.y0, rotation, wh);
        let b = unrotate(r.x1, r.y1, rotation, wh);
        let (ax, ay) = pg.display_to_pdf(a.0, a.1);
        let (bx, by) = pg.display_to_pdf(b.0, b.1);
        Ok(PdfRect { left: ax.min(bx), bottom: ay.min(by), right: ax.max(bx), top: ay.max(by) })
    }

    fn text_objects(&mut self, gen: u32, page: usize) {
        let boxes = self.page(page).map(|pg| {
            pg.text_objects()
                .into_iter()
                .map(|o| {
                    let (x0, y0) = pg.pdf_to_display(o.rect.left, o.rect.top);
                    let (x1, y1) = pg.pdf_to_display(o.rect.right, o.rect.bottom);
                    TextBox {
                        index: o.index,
                        text: o.text,
                        rect: DispRect { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) },
                        size: o.size,
                        color: o.color,
                    }
                })
                .collect()
        });
        match boxes {
            Ok(boxes) => self.emit(Event::TextObjects { gen, page, boxes }),
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    /// Снимок документа перед правкой содержимого страницы.
    fn snapshot(&self) -> Result<Vec<u8>, String> {
        self.doc.as_ref().ok_or("документ не открыт")?.to_bytes().map_err(|e| e.to_string())
    }

    /// Запоминает снимок после успешной правки; старые снимки вытесняются.
    fn push_content(&mut self, bytes: Vec<u8>) {
        let snaps = self.history.iter().filter(|s| matches!(s, UndoStep::Content(_))).count();
        if snaps >= SNAPSHOTS {
            if let Some(i) = self.history.iter().position(|s| matches!(s, UndoStep::Content(_))) {
                self.history.remove(i);
            }
        }
        self.history.push(UndoStep::Content(bytes));
    }

    /// Точка display pt текущего поворота → PDF-пространство страницы.
    fn view_to_pdf(&mut self, page: usize, rotation: u8, x: f32, y: f32) -> Result<(f32, f32), String> {
        let size = self.doc.as_ref().and_then(|d| d.page_size(page)).ok_or("нет страницы")?;
        let (ux, uy) = unrotate(x, y, rotation, (size.width_pt, size.height_pt));
        Ok(self.page(page)?.display_to_pdf(ux, uy))
    }

    fn copy_text(&mut self, page: usize, rotation: u8, rect: DispRect) {
        match self.disp_rect_to_pdf(page, rotation, rect) {
            Ok(r) => {
                let text = self.page(page).map(|p| p.text_in_rect(r)).unwrap_or_default();
                self.emit(Event::TextCopied(text));
            }
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    fn edit(&mut self, op: EditOp, rotation: u8) -> Result<(), String> {
        if self.doc.is_none() {
            return Err("документ не открыт".into());
        }
        match op {
            EditOp::DeletePage(i) => {
                let d = self.doc.as_mut().unwrap();
                if d.page_count() <= 1 {
                    return Err("нельзя удалить единственную страницу".into());
                }
                self.loaded.clear();
                self.doc.as_mut().unwrap().delete_page(i);
            }
            EditOp::RotatePage { page, quarters } => {
                self.loaded.clear();
                self.doc.as_mut().unwrap().rotate_page(page, quarters).map_err(|e| e.to_string())?;
            }
            EditOp::MovePage { from, to } => {
                self.loaded.clear();
                self.doc.as_mut().unwrap().move_page(from, to).map_err(|e| e.to_string())?;
            }
            EditOp::InsertBlank { at } => {
                let d = self.doc.as_mut().unwrap();
                let (w, h) = d
                    .page_size(at.saturating_sub(1).min(d.page_count().saturating_sub(1)))
                    .map(|s| (s.width_pt, s.height_pt))
                    .unwrap_or((595.0, 842.0));
                self.loaded.clear();
                self.doc.as_mut().unwrap().insert_blank(at, w, h).map_err(|e| e.to_string())?;
            }
            EditOp::InsertPdf { path, at } => {
                let src = Document::open(&path, None).map_err(|e| e.to_string())?;
                self.loaded.clear();
                self.doc.as_mut().unwrap().import_pages(&src, &[], at).map_err(|e| e.to_string())?;
            }
            EditOp::InsertImages { paths, at } => {
                let mut tmp = Document::new().map_err(|e| e.to_string())?;
                for (i, p) in paths.iter().enumerate() {
                    self.progress(format!("Картинка {}/{}", i + 1, paths.len()), i as f32 / paths.len() as f32);
                    add_image_file(&mut tmp, p)?;
                }
                self.loaded.clear();
                self.doc.as_mut().unwrap().import_pages(&tmp, &[], at).map_err(|e| e.to_string())?;
            }
            EditOp::Ink { page, points, color, width } => {
                let size = self.doc.as_ref().and_then(|d| d.page_size(page)).ok_or("нет страницы")?;
                let wh = (size.width_pt, size.height_pt);
                let pg = self.page(page)?;
                let pts: Vec<(f32, f32)> = points
                    .iter()
                    .map(|&(x, y)| {
                        let (ux, uy) = unrotate(x, y, rotation, wh);
                        pg.display_to_pdf(ux, uy)
                    })
                    .collect();
                pg.add_ink(&pts, color, width).map_err(|e| e.to_string())?;
                self.history.push(UndoStep::Annot(page));
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::Rect { page, rect, color, width } => {
                let r = self.disp_rect_to_pdf(page, rotation, rect)?;
                self.page(page)?.add_rect(r, color, width).map_err(|e| e.to_string())?;
                self.history.push(UndoStep::Annot(page));
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::Highlight { page, rect, color } => {
                let r = self.disp_rect_to_pdf(page, rotation, rect)?;
                self.page(page)?.add_highlight(r, color).map_err(|e| e.to_string())?;
                self.history.push(UndoStep::Annot(page));
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::Note { page, x, y, text, color } => {
                let size = self.doc.as_ref().and_then(|d| d.page_size(page)).ok_or("нет страницы")?;
                let (ux, uy) = unrotate(x, y, rotation, (size.width_pt, size.height_pt));
                let pg = self.page(page)?;
                let (px, py) = pg.display_to_pdf(ux, uy);
                pg.add_note(px, py, &text, color).map_err(|e| e.to_string())?;
                self.history.push(UndoStep::Annot(page));
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::ReplaceText { page, index, expect, text } => {
                let snap = self.snapshot()?;
                let i = self.ensure_loaded(page)?;
                let doc = self.doc.as_ref().unwrap();
                let change = doc.replace_text(&self.loaded[i].1, index, &expect, &text).map_err(|e| e.to_string())?;
                if change == TextChange::NewFont {
                    self.emit(Event::Done("Текст изменён. В шрифте документа нет новых букв — взят похожий системный".into()));
                }
                self.push_content(snap);
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::AddText { page, x, y, text, size, color } => {
                // Базовая линия — на высоте асцента под точкой щелчка; оси строки
                // берём из преобразования, чтобы текст стоял ровно при любом повороте.
                let base = y + size * 0.9;
                let o = self.view_to_pdf(page, rotation, x, base)?;
                let rx = self.view_to_pdf(page, rotation, x + 100.0, base)?;
                let up = self.view_to_pdf(page, rotation, x, base - 100.0)?;
                let unit = |p: (f32, f32)| {
                    let (dx, dy) = (p.0 - o.0, p.1 - o.1);
                    let l = (dx * dx + dy * dy).sqrt().max(1e-3);
                    (dx / l, dy / l)
                };
                let snap = self.snapshot()?;
                let i = self.ensure_loaded(page)?;
                let doc = self.doc.as_ref().unwrap();
                doc.add_text(&self.loaded[i].1, o, unit(rx), unit(up), &text, size, color).map_err(|e| e.to_string())?;
                self.push_content(snap);
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::MoveObject { page, index, dx, dy } => {
                let a = self.view_to_pdf(page, rotation, 0.0, 0.0)?;
                let b = self.view_to_pdf(page, rotation, dx, dy)?;
                let snap = self.snapshot()?;
                self.page(page)?.move_object(index, b.0 - a.0, b.1 - a.1).map_err(|e| e.to_string())?;
                self.push_content(snap);
                self.mark_page_changed(page);
                return Ok(());
            }
            EditOp::Undo { page } => match self.history.pop() {
                Some(UndoStep::Content(bytes)) => {
                    let doc = Document::from_bytes(bytes, None).map_err(|e| e.to_string())?;
                    self.loaded.clear();
                    self.doc = Some(doc);
                    // Страницы те же — сброс кэшей, но история остаётся.
                    self.disk = None;
                    self.dirty_pages.clear();
                    self.dirty = true;
                    let sizes = self.sizes();
                    self.emit(Event::Changed { page_sizes: sizes });
                    return Ok(());
                }
                Some(UndoStep::Annot(p)) => {
                    self.page(p)?.remove_last_annotation();
                    self.mark_page_changed(p);
                    return Ok(());
                }
                None => {
                    if !self.page(page)?.remove_last_annotation() {
                        return Err("отменять нечего".into());
                    }
                    self.mark_page_changed(page);
                    return Ok(());
                }
            },
            EditOp::UndoAnnotation(page) => {
                if !self.page(page)?.remove_last_annotation() {
                    return Err("на странице нет аннотаций для отмены".into());
                }
                if let Some(i) = self.history.iter().rposition(|s| matches!(s, UndoStep::Annot(p) if *p == page)) {
                    self.history.remove(i);
                }
                self.mark_page_changed(page);
                return Ok(());
            }
        }
        // Структура изменилась: снимки и номера страниц в истории устарели.
        self.history.clear();
        self.mark_changed();
        Ok(())
    }

    /// После правки аннотаций: страница остаётся загруженной (PDFium строит
    /// список аннотаций заново при каждом рендере), остальной кэш цел.
    fn mark_page_changed(&mut self, page: usize) {
        self.dirty = true;
        self.dirty_pages.insert(page);
        self.emit(Event::PageChanged { page });
    }

    fn print_preview(&mut self, page: usize, annotations: bool) {
        let Some(size) = self.doc.as_ref().and_then(|d| d.page_size(page)) else { return };
        let scale = PREVIEW_PX / size.width_pt.max(size.height_pt).max(1.0);
        let w = ((size.width_pt * scale).round() as i32).max(1);
        let h = ((size.height_pt * scale).round() as i32).max(1);
        let opts = RenderOpts { annotations, printing: true, ..RenderOpts::default() };
        let res = self.page(page).and_then(|p| p.render_region_opts(scale, 0, 0, w, h, 0, opts).map_err(|e| e.to_string()));
        match res {
            Ok(img) => self.emit(Event::PrintPreview { page, annotations, width: img.width, height: img.height, rgba: img.rgba }),
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    /// Печать текущего состояния документа (с несохранёнными правками).
    fn print(&mut self, job: PrintJob, cancel: &AtomicBool) {
        let sizes = self.sizes();
        let (tx, ctx) = (self.event_tx.clone(), self.ctx.clone());
        let mut progress = |i: usize, n: usize| {
            let msg = format!("Печать: страница {} из {n}", (i + 1).min(n));
            let _ = tx.send(Event::Progress { msg, frac: i as f32 / n.max(1) as f32 });
            ctx.request_repaint();
        };
        let opts = RenderOpts { annotations: job.annotations, printing: true, ..RenderOpts::default() };
        let mut render = |page: usize, scale: f32, x: i32, y: i32, w: i32, h: i32, rot: u8| -> Result<RenderedImage, String> {
            self.page(page)?.render_region_opts(scale, x, y, w, h, rot, opts).map_err(|e| e.to_string())
        };
        log::info!("печать: {} стр. × {} на «{}»", job.pages.len(), job.copies, job.printer);
        let res = print::print(&job, &sizes, &mut render, &mut progress, cancel);
        if let Err(e) = &res {
            log::warn!("печать не удалась: {e:?}");
        }
        self.emit(Event::PrintFinished(res));
    }

    fn save(&mut self, path: PathBuf) {
        let Some(doc) = &self.doc else {
            self.emit(Event::Error("документ не открыт".into()));
            return;
        };
        match doc.save_to(&path) {
            Ok(()) => {
                self.dirty = false;
                self.dirty_pages.clear();
                self.path = Some(path.clone());
                let id = format!("{}-{CACHE_VERSION}", file_id(&path));
                self.disk = Some(DiskCache::new(&cache_root(), &id));
                self.emit(Event::Saved(path));
            }
            Err(e) => self.emit(Event::Error(e.to_string())),
        }
    }

    fn sign(&mut self, job: SignJob) -> Result<(), String> {
        let r = self.disp_rect_to_pdf(job.page, job.rotation, job.rect)?;
        let page_rotation = self.page(job.page)?.rotation();
        let doc = self.doc.as_ref().ok_or("документ не открыт")?;
        // Без правок подписываем файл как есть — прежние подписи остаются
        // валидными; с правками — свежую копию из PDFium.
        let base = match (&self.path, self.dirty) {
            (Some(p), false) => std::fs::read(p).map_err(|e| e.to_string())?,
            _ => doc.to_bytes().map_err(|e| e.to_string())?,
        };
        let font = load_font()?;
        let now = store::now();
        let c = &job.cert;
        let mut lines = vec!["Подписано электронной подписью".to_string(), c.owner.clone()];
        if !c.organization.is_empty() && c.organization != c.owner {
            lines.push(c.organization.clone());
        }
        lines.push(format!("Сертификат: {}", c.serial));
        lines.push(format!("Действителен с {} по {}", c.valid_from, c.valid_to));
        lines.push(format!("Дата: {}", now.local.display()));
        if !job.reason.trim().is_empty() {
            lines.push(format!("Причина: {}", job.reason.trim()));
        }
        let field = SignField { page: job.page, rect: [r.left, r.bottom, r.right, r.top], page_rotation, lines };
        let info = SignInfo { name: c.owner.clone(), reason: job.reason.clone(), location: job.location.clone(), time: now.local };
        self.progress("Подпись… введите PIN-код токена, если его спросят", 0.5);
        let attempt = |base: Vec<u8>| {
            sign_pdf(base, &field, &info, &font, |chunks| store::sign_detached(&c.thumbprint, chunks, now.utc_filetime))
        };
        let signed = match attempt(base) {
            // Файл, который не разобрал lopdf, нормализуем через PDFium.
            Err(SignError::Parse(e)) => {
                log::warn!("подпись: исходный файл не разобран ({e}), пересохраняю через PDFium");
                attempt(doc.to_bytes().map_err(|e| e.to_string())?)
            }
            r => r,
        }
        .map_err(|e| e.to_string())?;

        let tmp = job.out.with_extension("pdf.tmp");
        std::fs::write(&tmp, &signed).map_err(|e| format!("запись: {e}"))?;
        std::fs::rename(&tmp, &job.out).map_err(|e| format!("переименование: {e}"))?;
        log::info!("подписано: {} ({} байт)", job.out.display(), signed.len());
        self.open(job.out.clone());
        self.emit(Event::Done(format!("Подписано: {}", job.out.display())));
        Ok(())
    }

    fn convert(&mut self, op: ConvertOp) -> Result<(), String> {
        match op {
            ConvertOp::PdfToImages { src, out_dir, format, dpi } => {
                let doc = Document::open(&src, None).map_err(|e| e.to_string())?;
                std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
                let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("page".into());
                let ext = match format {
                    ImageFormat::Png => "png",
                    ImageFormat::Jpeg => "jpg",
                };
                let n = doc.page_count();
                for i in 0..n {
                    self.progress(format!("Страница {}/{n}", i + 1), i as f32 / n.max(1) as f32);
                    let img = doc.render_page_scaled(i, dpi / 72.0).map_err(|e| e.to_string())?;
                    let path = out_dir.join(format!("{stem}_{:03}.{ext}", i + 1));
                    save_image(&img, &path, &format)?;
                }
                self.emit(Event::Done(format!("Готово: {n} стр. → {}", out_dir.display())));
            }
            ConvertOp::PdfToText { src, out } => {
                let doc = Document::open(&src, None).map_err(|e| e.to_string())?;
                let n = doc.page_count();
                let mut text = String::new();
                for i in 0..n {
                    self.progress(format!("Текст: страница {}/{n}", i + 1), i as f32 / n.max(1) as f32);
                    let pg = doc.load_page(i).map_err(|e| e.to_string())?;
                    if i > 0 {
                        text.push_str("\n\n");
                    }
                    text.push_str(&pg.text());
                }
                std::fs::write(&out, text.replace("\r\n", "\n")).map_err(|e| e.to_string())?;
                self.emit(Event::Done(format!("Готово: {}", out.display())));
            }
            ConvertOp::ImagesToPdf { srcs, out } => {
                let mut doc = Document::new().map_err(|e| e.to_string())?;
                for (i, p) in srcs.iter().enumerate() {
                    self.progress(format!("Картинка {}/{}", i + 1, srcs.len()), i as f32 / srcs.len().max(1) as f32);
                    add_image_file(&mut doc, p)?;
                }
                doc.save_to(&out).map_err(|e| e.to_string())?;
                self.emit(Event::Done(format!("Готово: {}", out.display())));
            }
            ConvertOp::TextToPdf { src, out } => {
                let bytes = std::fs::read(&src).map_err(|e| e.to_string())?;
                let text = decode_text(&bytes);
                let font = load_font()?;
                let mut doc = Document::new().map_err(|e| e.to_string())?;
                self.progress("Вёрстка текста…", 0.3);
                doc.add_text_pages(&text, &font, 11.0).map_err(|e| e.to_string())?;
                doc.save_to(&out).map_err(|e| e.to_string())?;
                self.emit(Event::Done(format!("Готово: {} стр. → {}", doc.page_count(), out.display())));
            }
            ConvertOp::Merge { srcs, out } => {
                let mut doc = Document::new().map_err(|e| e.to_string())?;
                let tmp = office::TempDir::new()?;
                for (i, p) in srcs.iter().enumerate() {
                    self.progress(format!("Файл {}/{}", i + 1, srcs.len()), i as f32 / srcs.len().max(1) as f32);
                    // Документы Office сначала превращаем в PDF их же программой.
                    let pdf = if let Some(app) = office::app_for(p) {
                        self.progress(format!("{}: {} → PDF…", i + 1, app.name()), i as f32 / srcs.len().max(1) as f32);
                        let out = tmp.0.join(format!("{i}.pdf"));
                        office::to_pdf(p, &out).map_err(|e| format!("{}: {e}", file_name(p)))?;
                        out
                    } else {
                        p.clone()
                    };
                    let s = Document::open(&pdf, None).map_err(|e| format!("{}: {e}", p.display()))?;
                    let at = doc.page_count();
                    doc.import_pages(&s, &[], at).map_err(|e| e.to_string())?;
                }
                doc.save_to(&out).map_err(|e| e.to_string())?;
                self.emit(Event::Done(format!("Готово: {} стр. → {}", doc.page_count(), out.display())));
            }
            ConvertOp::Split { src, out_dir } => {
                let doc = Document::open(&src, None).map_err(|e| e.to_string())?;
                std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
                let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("page".into());
                let n = doc.page_count();
                for i in 0..n {
                    self.progress(format!("Страница {}/{n}", i + 1), i as f32 / n.max(1) as f32);
                    let one = doc.extract(&[i]).map_err(|e| e.to_string())?;
                    one.save_to(&out_dir.join(format!("{stem}_{:03}.pdf", i + 1))).map_err(|e| e.to_string())?;
                }
                self.emit(Event::Done(format!("Готово: {n} файлов → {}", out_dir.display())));
            }
            ConvertOp::OfficeToPdf { srcs, out } => {
                self.office_job(move |progress| {
                    if let [src] = srcs.as_slice() {
                        progress(format!("{} → PDF…", office_name(src)), 0.3);
                        office::to_pdf(src, &out)?;
                        return Ok(format!("Готово: {}", out.display()));
                    }
                    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
                    let n = srcs.len();
                    for (i, src) in srcs.iter().enumerate() {
                        progress(format!("{}/{n}: {} → PDF…", i + 1, office_name(src)), i as f32 / n as f32);
                        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("документ".into());
                        office::to_pdf(src, &out.join(format!("{stem}.pdf"))).map_err(|e| format!("{}: {e}", file_name(src)))?;
                    }
                    Ok(format!("Готово: {n} PDF → {}", out.display()))
                });
            }
            ConvertOp::OpenOffice { src } => {
                let tx = self.event_tx.clone();
                let ctx = self.ctx.clone();
                thread::spawn(move || {
                    let dir = std::env::temp_dir().join("PDFsmith").join("opened");
                    let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("документ".into());
                    let pdf = dir.join(format!("{}-{stem}.pdf", std::process::id()));
                    let res = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|_| office::to_pdf(&src, &pdf));
                    let _ = tx.send(match res {
                        Ok(()) => Event::OfficeOpened { pdf, source: src },
                        Err(e) => Event::Error(e),
                    });
                    ctx.request_repaint();
                });
            }
            ConvertOp::PdfToWord { src, out } => {
                self.office_job(move |progress| {
                    progress("Word распознаёт PDF…".into(), 0.3);
                    office::pdf_to_word(&src, &out)?;
                    Ok(format!("Готово: {}", out.display()))
                });
            }
            ConvertOp::PdfToExcel { src, out } => {
                // Строки и колонки — по геометрии текста; Office для этого не нужен.
                let doc = Document::open(&src, None).map_err(|e| e.to_string())?;
                let n = doc.page_count();
                let mut rows = Vec::new();
                for i in 0..n {
                    self.progress(format!("Таблицы: страница {}/{n}", i + 1), i as f32 / n.max(1) as f32);
                    let pg = doc.load_page(i).map_err(|e| e.to_string())?;
                    if i > 0 {
                        rows.push(Vec::new());
                    }
                    rows.extend(tables::page_rows(&pg.chars()));
                }
                if rows.iter().all(|r| r.is_empty()) {
                    return Err("В PDF нет текстового слоя (похоже на скан) — выгружать в Excel нечего".into());
                }
                tables::write_xlsx(&out, &rows).map_err(|e| format!("{}: {e}", out.display()))?;
                self.emit(Event::Done(format!("Готово: {}", out.display())));
            }
            ConvertOp::PdfToPowerPoint { src, out } => {
                // Картинки страниц рисует PDFium (здесь), слайды собирает PowerPoint (в фоне).
                let doc = Document::open(&src, None).map_err(|e| e.to_string())?;
                let tmp = office::TempDir::new()?;
                let n = doc.page_count();
                let slide = doc.page_size(0).map(|s| (s.width_pt, s.height_pt)).unwrap_or((842.0, 595.0));
                for i in 0..n {
                    self.progress(format!("Слайд {}/{n}", i + 1), 0.5 * i as f32 / n.max(1) as f32);
                    let img = doc.render_page_scaled(i, 160.0 / 72.0).map_err(|e| e.to_string())?;
                    save_image(&img, &tmp.0.join(format!("page{:05}.png", i + 1)), &ImageFormat::Png)?;
                }
                drop(doc);
                self.office_job(move |progress| {
                    progress("PowerPoint собирает слайды…".into(), 0.6);
                    office::images_to_pptx(&tmp.0, slide, &out)?;
                    Ok(format!("Готово: {n} слайдов → {}", out.display()))
                });
            }
        }
        Ok(())
    }

    /// Работа с Office — в отдельном потоке: Office думает секундами, а просмотр
    /// документа (тайлы, миниатюры) не должен замирать.
    fn office_job(&self, f: impl FnOnce(&dyn Fn(String, f32)) -> Result<String, String> + Send + 'static) {
        let tx = self.event_tx.clone();
        let ctx = self.ctx.clone();
        thread::Builder::new()
            .name("office".into())
            .spawn(move || {
                let progress = |msg: String, frac: f32| {
                    let _ = tx.send(Event::Progress { msg, frac });
                    ctx.request_repaint();
                };
                let ev = match f(&progress) {
                    Ok(msg) => Event::Done(msg),
                    Err(e) => Event::Error(e),
                };
                let _ = tx.send(ev);
                ctx.request_repaint();
            })
            .expect("поток office");
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn office_name(p: &std::path::Path) -> &'static str {
    office::app_for(p).map(|a| a.name()).unwrap_or("Office")
}

fn save_image(img: &RenderedImage, path: &std::path::Path, format: &ImageFormat) -> Result<(), String> {
    let buf = image::RgbaImage::from_raw(img.width, img.height, img.rgba.clone()).ok_or("неверный буфер")?;
    match format {
        ImageFormat::Png => buf.save(path).map_err(|e| e.to_string()),
        ImageFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgba8(buf).to_rgb8();
            let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 90);
            enc.encode_image(&rgb).map_err(|e| e.to_string())
        }
    }
}

/// Добавляет файл-картинку страницей; DPI подбирается по размеру (скриншоты —
/// 96, фото — 150/300), чтобы страница не выходила гигантской.
fn add_image_file(doc: &mut Document, path: &std::path::Path) -> Result<(), String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.to_rgba8();
    let (w, h) = img.dimensions();
    let dpi = match w.max(h) {
        0..=1400 => 96.0,
        1401..=3000 => 150.0,
        _ => 300.0,
    };
    doc.add_image_page(img.as_raw(), w, h, dpi).map_err(|e| e.to_string())
}

/// Шрифт для текстовых PDF: Arial/Segoe UI из системы (нужна кириллица).
fn load_font() -> Result<Vec<u8>, String> {
    let dir = std::env::var("WINDIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(r"C:\Windows"));
    for name in ["arial.ttf", "segoeui.ttf", "calibri.ttf", "tahoma.ttf"] {
        if let Ok(b) = std::fs::read(dir.join("Fonts").join(name)) {
            return Ok(b);
        }
    }
    Err("не найден системный TrueType-шрифт (arial.ttf)".into())
}

/// UTF-8 (с BOM или без), UTF-16LE с BOM, иначе — CP1251.
fn decode_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let u16s: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u16s);
    }
    let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(body) {
        return s.to_string();
    }
    body.iter().map(|&b| cp1251(b)).collect()
}

/// PDFium нельзя гонять из нескольких тестов сразу (последовательности
/// вызовов должны быть атомарны) — тесты с ним берут этот замок.
#[cfg(test)]
pub(crate) fn pdfium_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_unrotate_roundtrip() {
        let wh = (300.0, 500.0);
        for rot in 0..4u8 {
            let (rx, ry) = rotate_pt(20.0, 70.0, rot, wh);
            let (x, y) = unrotate(rx, ry, rot, wh);
            assert!((x - 20.0).abs() < 1e-4 && (y - 70.0).abs() < 1e-4, "rot={rot}");
        }
        // 90° по часовой: левый-верхний угол уходит в правый-верхний.
        assert_eq!(rotate_pt(0.0, 0.0, 1, wh), (500.0, 0.0));
        assert_eq!(rotate_pt(0.0, 0.0, 3, wh), (0.0, 300.0));
    }

    /// Регрессия «белый экран после рисования»: миниатюра, пришедшая посреди
    /// рендера тайлов, не должна отменять оставшиеся тайлы — UI их повторно не
    /// запросит (запрос не изменился), и страница останется белой.
    #[test]
    fn thumbnail_does_not_cancel_pending_tiles() {
        let _serial = pdfium_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let pdf = root.join("test_pdfs").join("text.pdf");
        if !root.join("pdfium.dll").exists() || !pdf.exists() {
            eprintln!("ПРОПУСК: нет pdfium.dll или test_pdfs/text.pdf");
            return;
        }
        let h = spawn(egui::Context::default(), Some(root));
        h.job_tx.send(Job::Open(pdf)).unwrap();
        let sizes = loop {
            match h.event_rx.recv_timeout(std::time::Duration::from_secs(15)).expect("нет Opened") {
                Event::Opened { page_sizes, .. } => break page_sizes,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        };
        // Крупный масштаб: много тайлов, рендер заметно дольше одного тайла.
        let scale = 4.0;
        let (w, hh) = sizes[0];
        let cols = ((w * scale) / TILE as f32).ceil() as u32;
        let rows = ((hh * scale) / TILE as f32).ceil() as u32;
        let tiles: Vec<(u32, u32)> = (0..rows).flat_map(|r| (0..cols).map(move |c| (c, r))).collect();
        let total = tiles.len();
        // Уникальный LOD — ключ дискового кэша: тайлы не придут готовыми из прошлого прогона.
        let lod = 1000 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos() % 100_000) as i32;
        h.job_tx.send(Job::Tiles { gen: 1, rotation: 0, lod, lod_scale: scale, pages: vec![PageTiles { page: 0, page_pt: (w, hh), tiles }] }).unwrap();
        // Даём начать рендер, затем подкидываем миниатюры, как боковая панель.
        std::thread::sleep(std::time::Duration::from_millis(30));
        for p in 1..4 {
            h.job_tx.send(Job::Thumbnail { gen: 1, page: p, rotation: 0 }).unwrap();
        }
        let mut got = std::collections::HashSet::new();
        let t0 = std::time::Instant::now();
        while got.len() < total && t0.elapsed().as_secs() < 30 {
            if let Ok(Event::Tile { col, row, .. }) = h.event_rx.recv_timeout(std::time::Duration::from_millis(500)) {
                got.insert((col, row));
            }
        }
        assert_eq!(got.len(), total, "тайлы потерялись после миниатюр: {}/{total}", got.len());
    }

    /// Правка текста через рендер-поток: замена, новый текст, затем Ctrl+Z
    /// по шагам возвращает исходную страницу; аннотации отменяются в общем порядке.
    #[test]
    fn text_edit_and_undo_history() {
        let _serial = pdfium_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let pdf = root.join("test_pdfs").join("text.pdf");
        if !root.join("pdfium.dll").exists() || !pdf.exists() {
            eprintln!("ПРОПУСК: нет pdfium.dll или test_pdfs/text.pdf");
            return;
        }
        let h = spawn(egui::Context::default(), Some(root));
        let wait = |pred: &dyn Fn(&Event) -> bool| loop {
            let ev = h.event_rx.recv_timeout(std::time::Duration::from_secs(15)).expect("нет события");
            if let Event::Error(e) = &ev {
                panic!("{e}");
            }
            if pred(&ev) {
                return ev;
            }
        };
        let boxes = |h: &RenderHandle| {
            h.job_tx.send(Job::TextObjects { gen: 7, page: 0 }).unwrap();
            match wait(&|e| matches!(e, Event::TextObjects { .. })) {
                Event::TextObjects { gen, boxes, .. } => {
                    assert_eq!(gen, 7);
                    boxes
                }
                _ => unreachable!(),
            }
        };
        h.job_tx.send(Job::Open(pdf)).unwrap();
        wait(&|e| matches!(e, Event::Opened { .. }));
        let before: Vec<String> = boxes(&h).into_iter().map(|b| b.text).collect();
        let first = boxes(&h).into_iter().find(|b| b.text.chars().count() >= 4).unwrap();
        assert!(first.rect.x1 > first.rect.x0 && first.size > 4.0);

        let edit = |op| h.job_tx.send(Job::Edit { op, rotation: 0 }).unwrap();
        edit(EditOp::ReplaceText { page: 0, index: first.index, expect: first.text.clone(), text: "Новая строка 42".into() });
        wait(&|e| matches!(e, Event::PageChanged { page: 0 }));
        edit(EditOp::Highlight { page: 0, rect: DispRect { x0: 50.0, y0: 50.0, x1: 200.0, y1: 70.0 }, color: Rgba(255, 220, 0, 255) });
        wait(&|e| matches!(e, Event::PageChanged { page: 0 }));
        edit(EditOp::AddText { page: 0, x: 60.0, y: 700.0, text: "Подпись внизу".into(), size: 14.0, color: Rgba(0, 0, 0, 255) });
        wait(&|e| matches!(e, Event::PageChanged { page: 0 }));
        let now: Vec<String> = boxes(&h).into_iter().map(|b| b.text).collect();
        assert!(now.iter().any(|t| t.trim() == "Новая строка 42"), "{now:?}");
        let added = boxes(&h).into_iter().find(|b| b.text == "Подпись внизу").expect("новый текст");
        assert!((added.rect.x0 - 60.0).abs() < 3.0 && added.rect.y0 > 695.0 && added.rect.y0 < 720.0, "{:?}", added.rect);

        // Ctrl+Z ×3: новый текст, маркер, замена.
        edit(EditOp::Undo { page: 0 });
        wait(&|e| matches!(e, Event::Changed { .. }));
        assert!(!boxes(&h).iter().any(|b| b.text == "Подпись внизу"));
        edit(EditOp::Undo { page: 0 });
        wait(&|e| matches!(e, Event::PageChanged { page: 0 }));
        edit(EditOp::Undo { page: 0 });
        wait(&|e| matches!(e, Event::Changed { .. }));
        let after: Vec<String> = boxes(&h).into_iter().map(|b| b.text).collect();
        assert_eq!(after, before);
        // История пуста, аннотаций нет — отменять нечего.
        edit(EditOp::Undo { page: 0 });
        match h.event_rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap() {
            Event::Error(e) => assert!(e.contains("нечего"), "{e}"),
            _ => panic!("ожидалась ошибка «нечего отменять»"),
        }
    }

    #[test]
    fn decode_text_handles_bom_and_cp1251() {
        assert_eq!(decode_text(&[0xEF, 0xBB, 0xBF, b'h', b'i']), "hi");
        assert_eq!(decode_text(&[0xFF, 0xFE, 0x2F, 0x04]), "Я");
        assert_eq!(decode_text(&[0xCF, 0xF0, 0xE8]), "При");
    }
}

fn cp1251(b: u8) -> char {
    match b {
        0x00..=0x7F => b as char,
        0xC0..=0xFF => char::from_u32(0x410 + (b as u32 - 0xC0)).unwrap(),
        0xA8 => 'Ё',
        0xB8 => 'ё',
        0xA0 => ' ',
        0xAB => '«',
        0xBB => '»',
        0x96 => '–',
        0x97 => '—',
        _ => '?',
    }
}
