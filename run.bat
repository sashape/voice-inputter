@echo off
REM Запуск Voice Inputter (release). Рабочий каталог = папка проекта,
REM чтобы нашлись config.json и папка модели.
cd /d "%~dp0"

if not exist "target\release\voice-inputter.exe" (
    echo Бинарник не собран. Выполните:  cargo build --release
    pause
    exit /b 1
)
if not exist "models" (
    echo Папка models не найдена. Сначала запустите:  powershell -ExecutionPolicy Bypass -File setup.ps1
    pause
    exit /b 1
)

start "" "target\release\voice-inputter.exe"
