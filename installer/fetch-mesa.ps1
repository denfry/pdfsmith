# Скачивает программный OpenGL Mesa (llvmpipe, pal1000/mesa-dist-win) в корень репозитория:
# opengl32.dll + libgallium_wgl.dll. С ними PDFsmith запускается на ПК без видеокарты и драйверов.
param(
    [string]$Version = "26.2.1",
    [string]$Sha256 = "78a0305844074535e73dfb6dcb5eb2a65f1d9dc445102e1f4c3c3e97dace19bf"
)
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
$files = @("opengl32.dll", "libgallium_wgl.dll")
if (-not ($files | Where-Object { -not (Test-Path (Join-Path $root $_)) })) {
    Write-Host "Mesa уже есть в $root"; exit 0
}
$sevenZip = (Get-Command 7z -ErrorAction SilentlyContinue).Source
if (-not $sevenZip) { $sevenZip = "$env:ProgramFiles\7-Zip\7z.exe" }
if (-not (Test-Path $sevenZip)) { throw "Нужен 7-Zip (7z.exe) для распаковки Mesa" }

$url = "https://github.com/pal1000/mesa-dist-win/releases/download/$Version/mesa3d-$Version-release-msvc.7z"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "mesa-$([guid]::NewGuid())"
New-Item -ItemType Directory $tmp | Out-Null
try {
    $archive = "$tmp\mesa.7z"
    Invoke-WebRequest $url -OutFile $archive
    $actual = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $Sha256) { throw "SHA-256 архива Mesa не совпал: $actual" }
    & $sevenZip e -y "-o$tmp" $archive ($files | ForEach-Object { "x64\$_" }) | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "7z завершился с кодом $LASTEXITCODE" }
    foreach ($f in $files) { Copy-Item "$tmp\$f" (Join-Path $root $f) -Force }
    Write-Host "Mesa $Version (llvmpipe) → $root"
} finally {
    Remove-Item -Recurse -Force $tmp
}
