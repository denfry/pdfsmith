//! Окно «Подписать»: выбор сертификата (КриптоПро, токены — через хранилище
//! Windows «Личное»), причина и место подписи.

use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use pdfsmith_sign::{store, CertInfo};

use crate::render_thread::DispRect;
use crate::theme;

/// Что выбрал пользователь; путь к файлу спрашивает приложение.
pub struct SignChoice {
    pub page: usize,
    pub rotation: u8,
    pub rect: DispRect,
    pub cert: CertInfo,
    pub reason: String,
    pub location: String,
}

pub struct SignDialog {
    /// Куда ставится штамп: страница, поворот просмотра, прямоугольник.
    target: Option<(usize, u8, DispRect)>,
    certs: Result<Vec<CertInfo>, String>,
    selected: Option<usize>,
    reason: String,
    location: String,
}

impl Default for SignDialog {
    fn default() -> Self {
        SignDialog { target: None, certs: Ok(Vec::new()), selected: None, reason: String::new(), location: String::new() }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

impl SignDialog {
    pub fn is_open(&self) -> bool {
        self.target.is_some()
    }

    /// Прямоугольник штампа, пока окно открыто (для подсветки на странице).
    pub fn target(&self) -> Option<(usize, u8, DispRect)> {
        self.target
    }

    /// Открывает окно; `last_cert` — отпечаток прошлого выбора.
    pub fn show_for(&mut self, page: usize, rotation: u8, rect: DispRect, last_cert: Option<&str>) {
        self.target = Some((page, rotation, rect));
        self.reload(last_cert);
    }

    fn reload(&mut self, last_cert: Option<&str>) {
        self.certs = store::list_certificates();
        let certs = self.certs.as_deref().unwrap_or(&[]);
        self.selected = certs
            .iter()
            .position(|c| !c.expired && last_cert == Some(hex(&c.thumbprint).as_str()))
            .or_else(|| certs.iter().position(|c| !c.expired));
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<SignChoice> {
        let (page, rotation, rect) = self.target?;
        let mut keep = true;
        let mut choice = None;
        egui::Window::new("Подписать электронной подписью")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(RichText::new(format!("Штамп на странице {}", page + 1)).color(theme::muted()).small());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Сертификат").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(format!("{}  Обновить", ph::ARROWS_CLOCKWISE)).clicked() {
                            let last = self.selected_cert().map(|c| hex(&c.thumbprint));
                            self.reload(last.as_deref());
                        }
                    });
                });
                match &self.certs {
                    Err(e) => {
                        ui.label(RichText::new(format!("{}  {e}", ph::WARNING)).color(theme::danger()));
                    }
                    Ok(certs) if certs.is_empty() => {
                        ui.label(
                            RichText::new(
                                "В хранилище «Личное» нет сертификатов с закрытым ключом. Вставьте токен и \
                                 установите сертификат в КриптоПро CSP: «Сервис → Просмотреть сертификаты \
                                 в контейнере → Установить», затем нажмите «Обновить».",
                            )
                            .color(theme::muted()),
                        );
                    }
                    Ok(certs) => {
                        egui::ScrollArea::vertical().max_height(240.0).show(ui, |ui| {
                            for (i, c) in certs.iter().enumerate() {
                                let mut text = egui::text::LayoutJob::default();
                                let strong = egui::TextFormat { color: if c.expired { theme::muted() } else { theme::text() }, ..Default::default() };
                                text.append(&c.owner, 0.0, strong);
                                let mut details = Vec::new();
                                if !c.organization.is_empty() && c.organization != c.owner {
                                    details.push(c.organization.clone());
                                }
                                details.push(format!("до {}", c.valid_to));
                                if !c.issuer.is_empty() {
                                    details.push(format!("выдан: {}", c.issuer));
                                }
                                let small = egui::TextFormat {
                                    color: theme::muted(),
                                    font_id: egui::FontId::proportional(11.5),
                                    ..Default::default()
                                };
                                text.append(&format!("\n{}", details.join(" · ")), 0.0, small);
                                if c.expired {
                                    let warn = egui::TextFormat {
                                        color: theme::danger(),
                                        font_id: egui::FontId::proportional(11.5),
                                        ..Default::default()
                                    };
                                    text.append("  срок действия истёк", 0.0, warn);
                                }
                                let r = ui.add_enabled(
                                    !c.expired,
                                    egui::SelectableLabel::new(self.selected == Some(i), text),
                                );
                                if r.clicked() {
                                    self.selected = Some(i);
                                }
                                ui.add_space(2.0);
                            }
                        });
                    }
                }
                ui.add_space(8.0);
                egui::Grid::new("sign_fields").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Причина");
                    ui.add(egui::TextEdit::singleline(&mut self.reason).desired_width(340.0).hint_text("необязательно, например «Согласовано»"));
                    ui.end_row();
                    ui.label("Место");
                    ui.add(egui::TextEdit::singleline(&mut self.location).desired_width(340.0).hint_text("необязательно"));
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Подписанная копия сохранится отдельным файлом. PIN-код токена спросит КриптоПро.")
                        .color(theme::muted())
                        .small(),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ready = self.selected_cert().is_some();
                    if ui.add_enabled(ready, theme::primary_button(format!("{}  Подписать…", ph::SEAL_CHECK), ready)).clicked() {
                        if let Some(cert) = self.selected_cert().cloned() {
                            choice = Some(SignChoice {
                                page,
                                rotation,
                                rect,
                                cert,
                                reason: self.reason.trim().to_string(),
                                location: self.location.trim().to_string(),
                            });
                        }
                    }
                    if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        keep = false;
                    }
                });
            });
        if choice.is_some() || !keep {
            self.target = None;
        }
        choice
    }

    fn selected_cert(&self) -> Option<&CertInfo> {
        let certs = self.certs.as_ref().ok()?;
        self.selected.and_then(|i| certs.get(i)).filter(|c| !c.expired)
    }
}
