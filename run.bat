@echo off
REM Запуск Voice Inputter (release). Рабочий каталог = папка проекта,
REM чтобы нашлись config.json и папка модели.
cd /d "%~dp0"

if not exist "targeteleaseoice-inputter.exe" (
    echo Binary not built. Run:  cargo build --release
    pause
    exit /b 1
)

REM Папка models не обязательна: если модели нет, приложение предложит
REM скачать её при первом запуске (или запустите setup.ps1).
start "" "targeteleaseoice-inputter.exe"
