//! pdfsmith-engine — логика производительности просмотрщика без UI.
//!
//! Чистые, тестируемые модули: геометрия тайлов, математика вьюпорта,
//! LRU-кэш с бюджетом памяти, планировщик рендер-задач.

pub mod cache;
pub mod disk_cache;
pub mod lod;
pub mod tile;
pub mod viewport;
