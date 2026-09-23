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
