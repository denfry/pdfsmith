//! Ошибки обновления. Текст `Display` показывается пользователю как есть.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Нет связи с GitHub. Проверьте интернет и попробуйте позже.")]
    Network(String),
    #[error("На GitHub пока нет опубликованных обновлений")]
    NoRelease,
    #[error("некорректный манифест обновления: {0}")]
    BadManifest(String),
    #[error("Файл обновления повреждён, попробуйте ещё раз")]
    HashMismatch,
    #[error("ошибка файла обновления: {0}")]
    Io(#[from] std::io::Error),
    #[error("загрузка отменена")]
    Cancelled,
}
