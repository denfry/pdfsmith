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
