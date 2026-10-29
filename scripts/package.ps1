# Упаковка релиза: установщик (Inno Setup) и портативный zip — в папку installer\.
# Перед запуском: scripts\get-vosk.ps1 и cargo build --release.
# Версия берётся из Cargo.toml.
#
#   powershell -ExecutionPolicy Bypass -File scripts\package.ps1

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not (Test-Path "target\release\timer_pro.exe")) { throw "Нет target\release\timer_pro.exe — сначала cargo build --release" }
if (-not (Test-Path "resources\vosk\libvosk.dll")) { throw "Нет resources\vosk — сначала scripts\get-vosk.ps1" }

# Установщик.
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Нет Inno Setup 6 (winget install JRSoftware.InnoSetup)" }
& $iscc /Q "/DAppVersion=$version" setup.iss
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Портативный zip: внутри папка с exe и голосовой моделью рядом — программа ищет vosk\ возле exe.
$name = "TimerPro-$version-portable"
$stage = Join-Path "installer" $name
$zip = "installer\$name.zip"
foreach ($p in $stage, $zip) { if (Test-Path $p) { Remove-Item $p -Recurse -Force } }
New-Item -ItemType Directory $stage | Out-Null
Copy-Item "target\release\timer_pro.exe", "LICENSE", "THIRD_PARTY.md" $stage
Copy-Item "resources\vosk" (Join-Path $stage "vosk") -Recurse
Compress-Archive -Path $stage -DestinationPath $zip
Remove-Item $stage -Recurse -Force

Get-ChildItem installer -File | Select-Object Name, @{ n = "MB"; e = { [math]::Round($_.Length / 1MB, 1) } }
