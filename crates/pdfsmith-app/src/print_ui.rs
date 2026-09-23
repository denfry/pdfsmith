//! Окно «Печать»: принтер и его свойства, выбор страниц, копии, масштаб,
//! ориентация, бумага, двусторонняя и чёрно-белая печать, предпросмотр листа.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use egui_phosphor::regular as ph;

use crate::print::{
    self, default_devmode, list_printers, paper_info, place, printer_caps, properties_dialog, select_pages, DevMode,
    Duplex, Orientation, PageSet, Paper, PrintJob, PrinterCaps, Scaling, Subset,
};
use crate::theme;

/// Длинная сторона растра предпросмотра, px.
pub const PREVIEW_PX: f32 = 640.0;
const PREVIEW_BOX: Vec2 = Vec2::new(300.0, 392.0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum ScaleMode {
    Fit,
    Actual,
    Shrink,
    Custom,
}

/// Что окно просит у приложения.
pub enum PrintRequest {
    /// Растр страницы для предпросмотра.
    Preview { page: usize, annotations: bool },
    Print(PrintJob),
}

/// Сведения о документе для окна.
pub struct DocInfo<'a> {
    pub name: String,
    pub current: usize,
    /// Размеры страниц в пт (с учётом /Rotate).
    pub sizes: &'a [(f32, f32)],
    /// Идёт предыдущее задание печати.
    pub busy: bool,
}

struct Preview {
    page: usize,
    annotations: bool,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    tex: Option<(egui::TextureHandle, bool)>,
}

pub struct PrintDialog {
    pub open: bool,
    printers: Vec<String>,
    printer: String,
    devmode: Option<DevMode>,
    caps: PrinterCaps,
    paper: Option<Paper>,
    paper_id: i16,
    copies: u32,
    collate: bool,
    page_set: PageSet,
    range: String,
    subset: Subset,
    reverse: bool,
    scale_mode: ScaleMode,
    custom_pct: f32,
    orientation: Orientation,
    duplex: Duplex,
    grayscale: bool,
    annotations: bool,
    /// Номер листа в предпросмотре (индекс в списке печатаемых страниц).
    sheet: usize,
    preview: Option<Preview>,
    requested: Option<(usize, bool)>,
}

impl Default for PrintDialog {
    fn default() -> Self {
        PrintDialog {
            open: false,
            printers: Vec::new(),
            printer: String::new(),
            devmode: None,
            caps: PrinterCaps::default(),
            paper: None,
            paper_id: 0,
            copies: 1,
            collate: true,
            page_set: PageSet::All,
            range: String::new(),
            subset: Subset::All,
            reverse: false,
            scale_mode: ScaleMode::Fit,
            custom_pct: 100.0,
            orientation: Orientation::Auto,
            duplex: Duplex::Simplex,
            grayscale: false,
            annotations: true,
            sheet: 0,
            preview: None,
            requested: None,
        }
    }
}

impl PrintDialog {
    /// Открывает окно: перечитывает принтеры, выбирает прошлый или системный.
    pub fn show_for(&mut self, last_printer: Option<&str>, current: usize) {
        self.open = true;
        self.printers = list_printers();
        let pick = last_printer
            .filter(|p| self.printers.iter().any(|x| x == p))
            .map(str::to_string)
            .or_else(print::default_printer)
            .or_else(|| self.printers.first().cloned())
            .unwrap_or_default();
        self.select_printer(pick);
        self.sheet = if self.page_set == PageSet::Current { 0 } else { current };
        self.preview = None;
        self.requested = None;
    }

    /// Имя принтера, если печатать есть куда.
    pub fn printer(&self) -> Option<&str> {
        (!self.printer.is_empty()).then_some(self.printer.as_str())
    }

    fn select_printer(&mut self, name: String) {
        self.printer = name;
        self.devmode = if self.printer.is_empty() { None } else { default_devmode(&self.printer) };
        self.caps = if self.printer.is_empty() { PrinterCaps::default() } else { printer_caps(&self.printer, self.devmode.as_ref()) };
        // Бумагу переносим со старого принтера, если новый её знает.
        let keep = self.caps.papers.iter().any(|(id, _)| *id == self.paper_id);
        if !keep {
            self.paper_id = self.devmode.as_ref().map(|d| d.paper()).unwrap_or(0);
        }
        if !self.caps.duplex {
            self.duplex = Duplex::Simplex;
        }
        if !self.caps.color && !self.printer.is_empty() {
            self.grayscale = true;
        }
        self.apply_devmode();
    }

    /// Переносит выбор в окне в настройки драйвера и перечитывает лист.
    fn apply_devmode(&mut self) {
        if let Some(dm) = self.devmode.as_mut() {
            dm.set_orientation(if self.orientation == Orientation::Landscape { Orientation::Landscape } else { Orientation::Portrait });
            if self.paper_id != 0 {
                dm.set_paper(self.paper_id);
            }
            if self.caps.duplex {
                dm.set_duplex(self.duplex);
            }
            dm.set_grayscale(self.grayscale);
        }
        self.paper = if self.printer.is_empty() { None } else { paper_info(&self.printer, self.devmode.as_ref()) };
    }

    /// После «Свойств принтера»: выбор в окне следует за драйвером.
    fn sync_from_devmode(&mut self) {
        let Some(dm) = &self.devmode else { return };
        let o = dm.orientation();
        if !(self.orientation == Orientation::Auto && o == Orientation::Portrait) {
            self.orientation = o;
        }
        self.paper_id = dm.paper();
        if self.caps.duplex {
            self.duplex = dm.duplex();
        }
        self.grayscale = dm.grayscale() || !self.caps.color;
        self.paper = paper_info(&self.printer, self.devmode.as_ref());
    }

    fn scaling(&self) -> Scaling {
        match self.scale_mode {
            ScaleMode::Fit => Scaling::Fit,
            ScaleMode::Actual => Scaling::Actual,
            ScaleMode::Shrink => Scaling::Shrink,
            ScaleMode::Custom => Scaling::Custom(self.custom_pct),
        }
    }

    fn pages(&self, doc: &DocInfo) -> Result<Vec<usize>, String> {
        select_pages(self.page_set, doc.current, &self.range, self.subset, self.reverse, doc.sizes.len())
    }

    /// Растр предпросмотра пришёл из рендер-потока.
    pub fn set_preview(&mut self, page: usize, annotations: bool, width: u32, height: u32, rgba: Vec<u8>) {
        if self.requested == Some((page, annotations)) {
            self.preview = Some(Preview { page, annotations, width: width as usize, height: height as usize, rgba, tex: None });
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, doc: &DocInfo) -> Vec<PrintRequest> {
        let mut out = Vec::new();
        if !self.open {
            return out;
        }
        let pages = self.pages(doc);
        if let Ok(p) = &pages {
            self.sheet = self.sheet.min(p.len().saturating_sub(1));
            let want = (p[self.sheet], self.annotations);
            if self.requested != Some(want) {
                self.requested = Some(want);
                out.push(PrintRequest::Preview { page: want.0, annotations: want.1 });
            }
        }

        let mut open = true;
        let mut close = false;
        let mut print = false;
        egui::Window::new(format!("{}  Печать", ph::PRINTER))
            .id(egui::Id::new("print_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(360.0);
                        self.settings_ui(ui, doc, &pages);
                    });
                    ui.add_space(14.0);
                    ui.vertical(|ui| self.preview_ui(ui, doc, &pages));
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    match &pages {
                        Ok(p) => {
                            let total = p.len() * self.copies.max(1) as usize;
                            let mut s = format!("К печати: {total} стр.");
                            if self.copies > 1 {
                                s += &format!(" ({} × {} копии)", p.len(), self.copies);
                            }
                            if self.duplex != Duplex::Simplex {
                                s += &format!(" · листов: {}", self.copies.max(1) as usize * p.len().div_ceil(2));
                            }
                            ui.label(egui::RichText::new(s).color(theme::MUTED));
                        }
                        Err(e) => {
                            ui.label(egui::RichText::new(format!("{}  {e}", ph::WARNING)).color(theme::DANGER));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new("Отмена").min_size(Vec2::new(90.0, 28.0))).clicked() {
                            close = true;
                        }
                        let ready = pages.is_ok() && !self.printer.is_empty() && !doc.busy;
                        let btn = egui::Button::new(egui::RichText::new(format!("{}  Печать", ph::PRINTER)).color(if ready { Color32::BLACK } else { theme::MUTED }))
                            .fill(if ready { theme::ACCENT } else { theme::LINE })
                            .min_size(Vec2::new(110.0, 28.0));
                        let resp = ui.add_enabled(ready, btn);
                        let resp = if doc.busy { resp.on_disabled_hover_text("Дождитесь окончания текущей печати") } else { resp };
                        if resp.clicked() {
                            print = true;
                        }
                    });
                });
            });

        let typing = ctx.memory(|m| m.focused().is_some());
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }
        if !typing && ctx.input(|i| i.key_pressed(egui::Key::Enter)) && pages.is_ok() && !self.printer.is_empty() && !doc.busy {
            print = true;
        }
        if print {
            if let Ok(p) = pages {
                out.push(PrintRequest::Print(PrintJob {
                    printer: self.printer.clone(),
                    devmode: self.devmode.clone(),
                    doc_name: doc.name.clone(),
                    pages: p,
                    copies: self.copies.max(1),
                    collate: self.collate,
                    scaling: self.scaling(),
                    auto_rotate: self.orientation == Orientation::Auto,
                    grayscale: self.grayscale,
                    annotations: self.annotations,
                    output: None,
                }));
                close = true;
            }
        }
        if !open || close {
            self.open = false;
            self.preview = None;
            self.requested = None;
        }
        out
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, doc: &DocInfo, pages: &Result<Vec<usize>, String>) {
        let label = |ui: &mut egui::Ui, s: &str| {
            ui.label(egui::RichText::new(s).color(theme::MUTED));
        };
        let mut devmode_dirty = false;
        egui::Grid::new("print_grid").num_columns(2).spacing([14.0, 9.0]).min_col_width(96.0).show(ui, |ui| {
            // Принтер.
            label(ui, "Принтер");
            ui.horizontal(|ui| {
                if self.printers.is_empty() {
                    ui.label(egui::RichText::new("Принтеры не найдены").color(theme::DANGER));
                } else {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("printer")
                        .width(214.0)
                        .selected_text(elide(&self.printer, 30))
                        .show_ui(ui, |ui| {
                            for p in &self.printers {
                                if ui.selectable_label(*p == self.printer, p).clicked() {
                                    chosen = Some(p.clone());
                                }
                            }
                        });
                    if let Some(p) = chosen {
                        if p != self.printer {
                            self.select_printer(p);
                        }
                    }
                    let props = ui
                        .add_enabled(self.devmode.is_some(), egui::Button::new(ph::SLIDERS_HORIZONTAL).min_size(Vec2::new(30.0, 24.0)))
                        .on_hover_text("Свойства принтера: бумага, лоток, качество…");
                    if props.clicked() {
                        if let Some(dm) = self.devmode.as_mut() {
                            if properties_dialog(&self.printer, dm) {
                                self.sync_from_devmode();
                            }
                        }
                    }
                }
            });
            ui.end_row();
            if let Some(p) = &self.paper {
                ui.label("");
                let paper_name = self.caps.papers.iter().find(|(id, _)| *id == self.paper_id).map(|(_, n)| n.as_str());
                let mm = |pt: f32| pt / 72.0 * 25.4;
                let mut s = format!("{:.0}×{:.0} мм", mm(p.width), mm(p.height));
                if let Some(n) = paper_name {
                    s = format!("{n} · {s}");
                }
                s += &format!(" · {:.0} dpi", p.dpi_x.min(p.dpi_y));
                ui.label(egui::RichText::new(s).small().color(theme::MUTED));
                ui.end_row();
            }

            // Страницы.
            label(ui, "Страницы");
            ui.vertical(|ui| {
                let n = doc.sizes.len();
                ui.radio_value(&mut self.page_set, PageSet::All, format!("Все ({n})"));
                ui.radio_value(&mut self.page_set, PageSet::Current, format!("Текущая ({})", doc.current + 1));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.page_set, PageSet::Range, "Диапазон");
                    let r = ui.add(egui::TextEdit::singleline(&mut self.range).desired_width(132.0).hint_text("1-3, 5, 8-"));
                    if r.gained_focus() || r.changed() {
                        self.page_set = PageSet::Range;
                    }
                });
                if let (PageSet::Range, Err(e)) = (self.page_set, pages) {
                    ui.label(egui::RichText::new(e).small().color(theme::DANGER));
                }
            });
            ui.end_row();
            ui.label("");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("subset")
                    .width(150.0)
                    .selected_text(match self.subset {
                        Subset::All => "Все страницы",
                        Subset::Odd => "Только нечётные",
                        Subset::Even => "Только чётные",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.subset, Subset::All, "Все страницы");
                        ui.selectable_value(&mut self.subset, Subset::Odd, "Только нечётные");
                        ui.selectable_value(&mut self.subset, Subset::Even, "Только чётные");
                    });
                ui.checkbox(&mut self.reverse, "Обратный порядок");
            });
            ui.end_row();

            // Копии.
            label(ui, "Копии");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.copies).range(1..=999).speed(0.1));
                ui.add_enabled(self.copies > 1, egui::Checkbox::new(&mut self.collate, "Разобрать по копиям"))
                    .on_hover_text("Вкл: 1-2-3, 1-2-3. Выкл: 1-1, 2-2, 3-3");
            });
            ui.end_row();

            // Масштаб.
            label(ui, "Масштаб");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("scale")
                    .width(180.0)
                    .selected_text(match self.scale_mode {
                        ScaleMode::Fit => "По размеру бумаги",
                        ScaleMode::Actual => "Фактический размер",
                        ScaleMode::Shrink => "Уменьшать крупные",
                        ScaleMode::Custom => "Свой масштаб",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.scale_mode, ScaleMode::Fit, "По размеру бумаги");
                        ui.selectable_value(&mut self.scale_mode, ScaleMode::Actual, "Фактический размер");
                        ui.selectable_value(&mut self.scale_mode, ScaleMode::Shrink, "Уменьшать крупные");
                        ui.selectable_value(&mut self.scale_mode, ScaleMode::Custom, "Свой масштаб");
                    });
                if self.scale_mode == ScaleMode::Custom {
                    ui.add(egui::DragValue::new(&mut self.custom_pct).range(10.0..=400.0).speed(1.0).suffix(" %"));
                }
            });
            ui.end_row();

            // Ориентация.
            label(ui, "Ориентация");
            ui.horizontal(|ui| {
                for (o, s, tip) in [
                    (Orientation::Auto, "Авто", "Поворачивать страницы под лист"),
                    (Orientation::Portrait, "Книжная", ""),
                    (Orientation::Landscape, "Альбомная", ""),
                ] {
                    let r = ui.selectable_value(&mut self.orientation, o, s);
                    if !tip.is_empty() {
                        r.clone().on_hover_text(tip);
                    }
                    devmode_dirty |= r.changed();
                }
            });
            ui.end_row();

            // Бумага.
            if !self.caps.papers.is_empty() {
                label(ui, "Бумага");
                let cur = self.caps.papers.iter().find(|(id, _)| *id == self.paper_id).map(|(_, n)| n.clone()).unwrap_or_else(|| "—".into());
                egui::ComboBox::from_id_salt("paper").width(214.0).selected_text(elide(&cur, 30)).show_ui(ui, |ui| {
                    for (id, name) in &self.caps.papers {
                        if ui.selectable_label(*id == self.paper_id, name).clicked() && *id != self.paper_id {
                            self.paper_id = *id;
                            devmode_dirty = true;
                        }
                    }
                });
                ui.end_row();
            }

            // Двусторонняя.
            label(ui, "С двух сторон");
            ui.add_enabled_ui(self.caps.duplex, |ui| {
                let before = self.duplex;
                egui::ComboBox::from_id_salt("duplex")
                    .width(214.0)
                    .selected_text(match self.duplex {
                        Duplex::Simplex => "Нет",
                        Duplex::LongEdge => "Да, переплёт по длинному краю",
                        Duplex::ShortEdge => "Да, переплёт по короткому краю",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.duplex, Duplex::Simplex, "Нет");
                        ui.selectable_value(&mut self.duplex, Duplex::LongEdge, "Да, переплёт по длинному краю");
                        ui.selectable_value(&mut self.duplex, Duplex::ShortEdge, "Да, переплёт по короткому краю");
                    })
                    .response
                    .on_disabled_hover_text("Принтер не поддерживает двустороннюю печать");
                devmode_dirty |= before != self.duplex;
            });
            ui.end_row();

            // Цвет.
            label(ui, "Цвет");
            ui.horizontal(|ui| {
                let before = self.grayscale;
                ui.add_enabled_ui(self.caps.color || self.printer.is_empty(), |ui| {
                    ui.selectable_value(&mut self.grayscale, false, "Цветная").on_disabled_hover_text("Принтер печатает только в ч/б");
                });
                ui.selectable_value(&mut self.grayscale, true, "Чёрно-белая");
                devmode_dirty |= before != self.grayscale;
            });
            ui.end_row();

            ui.label("");
            ui.checkbox(&mut self.annotations, "Печатать пометки (рисунки, маркер, заметки)");
            ui.end_row();
        });
        if devmode_dirty {
            self.apply_devmode();
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, doc: &DocInfo, pages: &Result<Vec<usize>, String>) {
        let (area, _) = ui.allocate_exact_size(PREVIEW_BOX, egui::Sense::hover());
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 4.0, theme::BG);

        let paper = self.paper.unwrap_or_else(Paper::a4);
        let k = ((area.width() - 28.0) / paper.width).min((area.height() - 28.0) / paper.height);
        let sheet = Rect::from_center_size(area.center(), Vec2::new(paper.width, paper.height) * k);
        painter.rect_filled(sheet.translate(Vec2::new(0.0, 3.0)), 0.0, Color32::from_black_alpha(90));
        painter.rect_filled(sheet, 0.0, Color32::WHITE);
        let to_screen = |x: f32, y: f32| sheet.min + Vec2::new(x, y) * k;
        let a = paper.area;
        let printable = Rect::from_min_max(to_screen(a.x, a.y), to_screen(a.x + a.w, a.y + a.h));
        if a.x > 0.5 || a.y > 0.5 {
            dashed_rect(&painter, printable, Stroke::new(1.0, Color32::from_gray(200)));
        }

        let Ok(list) = pages else {
            painter.text(sheet.center(), egui::Align2::CENTER_CENTER, ph::WARNING, egui::FontId::proportional(28.0), theme::DANGER);
            return;
        };
        let page = list[self.sheet.min(list.len() - 1)];
        let size = doc.sizes.get(page).copied().unwrap_or((595.0, 842.0));
        let pl = place(size, &paper, self.scaling(), self.orientation == Orientation::Auto);
        let r = pl.rect;
        let page_rect = Rect::from_min_max(to_screen(r.x, r.y), to_screen(r.x + r.w, r.y + r.h));
        let clip = painter.with_clip_rect(printable.intersect(area));

        let gray = self.grayscale;
        let ready = self.preview.as_mut().filter(|p| p.page == page && p.annotations == self.annotations);
        match ready {
            Some(pv) => {
                if pv.tex.as_ref().map(|(_, g)| *g) != Some(gray) {
                    let mut rgba = pv.rgba.clone();
                    if gray {
                        for px in rgba.chunks_exact_mut(4) {
                            let y = ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8;
                            px[..3].fill(y);
                        }
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied([pv.width, pv.height], &rgba);
                    pv.tex = Some((ui.ctx().load_texture("print_preview", img, egui::TextureOptions::LINEAR), gray));
                }
                let (tex, _) = pv.tex.as_ref().unwrap();
                clip.add(rotated_image(tex.id(), page_rect, pl.rotation));
            }
            None => {
                clip.rect_filled(page_rect, 0.0, Color32::from_gray(236));
                clip.text(page_rect.center(), egui::Align2::CENTER_CENTER, ph::HOURGLASS, egui::FontId::proportional(22.0), Color32::from_gray(160));
            }
        }
        clip.rect_stroke(page_rect, 0.0, Stroke::new(1.0, Color32::from_gray(215)));

        // Листание.
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.set_width(PREVIEW_BOX.x);
            let n = list.len();
            ui.add_space((PREVIEW_BOX.x - 190.0).max(0.0) * 0.5);
            if ui.add_enabled(self.sheet > 0, egui::Button::new(ph::CARET_LEFT).min_size(Vec2::new(28.0, 24.0))).clicked() {
                self.sheet -= 1;
            }
            ui.add_sized(
                [120.0, 24.0],
                egui::Label::new(egui::RichText::new(format!("стр. {}  ·  {} из {n}", page + 1, self.sheet + 1)).color(theme::MUTED)),
            );
            if ui.add_enabled(self.sheet + 1 < n, egui::Button::new(ph::CARET_RIGHT).min_size(Vec2::new(28.0, 24.0))).clicked() {
                self.sheet += 1;
            }
        });
    }
}

/// Картинка в прямоугольнике с поворотом на `rotation` четвертей по часовой.
fn rotated_image(tex: egui::TextureId, r: Rect, rotation: u8) -> egui::Shape {
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
    let uv = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
    let mut mesh = egui::Mesh::with_texture(tex);
    for (i, pos) in corners.iter().enumerate() {
        // При повороте по часовой угол экрана i берёт угол картинки i − rotation.
        let src = (i + 4 - (rotation as usize & 3)) % 4;
        mesh.vertices.push(egui::epaint::Vertex { pos: *pos, uv: uv[src], color: Color32::WHITE });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    egui::Shape::mesh(mesh)
}

fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    for (a, b) in [
        (r.left_top(), r.right_top()),
        (r.right_top(), r.right_bottom()),
        (r.right_bottom(), r.left_bottom()),
        (r.left_bottom(), r.left_top()),
    ] {
        painter.extend(egui::Shape::dashed_line(&[a, b], stroke, 4.0, 3.0));
    }
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
