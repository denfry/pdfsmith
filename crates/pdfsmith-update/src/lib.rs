//! Обновления PDFsmith через GitHub Releases: манифест `latest.json`,
//! решение «предлагать ли», загрузка с проверкой SHA-256, запуск установщика.

pub mod apply;
pub mod decide;
pub mod download;
pub mod error;
pub mod manifest;
pub mod worker;

pub use error::Error;
pub use manifest::Manifest;
pub use worker::{spawn, Command, Config, UpdateEvent, UpdaterHandle};
