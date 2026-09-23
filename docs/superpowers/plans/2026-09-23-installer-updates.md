# PDFsmith: установщик, обновления и «по умолчанию» — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Тег `vX.Y.Z` в GitHub → готовый `pdfsmith-setup.exe` в Releases; установленное приложение само находит новую версию, предлагает (или тихо ставит) обновление и предлагает стать программой по умолчанию для PDF.

**Architecture:** Установщик Inno Setup переносится из ветки `origin/feat/windows-installer` и дорабатывается (версия из CI, тихий режим, `/RELAUNCH`). Новый крейт `pdfsmith-update` без egui: манифест `latest.json`, решения, загрузка с SHA-256, запуск установщика, фоновый поток. В `pdfsmith-app` — настройки (`settings.rs`), UI обновлений (`updates.rs`), «по умолчанию» (`default_app.rs`), окно настроек (`settings_ui.rs`). GitHub Actions собирает и публикует релиз.

**Tech Stack:** Rust 1.80+ (eframe/egui 0.30, wgpu), `ureq` 2 (rustls), `serde`/`serde_json`, `semver`, `sha2`, `windows-sys` 0.59, `winreg`, Inno Setup 6, GitHub Actions (`windows-latest`), PowerShell 7.

**Spec:** `docs/superpowers/specs/2026-09-23-installer-updates-design.md`

## Global Constraints

- Только Windows 10/11 x64; установка per-user в `%LocalAppData%\PDFsmith`, без прав администратора.
- `AppId={{8F3B6A2C-1D4E-4C7A-9B12-A1B2C3D4E5F6}` не меняется.
- Манифест: `https://github.com/denfry/pdfsmith/releases/latest/download/latest.json`; допустимый префикс ссылки загрузки: `https://github.com/denfry/pdfsmith/releases/download/`.
- Интервал фоновой проверки — 12 часов. Таймауты HTTP: соединение 10 с, чтение 30 с.
- Настройки: `%APPDATA%\PDFsmith\settings.json`; по умолчанию `auto_update = false`, `ask_default_app = true`.
- Загрузки: `%LOCALAPPDATA%\PDFsmith\updates\pdfsmith-setup-<ver>.exe` (`.part` во время загрузки).
- Аргументы обновления: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART` + `/RELAUNCH` при перезапуске.
- ProgID: `PDFsmith.Document`. PDFium: `bblanchon/pdfium-binaries`, тег `chromium/7881`, `pdfium-win-x64.tgz`.
- Теги только `vX.Y.Z` (без суффиксов); версия тега = `workspace.package.version`.
- Тексты UI — на русском. Headless-тесты не ходят в сеть и не пишут в `%APPDATA%`.
- Коммиты: автор `denfry <dabinayo@pm.me>` (`git -c user.email=dabinayo@pm.me commit ...`), **без** `Co-Authored-By` и любых упоминаний ИИ.
- Нельзя инжектить клавиатуру/мышь в систему (пользователь работает за этим ПК) — ручные UI-проверки делает пользователь.
- Диск C почти полон: перед большими сборками `df -h /c`; при нехватке чистить только `target/debug`.

## Review Focus

- Открыто второе окно PDFsmith, когда первое закрывается с готовым обновлением → установщик **не** запускается (иначе `CloseApplications=force` убьёт окно с несохранённым документом); обновление ждёт следующего закрытия. Тесты: Task 6 (`other_instances_running` = false в одиночном процессе), Task 8 (`exit_action(true)` = `None`).
- Связь оборвалась посреди загрузки → ни `.exe`, ни `.part` не остаётся, повторная загрузка работает. Тест: Task 5 `truncated_download_leaves_nothing`.
- Повторное `Available` (ручная проверка) во время загрузки → вторая загрузка не стартует. Тест: Task 8 `duplicate_available_while_downloading_is_ignored`.
- `settings.json` нельзя записать (нет прав/папки) → приложение работает, настройки живут в памяти. Тест: Task 7 `save_failure_is_not_fatal`.
- Часы перевели назад (`last_check` в будущем) → проверка всё равно выполняется. Тест: Task 4 `clock_moved_back_triggers_check`.

---

## Файловая структура

- Из ветки: `installer/pdfsmith.iss`, `installer/README.md`, `crates/pdfsmith-app/build.rs`, `crates/pdfsmith-app/assets/pdfsmith.ico`, `assets/make_icon.py`.
- Изменить: `.gitignore` (`/dist`), корневой `Cargo.toml` (member + workspace-dep), `crates/pdfsmith-app/Cargo.toml`, `crates/pdfsmith-app/src/main.rs` (модули), `crates/pdfsmith-app/src/app.rs` (интеграция), `crates/pdfsmith-app/src/theme.rs` (`banner_frame`), `README.md`.
- Создать крейт `crates/pdfsmith-update/`: `Cargo.toml`, `src/lib.rs`, `src/error.rs`, `src/manifest.rs`, `src/decide.rs`, `src/download.rs`, `src/apply.rs`, `src/worker.rs`, `tests/http.rs`.
- Создать в app: `src/settings.rs`, `src/updates.rs`, `src/default_app.rs`, `src/settings_ui.rs`.
- Создать: `installer/fetch-pdfium.ps1`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`.

---

### Task 1: Перенести установщик из ветки в `main`

**Files:**
- Create (из ветки): `installer/pdfsmith.iss`, `installer/README.md`, `crates/pdfsmith-app/build.rs`, `crates/pdfsmith-app/assets/pdfsmith.ico`, `assets/make_icon.py`
- Modify: `crates/pdfsmith-app/Cargo.toml`, `.gitignore`

**Interfaces:**
- Produces: `target\release\pdfsmith.exe` с иконкой; `installer\pdfsmith.iss`, собирающийся в `dist\pdfsmith-setup.exe`.

- [ ] **Step 1: Забрать файлы из ветки**

```powershell
git fetch origin
git checkout origin/feat/windows-installer -- installer assets crates/pdfsmith-app/build.rs crates/pdfsmith-app/assets/pdfsmith.ico
```

- [ ] **Step 2: Добавить build-зависимость**

В конец `crates/pdfsmith-app/Cargo.toml`:

```toml

[build-dependencies]
winresource = "0.1"
```

(`[[bin]] name = "pdfsmith"` в `main` уже есть.)

- [ ] **Step 3: Игнорировать `dist/`**

В `.gitignore` перед блоком `# Логи и временное`:

```
# Выходная папка установщика
/dist

```

- [ ] **Step 4: Собрать и проверить**

```powershell
cargo build --release --bin pdfsmith
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" installer\pdfsmith.iss
Test-Path dist\pdfsmith-setup.exe
```

Expected: сборка успешна (предупреждение про `rc.exe` допустимо), ISCC `Successful compile`, `True`.

- [ ] **Step 5: Commit**

```powershell
git add installer assets crates/pdfsmith-app/build.rs crates/pdfsmith-app/assets/pdfsmith.ico crates/pdfsmith-app/Cargo.toml Cargo.lock .gitignore
git -c user.email=dabinayo@pm.me commit -m "build: установщик Inno Setup из ветки feat/windows-installer"
```

---

### Task 2: Доработать `pdfsmith.iss` для обновлений

**Files:**
- Modify: `installer/pdfsmith.iss`

**Interfaces:**
- Consumes: `installer/pdfsmith.iss` из Task 1.
- Produces: `ISCC /DAppVersion=X.Y.Z`; поддержка `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/RELAUNCH]`.

- [ ] **Step 1: Версия снаружи**

Заменить строку `#define AppVersion "0.1.0"` на:

```
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
```

- [ ] **Step 2: Секция `[Setup]`**

После `AppVersion={#AppVersion}` добавить:

```
VersionInfoVersion={#AppVersion}
CloseApplications=force
RestartApplications=no
```

- [ ] **Step 3: `[Run]` и `[UninstallDelete]`**

Перед `[Code]` вставить:

```
[Run]
Filename: "{app}\pdfsmith.exe"; Description: "Запустить PDFsmith"; Flags: nowait postinstall skipifsilent
Filename: "{app}\pdfsmith.exe"; Flags: nowait skipifnotsilent; Check: ShouldRelaunch

[UninstallDelete]
Type: filesandordirs; Name: "{app}\cache"
Type: filesandordirs; Name: "{app}\updates"

```

- [ ] **Step 4: `[Code]` — `ShouldRelaunch` и тихий режим**

Сразу после объявления `function SHOpenWithDialog(...)` добавить:

```pascal
// Перезапуск после тихого обновления из приложения: pdfsmith-setup.exe /VERYSILENT /RELAUNCH
function ShouldRelaunch: Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/RELAUNCH') = 0 then
      Result := True;
end;
```

И заменить условие в `CurStepChanged`:

```pascal
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('setdefault') and not WizardSilent then
```

- [ ] **Step 5: Собрать с версией и проверить**

```powershell
& "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe" /DAppVersion=0.1.1 installer\pdfsmith.iss
(Get-Item dist\pdfsmith-setup.exe).VersionInfo.FileVersion
```

Expected: `Successful compile`, версия `0.1.1.0`. Установку поверх рабочей копии здесь **не** запускать — это делается в Task 12.

- [ ] **Step 6: Commit**

```powershell
git add installer/pdfsmith.iss
git -c user.email=dabinayo@pm.me commit -m "installer: версия из CI, тихое обновление, /RELAUNCH, чистка кэша при удалении"
```

---

### Task 3: Крейт `pdfsmith-update` — ошибки и манифест

**Files:**
- Create: `crates/pdfsmith-update/Cargo.toml`, `crates/pdfsmith-update/src/lib.rs`, `crates/pdfsmith-update/src/error.rs`, `crates/pdfsmith-update/src/manifest.rs`
- Modify: корневой `Cargo.toml`

**Interfaces:**
- Produces: `pdfsmith_update::Error` (`Network(String)`, `BadManifest(String)`, `HashMismatch`, `Io(std::io::Error)`, `Cancelled`); `pdfsmith_update::Manifest { version: String, notes: String, url: String, sha256: String }` (`Debug, Clone, PartialEq, Eq, Deserialize`); `Manifest::parse(json: &str, allowed_prefix: &str) -> Result<Manifest, Error>`; `Manifest::validate(&self, allowed_prefix: &str) -> Result<(), Error>`; константы `manifest::MANIFEST_URL`, `manifest::ALLOWED_PREFIX`.

- [ ] **Step 1: Workspace**

В корневом `Cargo.toml`: в `members` добавить `"crates/pdfsmith-update",`; в `[workspace.dependencies]` добавить `pdfsmith-update = { path = "crates/pdfsmith-update" }`.

- [ ] **Step 2: `crates/pdfsmith-update/Cargo.toml`**

```toml
[package]
name = "pdfsmith-update"
edition.workspace = true
version.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
thiserror.workspace = true
log.workspace = true
serde = { version = "1", features = ["derive"] }
serde_json = "1"
semver = "1"
sha2 = "0.10"
ureq = "2"

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.59", features = ["Win32_Foundation", "Win32_System_Diagnostics_ToolHelp"] }
```

- [ ] **Step 3: `src/lib.rs` и `src/error.rs`**

`src/lib.rs`:

```rust
//! Обновления PDFsmith через GitHub Releases: манифест `latest.json`,
//! решение «предлагать ли», загрузка с проверкой SHA-256, запуск установщика.

pub mod error;
pub mod manifest;

pub use error::Error;
pub use manifest::Manifest;
```

`src/error.rs`:

```rust
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
```

- [ ] **Step 4: Написать падающие тесты манифеста**

`src/manifest.rs` (пока только тесты и заглушка, чтобы компилировалось):

```rust
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
    pub fn parse(_json: &str, _allowed_prefix: &str) -> Result<Manifest, Error> {
        unimplemented!()
    }

    pub fn validate(&self, _allowed_prefix: &str) -> Result<(), Error> {
        unimplemented!()
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
```

- [ ] **Step 5: Убедиться, что тесты падают**

Run: `cargo test -p pdfsmith-update manifest`
Expected: FAIL (паника `not implemented`).

- [ ] **Step 6: Реализация**

Заменить тела `parse`/`validate`:

```rust
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
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::BadManifest("sha256 должен состоять из 64 hex-символов".into()));
        }
        Ok(())
    }
```

- [ ] **Step 7: Тесты проходят**

Run: `cargo test -p pdfsmith-update manifest`
Expected: 6 passed.

- [ ] **Step 8: Commit**

```powershell
git add Cargo.toml Cargo.lock crates/pdfsmith-update
git -c user.email=dabinayo@pm.me commit -m "feat(update): крейт pdfsmith-update — манифест latest.json и его проверка"
```

---

### Task 4: Решения — «новее ли», «пора ли проверять», «пропущена ли»

**Files:**
- Create: `crates/pdfsmith-update/src/decide.rs`
- Modify: `crates/pdfsmith-update/src/lib.rs` (добавить `pub mod decide;`)

**Interfaces:**
- Produces: `decide::CHECK_INTERVAL_SECS: u64`, `decide::now_secs() -> u64`, `decide::should_check(last_check: Option<u64>, now: u64) -> bool`, `decide::is_newer(current: &str, candidate: &str) -> bool`, `decide::should_offer(current: &str, candidate: &str, skipped: Option<&str>, manual: bool) -> bool`.

- [ ] **Step 1: Падающие тесты**

`src/decide.rs`:

```rust
//! Чистые решения без ввода-вывода: когда проверять и что предлагать.

use std::time::{SystemTime, UNIX_EPOCH};

use semver::Version;

/// Фоновая проверка — не чаще раза в 12 часов.
pub const CHECK_INTERVAL_SECS: u64 = 12 * 60 * 60;

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn should_check(_last_check: Option<u64>, _now: u64) -> bool {
    unimplemented!()
}

pub fn is_newer(_current: &str, _candidate: &str) -> bool {
    unimplemented!()
}

pub fn should_offer(_current: &str, _candidate: &str, _skipped: Option<&str>, _manual: bool) -> bool {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_checks() {
        assert!(should_check(None, 1_000));
    }

    #[test]
    fn respects_interval_boundary() {
        let t = 1_000_000;
        assert!(!should_check(Some(t), t + CHECK_INTERVAL_SECS - 1));
        assert!(should_check(Some(t), t + CHECK_INTERVAL_SECS));
    }

    #[test]
    fn clock_moved_back_triggers_check() {
        assert!(should_check(Some(2_000_000), 1_000_000));
    }

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.1.0", "0.1.1"));
        assert!(is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("1.2.0", "1.1.9"));
        assert!(!is_newer("1.0.0", "мусор"));
        assert!(!is_newer("мусор", "1.0.0"));
    }

    #[test]
    fn prereleases_only_for_prerelease_users() {
        assert!(!is_newer("1.0.0", "1.1.0-beta.1"));
        assert!(is_newer("1.1.0-beta.1", "1.1.0-beta.2"));
        assert!(is_newer("1.1.0-beta.2", "1.1.0"));
    }

    #[test]
    fn skipped_version_is_not_offered_but_newer_is() {
        assert!(!should_offer("1.0.0", "1.1.0", Some("1.1.0"), false));
        assert!(should_offer("1.0.0", "1.1.1", Some("1.1.0"), false));
        assert!(should_offer("1.0.0", "1.1.0", None, false));
    }

    #[test]
    fn manual_check_ignores_skip_but_not_version_order() {
        assert!(should_offer("1.0.0", "1.1.0", Some("1.1.0"), true));
        assert!(!should_offer("1.1.0", "1.0.0", None, true));
    }
}
```

- [ ] **Step 2: Тесты падают**

Добавить `pub mod decide;` в `src/lib.rs` (после `pub mod error;`).
Run: `cargo test -p pdfsmith-update decide`
Expected: FAIL (`not implemented`).

- [ ] **Step 3: Реализация**

```rust
pub fn should_check(last_check: Option<u64>, now: u64) -> bool {
    match last_check {
        None => true,
        // Часы перевели назад — не ждём «будущего», проверяем.
        Some(t) if t > now => true,
        Some(t) => now - t >= CHECK_INTERVAL_SECS,
    }
}

/// `candidate` строго новее `current` по semver. Пре-релизы предлагаются
/// только тем, кто сам сидит на пре-релизе. Невалидные версии — `false`.
pub fn is_newer(current: &str, candidate: &str) -> bool {
    let (Ok(cur), Ok(cand)) = (Version::parse(current), Version::parse(candidate)) else {
        return false;
    };
    if !cand.pre.is_empty() && cur.pre.is_empty() {
        return false;
    }
    cand > cur
}

/// Предлагать ли версию: новее текущей и не пропущена пользователем
/// (ручная проверка пропуск игнорирует).
pub fn should_offer(current: &str, candidate: &str, skipped: Option<&str>, manual: bool) -> bool {
    is_newer(current, candidate) && (manual || skipped != Some(candidate))
}
```

- [ ] **Step 4: Тесты проходят**

Run: `cargo test -p pdfsmith-update decide`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```powershell
git add crates/pdfsmith-update
git -c user.email=dabinayo@pm.me commit -m "feat(update): решения — сравнение версий, интервал проверки, пропуск версии"
```

---

### Task 5: Загрузка с проверкой SHA-256 и очистка старых установщиков

**Files:**
- Create: `crates/pdfsmith-update/src/download.rs`, `crates/pdfsmith-update/tests/http.rs`
- Modify: `crates/pdfsmith-update/src/lib.rs` (добавить `pub mod download;`)

**Interfaces:**
- Consumes: `Manifest`, `Error`, `decide::is_newer`.
- Produces: `download::updates_dir() -> Option<PathBuf>`, `download::installer_path(dir: &Path, version: &str) -> PathBuf`, `download::agent() -> ureq::Agent`, `download::fetch_manifest(agent: &ureq::Agent, url: &str, allowed_prefix: &str) -> Result<Manifest, Error>`, `download::sha256_hex(data: &[u8]) -> String`, `download::sha256_file(path: &Path) -> Result<String, Error>`, `download::download(agent: &ureq::Agent, m: &Manifest, dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<PathBuf, Error>`, `download::cleanup(dir: &Path, current: &str)`.

- [ ] **Step 1: Модуль с заглушками**

`src/download.rs`:

```rust
//! Загрузка установщика: потоковая запись в `.part`, SHA-256 на лету,
//! переименование только после совпадения хэша.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{decide, Error, Manifest};

/// `%LOCALAPPDATA%\PDFsmith\updates`.
pub fn updates_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("PDFsmith").join("updates"))
}

pub fn installer_path(dir: &Path, version: &str) -> PathBuf {
    dir.join(format!("pdfsmith-setup-{version}.exe"))
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .build()
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    to_hex(&Sha256::digest(data))
}

pub fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

pub fn fetch_manifest(_agent: &ureq::Agent, _url: &str, _allowed_prefix: &str) -> Result<Manifest, Error> {
    unimplemented!()
}

pub fn download(
    _agent: &ureq::Agent,
    _m: &Manifest,
    _dir: &Path,
    _cancel: &AtomicBool,
    _progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    unimplemented!()
}

pub fn cleanup(_dir: &Path, _current: &str) {
    unimplemented!()
}
```

Добавить `pub mod download;` в `src/lib.rs`.

- [ ] **Step 2: Интеграционные тесты с локальным HTTP-сервером**

`tests/http.rs`:

```rust
//! Загрузка и поток обновлений против локального HTTP-сервера (без интернета).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use pdfsmith_update::download::{self, sha256_hex};
use pdfsmith_update::{Error, Manifest};

struct Route {
    path: &'static str,
    body: Vec<u8>,
    /// Объявленная длина больше тела = обрыв связи посреди ответа.
    declared_len: Option<usize>,
}

fn route(path: &'static str, body: impl Into<Vec<u8>>) -> Route {
    Route { path, body: body.into(), declared_len: None }
}

fn bind() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

fn serve(listener: TcpListener, routes: Vec<Route>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                    break;
                }
            }
            match routes.iter().find(|r| r.path == path) {
                Some(r) => {
                    let len = r.declared_len.unwrap_or(r.body.len());
                    let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n");
                    let _ = stream.write_all(&r.body);
                }
                None => {
                    let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        }
    });
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfsmith-upd-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn body() -> Vec<u8> {
    b"MZ fake installer ".repeat(10_000)
}

fn manifest(base: &str, version: &str, data: &[u8]) -> Manifest {
    Manifest { version: version.into(), notes: String::new(), url: format!("{base}/setup.exe"), sha256: sha256_hex(data) }
}

fn leftovers(dir: &PathBuf) -> Vec<String> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect()
}

#[test]
fn downloads_and_verifies() {
    let (l, base) = bind();
    serve(l, vec![route("/setup.exe", body())]);
    let dir = tmpdir("ok");
    let m = manifest(&base, "9.9.9", &body());
    let mut last = (0, None);
    let path = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |d, t| last = (d, t)).unwrap();
    assert_eq!(path, download::installer_path(&dir, "9.9.9"));
    assert_eq!(std::fs::read(&path).unwrap(), body());
    assert_eq!(last, (body().len() as u64, Some(body().len() as u64)));
    assert_eq!(leftovers(&dir), vec!["pdfsmith-setup-9.9.9.exe".to_string()]);
}

#[test]
fn hash_mismatch_leaves_nothing() {
    let (l, base) = bind();
    serve(l, vec![route("/setup.exe", body())]);
    let dir = tmpdir("mismatch");
    let m = manifest(&base, "9.9.9", b"other content");
    let r = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |_, _| {});
    assert!(matches!(r, Err(Error::HashMismatch)), "{r:?}");
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn truncated_download_leaves_nothing() {
    let (l, base) = bind();
    let data = body();
    serve(l, vec![Route { path: "/setup.exe", body: data[..1000].to_vec(), declared_len: Some(data.len()) }]);
    let dir = tmpdir("truncated");
    let m = manifest(&base, "9.9.9", &data);
    let r = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |_, _| {});
    assert!(r.is_err());
    assert!(leftovers(&dir).is_empty(), "остались файлы: {:?}", leftovers(&dir));
}

#[test]
fn verified_file_is_reused_without_network() {
    let (l, base) = bind();
    serve(l, vec![]); // любой запрос → 404
    let dir = tmpdir("reuse");
    std::fs::write(download::installer_path(&dir, "9.9.9"), body()).unwrap();
    let m = manifest(&base, "9.9.9", &body());
    let path = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), body());
}

#[test]
fn corrupt_cached_file_is_replaced() {
    let (l, base) = bind();
    serve(l, vec![route("/setup.exe", body())]);
    let dir = tmpdir("corrupt");
    std::fs::write(download::installer_path(&dir, "9.9.9"), b"broken").unwrap();
    let m = manifest(&base, "9.9.9", &body());
    let path = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), body());
}

#[test]
fn cancel_stops_and_cleans_up() {
    let (l, base) = bind();
    serve(l, vec![route("/setup.exe", body())]);
    let dir = tmpdir("cancel");
    let m = manifest(&base, "9.9.9", &body());
    let r = download::download(&download::agent(), &m, &dir, &AtomicBool::new(true), &mut |_, _| {});
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(leftovers(&dir).is_empty());
}

#[test]
fn fetch_manifest_ok_and_404() {
    let (l, base) = bind();
    let json = format!(r#"{{"version":"9.9.9","url":"{base}/setup.exe","sha256":"{}"}}"#, sha256_hex(&body()));
    serve(l, vec![route("/latest.json", json)]);
    let agent = download::agent();
    let prefix = format!("{base}/");
    let m = download::fetch_manifest(&agent, &format!("{base}/latest.json"), &prefix).unwrap();
    assert_eq!(m.version, "9.9.9");
    let r = download::fetch_manifest(&agent, &format!("{base}/missing.json"), &prefix);
    assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
}

#[test]
fn cleanup_removes_old_installers_only() {
    let dir = tmpdir("cleanup");
    for name in ["pdfsmith-setup-0.1.0.exe", "pdfsmith-setup-0.2.0.exe.part", "pdfsmith-setup-0.3.0.exe", "other.txt"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    download::cleanup(&dir, "0.2.0");
    let mut left = leftovers(&dir);
    left.sort();
    assert_eq!(left, vec!["other.txt".to_string(), "pdfsmith-setup-0.3.0.exe".to_string()]);
}
```

- [ ] **Step 3: Тесты падают**

Run: `cargo test -p pdfsmith-update --test http`
Expected: FAIL (`not implemented`).

- [ ] **Step 4: Реализация**

Заменить заглушки в `src/download.rs`:

```rust
pub fn fetch_manifest(agent: &ureq::Agent, url: &str, allowed_prefix: &str) -> Result<Manifest, Error> {
    let body = agent.get(url).call().map_err(net)?.into_string().map_err(net)?;
    Manifest::parse(&body, allowed_prefix)
}

/// Скачивает установщик из `m.url` в `dir`. Уже скачанный файл с верным
/// хэшем возвращается без сети. При любой ошибке `.part` удаляется.
pub fn download(
    agent: &ureq::Agent,
    m: &Manifest,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir)?;
    let target = installer_path(dir, &m.version);
    if target.exists() {
        if sha256_file(&target)? == m.sha256 {
            return Ok(target);
        }
        let _ = std::fs::remove_file(&target);
    }
    let part = target.with_extension("exe.part");
    let result = fetch_to(agent, m, &part, cancel, progress);
    match result {
        Ok(hash) if hash == m.sha256 => {
            std::fs::rename(&part, &target)?;
            Ok(target)
        }
        Ok(_) => {
            let _ = std::fs::remove_file(&part);
            Err(Error::HashMismatch)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Пишет ответ в `part`, возвращает SHA-256 записанного.
fn fetch_to(
    agent: &ureq::Agent,
    m: &Manifest,
    part: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<String, Error> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let resp = agent.get(&m.url).call().map_err(net)?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
    let mut reader = resp.into_reader();
    let mut file = File::create(part)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = reader.read(&mut buf).map_err(net)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    file.flush()?;
    if total.is_some_and(|t| t != done) {
        return Err(Error::Network(format!("получено {done} из {} байт", total.unwrap_or(0))));
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Удаляет из `dir` установщики (и недокачанные `.part`) версий не новее `current`.
pub fn cleanup(dir: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("pdfsmith-setup-") else { continue };
        let version = rest.trim_end_matches(".part").trim_end_matches(".exe");
        if !decide::is_newer(current, version) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
```

Важно: `File` в `fetch_to` закрывается при выходе из функции — до `remove_file`/`rename` в `download` (на Windows открытый файл не удалить).

- [ ] **Step 5: Тесты проходят**

Run: `cargo test -p pdfsmith-update --test http`
Expected: 8 passed. Если `ureq` 2 не отдаёт ошибку на обрыве — сработает проверка `total != done` (тест `truncated_download_leaves_nothing`).

- [ ] **Step 6: Commit**

```powershell
git add crates/pdfsmith-update
git -c user.email=dabinayo@pm.me commit -m "feat(update): загрузка установщика с проверкой SHA-256, повторное использование и очистка"
```

---

### Task 6: Запуск установщика, другие окна, фоновый поток

**Files:**
- Create: `crates/pdfsmith-update/src/apply.rs`, `crates/pdfsmith-update/src/worker.rs`
- Modify: `crates/pdfsmith-update/src/lib.rs`, `crates/pdfsmith-update/tests/http.rs`

**Interfaces:**
- Consumes: `download::*`, `decide::is_newer`, `manifest::{MANIFEST_URL, ALLOWED_PREFIX}`.
- Produces:
  - `apply::installer_args(relaunch: bool) -> Vec<&'static str>`, `apply::launch_installer(path: &Path, relaunch: bool) -> std::io::Result<()>`, `apply::other_instances_running() -> bool`.
  - `Config { current_version: String, manifest_url: String, allowed_prefix: String, updates_dir: PathBuf }`, `Config::github(current_version: &str) -> Option<Config>`.
  - `#[derive(Debug, Clone)] enum Command { Check { quiet: bool }, Download { manifest: Manifest, quiet: bool } }`.
  - `#[derive(Debug, Clone)] enum UpdateEvent { Available { manifest: Manifest, quiet: bool }, UpToDate { quiet: bool }, Progress { done: u64, total: Option<u64> }, Ready { manifest: Manifest, path: PathBuf }, Failed { quiet: bool, error: String, cancelled: bool } }`.
  - `struct UpdaterHandle { pub cmd_tx: Sender<Command>, pub event_rx: Receiver<UpdateEvent>, pub cancel: Arc<AtomicBool> }`.
  - `spawn(cfg: Config, wake: Box<dyn Fn() + Send>) -> UpdaterHandle`.
  - Реэкспорт в корне: `pdfsmith_update::{spawn, Command, Config, UpdateEvent, UpdaterHandle}`.

- [ ] **Step 1: `src/apply.rs` с тестами**

```rust
//! Запуск скачанного установщика и проверка других окон PDFsmith.

use std::path::Path;

pub fn installer_args(relaunch: bool) -> Vec<&'static str> {
    let mut args = vec!["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"];
    if relaunch {
        args.push("/RELAUNCH");
    }
    args
}

/// Запускает установщик отдельным процессом; вызывающий сразу завершается.
pub fn launch_installer(path: &Path, relaunch: bool) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(path);
    cmd.args(installer_args(relaunch));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd.spawn().map(|_| ())
}

/// Запущены ли другие процессы с тем же именем exe. Установщик закрывает их
/// принудительно, поэтому при открытых окнах обновление откладывается.
#[cfg(windows)]
pub fn other_instances_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    let me = std::process::id();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .unwrap_or_else(|| "pdfsmith.exe".into());
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            if name == exe && entry.th32ProcessID != me {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(not(windows))]
pub fn other_instances_running() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_without_and_with_relaunch() {
        assert_eq!(installer_args(false), vec!["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
        assert_eq!(installer_args(true), vec!["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/RELAUNCH"]);
    }

    #[test]
    fn single_test_process_sees_no_other_instances() {
        assert!(!other_instances_running());
    }
}
```

- [ ] **Step 2: `src/worker.rs`**

```rust
//! Фоновый поток `updater`: команды от UI → сеть → события обратно.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::manifest::{ALLOWED_PREFIX, MANIFEST_URL};
use crate::{decide, download, Error, Manifest};

pub struct Config {
    pub current_version: String,
    pub manifest_url: String,
    pub allowed_prefix: String,
    pub updates_dir: PathBuf,
}

impl Config {
    /// Боевая конфигурация: релизы `denfry/pdfsmith`. `None`, если нет `%LOCALAPPDATA%`.
    pub fn github(current_version: &str) -> Option<Config> {
        Some(Config {
            current_version: current_version.into(),
            manifest_url: MANIFEST_URL.into(),
            allowed_prefix: ALLOWED_PREFIX.into(),
            updates_dir: download::updates_dir()?,
        })
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    /// `quiet` — фоновая проверка: ошибки пользователю не показываются.
    Check { quiet: bool },
    Download { manifest: Manifest, quiet: bool },
}

#[derive(Debug, Clone)]
pub enum UpdateEvent {
    Available { manifest: Manifest, quiet: bool },
    UpToDate { quiet: bool },
    Progress { done: u64, total: Option<u64> },
    Ready { manifest: Manifest, path: PathBuf },
    Failed { quiet: bool, error: String, cancelled: bool },
}

pub struct UpdaterHandle {
    pub cmd_tx: Sender<Command>,
    pub event_rx: Receiver<UpdateEvent>,
    /// Выставить `true`, чтобы прервать текущую загрузку.
    pub cancel: Arc<AtomicBool>,
}

/// Запускает поток. `wake` вызывается после каждого события (перерисовка UI).
pub fn spawn(cfg: Config, wake: Box<dyn Fn() + Send>) -> UpdaterHandle {
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, event_rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("updater".into())
        .spawn(move || run(cfg, cmd_rx, ev_tx, flag, wake))
        .expect("не удалось запустить поток обновлений");
    UpdaterHandle { cmd_tx, event_rx, cancel }
}

fn run(cfg: Config, cmd_rx: Receiver<Command>, ev_tx: Sender<UpdateEvent>, cancel: Arc<AtomicBool>, wake: Box<dyn Fn() + Send>) {
    download::cleanup(&cfg.updates_dir, &cfg.current_version);
    let agent = download::agent();
    let send = |ev: UpdateEvent| {
        let _ = ev_tx.send(ev);
        wake();
    };
    for cmd in cmd_rx {
        match cmd {
            Command::Check { quiet } => match download::fetch_manifest(&agent, &cfg.manifest_url, &cfg.allowed_prefix) {
                Ok(m) if decide::is_newer(&cfg.current_version, &m.version) => send(UpdateEvent::Available { manifest: m, quiet }),
                Ok(_) => send(UpdateEvent::UpToDate { quiet }),
                Err(e) => {
                    log::warn!("проверка обновлений: {e}");
                    send(UpdateEvent::Failed { quiet, error: e.to_string(), cancelled: false });
                }
            },
            Command::Download { manifest, quiet } => {
                cancel.store(false, Ordering::SeqCst);
                let mut last = Instant::now() - Duration::from_secs(1);
                let mut progress = |done: u64, total: Option<u64>| {
                    if last.elapsed() >= Duration::from_millis(100) || Some(done) == total {
                        last = Instant::now();
                        send(UpdateEvent::Progress { done, total });
                    }
                };
                match download::download(&agent, &manifest, &cfg.updates_dir, &cancel, &mut progress) {
                    Ok(path) => send(UpdateEvent::Ready { manifest, path }),
                    Err(e) => {
                        log::warn!("загрузка обновления {}: {e}", manifest.version);
                        send(UpdateEvent::Failed { quiet, cancelled: matches!(e, Error::Cancelled), error: e.to_string() });
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 3: Подключить модули**

`src/lib.rs` итоговый:

```rust
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
```

- [ ] **Step 4: Тесты потока в `tests/http.rs`**

Дописать в конец файла:

```rust
use std::time::Duration;

use pdfsmith_update::{Command, Config, UpdateEvent};

fn worker_config(base: &str, current: &str, dir: PathBuf) -> Config {
    Config {
        current_version: current.into(),
        manifest_url: format!("{base}/latest.json"),
        allowed_prefix: format!("{base}/"),
        updates_dir: dir,
    }
}

fn latest_json(base: &str) -> String {
    format!(r#"{{"version":"9.9.9","notes":"n","url":"{base}/setup.exe","sha256":"{}"}}"#, sha256_hex(&body()))
}

#[test]
fn worker_check_then_download() {
    let (l, base) = bind();
    serve(l, vec![route("/latest.json", latest_json(&base)), route("/setup.exe", body())]);
    let h = pdfsmith_update::spawn(worker_config(&base, "0.1.0", tmpdir("worker")), Box::new(|| {}));
    h.cmd_tx.send(Command::Check { quiet: false }).unwrap();
    let manifest = match h.event_rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        UpdateEvent::Available { manifest, quiet: false } => manifest,
        other => panic!("ожидали Available, получили {other:?}"),
    };
    h.cmd_tx.send(Command::Download { manifest, quiet: false }).unwrap();
    loop {
        match h.event_rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            UpdateEvent::Progress { .. } => continue,
            UpdateEvent::Ready { path, .. } => {
                assert_eq!(std::fs::read(path).unwrap(), body());
                break;
            }
            other => panic!("ожидали Ready, получили {other:?}"),
        }
    }
}

#[test]
fn worker_reports_up_to_date() {
    let (l, base) = bind();
    serve(l, vec![route("/latest.json", latest_json(&base))]);
    let h = pdfsmith_update::spawn(worker_config(&base, "9.9.9", tmpdir("uptodate")), Box::new(|| {}));
    h.cmd_tx.send(Command::Check { quiet: true }).unwrap();
    assert!(matches!(h.event_rx.recv_timeout(Duration::from_secs(10)).unwrap(), UpdateEvent::UpToDate { quiet: true }));
}

#[test]
fn worker_reports_network_failure() {
    let (l, base) = bind();
    serve(l, vec![]);
    let h = pdfsmith_update::spawn(worker_config(&base, "0.1.0", tmpdir("fail")), Box::new(|| {}));
    h.cmd_tx.send(Command::Check { quiet: true }).unwrap();
    match h.event_rx.recv_timeout(Duration::from_secs(10)).unwrap() {
        UpdateEvent::Failed { quiet: true, cancelled: false, .. } => {}
        other => panic!("ожидали Failed, получили {other:?}"),
    }
}
```

- [ ] **Step 5: Все тесты крейта проходят**

Run: `cargo test -p pdfsmith-update`
Expected: все passed (6 manifest + 7 decide + 2 apply + 11 http).

- [ ] **Step 6: Commit**

```powershell
git add crates/pdfsmith-update
git -c user.email=dabinayo@pm.me commit -m "feat(update): запуск установщика, проверка других окон, фоновый поток updater"
```

---

### Task 7: Настройки приложения

**Files:**
- Create: `crates/pdfsmith-app/src/settings.rs`
- Modify: `crates/pdfsmith-app/Cargo.toml`, `crates/pdfsmith-app/src/main.rs` (`mod settings;`)

**Interfaces:**
- Produces: `Settings { auto_update: bool, last_check: Option<u64>, skipped_version: Option<String>, ask_default_app: bool }` (`Default`: `false, None, None, true`); `Settings::load(&Path) -> Settings`; `Settings::save(&self, &Path) -> io::Result<()>`; `SettingsStore { pub data: Settings }` с `load_default()`, `in_memory()`, `at(PathBuf)`, `save(&self)`.

- [ ] **Step 1: Зависимости**

В `[dependencies]` `crates/pdfsmith-app/Cargo.toml`:

```toml
pdfsmith-update.workspace = true
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

В `main.rs` после `mod render_thread;` добавить `mod settings;`.

- [ ] **Step 2: Модуль с заглушками и тестами**

`crates/pdfsmith-app/src/settings.rs`:

```rust
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
    pub fn load(_path: &Path) -> Settings {
        unimplemented!()
    }

    pub fn save(&self, _path: &Path) -> std::io::Result<()> {
        unimplemented!()
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
```

- [ ] **Step 3: Тесты падают**

Run: `cargo test -p pdfsmith-app settings::`
Expected: FAIL (`not implemented`).

- [ ] **Step 4: Реализация**

```rust
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
```

- [ ] **Step 5: Тесты проходят**

Run: `cargo test -p pdfsmith-app settings::`
Expected: 5 passed. (Предупреждения `dead_code` для `SettingsStore` допустимы до Task 10.)

- [ ] **Step 6: Commit**

```powershell
git add crates/pdfsmith-app Cargo.lock
git -c user.email=dabinayo@pm.me commit -m "feat(app): настройки в %APPDATA%\PDFsmith\settings.json"
```

---

### Task 8: UI обновлений — состояние, плашка, выход

**Files:**
- Create: `crates/pdfsmith-app/src/updates.rs`
- Modify: `crates/pdfsmith-app/src/main.rs` (`mod updates;`)

**Interfaces:**
- Consumes: `SettingsStore` (Task 7); `pdfsmith_update::{decide, apply, Command, Config, Manifest, UpdateEvent, UpdaterHandle, spawn}` (Tasks 3–6); `theme::{ICON, ACCENT, DANGER, MUTED, icon_button}`.
- Produces: `CURRENT_VERSION: &str`; `enum UpdState { Idle, Available(Manifest), Downloading { manifest, done: u64, total: Option<u64> }, Ready { manifest, path: PathBuf } }`; `UpdateUi` с `pub state`, `pub checking`, `pub message: Option<(String, bool)>` и методами `disabled()`, `new(&egui::Context, &SettingsStore)`, `with_handle(Option<UpdaterHandle>)`, `enabled()`, `poll(&mut SettingsStore)`, `update_now()`, `cancel_download()`, `skip(&mut SettingsStore)`, `check_now()`, `set_auto_update(&mut SettingsStore, bool)`, `request_restart() -> bool`, `cancel_restart()`, `exit_action(others_running: bool) -> Option<(PathBuf, bool)>`, `banner_visible(&SettingsStore) -> bool`, `status_text(&SettingsStore) -> Option<String>`, `banner(&mut egui::Ui, &mut SettingsStore)`.

- [ ] **Step 1: Модуль**

В `main.rs` добавить `mod updates;`. Файл `crates/pdfsmith-app/src/updates.rs`:

```rust
//! Обновления в UI: плашка над документом, фоновые загрузки, решение при выходе.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;
use pdfsmith_update::{decide, Command, Config, Manifest, UpdateEvent, UpdaterHandle};

use crate::settings::SettingsStore;
use crate::theme::{self, icon_button};

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq)]
pub enum UpdState {
    Idle,
    Available(Manifest),
    Downloading { manifest: Manifest, done: u64, total: Option<u64> },
    Ready { manifest: Manifest, path: PathBuf },
}

pub struct UpdateUi {
    handle: Option<UpdaterHandle>,
    pub state: UpdState,
    /// Идёт ручная проверка из окна настроек.
    pub checking: bool,
    /// Последнее сообщение для пользователя: (текст, это ошибка).
    pub message: Option<(String, bool)>,
    banner_hidden: bool,
    show_notes: bool,
    restart_requested: bool,
}

impl UpdateUi {
    /// Без потока обновлений (тесты, нет `%LOCALAPPDATA%`).
    pub fn disabled() -> Self {
        Self::with_handle(None)
    }

    /// Боевой режим: поток `updater`, фоновая проверка при запуске (раз в 12 ч).
    pub fn new(ctx: &egui::Context, store: &SettingsStore) -> Self {
        let handle = Config::github(CURRENT_VERSION).map(|cfg| {
            let ctx = ctx.clone();
            pdfsmith_update::spawn(cfg, Box::new(move || ctx.request_repaint()))
        });
        let ui = Self::with_handle(handle);
        if decide::should_check(store.data.last_check, decide::now_secs()) {
            ui.send(Command::Check { quiet: true });
        }
        ui
    }

    pub fn with_handle(handle: Option<UpdaterHandle>) -> Self {
        UpdateUi {
            handle,
            state: UpdState::Idle,
            checking: false,
            message: None,
            banner_hidden: false,
            show_notes: false,
            restart_requested: false,
        }
    }

    pub fn enabled(&self) -> bool {
        self.handle.is_some()
    }

    fn send(&self, cmd: Command) {
        if let Some(h) = &self.handle {
            let _ = h.cmd_tx.send(cmd);
        }
    }

    pub fn poll(&mut self, store: &mut SettingsStore) {
        let events: Vec<UpdateEvent> = match &self.handle {
            Some(h) => h.event_rx.try_iter().collect(),
            None => return,
        };
        for ev in events {
            self.on_event(ev, store);
        }
    }

    fn on_event(&mut self, ev: UpdateEvent, store: &mut SettingsStore) {
        match ev {
            UpdateEvent::Available { manifest, quiet } => {
                self.checking = false;
                store.data.last_check = Some(decide::now_secs());
                store.save();
                if matches!(self.state, UpdState::Downloading { .. } | UpdState::Ready { .. }) {
                    return;
                }
                if !decide::should_offer(CURRENT_VERSION, &manifest.version, store.data.skipped_version.as_deref(), !quiet) {
                    return;
                }
                if !quiet {
                    self.message = Some((format!("Доступна версия {}", manifest.version), false));
                    self.banner_hidden = false;
                }
                if store.data.auto_update {
                    self.start_download(manifest, true);
                } else {
                    self.state = UpdState::Available(manifest);
                }
            }
            UpdateEvent::UpToDate { quiet } => {
                self.checking = false;
                store.data.last_check = Some(decide::now_secs());
                store.save();
                if !quiet {
                    self.message = Some(("У вас последняя версия".into(), false));
                }
            }
            UpdateEvent::Progress { done, total } => {
                if let UpdState::Downloading { done: d, total: t, .. } = &mut self.state {
                    *d = done;
                    *t = total;
                }
            }
            UpdateEvent::Ready { manifest, path } => self.state = UpdState::Ready { manifest, path },
            UpdateEvent::Failed { quiet, error, cancelled } => {
                self.checking = false;
                if let UpdState::Downloading { manifest, .. } = &self.state {
                    let m = manifest.clone();
                    self.state = if quiet { UpdState::Idle } else { UpdState::Available(m) };
                }
                if !quiet && !cancelled {
                    self.message = Some((error, true));
                }
            }
        }
    }

    fn start_download(&mut self, manifest: Manifest, quiet: bool) {
        self.state = UpdState::Downloading { manifest: manifest.clone(), done: 0, total: None };
        self.send(Command::Download { manifest, quiet });
    }

    pub fn update_now(&mut self) {
        if let UpdState::Available(m) = &self.state {
            let m = m.clone();
            self.message = None;
            self.start_download(m, false);
        }
    }

    pub fn cancel_download(&self) {
        if let Some(h) = &self.handle {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn skip(&mut self, store: &mut SettingsStore) {
        if let UpdState::Available(m) = &self.state {
            store.data.skipped_version = Some(m.version.clone());
            store.save();
            self.state = UpdState::Idle;
        }
    }

    pub fn check_now(&mut self) {
        if self.enabled() {
            self.checking = true;
            self.message = None;
            self.send(Command::Check { quiet: false });
        }
    }

    pub fn set_auto_update(&mut self, store: &mut SettingsStore, on: bool) {
        store.data.auto_update = on;
        store.save();
        if on {
            if let UpdState::Available(m) = &self.state {
                let m = m.clone();
                self.start_download(m, true);
            }
        }
    }

    pub fn request_restart(&mut self) -> bool {
        self.restart_requested = matches!(self.state, UpdState::Ready { .. });
        self.restart_requested
    }

    /// Пользователь нажал «Отмена» в диалоге несохранённых изменений.
    pub fn cancel_restart(&mut self) {
        self.restart_requested = false;
    }

    /// Что сделать при выходе: (установщик, перезапускать ли). При других
    /// открытых окнах PDFsmith — ничего: установщик закрыл бы их без спроса.
    pub fn exit_action(&self, others_running: bool) -> Option<(PathBuf, bool)> {
        match &self.state {
            UpdState::Ready { path, .. } if !others_running => Some((path.clone(), self.restart_requested)),
            _ => None,
        }
    }

    pub fn banner_visible(&self, store: &SettingsStore) -> bool {
        if self.banner_hidden {
            return false;
        }
        match &self.state {
            UpdState::Idle => false,
            UpdState::Available(_) => true,
            // В режиме автообновления загрузка тихая — только строка состояния.
            UpdState::Downloading { .. } | UpdState::Ready { .. } => !store.data.auto_update,
        }
    }

    pub fn status_text(&self, store: &SettingsStore) -> Option<String> {
        if !store.data.auto_update {
            return None;
        }
        match &self.state {
            UpdState::Downloading { manifest, .. } => Some(format!("Загрузка обновления {}…", manifest.version)),
            UpdState::Ready { manifest, .. } => Some(format!("Обновление {} будет установлено при закрытии", manifest.version)),
            _ => None,
        }
    }

    pub fn banner(&mut self, ui: &mut egui::Ui, store: &mut SettingsStore) {
        let state = self.state.clone();
        ui.horizontal(|ui| {
            ui.label(RichText::new(ph::ARROW_CIRCLE_UP).size(theme::ICON).color(theme::ACCENT));
            match &state {
                UpdState::Idle => {}
                UpdState::Available(m) => {
                    ui.label(format!("Доступна версия {}", m.version));
                    if !m.notes.trim().is_empty() && ui.link("Что нового").clicked() {
                        self.show_notes = !self.show_notes;
                    }
                    if ui.button("Обновить").clicked() {
                        self.update_now();
                    }
                    if ui.button("Пропустить эту версию").clicked() {
                        self.skip(store);
                    }
                }
                UpdState::Downloading { manifest, done, total } => {
                    ui.label(format!("Загрузка версии {}…", manifest.version));
                    match total.filter(|t| *t > 0) {
                        Some(t) => {
                            ui.add(egui::ProgressBar::new(*done as f32 / t as f32).desired_width(160.0).show_percentage());
                        }
                        None => {
                            ui.label(format!("{:.1} МБ", *done as f64 / 1_048_576.0));
                        }
                    }
                    if ui.button("Отмена").clicked() {
                        self.cancel_download();
                    }
                }
                UpdState::Ready { manifest, .. } => {
                    ui.label(format!("Версия {} загружена", manifest.version));
                    if ui.button("Перезапустить и установить").clicked() && self.request_restart() {
                        if pdfsmith_update::apply::other_instances_running() {
                            self.cancel_restart();
                            self.message = Some(("Закройте другие окна PDFsmith и нажмите ещё раз".into(), true));
                        } else {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
            }
            if let Some((msg, true)) = &self.message {
                ui.label(RichText::new(msg).color(theme::DANGER));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, ph::X, "Скрыть до следующего запуска", false).clicked() {
                    self.banner_hidden = true;
                }
            });
        });
        if self.show_notes {
            if let UpdState::Available(m) = &state {
                ui.label(RichText::new(&m.notes).color(theme::MUTED));
            }
        }
    }
}
```

- [ ] **Step 2: Тесты**

Дописать в конец `updates.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Arc;

    use pdfsmith_update::manifest::ALLOWED_PREFIX;

    use crate::settings::Settings;

    struct Rig {
        ui: UpdateUi,
        store: SettingsStore,
        ev: Sender<UpdateEvent>,
        cmds: Receiver<Command>,
        path: PathBuf,
    }

    impl Rig {
        fn push(&mut self, e: UpdateEvent) {
            self.ev.send(e).unwrap();
            self.ui.poll(&mut self.store);
        }
    }

    fn rig(name: &str) -> Rig {
        let dir = std::env::temp_dir().join(format!("pdfsmith-updui-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let (cmd_tx, cmds) = channel();
        let (ev, event_rx) = channel();
        let handle = UpdaterHandle { cmd_tx, event_rx, cancel: Arc::new(AtomicBool::new(false)) };
        Rig { ui: UpdateUi::with_handle(Some(handle)), store: SettingsStore::at(path.clone()), ev, cmds, path }
    }

    fn m(v: &str) -> Manifest {
        Manifest { version: v.into(), notes: "Исправления".into(), url: format!("{ALLOWED_PREFIX}v{v}/pdfsmith-setup.exe"), sha256: "a".repeat(64) }
    }

    fn setup_path() -> PathBuf {
        PathBuf::from(r"C:\tmp\pdfsmith-setup-999.0.0.exe")
    }

    #[test]
    fn quiet_check_shows_banner_and_records_check_time() {
        let mut r = rig("banner");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")));
        assert!(r.ui.banner_visible(&r.store));
        assert!(Settings::load(&r.path).last_check.is_some());
        assert!(r.cmds.try_recv().is_err(), "без автообновления ничего не качаем");
    }

    #[test]
    fn older_or_same_version_is_ignored() {
        let mut r = rig("older");
        r.push(UpdateEvent::Available { manifest: m("0.0.1"), quiet: false });
        r.push(UpdateEvent::Available { manifest: m(CURRENT_VERSION), quiet: false });
        assert_eq!(r.ui.state, UpdState::Idle);
    }

    #[test]
    fn skip_persists_and_newer_version_still_offered() {
        let mut r = rig("skip");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.skip(&mut r.store);
        assert_eq!(r.ui.state, UpdState::Idle);
        assert_eq!(Settings::load(&r.path).skipped_version.as_deref(), Some("999.0.0"));
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Idle, "пропущенная версия не предлагается");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: false });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")), "ручная проверка игнорирует пропуск");
        r.ui.state = UpdState::Idle;
        r.push(UpdateEvent::Available { manifest: m("999.0.1"), quiet: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.1")));
    }

    #[test]
    fn auto_update_downloads_quietly_and_installs_on_exit() {
        let mut r = rig("auto");
        r.ui.set_auto_update(&mut r.store, true);
        assert!(Settings::load(&r.path).auto_update);
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        match r.cmds.try_recv() {
            Ok(Command::Download { manifest, quiet }) => {
                assert_eq!(manifest.version, "999.0.0");
                assert!(quiet);
            }
            other => panic!("ожидали Download, получили {other:?}"),
        }
        assert!(!r.ui.banner_visible(&r.store));
        assert!(r.ui.status_text(&r.store).unwrap().contains("Загрузка"));
        r.push(UpdateEvent::Ready { manifest: m("999.0.0"), path: setup_path() });
        assert!(r.ui.status_text(&r.store).unwrap().contains("при закрытии"));
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
    }

    #[test]
    fn manual_update_restart_flow() {
        let mut r = rig("manual");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        assert!(matches!(r.cmds.try_recv(), Ok(Command::Download { quiet: false, .. })));
        r.push(UpdateEvent::Progress { done: 50, total: Some(100) });
        assert_eq!(r.ui.state, UpdState::Downloading { manifest: m("999.0.0"), done: 50, total: Some(100) });
        assert!(r.ui.banner_visible(&r.store));
        assert_eq!(r.ui.exit_action(false), None, "пока не скачано — ставить нечего");
        r.push(UpdateEvent::Ready { manifest: m("999.0.0"), path: setup_path() });
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
        assert!(r.ui.request_restart());
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), true)));
        assert_eq!(r.ui.exit_action(true), None, "другие окна открыты — откладываем");
        r.ui.cancel_restart();
        assert_eq!(r.ui.exit_action(false), Some((setup_path(), false)));
    }

    #[test]
    fn duplicate_available_while_downloading_is_ignored() {
        let mut r = rig("dup");
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        let _ = r.cmds.try_recv();
        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: false });
        assert!(matches!(r.ui.state, UpdState::Downloading { .. }));
        assert!(r.cmds.try_recv().is_err(), "вторая загрузка не должна стартовать");
    }

    #[test]
    fn failures_are_silent_in_background_and_visible_on_demand() {
        let mut r = rig("fail");
        r.push(UpdateEvent::Failed { quiet: true, error: "нет связи".into(), cancelled: false });
        assert_eq!(r.ui.message, None);

        r.ui.check_now();
        assert!(matches!(r.cmds.try_recv(), Ok(Command::Check { quiet: false })));
        assert!(r.ui.checking);
        r.push(UpdateEvent::Failed { quiet: false, error: "нет связи".into(), cancelled: false });
        assert!(!r.ui.checking);
        assert_eq!(r.ui.message, Some(("нет связи".into(), true)));

        r.push(UpdateEvent::Available { manifest: m("999.0.0"), quiet: true });
        r.ui.update_now();
        r.push(UpdateEvent::Failed { quiet: false, error: "повреждён".into(), cancelled: false });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")), "после ошибки можно повторить");
        assert_eq!(r.ui.message, Some(("повреждён".into(), true)));

        r.ui.update_now();
        r.push(UpdateEvent::Failed { quiet: false, error: "загрузка отменена".into(), cancelled: true });
        assert_eq!(r.ui.state, UpdState::Available(m("999.0.0")));
        assert_eq!(r.ui.message, None, "отмена — не ошибка");
    }

    #[test]
    fn manual_up_to_date_message() {
        let mut r = rig("uptodate");
        r.ui.check_now();
        r.push(UpdateEvent::UpToDate { quiet: false });
        assert_eq!(r.ui.message, Some(("У вас последняя версия".into(), false)));
    }

    #[test]
    fn banner_renders_in_every_state() {
        let mut r = rig("render");
        let ctx = egui::Context::default();
        for state in [
            UpdState::Available(m("999.0.0")),
            UpdState::Downloading { manifest: m("999.0.0"), done: 10, total: None },
            UpdState::Downloading { manifest: m("999.0.0"), done: 10, total: Some(100) },
            UpdState::Ready { manifest: m("999.0.0"), path: setup_path() },
        ] {
            r.ui.state = state;
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| r.ui.banner(ui, &mut r.store));
            });
        }
    }
}
```

- [ ] **Step 3: Тесты проходят**

Run: `cargo test -p pdfsmith-app updates::`
Expected: 9 passed. (Модуль написан целиком в Step 1: код — прямой перенос логики из спека §4; тесты фиксируют поведение. Если какой-то тест падает — исправлять код, не тест, сверяясь со спеком.)

- [ ] **Step 4: Commit**

```powershell
git add crates/pdfsmith-app/src/updates.rs crates/pdfsmith-app/src/main.rs
git -c user.email=dabinayo@pm.me commit -m "feat(app): состояние и плашка обновлений, решение при выходе"
```

---

### Task 9: «Приложение по умолчанию» для PDF

**Files:**
- Create: `crates/pdfsmith-app/src/default_app.rs`
- Modify: `crates/pdfsmith-app/Cargo.toml`, `crates/pdfsmith-app/src/main.rs` (`mod default_app;`)

**Interfaces:**
- Consumes: `SettingsStore` (Task 7), `theme`.
- Produces: `PROG_ID`, `DefaultStatus { installed: bool, is_default: bool }`, `should_prompt(DefaultStatus, ask: bool) -> bool`, `query() -> DefaultStatus`, `DefaultApp` с `disabled()`, `detect()`, `with_status(DefaultStatus)`, `pub status`, `poll()`, `picker_open() -> bool`, `open_picker(&egui::Context)`, `dont_ask(&mut SettingsStore)`, `banner_visible(ask: bool) -> bool`, `banner(&mut egui::Ui, &mut SettingsStore)`, `settings_section(&mut egui::Ui)`.

- [ ] **Step 1: Зависимости**

В `crates/pdfsmith-app/Cargo.toml`:

```toml

[target.'cfg(windows)'.dependencies]
winreg = "0.55"
windows-sys = { version = "0.59", features = ["Win32_Foundation", "Win32_UI_Shell", "Win32_System_Com"] }
```

(Секцию вставить перед `[build-dependencies]`.) В `main.rs` добавить `mod default_app;`.

- [ ] **Step 2: Модуль**

`crates/pdfsmith-app/src/default_app.rs`:

```rust
//! Приложение по умолчанию для `.pdf`: проверка через реестр HKCU и
//! системный диалог выбора (назначить программно Windows не позволяет).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, RichText};
use egui_phosphor::regular as ph;

use crate::settings::SettingsStore;
use crate::theme::{self, icon_button};

pub const PROG_ID: &str = "PDFsmith.Document";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultStatus {
    /// Установлено установщиком (есть ProgID в HKCU).
    pub installed: bool,
    pub is_default: bool,
}

pub fn should_prompt(status: DefaultStatus, ask: bool) -> bool {
    ask && status.installed && !status.is_default
}

#[cfg(windows)]
pub fn query() -> DefaultStatus {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let installed = hkcu.open_subkey(format!(r"Software\Classes\{PROG_ID}")).is_ok();
    let base = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.pdf";
    let prog = |key: &str| {
        hkcu.open_subkey(format!(r"{base}\{key}")).and_then(|k| k.get_value::<String, _>("ProgId")).ok()
    };
    // Новые сборки Windows 11 пишут выбор в UserChoiceLatest, он главнее.
    let current = prog("UserChoiceLatest").or_else(|| prog("UserChoice"));
    let is_default = current.is_some_and(|p| p.eq_ignore_ascii_case(PROG_ID));
    DefaultStatus { installed, is_default }
}

#[cfg(not(windows))]
pub fn query() -> DefaultStatus {
    DefaultStatus { installed: false, is_default: false }
}

/// Системный диалог «Чем открывать .pdf». `false` — диалог не открылся.
#[cfg(windows)]
fn show_picker() -> bool {
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows_sys::Win32::UI::Shell::{
        SHOpenWithDialog, OAIF_ALLOW_REGISTRATION, OAIF_FORCE_REGISTRATION, OAIF_REGISTER_EXT, OPENASINFO,
    };

    let class: Vec<u16> = ".pdf\0".encode_utf16().collect();
    let file: Vec<u16> = "\0".encode_utf16().collect();
    let info = OPENASINFO {
        pcszFile: file.as_ptr(),
        pcszClass: class.as_ptr(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_FORCE_REGISTRATION,
    };
    unsafe {
        CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        SHOpenWithDialog(std::ptr::null_mut(), &info) >= 0
    }
}

#[cfg(not(windows))]
fn show_picker() -> bool {
    false
}

fn open_settings_page() {
    let _ = std::process::Command::new("explorer.exe").arg("ms-settings:defaultapps").spawn();
}

pub struct DefaultApp {
    pub status: DefaultStatus,
    picker_done: Option<Arc<AtomicBool>>,
    hidden: bool,
}

impl DefaultApp {
    /// Не установлено → никаких предложений (тесты, портативный запуск).
    pub fn disabled() -> Self {
        Self::with_status(DefaultStatus { installed: false, is_default: false })
    }

    pub fn detect() -> Self {
        Self::with_status(query())
    }

    pub fn with_status(status: DefaultStatus) -> Self {
        DefaultApp { status, picker_done: None, hidden: false }
    }

    /// После закрытия системного диалога перечитываем реестр.
    pub fn poll(&mut self) {
        if self.picker_done.as_ref().is_some_and(|d| d.load(Ordering::Acquire)) {
            self.picker_done = None;
            self.status = query();
        }
    }

    pub fn picker_open(&self) -> bool {
        self.picker_done.is_some()
    }

    /// Диалог модальный — открываем в отдельном потоке, чтобы окно не замирало.
    pub fn open_picker(&mut self, ctx: &egui::Context) {
        if self.picker_open() {
            return;
        }
        let done = Arc::new(AtomicBool::new(false));
        self.picker_done = Some(done.clone());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            if !show_picker() {
                open_settings_page();
            }
            done.store(true, Ordering::Release);
            ctx.request_repaint();
        });
    }

    pub fn dont_ask(&mut self, store: &mut SettingsStore) {
        store.data.ask_default_app = false;
        store.save();
    }

    pub fn banner_visible(&self, ask: bool) -> bool {
        !self.hidden && should_prompt(self.status, ask)
    }

    pub fn banner(&mut self, ui: &mut egui::Ui, store: &mut SettingsStore) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(ph::FILE_PDF).size(theme::ICON).color(theme::ACCENT));
            ui.label("Сделать PDFsmith программой для PDF по умолчанию?");
            if ui.add_enabled(!self.picker_open(), egui::Button::new("Сделать")).clicked() {
                self.open_picker(ui.ctx());
            }
            if ui.button("Не спрашивать").clicked() {
                self.dont_ask(store);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, ph::X, "Скрыть до следующего запуска", false).clicked() {
                    self.hidden = true;
                }
            });
        });
    }

    pub fn settings_section(&mut self, ui: &mut egui::Ui) {
        if !self.status.installed {
            ui.label(RichText::new("Доступно после установки программы").color(theme::MUTED));
        } else if self.status.is_default {
            ui.label(RichText::new(format!("{}  PDFsmith — программа по умолчанию для PDF", ph::CHECK)).color(theme::OK));
        } else if ui.add_enabled(!self.picker_open(), egui::Button::new("Сделать по умолчанию")).clicked() {
            self.open_picker(ui.ctx());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_DEFAULT: DefaultStatus = DefaultStatus { installed: true, is_default: false };

    #[test]
    fn prompt_only_when_installed_not_default_and_allowed() {
        assert!(should_prompt(NOT_DEFAULT, true));
        assert!(!should_prompt(NOT_DEFAULT, false));
        assert!(!should_prompt(DefaultStatus { installed: true, is_default: true }, true));
        assert!(!should_prompt(DefaultStatus { installed: false, is_default: false }, true));
    }

    #[test]
    fn dont_ask_persists() {
        let dir = std::env::temp_dir().join(format!("pdfsmith-defapp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut store = SettingsStore::at(path.clone());
        let mut d = DefaultApp::with_status(NOT_DEFAULT);
        assert!(d.banner_visible(store.data.ask_default_app));
        d.dont_ask(&mut store);
        assert!(!d.banner_visible(store.data.ask_default_app));
        assert!(!crate::settings::Settings::load(&path).ask_default_app);
    }

    #[test]
    fn disabled_never_prompts() {
        assert!(!DefaultApp::disabled().banner_visible(true));
    }

    #[test]
    fn banner_and_section_render() {
        let mut store = SettingsStore::in_memory();
        let ctx = egui::Context::default();
        for status in [NOT_DEFAULT, DefaultStatus { installed: true, is_default: true }, DefaultStatus { installed: false, is_default: false }] {
            let mut d = DefaultApp::with_status(status);
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    d.banner(ui, &mut store);
                    d.settings_section(ui);
                });
            });
        }
    }
}
```

- [ ] **Step 3: Тесты и ручная проверка чтения реестра**

Run: `cargo test -p pdfsmith-app default_app::`
Expected: 4 passed.

Ручная проверка `query()` (у разработчика стоит 0.1.0, ProgID есть):

```powershell
reg query "HKCU\Software\Classes\PDFsmith.Document"
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.pdf\UserChoice" /v ProgId
```

Сопоставить с ожиданием `installed`/`is_default` — полностью проверяется в Task 12.

- [ ] **Step 4: Commit**

```powershell
git add crates/pdfsmith-app Cargo.lock
git -c user.email=dabinayo@pm.me commit -m "feat(app): проверка и предложение стать программой по умолчанию для PDF"
```

---

### Task 10: Встроить всё в окно — плашки, настройки, выход

**Files:**
- Create: `crates/pdfsmith-app/src/settings_ui.rs`
- Modify: `crates/pdfsmith-app/src/app.rs` (поля `ViewerApp` ~стр. 302–349, `new`/`with_context` ~354–410, `toolbar` правая часть ~1386–1391, `status_bar` ~1439, `close_window` ~1930, `impl eframe::App` ~1939, `frame` ~1947–1998), `crates/pdfsmith-app/src/theme.rs`, `crates/pdfsmith-app/src/main.rs` (`mod settings_ui;`)

**Interfaces:**
- Consumes: `SettingsStore`, `UpdateUi`, `UpdState`, `DefaultApp`, `pdfsmith_update::apply::{launch_installer, other_instances_running}`.
- Produces: `settings_ui::settings_window(ctx, open: &mut bool, store, upd, def)`; `theme::banner_frame() -> egui::Frame`.

- [ ] **Step 1: `theme::banner_frame`**

В конец `theme.rs`:

```rust
/// Рамка информационной плашки под панелью инструментов.
pub fn banner_frame() -> egui::Frame {
    egui::Frame::none().fill(ACCENT_DIM).inner_margin(egui::Margin::symmetric(8.0, 4.0))
}
```

- [ ] **Step 2: `settings_ui.rs`**

В `main.rs` добавить `mod settings_ui;`. Файл:

```rust
//! Окно «Настройки»: обновления и приложение по умолчанию.

use eframe::egui::{self, RichText};

use crate::default_app::DefaultApp;
use crate::settings::SettingsStore;
use crate::theme;
use crate::updates::{UpdateUi, CURRENT_VERSION};

pub fn settings_window(ctx: &egui::Context, open: &mut bool, store: &mut SettingsStore, upd: &mut UpdateUi, def: &mut DefaultApp) {
    if !*open {
        return;
    }
    let mut still_open = true;
    egui::Window::new("Настройки")
        .open(&mut still_open)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(RichText::new("Обновления").strong());
            let mut auto = store.data.auto_update;
            if ui
                .checkbox(&mut auto, "Автоматически устанавливать обновления")
                .on_hover_text("Скачивать в фоне и устанавливать при закрытии программы")
                .changed()
            {
                upd.set_auto_update(store, auto);
            }
            ui.label(RichText::new(format!("Текущая версия: {CURRENT_VERSION}")).color(theme::MUTED));
            ui.horizontal(|ui| {
                if ui.add_enabled(upd.enabled() && !upd.checking, egui::Button::new("Проверить сейчас")).clicked() {
                    upd.check_now();
                }
                if upd.checking {
                    ui.spinner();
                }
            });
            if let Some((msg, err)) = &upd.message {
                ui.label(RichText::new(msg).color(if *err { theme::DANGER } else { theme::OK }));
            }
            ui.separator();
            ui.label(RichText::new("Приложение по умолчанию").strong());
            def.settings_section(ui);
        });
    *open = still_open;
}
```

- [ ] **Step 3: Поля и конструкторы `ViewerApp`**

В `app.rs` импорты — добавить:

```rust
use crate::default_app::DefaultApp;
use crate::settings::SettingsStore;
use crate::updates::{UpdState, UpdateUi};
```

В `struct ViewerApp` после `cursor_pt: Option<(usize, f32, f32)>,`:

```rust
    // Настройки, обновления, «по умолчанию».
    store: SettingsStore,
    upd: UpdateUi,
    def_app: DefaultApp,
    show_settings: bool,
```

В `with_context` в литерал `ViewerApp { ... }` после `cursor_pt: None,`:

```rust
            store: SettingsStore::in_memory(),
            upd: UpdateUi::disabled(),
            def_app: DefaultApp::disabled(),
            show_settings: false,
```

`new` заменить на:

```rust
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let dll_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let mut app = Self::with_context(cc.egui_ctx.clone(), dll_dir, path);
        // Боевые настройки, сеть и реестр — только в настоящем окне, не в тестах.
        app.store = SettingsStore::load_default();
        app.upd = UpdateUi::new(&cc.egui_ctx, &app.store);
        app.def_app = DefaultApp::detect();
        app
    }
```

- [ ] **Step 4: Шестерёнка на панели и строка состояния**

В `toolbar`, в правой части сразу после блока кнопки `ph::INFO` (`self.show_help = !self.show_help; }`):

```rust
                if icon_button(ui, ph::GEAR, "Настройки", self.show_settings).clicked() {
                    self.show_settings = !self.show_settings;
                }
```

В `status_bar` после блока `if self.loading_page { ... }`:

```rust
            if let Some(text) = self.upd.status_text(&self.store) {
                ui.label(muted("·".into()));
                ui.label(muted(text));
            }
```

- [ ] **Step 5: `frame()` — опрос, плашки, окно настроек**

В `frame` после `self.poll_events(ctx);`:

```rust
        self.upd.poll(&mut self.store);
        self.def_app.poll();
```

Сразу после панели `TopBottomPanel::top("toolbar")...show(ctx, |ui| self.toolbar(ui));`:

```rust
        // Одна плашка за раз: обновление важнее предложения «по умолчанию».
        if self.upd.banner_visible(&self.store) {
            egui::TopBottomPanel::top("banner")
                .frame(theme::banner_frame())
                .show(ctx, |ui| self.upd.banner(ui, &mut self.store));
        } else if self.def_app.banner_visible(self.store.data.ask_default_app) {
            egui::TopBottomPanel::top("banner")
                .frame(theme::banner_frame())
                .show(ctx, |ui| self.def_app.banner(ui, &mut self.store));
        }
```

В конце `frame` после `self.close_window(ctx);`:

```rust
        crate::settings_ui::settings_window(ctx, &mut self.show_settings, &mut self.store, &mut self.upd, &mut self.def_app);
```

- [ ] **Step 6: Отмена перезапуска и установка при выходе**

В `close_window` ветку `Some(2) => self.close_dialog = false,` заменить на:

```rust
            Some(2) => {
                self.close_dialog = false;
                self.upd.cancel_restart();
            }
```

`impl eframe::App for ViewerApp` дополнить (фича `glow` у eframe выключена, поэтому сигнатура без `gl`):

```rust
    fn on_exit(&mut self) {
        let others = pdfsmith_update::apply::other_instances_running();
        match self.upd.exit_action(others) {
            Some((path, relaunch)) => {
                log::info!("запуск установщика {} (перезапуск: {relaunch})", path.display());
                if let Err(e) = pdfsmith_update::apply::launch_installer(&path, relaunch) {
                    log::error!("не удалось запустить установщик: {e}");
                }
            }
            None if others && matches!(self.upd.state, UpdState::Ready { .. }) => {
                log::info!("обновление отложено: открыты другие окна PDFsmith");
            }
            None => {}
        }
    }
```

- [ ] **Step 7: Сборка и все тесты**

```powershell
cargo build --release --bin pdfsmith
cargo test --workspace
```

Expected: сборка без ошибок и без новых предупреждений; все тесты passed, включая `app::tests::headless_open_scroll_zoom_search_edit_save_convert` (он использует `with_context` → настройки в памяти, без сети).

- [ ] **Step 8: Ручная проверка (делает пользователь, не инжектить ввод)**

Запустить `target\release\pdfsmith.exe`. Проверить: шестерёнка справа на панели открывает «Настройки»; версия `0.1.0`; «Проверить сейчас» пишет ошибку связи или «У вас последняя версия» (релизов ещё нет → ожидается ошибка 404 — это нормально до Task 12); переключатель автообновления сохраняется в `%APPDATA%\PDFsmith\settings.json` после перезапуска.

- [ ] **Step 9: Commit**

```powershell
git add crates/pdfsmith-app
git -c user.email=dabinayo@pm.me commit -m "feat(app): плашки обновления и «по умолчанию», окно настроек, установка при выходе"
```

---

### Task 11: CI и публикация релизов

**Files:**
- Create: `installer/fetch-pdfium.ps1`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`
- Modify: `installer/README.md`, `README.md`

**Interfaces:**
- Consumes: `installer\pdfsmith.iss` с `/DAppVersion` (Task 2); `pdfsmith-app` версии `workspace.package.version`.
- Produces: Release `vX.Y.Z` с `pdfsmith-setup.exe`, `pdfsmith-setup.exe.sha256`, `latest.json` в формате `Manifest` (Task 3).

- [ ] **Step 1: `installer/fetch-pdfium.ps1`**

```powershell
# Скачивает pdfium.dll (bblanchon/pdfium-binaries) в корень репозитория.
param([string]$Tag = "chromium/7881")
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$dll = Join-Path $root "pdfium.dll"
if (Test-Path $dll) { Write-Host "pdfium.dll уже есть: $dll"; exit 0 }
$url = "https://github.com/bblanchon/pdfium-binaries/releases/download/$([uri]::EscapeDataString($Tag))/pdfium-win-x64.tgz"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "pdfium-$([guid]::NewGuid())"
New-Item -ItemType Directory $tmp | Out-Null
try {
    Invoke-WebRequest $url -OutFile "$tmp\pdfium.tgz"
    tar -xzf "$tmp\pdfium.tgz" -C $tmp
    Copy-Item "$tmp\bin\pdfium.dll" $dll
    Write-Host "pdfium.dll ($Tag) → $dll"
} finally {
    Remove-Item -Recurse -Force $tmp
}
```

Проверка локально (DLL уже есть → скрипт выходит сразу): `pwsh installer\fetch-pdfium.ps1` → «pdfium.dll уже есть».

- [ ] **Step 2: `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  test:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: PDFium
        shell: pwsh
        run: ./installer/fetch-pdfium.ps1
      - name: Тесты
        run: cargo test --workspace
      - name: Сборка
        run: cargo build --release --bin pdfsmith
```

- [ ] **Step 3: `.github/workflows/release.yml`**

```yaml
name: Release

on:
  push:
    tags: ['v*']

permissions:
  contents: write

jobs:
  release:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - name: Проверка версии
        id: ver
        shell: pwsh
        run: |
          $tag = "${{ github.ref_name }}"
          if ($tag -notmatch '^v(\d+\.\d+\.\d+)$') { throw "Тег $tag не в формате vX.Y.Z" }
          $ver = $Matches[1]
          $meta = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
          $cargoVer = ($meta.packages | Where-Object name -eq 'pdfsmith-app').version
          if ($cargoVer -ne $ver) { throw "Тег $tag не совпадает с версией в Cargo.toml ($cargoVer)" }
          "version=$ver" >> $env:GITHUB_OUTPUT

      - name: PDFium
        shell: pwsh
        run: ./installer/fetch-pdfium.ps1

      - name: Тесты
        run: cargo test --workspace

      - name: Сборка
        run: cargo build --release --bin pdfsmith

      - name: Установщик
        shell: pwsh
        run: |
          $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
          if (-not (Test-Path $iscc)) { choco install innosetup -y --no-progress }
          & $iscc "/DAppVersion=${{ steps.ver.outputs.version }}" installer\pdfsmith.iss
          if ($LASTEXITCODE -ne 0) { throw "ISCC завершился с кодом $LASTEXITCODE" }

      - name: Манифест
        shell: pwsh
        run: |
          [Console]::OutputEncoding = [Text.Encoding]::UTF8
          $ver = "${{ steps.ver.outputs.version }}"
          $tag = "${{ github.ref_name }}"
          $hash = (Get-FileHash dist\pdfsmith-setup.exe -Algorithm SHA256).Hash.ToLower()
          "$hash  pdfsmith-setup.exe" | Out-File -Encoding ascii -NoNewline dist\pdfsmith-setup.exe.sha256
          git fetch --force origin "refs/tags/${tag}:refs/tags/${tag}"
          $notes = (git tag -l --format='%(contents)' $tag | Out-String).Trim()
          [ordered]@{
            version = $ver
            notes   = $notes
            url     = "https://github.com/${{ github.repository }}/releases/download/$tag/pdfsmith-setup.exe"
            sha256  = $hash
          } | ConvertTo-Json | Out-File -Encoding utf8NoBOM dist\latest.json
          $notes | Out-File -Encoding utf8NoBOM dist\notes.txt
          Get-Content dist\latest.json

      - name: Публикация
        shell: pwsh
        env:
          GH_TOKEN: ${{ github.token }}
        run: >
          gh release create "${{ github.ref_name }}"
          dist\pdfsmith-setup.exe dist\pdfsmith-setup.exe.sha256 dist\latest.json
          --title "PDFsmith ${{ steps.ver.outputs.version }}"
          --notes-file dist\notes.txt
```

- [ ] **Step 4: Документация**

В `installer/README.md` в конец:

````markdown
## Выпуск новой версии (автоматически через GitHub Actions)
1. Поднять `version` в `[workspace.package]` корневого `Cargo.toml`, `cargo build`
   (обновит `Cargo.lock`), закоммитить.
2. `git tag -a v1.2.3 -m "Что нового: ..."` — текст тега станет описанием релиза
   и показывается в приложении по ссылке «Что нового».
3. `git push --follow-tags`.
4. Workflow **Release** проверит версию, соберёт и опубликует Release с
   `pdfsmith-setup.exe`, `pdfsmith-setup.exe.sha256` и `latest.json`.

## Как приложение обновляется
- Раз в 12 часов (и по кнопке «Проверить сейчас» в настройках) читает
  `releases/latest/download/latest.json`.
- По умолчанию показывает плашку «Доступна версия …»; с включённым
  «Автоматически устанавливать обновления» качает в фоне и ставит при закрытии.
- Перед запуском установщика сверяет SHA-256. Установщик запускается тихо:
  `pdfsmith-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/RELAUNCH]`.
- Если открыты другие окна PDFsmith, установка откладывается до следующего закрытия.
````

В корневой `README.md` после раздела «Сборка» добавить:

````markdown
## Установка и обновления

Готовый установщик — `pdfsmith-setup.exe` в
[Releases](https://github.com/denfry/pdfsmith/releases/latest). Ставится без прав
администратора; обновления приходят из тех же Releases (см. `installer/README.md`).
````

- [ ] **Step 5: Commit**

```powershell
git add installer .github README.md
git -c user.email=dabinayo@pm.me commit -m "ci: сборка и тесты на push, публикация релиза с установщиком по тегу"
```

---

### Task 12: Сквозная проверка на настоящих релизах

Внешние действия (push в `origin`, теги, публичные релизы, установка поверх рабочей 0.1.0) — **каждый шаг с push/тегом/установкой подтвердить у пользователя перед выполнением.** UI-клики делает пользователь.

**Files:**
- Modify: корневой `Cargo.toml` (`version`), `Cargo.lock`

- [ ] **Step 1: Push `main` и зелёный CI** (подтвердить)

```powershell
git push origin main
gh run watch --exit-status (gh run list --workflow CI --limit 1 --json databaseId -q '.[0].databaseId')
```

Expected: CI зелёный. Если красный — чинить до перехода дальше.

- [ ] **Step 2: Релиз v0.1.1** (подтвердить)

В корневом `Cargo.toml` `version = "0.1.1"`, затем:

```powershell
cargo build --release --bin pdfsmith
git add Cargo.toml Cargo.lock
git -c user.email=dabinayo@pm.me commit -m "release: 0.1.1"
git tag -a v0.1.1 -m "Первый релиз с установщиком и обновлениями"
git push --follow-tags
gh run watch --exit-status (gh run list --workflow Release --limit 1 --json databaseId -q '.[0].databaseId')
gh release view v0.1.1 --json assets -q '.assets[].name'
Invoke-RestMethod https://github.com/denfry/pdfsmith/releases/latest/download/latest.json
```

Expected: три файла в релизе; `latest.json` с `version: 0.1.1`, `url` на `.../releases/download/v0.1.1/pdfsmith-setup.exe`.

- [ ] **Step 3: Установка 0.1.1 поверх 0.1.0** (делает пользователь)

Пользователь скачивает `pdfsmith-setup.exe` из релиза и ставит (SmartScreen: «Подробнее → Выполнить в любом случае»). Проверки:
- `reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{8F3B6A2C-1D4E-4C7A-9B12-A1B2C3D4E5F6}_is1" /v DisplayVersion` → `0.1.1` (одна запись, не две).
- Если PDFsmith не по умолчанию — при запуске видна плашка «Сделать PDFsmith программой для PDF по умолчанию?»; «Сделать» открывает системный выбор; после выбора плашка исчезает.

- [ ] **Step 4: Релиз v0.1.2 и обновление из приложения** (подтвердить)

Повторить Step 2 с `0.1.2` и тегом `v0.1.2 -m "Проверка обновления"`. Пользователь: в установленной 0.1.1 → «Настройки» → «Проверить сейчас» → плашка «Доступна версия 0.1.2» → «Что нового» показывает текст тега → «Обновить» → прогресс → «Перезапустить и установить» → программа закрывается и через несколько секунд открывается снова; в настройках версия `0.1.2`. Никаких окон установщика и диалога «по умолчанию» не появилось.

- [ ] **Step 5: Автообновление** (подтвердить)

Пользователь включает «Автоматически устанавливать обновления». Выпустить `v0.1.3` (как Step 2). Пользователь: «Проверить сейчас» → в строке состояния «Обновление 0.1.3 будет установлено при закрытии» → закрыть окно → через ~10 с запустить PDFsmith → версия `0.1.3`.

- [ ] **Step 6: Отказ при подмене и при втором окне**

- Второе окно: при готовом обновлении открыть два окна PDFsmith, закрыть одно → версия не меняется, в логе «обновление отложено»; закрыть второе → обновление встало.
- Подмена хэша покрыта тестом `hash_mismatch_leaves_nothing`; вручную не проверяем.

- [ ] **Step 7: Итог**

Отметить в спеке `Статус: реализовано`, закоммитить:

```powershell
git add docs/superpowers/specs/2026-09-23-installer-updates-design.md
git -c user.email=dabinayo@pm.me commit -m "docs: спек установщика и обновлений — реализовано"
git push
```
