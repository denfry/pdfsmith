//! Манифест `latest.json`, который CI кладёт в каждый релиз.

use serde::Deserialize;

use crate::Error;

pub const MANIFEST_URL: &str = "https://github.com/denfry/pdfsmith/releases/latest/download/latest.json";
pub const ALLOWED_PREFIX: &str = "https://github.com/denfry/pdfsmith/releases/download/";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub url: String,
    pub sha256: String,
}

impl Manifest {
    /// Разбирает и проверяет манифест: ссылка только на релизы с `allowed_prefix`,
    /// `sha256` — 64 hex-символа (приводится к нижнему регистру), версия — semver.
    pub fn parse(json: &str, allowed_prefix: &str) -> Result<Manifest, Error> {
        let mut m: Manifest = serde_json::from_str(json).map_err(|e| Error::BadManifest(e.to_string()))?;
        m.sha256 = m.sha256.trim().to_ascii_lowercase();
        m.validate(allowed_prefix)?;
        Ok(m)
    }

    pub fn validate(&self, allowed_prefix: &str) -> Result<(), Error> {
        semver::Version::parse(&self.version)
            .map_err(|e| Error::BadManifest(format!("версия «{}»: {e}", self.version)))?;
        if !self.url.starts_with(allowed_prefix) || self.url.contains("..") {
            return Err(Error::BadManifest(format!("недопустимый адрес загрузки: {}", self.url)));
        }
        // Whitelist: ASCII letters, digits, and -._~/:
        if !self.url.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/' | b':')
        }) {
            return Err(Error::BadManifest(format!("недопустимый адрес загрузки: {}", self.url)));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::BadManifest("sha256 должен состоять из 64 hex-символов".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c42d1c1835420b6b9942dd4f1b";

    fn json(version: &str, url: &str, sha: &str) -> String {
        format!(r#"{{"version":"{version}","notes":"Что нового","url":"{url}","sha256":"{sha}"}}"#)
    }

    fn good_url() -> String {
        format!("{ALLOWED_PREFIX}v1.2.3/pdfsmith-setup.exe")
    }

    #[test]
    fn parses_valid_manifest() {
        let m = Manifest::parse(&json("1.2.3", &good_url(), HASH), ALLOWED_PREFIX).unwrap();
        assert_eq!(m.version, "1.2.3");
        assert_eq!(m.notes, "Что нового");
        assert_eq!(m.sha256, HASH);
    }

    #[test]
    fn uppercase_hash_is_normalized() {
        let m = Manifest::parse(&json("1.2.3", &good_url(), &HASH.to_uppercase()), ALLOWED_PREFIX).unwrap();
        assert_eq!(m.sha256, HASH);
    }

    #[test]
    fn notes_are_optional() {
        let text = format!(r#"{{"version":"1.2.3","url":"{}","sha256":"{HASH}"}}"#, good_url());
        assert_eq!(Manifest::parse(&text, ALLOWED_PREFIX).unwrap().notes, "");
    }

    #[test]
    fn rejects_foreign_urls() {
        for url in [
            "https://evil.example/pdfsmith-setup.exe",
            "https://github.com/denfry/pdfsmith-evil/releases/download/v1/x.exe",
            "http://github.com/denfry/pdfsmith/releases/download/v1/x.exe",
            "https://github.com/denfry/pdfsmith/releases/download/v1/../../../other/x.exe",
            "https://github.com/denfry/pdfsmith/releases/download/v1/%2e%2e/%2e%2e/other/x.exe",
            "https://github.com/denfry/pdfsmith/releases/download/v1/..\\..\\x.exe",
            "https://github.com/denfry/pdfsmith/releases/download/v1/x.exe?u=@evil",
            "https://github.com/denfry/pdfsmith/releases/download/v1/x y.exe",
        ] {
            let r = Manifest::parse(&json("1.2.3", url, HASH), ALLOWED_PREFIX);
            assert!(matches!(r, Err(Error::BadManifest(_))), "{url} должен быть отклонён");
        }
    }

    #[test]
    fn rejects_bad_hash_and_version() {
        assert!(matches!(Manifest::parse(&json("1.2.3", &good_url(), "abc"), ALLOWED_PREFIX), Err(Error::BadManifest(_))));
        let not_hex = "z".repeat(64);
        assert!(matches!(Manifest::parse(&json("1.2.3", &good_url(), &not_hex), ALLOWED_PREFIX), Err(Error::BadManifest(_))));
        assert!(matches!(Manifest::parse(&json("1.2", &good_url(), HASH), ALLOWED_PREFIX), Err(Error::BadManifest(_))));
    }

    #[test]
    fn rejects_garbage() {
        assert!(matches!(Manifest::parse("<html>404</html>", ALLOWED_PREFIX), Err(Error::BadManifest(_))));
    }
}
