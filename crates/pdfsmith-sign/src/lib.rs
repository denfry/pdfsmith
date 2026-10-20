//! Электронная подпись PDF: инкрементальное обновление с отделённой CMS
//! (`adbe.pkcs7.detached`, как у Adobe/КриптоПро PDF), видимый штамп и
//! сертификаты из хранилища Windows «Личное» (КриптоПро CSP, токены).

mod appearance;
mod pdf;
#[cfg(windows)]
pub mod store;
#[cfg(not(windows))]
pub mod store {
    //! Хранилище сертификатов есть только в Windows.
    use crate::{CertInfo, Now, SignTime};

    const NO: &str = "подпись сертификатом доступна только в Windows";

    pub fn now() -> Now {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let (days, rem) = ((s / 86_400) as i64, s % 86_400);
        // Гражданская дата из дней от 1970-01-01 (алгоритм Хиннанта).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
        Now {
            local: SignTime { year, month, day, hour: (rem / 3600) as u8, minute: (rem / 60 % 60) as u8, second: (rem % 60) as u8, offset_min: 0 },
            utc_filetime: (s + 11_644_473_600) * 10_000_000,
        }
    }

    pub fn list_certificates() -> Result<Vec<CertInfo>, String> {
        Err(NO.into())
    }

    pub fn sign_detached(_thumbprint: &[u8], _chunks: &[&[u8]], _utc_filetime: u64) -> Result<Vec<u8>, String> {
        Err(NO.into())
    }
}

pub use appearance::{render_text, Raster};
pub use pdf::{sign_pdf, SignField, SignInfo, CONTENTS_LEN};

#[derive(Debug, thiserror::Error)]
pub enum SignError {
    #[error("не удалось разобрать PDF: {0}")]
    Parse(String),
    #[error("зашифрованный PDF подписать нельзя")]
    Encrypted,
    #[error("нет страницы {0}")]
    NoPage(usize),
    #[error("структура PDF: {0}")]
    Structure(String),
    #[error("{0}")]
    Signer(String),
    #[error("подпись ({0} байт) не помещается в зарезервированное место")]
    TooLarge(usize),
}

impl From<lopdf::Error> for SignError {
    fn from(e: lopdf::Error) -> Self {
        SignError::Structure(e.to_string())
    }
}

/// Сертификат с закрытым ключом (на токене или в реестре).
#[derive(Clone, Debug)]
pub struct CertInfo {
    /// SHA-1 отпечаток — по нему сертификат ищется при подписи.
    pub thumbprint: Vec<u8>,
    /// Владелец: «Фамилия Имя Отчество» или CN.
    pub owner: String,
    pub organization: String,
    pub issuer: String,
    /// Серийный номер, hex, старший байт первым.
    pub serial: String,
    pub valid_from: String,
    pub valid_to: String,
    /// Просрочен или ещё не начал действовать.
    pub expired: bool,
}

/// Текущее время: местное для штампа и UTC (FILETIME) для CMS.
#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub local: SignTime,
    pub utc_filetime: u64,
}

/// Местное время подписи с точностью до секунды.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Смещение местного времени от UTC, минуты.
    pub offset_min: i32,
}

impl SignTime {
    fn offset_parts(&self) -> (char, i32, i32) {
        let sign = if self.offset_min < 0 { '-' } else { '+' };
        let a = self.offset_min.abs();
        (sign, a / 60, a % 60)
    }

    /// Дата в формате PDF: `D:20261020153000+03'00'`.
    pub fn pdf_date(&self) -> String {
        let (s, h, m) = self.offset_parts();
        format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}{s}{h:02}'{m:02}'",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Для штампа: `20.10.2026 15:30:00 +03:00`.
    pub fn display(&self) -> String {
        let (s, h, m) = self.offset_parts();
        format!(
            "{:02}.{:02}.{:04} {:02}:{:02}:{:02} {s}{h:02}:{m:02}",
            self.day, self.month, self.year, self.hour, self.minute, self.second
        )
    }
}

#[cfg(test)]
mod tests {
    use super::SignTime;

    #[test]
    fn dates_format() {
        let t = SignTime { year: 2026, month: 10, day: 2, hour: 9, minute: 5, second: 7, offset_min: 180 };
        assert_eq!(t.pdf_date(), "D:20261002090507+03'00'");
        assert_eq!(t.display(), "02.10.2026 09:05:07 +03:00");
        let w = SignTime { offset_min: -330, ..t };
        assert_eq!(w.pdf_date(), "D:20261002090507-05'30'");
    }
}
