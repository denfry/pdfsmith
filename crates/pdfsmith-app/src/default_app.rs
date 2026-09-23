//! Приложение по умолчанию для `.pdf`: проверка через реестр HKCU и
//! системный диалог выбора (назначить программно Windows не позволяет).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;

use crate::settings::SettingsStore;
use crate::theme::{self, icon_button};

pub const PROG_ID: &str = "PDFsmith.Document";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultStatus {
    /// Установлено установщиком (есть ProgID в HKCU).
    pub installed: bool,
    pub is_default: bool,
}

pub fn should_prompt(status: DefaultStatus, ask: bool) -> bool {
    ask && status.installed && !status.is_default
}

#[cfg(windows)]
pub fn query() -> DefaultStatus {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let installed = hkcu.open_subkey(format!(r"Software\Classes\{PROG_ID}")).is_ok();
    let base = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.pdf";
    let prog = |key: &str| {
        hkcu.open_subkey(format!(r"{base}\{key}")).and_then(|k| k.get_value::<String, _>("ProgId")).ok()
    };
    // Новые сборки Windows 11 пишут выбор в UserChoiceLatest, он главнее.
    let current = prog("UserChoiceLatest").or_else(|| prog("UserChoice"));
    let is_default = current.is_some_and(|p| p.eq_ignore_ascii_case(PROG_ID));
    DefaultStatus { installed, is_default }
}

#[cfg(not(windows))]
pub fn query() -> DefaultStatus {
    DefaultStatus { installed: false, is_default: false }
}

/// HRESULT_FROM_WIN32(ERROR_CANCELLED), как i32 — пользователь закрыл диалог
/// выбора кнопкой «Отмена» либо крестиком. Это не ошибка: фолбэк на
/// ms-settings не нужен.
#[cfg(windows)]
const HRESULT_ERROR_CANCELLED: i32 = 0x800704C7u32 as i32;

/// `true`, если `SHOpenWithDialog` можно считать успешным: диалог реально
/// открылся и либо завершился штатно, либо пользователь его отменил.
#[cfg(windows)]
fn picker_succeeded(hresult: i32) -> bool {
    hresult >= 0 || hresult == HRESULT_ERROR_CANCELLED
}

/// Системный диалог «Чем открывать .pdf». `false` — диалог не открылся.
#[cfg(windows)]
fn show_picker() -> bool {
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{
        SHOpenWithDialog, OAIF_ALLOW_REGISTRATION, OAIF_FORCE_REGISTRATION, OAIF_REGISTER_EXT, OPENASINFO,
    };

    let class: Vec<u16> = ".pdf\0".encode_utf16().collect();
    let file: Vec<u16> = "\0".encode_utf16().collect();
    let info = OPENASINFO {
        pcszFile: file.as_ptr(),
        pcszClass: class.as_ptr(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_FORCE_REGISTRATION,
    };
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        picker_succeeded(SHOpenWithDialog(std::ptr::null_mut(), &info))
    }
}

#[cfg(not(windows))]
fn show_picker() -> bool {
    false
}

fn open_settings_page() {
    let _ = std::process::Command::new("explorer.exe").arg("ms-settings:defaultapps").spawn();
}

pub struct DefaultApp {
    pub status: DefaultStatus,
    picker_done: Option<Arc<AtomicBool>>,
    hidden: bool,
}

impl DefaultApp {
    /// Не установлено → никаких предложений (тесты, портативный запуск).
    pub fn disabled() -> Self {
        Self::with_status(DefaultStatus { installed: false, is_default: false })
    }

    pub fn detect() -> Self {
        Self::with_status(query())
    }

    pub fn with_status(status: DefaultStatus) -> Self {
        DefaultApp { status, picker_done: None, hidden: false }
    }

    /// После закрытия системного диалога перечитываем реестр.
    pub fn poll(&mut self) {
        if self.picker_done.as_ref().is_some_and(|d| d.load(Ordering::Acquire)) {
            self.picker_done = None;
            self.status = query();
        }
    }

    pub fn picker_open(&self) -> bool {
        self.picker_done.is_some()
    }

    /// Диалог модальный — открываем в отдельном потоке, чтобы окно не замирало.
    pub fn open_picker(&mut self, ctx: &egui::Context) {
        if self.picker_open() {
            return;
        }
        let done = Arc::new(AtomicBool::new(false));
        self.picker_done = Some(done.clone());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if !show_picker() {
                open_settings_page();
            }
            done.store(true, Ordering::Release);
            ctx.request_repaint();
        });
    }

    pub fn dont_ask(&mut self, store: &mut SettingsStore) {
        store.data.ask_default_app = false;
        store.save();
    }

    pub fn banner_visible(&self, ask: bool) -> bool {
        !self.hidden && should_prompt(self.status, ask)
    }

    pub fn banner(&mut self, ui: &mut egui::Ui, store: &mut SettingsStore) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(ph::FILE_PDF).size(theme::ICON).color(theme::ACCENT));
            ui.label("Сделать PDFsmith программой для PDF по умолчанию?");
            if ui.add_enabled(!self.picker_open(), egui::Button::new("Сделать")).clicked() {
                self.open_picker(ui.ctx());
            }
            if ui.button("Не спрашивать").clicked() {
                self.dont_ask(store);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, ph::X, "Скрыть до следующего запуска", false).clicked() {
                    self.hidden = true;
                }
            });
        });
    }

    pub fn settings_section(&mut self, ui: &mut egui::Ui) {
        if !self.status.installed {
            ui.label(RichText::new("Доступно после установки программы").color(theme::MUTED));
        } else if self.status.is_default {
            ui.label(RichText::new(format!("{}  PDFsmith — программа по умолчанию для PDF", ph::CHECK)).color(theme::OK));
        } else if ui.add_enabled(!self.picker_open(), egui::Button::new("Сделать по умолчанию")).clicked() {
            self.open_picker(ui.ctx());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_DEFAULT: DefaultStatus = DefaultStatus { installed: true, is_default: false };

    #[cfg(windows)]
    #[test]
    fn picker_succeeded_treats_cancel_as_success() {
        assert!(picker_succeeded(0)); // S_OK
        assert!(picker_succeeded(HRESULT_ERROR_CANCELLED), "отмена пользователем — не ошибка");
        assert!(!picker_succeeded(-2147024809i32), "прочие ошибки — фолбэк на ms-settings");
    }

    #[test]
    fn prompt_only_when_installed_not_default_and_allowed() {
        assert!(should_prompt(NOT_DEFAULT, true));
        assert!(!should_prompt(NOT_DEFAULT, false));
        assert!(!should_prompt(DefaultStatus { installed: true, is_default: true }, true));
        assert!(!should_prompt(DefaultStatus { installed: false, is_default: false }, true));
    }

    #[test]
    fn dont_ask_persists() {
        let dir = std::env::temp_dir().join(format!("pdfsmith-defapp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut store = SettingsStore::at(path.clone());
        let mut d = DefaultApp::with_status(NOT_DEFAULT);
        assert!(d.banner_visible(store.data.ask_default_app));
        d.dont_ask(&mut store);
        assert!(!d.banner_visible(store.data.ask_default_app));
        assert!(!crate::settings::Settings::load(&path).ask_default_app);
    }

    #[test]
    fn disabled_never_prompts() {
        assert!(!DefaultApp::disabled().banner_visible(true));
    }

    #[test]
    fn banner_and_section_render() {
        let mut store = SettingsStore::in_memory();
        let ctx = egui::Context::default();
        for status in [NOT_DEFAULT, DefaultStatus { installed: true, is_default: true }, DefaultStatus { installed: false, is_default: false }] {
            let mut d = DefaultApp::with_status(status);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    d.banner(ui, &mut store);
                    d.settings_section(ui);
                });
            });
        }
    }
}
