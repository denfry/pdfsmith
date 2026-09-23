//! Тема оформления: нейтральный тёмный интерфейс вокруг светлого документа,
//! системный шрифт Segoe UI (кириллица), иконки Phosphor.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, Rounding, Stroke, Vec2};

pub const BG: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x20);
pub const PANEL: Color32 = Color32::from_rgb(0x25, 0x26, 0x2a);
pub const CANVAS: Color32 = Color32::from_rgb(0x30, 0x31, 0x35);
pub const LINE: Color32 = Color32::from_rgb(0x3a, 0x3b, 0x40);
pub const TEXT: Color32 = Color32::from_rgb(0xe4, 0xe4, 0xe6);
pub const MUTED: Color32 = Color32::from_rgb(0x96, 0x97, 0x9d);
pub const ACCENT: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x3c);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(0x5a, 0x48, 0x22);
pub const DANGER: Color32 = Color32::from_rgb(0xe5, 0x6a, 0x5c);
pub const OK: Color32 = Color32::from_rgb(0x7f, 0xc8, 0x8a);

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
    // Тема фиксированная: иначе eframe подхватит светлую системную тему и
    // часть виджетов (поля ввода, списки) окажется светлой.
    ctx.set_theme(egui::ThemePreference::Dark);

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
    style.spacing.item_spacing = Vec2::new(6.0, 5.0);
    style.spacing.button_padding = Vec2::new(8.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(14.0);
    style.spacing.menu_margin = egui::Margin::same(6.0);
    style.spacing.interact_size.y = 24.0;
    style.spacing.tooltip_width = 320.0;

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = Some(TEXT);
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.window_rounding = Rounding::same(4.0);
    v.window_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 6.0), blur: 20.0, spread: 0.0, color: Color32::from_black_alpha(120) };
    v.popup_shadow = egui::epaint::Shadow { offset: Vec2::new(0.0, 4.0), blur: 12.0, spread: 0.0, color: Color32::from_black_alpha(100) };
    v.menu_rounding = Rounding::same(4.0);
    v.extreme_bg_color = BG;
    v.faint_bg_color = Color32::from_rgb(0x2b, 0x2c, 0x30);
    v.code_bg_color = BG;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = ACCENT;
    v.error_fg_color = DANGER;
    v.selection.bg_fill = ACCENT_DIM;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.widgets.noninteractive.bg_fill = PANEL;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
    v.widgets.inactive.bg_fill = Color32::from_rgb(0x2f, 0x30, 0x35);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x2f, 0x30, 0x35);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x3b, 0x3c, 0x42);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x3b, 0x3c, 0x42);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x4a, 0x4b, 0x52));
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.bg_fill = ACCENT_DIM;
    v.widgets.active.weak_bg_fill = ACCENT_DIM;
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.open.bg_fill = Color32::from_rgb(0x3b, 0x3c, 0x42);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(0x3b, 0x3c, 0x42);
    for w in [&mut v.widgets.noninteractive, &mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.rounding = Rounding::same(3.0);
        w.expansion = 0.0;
    }
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

fn system_font(names: &[&str]) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|_| r"C:\Windows".into());
    names.iter().find_map(|n| std::fs::read(dir.join("Fonts").join(n)).ok())
}

/// Кнопка-иконка панели инструментов; `active` — выбранный инструмент.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, tip: &str, active: bool) -> egui::Response {
    let text = egui::RichText::new(icon).size(ICON).color(if active { ACCENT } else { TEXT });
    let mut btn = egui::Button::new(text).frame(active).min_size(Vec2::new(30.0, 26.0));
    if active {
        btn = btn.fill(ACCENT_DIM).stroke(Stroke::new(1.0, ACCENT));
    }
    ui.add(btn).on_hover_text(tip)
}

/// Тонкая вертикальная разделительная черта для панели инструментов.
pub fn vsep(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(9.0, 22.0), egui::Sense::hover());
    let x = rect.center().x;
    ui.painter().line_segment([egui::pos2(x, rect.top() + 2.0), egui::pos2(x, rect.bottom() - 2.0)], Stroke::new(1.0, LINE));
}
