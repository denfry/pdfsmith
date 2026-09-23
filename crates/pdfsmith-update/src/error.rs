//! Ошибки обновления. Текст `Display` показывается пользователю как есть.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("нет связи с сервером обновлений: {0}")]
    Network(String),
    #[error("некорректный манифест обновления: {0}")]
    BadManifest(String),
    #[error("файл обновления повреждён (контрольная сумма не совпала), попробуйте ещё раз")]
    HashMismatch,
    #[error("ошибка файла обновления: {0}")]
    Io(#[from] std::io::Error),
    #[error("загрузка отменена")]
    Cancelled,
}
