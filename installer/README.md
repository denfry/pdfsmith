# Сборка установщика PDFsmith

## Требования
- Rust toolchain (`cargo`) + Windows SDK (для вшивания иконки через `winresource`;
  без SDK exe соберётся без встроенной иконки — это не ошибка).
- Python 3 + Pillow (`python -m pip install --user pillow`) — только если меняете иконку.
- Inno Setup 6 (`winget install --id JRSoftware.InnoSetup -e`).

## Шаги
1. (если меняли иконку) `python assets\make_icon.py`
2. `cargo build --release --bin pdfsmith`
3. `& "<ISCC.exe>" installer\pdfsmith.iss`
   - winget ставит ISCC per-user в `%LocalAppData%\Programs\Inno Setup 6\ISCC.exe`
     (либо системно — `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`).
4. Готовый файл: `dist\pdfsmith-setup.exe`

## Что делает установщик
- Кладёт `pdfsmith.exe` + `pdfium.dll` + `pdfsmith.ico` в `%LocalAppData%\PDFsmith`
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
