//! Настройки приложения: `%APPDATA%\PDFsmith\settings.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub auto_update: bool,
    pub last_check: Option<u64>,
    pub skipped_version: Option<String>,
    pub ask_default_app: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { auto_update: false, last_check: None, skipped_version: None, ask_default_app: true }
    }
}

impl Settings {
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
        let mut s = Settings { auto_update: true, last_check: Some(42), skipped_version: Some("1.2.3".into()), ask_default_app: false };
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
}
