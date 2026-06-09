# PDFsmith Windows Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Собрать per-user Windows-установщик (`setup.exe`) для PDFsmith, который ставит приложение, регистрирует ассоциацию `.pdf`, показывает программу в «Открыть с помощью» и даёт сделать её приложением по умолчанию в один клик.

**Architecture:** Бинарь переименовывается в `pdfsmith.exe`, в него вшивается иконка через `winresource`/`build.rs`. Иконка генерируется скриптом на Pillow. Установщик на Inno Setup кладёт `pdfsmith.exe` + `pdfium.dll` + `.ico` в `%LocalAppData%\PDFsmith`, пишет ключи реестра в HKCU и при выбранной галочке вызывает системный диалог `SHOpenWithDialog` для `.pdf`.

**Tech Stack:** Rust (eframe/egui), `winresource` (build-dep), Python + Pillow (генерация иконки), Inno Setup 6 (ISCC).

**Замечание о тестировании:** установщик, ключи реестра и GUI не покрываются юнит-тестами. Вместо них в каждом шаге — проверочные команды (сборка, `reg query`, валидация `.ico`) и ручной чек-лист открытия файла. Это осознанный выбор для данного типа работы.

**Спек:** `docs/superpowers/specs/2026-06-09-windows-installer-design.md`

---

## Файловая структура

- Изменить: `crates/pdfsmith-app/Cargo.toml` — `[[bin]] name = "pdfsmith"`, build-dep `winresource`.
- Создать: `crates/pdfsmith-app/build.rs` — вшивание иконки в exe (только под Windows).
- Создать: `assets/make_icon.py` — генератор иконки на Pillow.
- Создать: `crates/pdfsmith-app/assets/pdfsmith.ico` — иконка (генерируется скриптом).
- Создать: `installer/pdfsmith.iss` — скрипт Inno Setup (Setup/Files/Tasks/Icons/Registry/Code).
- Создать: `installer/README.md` — как собрать setup.exe.
- Создать: `dist/` — выходная папка для `pdfsmith-setup.exe` (создаётся ISCC; добавить в `.gitignore`).

---

## Task 1: Переименовать бинарь в `pdfsmith`

**Files:**
- Modify: `crates/pdfsmith-app/Cargo.toml`

- [ ] **Step 1: Добавить секцию `[[bin]]`**

В `crates/pdfsmith-app/Cargo.toml` после секции `[package]` (перед `[dependencies]`) добавить:

```toml
[[bin]]
name = "pdfsmith"
path = "src/main.rs"
```

- [ ] **Step 2: Собрать релиз и проверить имя бинаря**

Run (PowerShell):
```powershell
cargo build --release --bin pdfsmith
```
Expected: сборка успешна, появляется `target\release\pdfsmith.exe`.

Проверка:
```powershell
Test-Path target\release\pdfsmith.exe
```
Expected: `True`.

- [ ] **Step 3: Дымовой запуск с файлом**

Run:
```powershell
.\target\release\pdfsmith.exe .\test_pdfs\(любой).pdf
```
Expected: окно открывается, PDF отображается. Закрыть окно.

- [ ] **Step 4: Commit**

```powershell
git add crates/pdfsmith-app/Cargo.toml
git commit -m "build: переименовать бинарь в pdfsmith"
```

---

## Task 2: Сгенерировать иконку `pdfsmith.ico`

**Files:**
- Create: `assets/make_icon.py`
- Create: `crates/pdfsmith-app/assets/pdfsmith.ico` (вывод скрипта)

- [ ] **Step 1: Установить Pillow**

Run:
```powershell
python -m pip install --user pillow
```
Expected: `Successfully installed pillow-...` (или «already satisfied»).

- [ ] **Step 2: Написать генератор иконки**

Создать `assets/make_icon.py`:

```python
"""Генерирует pdfsmith.ico — лист документа с красной плашкой PDF."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

SIZE = 256
img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# Лист бумаги с загнутым уголком
margin, fold = 44, 64
page = [(margin, 24), (SIZE - margin - fold, 24), (SIZE - margin, 24 + fold),
        (SIZE - margin, SIZE - 24), (margin, SIZE - 24)]
d.polygon(page, fill=(245, 245, 247, 255), outline=(120, 120, 128, 255))
# Загнутый уголок
d.polygon([(SIZE - margin - fold, 24), (SIZE - margin - fold, 24 + fold),
           (SIZE - margin, 24 + fold)], fill=(210, 210, 214, 255))
# Красная плашка PDF
band_top, band_h = 150, 56
d.rectangle([margin, band_top, SIZE - margin, band_top + band_h],
            fill=(200, 42, 42, 255))
try:
    font = ImageFont.truetype("arialbd.ttf", 40)
except OSError:
    font = ImageFont.load_default()
d.text((SIZE // 2, band_top + band_h // 2), "PDF", font=font,
       fill=(255, 255, 255, 255), anchor="mm")

out = Path(__file__).resolve().parent.parent / "crates" / "pdfsmith-app" / "assets" / "pdfsmith.ico"
out.parent.mkdir(parents=True, exist_ok=True)
img.save(out, sizes=[(16, 16), (32, 32), (48, 48), (256, 256)])
print(f"wrote {out}")
```

- [ ] **Step 3: Запустить генератор**

Run:
```powershell
python assets\make_icon.py
```
Expected: `wrote ...\crates\pdfsmith-app\assets\pdfsmith.ico`.

- [ ] **Step 4: Проверить, что .ico валиден и многоразмерный**

Run:
```powershell
python -c "from PIL import Image; im=Image.open(r'crates/pdfsmith-app/assets/pdfsmith.ico'); print(sorted(im.info['sizes']))"
```
Expected: `[(16, 16), (32, 32), (48, 48), (256, 256)]`.

- [ ] **Step 5: Commit**

```powershell
git add assets/make_icon.py crates/pdfsmith-app/assets/pdfsmith.ico
git commit -m "assets: генератор и файл иконки pdfsmith.ico"
```

---

## Task 3: Вшить иконку в exe через `winresource`

**Files:**
- Modify: `crates/pdfsmith-app/Cargo.toml`
- Create: `crates/pdfsmith-app/build.rs`

- [ ] **Step 1: Добавить build-зависимость**

В `crates/pdfsmith-app/Cargo.toml` добавить секцию:

```toml
[build-dependencies]
winresource = "0.1"
```

- [ ] **Step 2: Написать build.rs**

Создать `crates/pdfsmith-app/build.rs`:

```rust
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/pdfsmith.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/pdfsmith.ico");
        res.compile().expect("failed to embed Windows icon");
    }
}
```

- [ ] **Step 3: Пересобрать релиз**

Run:
```powershell
cargo build --release --bin pdfsmith
```
Expected: сборка успешна. Если ошибка `cannot find rc.exe` — установить Windows SDK / VS Build Tools (компонент «Windows 10/11 SDK»), затем повторить.

- [ ] **Step 4: Проверить, что иконка вшита**

Run (PowerShell):
```powershell
Add-Type -AssemblyName System.Drawing
$i = [System.Drawing.Icon]::ExtractAssociatedIcon((Resolve-Path target\release\pdfsmith.exe))
if ($i -and $i.Width -gt 0) { "icon embedded: $($i.Width)x$($i.Height)" } else { "NO ICON" }
```
Expected: `icon embedded: 32x32` (значок ненулевого размера).

- [ ] **Step 5: Commit**

```powershell
git add crates/pdfsmith-app/Cargo.toml crates/pdfsmith-app/build.rs
git commit -m "build: вшить иконку в pdfsmith.exe через winresource"
```

---

## Task 4: Скрипт Inno Setup (файлы, ярлыки, реестр, задачи)

**Files:**
- Create: `installer/pdfsmith.iss`
- Modify: `.gitignore` (добавить `dist/`)

- [ ] **Step 1: Установить Inno Setup 6**

Run:
```powershell
winget install --id JRSoftware.InnoSetup -e --accept-source-agreements --accept-package-agreements
```
Expected: установка успешна. ISCC появляется по пути `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`.

Проверка:
```powershell
Test-Path "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
```
Expected: `True`.

- [ ] **Step 2: Написать .iss-скрипт (без блока [Code] — он в Task 5)**

Создать `installer/pdfsmith.iss`:

```iss
; PDFsmith — per-user установщик
#define AppName "PDFsmith"
#define AppVersion "0.1.0"
#define ProgId "PDFsmith.Document"

[Setup]
AppId={{8F3B6A2C-1D4E-4C7A-9B12-A1B2C3D4E5F6}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=PDFsmith
DefaultDirName={localappdata}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=pdfsmith-setup
SetupIconFile=..\crates\pdfsmith-app\assets\pdfsmith.ico
UninstallDisplayIcon={app}\pdfsmith.ico
WizardStyle=modern

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Files]
Source: "..\target\release\pdfsmith.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\pdfium.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\crates\pdfsmith-app\assets\pdfsmith.ico"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "desktopicon"; Description: "Создать ярлык на рабочем столе"; Flags: unchecked
Name: "setdefault"; Description: "Сделать PDFsmith приложением по умолчанию для PDF"

[Icons]
Name: "{group}\PDFsmith"; Filename: "{app}\pdfsmith.exe"; IconFilename: "{app}\pdfsmith.ico"
Name: "{userdesktop}\PDFsmith"; Filename: "{app}\pdfsmith.exe"; IconFilename: "{app}\pdfsmith.ico"; Tasks: desktopicon

[Registry]
; --- ProgID ---
Root: HKCU; Subkey: "Software\Classes\{#ProgId}"; ValueType: string; ValueData: "PDF-документ"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\{#ProgId}\DefaultIcon"; ValueType: string; ValueData: "{app}\pdfsmith.ico"
Root: HKCU; Subkey: "Software\Classes\{#ProgId}\shell\open\command"; ValueType: string; ValueData: """{app}\pdfsmith.exe"" ""%1"""
; --- Capabilities ---
Root: HKCU; Subkey: "Software\{#AppName}"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\{#AppName}\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"
Root: HKCU; Subkey: "Software\{#AppName}\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Просмотрщик PDF"
Root: HKCU; Subkey: "Software\{#AppName}\Capabilities\FileAssociations"; ValueType: string; ValueName: ".pdf"; ValueData: "{#ProgId}"
; --- RegisteredApplications ---
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "{#AppName}"; ValueData: "Software\{#AppName}\Capabilities"; Flags: uninsdeletevalue
; --- OpenWithProgids ---
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: none; ValueName: "{#ProgId}"; Flags: uninsdeletevalue
```

- [ ] **Step 3: Добавить dist/ в .gitignore**

Добавить строку в `.gitignore`:

```
/dist/
```

- [ ] **Step 4: Скомпилировать установщик**

Run:
```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\pdfsmith.iss
```
Expected: `Successful compile`, создан `dist\pdfsmith-setup.exe`.

Проверка:
```powershell
Test-Path dist\pdfsmith-setup.exe
```
Expected: `True`.

- [ ] **Step 5: Тестовая установка и проверка ключей реестра**

Run (тихая установка с задачей setdefault выключенной, чтобы не дёргать диалог):
```powershell
Start-Process dist\pdfsmith-setup.exe -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/TASKS=' -Wait
```
Проверка ключей:
```powershell
reg query "HKCU\Software\Classes\PDFsmith.Document\shell\open\command"
reg query "HKCU\Software\RegisteredApplications" /v PDFsmith
reg query "HKCU\Software\Classes\.pdf\OpenWithProgids" /v PDFsmith.Document
```
Expected: команда содержит `...\PDFsmith\pdfsmith.exe" "%1"`; значение PDFsmith = `Software\PDFsmith\Capabilities`; запись OpenWithProgids существует.

- [ ] **Step 6: Проверить «Открыть с помощью»**

Вручную: ПКМ по любому `.pdf` → «Открыть с помощью» → в списке есть **PDFsmith**. Клик по нему открывает файл, PDF отображается.

- [ ] **Step 7: Commit**

```powershell
git add installer/pdfsmith.iss .gitignore
git commit -m "installer: Inno Setup скрипт с ассоциацией .pdf (HKCU)"
```

---

## Task 5: Кнопка «сделать по умолчанию» через `SHOpenWithDialog`

**Files:**
- Modify: `installer/pdfsmith.iss` (добавить блок `[Code]`)

- [ ] **Step 1: Добавить блок [Code] в конец pdfsmith.iss**

Дописать в конец `installer/pdfsmith.iss`:

```iss
[Code]
const
  OAIF_ALLOW_REGISTRATION = $00000001;
  OAIF_REGISTER_EXT       = $00000002;
  OAIF_FORCE_REGISTRATION = $00000008;

type
  TOpenAsInfo = record
    pcszFile: WideString;
    pcszClass: WideString;
    oaifInFlags: Integer;
  end;

function SHOpenWithDialog(hwndParent: Integer; var poainfo: TOpenAsInfo): Integer;
  external 'SHOpenWithDialog@shell32.dll stdcall';

procedure CurStepChanged(CurStep: TSetupStep);
var
  Info: TOpenAsInfo;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('setdefault') then
  begin
    Info.pcszFile := '';
    Info.pcszClass := '.pdf';
    Info.oaifInFlags := OAIF_REGISTER_EXT or OAIF_ALLOW_REGISTRATION or OAIF_FORCE_REGISTRATION;
    try
      SHOpenWithDialog(0, Info);
    except
      // Фолбэк: открыть Параметры → Приложения по умолчанию
      ShellExec('open', 'ms-settings:defaultapps', '', '', SW_SHOW, ewNoWait, ErrorCode);
    end;
  end;
end;
```

Примечание: если компилятор ругается на `ErrorCode` как необъявленную — добавить `var ErrorCode: Integer;` в секцию `var` процедуры `CurStepChanged`.

- [ ] **Step 2: Пересобрать установщик**

Run:
```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\pdfsmith.iss
```
Expected: `Successful compile`, `dist\pdfsmith-setup.exe` обновлён.

- [ ] **Step 3: Установка с включённой задачей setdefault (интерактивно)**

Run:
```powershell
Start-Process dist\pdfsmith-setup.exe -Wait
```
В мастере оставить галочку «Сделать PDFsmith приложением по умолчанию для PDF», завершить.
Expected: в конце появляется системный диалог «Каким образом вы хотите открывать файлы .pdf?» со списком приложений, включая PDFsmith. Выбрать PDFsmith → OK.

- [ ] **Step 4: Проверить, что PDFsmith стал обработчиком по умолчанию**

Run:
```powershell
reg query "HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.pdf\UserChoice" /v ProgId
```
Expected: значение ProgId = `PDFsmith.Document` (если пользователь выбрал PDFsmith в диалоге).

Затем двойной клик по любому `.pdf` в проводнике.
Expected: файл открывается в PDFsmith и отображается.

- [ ] **Step 5: Commit**

```powershell
git add installer/pdfsmith.iss
git commit -m "installer: системный диалог выбора приложения по умолчанию для .pdf"
```

---

## Task 6: Документация сборки и проверка удаления

**Files:**
- Create: `installer/README.md`

- [ ] **Step 1: Написать installer/README.md**

Создать `installer/README.md`:

```markdown
# Сборка установщика PDFsmith

## Требования
- Rust toolchain (`cargo`)
- Python 3 + Pillow (`python -m pip install --user pillow`)
- Inno Setup 6 (`winget install --id JRSoftware.InnoSetup -e`)

## Шаги
1. Сгенерировать иконку (если менялась):
   `python assets\make_icon.py`
2. Собрать релизный бинарь:
   `cargo build --release --bin pdfsmith`
3. Скомпилировать установщик:
   `& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\pdfsmith.iss`
4. Готовый файл: `dist\pdfsmith-setup.exe`

## Что делает установщик
- Ставит `pdfsmith.exe` + `pdfium.dll` + `pdfsmith.ico` в `%LocalAppData%\PDFsmith` (без прав администратора).
- Регистрирует ProgID `PDFsmith.Document` и ассоциацию `.pdf` в HKCU.
- Добавляет PDFsmith в «Открыть с помощью» и в «Приложения по умолчанию».
- По галочке открывает системный диалог выбора приложения по умолчанию.

## Тихая установка
`pdfsmith-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /TASKS=`
```

- [ ] **Step 2: Проверить деинсталляцию**

Run (найти и запустить деинсталлятор тихо):
```powershell
$u = Join-Path $env:LOCALAPPDATA 'PDFsmith\unins000.exe'
Start-Process $u -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES' -Wait
```
Проверка, что файлы и ключи удалены:
```powershell
Test-Path (Join-Path $env:LOCALAPPDATA 'PDFsmith\pdfsmith.exe')   # Expected: False
reg query "HKCU\Software\Classes\PDFsmith.Document" 2>$null       # Expected: ошибка «не удалось найти»
reg query "HKCU\Software\RegisteredApplications" /v PDFsmith 2>$null  # Expected: ошибка
reg query "HKCU\Software\PDFsmith" 2>$null                        # Expected: ошибка
```
Expected: все проверки показывают отсутствие файлов и ключей.

- [ ] **Step 3: Commit**

```powershell
git add installer/README.md
git commit -m "docs: инструкция по сборке установщика"
```

---

## Финальный чек-лист (соответствие критериям готовности спека)

- [ ] `setup.exe` ставит приложение без прав администратора (Task 4, Step 5).
- [ ] PDFsmith есть в «Открыть с помощью» для `.pdf` (Task 4, Step 6).
- [ ] PDFsmith виден в «Приложения по умолчанию» (Capabilities/RegisteredApplications, Task 4, Step 5).
- [ ] Двойной клик по `.pdf` открывает и отображает файл в PDFsmith (Task 5, Step 4).
- [ ] Деинсталляция не оставляет файлов и ключей (Task 6, Step 2).
