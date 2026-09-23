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

pub fn fetch_manifest(agent: &ureq::Agent, url: &str, allowed_prefix: &str) -> Result<Manifest, Error> {
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => Error::NoRelease,
        e => net(e),
    })?;
    let body = resp.into_string().map_err(net)?;
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
    if target.is_file() {
        if sha256_file(&target)? == m.sha256 {
            return Ok(target);
        }
        let _ = std::fs::remove_file(&target);
    }
    let part = target.with_extension("exe.part");
    let result = fetch_to(agent, m, &part, cancel, progress);
    match result {
        Ok(hash) if hash == m.sha256 => {
            match std::fs::rename(&part, &target) {
                Ok(()) => Ok(target),
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    Err(Error::Io(e))
                }
            }
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
