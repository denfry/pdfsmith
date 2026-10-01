//! Тема оформления: тёмная и светлая палитры вокруг белого документа,
//! системный шрифт Segoe UI (кириллица), иконки Phosphor.
//!
//! Цвета берутся функциями (`theme::text()` и т. п.): палитра переключается
//! на лету вслед за `ctx.theme()` — см. [`sync`].

use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Rounding, Stroke, Vec2};
use serde::{Deserialize, Serialize};

/// Выбор темы в настройках.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    /// Как в Windows (светлая или тёмная).
    #[default]
    System,
    Light,
    Dark,
}

impl ThemeChoice {
    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Как в системе",
            ThemeChoice::Light => "Светлая",
            ThemeChoice::Dark => "Тёмная",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        }
    }
}

/// Палитра одной темы.
struct Palette {
    /// Поля ввода, самый «глубокий» фон.
    bg: Color32,
    /// Панели и окна.
    panel: Color32,
    /// Подложка под страницами документа.
    canvas: Color32,
    line: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    /// Заливка выбранного/нажатого (под текстом цвета `accent`).
    accent_dim: Color32,
    /// Текст поверх заливки `accent`.
    on_accent: Color32,
    danger: Color32,
    ok: Color32,
    /// Фон кнопок и полей в покое / под курсором.
    widget: Color32,
    widget_hover: Color32,
    widget_hover_line: Color32,
    /// Лёгкая подсветка строки под курсором (миниатюры, списки).
    hover_wash: Color32,
    /// Тень страниц и окон.
    shadow: u8,
}

const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x16, 0x17, 0x1a),
    panel: Color32::from_rgb(0x21, 0x22, 0x26),
    canvas: Color32::from_rgb(0x2c, 0x2d, 0x32),
    line: Color32::from_rgb(0x36, 0x37, 0x3d),
    text: Color32::from_rgb(0xe8, 0xe8, 0xea),
    muted: Color32::from_rgb(0x9a, 0x9b, 0xa2),
    accent: Color32::from_rgb(0xf2, 0xb5, 0x44),
    accent_dim: Color32::from_rgb(0x55, 0x44, 0x22),
    on_accent: Color32::from_rgb(0x1a, 0x14, 0x08),
    danger: Color32::from_rgb(0xec, 0x70, 0x63),
    ok: Color32::from_rgb(0x82, 0xcc, 0x8d),
    widget: Color32::from_rgb(0x2d, 0x2e, 0x33),
    widget_hover: Color32::from_rgb(0x38, 0x39, 0x40),
    widget_hover_line: Color32::from_rgb(0x48, 0x49, 0x51),
    hover_wash: Color32::from_rgba_premultiplied(10, 10, 10, 10),
    shadow: 110,
};

const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xff, 0xff, 0xff),
    panel: Color32::from_rgb(0xf6, 0xf6, 0xf7),
    canvas: Color32::from_rgb(0xe3, 0xe4, 0xe8),
    line: Color32::from_rgb(0xda, 0xdb, 0xe0),
    text: Color32::from_rgb(0x1d, 0x1e, 0x22),
    muted: Color32::from_rgb(0x66, 0x68, 0x70),
    accent: Color32::from_rgb(0xa8, 0x62, 0x00),
    accent_dim: Color32::from_rgb(0xfb, 0xe9, 0xc6),
    on_accent: Color32::WHITE,
    danger: Color32::from_rgb(0xc0, 0x39, 0x2b),
    ok: Color32::from_rgb(0x2a, 0x7f, 0x3c),
    widget: Color32::from_rgb(0xff, 0xff, 0xff),
    widget_hover: Color32::from_rgb(0xea, 0xeb, 0xee),
    widget_hover_line: Color32::from_rgb(0xc4, 0xc6, 0xcc),
    hover_wash: Color32::from_rgba_premultiplied(0, 0, 0, 12),
    shadow: 45,
};

static IS_DARK: AtomicBool = AtomicBool::new(true);

fn pal() -> &'static Palette {
    if IS_DARK.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn bg() -> Color32 { pal().bg }
pub fn panel() -> Color32 { pal().panel }
pub fn canvas() -> Color32 { pal().canvas }
pub fn line() -> Color32 { pal().line }
pub fn text() -> Color32 { pal().text }
pub fn muted() -> Color32 { pal().muted }
pub fn accent() -> Color32 { pal().accent }
pub fn accent_dim() -> Color32 { pal().accent_dim }
pub fn on_accent() -> Color32 { pal().on_accent }
pub fn danger() -> Color32 { pal().danger }
pub fn ok() -> Color32 { pal().ok }
pub fn hover_wash() -> Color32 { pal().hover_wash }
/// Полупрозрачная акцентная подложка выбранной строки/миниатюры.
pub fn accent_wash() -> Color32 {
    let a = pal().accent;
    Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), if is_dark() { 26 } else { 34 })
}
/// Тень под страницей и миниатюрами.
pub fn shadow() -> Color32 { Color32::from_black_alpha(pal().shadow) }

pub fn is_dark() -> bool {
    IS_DARK.load(Ordering::Relaxed)
}

/// Размер иконок в панели инструментов.
pub const ICON: f32 = 17.0;

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    if let Some(data) = system_font(&["segoeui.ttf", "arial.ttf", "tahoma.ttf"]) {
        fonts.font_data.insert("ui".into(), FontData::from_owned(data).into());
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".into());
    }
    if let Some(data) = system_font(&["consola.ttf", "cour.ttf"]) {
        fonts.font_data.insert("mono".into(), FontData::from_owned(data).into());
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
    ctx.set_style_of(egui::Theme::Dark, style(ctx, &DARK, true));
    ctx.set_style_of(egui::Theme::Light, style(ctx, &LIGHT, false));
    set_choice(ctx, ThemeChoice::default());
}

/// Применить выбор темы из настроек.
pub fn set_choice(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_theme(choice.preference());
    sync(ctx);
}

/// Палитра вслед за активной темой egui (в т. ч. при смене темы Windows).
/// Вызывается в начале каждого кадра.
pub fn sync(ctx: &egui::Context) {
    IS_DARK.store(ctx.theme() == egui::Theme::Dark, Ordering::Relaxed);
}

fn style(ctx: &egui::Context, p: &Palette, dark: bool) -> egui::Style {
    let mut style = (*ctx.style()).clone();
    use egui::{FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(11.5)),
        (TextStyle::Body, FontId::proportional(13.5)),
        (TextStyle::Button, FontId::proportional(13.5)),
        (TextStyle::Heading, FontId::proportional(17.0)),
        (TextStyle::Monospace, FontId::monospace(12.5)),
    ]
    .into();
    style.spacing.item_spacing = Vec2::new(6.0, 6.0);
    style.spacing.button_padding = Vec2::new(10.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(16.0);
    style.spacing.menu_margin = egui::Margin::same(5.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.tooltip_width = 320.0;

    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.override_text_color = Some(p.text);
    v.panel_fill = p.panel;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_rounding = Rounding::same(8.0);
    v.window_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 8.0), blur: 28.0, spread: 0.0, color: Color32::from_black_alpha(p.shadow) };
    v.popup_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 4.0), blur: 14.0, spread: 0.0, color: Color32::from_black_alpha((p.shadow as u16 * 3 / 4) as u8) };
    v.menu_rounding = Rounding::same(6.0);
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.widget;
    v.code_bg_color = p.bg;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.accent;
    v.error_fg_color = p.danger;
    v.selection.bg_fill = p.accent_dim;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.widgets.noninteractive.bg_fill = p.panel;
    v.widgets.noninteractive.weak_bg_fill = p.panel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.muted);
    v.widgets.inactive.bg_fill = p.widget;
    v.widgets.inactive.weak_bg_fill = p.widget;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_fill = p.widget_hover;
    v.widgets.hovered.weak_bg_fill = p.widget_hover;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.widget_hover_line);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.active.bg_fill = p.accent_dim;
    v.widgets.active.weak_bg_fill = p.accent_dim;
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.open.bg_fill = p.widget_hover;
    v.widgets.open.weak_bg_fill = p.widget_hover;
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.widget_hover_line);
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = Rounding::same(5.0);
        w.expansion = 0.0;
    }
    style.visuals = v;
    style
}

fn system_font(names: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|_| r"C:\Windows".into());
    names.iter().find_map(|n| std::fs::read(dir.join("Fonts").join(n)).ok())
}

/// Кнопка-иконка панели инструментов; `active` — выбранный инструмент.
/// Без рамки в покое, с мягкой подложкой под курсором.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str, active: bool) -> egui::Response {
    let text = egui::RichText::new(icon).size(ICON).color(if active { accent() } else { text() });
    let mut btn = egui::Button::new(text).min_size(Vec2::new(30.0, 28.0));
    if active {
        btn = btn.fill(accent_dim()).stroke(Stroke::NONE);
    }
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for s in [&mut w.inactive, &mut w.noninteractive] {
            s.weak_bg_fill = Color32::TRANSPARENT;
            s.bg_stroke = Stroke::NONE;
        }
        w.hovered.bg_stroke = Stroke::NONE;
        w.active.bg_stroke = Stroke::NONE;
        ui.add(btn)
    })
    .inner
    .on_hover_text(tip)
}

/// Основная (акцентная) кнопка диалога.
pub fn primary_button(text: impl Into<String>, enabled: bool) -> egui::Button<'static> {
    let label = egui::RichText::new(text.into()).color(if enabled { on_accent() } else { muted() });
    egui::Button::new(label).fill(if enabled { accent() } else { line() }).stroke(Stroke::NONE)
}

/// Тонкая вертикальная разделительная черта для панели инструментов.
pub fn vsep(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(11.0, 22.0), egui::Sense::hover());
    let x = rect.center().x;
    ui.painter().line_segment([egui::pos2(x, rect.top() + 3.0), egui::pos2(x, rect.bottom() - 3.0)], Stroke::new(1.0, line()));
}

/// Рамка информационной плашки под панелью инструментов.
pub fn banner_frame() -> egui::Frame {
    egui::Frame::none().fill(accent_dim()).inner_margin(egui::Margin::symmetric(10.0, 5.0))
}
