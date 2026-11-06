//! Конвертация через установленный Microsoft Office (COM-автоматизация).
//!
//! Word, Excel и PowerPoint управляются скриптом PowerShell: он запускается
//! скрыто, пути получает через переменные окружения (никакой подстановки в текст
//! скрипта), а Office открывает документы только для чтения и без диалогов.
//!
//! - Word / Excel / PowerPoint → PDF: `ExportAsFixedFormat` / `SaveAs` самого Office —
//!   вёрстка один в один, как при «Сохранить как PDF».
//! - PDF → Word: Word сам распознаёт PDF (PDF Reflow) — редактируемый текст и таблицы.
//! - PDF → Excel делается без Office — см. `tables`.
//! - PDF → PowerPoint: каждая страница — слайд-картинка (картинки рендерит PDFium).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OfficeApp {
    Word,
    Excel,
    PowerPoint,
}

impl OfficeApp {
    fn prog_id(self) -> &'static str {
        match self {
            OfficeApp::Word => "Word.Application",
            OfficeApp::Excel => "Excel.Application",
            OfficeApp::PowerPoint => "PowerPoint.Application",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            OfficeApp::Word => "Word",
            OfficeApp::Excel => "Excel",
            OfficeApp::PowerPoint => "PowerPoint",
        }
    }
}

pub const WORD_EXTS: &[&str] = &["doc", "docx", "docm", "dotx", "rtf", "odt"];
pub const EXCEL_EXTS: &[&str] = &["xls", "xlsx", "xlsm", "xlsb", "ods", "csv"];
pub const POWERPOINT_EXTS: &[&str] = &["ppt", "pptx", "pptm", "pps", "ppsx", "odp"];
/// Все форматы, которые умеем превращать в PDF через Office.
pub const OFFICE_EXTS: &[&str] = &[
    "doc", "docx", "docm", "dotx", "rtf", "odt", "xls", "xlsx", "xlsm", "xlsb", "ods", "csv",
    "ppt", "pptx", "pptm", "pps", "ppsx", "odp",
];

/// Каким приложением Office открывать файл.
pub fn app_for(path: &Path) -> Option<OfficeApp> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let e = ext.as_str();
    if WORD_EXTS.contains(&e) {
        Some(OfficeApp::Word)
    } else if EXCEL_EXTS.contains(&e) {
        Some(OfficeApp::Excel)
    } else if POWERPOINT_EXTS.contains(&e) {
        Some(OfficeApp::PowerPoint)
    } else {
        None
    }
}

/// Установлено ли приложение Office (зарегистрирован его COM-сервер). Кэшируется.
pub fn available(app: OfficeApp) -> bool {
    static CACHE: OnceLock<[bool; 3]> = OnceLock::new();
    let c = CACHE.get_or_init(|| {
        [
            registered(OfficeApp::Word),
            registered(OfficeApp::Excel),
            registered(OfficeApp::PowerPoint),
        ]
    });
    c[app as usize]
}

#[cfg(windows)]
fn registered(app: OfficeApp) -> bool {
    winreg::RegKey::predef(winreg::enums::HKEY_CLASSES_ROOT)
        .open_subkey(format!("{}\\CLSID", app.prog_id()))
        .is_ok()
}

#[cfg(not(windows))]
fn registered(_: OfficeApp) -> bool {
    false
}

/// Сколько ждать Office: первый запуск Word на слабом ПК бывает долгим.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Общее начало скриптов: ошибки — исключения, вывод в UTF-8, сообщение об ошибке
/// строкой `ERR: …` (её показываем пользователю).
const PRELUDE: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$failed = $null
function Release($o) { if ($o) { try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($o) } catch {} } }
# Word при открытии PDF спрашивает «преобразовать в редактируемый документ?» — окно
# рисует отдельный процесс, и DisplayAlerts его не гасит. На время открытия ставим
# ту же настройку, что и флажок «Больше не показывать», затем возвращаем как было.
function Open-Pdf($w, $path) {
  $k = 'HKCU:\Software\Microsoft\Office\' + $w.Version + '\Word\Options'
  if (-not (Test-Path $k)) { New-Item -Path $k -Force | Out-Null }
  $old = (Get-ItemProperty -Path $k -Name DisableConvertPdfWarning -ErrorAction SilentlyContinue).DisableConvertPdfWarning
  Set-ItemProperty -Path $k -Name DisableConvertPdfWarning -Value 1 -Type DWord
  try { return $w.Documents.Open($path, $false, $true, $false) }
  finally {
    if ($null -eq $old) { Remove-ItemProperty -Path $k -Name DisableConvertPdfWarning -ErrorAction SilentlyContinue }
    else { Set-ItemProperty -Path $k -Name DisableConvertPdfWarning -Value $old -Type DWord }
  }
}
"#;

/// Конец скриптов: код возврата по `$failed`.
const EPILOGUE: &str = r#"
if ($failed) { [Console]::Out.WriteLine('ERR: ' + $failed); exit 1 }
[Console]::Out.WriteLine('OK')
"#;

// Все файлы открываются только для чтения: Office не создаёт рядом файл-владелец
// «~$…» и не спрашивает «файл занят». Пароль-заглушка: для защищённого файла Office
// сразу вернёт ошибку вместо окна ввода пароля (которое некому закрыть).
const WORD_TO_PDF: &str = r#"
$w = New-Object -ComObject Word.Application
try {
  $w.Visible = $false; $w.DisplayAlerts = 0
  $d = $w.Documents.Open($env:PDFSMITH_SRC, $false, $true, $false, 'pdfsmith-no-password')
  # PDF, без открытия, для печати, весь документ, закладки из заголовков.
  $d.ExportAsFixedFormat($env:PDFSMITH_OUT, 17, $false, 0, 0, 1, 1, 0, $true, $true, 1)
  $d.Close(0)
} catch { $failed = $_.Exception.Message }
finally { try { if ($w.Documents.Count -eq 0) { $w.Quit() } } catch {}; Release $w }
"#;

const EXCEL_TO_PDF: &str = r#"
$x = New-Object -ComObject Excel.Application
try {
  $x.Visible = $false; $x.DisplayAlerts = $false; $x.ScreenUpdating = $false; $x.AskToUpdateLinks = $false
  $wb = $x.Workbooks.Open($env:PDFSMITH_SRC, 0, $true, [Type]::Missing, 'pdfsmith-no-password')
  $wb.ExportAsFixedFormat(0, $env:PDFSMITH_OUT)
  $wb.Close($false)
} catch { $failed = $_.Exception.Message }
finally { try { if ($x.Workbooks.Count -eq 0) { $x.Quit() } } catch {}; Release $x }
"#;

// PowerPoint — одиночный сервер: подключаемся к уже открытому и закрываем его,
// только если других презентаций нет.
const POWERPOINT_TO_PDF: &str = r#"
$p = New-Object -ComObject PowerPoint.Application
try {
  $pr = $p.Presentations.Open($env:PDFSMITH_SRC, -1, 0, 0)
  $pr.SaveAs($env:PDFSMITH_OUT, 32)
  $pr.Close()
} catch { $failed = $_.Exception.Message }
finally { try { if ($p.Presentations.Count -eq 0) { $p.Quit() } } catch {}; Release $p }
"#;

const PDF_TO_WORD: &str = r#"
$w = New-Object -ComObject Word.Application
try {
  $w.Visible = $false; $w.DisplayAlerts = 0
  $d = Open-Pdf $w $env:PDFSMITH_SRC
  $d.SaveAs2($env:PDFSMITH_OUT, 16)
  $d.Close(0)
} catch { $failed = $_.Exception.Message }
finally { try { if ($w.Documents.Count -eq 0) { $w.Quit() } } catch {}; Release $w }
"#;

const IMAGES_TO_PPTX: &str = r#"
$W = [single]$env:PDFSMITH_W; $H = [single]$env:PDFSMITH_H
$p = New-Object -ComObject PowerPoint.Application
try {
  $pr = $p.Presentations.Add(0)
  $pr.PageSetup.SlideWidth = $W; $pr.PageSetup.SlideHeight = $H
  $i = 1
  foreach ($f in (Get-ChildItem -LiteralPath $env:PDFSMITH_TMP -Filter '*.png' | Sort-Object Name)) {
    $s = $pr.Slides.Add($i, 12)
    $pic = $s.Shapes.AddPicture($f.FullName, 0, -1, 0, 0)
    $pic.LockAspectRatio = -1
    $k = [Math]::Min($W / $pic.Width, $H / $pic.Height)
    $pic.Width = $pic.Width * $k
    $pic.Left = ($W - $pic.Width) / 2; $pic.Top = ($H - $pic.Height) / 2
    $i++
  }
  $pr.SaveAs($env:PDFSMITH_OUT, 24)
  $pr.Close()
} catch { $failed = $_.Exception.Message }
finally { try { if ($p.Presentations.Count -eq 0) { $p.Quit() } } catch {}; Release $p }
"#;

/// Документ Office → PDF средствами самого Office.
pub fn to_pdf(src: &Path, out: &Path) -> Result<(), String> {
    let app = app_for(src).ok_or_else(|| format!("{}: не документ Office", src.display()))?;
    require(app)?;
    let script = match app {
        OfficeApp::Word => WORD_TO_PDF,
        OfficeApp::Excel => EXCEL_TO_PDF,
        OfficeApp::PowerPoint => POWERPOINT_TO_PDF,
    };
    run(
        script,
        &[
            ("PDFSMITH_SRC", src.as_os_str()),
            ("PDFSMITH_OUT", out.as_os_str()),
        ],
    )?;
    expect_file(out)
}

/// PDF → редактируемый .docx (распознаёт Word).
pub fn pdf_to_word(src: &Path, out: &Path) -> Result<(), String> {
    require(OfficeApp::Word)?;
    run(
        PDF_TO_WORD,
        &[
            ("PDFSMITH_SRC", src.as_os_str()),
            ("PDFSMITH_OUT", out.as_os_str()),
        ],
    )?;
    expect_file(out)
}

/// PNG-страницы из `dir` (по имени) → .pptx, слайд размером `slide_pt`.
pub fn images_to_pptx(dir: &Path, slide_pt: (f32, f32), out: &Path) -> Result<(), String> {
    require(OfficeApp::PowerPoint)?;
    // PowerPoint принимает слайды от 1 до 56 дюймов.
    let (w, h) = (
        slide_pt.0.clamp(72.0, 4032.0).to_string(),
        slide_pt.1.clamp(72.0, 4032.0).to_string(),
    );
    run(
        IMAGES_TO_PPTX,
        &[
            ("PDFSMITH_TMP", dir.as_os_str()),
            ("PDFSMITH_OUT", out.as_os_str()),
            ("PDFSMITH_W", w.as_ref()),
            ("PDFSMITH_H", h.as_ref()),
        ],
    )?;
    expect_file(out)
}

fn require(app: OfficeApp) -> Result<(), String> {
    if available(app) {
        Ok(())
    } else {
        Err(format!(
            "Не найден Microsoft {} — для этой операции нужен установленный Office",
            app.name()
        ))
    }
}

fn expect_file(out: &Path) -> Result<(), String> {
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(format!("Office не создал файл {}", out.display())),
    }
}

/// Временная папка задания; удаляется вместе с содержимым.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Result<TempDir, String> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir()
            .join("PDFsmith")
            .join("office")
            .join(format!(
                "{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("временная папка {}: {e}", dir.display()))?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Запускает скрипт скрытым PowerShell; переменные — через окружение.
fn run(body: &str, env: &[(&str, &std::ffi::OsStr)]) -> Result<(), String> {
    let script = format!("{PRELUDE}{body}{EPILOGUE}");
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-EncodedCommand",
        &encode_command(&script),
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("не удалось запустить PowerShell: {e}"))?;
    let read = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = read(Box::new(child.stdout.take().expect("stdout")));
    let err = read(Box::new(child.stderr.take().expect("stderr")));
    let t0 = Instant::now();
    let started = std::time::SystemTime::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if t0.elapsed() > TIMEOUT => {
                let _ = child.kill();
                kill_stuck_office(started);
                return Err(
                    "Office не ответил за 5 минут — возможно, он ждёт ответа в своём окне".into(),
                );
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if let Some(msg) = out.lines().find_map(|l| l.strip_prefix("ERR: ")) {
        return Err(clean_message(msg));
    }
    if !status.success() {
        let msg = err
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("неизвестная ошибка");
        return Err(format!("PowerShell: {}", clean_message(msg)));
    }
    Ok(())
}

/// Office завис с невидимым окном: завершаем только экземпляры, запущенные
/// автоматизацией за время нашего задания (и их разборщик PDF), — иначе они
/// останутся висеть в памяти. Открытые пользователем окна Office не трогаем.
fn kill_stuck_office(since: std::time::SystemTime) {
    let unix_100ns = since.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(0);
    let filetime = (unix_100ns + 116_444_736_000_000_000).to_string();
    let script = r#"
$t = [DateTime]::FromFileTimeUtc([long]$env:PDFSMITH_SINCE).ToLocalTime().AddSeconds(-2)
Get-CimInstance Win32_Process | Where-Object {
  $_.CreationDate -ge $t -and (($_.Name -match '^(WINWORD|EXCEL|POWERPNT)\.EXE$' -and $_.CommandLine -match '/Automation') -or $_.Name -eq 'PDFREFLOW.EXE')
} | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
"#;
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encode_command(script)])
        .env("PDFSMITH_SINCE", filetime)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if let Ok(mut c) = cmd.spawn() {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(30) && matches!(c.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Сообщения COM бывают многострочными и с хвостом «(Exception from HRESULT…)».
fn clean_message(msg: &str) -> String {
    let first = msg
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(msg);
    let first = first
        .split(" (Exception from HRESULT")
        .next()
        .unwrap_or(first);
    first
        .split(" (Исключение из HRESULT")
        .next()
        .unwrap_or(first)
        .trim()
        .to_string()
}

/// Скрипт для `-EncodedCommand`: base64 от UTF-16LE.
fn encode_command(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        s.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn office_app_by_extension() {
        assert_eq!(app_for(Path::new("a.DOCX")), Some(OfficeApp::Word));
        assert_eq!(app_for(Path::new("b.xlsx")), Some(OfficeApp::Excel));
        assert_eq!(app_for(Path::new("c.pptx")), Some(OfficeApp::PowerPoint));
        assert_eq!(app_for(Path::new("d.pdf")), None);
        for e in OFFICE_EXTS {
            assert!(app_for(Path::new(&format!("x.{e}"))).is_some(), "{e}");
        }
    }

    #[test]
    fn com_error_message_is_trimmed() {
        assert_eq!(
            clean_message("Файл повреждён. (Exception from HRESULT: 0x800A1066)\n  at line 3"),
            "Файл повреждён."
        );
    }

    #[test]
    fn powershell_runs_encoded_script_with_unicode_env() {
        if !cfg!(windows) {
            return;
        }
        // Кириллица в пути и сообщении проходит без искажений.
        let r = run(
            "$failed = 'путь: ' + $env:PDFSMITH_SRC",
            &[(
                "PDFSMITH_SRC",
                std::ffi::OsStr::new(r"C:\Документы\отчёт.docx"),
            )],
        );
        assert_eq!(r, Err(r"путь: C:\Документы\отчёт.docx".to_string()));
        assert_eq!(run("$x = 1", &[]), Ok(()));
    }

    /// Живая проверка с Office: `PDFSMITH_OFFICE_TESTS=1`, нужен test_pdfs/text.pdf.
    #[test]
    fn office_roundtrip_live() {
        if std::env::var_os("PDFSMITH_OFFICE_TESTS").is_none() || !available(OfficeApp::Word) {
            eprintln!("ПРОПУСК: PDFSMITH_OFFICE_TESTS не задан или нет Word");
            return;
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let pdf = std::path::absolute(root.join("test_pdfs").join("text.pdf")).unwrap();
        let _serial = crate::render_thread::pdfium_test_lock();
        pdfsmith_pdfium::init(Some(&root)).unwrap();
        let tmp = TempDir::new().unwrap();
        let docx = tmp.0.join("отчёт.docx");
        pdf_to_word(&pdf, &docx).unwrap();
        let back = tmp.0.join("обратно.pdf");
        to_pdf(&docx, &back).unwrap();
        // Наша книга .xlsx (без Office) должна открываться в настоящем Excel.
        let xlsx = tmp.0.join("таблица.xlsx");
        let doc = pdfsmith_pdfium::Document::open(&pdf, None).unwrap();
        let rows = crate::tables::page_rows(&doc.load_page(0).unwrap().chars());
        crate::tables::write_xlsx(&xlsx, &rows).unwrap();
        let xpdf = tmp.0.join("таблица.pdf");
        to_pdf(&xlsx, &xpdf).unwrap();
        for f in [&docx, &back, &xlsx, &xpdf] {
            assert!(
                std::fs::metadata(f)
                    .map(|m| m.len() > 1000)
                    .unwrap_or(false),
                "{}",
                f.display()
            );
        }
        // PowerPoint: страница-картинка → слайд → обратно в PDF.
        if available(OfficeApp::PowerPoint) {
            let img = image::RgbaImage::from_pixel(400, 300, image::Rgba([40, 90, 200, 255]));
            img.save(tmp.0.join("page00001.png")).unwrap();
            let pptx = tmp.0.join("слайды.pptx");
            images_to_pptx(&tmp.0, (720.0, 540.0), &pptx).unwrap();
            let ppdf = tmp.0.join("слайды.pdf");
            to_pdf(&pptx, &ppdf).unwrap();
            assert!(std::fs::metadata(&ppdf).map(|m| m.len() > 1000).unwrap_or(false));
        }
        // Несуществующий файл — понятная ошибка, а не зависание.
        let err = to_pdf(&tmp.0.join("нет.docx"), &tmp.0.join("x.pdf")).unwrap_err();
        assert!(!err.is_empty());
    }
}
