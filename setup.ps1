# Загрузка модели распознавания для Voice Inputter (sherpa-onnx).
# Сам exe самодостаточный (статическая линковка sherpa+onnxruntime+CRT) —
# никаких DLL и VC++ Redistributable не нужно. Нужна только модель.
#
# ВНИМАНИЕ (только для СБОРКИ, не для запуска): чтобы собрать проект,
# нужен msvc-тулчейн Rust и VS Build Tools (C++). Пользователю готового
# exe это не требуется.
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $here

$model = "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16"
$url = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$model.tar.bz2"

$dest = Join-Path $here "models\$model"
if (Test-Path $dest) {
    Write-Host "Модель уже на месте: $dest"
} else {
    Write-Host "Скачиваю модель $model (~23 МБ) ..."
    New-Item -ItemType Directory -Force -Path (Join-Path $here "models") | Out-Null
    $tmp = Join-Path $env:TEMP "$model.tar.bz2"
    Invoke-WebRequest -Uri $url -OutFile $tmp
    Write-Host "Распаковываю ..."
    # Windows tar.exe (bsdtar) понимает .tar.bz2
    tar -xf $tmp -C (Join-Path $here "models")
    Remove-Item $tmp
    Write-Host "  Готово: $dest"
}

Write-Host ""
Write-Host "Запуск:  run.bat   (или собрать заново: cargo build --release)"
