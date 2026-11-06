//! Стартовый экран, пока документ не открыт: открыть файл, быстрые действия,
//! недавние файлы.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Sense, Vec2};
use egui_phosphor::regular as ph;

use crate::convert_ui::ConvertMode;
use crate::settings::RecentFile;
use crate::theme;

pub enum HomeAction {
    OpenDialog,
    Open(PathBuf),
    Forget(PathBuf),
    Convert(ConvertMode),
}

const COL_W: f32 = 470.0;
const RECENT_SHOWN: usize = 8;

/// Однострочный текст, обрезанный многоточием по ширине.
pub fn elided(
    ui: &egui::Ui,
    text: String,
    font: FontId,
    color: Color32,
    max_w: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping {
        max_width: max_w,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|f| f.layout_job(job))
}

pub fn home(ui: &mut egui::Ui, recent: &[RecentFile]) -> Option<HomeAction> {
    let mut action = None;
    let shown = recent.len().min(RECENT_SHOWN);
    let content_h = 410.0
        + if shown > 0 {
            40.0 + shown as f32 * 46.0
        } else {
            0.0
        };
    let avail = ui.available_size();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(((avail.y - content_h) * 0.42).max(24.0));
            ui.horizontal(|ui| {
                ui.add_space(((avail.x - COL_W) * 0.5).max(16.0));
                ui.vertical(|ui| {
                    ui.set_width(COL_W);
                    header(ui, &mut action);
                    ui.add_space(22.0);
                    quick_actions(ui, &mut action);
                    if shown > 0 {
                        ui.add_space(26.0);
                        theme::section_label(ui, "НЕДАВНИЕ ФАЙЛЫ");
                        ui.add_space(2.0);
                        for r in &recent[..shown] {
                            recent_row(ui, r, &mut action);
                        }
                    }
                    ui.add_space(24.0);
                });
            });
        });
    action
}

fn header(ui: &mut egui::Ui, action: &mut Option<HomeAction>) {
    ui.vertical_centered(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
        ui.painter().rect_filled(rect, 18.0, theme::accent_dim());
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            ph::FILE_PDF,
            FontId::proportional(34.0),
            theme::accent(),
        );
        ui.add_space(10.0);
        ui.label(RichText::new("PDFsmith").size(24.0).strong());
        ui.label(
            RichText::new("Просмотр, пометки, подпись PDF и конвертация в Word, Excel, PowerPoint")
                .color(theme::muted()),
        );
        ui.add_space(20.0);
        let btn = theme::primary_button(format!("{}   Открыть файл…", ph::FOLDER_OPEN), true)
            .min_size(Vec2::new(230.0, 40.0))
            .rounding(8.0);
        if ui.add(btn).clicked() {
            *action = Some(HomeAction::OpenDialog);
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("PDF, Word, Excel, PowerPoint  ·  Ctrl+O или перетащите файл в окно")
                .small()
                .color(theme::muted()),
        );
    });
}

fn quick_actions(ui: &mut egui::Ui, action: &mut Option<HomeAction>) {
    let rows = [
        [
            ConvertMode::Merge,
            ConvertMode::OfficeToPdf,
            ConvertMode::ImagesToPdf,
        ],
        [
            ConvertMode::PdfToWord,
            ConvertMode::PdfToExcel,
            ConvertMode::Split,
        ],
    ];
    let gap = 10.0;
    let w = (COL_W - gap * 2.0) / 3.0;
    ui.spacing_mut().item_spacing.y = gap;
    for items in rows {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for m in items {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 68.0), Sense::click());
                let p = ui.painter();
                p.rect_filled(
                    rect,
                    10.0,
                    if resp.hovered() {
                        theme::widget_hover()
                    } else {
                        theme::group()
                    },
                );
                p.text(
                    rect.center_top() + Vec2::new(0.0, 24.0),
                    egui::Align2::CENTER_CENTER,
                    m.icon(),
                    FontId::proportional(20.0),
                    theme::accent(),
                );
                p.text(
                    rect.center_bottom() - Vec2::new(0.0, 18.0),
                    egui::Align2::CENTER_CENTER,
                    m.label(),
                    FontId::proportional(13.0),
                    theme::text(),
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    *action = Some(HomeAction::Convert(m));
                }
            }
        });
    }
}

fn recent_row(ui: &mut egui::Ui, r: &RecentFile, action: &mut Option<HomeAction>) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(COL_W, 44.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let exists = r.path.exists();
    let x_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 18.0, rect.center().y),
        Vec2::splat(24.0),
    );
    let x_resp = ui.interact(x_rect, ui.id().with(("forget", &r.path)), Sense::click());
    let hovered = resp.hovered() || x_resp.hovered();
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, 8.0, theme::hover_wash());
    }
    let dim = if exists {
        theme::text()
    } else {
        theme::muted()
    };
    let icon = match crate::office::app_for(&r.path) {
        Some(crate::office::OfficeApp::Word) => ph::MICROSOFT_WORD_LOGO,
        Some(crate::office::OfficeApp::Excel) => ph::MICROSOFT_EXCEL_LOGO,
        Some(crate::office::OfficeApp::PowerPoint) => ph::MICROSOFT_POWERPOINT_LOGO,
        None => ph::FILE_PDF,
    };
    p.text(
        rect.left_center() + Vec2::new(18.0, 0.0),
        egui::Align2::CENTER_CENTER,
        icon,
        FontId::proportional(20.0),
        if exists {
            theme::accent()
        } else {
            theme::muted()
        },
    );
    let name = r
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let dir = r
        .path
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    let text_w = rect.width() - 36.0 - 90.0;
    let g = elided(ui, name, FontId::proportional(13.5), dim, text_w);
    p.galley(Pos2::new(rect.left() + 36.0, rect.top() + 5.0), g, dim);
    let sub = if exists {
        dir
    } else {
        "файл не найден — перемещён или удалён".into()
    };
    let g = elided(ui, sub, FontId::proportional(11.5), theme::muted(), text_w);
    p.galley(
        Pos2::new(rect.left() + 36.0, rect.top() + 24.0),
        g,
        theme::muted(),
    );
    if hovered {
        let c = if x_resp.hovered() {
            theme::text()
        } else {
            theme::muted()
        };
        if x_resp.hovered() {
            p.rect_filled(x_rect, 6.0, theme::widget_hover());
        }
        p.text(
            x_rect.center(),
            egui::Align2::CENTER_CENTER,
            ph::X,
            FontId::proportional(13.0),
            c,
        );
    } else if r.page > 0 && exists {
        p.text(
            rect.right_center() - Vec2::new(12.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            format!("стр. {}", r.page + 1),
            FontId::proportional(11.5),
            theme::muted(),
        );
    }
    if x_resp.on_hover_text("Убрать из списка").clicked() {
        *action = Some(HomeAction::Forget(r.path.clone()));
    } else if resp.on_hover_text(r.path.display().to_string()).clicked() {
        *action = Some(HomeAction::Open(r.path.clone()));
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}
