# Собирает автономный дистрибутив в .\dist (exe + модель + config).
# Exe самодостаточный: ни DLL, ни VC++ Redistributable не нужны.
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $here

Write-Host "Сборка release..."
cargo build --release

$dist = Join-Path $here "dist"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Path $dist | Out-Null

Copy-Item "target\release\voice-inputter.exe" $dist

# модель
$model = "sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16"
New-Item -ItemType Directory -Path (Join-Path $dist "models") | Out-Null
Copy-Item -Recurse (Join-Path "models" $model) (Join-Path $dist "models\$model")

# config с относительным путём к модели
$cfg = [ordered]@{
    model_path      = "models/$model"
    device_name     = $null
    wake_words      = @("джарвис", "компьютер")
    stop_words      = @("стоп", "хватит", "достаточно")
    silence_timeout = 6.0
    hotkey          = "ctrl+alt+j"
    overlay_scale   = 1.0
    overlay_mode    = "always"
    hotwords_score  = 2.0
    live_typing     = $true
    append_space    = $true
    capitalize      = $true
} | ConvertTo-Json
[System.IO.File]::WriteAllText((Join-Path $dist "config.json"), $cfg, (New-Object System.Text.UTF8Encoding($false)))

Write-Host ""
Write-Host "Готово: $dist  (папку можно переносить целиком; DLL не нужны)"
