//! Настройки приложения: `%APPDATA%\PDFsmith\settings.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::ThemeChoice;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub auto_update: bool,
    pub last_check: Option<u64>,
    pub skipped_version: Option<String>,
    pub ask_default_app: bool,
    /// Принтер, на котором печатали в прошлый раз.
    pub last_printer: Option<String>,
    /// Оформление: как в системе, светлое или тёмное.
    pub theme: ThemeChoice,
    /// Отпечаток (hex) сертификата, которым подписывали в прошлый раз.
    pub last_cert: Option<String>,
    /// Недавние файлы, свежие первыми.
    pub recent: Vec<RecentFile>,
}

/// Недавний документ и страница, на которой его закрыли.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFile {
    pub path: PathBuf,
    #[serde(default)]
    pub page: usize,
}

/// Сколько недавних файлов помнить.
pub const RECENT_MAX: usize = 12;

/// Пути Windows сравниваем без учёта регистра и вида слэшей.
fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_update: false,
            last_check: None,
            skipped_version: None,
            ask_default_app: true,
            last_printer: None,
            theme: ThemeChoice::System,
            last_cert: None,
            recent: Vec::new(),
        }
    }
}

impl Settings {
    /// Файл открыт: поднять его в начало списка недавних (страница сохраняется).
    pub fn touch_recent(&mut self, path: &Path) {
        let page = self.recent_page(path).unwrap_or(0);
        self.recent.retain(|r| !same_path(&r.path, path));
        self.recent.insert(0, RecentFile { path: path.to_path_buf(), page });
        self.recent.truncate(RECENT_MAX);
    }

    /// Запомнить страницу, не меняя порядок списка.
    pub fn set_recent_page(&mut self, path: &Path, page: usize) {
        if let Some(r) = self.recent.iter_mut().find(|r| same_path(&r.path, path)) {
            r.page = page;
        }
    }

    pub fn recent_page(&self, path: &Path) -> Option<usize> {
        self.recent.iter().find(|r| same_path(&r.path, path)).map(|r| r.page)
    }

    pub fn forget_recent(&mut self, path: &Path) {
        self.recent.retain(|r| !same_path(&r.path, path));
    }

    /// Отсутствующий или повреждённый файл → значения по умолчанию.
    pub fn load(path: &Path) -> Settings {
        let Ok(text) = std::fs::read_to_string(path) else { return Settings::default() };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            log::warn!("настройки {} повреждены ({e}), беру значения по умолчанию", path.display());
            Settings::default()
        })
    }

    /// Атомарная запись: временный файл рядом + rename.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, data)?;
        std::fs::rename(&tmp, path)
    }
}

/// Настройки + куда их сохранять. Без пути (тесты) живут только в памяти.
pub struct SettingsStore {
    pub data: Settings,
    path: Option<PathBuf>,
}

impl SettingsStore {
    pub fn load_default() -> Self {
        let path = std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("PDFsmith").join("settings.json"));
        let data = path.as_deref().map(Settings::load).unwrap_or_default();
        SettingsStore { data, path }
    }

    pub fn in_memory() -> Self {
        SettingsStore { data: Settings::default(), path: None }
    }

    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        SettingsStore { data: Settings::load(&path), path: Some(path) }
    }

    /// Правка поверх свежей копии с диска: другое окно PDFsmith могло успеть
    /// записать свои изменения (например, свой список недавних файлов).
    pub fn update(&mut self, f: impl FnOnce(&mut Settings)) {
        if let Some(p) = &self.path {
            if p.exists() {
                self.data = Settings::load(p);
            }
        }
        f(&mut self.data);
        self.save();
    }

    /// Ошибка записи не фатальна: пишем в лог, работаем с настройками в памяти.
    pub fn save(&self) {
        if let Some(p) = &self.path {
            if let Err(e) = self.data.save(p) {
                log::warn!("не удалось сохранить настройки {}: {e}", p.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pdfsmith-settings-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn missing_file_gives_defaults() {
        let s = Settings::load(&tmp("missing").join("settings.json"));
        assert_eq!(s, Settings::default());
        assert!(!s.auto_update);
        assert!(s.ask_default_app);
    }

    #[test]
    fn garbage_gives_defaults() {
        let dir = tmp("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, "{ не json").unwrap();
        assert_eq!(Settings::load(&p), Settings::default());
    }

    #[test]
    fn partial_json_keeps_other_defaults() {
        let dir = tmp("partial");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, r#"{"auto_update": true}"#).unwrap();
        let s = Settings::load(&p);
        assert!(s.auto_update);
        assert!(s.ask_default_app);
        assert_eq!(s.last_check, None);
    }

    #[test]
    fn roundtrip_creates_dirs_and_overwrites() {
        let p = tmp("roundtrip").join("nested").join("settings.json");
        let mut s = Settings {
            auto_update: true,
            last_check: Some(42),
            skipped_version: Some("1.2.3".into()),
            ask_default_app: false,
            last_printer: Some("Microsoft Print to PDF".into()),
            theme: ThemeChoice::Light,
            last_cert: Some("0A1B".into()),
            recent: vec![RecentFile { path: PathBuf::from(r"C:\docs\a.pdf"), page: 3 }],
        };
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
        s.last_check = Some(43);
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p).last_check, Some(43));
        assert!(!p.with_extension("json.tmp").exists());
    }

    #[test]
    fn save_failure_is_not_fatal() {
        let dir = tmp("blocked");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"file, not a dir").unwrap();
        let mut store = SettingsStore::at(blocker.join("settings.json"));
        store.data.auto_update = true;
        store.save(); // не паникует
        assert!(store.data.auto_update);
    }

    #[test]
    fn recent_moves_to_front_keeps_page_and_caps() {
        let mut s = Settings::default();
        s.touch_recent(Path::new(r"C:\docs\a.pdf"));
        s.touch_recent(Path::new(r"C:\docs\b.pdf"));
        s.set_recent_page(Path::new(r"c:/DOCS/A.PDF"), 7);
        s.touch_recent(Path::new(r"C:\Docs\A.pdf"));
        assert_eq!(s.recent.len(), 2, "тот же файл в другом регистре не дублируется");
        assert_eq!(s.recent[0].page, 7);
        assert_eq!(s.recent_page(Path::new(r"C:\docs\b.pdf")), Some(0));
        for i in 0..20 {
            s.touch_recent(&PathBuf::from(format!(r"C:\docs\f{i}.pdf")));
        }
        assert_eq!(s.recent.len(), RECENT_MAX);
        assert_eq!(s.recent[0].path, PathBuf::from(r"C:\docs\f19.pdf"));
        s.forget_recent(Path::new(r"C:\docs\f19.pdf"));
        assert_eq!(s.recent[0].path, PathBuf::from(r"C:\docs\f18.pdf"));
    }

    #[test]
    fn update_merges_with_file_written_by_other_window() {
        let p = tmp("update").join("settings.json");
        let mut a = SettingsStore::at(p.clone());
        let mut b = SettingsStore::at(p.clone());
        a.update(|s| s.touch_recent(Path::new(r"C:\docs\one.pdf")));
        b.update(|s| s.touch_recent(Path::new(r"C:\docs\two.pdf")));
        let r = Settings::load(&p).recent;
        assert_eq!(r.len(), 2, "второе окно не должно затирать список первого");
        assert_eq!(r[0].path, PathBuf::from(r"C:\docs\two.pdf"));
    }
}
