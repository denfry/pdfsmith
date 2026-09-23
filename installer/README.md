# Сборка установщика PDFsmith

## Требования
- Rust toolchain (`cargo`) + Windows SDK (для вшивания иконки через `winresource`;
  без SDK exe соберётся без встроенной иконки — это не ошибка).
- Python 3 + Pillow (`python -m pip install --user pillow`) — только если меняете иконку.
- Inno Setup 6 (`winget install --id JRSoftware.InnoSetup -e`).

## Шаги
0. `pwsh installer\fetch-pdfium.ps1` и `pwsh installer\fetch-mesa.ps1` — DLL в корень
   репозитория (нужен 7-Zip для Mesa).
1. (если меняли иконку) `python assets\make_icon.py`
2. `cargo build --release --bin pdfsmith`
3. `& "<ISCC.exe>" /DAppVersion=0.1.0 installer\pdfsmith.iss`
   - winget ставит ISCC per-user в `%LocalAppData%\Programs\Inno Setup 6\ISCC.exe`
     (либо системно — `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`).
   - `/DAppVersion` — версия для номера сборки установщика; при ручном запуске
     подставьте текущую версию. CI передаёт реальную версию из тега релиза.
4. Готовый файл: `dist\pdfsmith-setup.exe`

## Что делает установщик
- Кладёт `pdfsmith.exe` + `pdfium.dll` + `pdfsmith.ico` + программный OpenGL
  (`opengl32.dll`, `libgallium_wgl.dll`, скачиваются `fetch-mesa.ps1`) в `%LocalAppData%\PDFsmith`
  (без прав администратора).
- Регистрирует ProgID `PDFsmith.Document` и ассоциацию `.pdf` в HKCU; добавляет
  PDFsmith в «Открыть с помощью» и в «Приложения по умолчанию».
- По галочке «Сделать PDFsmith приложением по умолчанию для PDF» открывает в конце
  системный диалог выбора приложения для `.pdf`.
- Ярлык в меню «Пуск» (на рабочем столе — по галочке).

## Тихая установка / удаление
- Установка: `pdfsmith-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /TASKS=`
  (`/TASKS=` отключает все задачи, в т.ч. диалог «по умолчанию»).
- Удаление: `"%LocalAppData%\PDFsmith\unins000.exe" /VERYSILENT`

## Проверено
Тихая установка раскладывает файлы и ключи HKCU (ProgID `shell\open\command`,
Capabilities/RegisteredApplications, `.pdf\OpenWithProgids` = `PDFsmith.Document`);
деинсталляция удаляет файлы, папку и все ключи без остатка.

## Выпуск новой версии (автоматически через GitHub Actions)
1. Поднять `version` в `[workspace.package]` корневого `Cargo.toml`, `cargo build`
   (обновит `Cargo.lock`), закоммитить.
2. Добавить запись в CHANGELOG.md.
3. `git tag -a v1.2.3 -m "Что нового: ..."` — текст тега станет описанием релиза
   и показывается в приложении по ссылке «Что нового».
4. `git push --follow-tags`.
5. Workflow **Release** проверит версию, соберёт и опубликует Release с
   `pdfsmith-setup.exe`, `pdfsmith-setup.exe.sha256` и `latest.json`.

## Как приложение обновляется
- Раз в 12 часов (и по кнопке «Проверить сейчас» в настройках) читает
  `releases/latest/download/latest.json`.
- По умолчанию показывает плашку «Доступна версия …»; с включённым
  «Автоматически устанавливать обновления» качает в фоне и ставит при закрытии.
- Перед запуском установщика сверяет SHA-256. Установщик запускается тихо:
  `pdfsmith-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART [/RELAUNCH]`.
- Если открыты другие окна PDFsmith, установка откладывается до следующего закрытия.
