# Скачивает офлайн-распознавание речи для голосового управления «Джарвис»:
# libvosk.dll (Vosk 0.3.45, win64) и маленькую русскую модель vosk-model-small-ru-0.22.
# Кладёт всё в resources\vosk — оттуда их берёт сборка и установщик.
#
# Запуск из корня проекта:
#   powershell -ExecutionPolicy Bypass -File scripts\get-vosk.ps1

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "resources\vosk"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "timer-pro-vosk"
New-Item -ItemType Directory -Force $dest, $tmp | Out-Null

$lib = @{ url = "https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-win64-0.3.45.zip"; zip = "vosk-lib.zip" }
$model = @{ url = "https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip"; zip = "vosk-model.zip" }

foreach ($item in $lib, $model) {
    $zip = Join-Path $tmp $item.zip
    if (-not (Test-Path $zip)) {
        Write-Host "Скачиваю $($item.url)"
        Invoke-WebRequest -Uri $item.url -OutFile $zip -UseBasicParsing
    }
    Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp ($item.zip -replace '\.zip$', '')) -Force
}

# Библиотеки: libvosk.dll и её зависимости (libstdc++, libgcc, winpthread).
Get-ChildItem (Join-Path $tmp "vosk-lib") -Recurse -Filter *.dll | Copy-Item -Destination $dest -Force

# Модель: содержимое папки vosk-model-small-ru-0.22 → resources\vosk\model.
$modelSrc = Get-ChildItem (Join-Path $tmp "vosk-model") -Directory | Select-Object -First 1
$modelDst = Join-Path $dest "model"
if (Test-Path $modelDst) { Remove-Item $modelDst -Recurse -Force }
Copy-Item $modelSrc.FullName $modelDst -Recurse

Write-Host "Готово: $dest"
Get-ChildItem $dest | Select-Object Name
