//! Дисковый кэш отрендеренных тайлов.
//!
//! Чертёж не меняется, поэтому однажды отрендеренный тайл можно сохранить на
//! диск и при повторном открытии файла взять готовым (загрузку страницы можно
//! отложить до промаха кэша). Формат файла тайла: 8-байтный заголовок
//! (u32 width, u32 height, little-endian) + сырой RGBA.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Тайл в виде RGBA.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Дисковый кэш для одного документа. Все тайлы лежат в подкаталоге,
/// уникальном для конкретного файла (по пути/размеру/времени изменения).
pub struct DiskCache {
    dir: PathBuf,
    enabled: bool,
}

impl DiskCache {
    /// Создаёт (при необходимости) каталог кэша `root/<file_id>`. Если каталог
    /// создать не удалось, кэш молча отключается.
    pub fn new(root: &Path, file_id: &str) -> Self {
        let dir = root.join(file_id);
        let enabled = std::fs::create_dir_all(&dir).is_ok();
        // Отметка использования: обновляет время каталога для [`prune`].
        let _ = std::fs::write(dir.join(".used"), []);
        DiskCache { dir, enabled }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Возвращает тайл из кэша, если он есть и не повреждён.
    pub fn get(&self, page: usize, rot: u8, lod: i32, col: u32, row: u32) -> Option<Tile> {
        if !self.enabled {
            return None;
        }
        let data = std::fs::read(self.path(page, rot, lod, col, row)).ok()?;
        if data.len() < 8 {
            return None;
        }
        let width = u32::from_le_bytes(data[0..4].try_into().ok()?);
        let height = u32::from_le_bytes(data[4..8].try_into().ok()?);
        let rgba = &data[8..];
        if rgba.len() != (width as usize) * (height as usize) * 4 {
            return None;
        }
        Some(Tile { width, height, rgba: rgba.to_vec() })
    }

    /// Сохраняет тайл. Ошибки записи игнорируются (кэш — best-effort).
    pub fn put(&self, page: usize, rot: u8, lod: i32, col: u32, row: u32, tile: &Tile) {
        if !self.enabled {
            return;
        }
        let mut buf = Vec::with_capacity(8 + tile.rgba.len());
        buf.extend_from_slice(&tile.width.to_le_bytes());
        buf.extend_from_slice(&tile.height.to_le_bytes());
        buf.extend_from_slice(&tile.rgba);
        let _ = std::fs::write(self.path(page, rot, lod, col, row), buf);
    }

    fn path(&self, page: usize, rot: u8, lod: i32, col: u32, row: u32) -> PathBuf {
        self.dir.join(format!("p{page}_r{rot}_l{lod}_{col}_{row}.bin"))
    }
}

/// Чистит корень кэша: удаляет каталоги чужой версии (без суффикса
/// `keep_suffix`), затем самые давно использованные, пока общий размер больше
/// `max_bytes`. Каталоги, тронутые за последние `recent`, не трогает — их может
/// прямо сейчас читать открытый документ. Возвращает число удалённых каталогов.
pub fn prune(root: &Path, keep_suffix: &str, max_bytes: u64, recent: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    let mut live: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let used = meta.modified().unwrap_or(now);
        let is_recent = now.duration_since(used).map(|d| d < recent).unwrap_or(true);
        let current = path.file_name().map(|n| n.to_string_lossy().ends_with(keep_suffix)).unwrap_or(false);
        if !current && !is_recent {
            if std::fs::remove_dir_all(&path).is_ok() {
                removed += 1;
            }
            continue;
        }
        live.push((if is_recent { now } else { used }, dir_size(&path), path));
    }
    let mut total: u64 = live.iter().map(|(_, s, _)| s).sum();
    live.sort_by_key(|(t, _, _)| *t);
    for (t, size, path) in live {
        if total <= max_bytes || t == now {
            break;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            total -= size;
            removed += 1;
        }
    }
    removed
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|it| it.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum())
        .unwrap_or(0)
}

/// Стабильный идентификатор файла по пути, размеру и времени изменения.
/// Меняется, если файл изменили, — кэш не вернёт устаревшие тайлы.
pub fn file_id(path: &Path) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().hash(&mut h);
    if let Ok(meta) = std::fs::metadata(path) {
        meta.len().hash(&mut h);
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                dur.as_secs().hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        // Уникальный подкаталог во временной папке (без Date/Random — берём
        // адрес локальной переменной как зерно уникальности теста).
        let seed = &temp_root as *const _ as usize;
        std::env::temp_dir().join(format!("pdfsmith-test-{seed:x}"))
    }

    #[test]
    fn put_then_get_roundtrips_a_tile() {
        let cache = DiskCache::new(&temp_root(), "doc-a");
        let tile = Tile { width: 2, height: 1, rgba: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        cache.put(0, 0, 1, 3, 4, &tile);
        assert_eq!(cache.get(0, 0, 1, 3, 4), Some(tile));
    }

    #[test]
    fn prune_drops_old_versions_then_oldest_until_under_limit() {
        let root = std::env::temp_dir().join("pdfsmith-test-prune");
        let _ = std::fs::remove_dir_all(&root);
        let old_ver = DiskCache::new(&root, "aaa-v1");
        old_ver.put(0, 0, 0, 0, 0, &Tile { width: 1, height: 1, rgba: vec![0; 4] });
        let a = DiskCache::new(&root, "bbb-v3");
        a.put(0, 0, 0, 0, 0, &Tile { width: 16, height: 16, rgba: vec![0; 1024] });
        let b = DiskCache::new(&root, "ccc-v3");
        b.put(0, 0, 0, 0, 0, &Tile { width: 16, height: 16, rgba: vec![0; 1024] });
        // Всё «свежее» — ничего не трогаем.
        assert_eq!(prune(&root, "-v3", 0, std::time::Duration::from_secs(3600)), 0);
        // Без защиты свежих: чужая версия уходит, затем старейшие до лимита.
        let removed = prune(&root, "-v3", 1500, std::time::Duration::ZERO);
        assert!(!root.join("aaa-v1").exists());
        assert_eq!(removed, 2, "старая версия + один из двух текущих");
        assert_eq!(["bbb-v3", "ccc-v3"].iter().filter(|d| root.join(d).exists()).count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn get_missing_returns_none() {
        let cache = DiskCache::new(&temp_root(), "doc-b");
        assert_eq!(cache.get(9, 0, 9, 9, 9), None);
    }

    #[test]
    fn corrupt_short_file_returns_none() {
        let cache = DiskCache::new(&temp_root(), "doc-c");
        std::fs::write(cache.path(0, 0, 0, 0, 0), [1u8, 2, 3]).unwrap();
        assert_eq!(cache.get(0, 0, 0, 0, 0), None);
    }

    #[test]
    fn wrong_length_payload_returns_none() {
        let cache = DiskCache::new(&temp_root(), "doc-d");
        // Заголовок обещает 2×2×4=16 байт, а данных меньше.
        let mut buf = Vec::new();
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&[0u8; 4]);
        std::fs::write(cache.path(0, 0, 0, 0, 0), buf).unwrap();
        assert_eq!(cache.get(0, 0, 0, 0, 0), None);
    }
}
