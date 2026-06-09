//! pdfsmith — просмотрщик PDF (Этап 2: навигация и UI поверх тайлового ядра).
//!
//! Тайлы видимой области рендерятся в фоне и компонуются на GPU; недостающие
//! подменяются апскейлом более грубого LOD (размыто→резко). Добавлены режимы
//! вписывания, зум с клавиатуры/панели, переключение страниц, drag&drop и
//! диалог открытия файла.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;
use pdfsmith_engine::lod::lod_scale_for;
use pdfsmith_engine::viewport::visible_tiles_center_out;

mod render_thread;
use render_thread::{spawn, Event, Job, RenderHandle, TILE};

const MIN_LOD: i32 = -4;
const MAX_LOD: i32 = 8;
const TEXTURE_CAP: usize = 400;
const ZOOM_STEP: f32 = 1.25;

fn main() {
    env_logger::init();

    // В release нет консоли: показываем панику нативным окном, иначе сбой
    // запуска (например, отсутствие OpenGL) был бы невидим.
    std::panic::set_hook(Box::new(|info| {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("pdfsmith — критическая ошибка")
            .set_description(info.to_string())
            .show();
    }));

    let path = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_title("pdfsmith"),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: build_wgpu_setup(),
            ..Default::default()
        },
        ..Default::default()
    };

    let result = eframe::run_native(
        "pdfsmith",
        options,
        Box::new(move |cc| Ok(Box::new(ViewerApp::new(cc, path)))),
    );

    if let Err(e) = result {
        // Чаще всего — не удалось создать OpenGL-контекст (нет драйверов GPU,
        // удалённый рабочий стол, виртуалка).
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("pdfsmith — не удалось запустить окно")
            .set_description(format!(
                "{e}\n\nВозможные причины: нет поддержки OpenGL (драйверы видеокарты, \
                 удалённый рабочий стол или виртуальная машина)."
            ))
            .show();
    }
}

/// Ключ тайла-текстуры: (страница, поворот, LOD, столбец, строка).
type TexKey = (usize, u8, i32, u32, u32);

/// Отложенное действие, требующее размеров холста (применяется в центр-панели).
#[derive(Clone, Copy)]
enum Action {
    FitPage,
    FitWidth,
    ActualSize,
    Zoom(f32),
}

struct View {
    /// Экранная (глобальная) позиция верхнего-левого угла страницы.
    offset: egui::Vec2,
    /// Масштаб: device-px на пункт.
    zoom: f32,
    needs_fit: bool,
}

impl Default for View {
    fn default() -> Self {
        View { offset: egui::Vec2::ZERO, zoom: 1.0, needs_fit: true }
    }
}

struct ViewerApp {
    handle: RenderHandle,
    page_sizes: Vec<(f32, f32)>,
    page: usize,
    rotation: u8,
    view: View,
    textures: HashMap<TexKey, egui::TextureHandle>,
    order: Vec<TexKey>,
    last_request: Option<(usize, u8, i32, Vec<(u32, u32)>)>,
    opened: bool,
    loading_page: bool,
    error: Option<String>,
    no_file: bool,
    pending: Option<Action>,
    goto_text: String,
    cursor_pt: Option<(f32, f32)>,
    thumbnail: Option<(usize, u8, egui::TextureHandle)>,
    thumb_requested: Option<(usize, u8)>,
    show_minimap: bool,
    status_msg: Option<String>,
}

impl ViewerApp {
    fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let dll_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let handle = spawn(cc.egui_ctx.clone(), dll_dir);

        let no_file = path.is_none();
        if let Some(path) = path {
            let _ = handle.job_tx.send(Job::Open(path));
        }

        ViewerApp {
            handle,
            page_sizes: Vec::new(),
            page: 0,
            rotation: 0,
            view: View::default(),
            textures: HashMap::new(),
            order: Vec::new(),
            last_request: None,
            opened: false,
            loading_page: false,
            error: None,
            no_file,
            pending: None,
            goto_text: String::new(),
            cursor_pt: None,
            thumbnail: None,
            thumb_requested: None,
            show_minimap: true,
            status_msg: None,
        }
    }

    /// Размер текущей страницы в пунктах с учётом поворота (для раскладки/тайлов).
    fn page_pt(&self) -> (f32, f32) {
        let (w, h) = self.page_sizes.get(self.page).copied().unwrap_or((1.0, 1.0));
        if self.rotation & 1 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn rotate(&mut self, delta: i32) {
        self.rotation = (((self.rotation as i32) + delta).rem_euclid(4)) as u8;
        self.view.needs_fit = true;
        self.last_request = None;
    }

    fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    /// Открывает новый файл: сбрасывает состояние просмотра.
    fn open_path(&mut self, path: PathBuf) {
        self.textures.clear();
        self.order.clear();
        self.last_request = None;
        self.error = None;
        self.opened = false;
        self.no_file = false;
        self.page = 0;
        self.rotation = 0;
        self.view = View::default();
        let _ = self.handle.job_tx.send(Job::Open(path));
    }

    fn set_page(&mut self, page: usize) {
        if self.page_count() == 0 {
            return;
        }
        let page = page.min(self.page_count() - 1);
        if page != self.page {
            self.page = page;
            self.view.needs_fit = true;
            self.last_request = None;
        }
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.handle.event_rx.try_recv() {
            match ev {
                Event::Opened { page_sizes } => {
                    self.page_sizes = page_sizes;
                    self.opened = true;
                    self.page = 0;
                    self.view.needs_fit = true;
                }
                Event::PageLoading => self.loading_page = true,
                Event::Tile { lod, col, row, width, height, rgba } => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let name = format!("p{}_r{}_{lod}_{col}_{row}", self.page, self.rotation);
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                    let key = (self.page, self.rotation, lod, col, row);
                    if self.textures.insert(key, tex).is_none() {
                        self.order.push(key);
                    }
                    self.loading_page = false;
                }
                Event::Thumbnail { page, rotation, width, height, rgba } => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let tex = ctx.load_texture(
                        format!("thumb{page}_{rotation}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.thumbnail = Some((page, rotation, tex));
                }
                Event::Exported(path) => {
                    self.status_msg = Some(format!("Сохранено: {}", path.display()));
                }
                Event::Error(e) => self.error = Some(e),
            }
        }
    }

    /// Запрашивает обзорную миниатюру текущей страницы/поворота, если её ещё нет.
    fn request_thumbnail(&mut self) {
        if !self.show_minimap || !self.opened {
            return;
        }
        let want = (self.page, self.rotation);
        let have = self.thumbnail.as_ref().map(|(p, r, _)| (*p, *r)) == Some(want);
        if have || self.thumb_requested == Some(want) {
            return;
        }
        self.thumb_requested = Some(want);
        let _ = self.handle.job_tx.send(Job::Thumbnail {
            page: self.page,
            rotation: self.rotation,
        });
    }

    fn fit_page(&mut self, canvas: egui::Rect) {
        let (pw, ph) = self.page_pt();
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }
        self.view.zoom = (canvas.width() / pw).min(canvas.height() / ph).max(0.01);
        self.center(canvas);
    }

    fn fit_width(&mut self, canvas: egui::Rect) {
        let (pw, _) = self.page_pt();
        if pw <= 0.0 {
            return;
        }
        self.view.zoom = (canvas.width() / pw).max(0.01);
        let shown_w = pw * self.view.zoom;
        self.view.offset.x = canvas.min.x + (canvas.width() - shown_w) * 0.5;
        self.view.offset.y = canvas.min.y; // выровнять по верху
    }

    fn actual_size(&mut self, canvas: egui::Rect) {
        self.view.zoom = 1.0;
        self.center(canvas);
    }

    fn center(&mut self, canvas: egui::Rect) {
        let (pw, ph) = self.page_pt();
        let shown = egui::vec2(pw, ph) * self.view.zoom;
        self.view.offset = canvas.min.to_vec2() + (canvas.size() - shown) * 0.5;
    }

    fn zoom_around(&mut self, factor: f32, pivot: egui::Vec2) {
        let new_zoom = (self.view.zoom * factor).clamp(0.02, 256.0);
        self.view.offset = pivot - (pivot - self.view.offset) * (new_zoom / self.view.zoom);
        self.view.zoom = new_zoom;
    }

    fn apply_action(&mut self, action: Action, canvas: egui::Rect) {
        match action {
            Action::FitPage => self.fit_page(canvas),
            Action::FitWidth => self.fit_width(canvas),
            Action::ActualSize => self.actual_size(canvas),
            Action::Zoom(f) => self.zoom_around(f, canvas.center().to_vec2()),
        }
    }

    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                self.pending = Some(Action::Zoom(ZOOM_STEP));
            }
            if i.key_pressed(egui::Key::Minus) {
                self.pending = Some(Action::Zoom(1.0 / ZOOM_STEP));
            }
            if i.key_pressed(egui::Key::Num0) {
                self.pending = Some(Action::ActualSize);
            }
            if i.key_pressed(egui::Key::F) {
                self.pending = Some(Action::FitPage);
            }
            if i.key_pressed(egui::Key::W) {
                self.pending = Some(Action::FitWidth);
            }
            if i.modifiers.command && i.key_pressed(egui::Key::O) {
                if let Some(p) = pick_pdf() {
                    self.open_path(p);
                }
            }
            if i.key_pressed(egui::Key::OpenBracket) {
                self.rotate(-1);
            }
            if i.key_pressed(egui::Key::CloseBracket) {
                self.rotate(1);
            }
        });
        let (prev, next) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::PageUp) || i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::PageDown) || i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if prev && self.page > 0 {
            self.set_page(self.page - 1);
        }
        if next {
            self.set_page(self.page + 1);
        }
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.into_iter().find_map(|f| f.path) {
            if path.extension().map(|e| e.eq_ignore_ascii_case("pdf")).unwrap_or(false) {
                self.open_path(path);
            }
        }
    }

    fn handle_pan_zoom(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.dragged() {
            self.view.offset += response.drag_delta();
        }
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            if let Some(cursor) = response.hover_pos() {
                self.zoom_around((scroll * 0.0015).exp(), cursor.to_vec2());
            }
        }
    }

    fn tile_screen_rect(&self, lod: i32, col: u32, row: u32, size: [usize; 2]) -> egui::Rect {
        let lod_scale = (lod as f32).exp2();
        let origin_pt =
            egui::vec2((col * TILE) as f32 / lod_scale, (row * TILE) as f32 / lod_scale);
        let size_pt = egui::vec2(size[0] as f32 / lod_scale, size[1] as f32 / lod_scale);
        let min = self.view.offset + origin_pt * self.view.zoom;
        egui::Rect::from_min_size(min.to_pos2(), size_pt * self.view.zoom)
    }

    fn request_visible(&mut self, lod: i32, lod_scale: f32, visible: &[(u32, u32)]) {
        let key = (self.page, self.rotation, lod, visible.to_vec());
        if self.last_request.as_ref() == Some(&key) {
            return;
        }
        self.last_request = Some(key);

        let missing: Vec<(u32, u32)> = visible
            .iter()
            .copied()
            .filter(|(c, r)| !self.textures.contains_key(&(self.page, self.rotation, lod, *c, *r)))
            .collect();
        if missing.is_empty() {
            return;
        }
        let _ = self.handle.job_tx.send(Job::Tiles {
            page: self.page,
            rotation: self.rotation,
            lod,
            lod_scale,
            page_pt: self.page_pt(),
            tiles: missing,
        });
    }

    fn draw_tiles(&self, painter: &egui::Painter, canvas: egui::Rect, current_lod: i32) {
        let mut keys: Vec<&TexKey> = self
            .textures
            .keys()
            .filter(|(p, rot, lod, _, _)| {
                *p == self.page && *rot == self.rotation && *lod <= current_lod
            })
            .collect();
        keys.sort_by_key(|(_, _, lod, _, _)| *lod); // грубые сначала, резкие поверх

        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        for &&(_, _, lod, col, row) in &keys {
            let tex = &self.textures[&(self.page, self.rotation, lod, col, row)];
            let rect = self.tile_screen_rect(lod, col, row, tex.size());
            if rect.intersects(canvas) {
                painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
            }
        }
    }

    fn evict(&mut self, visible: &[(u32, u32)], lod: i32) {
        let cur = self.page;
        let rot = self.rotation;
        let is_protected =
            |k: &TexKey| k.0 == cur && k.1 == rot && k.2 == lod && visible.contains(&(k.3, k.4));
        let mut i = 0;
        while self.order.len() > TEXTURE_CAP && i < self.order.len() {
            let key = self.order[i];
            if is_protected(&key) {
                i += 1;
                continue;
            }
            self.order.remove(i);
            self.textures.remove(&key);
        }
    }

    /// Рисует обзорную миникарту в правом верхнем углу + навигацию кликом.
    fn draw_minimap(&mut self, ui: &egui::Ui, painter: &egui::Painter, canvas: egui::Rect) {
        if !self.show_minimap {
            return;
        }
        let Some((tp, tr, tex)) = &self.thumbnail else { return };
        if *tp != self.page || *tr != self.rotation {
            return;
        }
        let (pw, ph) = self.page_pt();
        if pw <= 0.0 || ph <= 0.0 {
            return;
        }

        let max_side = 180.0_f32;
        let ts = tex.size();
        let aspect = ts[0] as f32 / ts[1].max(1) as f32;
        let (mw, mh) = if aspect >= 1.0 {
            (max_side, max_side / aspect)
        } else {
            (max_side * aspect, max_side)
        };
        let margin = 12.0;
        let mm = egui::Rect::from_min_size(
            egui::pos2(canvas.right() - margin - mw, canvas.top() + margin),
            egui::vec2(mw, mh),
        );

        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.rect_filled(mm.expand(3.0), 2.0, egui::Color32::from_black_alpha(150));
        painter.image(tex.id(), mm, uv, egui::Color32::WHITE);
        stroke_rect(painter, mm, egui::Color32::GRAY, 1.0);

        // Прямоугольник видимой области.
        let zoom = self.view.zoom;
        let to_mm = |x: f32, y: f32| {
            egui::pos2(
                mm.min.x + (x.clamp(0.0, pw) / pw) * mw,
                mm.min.y + (y.clamp(0.0, ph) / ph) * mh,
            )
        };
        let p0 = ((canvas.left() - self.view.offset.x) / zoom, (canvas.top() - self.view.offset.y) / zoom);
        let p1 = ((canvas.right() - self.view.offset.x) / zoom, (canvas.bottom() - self.view.offset.y) / zoom);
        let vr = egui::Rect::from_two_pos(to_mm(p0.0, p0.1), to_mm(p1.0, p1.1));
        stroke_rect(painter, vr, egui::Color32::from_rgb(80, 160, 255), 1.5);

        // Навигация: клик/перетаскивание по миникарте центрирует вид.
        let resp = ui.interact(mm, egui::Id::new("minimap"), egui::Sense::click_and_drag());
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.clicked() || resp.dragged() {
                let fx = ((pos.x - mm.min.x) / mw).clamp(0.0, 1.0) * pw;
                let fy = ((pos.y - mm.min.y) / mh).clamp(0.0, 1.0) * ph;
                self.view.offset = canvas.center().to_vec2() - egui::vec2(fx, fy) * zoom;
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📂 Открыть").clicked() {
                if let Some(p) = pick_pdf() {
                    self.open_path(p);
                }
            }
            ui.separator();
            if ui.button("Вписать").on_hover_text("F").clicked() {
                self.pending = Some(Action::FitPage);
            }
            if ui.button("Ширина").on_hover_text("W").clicked() {
                self.pending = Some(Action::FitWidth);
            }
            if ui.button("100%").on_hover_text("0").clicked() {
                self.pending = Some(Action::ActualSize);
            }
            if ui.button("−").clicked() {
                self.pending = Some(Action::Zoom(1.0 / ZOOM_STEP));
            }
            if ui.button("+").clicked() {
                self.pending = Some(Action::Zoom(ZOOM_STEP));
            }
            ui.separator();
            if ui.button("⟲").on_hover_text("Повернуть влево  [").clicked() {
                self.rotate(-1);
            }
            if ui.button("⟳").on_hover_text("Повернуть вправо  ]").clicked() {
                self.rotate(1);
            }
            ui.separator();
            if ui
                .add_enabled(self.opened, egui::Button::new("⤓ PNG"))
                .on_hover_text("Экспорт страницы в PNG")
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PNG", &["png"])
                    .set_file_name("page.png")
                    .save_file()
                {
                    self.status_msg = Some("Экспорт…".into());
                    let _ = self.handle.job_tx.send(Job::Export {
                        path,
                        page: self.page,
                        rotation: self.rotation,
                    });
                }
            }
            ui.checkbox(&mut self.show_minimap, "Обзор");

            if self.page_count() > 1 {
                ui.separator();
                if ui.button("◀").clicked() && self.page > 0 {
                    self.set_page(self.page - 1);
                }
                ui.label(format!("{}/{}", self.page + 1, self.page_count()));
                if ui.button("▶").clicked() {
                    self.set_page(self.page + 1);
                }
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.goto_text)
                        .desired_width(40.0)
                        .hint_text("№"),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    if let Ok(n) = self.goto_text.trim().parse::<usize>() {
                        if n >= 1 {
                            self.set_page(n - 1);
                        }
                    }
                    self.goto_text.clear();
                }
            }
        });
    }
}

/// Создаёт wgpu-устройство: сначала аппаратное (DX12/Vulkan), при отказе —
/// программный WARP (встроен в Windows, не требует драйверов GPU). Благодаря
/// этому приложение запускается и на машинах без видеодрайверов.
fn build_wgpu_setup() -> eframe::egui_wgpu::WgpuSetup {
    use eframe::wgpu;
    use std::sync::Arc;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::DX12 | wgpu::Backends::VULKAN | wgpu::Backends::GL,
        ..Default::default()
    });

    // PDFSMITH_FORCE_WARP=1 принудительно выбирает программный рендер (на случай
    // битых видеодрайверов).
    let force_warp = std::env::var("PDFSMITH_FORCE_WARP").is_ok();
    let adapter = pollster::block_on(async {
        // 1. Аппаратный адаптер (DX12/Vulkan).
        if !force_warp {
            if let Some(a) = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })
                .await
            {
                return Some(a);
            }
        }
        // 2. Запасной: перечисляем адаптеры и предпочитаем программный (WARP, тип
        //    Cpu) — он встроен в Windows и не требует видеодрайверов.
        instance
            .enumerate_adapters(wgpu::Backends::all())
            .into_iter()
            .min_by_key(|a| match a.get_info().device_type {
                wgpu::DeviceType::Cpu => 0u8,
                _ => 1u8,
            })
    })
    .expect("не найден ни один графический адаптер (DX12/Vulkan/WARP)");

    log::info!("wgpu адаптер: {:?}", adapter.get_info());

    let (device, queue) = pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("pdfsmith"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::default(),
        },
        None,
    ))
    .expect("не удалось создать графическое устройство");

    eframe::egui_wgpu::WgpuSetup::Existing {
        instance: Arc::new(instance),
        adapter: Arc::new(adapter),
        device: Arc::new(device),
        queue: Arc::new(queue),
    }
}

/// Открывает нативный диалог выбора PDF.
fn pick_pdf() -> Option<PathBuf> {
    rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file()
}

/// Рисует контур прямоугольника отрезками (стабильный API egui).
fn stroke_rect(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32, width: f32) {
    let s = egui::Stroke::new(width, color);
    let c = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    for i in 0..4 {
        painter.line_segment([c[i], c[(i + 1) % 4]], s);
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events(ctx);
        self.handle_dropped(ctx);
        self.handle_keyboard(ctx);
        self.request_thumbnail();

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| self.toolbar(ui));

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("Зум: {:.0}%", self.view.zoom * 100.0));
                if self.opened {
                    let (pw, ph) = self.page_pt();
                    ui.separator();
                    ui.label(format!("Стр. {}/{}", self.page + 1, self.page_count().max(1)));
                    ui.separator();
                    ui.label(format!("{:.0}×{:.0} pt", pw, ph));
                    if self.rotation != 0 {
                        ui.separator();
                        ui.label(format!("↻ {}°", self.rotation as u32 * 90));
                    }
                    if let Some((x, y)) = self.cursor_pt {
                        if x >= 0.0 && y >= 0.0 && x <= pw && y <= ph {
                            ui.separator();
                            ui.label(format!("X {x:.0}  Y {y:.0} pt"));
                        }
                    }
                }
                if self.loading_page {
                    ui.separator();
                    ui.spinner();
                    ui.label("Загрузка страницы…");
                } else if self.opened {
                    ui.separator();
                    ui.label(format!("Тайлов: {}", self.textures.len()));
                }
                if let Some(msg) = &self.status_msg {
                    ui.separator();
                    ui.label(msg);
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.error.is_some() {
                ui.centered_and_justified(|ui| {
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        format!("Ошибка: {}", self.error.as_deref().unwrap_or("")),
                    );
                });
                return;
            }
            if self.no_file {
                ui.centered_and_justified(|ui| {
                    ui.label("Откройте PDF (📂, Ctrl+O) или перетащите файл в окно.");
                });
                return;
            }
            if !self.opened {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                });
                return;
            }

            let (response, painter) =
                ui.allocate_painter(ui.available_size(), egui::Sense::click_and_drag());
            let canvas = response.rect;

            if self.view.needs_fit {
                self.fit_page(canvas);
                self.view.needs_fit = false;
            }
            if let Some(action) = self.pending.take() {
                self.apply_action(action, canvas);
            }
            self.handle_pan_zoom(ui, &response);

            let zoom = self.view.zoom;
            self.cursor_pt = response.hover_pos().map(|p| {
                let rel = p.to_vec2() - self.view.offset;
                (rel.x / zoom, rel.y / zoom)
            });
            let (lod, lod_scale) = lod_scale_for(zoom, MIN_LOD, MAX_LOD);
            let rel = self.view.offset - canvas.min.to_vec2();
            let visible: Vec<(u32, u32)> = visible_tiles_center_out(
                (rel.x, rel.y),
                zoom,
                (canvas.width(), canvas.height()),
                lod_scale,
                self.page_pt(),
                TILE,
            )
            .iter()
            .map(|t| (t.col, t.row))
            .collect();

            self.draw_tiles(&painter, canvas, lod);
            self.request_visible(lod, lod_scale, &visible);
            self.evict(&visible, lod);
            self.draw_minimap(ui, &painter, canvas);
        });
    }
}
