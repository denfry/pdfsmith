//! Обновления в UI: плашка над документом, фоновые загрузки, решение при выходе.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use pdfsmith_update::{decide, Command, Config, Manifest, UpdateEvent, UpdaterHandle};

use crate::settings::SettingsStore;
use crate::theme::{self, icon_button};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `exe_dir` — это установленное расположение `%LOCALAPPDATA%\PDFsmith`?
/// Портативный/dev-запуск (любая другая папка) не должен трогать сеть и
/// ставить обновления в профиль пользователя. Сравнение без учёта регистра,
/// после канонизации там, где обе стороны существуют.
pub fn is_installed_location(exe_dir: &Path, local_appdata: &Path) -> bool {
    let installed = local_appdata.join("PDFsmith");
    let (a, b) = match (exe_dir.canonicalize(), installed.canonicalize()) {
        (Ok(a), Ok(b)) => (a, b),
        _ => (exe_dir.to_path_buf(), installed),
    };
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdState {
    Idle,
    Available(Manifest),
    Downloading { manifest: Manifest, done: u64, total: Option<u64> },
    Ready { manifest: Manifest, path: PathBuf },
}

pub struct UpdateUi {
    handle: Option<UpdaterHandle>,
    pub state: UpdState,
    /// Идёт ручная проверка из окна настроек.
    pub checking: bool,
    /// Последнее сообщение для пользователя: (текст, это ошибка).
    pub message: Option<(String, bool)>,
    banner_hidden: bool,
    show_notes: bool,
    restart_requested: bool,
}

impl UpdateUi {
    /// Без потока обновлений (тесты, нет `%LOCALAPPDATA%`).
    pub fn disabled() -> Self {
        Self::with_handle(None)
    }

    /// Боевой режим: поток `updater`, фоновая проверка при запуске (раз в 12 ч).
    /// Портативный/dev-запуск обновления не проверяет (см. `is_installed_location`).
    pub fn new(ctx: &egui::Context, store: &SettingsStore) -> Self {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .zip(std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
            .is_some_and(|(exe_dir, local_appdata)| is_installed_location(&exe_dir, &local_appdata));
        if !installed {
            return Self::with_handle(None);
        }
        let handle = Config::github(CURRENT_VERSION).map(|cfg| {
            let ctx = ctx.clone();
            pdfsmith_update::spawn(cfg, Box::new(move || ctx.request_repaint()))
        });
        let ui = Self::with_handle(handle);
        if decide::should_check(store.data.last_check, decide::now_secs()) {
            ui.send(Command::Check { quiet: true });
        }
        ui
    }

    pub fn with_handle(handle: Option<UpdaterHandle>) -> Self {
        UpdateUi {
            handle,
            state: UpdState::Idle,
            checking: false,
            message: None,
            banner_hidden: false,
            show_notes: false,
            restart_requested: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.handle.is_some()
    }

    fn send(&self, cmd: Command) {
        if let Some(h) = &self.handle {
            let _ = h.cmd_tx.send(cmd);
        }
    }

    pub fn poll(&mut self, store: &mut SettingsStore) {
        let events: Vec<UpdateEvent> = match &self.handle {
            Some(h) => h.event_rx.try_iter().collect(),
            None => return,
        };
        for ev in events {
            self.on_event(ev, store);
        }
    }

    fn on_event(&mut self, ev: UpdateEvent, store: &mut SettingsStore) {
        match ev {
            UpdateEvent::Available { manifest, quiet } => {
                self.checking = false;
                if matches!(self.state, UpdState::Downloading { .. } | UpdState::Ready { .. }) {
                    return;
                }
                if !decide::should_offer(CURRENT_VERSION, &manifest.version, store.data.skipped_version.as_deref(), !quiet) {
                    // Нечего предложить — как будто и не было обновления.
                    store.data.last_check = Some(decide::now_secs());
                    store.save();
                    return;
                }
                if !quiet {
                    self.message = Some((format!("Доступна версия {}", manifest.version), false));
                    self.banner_hidden = false;
                }
                if store.data.auto_update {
                    self.start_download(manifest, quiet);
                } else {
                    self.state = UpdState::Available(manifest);
                }
            }
            UpdateEvent::UpToDate { quiet } => {
                self.checking = false;
                store.data.last_check = Some(decide::now_secs());
                store.save();
                if !quiet {
                    self.message = Some(("У вас последняя версия".into(), false));
                }
            }
            UpdateEvent::Progress { done, total } => {
                if let UpdState::Downloading { done: d, total: t, .. } = &mut self.state {
                    *d = done;
                    *t = total;
                }
            }
            UpdateEvent::Ready { manifest, path } => self.state = UpdState::Ready { manifest, path },
            UpdateEvent::Failed { quiet, error, cancelled } => {
                self.checking = false;
                if let UpdState::Downloading { manifest, .. } = &self.state {
                    let m = manifest.clone();
                    self.state = if quiet { UpdState::Idle } else { UpdState::Available(m) };
                }
                if !quiet && !cancelled {
                    self.message = Some((error, true));
                }
            }
        }
    }

    fn start_download(&mut self, manifest: Manifest, quiet: bool) {
        self.state = UpdState::Downloading { manifest: manifest.clone(), done: 0, total: None };
        self.send(Command::Download { manifest, quiet });
    }

    pub fn update_now(&mut self) {
        if let UpdState::Available(m) = &self.state {
            let m = m.clone();
            self.message = None;
            self.start_download(m, false);
        }
    }

    pub fn cancel_download(&self) {
        if let Some(h) = &self.handle {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn skip(&mut self, store: &mut SettingsStore) {
        if let UpdState::Available(m) = &self.state {
            store.data.skipped_version = Some(m.version.clone());
            store.data.last_check = Some(decide::now_secs());
            store.save();
            self.state = UpdState::Idle;
        }
    }

    pub fn check_now(&mut self) {
        if self.enabled() {
            self.checking = true;
            self.message = None;
            self.send(Command::Check { quiet: false });
        }
    }

    pub fn set_auto_update(&mut self, store: &mut SettingsStore, on: bool) {
        store.data.auto_update = on;
        store.save();
        if on {
            if let UpdState::Available(m) = &self.state {
                let m = m.clone();
                self.start_download(m, true);
            }
        }
    }

    pub fn request_restart(&mut self) -> bool {
        self.restart_requested = matches!(self.state, UpdState::Ready { .. });
        self.restart_requested
    }

    /// Пользователь нажал «Отмена» в диалоге несохранённых изменений.
    pub fn cancel_restart(&mut self) {
        self.restart_requested = false;
    }

    /// Что сделать при выходе: (установщик, перезапускать ли). При других
    /// открытых окнах PDFsmith — ничего: установщик закрыл бы их без спроса.
    pub fn exit_action(&self, others_running: bool) -> Option<(PathBuf, bool)> {
        match &self.state {
            UpdState::Ready { path, .. } if !others_running => Some((path.clone(), self.restart_requested)),
            _ => None,
        }
    }

    pub fn banner_visible(&self, store: &SettingsStore) -> bool {
        if self.banner_hidden {
            return false;
        }
        match &self.state {
            UpdState::Idle => false,
            UpdState::Available(_) => true,
            // В режиме автообновления загрузка тихая — только строка состояния.
            UpdState::Downloading { .. } | UpdState::Ready { .. } => !store.data.auto_update,
        }
    }

    pub fn status_text(&self, store: &SettingsStore) -> Option<String> {
        if !store.data.auto_update {
            return None;
        }
        match &self.state {
            UpdState::Downloading { manifest, .. } => Some(format!("Загрузка обновления {}…", manifest.version)),
            UpdState::Ready { manifest, .. } => Some(format!("Обновление {} будет установлено при закрытии", manifest.version)),
            _ => None,
        }
    }

    pub fn banner(&mut self, ui: &mut egui::Ui, store: &mut SettingsStore) {
        let state = self.state.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new(ph::ARROW_CIRCLE_UP).size(theme::ICON).color(theme::ACCENT));
            match &state {
                UpdState::Idle => {}
                UpdState::Available(m) => {
                    ui.label(format!("Доступна версия {}", m.version));
                    if !m.notes.trim().is_empty() && ui.link("Что нового").clicked() {
                        self.show_notes = !self.show_notes;
                    }
                    if ui.button("Обновить").clicked() {
                        self.update_now();
                    }
                    if ui.button("Пропустить эту версию").clicked() {
                        self.skip(store);
                    }
                }
                UpdState::Downloading { manifest, done, total } => {
                    ui.label(format!("Загрузка версии {}…", manifest.version));
                    match total.filter(|t| *t > 0) {
                        Some(t) => {
                            ui.add(egui::ProgressBar::new(*done as f32 / t as f32).desired_width(160.0).show_percentage());
                        }
                        None => {
                            ui.label(format!("{:.1} МБ", *done as f64 / 1_048_576.0));
                        }
                    }
                    if ui.button("Отмена").clicked() {
                        self.cancel_download();
                    }
                }
                UpdState::Ready { manifest, .. } => {
                    ui.label(format!("Версия {} загружена", manifest.version));
                    if ui.button("Перезапустить и установить").clicked() && self.request_restart() {
                        if pdfsmith_update::apply::other_instances_running() {
                            self.cancel_restart();
                            self.message = Some(("Закройте другие окна PDFsmith и нажмите ещё раз".into(), true));
                        } else {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
            }
            if let Some((msg, true)) = &self.message {
                ui.label(RichText::new(msg).color(theme::DANGER));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, ph::X, "Скрыть до следующего запуска", false).clicked() {
                    self.banner_hidden = true;
                }
            });
        });
        if self.show_notes {
            if let UpdState::Available(m) = &state {
                ui.label(RichText::new(&m.notes).color(theme::MUTED));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;

    use pdfsmith_update::manifest::ALLOWED_PREFIX;

    use crate::settings::Settings;

    struct Rig {
        ui: UpdateUi,
        store: SettingsStore,
        ev: Sender<UpdateEvent>,
        cmds: Receiver<Command>,
        path: PathBuf,
    }

    impl Rig {
        fn push(&mut self, e: UpdateEvent) {
            self.ev.send(e).unwrap();
            self.ui.poll(&mut self.store);
        }
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("pdfsmith-updui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let (cmd_tx, cmds) = channel();
        let (ev, event_rx) = channel();
        let handle = UpdaterHandle { cmd_tx, event_rx, cancel: Arc::new(AtomicBool::new(false)) };
        Rig { ui: UpdateUi::with_handle(Some(handle)), store: SettingsStore::at(path.clone()), ev, cmds, path }
    }

    fn m(v: &str) -> Manifest {
        Manifest { version: v.into(), notes: "Исправления".into(), url: format!("{ALLOWED_PREFIX}v{v}/pdfsmith-setup.exe"), sha256: "a".repeat(64) }
    }

    fn setup_path() -> PathBuf {
        PathBuf::from(r"C:\tmp\pdfsmith-setup-999.0.0.exe")
    }

    #[test]
    fn is_installed_location_matches_case_insensitively() {
        let dir = std::env::temp_dir().join(format!("pdfsmith-installed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let local_appdata = dir.join("LocalAppData");
        let installed = local_appdata.join("PDFsmith");
        std::fs::create_dir_all(&installed).unwrap();
        let differently_cased = local_appdata.join("PDFSMITH");
        assert!(is_installed_location(&installed, &local_appdata));
        assert!(is_installed_location(&differently_cased, &local_appdata), "регистр не должен иметь значения");
    }

    #[test]
    fn is_installed_location_rejects_other_dir() {
        let dir = std::env::temp_dir().join(format!("pdfsmith-not-installed-{}", std::process::id()));
        let local_appdata = dir.join("LocalAppData");
        let other = dir.join("Portable");
        assert!(!is_installed_location(&other, &local_appdata));
    }

    #[test]
    fn quiet_check_shows_banner_and_does_not_record_check_time() {
        // Обновление предложено, но ещё не поставлено — на следующем запуске
        // его нужно предложить снова, поэтому `last_check` не трогаем.
        let mut r = rig("banner");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")));
        assert!(r.ui.banner_visible(&r.store));
        assert!(Settings::load(&r.path).last_check.is_none());
        assert!(r.cmds.try_recv().is_err(), "без автообновления ничего не качаем");
    }

    #[test]
    fn up_to_date_records_check_time() {
        let mut r = rig("uptodate-check-time");
        r.push(UpdateEvent::UpToDate { quiet: true });
        assert!(Settings::load(&r.path).last_check.is_some());
    }

    #[test]
    fn nothing_to_offer_records_check_time() {
        // Версия старее текущей или пропущенная — предлагать нечего, но
        // проверка состоялась, так что `last_check` обновляем.
        let mut r = rig("nothing-to-offer");
        r.push(UpdateEvent::Available { manifest: m("0.0.1"), quiet: true });
        assert!(Settings::load(&r.path).last_check.is_some());
    }

    #[test]
    fn skip_records_check_time() {
        let mut r = rig("skip-check-time");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        assert!(Settings::load(&r.path).last_check.is_none());
        r.ui.skip(&mut r.store);
        assert!(Settings::load(&r.path).last_check.is_some());
    }

    #[test]
    fn auto_mode_manual_check_downloads_not_quietly() {
        // F4: ручная проверка «Проверить сейчас» в режиме автообновления —
        // ошибка загрузки должна быть видна, раз пользователь сам её запросил.
        let mut r = rig("auto-manual");
        r.ui.set_auto_update(&mut r.store, true);
        let _ = r.cmds.try_recv();
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: false });
        match r.cmds.try_recv() {
            Ok(Command::Download { manifest, quiet }) => {
                assert_eq!(manifest.version, "999.0.0");
                assert!(!quiet, "ручная проверка не должна качать тихо");
            }
            other => panic!("ожидали Download, получили {other:?}"),
        }
    }

    #[test]
    fn older_or_same_version_is_ignored() {
        let mut r = rig("older");
        r.push(UpdateEvent::Available { manifest: m("0.0.1"), quiet: false });
        r.push(UpdateEvent::Available { manifest: m(CURRENT_VERSION), quiet: false });
        assert_eq!(r.ui.state, UpdState::Idle);
    }

    #[test]
    fn skip_persists_and_newer_version_still_offered() {
        let mut r = rig("skip");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.skip(&mut r.store);
        assert_eq!(r.ui.state, UpdState::Idle);
        assert_eq!(Settings::load(&r.path).skipped_version.as_deref(), Some("999.0.0"));
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Idle, "пропущенная версия не предлагается");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: false });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")), "ручная проверка игнорирует пропуск");
        r.ui.state = UpdState::Idle;
        r.push(UpdateEvent::Available { manifest: m("999.0.1"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.1")));
    }

    #[test]
    fn auto_update_downloads_quietly_and_installs_on_exit() {
        let mut r = rig("auto");
        r.ui.set_auto_update(&mut r.store, true);
        assert!(Settings::load(&r.path).auto_update);
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        match r.cmds.try_recv() {
            Ok(Command::Download { manifest, quiet }) => {
                assert_eq!(manifest.version, "999.0.0");
                assert!(quiet);
            }
            other => panic!("ожидали Download, получили {other:?}"),
        }
        assert!(!r.ui.banner_visible(&r.store));
        assert!(r.ui.status_text(&r.store).unwrap().contains("Загрузка"));
        r.push(UpdateEvent::Ready { manifest: m("999.0.0"), path: setup_path() });
        assert!(r.ui.status_text(&r.store).unwrap().contains("при закрытии"));
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
    }

    #[test]
    fn manual_update_restart_flow() {
        let mut r = rig("manual");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        assert!(matches!(r.cmds.try_recv(), Ok(Command::Download { quiet: false, .. })));
        r.push(UpdateEvent::Progress { done: 50, total: Some(100) });
        assert_eq!(r.ui.state, UpdState::Downloading { manifest: m("999.0.0"), done: 50, total: Some(100) });
        assert!(r.ui.banner_visible(&r.store));
        assert_eq!(r.ui.exit_action(false), None, "пока не скачано — ставить нечего");
        r.push(UpdateEvent::Ready { manifest: m("999.0.0"), path: setup_path() });
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
        assert!(r.ui.request_restart());
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), true)));
        assert_eq!(r.ui.exit_action(true), None, "другие окна открыты — откладываем");
        r.ui.cancel_restart();
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
    }

    #[test]
    fn duplicate_available_while_downloading_is_ignored() {
        let mut r = rig("dup");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        let _ = r.cmds.try_recv();
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: false });
        assert!(matches!(r.ui.state, UpdState::Downloading { .. }));
        assert!(r.cmds.try_recv().is_err(), "вторая загрузка не должна стартовать");
    }

    #[test]
    fn failures_are_silent_in_background_and_visible_on_demand() {
        let mut r = rig("fail");
        r.push(UpdateEvent::Failed { quiet: true, error: "нет связи".into(), cancelled: false });
        assert_eq!(r.ui.message, None);

        r.ui.check_now();
        assert!(matches!(r.cmds.try_recv(), Ok(Command::Check { quiet: false })));
        assert!(r.ui.checking);
        r.push(UpdateEvent::Failed { quiet: false, error: "нет связи".into(), cancelled: false });
        assert!(!r.ui.checking);
        assert_eq!(r.ui.message, Some(("нет связи".into(), true)));

        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        r.push(UpdateEvent::Failed { quiet: false, error: "повреждён".into(), cancelled: false });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")), "после ошибки можно повторить");
        assert_eq!(r.ui.message, Some(("повреждён".into(), true)));

        r.ui.update_now();
        r.push(UpdateEvent::Failed { quiet: false, error: "загрузка отменена".into(), cancelled: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")));
        assert_eq!(r.ui.message, None, "отмена — не ошибка");
    }

    #[test]
    fn manual_up_to_date_message() {
        let mut r = rig("uptodate");
        r.ui.check_now();
        r.push(UpdateEvent::UpToDate { quiet: false });
        assert_eq!(r.ui.message, Some(("У вас последняя версия".into(), false)));
    }

    #[test]
    fn banner_renders_in_every_state() {
        let mut r = rig("render");
        let ctx = egui::Context::default();
        for state in [
            UpdState::Available(m("999.0.0")),
            UpdState::Downloading { manifest: m("999.0.0"), done: 10, total: None },
            UpdState::Downloading { manifest: m("999.0.0"), done: 10, total: Some(100) },
            UpdState::Ready { manifest: m("999.0.0"), path: setup_path() },
        ] {
            r.ui.state = state;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| r.ui.banner(ui, &mut r.store));
            });
        }
    }
}
