//! Обновления PDFsmith через GitHub Releases: манифест `latest.json`,
//! решение «предлагать ли», загрузка с проверкой SHA-256, запуск установщика.

pub mod error;
pub mod manifest;
pub mod decide;

pub use error::Error;
pub use manifest::Manifest;
