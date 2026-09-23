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

#[test]
fn rename_failure_leaves_no_part() {
    let (l, base) = bind();
    serve(l, vec![route("/setup.exe", body())]);
    let dir = tmpdir("rename_fail");
    // Create a directory at the target path so rename will fail
    let target_dir = download::installer_path(&dir, "9.9.9");
    std::fs::create_dir(&target_dir).unwrap();
    std::fs::write(target_dir.join("file_inside"), b"x").unwrap();

    let m = manifest(&base, "9.9.9", &body());
    let r = download::download(&download::agent(), &m, &dir, &AtomicBool::new(false), &mut |_, _| {});

    // Should get an Io error
    assert!(matches!(r, Err(Error::Io(_))), "{r:?}");

    // No .part file should be left
    let lefts = leftovers(&dir);
    assert!(!lefts.iter().any(|f| f.ends_with(".part")), "leftover .part file: {:?}", lefts);
}

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
