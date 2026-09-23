; PDFsmith — per-user установщик (без прав администратора)
#define AppName "PDFsmith"
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define ProgId "PDFsmith.Document"

[Setup]
AppId={{8F3B6A2C-1D4E-4C7A-9B12-A1B2C3D4E5F6}
AppName={#AppName}
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
CloseApplications=force
RestartApplications=no
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
Root: HKCU; Subkey: "Software\Classes\.pdf\OpenWithProgids"; ValueType: string; ValueName: "{#ProgId}"; ValueData: ""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\pdfsmith.exe"; Description: "Запустить PDFsmith"; Flags: nowait postinstall skipifsilent
Filename: "{app}\pdfsmith.exe"; Flags: nowait skipifnotsilent; Check: ShouldRelaunch

[UninstallDelete]
Type: filesandordirs; Name: "{app}\cache"
Type: filesandordirs; Name: "{app}\updates"

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

procedure CurStepChanged(CurStep: TSetupStep);
var
  Info: TOpenAsInfo;
  ErrorCode: Integer;
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('setdefault') and not WizardSilent then
  begin
    Info.pcszFile := '';
    Info.pcszClass := '.pdf';
    Info.oaifInFlags := OAIF_REGISTER_EXT or OAIF_ALLOW_REGISTRATION or OAIF_FORCE_REGISTRATION;
    try
      SHOpenWithDialog(0, Info);
    except
      // Фолбэк: открыть Параметры → Приложения по умолчанию.
      ShellExec('open', 'ms-settings:defaultapps', '', '', SW_SHOW, ewNoWait, ErrorCode);
    end;
  end;
end;
