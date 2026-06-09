//! Постоянный фоновый рендер-поток.
//!
//! PDFium живёт целиком в одном потоке (его нельзя звать из нескольких).
//! Поток открывает документ, лениво загружает страницу (дорого — один раз) и
//! рендерит запрошенные тайлы: сначала пробует дисковый кэш, иначе растрирует
//! и кладёт в кэш. Между тайлами проверяет, не пришёл ли более свежий запрос
//! (отмена устаревших задач при движении вьюпорта).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

use eframe::egui;
use pdfsmith_engine::disk_cache::{file_id, DiskCache, Tile};
use pdfsmith_pdfium::{init, Document, Page, RenderedImage};

/// Сторона тайла в device-пикселях.
pub const TILE: u32 = 256;

/// Длинная сторона миниатюры (обзор/миникарта) в пикселях.
pub const THUMB_PX: f32 = 256.0;

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

struct Worker {
    event_tx: Sender<Event>,
    ctx: egui::Context,
    doc: Option<Document>,
    disk: Option<DiskCache>,
    loaded: Option<(usize, Page)>,
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

    let mut w = Worker { event_tx, ctx, doc: None, disk: None, loaded: None };
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
