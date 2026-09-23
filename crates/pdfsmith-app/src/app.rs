//! Состояние приложения и весь UI: непрерывная прокрутка страниц, тайловый
//! просмотр, боковая панель (миниатюры/поиск), инструменты аннотаций,
//! операции со страницами, конвертер.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use egui_phosphor::regular as ph;
use pdfsmith_engine::lod::lod_scale_for;
use pdfsmith_engine::viewport::visible_tiles_center_out;
use pdfsmith_pdfium::Rgba;

use crate::default_app::DefaultApp;
use crate::render_thread::{
    rotate_pt, spawn, ConvertOp, DispRect, EditOp, Event, ImageFormat, Job, PageTiles, RenderHandle,
    SearchHit, TILE,
};
use crate::settings::SettingsStore;
use crate::theme::{self, icon_button, vsep};
use crate::updates::{UpdState, UpdateUi};

const MIN_LOD: i32 = -4;
const MAX_LOD: i32 = 8;
const TEXTURE_CAP: usize = 600;
const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 64.0;
/// Зазор между страницами в пунктах.
const GAP_PT: f32 = 14.0;
/// Поле вокруг документа на холсте (экранные px).
const MARGIN_PX: f32 = 18.0;
const SIDEBAR_W: f32 = 236.0;
const THUMB_INFLIGHT: usize = 2;

/// Ключ тайла-текстуры: (страница, поворот, LOD, столбец, строка).
type TexKey = (usize, u8, i32, u32, u32);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Hand,
    Select,
    Pen,
    Highlight,
    Rect,
    Note,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideTab {
    Pages,
    Search,
}

#[derive(Clone, Copy)]
enum Action {
    FitPage,
    FitWidth,
    ActualSize,
    Zoom(f32),
    GotoPage(usize),
    ScrollBy(Vec2),
    /// Центрировать точку (страница, x, y в display pt).
    Reveal(usize, f32, f32),
}

/// Текущее перетаскивание инструментом.
enum Drag {
    Pan,
    /// Прямоугольник: страница, начало (display pt), текущая точка.
    Rect { page: usize, start: (f32, f32), cur: (f32, f32) },
    Ink { page: usize, points: Vec<(f32, f32)> },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConvertMode {
    PdfToImages,
    PdfToText,
    ImagesToPdf,
    TextToPdf,
    Merge,
    Split,
}

impl ConvertMode {
    const ALL: [ConvertMode; 6] = [
        ConvertMode::PdfToImages,
        ConvertMode::PdfToText,
        ConvertMode::ImagesToPdf,
        ConvertMode::TextToPdf,
        ConvertMode::Merge,
        ConvertMode::Split,
    ];
    fn label(self) -> &'static str {
        match self {
            ConvertMode::PdfToImages => "PDF → изображения (PNG/JPEG)",
            ConvertMode::PdfToText => "PDF → текст (.txt)",
            ConvertMode::ImagesToPdf => "Изображения → PDF",
            ConvertMode::TextToPdf => "Текст (.txt) → PDF",
            ConvertMode::Merge => "Объединить PDF",
            ConvertMode::Split => "Разделить PDF по страницам",
        }
    }
    fn multi_input(self) -> bool {
        matches!(self, ConvertMode::ImagesToPdf | ConvertMode::Merge)
    }
    fn input_filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            ConvertMode::ImagesToPdf => ("Изображения", &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"]),
            ConvertMode::TextToPdf => ("Текст", &["txt", "md", "log", "csv"]),
            _ => ("PDF", &["pdf"]),
        }
    }
    fn output_is_dir(self) -> bool {
        matches!(self, ConvertMode::PdfToImages | ConvertMode::Split)
    }
}

struct ConvertDialog {
    open: bool,
    mode: ConvertMode,
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    jpeg: bool,
    dpi: f32,
}

struct View {
    /// Экранная позиция начала координат документа.
    offset: Vec2,
    /// Масштаб: экранных px на пункт.
    zoom: f32,
    needs_fit: bool,
}

/// Раскладка страниц в документных пунктах (вертикальная колонка).
#[derive(Default)]
struct Layout {
    /// (left, top, w, h) каждой страницы с учётом поворота.
    rects: Vec<(f32, f32, f32, f32)>,
    width: f32,
    height: f32,
}

impl Layout {
    fn build(sizes: &[(f32, f32)], rotation: u8) -> Layout {
        let rotated: Vec<(f32, f32)> = sizes
            .iter()
            .map(|&(w, h)| if rotation & 1 == 1 { (h, w) } else { (w, h) })
            .collect();
        let width = rotated.iter().map(|s| s.0).fold(1.0f32, f32::max);
        let mut y = 0.0;
        let mut rects = Vec::with_capacity(rotated.len());
        for (i, &(w, h)) in rotated.iter().enumerate() {
            if i > 0 {
                y += GAP_PT;
            }
            rects.push(((width - w) * 0.5, y, w, h));
            y += h;
        }
        Layout { rects, width, height: y.max(1.0) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_stacks_pages_and_centers_narrow_ones() {
        let l = Layout::build(&[(100.0, 200.0), (50.0, 80.0)], 0);
        assert_eq!(l.width, 100.0);
        assert_eq!(l.rects[0], (0.0, 0.0, 100.0, 200.0));
        assert_eq!(l.rects[1], (25.0, 200.0 + GAP_PT, 50.0, 80.0));
        assert_eq!(l.height, 280.0 + GAP_PT);
    }

    #[test]
    fn layout_rotation_swaps_sides() {
        let l = Layout::build(&[(100.0, 200.0)], 1);
        assert_eq!(l.rects[0], (0.0, 0.0, 200.0, 100.0));
        let l = Layout::build(&[(100.0, 200.0)], 2);
        assert_eq!(l.rects[0], (0.0, 0.0, 100.0, 200.0));
    }

    // ---- Интеграция без окна: реальный PDFium, реальный документ. ----

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    fn headless() -> Option<(egui::Context, ViewerApp)> {
        let root = workspace_root();
        let pdf = root.join("test_pdfs").join("text.pdf");
        if !root.join("pdfium.dll").exists() || !pdf.exists() {
            eprintln!("ПРОПУСК: нет pdfium.dll или test_pdfs/text.pdf");
            return None;
        }
        let ctx = egui::Context::default();
        let app = ViewerApp::with_context(ctx.clone(), Some(root), Some(pdf));
        Some((ctx, app))
    }

    fn raw(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 840.0))),
            events,
            ..Default::default()
        }
    }

    fn pump(ctx: &egui::Context, app: &mut ViewerApp, events: Vec<egui::Event>) {
        let _ = ctx.run(raw(events), |ctx| app.frame(ctx));
    }

    /// Крутит кадры, пока условие не выполнится (или не выйдет таймаут).
    fn wait(ctx: &egui::Context, app: &mut ViewerApp, secs: f32, cond: impl Fn(&ViewerApp) -> bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed().as_secs_f32() < secs {
            pump(ctx, app, Vec::new());
            if cond(app) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        cond(app)
    }

    #[test]
    fn headless_open_scroll_zoom_search_edit_save_convert() {
        let Some((ctx, mut app)) = headless() else { return };
        assert!(wait(&ctx, &mut app, 15.0, |a| a.opened), "документ не открылся: {:?}", app.error);
        pump(&ctx, &mut app, Vec::new());
        let n = app.page_count();
        assert!(n >= 2);
        assert_eq!(app.layout.rects.len(), n);
        assert_eq!(app.current_page, 0);
        let y0 = app.view.offset.y;
        let z0 = app.view.zoom;
        assert!(z0 > 0.05 && z0 < 5.0, "fit-зум неадекватен: {z0}");

        // Колесо мыши над холстом: прокрутка вниз.
        let over = Pos2::new(760.0, 420.0);
        let wheel = |dy: f32, ctrl: bool| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, dy),
            modifiers: egui::Modifiers { ctrl, command: ctrl, ..Default::default() },
        };
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(over)]);
        for _ in 0..5 {
            pump(&ctx, &mut app, vec![egui::Event::PointerMoved(over), wheel(-120.0, false)]);
        }
        assert!(app.view.offset.y < y0 - 50.0, "прокрутка колесом не сработала: {} -> {}", y0, app.view.offset.y);
        // Пролистываем до конца — текущая страница должна смениться.
        for _ in 0..200 {
            pump(&ctx, &mut app, vec![egui::Event::PointerMoved(over), wheel(-400.0, false)]);
        }
        assert!(app.current_page > 0, "текущая страница не сменилась при прокрутке");
        // Ctrl+колесо: масштаб.
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(over), wheel(60.0, true)]);
        assert!(app.view.zoom > z0 * 1.01, "ctrl+колесо не увеличило масштаб");

        // Тайлы приходят и превращаются в текстуры.
        assert!(wait(&ctx, &mut app, 15.0, |a| !a.textures.is_empty()), "тайлы не пришли");

        // Поиск.
        app.search_text = "работа".into();
        app.start_search();
        assert!(wait(&ctx, &mut app, 30.0, |a| !a.searching), "поиск не завершился");
        assert!(!app.hits.is_empty(), "поиск ничего не нашёл; статус: {:?}", app.status.as_ref().map(|s| &s.0));
        assert_eq!(app.active_hit, Some(0));

        // Аннотация → документ помечен изменённым, кэши сброшены.
        let g = app.gen;
        app.edit(EditOp::Ink { page: 0, points: vec![(10.0, 10.0), (80.0, 40.0), (150.0, 20.0)], color: Rgba(200, 0, 0, 255), width: 3.0 });
        assert!(wait(&ctx, &mut app, 15.0, |a| a.dirty), "Changed не пришёл");
        assert!(app.gen > g);
        // Удаление последней страницы.
        app.edit(EditOp::DeletePage(n - 1));
        assert!(wait(&ctx, &mut app, 15.0, |a| a.page_count() == n - 1), "страница не удалилась");
        assert_eq!(app.layout.rects.len(), n - 1);

        // Сохранение.
        let out = std::env::temp_dir().join("pdfsmith_headless_save.pdf");
        let _ = std::fs::remove_file(&out);
        let _ = app.handle.job_tx.send(Job::Save(out.clone()));
        assert!(wait(&ctx, &mut app, 15.0, |a| !a.dirty), "Saved не пришёл");
        assert!(out.exists());
        assert_eq!(app.path.as_deref(), Some(out.as_path()));

        // Конвертация: PDF → PNG в папку.
        let dir = std::env::temp_dir().join("pdfsmith_headless_png");
        let _ = std::fs::remove_dir_all(&dir);
        app.progress = Some(("…".into(), 0.0));
        let _ = app.handle.job_tx.send(Job::Convert(ConvertOp::PdfToImages {
            src: out.clone(),
            out_dir: dir.clone(),
            format: ImageFormat::Png,
            dpi: 40.0,
        }));
        assert!(wait(&ctx, &mut app, 60.0, |a| a.progress.is_none()), "конвертация не завершилась");
        let pngs = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(pngs, n - 1, "ожидалось {} PNG", n - 1);
        assert!(app.status.as_ref().map(|s| !s.2).unwrap_or(false), "статус: {:?}", app.status.as_ref().map(|s| &s.0));
    }
}

pub struct ViewerApp {
    handle: RenderHandle,
    path: Option<PathBuf>,
    page_sizes: Vec<(f32, f32)>,
    layout: Layout,
    rotation: u8,
    view: View,
    current_page: usize,
    textures: HashMap<TexKey, egui::TextureHandle>,
    order: Vec<TexKey>,
    last_request: Option<(u8, i32, Vec<(usize, Vec<(u32, u32)>)>)>,
    /// Поколение документа: растёт при открытии/изменении, отсекает устаревшие тайлы.
    gen: u32,
    opened: bool,
    dirty: bool,
    loading_page: bool,
    error: Option<String>,
    pending: Vec<Action>,
    drag: Option<Drag>,
    tool: Tool,
    pen_color: Color32,
    pen_width: f32,
    // Боковая панель.
    show_sidebar: bool,
    side_tab: SideTab,
    thumbs: HashMap<(usize, u8), egui::TextureHandle>,
    thumb_requested: HashSet<(usize, u8)>,
    thumb_inflight: usize,
    sidebar_page: Option<usize>,
    // Поиск.
    search_text: String,
    search_query: String,
    hits: Vec<SearchHit>,
    active_hit: Option<usize>,
    searching: bool,
    // Диалоги.
    goto_text: String,
    note_dialog: Option<(usize, f32, f32, String)>,
    convert: ConvertDialog,
    close_dialog: bool,
    show_help: bool,
    // Статус.
    status: Option<(String, Instant, bool)>,
    progress: Option<(String, f32)>,
    cursor_pt: Option<(usize, f32, f32)>,
    // Настройки, обновления, «по умолчанию».
    store: SettingsStore,
    upd: UpdateUi,
    def_app: DefaultApp,
    show_settings: bool,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let dll_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let mut app = Self::with_context(cc.egui_ctx.clone(), dll_dir, path);
        // Боевые настройки, сеть и реестр — только в настоящем окне, не в тестах.
        app.store = SettingsStore::load_default();
        app.upd = UpdateUi::new(&cc.egui_ctx, &app.store);
        app.def_app = DefaultApp::detect();
        app
    }

    /// Создание без eframe (тесты): контекст egui и папка с `pdfium.dll`.
    pub fn with_context(ctx: egui::Context, dll_dir: Option<PathBuf>, path: Option<PathBuf>) -> Self {
        let handle = spawn(ctx, dll_dir);
        if let Some(p) = &path {
            let _ = handle.job_tx.send(Job::Open(p.clone()));
        }
        ViewerApp {
            handle,
            path: None,
            page_sizes: Vec::new(),
            layout: Layout::default(),
            rotation: 0,
            view: View { offset: Vec2::ZERO, zoom: 1.0, needs_fit: true },
            current_page: 0,
            textures: HashMap::new(),
            order: Vec::new(),
            last_request: None,
            gen: 0,
            opened: false,
            dirty: false,
            loading_page: false,
            error: None,
            pending: Vec::new(),
            drag: None,
            tool: Tool::Hand,
            pen_color: Color32::from_rgb(0xe0, 0x3c, 0x31),
            pen_width: 2.5,
            show_sidebar: true,
            side_tab: SideTab::Pages,
            thumbs: HashMap::new(),
            thumb_requested: HashSet::new(),
            thumb_inflight: 0,
            sidebar_page: None,
            search_text: String::new(),
            search_query: String::new(),
            hits: Vec::new(),
            active_hit: None,
            searching: false,
            goto_text: String::new(),
            note_dialog: None,
            convert: ConvertDialog {
                open: false,
                mode: ConvertMode::PdfToImages,
                inputs: Vec::new(),
                output: None,
                jpeg: false,
                dpi: 150.0,
            },
            close_dialog: false,
            show_help: false,
            status: None,
            progress: None,
            cursor_pt: None,
            store: SettingsStore::in_memory(),
            upd: UpdateUi::disabled(),
            def_app: DefaultApp::disabled(),
            show_settings: false,
        }
    }

    // ------------------------------------------------------------------
    // Состояние документа

    fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    fn page_pt(&self, page: usize) -> (f32, f32) {
        self.layout.rects.get(page).map(|r| (r.2, r.3)).unwrap_or((1.0, 1.0))
    }

    fn rebuild_layout(&mut self) {
        self.layout = Layout::build(&self.page_sizes, self.rotation);
        self.last_request = None;
    }

    fn reset_caches(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        self.textures.clear();
        self.order.clear();
        self.thumbs.clear();
        self.thumb_requested.clear();
        self.thumb_inflight = 0;
        self.last_request = None;
    }

    fn open_path(&mut self, path: PathBuf) {
        self.reset_caches();
        self.error = None;
        self.opened = false;
        self.dirty = false;
        self.hits.clear();
        self.active_hit = None;
        self.rotation = 0;
        self.view = View { offset: Vec2::ZERO, zoom: 1.0, needs_fit: true };
        let _ = self.handle.job_tx.send(Job::Open(path));
    }

    fn rotate_view(&mut self, delta: i32) {
        self.rotation = (((self.rotation as i32) + delta).rem_euclid(4)) as u8;
        let page = self.current_page;
        self.rebuild_layout();
        self.thumb_requested.clear();
        self.thumb_inflight = 0;
        self.pending.push(Action::GotoPage(page));
    }

    fn set_status(&mut self, msg: impl Into<String>, is_error: bool) {
        self.status = Some((msg.into(), Instant::now(), is_error));
    }

    fn edit(&mut self, op: EditOp) {
        let _ = self.handle.job_tx.send(Job::Edit { op, rotation: self.rotation });
    }

    fn save(&mut self, save_as: bool) {
        if !self.opened {
            return;
        }
        let target = if save_as || self.path.is_none() {
            let mut d = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
            if let Some(p) = &self.path {
                if let Some(name) = p.file_name() {
                    d = d.set_file_name(name.to_string_lossy());
                }
                if let Some(dir) = p.parent() {
                    d = d.set_directory(dir);
                }
            }
            d.save_file()
        } else {
            self.path.clone()
        };
        if let Some(p) = target {
            self.set_status("Сохранение…", false);
            let _ = self.handle.job_tx.send(Job::Save(p));
        }
    }

    fn title(&self) -> String {
        match &self.path {
            Some(p) => format!(
                "{}{} — pdfsmith",
                if self.dirty { "• " } else { "" },
                p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
            ),
            None => "pdfsmith".into(),
        }
    }

    // ------------------------------------------------------------------
    // События из рендер-потока

    fn poll_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.handle.event_rx.try_recv() {
            match ev {
                Event::Opened { path, page_sizes } => {
                    self.page_sizes = page_sizes;
                    self.path = Some(path);
                    self.opened = true;
                    self.dirty = false;
                    self.current_page = 0;
                    self.rebuild_layout();
                    self.view.needs_fit = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
                }
                Event::Changed { page_sizes } => {
                    let same_count = page_sizes.len() == self.page_sizes.len();
                    self.page_sizes = page_sizes;
                    self.dirty = true;
                    self.reset_caches();
                    self.rebuild_layout();
                    self.hits.clear();
                    self.active_hit = None;
                    if !same_count {
                        self.current_page = self.current_page.min(self.page_count().saturating_sub(1));
                        self.pending.push(Action::GotoPage(self.current_page));
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
                }
                Event::PageLoading(v) => self.loading_page = v,
                Event::Tile { gen, page, rotation, lod, col, row, width, height, rgba } => {
                    if rotation != self.rotation || gen != self.gen || page >= self.page_count() {
                        continue;
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                    let name = format!("p{page}_r{rotation}_{lod}_{col}_{row}");
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                    let key = (page, rotation, lod, col, row);
                    if self.textures.insert(key, tex).is_none() {
                        self.order.push(key);
                    }
                }
                Event::Thumbnail { gen, page, rotation, width, height, rgba } => {
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    if rotation != self.rotation || gen != self.gen {
                        continue;
                    }
                    let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                    let tex = ctx.load_texture(format!("thumb{page}_{rotation}"), image, egui::TextureOptions::LINEAR);
                    self.thumbs.insert((page, rotation), tex);
                }
                Event::Saved(path) => {
                    self.dirty = false;
                    self.path = Some(path.clone());
                    self.set_status(format!("Сохранено: {}", path.display()), false);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
                    if self.close_dialog {
                        self.close_dialog = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Event::SearchResults { query, hits } => {
                    self.searching = false;
                    self.progress = None;
                    self.search_query = query;
                    let n = hits.len();
                    self.hits = hits;
                    self.active_hit = if n > 0 { Some(0) } else { None };
                    if let Some(h) = self.hits.first() {
                        if let Some(r) = h.rects.first() {
                            let (x, y) = self.hit_center(h.page, r);
                            self.pending.push(Action::Reveal(h.page, x, y));
                        }
                    }
                    self.set_status(if n == 0 { "Ничего не найдено".to_string() } else { format!("Найдено: {n}") }, n == 0);
                }
                Event::TextCopied(text) => {
                    let n = text.chars().count();
                    if n == 0 {
                        self.set_status("В выделении нет текста", true);
                    } else {
                        ctx.copy_text(text);
                        self.set_status(format!("Скопировано символов: {n}"), false);
                    }
                }
                Event::Progress { msg, frac } => self.progress = Some((msg, frac)),
                Event::Done(msg) => {
                    self.progress = None;
                    self.set_status(msg, false);
                }
                Event::Error(e) => {
                    self.progress = None;
                    self.searching = false;
                    self.thumb_inflight = 0;
                    if self.opened {
                        self.set_status(e, true);
                    } else {
                        self.error = Some(e);
                    }
                }
            }
        }
    }

    /// Центр прямоугольника попадания в display-координатах текущего поворота.
    fn hit_center(&self, page: usize, r: &DispRect) -> (f32, f32) {
        let (w, h) = self.page_sizes.get(page).copied().unwrap_or((1.0, 1.0));
        let (x, y) = rotate_pt((r.x0 + r.x1) * 0.5, (r.y0 + r.y1) * 0.5, self.rotation, (w, h));
        (x, y)
    }

    fn hit_rect_rotated(&self, page: usize, r: &DispRect) -> (f32, f32, f32, f32) {
        let (w, h) = self.page_sizes.get(page).copied().unwrap_or((1.0, 1.0));
        let a = rotate_pt(r.x0, r.y0, self.rotation, (w, h));
        let b = rotate_pt(r.x1, r.y1, self.rotation, (w, h));
        (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
    }

    // ------------------------------------------------------------------
    // Геометрия просмотра

    fn page_screen_rect(&self, page: usize) -> Rect {
        let (l, t, w, h) = self.layout.rects[page];
        let min = self.view.offset + Vec2::new(l, t) * self.view.zoom;
        Rect::from_min_size(min.to_pos2(), Vec2::new(w, h) * self.view.zoom)
    }

    /// Экранная точка → (страница, x, y в display pt), если попала на страницу.
    fn page_at(&self, pos: Pos2) -> Option<(usize, f32, f32)> {
        for i in 0..self.layout.rects.len() {
            let r = self.page_screen_rect(i);
            if r.contains(pos) {
                let rel = (pos - r.min) / self.view.zoom;
                return Some((i, rel.x, rel.y));
            }
        }
        None
    }

    fn fit_page(&mut self, canvas: Rect) {
        let (pw, ph) = self.page_pt(self.current_page);
        let avail = canvas.shrink(MARGIN_PX);
        self.view.zoom = (avail.width() / pw).min(avail.height() / ph).clamp(MIN_ZOOM, MAX_ZOOM);
        self.goto_page(self.current_page, canvas, true);
    }

    fn fit_width(&mut self, canvas: Rect) {
        let (pw, _) = self.page_pt(self.current_page);
        let avail = canvas.shrink(MARGIN_PX);
        self.view.zoom = (avail.width() / pw).clamp(MIN_ZOOM, MAX_ZOOM);
        self.goto_page(self.current_page, canvas, false);
    }

    /// Прокручивает к странице: верх страницы у верха холста (или центр).
    fn goto_page(&mut self, page: usize, canvas: Rect, center: bool) {
        if self.layout.rects.is_empty() {
            return;
        }
        let page = page.min(self.layout.rects.len() - 1);
        let (l, t, w, h) = self.layout.rects[page];
        let z = self.view.zoom;
        self.view.offset.x = canvas.center().x - (l + w * 0.5) * z;
        self.view.offset.y = if center {
            canvas.center().y - (t + h * 0.5) * z
        } else {
            canvas.top() + MARGIN_PX - t * z
        };
        self.current_page = page;
        self.clamp(canvas);
    }

    fn reveal(&mut self, page: usize, x: f32, y: f32, canvas: Rect) {
        let Some(&(l, t, _, _)) = self.layout.rects.get(page) else { return };
        let z = self.view.zoom;
        self.view.offset = canvas.center().to_vec2() - Vec2::new(l + x, t + y) * z;
        self.clamp(canvas);
    }

    fn zoom_around(&mut self, factor: f32, pivot: Vec2) {
        let new_zoom = (self.view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.view.offset = pivot - (pivot - self.view.offset) * (new_zoom / self.view.zoom);
        self.view.zoom = new_zoom;
    }

    /// Не даёт документу улететь за пределы холста; маленький — центрируется.
    fn clamp(&mut self, canvas: Rect) {
        let doc = Vec2::new(self.layout.width, self.layout.height) * self.view.zoom;
        let m = MARGIN_PX;
        // X
        if doc.x + 2.0 * m <= canvas.width() {
            self.view.offset.x = canvas.center().x - doc.x * 0.5;
        } else {
            self.view.offset.x = self.view.offset.x.clamp(canvas.right() - m - doc.x, canvas.left() + m);
        }
        // Y
        if doc.y + 2.0 * m <= canvas.height() {
            self.view.offset.y = canvas.center().y - doc.y * 0.5;
        } else {
            self.view.offset.y = self.view.offset.y.clamp(canvas.bottom() - m - doc.y, canvas.top() + m);
        }
    }

    /// Текущая страница — с наибольшей видимой площадью (при равенстве — верхняя).
    fn update_current_page(&mut self, canvas: Rect) {
        let mut best = (0.0f32, self.current_page);
        for i in 0..self.layout.rects.len() {
            let r = self.page_screen_rect(i);
            if r.bottom() < canvas.top() {
                continue;
            }
            if r.top() > canvas.bottom() {
                break;
            }
            let vis = r.intersect(canvas);
            let area = if vis.is_positive() { vis.area() } else { 0.0 };
            if area > best.0 {
                best = (area, i);
            }
        }
        self.current_page = best.1;
    }

    fn apply_action(&mut self, action: Action, canvas: Rect) {
        match action {
            Action::FitPage => self.fit_page(canvas),
            Action::FitWidth => self.fit_width(canvas),
            Action::ActualSize => {
                self.view.zoom = 1.0;
                self.goto_page(self.current_page, canvas, true);
            }
            Action::Zoom(f) => {
                self.zoom_around(f, canvas.center().to_vec2());
                self.clamp(canvas);
            }
            Action::GotoPage(p) => self.goto_page(p, canvas, false),
            Action::ScrollBy(d) => {
                self.view.offset += d;
                self.clamp(canvas);
            }
            Action::Reveal(p, x, y) => self.reveal(p, x, y, canvas),
        }
    }

    // ------------------------------------------------------------------
    // Ввод

    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        // Хоткеи не должны срабатывать, пока фокус в текстовом поле.
        let typing = ctx.memory(|m| m.focused().is_some());
        let mods = ctx.input(|i| i.modifiers);
        if mods.command {
            let (o, s, f, z, p, w) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::O),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::F),
                    i.key_pressed(egui::Key::Z),
                    i.key_pressed(egui::Key::P),
                    i.key_pressed(egui::Key::W),
                )
            });
            if o {
                if let Some(p) = pick_pdf() {
                    self.open_path(p);
                }
            }
            if s {
                self.save(mods.shift);
            }
            if f {
                self.show_sidebar = true;
                self.side_tab = SideTab::Search;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("search_field")));
            }
            if z && self.opened && !typing {
                self.edit(EditOp::UndoAnnotation(self.current_page));
            }
            if p && self.opened {
                self.export_png();
            }
            if w {
                self.show_sidebar = !self.show_sidebar;
            }
            return;
        }
        if typing {
            return;
        }
        ctx.input(|i| {
            let k = |key| i.key_pressed(key);
            if k(egui::Key::Plus) || k(egui::Key::Equals) {
                self.pending.push(Action::Zoom(ZOOM_STEP));
            }
            if k(egui::Key::Minus) {
                self.pending.push(Action::Zoom(1.0 / ZOOM_STEP));
            }
            if k(egui::Key::Num0) {
                self.pending.push(Action::ActualSize);
            }
            if k(egui::Key::F) {
                self.pending.push(Action::FitPage);
            }
            if k(egui::Key::W) {
                self.pending.push(Action::FitWidth);
            }
            if k(egui::Key::OpenBracket) {
                self.rotate_view(-1);
            }
            if k(egui::Key::CloseBracket) {
                self.rotate_view(1);
            }
            if k(egui::Key::PageUp) || k(egui::Key::ArrowLeft) {
                if self.current_page > 0 {
                    self.pending.push(Action::GotoPage(self.current_page - 1));
                }
            }
            if k(egui::Key::PageDown) || k(egui::Key::ArrowRight) {
                self.pending.push(Action::GotoPage(self.current_page + 1));
            }
            if k(egui::Key::Home) {
                self.pending.push(Action::GotoPage(0));
            }
            if k(egui::Key::End) {
                self.pending.push(Action::GotoPage(usize::MAX));
            }
            if k(egui::Key::ArrowDown) {
                self.pending.push(Action::ScrollBy(Vec2::new(0.0, -60.0)));
            }
            if k(egui::Key::ArrowUp) {
                self.pending.push(Action::ScrollBy(Vec2::new(0.0, 60.0)));
            }
            if k(egui::Key::Space) {
                self.pending.push(Action::ScrollBy(Vec2::new(0.0, if i.modifiers.shift { 400.0 } else { -400.0 })));
            }
            if k(egui::Key::Escape) {
                self.tool = Tool::Hand;
                self.drag = None;
            }
            if k(egui::Key::H) {
                self.tool = Tool::Hand;
            }
            if k(egui::Key::T) {
                self.tool = Tool::Select;
            }
            if k(egui::Key::P) {
                self.tool = Tool::Pen;
            }
            if k(egui::Key::M) {
                self.tool = Tool::Highlight;
            }
            if k(egui::Key::R) {
                self.tool = Tool::Rect;
            }
            if k(egui::Key::N) {
                self.tool = Tool::Note;
            }
            if k(egui::Key::F3) && !self.hits.is_empty() {
                let n = self.hits.len();
                let cur = self.active_hit.unwrap_or(0);
                let next = if i.modifiers.shift { (cur + n - 1) % n } else { (cur + 1) % n };
                self.active_hit = Some(next);
                let h = &self.hits[next];
                if let Some(r) = h.rects.first() {
                    let (x, y) = self.hit_center(h.page, r);
                    self.pending.push(Action::Reveal(h.page, x, y));
                }
            }
            if k(egui::Key::F1) {
                self.show_help = !self.show_help;
            }
        });
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        let paths: Vec<PathBuf> = dropped.into_iter().filter_map(|f| f.path).collect();
        if paths.is_empty() {
            return;
        }
        let is_pdf = |p: &PathBuf| p.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false);
        let is_img = |p: &PathBuf| {
            p.extension()
                .map(|e| {
                    let e = e.to_string_lossy().to_ascii_lowercase();
                    matches!(e.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "gif" | "tif" | "tiff" | "webp")
                })
                .unwrap_or(false)
        };
        if let Some(p) = paths.iter().find(|p| is_pdf(p)) {
            if self.opened && paths.len() == 1 && ctx.input(|i| i.modifiers.shift) {
                let at = self.page_count();
                self.edit(EditOp::InsertPdf { path: p.clone(), at });
            } else {
                self.open_path(p.clone());
            }
        } else if paths.iter().all(is_img) {
            if self.opened {
                let at = self.page_count();
                self.edit(EditOp::InsertImages { paths, at });
            } else {
                self.convert.open = true;
                self.convert.mode = ConvertMode::ImagesToPdf;
                self.convert.inputs = paths;
                self.convert.output = None;
            }
        }
    }

    fn export_png(&mut self) {
        let stem = self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or("page".into());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .set_file_name(format!("{stem}_стр{}.png", self.current_page + 1))
            .save_file()
        {
            self.set_status("Экспорт…", false);
            let _ = self.handle.job_tx.send(Job::ExportPng { path, page: self.current_page, rotation: self.rotation });
        }
    }

    fn start_search(&mut self) {
        let q = self.search_text.trim().to_string();
        if q.is_empty() || !self.opened {
            return;
        }
        self.searching = true;
        self.hits.clear();
        self.active_hit = None;
        let _ = self.handle.job_tx.send(Job::Search(q));
    }

    /// Пан/зум/инструменты на холсте.
    fn handle_canvas_input(&mut self, ui: &egui::Ui, response: &egui::Response, canvas: Rect) {
        let (scroll, zoom_delta, middle) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.middle_down()));
        if response.hovered() {
            if zoom_delta != 1.0 {
                if let Some(cursor) = response.hover_pos() {
                    self.zoom_around(zoom_delta, cursor.to_vec2());
                }
            }
            if scroll != Vec2::ZERO {
                self.view.offset += scroll;
            }
        }

        let pointer = response.interact_pointer_pos();
        if response.drag_started() {
            let pan_tool = self.tool == Tool::Hand || middle;
            self.drag = match (pan_tool, pointer.and_then(|p| self.page_at(p))) {
                (true, _) => Some(Drag::Pan),
                (false, Some((page, x, y))) => match self.tool {
                    Tool::Pen => Some(Drag::Ink { page, points: vec![(x, y)] }),
                    Tool::Select | Tool::Highlight | Tool::Rect => Some(Drag::Rect { page, start: (x, y), cur: (x, y) }),
                    _ => None,
                },
                (false, None) => Some(Drag::Pan),
            };
        }
        if response.dragged() {
            let drag_page = match &self.drag {
                Some(Drag::Ink { page, .. }) | Some(Drag::Rect { page, .. }) => Some(*page),
                _ => None,
            };
            let rel = drag_page.zip(pointer).map(|(page, p)| {
                let r = self.page_screen_rect(page);
                let rel = (p - r.min) / self.view.zoom;
                let (w, h) = self.page_pt(page);
                ((rel.x, rel.y), (rel.x.clamp(0.0, w), rel.y.clamp(0.0, h)))
            });
            let zoom = self.view.zoom;
            match &mut self.drag {
                Some(Drag::Pan) => self.view.offset += response.drag_delta(),
                Some(Drag::Ink { points, .. }) => {
                    if let Some((raw, _)) = rel {
                        let last = points.last().copied().unwrap();
                        if (last.0 - raw.0).abs() + (last.1 - raw.1).abs() > 0.5 / zoom {
                            points.push(raw);
                        }
                    }
                }
                Some(Drag::Rect { cur, .. }) => {
                    if let Some((_, clamped)) = rel {
                        *cur = clamped;
                    }
                }
                None => {}
            }
        }
        if response.drag_stopped() {
            if let Some(d) = self.drag.take() {
                self.finish_drag(d);
            }
        }
        if response.clicked() && self.tool == Tool::Note {
            if let Some((page, x, y)) = pointer.and_then(|p| self.page_at(p)) {
                self.note_dialog = Some((page, x, y, String::new()));
            }
        }
        self.clamp(canvas);
    }

    fn finish_drag(&mut self, d: Drag) {
        let color = self.pen_color;
        let rgba = Rgba(color.r(), color.g(), color.b(), 255);
        match d {
            Drag::Pan => {}
            Drag::Ink { page, points } => {
                if points.len() >= 2 {
                    self.edit(EditOp::Ink { page, points, color: rgba, width: self.pen_width });
                }
            }
            Drag::Rect { page, start, cur } => {
                let rect = DispRect {
                    x0: start.0.min(cur.0),
                    y0: start.1.min(cur.1),
                    x1: start.0.max(cur.0),
                    y1: start.1.max(cur.1),
                };
                if (rect.x1 - rect.x0) < 1.0 || (rect.y1 - rect.y0) < 1.0 {
                    return;
                }
                match self.tool {
                    Tool::Select => {
                        let _ = self.handle.job_tx.send(Job::CopyText { page, rotation: self.rotation, rect });
                    }
                    Tool::Highlight => {
                        self.edit(EditOp::Highlight { page, rect, color: Rgba(255, 224, 64, 255) });
                    }
                    Tool::Rect => self.edit(EditOp::Rect { page, rect, color: rgba, width: self.pen_width }),
                    _ => {}
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Рендер холста

    fn request_visible(&mut self, lod: i32, lod_scale: f32, per_page: Vec<(usize, Vec<(u32, u32)>)>) {
        let key = (self.rotation, lod, per_page);
        if self.last_request.as_ref() == Some(&key) {
            return;
        }
        let mut pages = Vec::new();
        for (page, tiles) in &key.2 {
            let missing: Vec<(u32, u32)> = tiles
                .iter()
                .copied()
                .filter(|(c, r)| !self.textures.contains_key(&(*page, self.rotation, lod, *c, *r)))
                .collect();
            if !missing.is_empty() {
                pages.push(PageTiles { page: *page, page_pt: self.page_pt(*page), tiles: missing });
            }
        }
        self.last_request = Some(key);
        if !pages.is_empty() {
            let _ = self.handle.job_tx.send(Job::Tiles { gen: self.gen, rotation: self.rotation, lod, lod_scale, pages });
        }
    }

    fn tile_screen_rect(&self, page: usize, lod: i32, col: u32, row: u32, size: [usize; 2]) -> Rect {
        let lod_scale = (lod as f32).exp2();
        let origin_pt = Vec2::new((col * TILE) as f32 / lod_scale, (row * TILE) as f32 / lod_scale);
        let size_pt = Vec2::new(size[0] as f32 / lod_scale, size[1] as f32 / lod_scale);
        let base = self.page_screen_rect(page).min;
        Rect::from_min_size(base + origin_pt * self.view.zoom, size_pt * self.view.zoom)
    }

    fn draw_page_tiles(&self, painter: &egui::Painter, canvas: Rect, page: usize, current_lod: i32) {
        let mut keys: Vec<&TexKey> = self
            .textures
            .keys()
            .filter(|(p, rot, lod, _, _)| *p == page && *rot == self.rotation && *lod <= current_lod)
            .collect();
        keys.sort_by_key(|(_, _, lod, _, _)| *lod); // грубые сначала, резкие поверх
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        for &&(_, _, lod, col, row) in &keys {
            let tex = &self.textures[&(page, self.rotation, lod, col, row)];
            let rect = self.tile_screen_rect(page, lod, col, row, tex.size());
            if rect.intersects(canvas) {
                painter.image(tex.id(), rect, uv, Color32::WHITE);
            }
        }
    }

    fn evict(&mut self, protected: &HashSet<TexKey>) {
        let mut i = 0;
        while self.order.len() > TEXTURE_CAP && i < self.order.len() {
            let key = self.order[i];
            if protected.contains(&key) {
                i += 1;
                continue;
            }
            self.order.remove(i);
            self.textures.remove(&key);
        }
    }

    fn draw_overlays(&self, painter: &egui::Painter, canvas: Rect, visible: &[usize]) {
        // Результаты поиска.
        if !self.hits.is_empty() {
            for (idx, h) in self.hits.iter().enumerate() {
                if !visible.contains(&h.page) {
                    continue;
                }
                let base = self.page_screen_rect(h.page).min;
                let active = self.active_hit == Some(idx);
                for r in &h.rects {
                    let (x0, y0, x1, y1) = self.hit_rect_rotated(h.page, r);
                    let rect = Rect::from_min_max(
                        base + Vec2::new(x0, y0) * self.view.zoom,
                        base + Vec2::new(x1, y1) * self.view.zoom,
                    )
                    .expand(1.5);
                    if rect.intersects(canvas) {
                        let fill = if active { Color32::from_rgba_unmultiplied(255, 140, 0, 110) } else { Color32::from_rgba_unmultiplied(255, 220, 0, 80) };
                        painter.rect_filled(rect, 1.0, fill);
                        if active {
                            painter.rect_stroke(rect, 1.0, Stroke::new(1.5, Color32::from_rgb(255, 120, 0)));
                        }
                    }
                }
            }
        }
        // Текущее действие инструмента.
        match &self.drag {
            Some(Drag::Rect { page, start, cur }) => {
                let base = self.page_screen_rect(*page).min;
                let rect = Rect::from_two_pos(
                    base + Vec2::new(start.0, start.1) * self.view.zoom,
                    base + Vec2::new(cur.0, cur.1) * self.view.zoom,
                );
                match self.tool {
                    Tool::Select => {
                        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(80, 150, 255, 60));
                        painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_rgb(80, 150, 255)));
                    }
                    Tool::Highlight => {
                        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(255, 224, 64, 110));
                    }
                    _ => {
                        painter.rect_stroke(rect, 0.0, Stroke::new(self.pen_width * self.view.zoom, self.pen_color));
                    }
                }
            }
            Some(Drag::Ink { page, points }) => {
                let base = self.page_screen_rect(*page).min;
                let pts: Vec<Pos2> = points.iter().map(|&(x, y)| base + Vec2::new(x, y) * self.view.zoom).collect();
                painter.add(egui::Shape::line(pts, Stroke::new(self.pen_width * self.view.zoom, self.pen_color)));
            }
            _ => {}
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let canvas = response.rect;
        painter.rect_filled(canvas, 0.0, theme::CANVAS);

        if self.view.needs_fit {
            self.view.needs_fit = false;
            self.fit_page(canvas);
        }
        for a in std::mem::take(&mut self.pending) {
            self.apply_action(a, canvas);
        }
        self.handle_canvas_input(ui, &response, canvas);
        self.update_current_page(canvas);

        let cursor_icon = match (self.tool, &self.drag) {
            (_, Some(Drag::Pan)) => egui::CursorIcon::Grabbing,
            (Tool::Hand, _) => egui::CursorIcon::Grab,
            (Tool::Select, _) => egui::CursorIcon::Text,
            _ => egui::CursorIcon::Crosshair,
        };
        if response.hovered() {
            ui.ctx().set_cursor_icon(cursor_icon);
        }
        self.cursor_pt = response.hover_pos().and_then(|p| self.page_at(p));

        // Контекстное меню страницы.
        if let Some((page, _, _)) = response.hover_pos().and_then(|p| self.page_at(p)) {
            let mut ops = Vec::new();
            response.context_menu(|ui| self.page_menu(ui, page, &mut ops));
            for op in ops {
                self.edit(op);
            }
        }

        let zoom = self.view.zoom;
        // Растр выбираем по физическим пикселям (HiDPI), геометрия — в точках egui.
        let (lod, lod_scale) = lod_scale_for(zoom * ui.ctx().pixels_per_point(), MIN_LOD, MAX_LOD);
        // Запрашиваем чуть шире экрана — прокрутка не показывает пустоты.
        let prefetch = canvas.expand2(Vec2::new(0.0, canvas.height() * 0.35));
        let mut visible_pages = Vec::new();
        let mut per_page = Vec::new();
        let mut protected = HashSet::new();
        for i in 0..self.layout.rects.len() {
            let r = self.page_screen_rect(i);
            if !r.intersects(prefetch) {
                continue;
            }
            visible_pages.push(i);
            // Подложка и тень страницы.
            painter.rect_filled(r.translate(Vec2::new(0.0, 2.0)).expand(1.0), 0.0, Color32::from_black_alpha(70));
            painter.rect_filled(r, 0.0, Color32::WHITE);
            let rel = r.min - prefetch.min;
            let tiles: Vec<(u32, u32)> = visible_tiles_center_out(
                (rel.x, rel.y),
                zoom,
                (prefetch.width(), prefetch.height()),
                lod_scale,
                self.page_pt(i),
                TILE,
            )
            .iter()
            .map(|t| (t.col, t.row))
            .collect();
            for &(c, rr) in &tiles {
                protected.insert((i, self.rotation, lod, c, rr));
            }
            per_page.push((i, tiles));
            self.draw_page_tiles(&painter, canvas, i, lod);
        }
        // Текущую страницу — первой в очередь.
        per_page.sort_by_key(|(p, _)| (*p as i64 - self.current_page as i64).abs());
        self.request_visible(lod, lod_scale, per_page);
        self.evict(&protected);
        self.draw_overlays(&painter, canvas, &visible_pages);
    }

    fn page_menu(&self, ui: &mut egui::Ui, page: usize, ops: &mut Vec<EditOp>) {
        ui.set_min_width(220.0);
        ui.label(egui::RichText::new(format!("Страница {}", page + 1)).color(theme::MUTED).small());
        if ui.button(format!("{}  Повернуть по часовой", ph::ARROW_CLOCKWISE)).clicked() {
            ops.push(EditOp::RotatePage { page, quarters: 1 });
            ui.close_menu();
        }
        if ui.button(format!("{}  Повернуть против часовой", ph::ARROW_COUNTER_CLOCKWISE)).clicked() {
            ops.push(EditOp::RotatePage { page, quarters: -1 });
            ui.close_menu();
        }
        ui.separator();
        if ui.button(format!("{}  Пустая страница после", ph::FILE_PLUS)).clicked() {
            ops.push(EditOp::InsertBlank { at: page + 1 });
            ui.close_menu();
        }
        if ui.button(format!("{}  Вставить PDF после…", ph::FILES)).clicked() {
            if let Some(p) = pick_pdf() {
                ops.push(EditOp::InsertPdf { path: p, at: page + 1 });
            }
            ui.close_menu();
        }
        if ui.button(format!("{}  Вставить изображения после…", ph::IMAGE)).clicked() {
            let paths = rfd::FileDialog::new()
                .add_filter("Изображения", &["png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp"])
                .pick_files()
                .unwrap_or_default();
            if !paths.is_empty() {
                ops.push(EditOp::InsertImages { paths, at: page + 1 });
            }
            ui.close_menu();
        }
        ui.separator();
        if page > 0 && ui.button(format!("{}  Переместить выше", ph::ARROW_UP)).clicked() {
            ops.push(EditOp::MovePage { from: page, to: page - 1 });
            ui.close_menu();
        }
        if page + 1 < self.page_count() && ui.button(format!("{}  Переместить ниже", ph::ARROW_DOWN)).clicked() {
            ops.push(EditOp::MovePage { from: page, to: page + 1 });
            ui.close_menu();
        }
        if ui.button(format!("{}  Отменить последнюю аннотацию", ph::ARROW_U_UP_LEFT)).clicked() {
            ops.push(EditOp::UndoAnnotation(page));
            ui.close_menu();
        }
        ui.separator();
        if self.page_count() > 1 {
            if ui.button(egui::RichText::new(format!("{}  Удалить страницу", ph::TRASH)).color(theme::DANGER)).clicked() {
                ops.push(EditOp::DeletePage(page));
                ui.close_menu();
            }
        }
    }

    // ------------------------------------------------------------------
    // Панели

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(2.0);
            if icon_button(ui, ph::FOLDER_OPEN, "Открыть  (Ctrl+O)", false).clicked() {
                if let Some(p) = pick_pdf() {
                    self.open_path(p);
                }
            }
            let can = self.opened;
            ui.add_enabled_ui(can, |ui| {
                if icon_button(ui, ph::FLOPPY_DISK, "Сохранить  (Ctrl+S)", false).clicked() {
                    self.save(false);
                }
                if icon_button(ui, ph::EXPORT, "Сохранить как…  (Ctrl+Shift+S)", false).clicked() {
                    self.save(true);
                }
            });
            vsep(ui);
            if icon_button(ui, ph::SIDEBAR, "Боковая панель  (Ctrl+W)", self.show_sidebar).clicked() {
                self.show_sidebar = !self.show_sidebar;
            }
            vsep(ui);

            ui.add_enabled_ui(can, |ui| {
                // Зум
                if icon_button(ui, ph::MAGNIFYING_GLASS_MINUS, "Уменьшить  (−)", false).clicked() {
                    self.pending.push(Action::Zoom(1.0 / ZOOM_STEP));
                }
                let zoom_label = format!("{:.0}%", self.view.zoom * 100.0);
                egui::ComboBox::from_id_salt("zoom")
                    .selected_text(egui::RichText::new(zoom_label).monospace())
                    .width(78.0)
                    .show_ui(ui, |ui| {
                        for z in [0.25, 0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0, 8.0] {
                            if ui.selectable_label(false, format!("{:.0}%", z * 100.0)).clicked() {
                                self.pending.push(Action::Zoom(z / self.view.zoom));
                            }
                        }
                        ui.separator();
                        if ui.selectable_label(false, "Вписать страницу").clicked() {
                            self.pending.push(Action::FitPage);
                        }
                        if ui.selectable_label(false, "По ширине").clicked() {
                            self.pending.push(Action::FitWidth);
                        }
                    });
                if icon_button(ui, ph::MAGNIFYING_GLASS_PLUS, "Увеличить  (+)", false).clicked() {
                    self.pending.push(Action::Zoom(ZOOM_STEP));
                }
                if icon_button(ui, ph::FRAME_CORNERS, "Вписать страницу  (F)", false).clicked() {
                    self.pending.push(Action::FitPage);
                }
                if icon_button(ui, ph::ARROWS_OUT_LINE_HORIZONTAL, "По ширине  (W)", false).clicked() {
                    self.pending.push(Action::FitWidth);
                }
                vsep(ui);
                if icon_button(ui, ph::ARROW_COUNTER_CLOCKWISE, "Повернуть вид влево  ([)", false).clicked() {
                    self.rotate_view(-1);
                }
                if icon_button(ui, ph::ARROW_CLOCKWISE, "Повернуть вид вправо  (])", false).clicked() {
                    self.rotate_view(1);
                }
                vsep(ui);

                // Инструменты
                let tools = [
                    (Tool::Hand, ph::HAND, "Рука: перетаскивание  (H)"),
                    (Tool::Select, ph::CURSOR_TEXT, "Выделить текст → копировать  (T)"),
                    (Tool::Pen, ph::PEN, "Карандаш  (P)"),
                    (Tool::Highlight, ph::HIGHLIGHTER, "Маркер  (M)"),
                    (Tool::Rect, ph::RECTANGLE, "Рамка  (R)"),
                    (Tool::Note, ph::CHAT_TEXT, "Заметка  (N)"),
                ];
                for (t, icon, tip) in tools {
                    if icon_button(ui, icon, tip, self.tool == t).clicked() {
                        self.tool = t;
                    }
                }
                if matches!(self.tool, Tool::Pen | Tool::Rect) {
                    let mut c = [self.pen_color.r(), self.pen_color.g(), self.pen_color.b()];
                    if ui.color_edit_button_srgb(&mut c).on_hover_text("Цвет").changed() {
                        self.pen_color = Color32::from_rgb(c[0], c[1], c[2]);
                    }
                    ui.add(egui::Slider::new(&mut self.pen_width, 0.5..=12.0).show_value(false).fixed_decimals(1))
                        .on_hover_text(format!("Толщина: {:.1} pt", self.pen_width));
                }
                if icon_button(ui, ph::ARROW_U_UP_LEFT, "Отменить последнюю аннотацию  (Ctrl+Z)", false).clicked() {
                    self.edit(EditOp::UndoAnnotation(self.current_page));
                }
                vsep(ui);
            });

            // Правая часть.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(2.0);
                if icon_button(ui, ph::INFO, "Горячие клавиши  (F1)", self.show_help).clicked() {
                    self.show_help = !self.show_help;
                }
                if icon_button(ui, ph::GEAR, "Настройки", self.show_settings).clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui
                    .add(egui::Button::new(egui::RichText::new(format!("{}  Конвертер", ph::SWAP))).min_size(Vec2::new(0.0, 26.0)))
                    .on_hover_text("PDF ↔ изображения, текст; объединение и разделение")
                    .clicked()
                {
                    self.convert.open = !self.convert.open;
                    if self.convert.inputs.is_empty() {
                        if let Some(p) = &self.path {
                            self.convert.inputs = vec![p.clone()];
                        }
                    }
                }
                ui.add_enabled_ui(can, |ui| {
                    if icon_button(ui, ph::IMAGE, "Экспорт страницы в PNG  (Ctrl+P)", false).clicked() {
                        self.export_png();
                    }
                    vsep(ui);
                    // Навигация по страницам
                    let n = self.page_count().max(1);
                    if icon_button(ui, ph::CARET_RIGHT, "Следующая страница  (PageDown)", false).clicked() {
                        self.pending.push(Action::GotoPage(self.current_page + 1));
                    }
                    ui.label(egui::RichText::new(format!("/ {n}")).color(theme::MUTED));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.goto_text)
                            .desired_width(34.0)
                            .horizontal_align(egui::Align::Center)
                            .hint_text(format!("{}", self.current_page + 1)),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        if let Ok(k) = self.goto_text.trim().parse::<usize>() {
                            if k >= 1 {
                                self.pending.push(Action::GotoPage(k - 1));
                            }
                        }
                        self.goto_text.clear();
                    }
                    if icon_button(ui, ph::CARET_LEFT, "Предыдущая страница  (PageUp)", false).clicked() {
                        if self.current_page > 0 {
                            self.pending.push(Action::GotoPage(self.current_page - 1));
                        }
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let muted = |s: String| egui::RichText::new(s).color(theme::MUTED).small();
            match &self.path {
                Some(p) => {
                    let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(egui::RichText::new(name).small()).on_hover_text(p.display().to_string());
                    if self.dirty {
                        ui.label(egui::RichText::new("изменён").color(theme::ACCENT).small());
                    }
                }
                None => {
                    ui.label(muted("Файл не открыт".into()));
                }
            }
            if self.opened {
                ui.label(muted("·".into()));
                ui.label(muted(format!("стр. {} из {}", self.current_page + 1, self.page_count())));
                let (pw, ph_) = self.page_pt(self.current_page);
                ui.label(muted("·".into()));
                ui.label(muted(format!("{:.0}×{:.0} мм", pw / 72.0 * 25.4, ph_ / 72.0 * 25.4)));
                if let Some((page, x, y)) = self.cursor_pt {
                    ui.label(muted("·".into()));
                    ui.label(muted(format!("[{}] {:.1}, {:.1} мм", page + 1, x / 72.0 * 25.4, y / 72.0 * 25.4)));
                }
            }
            if self.loading_page {
                ui.label(muted("·".into()));
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(muted("Разбор страницы…".into()));
            }
            if let Some(text) = self.upd.status_text(&self.store) {
                ui.label(muted("·".into()));
                ui.label(muted(text));
            }
            if let Some((msg, frac)) = &self.progress {
                ui.label(muted("·".into()));
                ui.add(egui::ProgressBar::new(*frac).desired_width(140.0).desired_height(8.0));
                ui.label(muted(msg.clone()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                if let Some((msg, at, err)) = &self.status {
                    if at.elapsed().as_secs_f32() < 8.0 {
                        let color = if *err { theme::DANGER } else { theme::OK };
                        ui.label(egui::RichText::new(msg).color(color).small());
                        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
                    }
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            for (tab, label) in [(SideTab::Pages, "Страницы"), (SideTab::Search, "Поиск")] {
                let active = self.side_tab == tab;
                let text = egui::RichText::new(label).color(if active { theme::TEXT } else { theme::MUTED });
                let r = ui.add(egui::Button::new(text).frame(false));
                if r.clicked() {
                    self.side_tab = tab;
                }
                if active {
                    let rr = r.rect;
                    ui.painter().line_segment(
                        [Pos2::new(rr.left(), rr.bottom() + 3.0), Pos2::new(rr.right(), rr.bottom() + 3.0)],
                        Stroke::new(2.0, theme::ACCENT),
                    );
                }
            }
        });
        ui.add_space(6.0);
        ui.separator();
        match self.side_tab {
            SideTab::Pages => self.pages_panel(ui),
            SideTab::Search => self.search_panel(ui),
        }
    }

    fn pages_panel(&mut self, ui: &mut egui::Ui) {
        if !self.opened {
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Нет открытого документа").color(theme::MUTED));
            return;
        }
        let n = self.page_count();
        let thumb_w = SIDEBAR_W - 44.0;
        let mut goto: Option<usize> = None;
        let mut ops: Vec<EditOp> = Vec::new();
        let mut want_thumbs: Vec<usize> = Vec::new();
        // Автопрокрутка списка к текущей странице, когда она сменилась.
        let scroll_sidebar = self.sidebar_page != Some(self.current_page);
        self.sidebar_page = Some(self.current_page);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(6.0);
            for i in 0..n {
                let (pw, ph_) = self.page_pt(i);
                let th = (thumb_w * ph_ / pw.max(1.0)).clamp(30.0, 400.0);
                let card_h = th + 26.0;
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(SIDEBAR_W - 20.0, card_h), egui::Sense::click());
                let is_cur = i == self.current_page;
                let visible = ui.clip_rect().intersects(rect);
                if visible {
                    let p = ui.painter();
                    if is_cur {
                        p.rect_filled(rect, 4.0, Color32::from_rgba_unmultiplied(0xf0, 0xb4, 0x3c, 22));
                    } else if resp.hovered() {
                        p.rect_filled(rect, 4.0, Color32::from_white_alpha(8));
                    }
                    let img_rect = Rect::from_min_size(rect.min + Vec2::new(12.0, 6.0), Vec2::new(thumb_w, th));
                    p.rect_filled(img_rect.translate(Vec2::new(0.0, 1.5)), 0.0, Color32::from_black_alpha(80));
                    p.rect_filled(img_rect, 0.0, Color32::WHITE);
                    if let Some(tex) = self.thumbs.get(&(i, self.rotation)) {
                        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
                        p.image(tex.id(), img_rect, uv, Color32::WHITE);
                    } else {
                        want_thumbs.push(i);
                    }
                    if is_cur {
                        p.rect_stroke(img_rect.expand(1.5), 0.0, Stroke::new(2.0, theme::ACCENT));
                    }
                    p.text(
                        Pos2::new(rect.center().x, img_rect.bottom() + 10.0),
                        egui::Align2::CENTER_CENTER,
                        format!("{}", i + 1),
                        egui::FontId::proportional(11.5),
                        if is_cur { theme::TEXT } else { theme::MUTED },
                    );
                }
                if resp.clicked() {
                    goto = Some(i);
                }
                resp.context_menu(|ui| self.page_menu(ui, i, &mut ops));
                if is_cur && scroll_sidebar {
                    ui.scroll_to_rect(rect, Some(egui::Align::Center));
                }
            }
            ui.add_space(6.0);
        });
        if let Some(p) = goto {
            self.pending.push(Action::GotoPage(p));
        }
        for op in ops {
            self.edit(op);
        }
        for i in want_thumbs {
            if self.thumb_inflight >= THUMB_INFLIGHT {
                break;
            }
            let key = (i, self.rotation);
            if self.thumb_requested.insert(key) {
                self.thumb_inflight += 1;
                let _ = self.handle.job_tx.send(Job::Thumbnail { gen: self.gen, page: i, rotation: self.rotation });
            }
        }
    }

    fn search_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_text)
                    .id(egui::Id::new("search_field"))
                    .desired_width(SIDEBAR_W - 70.0)
                    .hint_text("Найти в документе…"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.start_search();
            }
            if icon_button(ui, ph::MAGNIFYING_GLASS, "Искать  (Enter)", false).clicked() {
                self.start_search();
            }
        });
        ui.add_space(4.0);
        if self.searching {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(egui::RichText::new("Поиск…").color(theme::MUTED).small());
            });
            return;
        }
        if !self.search_query.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("«{}»: {}", self.search_query, self.hits.len()))
                        .color(theme::MUTED)
                        .small(),
                );
                if !self.hits.is_empty() {
                    ui.label(egui::RichText::new("F3 — далее").color(theme::MUTED).small());
                }
            });
        }
        let mut reveal: Option<usize> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut last_page = usize::MAX;
            for (idx, h) in self.hits.iter().enumerate() {
                if h.page != last_page {
                    last_page = h.page;
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(format!("Страница {}", h.page + 1)).color(theme::ACCENT).small());
                    });
                }
                let active = self.active_hit == Some(idx);
                let text = egui::RichText::new(&h.snippet).size(12.0).color(if active { theme::TEXT } else { theme::MUTED });
                let r = ui.add_sized(
                    Vec2::new(SIDEBAR_W - 16.0, 0.0),
                    egui::Button::new(text).frame(false).wrap(),
                );
                if active {
                    ui.painter().rect_filled(r.rect, 3.0, Color32::from_rgba_unmultiplied(0xf0, 0xb4, 0x3c, 22));
                }
                if r.clicked() {
                    reveal = Some(idx);
                }
            }
        });
        if let Some(idx) = reveal {
            self.active_hit = Some(idx);
            let h = &self.hits[idx];
            if let Some(r) = h.rects.first() {
                let (x, y) = self.hit_center(h.page, r);
                self.pending.push(Action::Reveal(h.page, x, y));
            }
        }
    }

    fn convert_window(&mut self, ctx: &egui::Context) {
        if !self.convert.open {
            return;
        }
        let mut open = true;
        let mut run: Option<ConvertOp> = None;
        egui::Window::new("Конвертер")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let c = &mut self.convert;
                ui.horizontal(|ui| {
                    ui.label("Операция");
                    egui::ComboBox::from_id_salt("convert_mode")
                        .selected_text(c.mode.label())
                        .width(320.0)
                        .show_ui(ui, |ui| {
                            for m in ConvertMode::ALL {
                                if ui.selectable_value(&mut c.mode, m, m.label()).changed() {
                                    c.inputs.clear();
                                    c.output = None;
                                }
                            }
                        });
                });
                ui.add_space(8.0);

                // Входные файлы
                ui.horizontal(|ui| {
                    let (name, exts) = c.mode.input_filter();
                    let label = if c.mode.multi_input() { "Выбрать файлы…" } else { "Выбрать файл…" };
                    if ui.button(format!("{}  {label}", ph::FOLDER_OPEN)).clicked() {
                        let d = rfd::FileDialog::new().add_filter(name, exts);
                        if c.mode.multi_input() {
                            if let Some(p) = d.pick_files() {
                                c.inputs = p;
                            }
                        } else if let Some(p) = d.pick_file() {
                            c.inputs = vec![p];
                        }
                    }
                    if c.mode.multi_input() && !c.inputs.is_empty() && ui.button("Очистить").clicked() {
                        c.inputs.clear();
                    }
                });
                egui::Frame::none().fill(theme::BG).rounding(3.0).inner_margin(8.0).show(ui, |ui| {
                    ui.set_min_height(60.0);
                    ui.set_width(430.0);
                    if c.inputs.is_empty() {
                        ui.label(egui::RichText::new("Файлы не выбраны (можно перетащить в окно)").color(theme::MUTED).small());
                    } else {
                        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                            let mut remove = None;
                            for (i, p) in c.inputs.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if c.mode.multi_input() {
                                        if ui.small_button(ph::X).clicked() {
                                            remove = Some(i);
                                        }
                                        if i > 0 && ui.small_button(ph::ARROW_UP).clicked() {
                                            remove = Some(usize::MAX - i);
                                        }
                                    }
                                    ui.label(egui::RichText::new(p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()).small())
                                        .on_hover_text(p.display().to_string());
                                });
                            }
                            if let Some(r) = remove {
                                if r < c.inputs.len() {
                                    c.inputs.remove(r);
                                } else {
                                    let i = usize::MAX - r;
                                    c.inputs.swap(i, i - 1);
                                }
                            }
                        });
                    }
                });
                ui.add_space(8.0);

                // Параметры
                if c.mode == ConvertMode::PdfToImages {
                    ui.horizontal(|ui| {
                        ui.label("Формат");
                        ui.selectable_value(&mut c.jpeg, false, "PNG");
                        ui.selectable_value(&mut c.jpeg, true, "JPEG");
                        ui.add_space(12.0);
                        ui.label("DPI");
                        for d in [72.0, 150.0, 300.0, 600.0] {
                            ui.selectable_value(&mut c.dpi, d, format!("{d:.0}"));
                        }
                    });
                    ui.add_space(8.0);
                }

                // Выход
                ui.horizontal(|ui| {
                    let label = if c.mode.output_is_dir() { "Папка вывода…" } else { "Сохранить как…" };
                    if ui.button(format!("{}  {label}", ph::EXPORT)).clicked() {
                        let mut d = rfd::FileDialog::new();
                        if let Some(first) = c.inputs.first() {
                            if let Some(dir) = first.parent() {
                                d = d.set_directory(dir);
                            }
                            let stem = first.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("out".into());
                            d = match c.mode {
                                ConvertMode::PdfToText => d.add_filter("Текст", &["txt"]).set_file_name(format!("{stem}.txt")),
                                ConvertMode::Merge => d.add_filter("PDF", &["pdf"]).set_file_name("объединённый.pdf".to_string()),
                                _ => d.add_filter("PDF", &["pdf"]).set_file_name(format!("{stem}.pdf")),
                            };
                        }
                        c.output = if c.mode.output_is_dir() { d.pick_folder() } else { d.save_file() };
                    }
                    match &c.output {
                        Some(p) => {
                            ui.label(egui::RichText::new(p.display().to_string()).small().color(theme::MUTED));
                        }
                        None => {
                            ui.label(egui::RichText::new("не выбрано").small().color(theme::MUTED));
                        }
                    }
                });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let ready = !c.inputs.is_empty() && c.output.is_some() && self.progress.is_none();
                    let btn = egui::Button::new(egui::RichText::new(format!("{}  Запустить", ph::CHECK)).strong())
                        .fill(if ready { theme::ACCENT_DIM } else { theme::PANEL })
                        .stroke(Stroke::new(1.0, if ready { theme::ACCENT } else { theme::LINE }))
                        .min_size(Vec2::new(130.0, 28.0));
                    if ui.add_enabled(ready, btn).clicked() {
                        let out = c.output.clone().unwrap();
                        let src = c.inputs[0].clone();
                        run = Some(match c.mode {
                            ConvertMode::PdfToImages => ConvertOp::PdfToImages {
                                src,
                                out_dir: out,
                                format: if c.jpeg { ImageFormat::Jpeg } else { ImageFormat::Png },
                                dpi: c.dpi,
                            },
                            ConvertMode::PdfToText => ConvertOp::PdfToText { src, out },
                            ConvertMode::ImagesToPdf => ConvertOp::ImagesToPdf { srcs: c.inputs.clone(), out },
                            ConvertMode::TextToPdf => ConvertOp::TextToPdf { src, out },
                            ConvertMode::Merge => ConvertOp::Merge { srcs: c.inputs.clone(), out },
                            ConvertMode::Split => ConvertOp::Split { src, out_dir: out },
                        });
                    }
                    if let Some((msg, frac)) = &self.progress {
                        ui.add(egui::ProgressBar::new(*frac).desired_width(160.0).desired_height(10.0));
                        ui.label(egui::RichText::new(msg).small().color(theme::MUTED));
                    }
                });
            });
        self.convert.open = open;
        if let Some(op) = run {
            self.progress = Some(("Запуск…".into(), 0.0));
            let _ = self.handle.job_tx.send(Job::Convert(op));
        }
    }

    fn note_window(&mut self, ctx: &egui::Context) {
        let Some((page, x, y, mut text)) = self.note_dialog.take() else { return };
        let mut keep = true;
        let mut commit = false;
        egui::Window::new("Заметка")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("Страница {}", page + 1)).color(theme::MUTED).small());
                let r = ui.add(egui::TextEdit::multiline(&mut text).desired_width(340.0).desired_rows(4).hint_text("Текст заметки"));
                r.request_focus();
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Добавить").clicked() || (ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter))) {
                        commit = true;
                        keep = false;
                    }
                    if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                });
            });
        if commit && !text.trim().is_empty() {
            self.edit(EditOp::Note { page, x, y, text: text.trim().to_string(), color: Rgba(0xf0, 0xb4, 0x3c, 255) });
        } else if keep {
            self.note_dialog = Some((page, x, y, text));
        }
    }

    fn help_window(&mut self, ctx: &egui::Context) {
        if !self.show_help {
            return;
        }
        let mut open = true;
        egui::Window::new("Горячие клавиши")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-16.0, 52.0))
            .show(ctx, |ui| {
                egui::Grid::new("help").spacing([18.0, 4.0]).show(ui, |ui| {
                    let rows = [
                        ("Колесо", "прокрутка · Shift — горизонтально"),
                        ("Ctrl+колесо", "масштаб под курсором"),
                        ("+ / −  / 0", "масштаб / 100 %"),
                        ("F / W", "вписать страницу / по ширине"),
                        ("[ / ]", "повернуть вид"),
                        ("PgUp / PgDn, ← →", "страница назад / вперёд"),
                        ("Home / End", "первая / последняя"),
                        ("Пробел", "экран вниз (Shift — вверх)"),
                        ("H T P M R N", "рука · текст · карандаш · маркер · рамка · заметка"),
                        ("Ctrl+Z", "отменить аннотацию"),
                        ("Ctrl+F, F3", "поиск, следующее совпадение"),
                        ("Ctrl+O / S / Shift+S", "открыть / сохранить / сохранить как"),
                        ("Ctrl+P", "экспорт страницы в PNG"),
                        ("ПКМ по странице", "операции со страницей"),
                        ("Перетащить файл", "открыть · Shift+PDF — добавить · картинки — вставить"),
                    ];
                    for (k, v) in rows {
                        ui.label(egui::RichText::new(k).monospace().color(theme::ACCENT));
                        ui.label(egui::RichText::new(v).color(theme::MUTED));
                        ui.end_row();
                    }
                });
            });
        self.show_help = open;
    }

    fn close_window(&mut self, ctx: &egui::Context) {
        if !self.close_dialog {
            return;
        }
        let mut choice: Option<u8> = None;
        egui::Window::new("Несохранённые изменения")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Документ изменён. Сохранить перед закрытием?");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Сохранить").clicked() {
                        choice = Some(0);
                    }
                    if ui.button(egui::RichText::new("Не сохранять").color(theme::DANGER)).clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Отмена").clicked() {
                        choice = Some(2);
                    }
                });
            });
        match choice {
            Some(0) => self.save(false),
            Some(1) => {
                self.dirty = false;
                self.close_dialog = false;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Some(2) => {
                self.close_dialog = false;
                self.upd.cancel_restart();
            }
            _ => {}
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }

    fn on_exit(&mut self) {
        let others = pdfsmith_update::apply::other_instances_running();
        match self.upd.exit_action(others) {
            Some((path, relaunch)) => {
                log::info!("запуск установщика {} (перезапуск: {relaunch})", path.display());
                if let Err(e) = pdfsmith_update::apply::launch_installer(&path, relaunch) {
                    log::error!("не удалось запустить установщик: {e}");
                }
            }
            None if others && matches!(self.upd.state, UpdState::Ready { .. }) => {
                log::info!("обновление отложено: открыты другие окна PDFsmith");
            }
            None => {}
        }
    }
}

impl ViewerApp {
    /// Один кадр UI (вынесен из `eframe::App`, чтобы гонять без окна в тестах).
    pub fn frame(&mut self, ctx: &egui::Context) {
        self.poll_events(ctx);
        self.upd.poll(&mut self.store);
        self.def_app.poll();
        self.handle_dropped(ctx);
        self.handle_keyboard(ctx);

        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.close_dialog {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_dialog = true;
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::none().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(6.0, 5.0)))
            .show(ctx, |ui| self.toolbar(ui));

        // Одна плашка за раз: обновление важнее предложения «по умолчанию».
        if self.upd.banner_visible(&self.store) {
            egui::TopBottomPanel::top("banner")
                .frame(theme::banner_frame())
                .show(ctx, |ui| self.upd.banner(ui, &mut self.store));
        } else if self.def_app.banner_visible(self.store.data.ask_default_app) {
            egui::TopBottomPanel::top("banner")
                .frame(theme::banner_frame())
                .show(ctx, |ui| self.def_app.banner(ui, &mut self.store));
        }

        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::none().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(6.0, 3.0)))
            .show(ctx, |ui| self.status_bar(ui));
        if self.show_sidebar {
            egui::SidePanel::left("sidebar")
                .exact_width(SIDEBAR_W)
                .resizable(false)
                .frame(egui::Frame::none().fill(theme::PANEL))
                .show(ctx, |ui| self.sidebar(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::CANVAS))
            .show(ctx, |ui| {
                if let Some(e) = &self.error {
                    ui.centered_and_justified(|ui| {
                        ui.label(egui::RichText::new(format!("{}  {e}", ph::WARNING)).color(theme::DANGER));
                    });
                    return;
                }
                if !self.opened {
                    ui.centered_and_justified(|ui| {
                        if self.path.is_none() && self.page_sizes.is_empty() && !self.loading_page {
                            ui.label(
                                egui::RichText::new("Откройте PDF (Ctrl+O) или перетащите файл в окно")
                                    .color(theme::MUTED)
                                    .size(15.0),
                            );
                        } else {
                            ui.spinner();
                        }
                    });
                    return;
                }
                self.canvas(ui);
            });

        self.convert_window(ctx);
        self.note_window(ctx);
        self.help_window(ctx);
        self.close_window(ctx);
        crate::settings_ui::settings_window(ctx, &mut self.show_settings, &mut self.store, &mut self.upd, &mut self.def_app);
    }
}

fn pick_pdf() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file()
}
