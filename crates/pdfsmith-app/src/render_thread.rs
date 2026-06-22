//! Постоянный фоновый рендер-поток.
//!
//! PDFium живёт целиком в одном потоке (его нельзя звать из нескольких).
//! Поток открывает документ, лениво загружает страницу (дорого — один раз) и
//! рендерит запрошенные тайлы: сначала пробует дисковый кэш, иначе растрирует
//! и кладёт в кэш. Между тайлами проверяет, не пришёл ли более свежий запрос
//! (отмена устаревших задач при движении вьюпорта).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui;
use pdfsmith_engine::disk_cache::{file_id, DiskCache, Tile};
use pdfsmith_engine::geom::{pdf_rect_to_page_pt, union, PdfRect};
use pdfsmith_engine::search::{search_in_chars, search_page_order};
use pdfsmith_pdfium::text::PageText;
use pdfsmith_pdfium::{init, Document, Page, RenderedImage};

/// Сторона тайла в device-пикселях.
pub const TILE: u32 = 256;

/// Длинная сторона миниатюры (обзор/миникарта) в пикселях.
pub const THUMB_PX: f32 = 256.0;

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

/// Команда из UI в рендер-поток.
pub enum Job {
    Open(PathBuf),
    /// Экспорт страницы в PNG по пути.
    Export { path: PathBuf, page: usize, rotation: u8 },
    /// Рендер обзорной миниатюры страницы.
    Thumbnail { page: usize, rotation: u8 },
    Tiles {
        page: usize,
        rotation: u8,
        lod: i32,
        lod_scale: f32,
        /// Размер (повёрнутой) страницы в пунктах — для нарезки тайлов.
        page_pt: (f32, f32),
        /// Координаты тайлов (col, row) в порядке приоритета (центр первым).
        tiles: Vec<(u32, u32)>,
    },
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
}

/// Событие из рендер-потока в UI.
pub enum Event {
    Opened { page_sizes: Vec<(f32, f32)> },
    PageLoading,
    Tile {
        lod: i32,
        col: u32,
        row: u32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Thumbnail {
        page: usize,
        rotation: u8,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
    Exported(PathBuf),
    Error(String),
    SearchPage { generation: u64, page: usize, matches: Vec<MatchPt> },
    SearchProgress { generation: u64, scanned: usize, total: usize },
    SearchDone { generation: u64, document_has_text: bool },
    PageText { page: usize, rotation: u8, chars: Vec<char>, boxes: Vec<egui::Rect> },
}

pub struct RenderHandle {
    pub job_tx: Sender<Job>,
    pub event_rx: Receiver<Event>,
}

/// Запускает рендер-поток. `dll_dir` — где искать `pdfium.dll`.
pub fn spawn(ctx: egui::Context, dll_dir: Option<PathBuf>) -> RenderHandle {
    let (job_tx, job_rx) = channel::<Job>();
    let (event_tx, event_rx) = channel::<Event>();
    thread::spawn(move || worker(job_rx, event_tx, ctx, dll_dir));
    RenderHandle { job_tx, event_rx }
}

/// Корень дискового кэша: `%LOCALAPPDATA%\pdfsmith\cache` либо temp.
fn cache_root() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        PathBuf::from(local).join("pdfsmith").join("cache")
    } else {
        std::env::temp_dir().join("pdfsmith-cache")
    }
}

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

struct Worker {
    event_tx: Sender<Event>,
    ctx: egui::Context,
    doc: Option<Document>,
    disk: Option<DiskCache>,
    loaded: Option<(usize, Page)>,
    text_cache: HashMap<usize, PageText>,
}

fn worker(
    job_rx: Receiver<Job>,
    event_tx: Sender<Event>,
    ctx: egui::Context,
    dll_dir: Option<PathBuf>,
) {
    log::info!("рендер-поток: init pdfium, dll_dir={dll_dir:?}");
    if let Err(e) = init(dll_dir.as_deref()) {
        log::error!("init pdfium провалился: {e}");
        let _ = event_tx.send(Event::Error(e.to_string()));
        ctx.request_repaint();
        return;
    }

    let mut w = Worker { event_tx, ctx, doc: None, disk: None, loaded: None, text_cache: HashMap::new() };
    let mut pending: Option<Job> = None;

    loop {
        let job = match pending.take() {
            Some(j) => j,
            None => match job_rx.recv() {
                Ok(j) => j,
                Err(_) => return, // UI закрылся
            },
        };

        match job {
            Job::Open(path) => w.open(path),
            Job::Export { path, page, rotation } => w.export(path, page, rotation),
            Job::Thumbnail { page, rotation } => w.thumbnail(page, rotation),
            Job::Tiles { page, rotation, lod, lod_scale, page_pt, tiles } => {
                pending =
                    w.render_tiles(&job_rx, page, rotation, lod, lod_scale, page_pt, tiles);
            }
            Job::PageText { page, rotation } => w.page_text(page, rotation),
            Job::Search { query, opts, start_page, rotation, generation } => {
                pending = w.search(&job_rx, query, opts, start_page, rotation, generation);
            }
        }
    }
}

impl Worker {
    fn emit(&self, ev: Event) {
        let _ = self.event_tx.send(ev);
        self.ctx.request_repaint();
    }

    /// Гарантирует, что нужная страница загружена (дорого — один раз).
    fn ensure_loaded(&mut self, page: usize) -> Result<(), String> {
        if self.loaded.as_ref().map(|(p, _)| *p) == Some(page) {
            return Ok(());
        }
        self.emit(Event::PageLoading);
        match self.doc.as_ref().map(|d| d.load_page(page)) {
            Some(Ok(p)) => {
                self.loaded = Some((page, p));
                Ok(())
            }
            Some(Err(e)) => Err(e.to_string()),
            None => Err("документ не открыт".into()),
        }
    }

    /// Рендерит страницу целиком в указанном масштабе с поворотом.
    fn render_full(&mut self, page: usize, rotation: u8, scale: f32) -> Result<RenderedImage, String> {
        let size = self
            .doc
            .as_ref()
            .and_then(|d| d.page_size(page))
            .ok_or_else(|| "нет такой страницы".to_string())?;
        self.ensure_loaded(page)?;
        let pg = &self.loaded.as_ref().expect("страница загружена").1;
        let (ew, eh) = if rotation & 1 == 1 {
            (size.height_pt, size.width_pt)
        } else {
            (size.width_pt, size.height_pt)
        };
        let w = ((ew * scale).round() as i32).max(1);
        let h = ((eh * scale).round() as i32).max(1);
        pg.render_region(scale, 0, 0, w, h, rotation, true)
            .map_err(|e| e.to_string())
    }

    fn export(&mut self, path: PathBuf, page: usize, rotation: u8) {
        // ~150 DPI, но не длиннее 8000 px по большей стороне.
        let size = match self.doc.as_ref().and_then(|d| d.page_size(page)) {
            Some(s) => s,
            None => {
                self.emit(Event::Error("нет такой страницы".into()));
                return;
            }
        };
        let longest = size.width_pt.max(size.height_pt).max(1.0);
        let scale = (150.0 / 72.0_f32).min(8000.0 / longest);

        match self.render_full(page, rotation, scale) {
            Ok(img) => match image::RgbaImage::from_raw(img.width, img.height, img.rgba) {
                Some(buf) => match buf.save(&path) {
                    Ok(()) => {
                        log::info!("экспортировано в {}", path.display());
                        self.emit(Event::Exported(path));
                    }
                    Err(e) => self.emit(Event::Error(format!("сохранение PNG: {e}"))),
                },
                None => self.emit(Event::Error("неверный буфер изображения".into())),
            },
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    fn thumbnail(&mut self, page: usize, rotation: u8) {
        let size = match self.doc.as_ref().and_then(|d| d.page_size(page)) {
            Some(s) => s,
            None => return,
        };
        let longest = size.width_pt.max(size.height_pt).max(1.0);
        let scale = THUMB_PX / longest;
        match self.render_full(page, rotation, scale) {
            Ok(img) => self.emit(Event::Thumbnail {
                page,
                rotation,
                width: img.width,
                height: img.height,
                rgba: img.rgba,
            }),
            Err(e) => self.emit(Event::Error(e)),
        }
    }

    fn open(&mut self, path: PathBuf) {
        log::info!("открываю: {}", path.display());
        match Document::open(&path, None) {
            Ok(d) => {
                let page_sizes = (0..d.page_count())
                    .map(|i| {
                        let s = d.page_size(i).unwrap_or(pdfsmith_pdfium::PageSize {
                            width_pt: 0.0,
                            height_pt: 0.0,
                        });
                        (s.width_pt, s.height_pt)
                    })
                    .collect();
                let disk = DiskCache::new(&cache_root(), &file_id(&path));
                log::info!(
                    "документ открыт: {:?}, страниц={}, диск-кэш enabled={}",
                    path.file_name().unwrap_or_default(),
                    d.page_count(),
                    disk.is_enabled()
                );
                // Сначала закрываем страницу старого документа, и только потом
                // заменяем документ (иначе закрытие страницы произойдёт после
                // закрытия её документа — use-after-free).
                self.loaded = None;
                self.text_cache.clear();
                self.disk = Some(disk);
                self.doc = Some(d);
                self.emit(Event::Opened { page_sizes });
            }
            Err(e) => {
                log::error!("открытие провалилось: {e}");
                self.emit(Event::Error(e.to_string()));
            }
        }
    }

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
        let mut document_has_text = false;
        for (scanned, page) in order.into_iter().enumerate() {
            if let Ok(newer) = job_rx.try_recv() {
                // Завершаем текущее поколение поиска, чтобы UI снял индикатор,
                // даже если запрос отменён более свежим.
                self.emit(Event::SearchDone { generation, document_has_text });
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
            if !pt.chars.is_empty() { document_has_text = true; }
            let matches = build_matches(pt, &query, opts, size.width_pt, size.height_pt, rotation);
            if !matches.is_empty() {
                self.emit(Event::SearchPage { generation, page, matches });
            }
            self.emit(Event::SearchProgress { generation, scanned: scanned + 1, total });
        }
        self.emit(Event::SearchDone { generation, document_has_text });
        None
    }

    /// Рендерит список тайлов. Возвращает `Some(job)`, если пришёл более свежий
    /// запрос и нужно переключиться на него (отмена остатка).
    #[allow(clippy::too_many_arguments)]
    fn render_tiles(
        &mut self,
        job_rx: &Receiver<Job>,
        page: usize,
        rotation: u8,
        lod: i32,
        lod_scale: f32,
        page_pt: (f32, f32),
        tiles: Vec<(u32, u32)>,
    ) -> Option<Job> {
        log::info!("запрос {} тайлов: page={page} rot={rotation} lod={lod} scale={lod_scale}", tiles.len());
        let page_w_px = (page_pt.0 * lod_scale).round() as i32;
        let page_h_px = (page_pt.1 * lod_scale).round() as i32;

        for (col, row) in tiles {
            // Отмена: появился более свежий запрос — отдаём его наверх.
            if let Ok(newer) = job_rx.try_recv() {
                return Some(newer);
            }

            // 1. Дисковый кэш — не требует загрузки страницы.
            if let Some(t) = self.disk.as_ref().and_then(|d| d.get(page, rotation, lod, col, row)) {
                self.emit(Event::Tile {
                    lod,
                    col,
                    row,
                    width: t.width,
                    height: t.height,
                    rgba: t.rgba,
                });
                continue;
            }

            // 2. Нужен растр — гарантируем загруженную страницу (дорого, один раз).
            if let Err(e) = self.ensure_loaded(page) {
                self.emit(Event::Error(e));
                return None;
            }
            let pg = match &self.loaded {
                Some((_, pg)) => pg,
                None => return None,
            };

            let tx = (col * TILE) as i32;
            let ty = (row * TILE) as i32;
            let tw = (TILE as i32).min(page_w_px - tx);
            let th = (TILE as i32).min(page_h_px - ty);
            if tw <= 0 || th <= 0 {
                continue;
            }

            match pg.render_region(lod_scale, tx, ty, tw, th, rotation, true) {
                Ok(img) => {
                    if let Some(d) = self.disk.as_ref() {
                        d.put(
                            page,
                            rotation,
                            lod,
                            col,
                            row,
                            &Tile { width: img.width, height: img.height, rgba: img.rgba.clone() },
                        );
                    }
                    self.emit(Event::Tile {
                        lod,
                        col,
                        row,
                        width: img.width,
                        height: img.height,
                        rgba: img.rgba,
                    });
                }
                Err(e) => self.emit(Event::Error(e.to_string())),
            }
        }
        None
    }
}

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
