//! Состояние приложения и весь UI: непрерывная прокрутка страниц, тайловый
//! просмотр, боковая панель (миниатюры/поиск), инструменты аннотаций,
//! операции со страницами, конвертер.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use egui_phosphor::regular as ph;
use pdfsmith_engine::lod::lod_scale_for;
use pdfsmith_engine::viewport::visible_tiles_center_out;
use pdfsmith_pdfium::Rgba;

use crate::convert_ui::{has_ext, ConvertDialog, ConvertMode, IMAGE_EXTS};
use crate::default_app::DefaultApp;
use crate::home::{self, HomeAction};
use crate::office;
use crate::print::PrintError;
use crate::print_ui::{DocInfo, PrintDialog, PrintRequest};
use crate::render_thread::{
    rotate_pt, spawn, ConvertOp, DispRect, EditOp, Event, Job, PageTiles, RenderHandle, SearchHit, SignJob, TextBox, TILE,
};
use crate::settings::SettingsStore;
use crate::sign_ui::{hex, SignChoice, SignDialog};
use crate::theme::{self, icon_button, rail_button, vsep};
use crate::updates::{UpdState, UpdateUi};

const MIN_LOD: i32 = -4;
const MAX_LOD: i32 = 8;
/// Тайл 512² ≈ 1 МБ видеопамяти.
const TEXTURE_CAP: usize = 160;
/// Сколько тайлов загружать в GPU за кадр (пачка из дискового кэша не даёт рывка).
const TILE_UPLOADS_PER_FRAME: usize = 24;
const ZOOM_STEP: f32 = 1.25;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 64.0;
/// Зазор между страницами в пунктах.
const GAP_PT: f32 = 14.0;
/// Поле вокруг документа на холсте (экранные px).
const MARGIN_PX: f32 = 18.0;
const SIDEBAR_W: f32 = 236.0;
/// Высота строки результата поиска в боковой панели.
const HIT_ROW_H: f32 = 44.0;

/// Цвета пера и рамки.
const PEN_COLORS: [(Color32, &str); 5] = [
    (Color32::from_rgb(0xe0, 0x3c, 0x31), "Красный"),
    (Color32::from_rgb(0x25, 0x63, 0xeb), "Синий"),
    (Color32::from_rgb(0x16, 0xa3, 0x4a), "Зелёный"),
    (Color32::from_rgb(0xf5, 0x9e, 0x0b), "Оранжевый"),
    (Color32::from_rgb(0x1d, 0x1e, 0x22), "Чёрный"),
];
/// Цвета маркера.
const MARKER_COLORS: [(Color32, &str); 4] = [
    (Color32::from_rgb(255, 224, 64), "Жёлтый"),
    (Color32::from_rgb(140, 230, 120), "Зелёный"),
    (Color32::from_rgb(255, 150, 200), "Розовый"),
    (Color32::from_rgb(120, 200, 255), "Голубой"),
];

/// Инструменты ленты: инструмент, иконка, подпись, подсказка.
const TOOLS: [(Tool, &str, &str, &str); 8] = [
    (Tool::Hand, ph::HAND, "Рука", "Рука: двигать страницы  (H)"),
    (Tool::Select, ph::CURSOR_TEXT, "Текст", "Выделить и скопировать текст  (T)"),
    (Tool::EditText, ph::TEXT_T, "Правка", "Править, добавлять и двигать текст в самом PDF  (E)"),
    (Tool::Highlight, ph::HIGHLIGHTER, "Маркер", "Выделить маркером  (M)"),
    (Tool::Pen, ph::PEN, "Перо", "Рисовать от руки  (P)"),
    (Tool::Rect, ph::RECTANGLE, "Рамка", "Обвести рамкой  (R)"),
    (Tool::Note, ph::CHAT_TEXT, "Заметка", "Заметка-стикер  (N)"),
    (Tool::Sign, ph::SEAL_CHECK, "Подпись", "Электронная подпись: КриптоПро, токены  (G)"),
];
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
    /// Подпись ЭП: обвести место штампа.
    Sign,
    /// Правка текста страницы: изменить, добавить, перенести.
    EditText,
}

/// Открытый встроенный редактор текста на странице.
struct TextEditing {
    page: usize,
    /// Объект страницы; `None` — новый текст.
    index: Option<usize>,
    original: String,
    text: String,
    /// Место редактора: display pt текущего поворота (x0, y0, x1, y1).
    at: (f32, f32, f32, f32),
    size: f32,
    color: Color32,
    id: egui::Id,
    focused: bool,
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
    /// Перенос текстового объекта: точка захвата и текущая (display pt).
    Move { page: usize, index: usize, start: (f32, f32), cur: (f32, f32) },
}

/// Фигура аннотации поверх страницы (display pt текущего поворота).
enum Mark {
    Ink(Vec<(f32, f32)>),
    Frame((f32, f32), (f32, f32)),
    Marker((f32, f32), (f32, f32)),
    /// Правка текста до перерисовки: закрасить `cover` и написать `text` в точке.
    Text { cover: Option<(f32, f32, f32, f32)>, at: (f32, f32), text: String, size: f32 },
}

/// Только что нарисованная аннотация: видна, пока страница не перерисуется
/// с ней (иначе штрих мигнёт — старые тайлы ещё без него).
struct Ghost {
    page: usize,
    mark: Mark,
    color: Color32,
    width: f32,
    /// Рендер-поток подтвердил правку (`PageChanged`).
    armed: bool,
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
    use crate::render_thread::ImageFormat;

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
        headless_with(&std::env::var("PDFSMITH_TEST_PDF").unwrap_or_else(|_| "text.pdf".into()))
    }

    fn headless_with(name: &str) -> Option<(egui::Context, ViewerApp)> {
        let root = workspace_root();
        let pdf = root.join("test_pdfs").join(name);
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

    /// Регрессия: контекстное меню страницы не должно закрываться и мигать,
    /// когда курсор уходит с холста на само меню.
    #[test]
    fn headless_context_menu_stays_open_under_cursor() {
        let _serial = crate::render_thread::pdfium_test_lock();
        let Some((ctx, mut app)) = headless() else { return };
        assert!(wait(&ctx, &mut app, 15.0, |a| a.opened), "документ не открылся: {:?}", app.error);
        assert!(wait(&ctx, &mut app, 15.0, |a| !a.textures.is_empty()), "тайлы не пришли");
        let r = app.page_screen_rect(app.current_page);
        let at = r.center();
        let press = |pressed: bool| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        };
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(at)]);
        pump(&ctx, &mut app, vec![press(true)]);
        pump(&ctx, &mut app, vec![press(false)]);
        pump(&ctx, &mut app, Vec::new());
        assert_eq!(app.menu_page, Some(app.current_page), "меню не открылось");
        // Курсор над меню (оно раскрывается вправо-вниз от точки щелчка).
        let inside = at + Vec2::new(60.0, 40.0);
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(inside)]);
        for frame in 0..6 {
            pump(&ctx, &mut app, Vec::new());
            let layer = ctx.layer_id_at(inside).map(|l| l.order);
            assert_eq!(layer, Some(egui::Order::Foreground), "кадр {frame}: меню пропало из-под курсора");
        }
        // Escape закрывает меню и сбрасывает страницу.
        pump(&ctx, &mut app, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        pump(&ctx, &mut app, Vec::new());
        pump(&ctx, &mut app, Vec::new());
        assert_eq!(app.menu_page, None);
    }

    /// Регрессия: после штриха карандашом страницы не должны оставаться белыми —
    /// тайлы обязаны перерисоваться с новым поколением.
    #[test]
    fn headless_pen_stroke_rerenders_tiles() {
        let _serial = crate::render_thread::pdfium_test_lock();
        let Some((ctx, mut app)) = headless() else { return };
        assert!(wait(&ctx, &mut app, 15.0, |a| a.opened), "документ не открылся: {:?}", app.error);
        assert!(wait(&ctx, &mut app, 15.0, |a| !a.textures.is_empty()), "тайлы не пришли");
        app.tool = Tool::Pen;
        let r = app.page_screen_rect(app.current_page);
        let a = r.min + (r.max - r.min) * 0.3;
        let press = |pos: Pos2, pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(a)]);
        pump(&ctx, &mut app, vec![press(a, true)]);
        for i in 1..=20 {
            let p = a + Vec2::new(i as f32 * 8.0, (i as f32 * 0.7).sin() * 20.0);
            pump(&ctx, &mut app, vec![egui::Event::PointerMoved(p)]);
        }
        let end = a + Vec2::new(160.0, 0.0);
        pump(&ctx, &mut app, vec![egui::Event::PointerMoved(end), press(end, false)]);
        pump(&ctx, &mut app, vec![egui::Event::PointerGone]);
        assert!(wait(&ctx, &mut app, 15.0, |a| a.dirty), "штрих не применился; статус: {:?}", app.status.as_ref().map(|s| &s.0));
        // Все тайлы последнего запроса должны прийти, даже если рендер
        // перемежается миниатюрами боковой панели.
        let all_visible = |a: &ViewerApp| {
            a.last_request.as_ref().is_some_and(|(rot, lod, pages)| {
                pages.iter().all(|(p, tiles)| {
                    tiles.iter().all(|&(c, r)| {
                        let k = (*p, *rot, *lod, c, r);
                        a.textures.contains_key(&k) && !a.stale.contains(&k)
                    })
                })
            })
        };
        let ok = wait(&ctx, &mut app, 15.0, all_visible);
        let missing: usize = app.last_request.as_ref().map(|(rot, lod, pages)| {
            pages.iter().map(|(p, t)| t.iter().filter(|&&(c, r)| !app.textures.contains_key(&(*p, *rot, *lod, c, r)) || app.stale.contains(&(*p, *rot, *lod, c, r))).count()).sum()
        }).unwrap_or(0);
        assert!(ok, "после штриха часть тайлов так и не пришла (белые места): не хватает {missing}");
        assert!(app.ghosts.is_empty(), "призрак штриха не убран после перерисовки");

        // Содержимое свежего рендера страницы: не пустое и со штрихом.
        let page = app.current_page;
        let (w, h) = app.page_pt(page);
        let tiles: Vec<(u32, u32)> = (0..((w / TILE as f32).ceil() as u32)).flat_map(|c| (0..((h / TILE as f32).ceil() as u32)).map(move |r| (c, r))).collect();
        let _ = app.handle.job_tx.send(Job::Tiles { gen: 9999, rotation: 0, lod: 0, lod_scale: 1.0, pages: vec![PageTiles { page, page_pt: (w, h), tiles: tiles.clone() }] });
        let (mut dark, mut red, mut got) = (0usize, 0usize, 0usize);
        let t0 = Instant::now();
        while got < tiles.len() && t0.elapsed().as_secs() < 15 {
            if let Ok(Event::Tile { gen: 9999, rgba, .. }) = app.handle.event_rx.recv_timeout(std::time::Duration::from_millis(200)) {
                got += 1;
                dark += rgba.chunks(4).filter(|p| p[0] < 128 && p[1] < 128 && p[2] < 128).count();
                red += rgba.chunks(4).filter(|p| p[0] > 150 && p[1] < 100 && p[2] < 100).count();
            }
        }
        eprintln!("tiles={got}/{} dark={dark} red={red}", tiles.len());
        assert!(dark > 1000 && red > 20, "страница после штриха пустая: dark={dark} red={red}");
    }

    #[test]
    fn headless_open_scroll_zoom_search_edit_save_convert() {
        let _serial = crate::render_thread::pdfium_test_lock();
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

        // Аннотация → документ помечен изменённым, общий кэш цел.
        let g = app.gen;
        app.edit(EditOp::Ink { page: 0, points: vec![(10.0, 10.0), (80.0, 40.0), (150.0, 20.0)], color: Rgba(200, 0, 0, 255), width: 3.0 });
        assert!(wait(&ctx, &mut app, 15.0, |a| a.dirty), "PageChanged не пришёл");
        assert_eq!(app.gen, g, "правка аннотации не должна сбрасывать все тайлы");
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
    /// Тайлы, устаревшие после правки аннотаций: рисуются, пока нет свежих.
    stale: HashSet<TexKey>,
    ghosts: Vec<Ghost>,
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
    marker_color: Color32,
    /// Правка текста: объекты видимых страниц, запрошенные страницы, редактор.
    text_boxes: HashMap<usize, Vec<TextBox>>,
    text_requested: HashSet<usize>,
    text_edit: Option<TextEditing>,
    /// Кегль и цвет нового текста.
    text_size: f32,
    text_color: Color32,
    /// Файл, который сейчас открывается (для экрана загрузки).
    opening: Option<PathBuf>,
    /// Документ сконвертирован из Office и ещё не сохранён: куда предложить сохранить.
    untitled: Option<PathBuf>,
    /// Область под центральной панелью: к ней привязаны уведомления и подсказки.
    central: Rect,
    /// Прокрутить список результатов поиска к этому совпадению.
    search_scroll_to: Option<usize>,
    // Боковая панель.
    show_sidebar: bool,
    side_tab: SideTab,
    thumbs: HashMap<(usize, u8), egui::TextureHandle>,
    thumb_requested: HashSet<(usize, u8)>,
    thumb_inflight: usize,
    sidebar_page: Option<usize>,
    /// Видимые тайлы ещё не дорисованы: миниатюры ждут, чтобы не занимать
    /// рендер-поток (миниатюра — полный непрерываемый рендер страницы).
    view_pending: bool,
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
    print: PrintDialog,
    sign: SignDialog,
    /// Флаг отмены идущей печати.
    print_cancel: Option<Arc<AtomicBool>>,
    // Статус.
    status: Option<(String, Instant, bool)>,
    progress: Option<(String, f32)>,
    cursor_pt: Option<(usize, f32, f32)>,
    /// Страница, для которой открыто контекстное меню холста.
    menu_page: Option<usize>,
    // Настройки, обновления, «по умолчанию».
    store: SettingsStore,
    upd: UpdateUi,
    def_app: DefaultApp,
    show_settings: bool,
    /// Отладочный снимок окна (см. `devshot`).
    shot: Option<crate::devshot::DevShot>,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let dll_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let mut app = Self::with_context(cc.egui_ctx.clone(), dll_dir, path);
        // Боевые настройки, сеть и реестр — только в настоящем окне, не в тестах.
        app.store = SettingsStore::load_default();
        theme::set_choice(&cc.egui_ctx, app.store.data.theme);
        app.upd = UpdateUi::new(&cc.egui_ctx, &app.store);
        app.def_app = DefaultApp::detect();
        app.shot = crate::devshot::DevShot::from_env();
        app
    }

    /// Создание без eframe (тесты): контекст egui и папка с `pdfium.dll`.
    pub fn with_context(ctx: egui::Context, dll_dir: Option<PathBuf>, path: Option<PathBuf>) -> Self {
        let handle = spawn(ctx, dll_dir);
        let path = path.map(|p| std::path::absolute(&p).unwrap_or(p));
        if let Some(p) = &path {
            let job = if office::app_for(p).is_some() { Job::Convert(ConvertOp::OpenOffice { src: p.clone() }) } else { Job::Open(p.clone()) };
            let _ = handle.job_tx.send(job);
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
            stale: HashSet::new(),
            ghosts: Vec::new(),
            last_request: None,
            gen: 0,
            opened: false,
            dirty: false,
            loading_page: false,
            error: None,
            pending: Vec::new(),
            drag: None,
            tool: Tool::Hand,
            pen_color: PEN_COLORS[0].0,
            pen_width: 2.5,
            marker_color: MARKER_COLORS[0].0,
            text_boxes: HashMap::new(),
            text_requested: HashSet::new(),
            text_edit: None,
            text_size: 12.0,
            text_color: Color32::BLACK,
            opening: path.clone(),
            untitled: None,
            central: Rect::NOTHING,
            search_scroll_to: None,
            show_sidebar: true,
            side_tab: SideTab::Pages,
            thumbs: HashMap::new(),
            thumb_requested: HashSet::new(),
            thumb_inflight: 0,
            sidebar_page: None,
            view_pending: true,
            menu_page: None,
            search_text: String::new(),
            search_query: String::new(),
            hits: Vec::new(),
            active_hit: None,
            searching: false,
            goto_text: String::new(),
            note_dialog: None,
            convert: ConvertDialog::default(),
            close_dialog: false,
            show_help: false,
            print: PrintDialog::default(),
            sign: SignDialog::default(),
            print_cancel: None,
            status: None,
            progress: None,
            cursor_pt: None,
            store: SettingsStore::in_memory(),
            upd: UpdateUi::disabled(),
            def_app: DefaultApp::disabled(),
            show_settings: false,
            shot: None,
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
        self.stale.clear();
        self.ghosts.clear();
        self.text_boxes.clear();
        self.text_requested.clear();
        self.text_edit = None;
        self.thumbs.clear();
        self.thumb_requested.clear();
        self.thumb_inflight = 0;
        self.last_request = None;
    }

    /// Запомнить страницу текущего документа в списке недавних.
    fn remember_page(&mut self) {
        if let (true, Some(p)) = (self.opened, self.path.clone()) {
            let page = self.current_page;
            self.store.update(|s| s.set_recent_page(&p, page));
        }
    }

    fn open_dialog(&mut self) {
        let mut all = vec!["pdf"];
        all.extend_from_slice(office::OFFICE_EXTS);
        let mut d = rfd::FileDialog::new()
            .add_filter("PDF и документы Office", &all)
            .add_filter("PDF", &["pdf"])
            .add_filter("Word", office::WORD_EXTS)
            .add_filter("Excel", office::EXCEL_EXTS)
            .add_filter("PowerPoint", office::POWERPOINT_EXTS);
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        if let Some(p) = d.pick_file() {
            self.open_path(p);
        }
    }

    fn open_recent(&mut self, p: PathBuf) {
        if p.exists() {
            self.open_path(p);
        } else {
            self.store.update(|s| s.forget_recent(&p));
            self.set_status("Файл не найден — он убран из недавних", true);
        }
    }

    fn home_action(&mut self, a: HomeAction) {
        match a {
            HomeAction::OpenDialog => self.open_dialog(),
            HomeAction::Open(p) => self.open_recent(p),
            HomeAction::Forget(p) => self.store.update(|s| s.forget_recent(&p)),
            HomeAction::Convert(m) => {
                self.convert.open_with(m, Vec::new());
                self.convert.pick_inputs();
            }
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        // Абсолютный путь: иначе список недавних сломается при запуске из другой папки.
        let path = std::path::absolute(&path).unwrap_or(path);
        self.remember_page();
        self.opening = Some(path.clone());
        self.untitled = None;
        if let Some(app) = office::app_for(&path) {
            // Документ Office: сначала PDF его же программой, потом откроем (OfficeOpened).
            self.error = None;
            self.opened = false;
            if !office::available(app) {
                self.error = Some(format!("Для открытия этого файла нужен Microsoft {} — он не найден на компьютере", app.name()));
                return;
            }
            let _ = self.handle.job_tx.send(Job::Convert(ConvertOp::OpenOffice { src: path }));
            return;
        }
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
        self.ghosts.clear();
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
        // Незакрытый редактор текста — сначала применить (задания идут по порядку).
        self.commit_text_edit();
        let target = if save_as || self.path.is_none() || self.untitled.is_some() {
            let mut d = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
            if let Some(p) = self.untitled.as_ref().or(self.path.as_ref()) {
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
        match self.untitled.as_ref().or(self.path.as_ref()) {
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
        let mut uploads = 0;
        loop {
            if uploads >= TILE_UPLOADS_PER_FRAME {
                ctx.request_repaint();
                break;
            }
            let Ok(ev) = self.handle.event_rx.try_recv() else { break };
            match ev {
                Event::Opened { path, page_sizes } => {
                    let n = page_sizes.len();
                    let mut restore = 0;
                    if self.untitled.is_none() {
                        self.store.update(|s| {
                            restore = s.recent_page(&path).unwrap_or(0);
                            s.touch_recent(&path);
                        });
                    }
                    self.page_sizes = page_sizes;
                    self.path = Some(path);
                    self.opening = None;
                    self.opened = true;
                    // Сконвертированный из Office документ лежит во временной папке — он не сохранён.
                    self.dirty = self.untitled.is_some();
                    // Открываем там, где закончили в прошлый раз.
                    self.current_page = restore.min(n.saturating_sub(1));
                    self.view_pending = true;
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
                Event::PageChanged { page } => {
                    self.dirty = true;
                    self.text_boxes.remove(&page);
                    self.text_requested.remove(&page);
                    // Старые тайлы страницы показываем, пока не придут новые.
                    self.stale.extend(self.textures.keys().filter(|k| k.0 == page).copied());
                    self.last_request = None;
                    for g in self.ghosts.iter_mut().filter(|g| g.page == page) {
                        g.armed = true;
                    }
                    self.thumb_requested.insert((page, self.rotation));
                    self.thumb_inflight += 1;
                    let _ = self.handle.job_tx.send(Job::Thumbnail { gen: self.gen, page, rotation: self.rotation });
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title(self.title()));
                }
                Event::PageLoading(v) => self.loading_page = v,
                Event::Tile { gen, page, rotation, lod, col, row, width, height, rgba } => {
                    if rotation != self.rotation || gen != self.gen || page >= self.page_count() {
                        continue;
                    }
                    uploads += 1;
                    // Тайлы непрозрачны (рендер на белом), premultiplied == unmultiplied, но вдвое дешевле.
                    let image = egui::ColorImage::from_rgba_premultiplied([width as usize, height as usize], &rgba);
                    let name = format!("p{page}_r{rotation}_{lod}_{col}_{row}");
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                    let key = (page, rotation, lod, col, row);
                    if self.textures.insert(key, tex).is_none() {
                        self.order.push(key);
                    }
                    self.stale.remove(&key);
                }
                Event::Thumbnail { gen, page, rotation, width, height, rgba } => {
                    self.thumb_inflight = self.thumb_inflight.saturating_sub(1);
                    if rotation != self.rotation || gen != self.gen {
                        continue;
                    }
                    let image = egui::ColorImage::from_rgba_premultiplied([width as usize, height as usize], &rgba);
                    let tex = ctx.load_texture(format!("thumb{page}_{rotation}"), image, egui::TextureOptions::LINEAR);
                    self.thumbs.insert((page, rotation), tex);
                }
                Event::Saved(path) => {
                    self.store.update(|s| s.touch_recent(&path));
                    self.untitled = None;
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
                    self.search_scroll_to = self.active_hit;
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
                Event::PrintPreview { page, annotations, width, height, rgba } => {
                    self.print.set_preview(page, annotations, width, height, rgba);
                }
                Event::PrintFinished(res) => {
                    self.print_cancel = None;
                    self.progress = None;
                    match res {
                        Ok(n) => self.set_status(format!("Отправлено на печать: {n} стр."), false),
                        Err(PrintError::Cancelled) => self.set_status("Печать отменена", false),
                        Err(PrintError::Failed(e)) => self.set_status(format!("Печать не удалась: {e}"), true),
                    }
                }
                Event::Done(msg) => {
                    self.progress = None;
                    self.set_status(msg, false);
                }
                Event::Error(e) => {
                    self.ghosts.clear();
                    self.progress = None;
                    self.searching = false;
                    self.thumb_inflight = 0;
                    // Экран ошибки — только если не открылся документ; прочее — уведомлением.
                    if !self.opened && self.opening.is_some() {
                        self.error = Some(e);
                    } else {
                        self.set_status(e, true);
                    }
                }
                Event::TextObjects { gen, page, boxes } => {
                    if gen == self.gen {
                        self.text_boxes.insert(page, boxes);
                    }
                }
                Event::OfficeOpened { pdf, source } => {
                    self.store.update(|s| s.touch_recent(&source));
                    self.open_path(pdf);
                    self.untitled = Some(source.with_extension("pdf"));
                    self.opening = Some(source);
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

    /// Страницы, задевающие полосу экрана `y0..=y1`. Раскладка идёт сверху вниз,
    /// поэтому хватает двоичного поиска — без обхода всех страниц каждый кадр.
    fn pages_in(&self, y0: f32, y1: f32) -> std::ops::Range<usize> {
        let z = self.view.zoom;
        let top = (y0 - self.view.offset.y) / z;
        let bottom = (y1 - self.view.offset.y) / z;
        let r = &self.layout.rects;
        let first = r.partition_point(|&(_, t, _, h)| t + h < top);
        let last = r.partition_point(|&(_, t, _, _)| t <= bottom);
        first..last.max(first)
    }

    /// Экранная точка → (страница, x, y в display pt), если попала на страницу.
    fn page_at(&self, pos: Pos2) -> Option<(usize, f32, f32)> {
        for i in self.pages_in(pos.y, pos.y) {
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
        for i in self.pages_in(canvas.top(), canvas.bottom()) {
            let r = self.page_screen_rect(i);
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
        // Окна печати и подписи обрабатывают Enter/Esc сами; просмотр позади не трогаем.
        if self.print.open || self.sign.is_open() {
            return;
        }
        // Хоткеи не должны срабатывать, пока фокус в текстовом поле.
        let typing = ctx.memory(|m| m.focused().is_some());
        let mods = ctx.input(|i| i.modifiers);
        if mods.command {
            let (o, s, f, z, p, w, e) = ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::O),
                    i.key_pressed(egui::Key::S),
                    i.key_pressed(egui::Key::F),
                    i.key_pressed(egui::Key::Z),
                    i.key_pressed(egui::Key::P),
                    i.key_pressed(egui::Key::W),
                    i.key_pressed(egui::Key::E),
                )
            });
            if o {
                self.open_dialog();
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
                self.edit(EditOp::Undo { page: self.current_page });
            }
            if p && self.opened {
                self.open_print();
            }
            if e && self.opened {
                self.export_png();
            }
            if w {
                self.show_sidebar = !self.show_sidebar;
            }
            // Масштаб документа (интерфейс по Ctrl +/− не масштабируется, см. theme::install).
            if self.opened && !typing {
                let (plus, minus, k0, k1, k2) = ctx.input(|i| {
                    (
                        i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                        i.key_pressed(egui::Key::Minus),
                        i.key_pressed(egui::Key::Num0),
                        i.key_pressed(egui::Key::Num1),
                        i.key_pressed(egui::Key::Num2),
                    )
                });
                if plus {
                    self.pending.push(Action::Zoom(ZOOM_STEP));
                }
                if minus {
                    self.pending.push(Action::Zoom(1.0 / ZOOM_STEP));
                }
                if k0 {
                    self.pending.push(Action::FitPage);
                }
                if k1 {
                    self.pending.push(Action::ActualSize);
                }
                if k2 {
                    self.pending.push(Action::FitWidth);
                }
            }
            return;
        }
        // F3 работает и из поля поиска.
        if let Some(back) = ctx.input(|i| i.key_pressed(egui::Key::F3).then_some(i.modifiers.shift)) {
            self.step_hit(if back { -1 } else { 1 });
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
                // Сначала закрываем открытое окно, потом сбрасываем инструмент.
                if self.text_edit.is_some() {
                    // Редактор текста закрывается сам.
                } else if self.show_help {
                    self.show_help = false;
                } else if self.convert.open {
                    self.convert.open = false;
                } else if self.show_settings {
                    self.show_settings = false;
                } else {
                    self.tool = Tool::Hand;
                    self.drag = None;
                }
            }
            if k(egui::Key::H) {
                self.tool = Tool::Hand;
            }
            if k(egui::Key::T) {
                self.tool = Tool::Select;
            }
            if k(egui::Key::E) {
                self.tool = Tool::EditText;
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
            if k(egui::Key::G) {
                self.tool = Tool::Sign;
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
        if self.convert.open {
            if !self.convert.add_dropped(paths) {
                self.set_status(format!("Эти файлы не подходят для операции «{}»", self.convert.mode.label()), true);
            }
            return;
        }
        let office_n = paths.iter().filter(|p| office::app_for(p).is_some()).count();
        if office_n > 0 {
            // Один документ Office — открыть как PDF; несколько файлов — собрать в один.
            if paths.len() == 1 {
                self.open_path(paths[0].clone());
            } else {
                self.convert.open_with(ConvertMode::Merge, paths);
            }
            return;
        }
        if let Some(p) = paths.iter().find(|p| has_ext(p, &["pdf"])) {
            if self.opened && paths.len() == 1 && ctx.input(|i| i.modifiers.shift) {
                let at = self.page_count();
                self.edit(EditOp::InsertPdf { path: p.clone(), at });
            } else {
                self.open_path(p.clone());
            }
        } else if paths.iter().all(|p| has_ext(p, IMAGE_EXTS)) {
            if self.opened {
                let at = self.page_count();
                self.edit(EditOp::InsertImages { paths, at });
            } else {
                self.convert.open_with(ConvertMode::ImagesToPdf, paths);
            }
        } else {
            self.set_status("Поддерживаются PDF, документы Office и изображения", true);
        }
    }

    /// Следующее (`delta` = 1) или предыдущее (−1) совпадение поиска.
    fn step_hit(&mut self, delta: i32) {
        let n = self.hits.len();
        if n == 0 {
            return;
        }
        let next = match self.active_hit {
            Some(cur) => (cur as i32 + delta).rem_euclid(n as i32) as usize,
            None => 0,
        };
        self.select_hit(next);
    }

    fn select_hit(&mut self, idx: usize) {
        let Some(h) = self.hits.get(idx) else { return };
        let page = h.page;
        let first = h.rects.first().map(|r| self.hit_center(page, r));
        self.active_hit = Some(idx);
        self.search_scroll_to = Some(idx);
        if let Some((x, y)) = first {
            self.pending.push(Action::Reveal(page, x, y));
        }
    }

    fn open_print(&mut self) {
        if !self.opened {
            return;
        }
        self.print.show_for(self.store.data.last_printer.as_deref(), self.current_page);
    }

    fn print_window(&mut self, ctx: &egui::Context) {
        if !self.print.open {
            return;
        }
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "pdfsmith".into());
        let doc = DocInfo { name, current: self.current_page, sizes: &self.page_sizes, busy: self.print_cancel.is_some() };
        for req in self.print.ui(ctx, &doc) {
            match req {
                PrintRequest::Preview { page, annotations } => {
                    let _ = self.handle.job_tx.send(Job::PrintPreview { page, annotations });
                }
                PrintRequest::Print(job) => {
                    if let Some(p) = self.print.printer() {
                        if self.store.data.last_printer.as_deref() != Some(p) {
                            self.store.data.last_printer = Some(p.to_string());
                            self.store.save();
                        }
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    self.print_cancel = Some(cancel.clone());
                    self.progress = Some(("Подготовка печати…".into(), 0.0));
                    let _ = self.handle.job_tx.send(Job::Print { job, cancel });
                }
            }
        }
    }

    fn sign_window(&mut self, ctx: &egui::Context) {
        if let Some(choice) = self.sign.ui(ctx) {
            self.start_sign(choice);
        }
    }

    /// Спрашивает, куда сохранить подписанную копию, и отдаёт задание.
    fn start_sign(&mut self, c: SignChoice) {
        let mut d = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
        let stem = self.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "документ".into());
        d = d.set_file_name(format!("{stem}_подписан.pdf"));
        if let Some(dir) = self.path.as_ref().and_then(|p| p.parent()) {
            d = d.set_directory(dir);
        }
        let Some(out) = d.save_file() else { return };
        let thumb = hex(&c.cert.thumbprint);
        if self.store.data.last_cert.as_deref() != Some(thumb.as_str()) {
            self.store.data.last_cert = Some(thumb);
            self.store.save();
        }
        self.tool = Tool::Hand;
        self.progress = Some(("Подпись…".into(), 0.1));
        let job = SignJob { page: c.page, rotation: c.rotation, rect: c.rect, cert: c.cert, reason: c.reason, location: c.location, out };
        let _ = self.handle.job_tx.send(Job::Sign(Box::new(job)));
    }

    /// Экспорт открытого документа в формат Office: спросить, куда, и запустить.
    fn export_to(&mut self, mode: ConvertMode) {
        let Some(src) = self.path.clone() else { return };
        if self.dirty || self.untitled.is_some() {
            self.set_status("Сначала сохраните документ (Ctrl+S) — экспорт берёт файл с диска", true);
            return;
        }
        let stem = src.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or("документ".into());
        let (name, ext) = match mode {
            ConvertMode::PdfToWord => ("Документ Word", "docx"),
            ConvertMode::PdfToExcel => ("Книга Excel", "xlsx"),
            _ => ("Презентация PowerPoint", "pptx"),
        };
        let mut d = rfd::FileDialog::new().add_filter(name, &[ext]).set_file_name(format!("{stem}.{ext}"));
        if let Some(dir) = src.parent() {
            d = d.set_directory(dir);
        }
        let Some(out) = d.save_file() else { return };
        let op = match mode {
            ConvertMode::PdfToWord => ConvertOp::PdfToWord { src, out },
            ConvertMode::PdfToExcel => ConvertOp::PdfToExcel { src, out },
            _ => ConvertOp::PdfToPowerPoint { src, out },
        };
        self.progress = Some(("Запуск Office…".into(), 0.05));
        let _ = self.handle.job_tx.send(Job::Convert(op));
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
                    Tool::Select | Tool::Highlight | Tool::Rect | Tool::Sign => Some(Drag::Rect { page, start: (x, y), cur: (x, y) }),
                    Tool::EditText => match self.text_box_at(page, x, y) {
                        Some(b) => Some(Drag::Move { page, index: b.index, start: (x, y), cur: (x, y) }),
                        None => Some(Drag::Pan),
                    },
                    _ => None,
                },
                (false, None) => Some(Drag::Pan),
            };
        }
        if response.dragged() {
            let drag_page = match &self.drag {
                Some(Drag::Ink { page, .. }) | Some(Drag::Rect { page, .. }) | Some(Drag::Move { page, .. }) => Some(*page),
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
                Some(Drag::Move { cur, .. }) => {
                    if let Some((raw, _)) = rel {
                        *cur = raw;
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
        if response.clicked() && self.tool == Tool::EditText {
            self.commit_text_edit();
            if let Some((page, x, y)) = pointer.and_then(|p| self.page_at(p)) {
                self.start_text_edit(page, x, y);
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
            Drag::Move { page, index, start, cur } => {
                let (dx, dy) = (cur.0 - start.0, cur.1 - start.1);
                if dx.abs() + dy.abs() > 0.5 {
                    self.edit(EditOp::MoveObject { page, index, dx, dy });
                }
            }
            Drag::Ink { page, points } => {
                if points.len() >= 2 {
                    self.ghost(page, Mark::Ink(points.clone()));
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
                        self.ghost(page, Mark::Marker(start, cur));
                        let c = self.marker_color;
                        self.edit(EditOp::Highlight { page, rect, color: Rgba(c.r(), c.g(), c.b(), 255) });
                    }
                    Tool::Rect => {
                        self.ghost(page, Mark::Frame(start, cur));
                        self.edit(EditOp::Rect { page, rect, color: rgba, width: self.pen_width });
                    }
                    Tool::Sign => {
                        if (rect.x1 - rect.x0) < 20.0 || (rect.y1 - rect.y0) < 10.0 {
                            self.set_status("Обведите место для подписи побольше", true);
                            return;
                        }
                        self.sign.show_for(page, self.rotation, rect, self.store.data.last_cert.as_deref());
                    }
                    _ => {}
                }
            }
        }
    }

    fn ghost(&mut self, page: usize, mark: Mark) {
        let color = if matches!(mark, Mark::Marker(..)) { self.marker_color } else { self.pen_color };
        self.ghosts.push(Ghost { page, mark, color, width: self.pen_width, armed: false });
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
                .filter(|(c, r)| {
                    let k = (*page, self.rotation, lod, *c, *r);
                    !self.textures.contains_key(&k) || self.stale.contains(&k)
                })
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

    /// Рисует готовые тайлы страницы; нарисованные ключи — в `drawn` (их не вытесняем).
    /// Берём и более резкие LOD (после отдаления они ещё годятся, пока нет своих).
    fn draw_page_tiles(&self, painter: &egui::Painter, canvas: Rect, page: usize, current_lod: i32, drawn: &mut Vec<TexKey>) {
        let mut keys: Vec<&TexKey> = self
            .textures
            .keys()
            .filter(|(p, rot, lod, _, _)| *p == page && *rot == self.rotation && *lod <= current_lod + 2)
            .collect();
        // Устаревшие снизу, затем грубые, резкие поверх.
        keys.sort_by_key(|k| (!self.stale.contains(*k), k.2));
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        for &&(_, _, lod, col, row) in &keys {
            let tex = &self.textures[&(page, self.rotation, lod, col, row)];
            let rect = self.tile_screen_rect(page, lod, col, row, tex.size());
            if rect.intersects(canvas) {
                painter.image(tex.id(), rect, uv, Color32::WHITE);
                drawn.push((page, self.rotation, lod, col, row));
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
            self.stale.remove(&key);
        }
    }

    fn draw_overlays(&self, painter: &egui::Painter, canvas: Rect, visible: std::ops::Range<usize>) {
        // Результаты поиска: они упорядочены по страницам — берём только видимые.
        if !self.hits.is_empty() {
            let lo = self.hits.partition_point(|h| h.page < visible.start);
            let hi = self.hits.partition_point(|h| h.page < visible.end);
            for (idx, h) in self.hits.iter().enumerate().take(hi).skip(lo) {
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
        // Рамки текстовых объектов в режиме правки.
        if self.tool == Tool::EditText {
            let hover = self.cursor_pt.and_then(|(p, x, y)| self.text_box_at(p, x, y).map(|b| (p, b.index)));
            let editing = self.text_edit.as_ref().and_then(|e| e.index.map(|i| (e.page, i)));
            for page in visible.clone() {
                let Some(boxes) = self.text_boxes.get(&page) else { continue };
                let base = self.page_screen_rect(page).min;
                for b in boxes {
                    if editing == Some((page, b.index)) {
                        continue;
                    }
                    let (x0, y0, x1, y1) = self.hit_rect_rotated(page, &b.rect);
                    let mut rect = Rect::from_min_max(base + Vec2::new(x0, y0) * self.view.zoom, base + Vec2::new(x1, y1) * self.view.zoom).expand(1.5);
                    if let Some(Drag::Move { page: dp, index, start, cur }) = &self.drag {
                        if *dp == page && *index == b.index {
                            rect = rect.translate(Vec2::new(cur.0 - start.0, cur.1 - start.1) * self.view.zoom);
                        }
                    }
                    if !rect.intersects(canvas) {
                        continue;
                    }
                    let a = theme::accent();
                    if hover == Some((page, b.index)) || matches!(&self.drag, Some(Drag::Move { index, .. }) if *index == b.index) {
                        painter.rect_filled(rect, 2.0, Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 28));
                        painter.rect_stroke(rect, 2.0, Stroke::new(1.5, a));
                    } else {
                        painter.rect_stroke(rect, 2.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 40)));
                    }
                }
            }
        }
        // Свежие аннотации, которых ещё нет в тайлах.
        for g in &self.ghosts {
            if visible.contains(&g.page) {
                self.draw_mark(painter, g.page, &g.mark, g.color, g.width);
            }
        }
        // Текущее действие инструмента.
        match &self.drag {
            Some(Drag::Rect { page, start, cur }) => match self.tool {
                Tool::Select => {
                    let rect = self.disp_rect_on_screen(*page, *start, *cur);
                    painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(80, 150, 255, 60));
                    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_rgb(80, 150, 255)));
                }
                Tool::Highlight => self.draw_mark(painter, *page, &Mark::Marker(*start, *cur), self.marker_color, self.pen_width),
                Tool::Sign => self.draw_sign_frame(painter, *page, *start, *cur),
                _ => self.draw_mark(painter, *page, &Mark::Frame(*start, *cur), self.pen_color, self.pen_width),
            },
            Some(Drag::Ink { page, points }) => {
                let base = self.page_screen_rect(*page).min;
                let pts: Vec<Pos2> = points.iter().map(|&(x, y)| base + Vec2::new(x, y) * self.view.zoom).collect();
                painter.add(egui::Shape::line(pts, Stroke::new(self.pen_width * self.view.zoom, self.pen_color)));
            }
            _ => {}
        }
        // Место будущего штампа, пока открыто окно подписи.
        if let Some((page, rotation, r)) = self.sign.target() {
            if rotation == self.rotation && visible.contains(&page) {
                self.draw_sign_frame(painter, page, (r.x0, r.y0), (r.x1, r.y1));
            }
        }
    }

    fn draw_sign_frame(&self, painter: &egui::Painter, page: usize, a: (f32, f32), b: (f32, f32)) {
        let rect = self.disp_rect_on_screen(page, a, b);
        painter.rect_filled(rect, 2.0, Color32::from_rgba_unmultiplied(31, 58, 147, 30));
        painter.rect_stroke(rect, 2.0, Stroke::new(1.5, Color32::from_rgb(31, 58, 147)));
    }

    fn disp_rect_on_screen(&self, page: usize, a: (f32, f32), b: (f32, f32)) -> Rect {
        let base = self.page_screen_rect(page).min;
        Rect::from_two_pos(base + Vec2::new(a.0, a.1) * self.view.zoom, base + Vec2::new(b.0, b.1) * self.view.zoom)
    }

    fn draw_mark(&self, painter: &egui::Painter, page: usize, mark: &Mark, color: Color32, width: f32) {
        let stroke = Stroke::new(width * self.view.zoom, color);
        match mark {
            Mark::Ink(points) => {
                let base = self.page_screen_rect(page).min;
                let pts: Vec<Pos2> = points.iter().map(|&(x, y)| base + Vec2::new(x, y) * self.view.zoom).collect();
                painter.add(egui::Shape::line(pts, stroke));
            }
            Mark::Frame(a, b) => {
                painter.rect_stroke(self.disp_rect_on_screen(page, *a, *b), 0.0, stroke);
            }
            Mark::Marker(a, b) => {
                painter.rect_filled(self.disp_rect_on_screen(page, *a, *b), 0.0, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 110));
            }
            Mark::Text { cover, at, text, size } => {
                if let Some((x0, y0, x1, y1)) = cover {
                    painter.rect_filled(self.disp_rect_on_screen(page, (*x0, *y0), (*x1, *y1)).expand(1.0), 0.0, Color32::WHITE);
                }
                let pos = self.page_screen_rect(page).min + Vec2::new(at.0, at.1) * self.view.zoom;
                painter.text(pos, egui::Align2::LEFT_TOP, text, egui::FontId::proportional((size * self.view.zoom).max(1.0)), color);
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
        let canvas = response.rect;
        painter.rect_filled(canvas, 0.0, theme::canvas());

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
            (Tool::EditText, Some(Drag::Move { .. })) => egui::CursorIcon::Grabbing,
            (Tool::EditText, _) => match response.hover_pos().and_then(|p| self.page_at(p)) {
                Some((page, x, y)) if self.text_box_at(page, x, y).is_some() => egui::CursorIcon::Text,
                Some(_) => egui::CursorIcon::Crosshair,
                None => egui::CursorIcon::Default,
            },
            _ => egui::CursorIcon::Crosshair,
        };
        if response.hovered() {
            ui.ctx().set_cursor_icon(cursor_icon);
        }
        self.cursor_pt = response.hover_pos().and_then(|p| self.page_at(p));

        // Контекстное меню страницы. Страницу запоминаем в момент ПКМ: пока курсор
        // над самим меню, холст не под курсором, и меню должно рисоваться дальше.
        if response.secondary_clicked() {
            self.menu_page = response.interact_pointer_pos().and_then(|p| self.page_at(p)).map(|(page, _, _)| page);
        }
        if let Some(page) = self.menu_page {
            let mut ops = Vec::new();
            response.context_menu(|ui| self.page_menu(ui, page, &mut ops));
            if !response.context_menu_opened() {
                self.menu_page = None;
            }
            for op in ops {
                self.edit(op);
            }
        }

        let zoom = self.view.zoom;
        // Растр выбираем по физическим пикселям (HiDPI), геометрия — в точках egui.
        let (lod, lod_scale) = lod_scale_for(zoom * ui.ctx().pixels_per_point(), MIN_LOD, MAX_LOD);
        // Запрашиваем чуть шире экрана — прокрутка не показывает пустоты.
        let prefetch = canvas.expand2(Vec2::new(0.0, canvas.height() * 0.35));
        let visible_pages = self.pages_in(prefetch.top(), prefetch.bottom());
        let mut per_page = Vec::new();
        let mut protected = HashSet::new();
        let mut drawn = Vec::new();
        let shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 2.0), blur: 12.0, spread: 0.0, color: theme::shadow() };
        for i in visible_pages.clone() {
            let r = self.page_screen_rect(i);
            if !r.intersects(prefetch) {
                continue;
            }
            // Подложка и мягкая тень страницы.
            painter.add(shadow.as_shape(r, 0.0));
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
            self.draw_page_tiles(&painter, canvas, i, lod, &mut drawn);
        }
        // Текущую страницу — первой в очередь.
        per_page.sort_by_key(|(p, _)| (*p as i64 - self.current_page as i64).abs());
        self.request_visible(lod, lod_scale, per_page);
        protected.extend(drawn);
        self.evict(&protected);
        // Призрак нужен, пока видимые тайлы его страницы не пришли свежими
        // (при любом LOD: после смены масштаба новых тайлов ещё нет вовсе).
        let unfinished: HashSet<usize> = self
            .last_request
            .as_ref()
            .map(|(rot, lod, pages)| {
                pages
                    .iter()
                    .filter(|(p, tiles)| {
                        tiles.iter().any(|&(c, r)| {
                            let k = (*p, *rot, *lod, c, r);
                            !self.textures.contains_key(&k) || self.stale.contains(&k)
                        })
                    })
                    .map(|(p, _)| *p)
                    .collect()
            })
            .unwrap_or_default();
        self.ghosts.retain(|g| !g.armed || unfinished.contains(&g.page));
        let busy = !unfinished.is_empty();
        if self.view_pending && !busy {
            // Боковая панель рисуется раньше холста — дадим ей кадр на миниатюры.
            ui.ctx().request_repaint();
        }
        self.view_pending = busy;
        if self.tool == Tool::EditText {
            for page in visible_pages.clone() {
                if self.text_requested.insert(page) {
                    let _ = self.handle.job_tx.send(Job::TextObjects { gen: self.gen, page });
                }
            }
        } else if self.text_edit.is_some() {
            self.commit_text_edit();
        }
        self.draw_overlays(&painter, canvas, visible_pages);
        self.text_editor(ui, canvas);
    }

    // ------------------------------------------------------------------
    // Правка текста

    /// Текстовый объект под точкой (display pt текущего поворота): самый мелкий из накрывающих.
    fn text_box_at(&self, page: usize, x: f32, y: f32) -> Option<&TextBox> {
        self.text_boxes
            .get(&page)?
            .iter()
            .filter(|b| {
                let (x0, y0, x1, y1) = self.hit_rect_rotated(page, &b.rect);
                x >= x0 - 1.0 && x <= x1 + 1.0 && y >= y0 - 1.0 && y <= y1 + 1.0
            })
            .min_by(|a, b| {
                let area = |r: &DispRect| (r.x1 - r.x0) * (r.y1 - r.y0);
                area(&a.rect).total_cmp(&area(&b.rect))
            })
    }

    /// Открывает редактор: на объекте под точкой или новый текст в точке.
    fn start_text_edit(&mut self, page: usize, x: f32, y: f32) {
        let ed = match self.text_box_at(page, x, y).cloned() {
            Some(b) => TextEditing {
                page,
                index: Some(b.index),
                original: b.text.clone(),
                text: b.text.trim_end().to_string(),
                at: self.hit_rect_rotated(page, &b.rect),
                size: b.size,
                color: Color32::from_rgb(b.color.0, b.color.1, b.color.2),
                id: egui::Id::new(("text_edit", page, b.index, self.gen)),
                focused: false,
            },
            None => TextEditing {
                page,
                index: None,
                original: String::new(),
                text: String::new(),
                at: (x, y, x, y + self.text_size * 1.2),
                size: self.text_size,
                color: self.text_color,
                id: egui::Id::new(("text_new", page, x.to_bits(), y.to_bits())),
                focused: false,
            },
        };
        self.text_edit = Some(ed);
    }

    /// Применяет правку из открытого редактора (если текст изменился).
    fn commit_text_edit(&mut self) {
        let Some(ed) = self.text_edit.take() else { return };
        let (x0, y0, _, _) = ed.at;
        let color = Rgba(ed.color.r(), ed.color.g(), ed.color.b(), 255);
        match ed.index {
            Some(index) => {
                if ed.text == ed.original.trim_end() {
                    return;
                }
                self.ghosts.push(Ghost {
                    page: ed.page,
                    mark: Mark::Text { cover: Some(ed.at), at: (x0, y0), text: ed.text.clone(), size: ed.size },
                    color: ed.color,
                    width: 0.0,
                    armed: false,
                });
                // Хвостовой пробел строки сохраняем: PDFium считает его частью объекта.
                let tail = &ed.original[ed.original.trim_end().len()..];
                let text = if ed.text.trim().is_empty() { String::new() } else { format!("{}{tail}", ed.text) };
                self.edit(EditOp::ReplaceText { page: ed.page, index, expect: ed.original, text });
            }
            None => {
                if ed.text.trim().is_empty() {
                    return;
                }
                self.ghosts.push(Ghost {
                    page: ed.page,
                    mark: Mark::Text { cover: None, at: (x0, y0), text: ed.text.clone(), size: ed.size },
                    color: ed.color,
                    width: 0.0,
                    armed: false,
                });
                self.edit(EditOp::AddText { page: ed.page, x: x0, y: y0, text: ed.text, size: ed.size, color });
            }
        }
    }

    /// Встроенный редактор поверх страницы: Enter — применить, Esc — отменить.
    fn text_editor(&mut self, ui: &mut egui::Ui, canvas: Rect) {
        let zoom = self.view.zoom;
        let Some(ed) = &self.text_edit else { return };
        let base = self.page_screen_rect(ed.page).min;
        let (x0, y0, x1, y1) = ed.at;
        let orig = Rect::from_min_max(base + Vec2::new(x0, y0) * zoom, base + Vec2::new(x1, y1) * zoom);
        if !orig.intersects(canvas) {
            return;
        }
        let px = (ed.size * zoom).clamp(6.0, 400.0);
        let font = egui::FontId::proportional(px);
        let measured = ui.fonts(|f| f.layout_no_wrap(format!("{}  ", ed.text), font.clone(), Color32::WHITE).size().x);
        let w = measured.max(orig.width()).max(px * 3.0);
        let h = (px * 1.25).max(orig.height());
        let rect = Rect::from_min_size(orig.min, Vec2::new(w, h));
        // Исходный текст закрываем подложкой — видно только редактируемое.
        ui.painter().rect_filled(orig.union(rect).expand(2.0), 2.0, Color32::WHITE);
        let ed = self.text_edit.as_mut().unwrap();
        let resp = ui.put(
            rect,
            egui::TextEdit::singleline(&mut ed.text)
                .id(ed.id)
                .font(font)
                .text_color(ed.color)
                .frame(false)
                .margin(egui::Margin::ZERO)
                .desired_width(w),
        );
        ui.painter().rect_stroke(rect.expand(3.0), 3.0, Stroke::new(1.5, theme::accent()));
        if !ed.focused {
            ed.focused = true;
            resp.request_focus();
        } else if resp.lost_focus() {
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.text_edit = None;
            } else {
                self.commit_text_edit();
            }
        }
    }

    fn page_menu(&self, ui: &mut egui::Ui, page: usize, ops: &mut Vec<EditOp>) {
        ui.set_min_width(220.0);
        ui.label(egui::RichText::new(format!("Страница {}", page + 1)).color(theme::muted()).small());
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
            if ui.button(egui::RichText::new(format!("{}  Удалить страницу", ph::TRASH)).color(theme::danger())).clicked() {
                ops.push(EditOp::DeletePage(page));
                ui.close_menu();
            }
        }
    }

    // ------------------------------------------------------------------
    // Панели

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 32.0), egui::Sense::hover());
        let compact = rect.width() < 980.0;
        let zone = |ui: &mut egui::Ui, layout: egui::Layout| ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
        let mut left = zone(ui, egui::Layout::left_to_right(egui::Align::Center));
        self.toolbar_left(&mut left);
        let left_end = left.min_rect().right();
        let mut right = zone(ui, egui::Layout::right_to_left(egui::Align::Center));
        self.toolbar_right(&mut right, compact);
        let right_start = right.min_rect().left();
        if !self.opened {
            return;
        }
        // Навигация и масштаб — по центру окна, если хватает места; ширину берём с прошлого кадра.
        let id = egui::Id::new("toolbar_center_w");
        let w = ui.ctx().data(|d| d.get_temp::<f32>(id)).unwrap_or(360.0);
        let x = (rect.center().x - w * 0.5).min(right_start - 12.0 - w).max(left_end + 12.0);
        let crect = Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(rect.right(), rect.bottom()));
        let mut center = ui.new_child(egui::UiBuilder::new().max_rect(crect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        self.toolbar_center(&mut center, compact);
        let used = center.min_rect().width();
        if (used - w).abs() > 0.5 {
            ui.ctx().data_mut(|d| d.insert_temp(id, used));
            ui.ctx().request_repaint();
        }
    }

    fn toolbar_left(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.opened, |ui| {
            if icon_button(ui, ph::SIDEBAR_SIMPLE, "Панель страниц и поиска  (Ctrl+W)", self.show_sidebar && self.opened).clicked() {
                self.show_sidebar = !self.show_sidebar;
            }
        });
        vsep(ui);
        ui.spacing_mut().item_spacing.x = 2.0;
        if icon_button(ui, ph::FOLDER_OPEN, "Открыть  (Ctrl+O)", false).clicked() {
            self.open_dialog();
        }
        self.recent_menu(ui);
        ui.add_space(4.0);
        ui.add_enabled_ui(self.opened, |ui| {
            let tip = if self.dirty { "Сохранить изменения  (Ctrl+S)" } else { "Сохранить  (Ctrl+S)" };
            let r = icon_button(ui, ph::FLOPPY_DISK, tip, false);
            if self.dirty {
                // Точка на дискете: есть несохранённые правки.
                ui.painter().circle_filled(r.rect.right_top() + Vec2::new(-7.0, 7.0), 3.5, theme::accent());
            }
            if r.clicked() {
                self.save(false);
            }
            if icon_button(ui, ph::PRINTER, "Печать…  (Ctrl+P)", self.print.open).clicked() {
                self.open_print();
            }
        });
    }

    fn recent_menu(&mut self, ui: &mut egui::Ui) {
        let recent = self.store.data.recent.clone();
        let mut open: Option<PathBuf> = None;
        let mut clear = false;
        ui.scope(|ui| {
            theme::flat(ui);
            ui.spacing_mut().button_padding = Vec2::new(3.0, 4.0);
            ui.add_enabled_ui(!recent.is_empty(), |ui| {
                ui.menu_button(egui::RichText::new(ph::CARET_DOWN).size(11.0).color(theme::muted()), |ui| {
                    ui.set_min_width(300.0);
                    theme::section_label(ui, "НЕДАВНИЕ ФАЙЛЫ");
                    for r in &recent {
                        let name = r.path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        let color = if r.path.exists() { theme::text() } else { theme::muted() };
                        let text = egui::RichText::new(format!("{}  {name}", ph::FILE_PDF)).color(color);
                        if ui.button(text).on_hover_text(r.path.display().to_string()).clicked() {
                            open = Some(r.path.clone());
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    if ui.button(egui::RichText::new("Очистить список").color(theme::muted())).clicked() {
                        clear = true;
                        ui.close_menu();
                    }
                })
                .response
                .on_hover_text("Недавние файлы");
            });
        });
        if clear {
            self.store.update(|s| s.recent.clear());
        }
        if let Some(p) = open {
            self.open_recent(p);
        }
    }

    fn toolbar_center(&mut self, ui: &mut egui::Ui, compact: bool) {
        let n = self.page_count().max(1);
        theme::pill_frame().show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_button(ui, ph::CARET_LEFT, "Предыдущая страница  (PageUp)", false).clicked() && self.current_page > 0 {
                self.pending.push(Action::GotoPage(self.current_page - 1));
            }
            self.page_field(ui);
            ui.label(egui::RichText::new(format!("из {n}")).color(theme::muted()));
            ui.add_space(2.0);
            if icon_button(ui, ph::CARET_RIGHT, "Следующая страница  (PageDown)", false).clicked() {
                self.pending.push(Action::GotoPage(self.current_page + 1));
            }
        });
        ui.add_space(8.0);
        theme::pill_frame().show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_button(ui, ph::MINUS, "Мельче  (Ctrl −)", false).clicked() {
                self.pending.push(Action::Zoom(1.0 / ZOOM_STEP));
            }
            ui.scope(|ui| {
                theme::flat(ui);
                egui::ComboBox::from_id_salt("zoom")
                    .selected_text(egui::RichText::new(format!("{:.0}%", self.view.zoom * 100.0)).monospace())
                    .width(64.0)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, format!("{}  Вся страница", ph::FRAME_CORNERS)).clicked() {
                            self.pending.push(Action::FitPage);
                        }
                        if ui.selectable_label(false, format!("{}  По ширине", ph::ARROWS_OUT_LINE_HORIZONTAL)).clicked() {
                            self.pending.push(Action::FitWidth);
                        }
                        ui.separator();
                        for z in [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
                            if ui.selectable_label(false, format!("{:.0}%", z * 100.0)).clicked() {
                                self.pending.push(Action::Zoom(z / self.view.zoom));
                            }
                        }
                    })
                    .response
                    .on_hover_text("Масштаб");
            });
            if icon_button(ui, ph::PLUS, "Крупнее  (Ctrl +)", false).clicked() {
                self.pending.push(Action::Zoom(ZOOM_STEP));
            }
            if !compact {
                vsep(ui);
                if icon_button(ui, ph::FRAME_CORNERS, "Вся страница  (Ctrl 0)", false).clicked() {
                    self.pending.push(Action::FitPage);
                }
                if icon_button(ui, ph::ARROWS_OUT_LINE_HORIZONTAL, "По ширине  (Ctrl 2)", false).clicked() {
                    self.pending.push(Action::FitWidth);
                }
            }
        });
    }

    /// Поле номера страницы: в покое показывает текущую, при фокусе выделяет всё.
    fn page_field(&mut self, ui: &mut egui::Ui) {
        let id = egui::Id::new("goto_page");
        if !ui.memory(|m| m.has_focus(id)) {
            self.goto_text = (self.current_page + 1).to_string();
        }
        let out = egui::TextEdit::singleline(&mut self.goto_text)
            .id(id)
            .desired_width(34.0)
            .horizontal_align(egui::Align::Center)
            .margin(Vec2::new(4.0, 3.0))
            .show(ui);
        if out.response.gained_focus() {
            let mut st = out.state.clone();
            let end = self.goto_text.chars().count();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
            st.store(ui.ctx(), id);
        }
        if out.response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Ok(k) = self.goto_text.trim().parse::<usize>() {
                self.pending.push(Action::GotoPage(k.saturating_sub(1)));
            }
        }
        out.response.on_hover_text("Номер страницы: введите и нажмите Enter");
    }

    fn toolbar_right(&mut self, ui: &mut egui::Ui, compact: bool) {
        self.more_menu(ui);
        if icon_button(ui, ph::GEAR, "Настройки", self.show_settings).clicked() {
            self.show_settings = !self.show_settings;
        }
        vsep(ui);
        let text = if compact { ph::SWAP.to_string() } else { format!("{}  Конвертер", ph::SWAP) };
        let btn = egui::Button::new(text).min_size(Vec2::new(0.0, 28.0)).selected(self.convert.open);
        if ui.add(btn).on_hover_text("Объединить, разделить, PDF ↔ картинки и текст").clicked() {
            if self.convert.open {
                self.convert.open = false;
            } else if self.convert.inputs.is_empty() {
                let mode = self.convert.mode;
                self.convert.open_with(mode, self.path.iter().cloned().collect());
            } else {
                self.convert.open = true;
            }
        }
    }

    fn more_menu(&mut self, ui: &mut egui::Ui) {
        let can = self.opened;
        ui.scope(|ui| {
            theme::flat(ui);
            ui.spacing_mut().button_padding = Vec2::new(7.0, 4.0);
            ui.menu_button(egui::RichText::new(ph::DOTS_THREE).size(theme::ICON), |ui| {
                ui.set_min_width(250.0);
                let item = |icon: &str, text: &str, keys: &str| egui::Button::new(format!("{icon}  {text}")).shortcut_text(keys);
                if ui.add_enabled(can, item(ph::EXPORT, "Сохранить как…", "Ctrl+Shift+S")).clicked() {
                    ui.close_menu();
                    self.save(true);
                }
                if ui.add_enabled(can, item(ph::IMAGE, "Страницу в PNG…", "Ctrl+E")).clicked() {
                    ui.close_menu();
                    self.export_png();
                }
                ui.separator();
                theme::section_label(ui, "ЭКСПОРТ");
                for (mode, icon, text) in [
                    (ConvertMode::PdfToWord, ph::MICROSOFT_WORD_LOGO, "В Word (.docx)…"),
                    (ConvertMode::PdfToExcel, ph::MICROSOFT_EXCEL_LOGO, "В Excel (.xlsx)…"),
                    (ConvertMode::PdfToPowerPoint, ph::MICROSOFT_POWERPOINT_LOGO, "В PowerPoint (.pptx)…"),
                ] {
                    if ui.add_enabled(can, item(icon, text, "")).clicked() {
                        ui.close_menu();
                        self.export_to(mode);
                    }
                }
                ui.separator();
                if ui.add_enabled(can, item(ph::ARROW_COUNTER_CLOCKWISE, "Повернуть вид влево", "[")).clicked() {
                    self.rotate_view(-1);
                    ui.close_menu();
                }
                if ui.add_enabled(can, item(ph::ARROW_CLOCKWISE, "Повернуть вид вправо", "]")).clicked() {
                    self.rotate_view(1);
                    ui.close_menu();
                }
                ui.separator();
                if ui.add(item(ph::KEYBOARD, "Горячие клавиши", "F1")).clicked() {
                    self.show_help = true;
                    ui.close_menu();
                }
                if ui.add(item(ph::GEAR, "Настройки", "")).clicked() {
                    self.show_settings = true;
                    ui.close_menu();
                }
                ui.separator();
                ui.label(egui::RichText::new(format!("PDFsmith {}", crate::updates::CURRENT_VERSION)).small().color(theme::muted()));
            })
            .response
            .on_hover_text("Ещё");
        });
    }

    /// Лента инструментов слева от холста.
    fn tool_rail(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for (i, (t, icon, label, tip)) in TOOLS.into_iter().enumerate() {
                if i == 2 || i == 6 {
                    ui.add_space(3.0);
                    ui.separator();
                    ui.add_space(3.0);
                }
                if rail_button(ui, icon, label, tip, self.tool == t).clicked() {
                    // Повторный щелчок по инструменту возвращает к руке.
                    self.tool = if self.tool == t { Tool::Hand } else { t };
                    self.drag = None;
                }
            }
            ui.add_space(3.0);
            ui.separator();
            ui.add_space(3.0);
            if rail_button(ui, ph::ARROW_U_UP_LEFT, "Отменить", "Отменить последнюю правку  (Ctrl+Z)", false).clicked() {
                self.edit(EditOp::Undo { page: self.current_page });
            }
        });
    }

    /// Подсказка и настройки активного инструмента — плашка вверху холста.
    fn tool_hint(&mut self, ctx: &egui::Context) {
        if !self.opened || self.tool == Tool::Hand || !self.central.is_positive() {
            return;
        }
        let (name, hint) = match self.tool {
            Tool::Select => ("Текст", "Обведите текст — он скопируется в буфер обмена"),
            Tool::Highlight => ("Маркер", "Обведите строки, которые нужно выделить"),
            Tool::Pen => ("Перо", "Рисуйте, удерживая левую кнопку мыши"),
            Tool::Rect => ("Рамка", "Растяните рамку на странице"),
            Tool::Note => ("Заметка", "Щёлкните там, где нужна заметка"),
            Tool::Sign => ("Подпись", "Обведите место для штампа подписи"),
            Tool::EditText => ("Правка", "Щёлкните по тексту — изменить, на пустом месте — добавить, тяните — переместить"),
            Tool::Hand => return,
        };
        let icon = TOOLS.iter().find(|t| t.0 == self.tool).map(|t| t.1).unwrap_or(ph::HAND);
        egui::Area::new(egui::Id::new("tool_hint"))
            .order(egui::Order::Foreground)
            .movable(false)
            // Размер на первый (замерочный) кадр: иначе невидимая область по умолчанию
            // 600×400 на кадр перекроет холст и перехватит нажатие.
            .default_size(Vec2::new(760.0, 44.0))
            .pivot(egui::Align2::CENTER_TOP)
            .fixed_pos(Pos2::new(self.central.center().x, self.central.top() + 10.0))
            .show(ctx, |ui| {
                theme::float_frame().rounding(20.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(icon).size(16.0).color(theme::accent()));
                        ui.label(egui::RichText::new(name).strong());
                        ui.label(egui::RichText::new(hint).color(theme::muted()));
                        match self.tool {
                            Tool::Pen | Tool::Rect => {
                                vsep(ui);
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for (c, n) in PEN_COLORS {
                                    if theme::swatch(ui, c, self.pen_color == c, n).clicked() {
                                        self.pen_color = c;
                                    }
                                }
                                let mut rgb = [self.pen_color.r(), self.pen_color.g(), self.pen_color.b()];
                                ui.spacing_mut().interact_size = Vec2::new(22.0, 18.0);
                                if ui.color_edit_button_srgb(&mut rgb).on_hover_text("Свой цвет").changed() {
                                    self.pen_color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                                }
                                ui.add_space(6.0);
                                ui.spacing_mut().slider_width = 70.0;
                                ui.add(egui::Slider::new(&mut self.pen_width, 0.5..=12.0).show_value(false))
                                    .on_hover_text("Толщина линии");
                                ui.label(egui::RichText::new(format!("{:.1} pt", self.pen_width)).small().color(theme::muted()));
                            }
                            Tool::EditText => {
                                vsep(ui);
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for (c, n) in [(Color32::BLACK, "Чёрный")].into_iter().chain(PEN_COLORS) {
                                    if theme::swatch(ui, c, self.text_color == c, n).on_hover_text("Цвет нового текста").clicked() {
                                        self.text_color = c;
                                    }
                                }
                                ui.add_space(6.0);
                                ui.add(egui::DragValue::new(&mut self.text_size).range(4.0..=96.0).speed(0.2).suffix(" pt"))
                                    .on_hover_text("Кегль нового текста");
                            }
                            Tool::Highlight => {
                                vsep(ui);
                                ui.spacing_mut().item_spacing.x = 2.0;
                                for (c, n) in MARKER_COLORS {
                                    if theme::swatch(ui, c, self.marker_color == c, n).clicked() {
                                        self.marker_color = c;
                                    }
                                }
                            }
                            _ => {}
                        }
                        vsep(ui);
                        if ui.button("Готово").on_hover_text("Вернуться к руке  (Esc)").clicked() {
                            self.tool = Tool::Hand;
                        }
                    });
                });
            });
    }

    /// Всплывающее уведомление внизу холста: итог операции или ошибка.
    fn toast(&mut self, ctx: &egui::Context) {
        let Some((msg, at, err)) = self.status.clone() else { return };
        let life = if err { 7.0 } else { 3.5 };
        let t = at.elapsed().as_secs_f32();
        if t > life {
            return;
        }
        let area = if self.central.is_positive() { self.central } else { ctx.screen_rect() };
        let fade = ((life - t) / 0.35).clamp(0.0, 1.0);
        egui::Area::new(egui::Id::new("toast"))
            .order(egui::Order::Foreground)
            .movable(false)
            .interactable(false)
            .default_size(Vec2::new(360.0, 36.0))
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(Pos2::new(area.center().x, area.bottom() - 18.0))
            .show(ctx, |ui| {
                ui.multiply_opacity(fade);
                theme::float_frame().rounding(20.0).show(ui, |ui| {
                    ui.set_max_width((area.width() - 80.0).clamp(200.0, 620.0));
                    ui.horizontal(|ui| {
                        let (icon, color) = if err {
                            (ph::WARNING_CIRCLE, theme::danger())
                        } else if msg.ends_with('…') {
                            (ph::HOURGLASS, theme::muted())
                        } else {
                            (ph::CHECK_CIRCLE, theme::ok())
                        };
                        ui.label(egui::RichText::new(icon).size(16.0).color(color));
                        ui.add(egui::Label::new(msg.as_str()).truncate());
                    });
                });
            });
        if fade < 1.0 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32((life - 0.35 - t).max(0.02)));
        }
    }

    /// Затемнение с подсказкой, пока над окном тащат файлы.
    fn drop_overlay(&self, ctx: &egui::Context) {
        let (any, files, shift) = ctx.input(|i| {
            let files: Vec<PathBuf> = i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect();
            (!i.raw.hovered_files.is_empty(), files, i.modifiers.shift)
        });
        if !any {
            return;
        }
        let pdfs = files.iter().filter(|p| has_ext(p, &["pdf"])).count();
        let imgs = files.iter().filter(|p| has_ext(p, IMAGE_EXTS)).count();
        let offices = files.iter().filter(|p| office::app_for(p).is_some()).count();
        let (title, sub): (String, &str) = if self.convert.open {
            (format!("Добавить в «{}»", self.convert.mode.label()), "подходящие файлы попадут в список конвертера")
        } else if offices > 0 && files.len() == 1 {
            ("Открыть как PDF".into(), "документ сконвертирует установленный Microsoft Office")
        } else if offices > 0 {
            ("Объединить в один PDF".into(), "откроется конвертер со всеми файлами")
        } else if pdfs > 0 {
            if self.opened && shift && files.len() == 1 {
                ("Добавить страницы в конец".into(), "PDF будет вставлен после последней страницы")
            } else if self.opened {
                ("Открыть PDF".into(), "с Shift — добавить его страницы в конец текущего документа")
            } else {
                ("Открыть PDF".into(), "")
            }
        } else if imgs > 0 && imgs == files.len() {
            if self.opened {
                ("Вставить изображения".into(), "каждое станет новой страницей в конце документа")
            } else {
                ("Собрать PDF из изображений".into(), "откроется конвертер")
            }
        } else if files.is_empty() {
            ("Отпустите файл".into(), "")
        } else {
            ("Такие файлы не поддерживаются".into(), "перетащите PDF или изображения")
        };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop_overlay")));
        let screen = ctx.screen_rect();
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(if theme::is_dark() { 150 } else { 110 }));
        let r = screen.shrink(18.0);
        painter.rect_stroke(r, 16.0, Stroke::new(2.0, theme::accent()));
        let c = r.center();
        painter.text(c - Vec2::new(0.0, 40.0), egui::Align2::CENTER_CENTER, ph::FILE_ARROW_UP, egui::FontId::proportional(48.0), theme::accent());
        painter.text(c + Vec2::new(0.0, 14.0), egui::Align2::CENTER_CENTER, title, egui::FontId::proportional(22.0), Color32::WHITE);
        if !sub.is_empty() {
            painter.text(c + Vec2::new(0.0, 44.0), egui::Align2::CENTER_CENTER, sub, egui::FontId::proportional(14.0), Color32::from_gray(215));
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let muted = |s: String| egui::RichText::new(s).color(theme::muted()).small();
            match (self.untitled.as_ref().or(self.path.as_ref()), self.opened) {
                (Some(p), true) => {
                    let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(egui::RichText::new(ph::FILE_PDF).color(theme::muted()));
                    ui.label(egui::RichText::new(name).small()).on_hover_text(p.display().to_string());
                    if self.dirty {
                        ui.label(egui::RichText::new("● не сохранён").color(theme::accent()).small())
                            .on_hover_text("Есть несохранённые изменения — Ctrl+S");
                    }
                }
                _ => {
                    ui.label(muted("Готов к работе".into()));
                }
            }
            if self.loading_page {
                ui.add_space(8.0);
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(muted("Разбор страницы…".into()));
            }
            if let Some(text) = self.upd.status_text(&self.store) {
                ui.add_space(8.0);
                ui.label(muted(text));
            }
            if let Some((msg, frac)) = &self.progress {
                ui.add_space(8.0);
                ui.add(egui::ProgressBar::new(*frac).desired_width(140.0).desired_height(6.0));
                ui.label(muted(msg.clone()));
            }
            if let Some(cancel) = &self.print_cancel {
                let stopping = cancel.load(Ordering::Relaxed);
                let text = if stopping { "Отмена…" } else { "Отменить печать" };
                if ui.add_enabled(!stopping, egui::Button::new(egui::RichText::new(text).small())).clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            if self.opened {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    let (pw, ph_) = self.page_pt(self.current_page);
                    ui.label(muted(format!("{:.0} × {:.0} мм", pw / 72.0 * 25.4, ph_ / 72.0 * 25.4)))
                        .on_hover_text("Размер текущей страницы");
                    if let Some((page, x, y)) = self.cursor_pt {
                        ui.label(muted("·".into()));
                        ui.label(muted(format!("стр. {}: {:.0}, {:.0} мм", page + 1, x / 72.0 * 25.4, y / 72.0 * 25.4)))
                            .on_hover_text("Положение курсора от левого верхнего угла страницы");
                    }
                });
            }
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            theme::pill_frame().show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let w = (SIDEBAR_W - 16.0 - 4.0 - 2.0) / 2.0;
                for (tab, icon, label) in [(SideTab::Pages, ph::SQUARES_FOUR, "Страницы"), (SideTab::Search, ph::MAGNIFYING_GLASS, "Поиск")] {
                    let active = self.side_tab == tab;
                    let text = egui::RichText::new(format!("{icon}  {label}")).color(if active { theme::text() } else { theme::muted() });
                    let fill = if active { theme::seg_active() } else { Color32::TRANSPARENT };
                    let b = egui::Button::new(text).min_size(Vec2::new(w, 26.0)).fill(fill).stroke(Stroke::NONE);
                    if ui.add(b).clicked() {
                        self.side_tab = tab;
                        if tab == SideTab::Search {
                            ui.memory_mut(|m| m.request_focus(egui::Id::new("search_field")));
                        }
                    }
                }
            });
        });
        ui.add_space(6.0);
        match self.side_tab {
            SideTab::Pages => self.pages_panel(ui),
            SideTab::Search => self.search_panel(ui),
        }
    }

    /// Миниатюры страниц. Рисуются только видимые карточки — длинные документы
    /// не нагружают кадр.
    fn pages_panel(&mut self, ui: &mut egui::Ui) {
        let n = self.page_count();
        // Автопрокрутка списка к текущей странице, когда она сменилась.
        let scroll_sidebar = self.sidebar_page != Some(self.current_page);
        self.sidebar_page = Some(self.current_page);
        let card_w = SIDEBAR_W - 24.0;
        let thumb_w = card_w - 44.0;
        let thumb_h = |i: usize| {
            let (pw, ph_) = self.page_pt(i);
            (thumb_w * ph_ / pw.max(1.0)).clamp(30.0, 400.0)
        };
        let mut tops = Vec::with_capacity(n);
        let mut bottoms = Vec::with_capacity(n);
        let mut y = 4.0;
        for i in 0..n {
            tops.push(y);
            y += thumb_h(i) + 30.0;
            bottoms.push(y);
            y += 4.0;
        }
        let total = y + 8.0;
        let mut goto: Option<usize> = None;
        let mut ops: Vec<EditOp> = Vec::new();
        let mut want_thumbs: Vec<usize> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_viewport(ui, |ui, vp| {
            ui.set_height(total);
            let origin = ui.max_rect().min + Vec2::new(((ui.max_rect().width() - card_w) * 0.5).max(0.0), 0.0);
            let card = |i: usize| Rect::from_min_max(origin + Vec2::new(0.0, tops[i]), origin + Vec2::new(card_w, bottoms[i]));
            let first = bottoms.partition_point(|&b| b < vp.min.y);
            let last = tops.partition_point(|&t| t <= vp.max.y);
            for i in first..last.max(first) {
                let rect = card(i);
                let resp = ui.interact(rect, ui.id().with(("thumb", i)), egui::Sense::click());
                let is_cur = i == self.current_page;
                let p = ui.painter();
                if is_cur {
                    p.rect_filled(rect, 8.0, theme::accent_wash());
                } else if resp.hovered() {
                    p.rect_filled(rect, 8.0, theme::hover_wash());
                }
                let th = thumb_h(i);
                let img_rect = Rect::from_min_size(rect.min + Vec2::new((card_w - thumb_w) * 0.5, 8.0), Vec2::new(thumb_w, th));
                let shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 1.0), blur: 6.0, spread: 0.0, color: theme::shadow() };
                p.add(shadow.as_shape(img_rect, 0.0));
                p.rect_filled(img_rect, 0.0, Color32::WHITE);
                if let Some(tex) = self.thumbs.get(&(i, self.rotation)) {
                    let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
                    p.image(tex.id(), img_rect, uv, Color32::WHITE);
                } else {
                    want_thumbs.push(i);
                }
                if is_cur {
                    p.rect_stroke(img_rect.expand(2.0), 2.0, Stroke::new(2.0, theme::accent()));
                }
                p.text(
                    Pos2::new(rect.center().x, img_rect.bottom() + 11.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    egui::FontId::proportional(11.5),
                    if is_cur { theme::accent() } else { theme::muted() },
                );
                if resp.clicked() {
                    goto = Some(i);
                }
                resp.context_menu(|ui| self.page_menu(ui, i, &mut ops));
            }
            if scroll_sidebar && self.current_page < n {
                ui.scroll_to_rect(card(self.current_page), None);
            }
        });
        if let Some(p) = goto {
            self.pending.push(Action::GotoPage(p));
        }
        for op in ops {
            self.edit(op);
        }
        if self.view_pending {
            want_thumbs.clear();
        }
        // Сначала текущая и соседние — их видно первыми.
        want_thumbs.sort_by_key(|&i| (i as i64 - self.current_page as i64).abs());
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
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 4.0;
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.search_text)
                    .id(egui::Id::new("search_field"))
                    .desired_width(SIDEBAR_W - 8.0 - 4.0 - 30.0 - 12.0 - 12.0)
                    .margin(Vec2::new(6.0, 5.0))
                    .hint_text("Найти в документе"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                // Повторный Enter по тому же запросу — к следующему совпадению.
                if !self.hits.is_empty() && self.search_text.trim() == self.search_query {
                    let back = ui.input(|i| i.modifiers.shift);
                    self.step_hit(if back { -1 } else { 1 });
                } else {
                    self.start_search();
                }
                resp.request_focus();
            }
            if self.search_text.is_empty() {
                if icon_button(ui, ph::MAGNIFYING_GLASS, "Искать  (Enter)", false).clicked() {
                    self.start_search();
                }
            } else if icon_button(ui, ph::X, "Очистить поиск", false).clicked() {
                self.search_text.clear();
                self.search_query.clear();
                self.hits.clear();
                self.active_hit = None;
            }
        });
        ui.add_space(6.0);
        if self.searching {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(egui::RichText::new("Ищу…").color(theme::muted()).small());
            });
            return;
        }
        if self.search_query.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.set_width(SIDEBAR_W - 24.0);
                    ui.label(
                        egui::RichText::new("Введите слово и нажмите Enter. Следующее совпадение — Enter или F3, предыдущее — Shift+F3.")
                            .color(theme::muted())
                            .small(),
                    );
                });
            });
            return;
        }
        let n = self.hits.len();
        ui.horizontal(|ui| {
            ui.add_space(10.0);
            let label = match (n, self.active_hit) {
                (0, _) => "Ничего не найдено".to_string(),
                (n, Some(a)) => format!("{} из {n}", a + 1),
                (n, None) => format!("Найдено: {n}"),
            };
            ui.label(egui::RichText::new(label).color(theme::muted()).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_enabled_ui(n > 0, |ui| {
                    if icon_button(ui, ph::CARET_DOWN, "Следующее  (Enter, F3)", false).clicked() {
                        self.step_hit(1);
                    }
                    if icon_button(ui, ph::CARET_UP, "Предыдущее  (Shift+F3)", false).clicked() {
                        self.step_hit(-1);
                    }
                });
            });
        });
        ui.add_space(2.0);
        if n == 0 {
            return;
        }
        let mut pick: Option<usize> = None;
        let mut area = egui::ScrollArea::vertical().id_salt("hits").auto_shrink([false, false]);
        let row_step = HIT_ROW_H + 2.0;
        if let Some(i) = self.search_scroll_to.take() {
            // Прокручиваем, только если строка не видна.
            let st = egui::scroll_area::State::load(ui.ctx(), ui.make_persistent_id("hits"));
            let off = st.map(|s| s.offset.y).unwrap_or(0.0);
            let view_h = ui.available_height();
            let y = i as f32 * row_step;
            if y < off || y + row_step > off + view_h {
                area = area.vertical_scroll_offset((y - view_h * 0.35).max(0.0));
            }
        }
        let query = self.search_query.to_lowercase();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            area.show_rows(ui, HIT_ROW_H, n, |ui, range| {
                for idx in range {
                    let h = &self.hits[idx];
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(SIDEBAR_W - 20.0, HIT_ROW_H), egui::Sense::click());
                    let rect = rect.translate(Vec2::new(4.0, 0.0));
                    let active = self.active_hit == Some(idx);
                    let p = ui.painter();
                    if active {
                        p.rect_filled(rect, 6.0, theme::accent_wash());
                    } else if resp.hovered() {
                        p.rect_filled(rect, 6.0, theme::hover_wash());
                    }
                    p.text(
                        rect.left_top() + Vec2::new(28.0, 6.0),
                        egui::Align2::RIGHT_TOP,
                        format!("{}", h.page + 1),
                        egui::FontId::monospace(11.5),
                        if active { theme::accent() } else { theme::muted() },
                    );
                    let job = snippet_job(&h.snippet, &query, rect.width() - 42.0, active);
                    let galley = ui.fonts(|f| f.layout_job(job));
                    p.galley(rect.left_top() + Vec2::new(36.0, 5.0), galley, theme::text());
                    if resp.on_hover_text(format!("Страница {}", h.page + 1)).clicked() {
                        pick = Some(idx);
                    }
                }
            });
        });
        if let Some(idx) = pick {
            self.select_hit(idx);
            self.search_scroll_to = None;
        }
    }

    fn convert_window(&mut self, ctx: &egui::Context) {
        if let Some(op) = self.convert.ui(ctx, self.progress.as_ref()) {
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
                ui.label(egui::RichText::new(format!("Страница {}", page + 1)).color(theme::muted()).small());
                let r = ui.add(egui::TextEdit::multiline(&mut text).desired_width(340.0).desired_rows(4).hint_text("Текст заметки"));
                r.request_focus();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let ready = !text.trim().is_empty();
                    let add = ui.add_enabled(ready, theme::primary_button("Добавить", ready).min_size(Vec2::new(100.0, 28.0)));
                    if add.clicked() || (ready && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter))) {
                        commit = true;
                        keep = false;
                    }
                    if ui.add(egui::Button::new("Отмена").min_size(Vec2::new(0.0, 28.0))).clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                    ui.label(egui::RichText::new("Ctrl+Enter — добавить").small().color(theme::muted()));
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
        type Rows<'a> = &'a [(&'a [&'a str], &'a str)];
        let section = |ui: &mut egui::Ui, title: &str, rows: Rows| {
            theme::section_label(ui, title);
            egui::Grid::new(title).num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
                for (keys, what) in rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;
                        for k in keys.iter() {
                            theme::keycap(ui, k);
                        }
                    });
                    ui.label(egui::RichText::new(*what).color(theme::muted()));
                    ui.end_row();
                }
            });
            ui.add_space(10.0);
        };
        egui::Window::new("Горячие клавиши")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.columns(2, |cols| {
                    section(&mut cols[0], "НАВИГАЦИЯ", &[
                        (&["Колесо"], "прокрутка (Shift — вбок)"),
                        (&["PgUp", "PgDn"], "страница назад / вперёд"),
                        (&["Home", "End"], "первая / последняя"),
                        (&["Пробел"], "экран вниз (Shift — вверх)"),
                        (&["Ctrl", "F"], "поиск"),
                        (&["F3"], "следующее совпадение"),
                    ]);
                    section(&mut cols[0], "МАСШТАБ", &[
                        (&["Ctrl", "колесо"], "под курсором"),
                        (&["Ctrl", "+", "−"], "крупнее / мельче"),
                        (&["Ctrl", "0"], "вся страница"),
                        (&["Ctrl", "1"], "100 %"),
                        (&["Ctrl", "2"], "по ширине"),
                        (&["[", "]"], "повернуть вид"),
                    ]);
                    section(&mut cols[1], "ИНСТРУМЕНТЫ", &[
                        (&["H"], "рука"),
                        (&["T"], "выделить текст"),
                        (&["E"], "править текст PDF"),
                        (&["M"], "маркер"),
                        (&["P"], "перо"),
                        (&["R"], "рамка"),
                        (&["N"], "заметка"),
                        (&["G"], "электронная подпись"),
                        (&["Esc"], "вернуться к руке"),
                        (&["Ctrl", "Z"], "отменить последнюю правку"),
                    ]);
                    section(&mut cols[1], "ФАЙЛ", &[
                        (&["Ctrl", "O"], "открыть"),
                        (&["Ctrl", "S"], "сохранить (+Shift — как…)"),
                        (&["Ctrl", "P"], "печать"),
                        (&["Ctrl", "E"], "страницу в PNG"),
                        (&["Ctrl", "W"], "панель страниц"),
                    ]);
                });
                ui.label(
                    egui::RichText::new("Правый щелчок по странице — повернуть, вставить, переместить, удалить. Перетащите файл в окно, чтобы открыть; с Shift — добавить в конец.")
                        .small()
                        .color(theme::muted()),
                );
            });
        self.show_help = open;
    }

    fn close_window(&mut self, ctx: &egui::Context) {
        if !self.close_dialog {
            return;
        }
        let name = self.path.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "документ".into());
        let mut choice: Option<u8> = None;
        let modal = egui::Modal::new(egui::Id::new("close_dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(egui::RichText::new("Сохранить изменения?").size(17.0).strong());
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("В «{name}» есть несохранённые изменения. Без сохранения они пропадут.")).color(theme::muted()));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let save = ui.add(theme::primary_button("Сохранить", true).min_size(Vec2::new(110.0, 30.0)));
                if save.clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    choice = Some(0);
                }
                if ui.add(egui::Button::new(egui::RichText::new("Не сохранять").color(theme::danger())).min_size(Vec2::new(0.0, 30.0))).clicked() {
                    choice = Some(1);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Отмена").min_size(Vec2::new(0.0, 30.0))).clicked() {
                        choice = Some(2);
                    }
                });
            });
        });
        if choice.is_none() && modal.should_close() {
            choice = Some(2);
        }
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

    fn dev_shot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        shot.tick(ctx);
        if let Some(w) = shot.window.take() {
            match w.as_str() {
                "convert" => self.convert.open_with(ConvertMode::Merge, self.path.iter().cloned().collect()),
                "help" => self.show_help = true,
                "settings" => self.show_settings = true,
                _ => {}
            }
        }
        if !self.opened {
            return;
        }
        let Some(shot) = &mut self.shot else { return };
        if let Some(t) = shot.tool.take() {
            self.tool = match t.as_str() {
                "pen" => Tool::Pen,
                "marker" => Tool::Highlight,
                "select" => Tool::Select,
                "sign" => Tool::Sign,
                "edit" => Tool::EditText,
                _ => Tool::Hand,
            };
        }
        if let Some(q) = shot.search.take() {
            self.side_tab = SideTab::Search;
            self.search_text = q;
            self.start_search();
        }
        // Проверка правки текста без ввода: заменить первую строку и открыть редактор на следующей.
        if self.tool == Tool::EditText {
            if let Some(boxes) = self.text_boxes.get(&self.current_page).cloned() {
                if let Some(t) = self.shot.as_mut().and_then(|s| s.text.take()) {
                    let page = self.current_page;
                    let mut long = boxes.iter().filter(|b| b.text.chars().count() >= 4);
                    if let Some(b) = long.next() {
                        self.edit(EditOp::ReplaceText { page, index: b.index, expect: b.text.clone(), text: t });
                    }
                    if let Some(b) = long.next() {
                        let (x0, y0, _, _) = self.hit_rect_rotated(page, &b.rect);
                        self.start_text_edit(page, x0 + 1.0, y0 + 1.0);
                    }
                }
            }
        }
    }

    /// Экран, пока документ не открыт: стартовый, загрузка или ошибка.
    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let name = self.opening.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().to_string());
        if let Some(e) = self.error.clone() {
            let mut again = false;
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.32).max(20.0));
                ui.label(egui::RichText::new(ph::WARNING_CIRCLE).size(42.0).color(theme::danger()));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Не удалось открыть файл").size(18.0).strong());
                if let Some(n) = &name {
                    ui.label(egui::RichText::new(n).color(theme::muted()));
                }
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&e).small().color(theme::muted()));
                ui.add_space(16.0);
                again = ui.add(theme::primary_button(format!("{}  Открыть другой файл…", ph::FOLDER_OPEN), true).min_size(Vec2::new(220.0, 34.0))).clicked();
            });
            if again {
                self.open_dialog();
            }
            return;
        }
        if self.opening.is_some() {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() * 0.4).max(20.0));
                ui.add(egui::Spinner::new().size(28.0));
                ui.add_space(10.0);
                let n = name.unwrap_or_default();
                match self.opening.as_deref().and_then(office::app_for) {
                    Some(app) => {
                        ui.label(egui::RichText::new(format!("{} превращает «{n}» в PDF…", app.name())).color(theme::muted()));
                        ui.label(egui::RichText::new("Первый запуск Office может занять до минуты").small().color(theme::muted()));
                    }
                    None => {
                        ui.label(egui::RichText::new(format!("Открываю «{n}»…")).color(theme::muted()));
                    }
                }
            });
            return;
        }
        ui.painter().rect_filled(ui.max_rect(), 0.0, theme::panel());
        if let Some(a) = home::home(ui, &self.store.data.recent) {
            self.home_action(a);
        }
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }

    fn on_exit(&mut self) {
        self.remember_page();
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
        theme::sync(ctx);
        self.poll_events(ctx);
        self.upd.poll(&mut self.store);
        self.def_app.poll();
        self.handle_dropped(ctx);
        self.handle_keyboard(ctx);
        self.dev_shot(ctx);

        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.close_dialog {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_dialog = true;
        }

        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::none().fill(theme::panel()).inner_margin(egui::Margin::symmetric(8.0, 4.0)))
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
            .frame(egui::Frame::none().fill(theme::panel()).inner_margin(egui::Margin::symmetric(8.0, 3.0)))
            .show(ctx, |ui| self.status_bar(ui));
        if self.opened {
            if self.show_sidebar {
                egui::SidePanel::left("sidebar")
                    .exact_width(SIDEBAR_W)
                    .resizable(false)
                    .frame(egui::Frame::none().fill(theme::panel()))
                    .show(ctx, |ui| self.sidebar(ui));
            }
            egui::SidePanel::left("tools")
                .exact_width(theme::RAIL_W)
                .resizable(false)
                .frame(egui::Frame::none().fill(theme::panel()))
                .show(ctx, |ui| self.tool_rail(ui));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::canvas()))
            .show(ctx, |ui| {
                self.central = ui.max_rect();
                if self.opened && self.error.is_none() {
                    self.canvas(ui);
                } else {
                    self.empty_state(ui);
                }
            });

        self.convert_window(ctx);
        self.print_window(ctx);
        self.sign_window(ctx);
        self.note_window(ctx);
        self.help_window(ctx);
        self.close_window(ctx);
        crate::settings_ui::settings_window(ctx, &mut self.show_settings, &mut self.store, &mut self.upd, &mut self.def_app);
        self.tool_hint(ctx);
        self.toast(ctx);
        self.drop_overlay(ctx);
    }
}

/// Сниппет результата поиска: до двух строк, найденное слово подсвечено.
fn snippet_job(snippet: &str, query: &str, width: f32, active: bool) -> egui::text::LayoutJob {
    let base = egui::TextFormat {
        font_id: egui::FontId::proportional(12.5),
        color: if active { theme::text() } else { theme::muted() },
        ..Default::default()
    };
    let a = theme::accent();
    let hl = egui::TextFormat { color: theme::text(), background: Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 70), ..base.clone() };
    let mut job = egui::text::LayoutJob::default();
    // Сниппет начинается с середины слова — отрезаем обрубок до первого пробела.
    let q_at = snippet.to_lowercase().find(query).unwrap_or(usize::MAX);
    let snippet = match snippet.find(' ') {
        Some(sp) if sp < 16 && sp < q_at => {
            job.append("…", 0.0, base.clone());
            &snippet[sp + 1..]
        }
        _ => snippet,
    };
    let lower = snippet.to_lowercase();
    let found = (!query.is_empty() && lower.len() == snippet.len())
        .then(|| lower.find(query))
        .flatten()
        .filter(|&i| snippet.is_char_boundary(i) && snippet.is_char_boundary(i + query.len()));
    match found {
        Some(i) => {
            job.append(&snippet[..i], 0.0, base.clone());
            job.append(&snippet[i..i + query.len()], 0.0, hl);
            job.append(&snippet[i + query.len()..], 0.0, base);
        }
        None => job.append(snippet, 0.0, base),
    }
    job.wrap = egui::text::TextWrapping { max_width: width, max_rows: 2, break_anywhere: false, overflow_character: Some('…') };
    job
}

fn pick_pdf() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file()
}
