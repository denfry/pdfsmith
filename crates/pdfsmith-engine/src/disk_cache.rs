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
