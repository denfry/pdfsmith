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
