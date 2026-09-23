//! Окно «Настройки»: обновления и приложение по умолчанию.

use eframe::egui::{self, RichText};

use crate::default_app::DefaultApp;
use crate::settings::SettingsStore;
use crate::theme;
use crate::updates::{UpdState, UpdateUi, CURRENT_VERSION};

pub fn settings_window(ctx: &egui::Context, open: &mut bool, store: &mut SettingsStore, upd: &mut UpdateUi, def: &mut DefaultApp) {
    if !*open {
        return;
    }
    let mut still_open = true;
    egui::Window::new("Настройки")
        .open(&mut still_open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(RichText::new("Обновления").strong());
            let mut auto = store.data.auto_update;
            if ui
                .checkbox(&mut auto, "Автоматически устанавливать обновления")
                .on_hover_text("Скачивать в фоне и устанавливать при закрытии программы")
                .changed()
            {
                upd.set_auto_update(store, auto);
            }
            ui.label(RichText::new(format!("Текущая версия: {CURRENT_VERSION}")).color(theme::MUTED));
            if upd.enabled() {
                ui.horizontal(|ui| {
                    let downloading = matches!(upd.state, UpdState::Downloading { .. });
                    if ui.add_enabled(!upd.checking && !downloading, egui::Button::new("Проверить сейчас")).clicked() {
                        upd.check_now();
                    }
                    if upd.checking {
                        ui.spinner();
                    }
                });
            } else {
                ui.label(RichText::new("Обновления работают в установленной версии программы").color(theme::MUTED));
            }
            if let Some((msg, err)) = &upd.message {
                ui.label(RichText::new(msg).color(if *err { theme::DANGER } else { theme::OK }));
            }
            ui.separator();
            ui.label(RichText::new("Приложение по умолчанию").strong());
            def.settings_section(ui);
        });
    *open = still_open;
}
