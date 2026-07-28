# Voice Inputter 🎙️

Native voice dictation for Windows, written in Rust. It listens to your
microphone, recognizes speech **offline and streaming** (via
[sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx), a streaming zipformer
model), and types the text **wherever your cursor is — live, word by word**.
While dictating it shows a waveform overlay (floating) **without
stealing focus**, so your cursor stays exactly where it was.

The default model is Russian, but any sherpa-onnx streaming model works (see
[Models](#models)).

## Features

- 🎙 **Wake word** — say the assistant's name (`джарвис` / `компьютер`, configurable).
- ⌨️ **Global hotkey** — configurable (default `Ctrl+Alt+J`).
- 🖱 **Tray icon + right-click menu** — Settings (pick microphone, set wake word),
  toggle listening, quit.
- 🌊 **Waveform overlay** — top-most, click-through (`WS_EX_NOACTIVATE`) window;
  focus and caret position never change. The mic / ⚙️ / ✕ buttons are clickable.
- ⚡ **Live typing** — words appear as you speak; if the recognizer revises a
  word, the extra characters are erased with Backspace and retyped.
- ⌨️ **Unicode input via `SendInput`** — Cyrillic/Latin typed directly, no
  clipboard, no keyboard-layout dependency.
- 🔄 **Update check** — once a day the app asks GitHub for the latest release
  and, if a newer version exists, shows a tray notification and a menu item that
  opens the release page. Check only — nothing is downloaded or installed
  behind your back; switch it off with "Проверять обновления" in the settings.
- 📦 **Self-contained exe** — sherpa, onnxruntime and the CRT are linked
  statically: **no DLLs and no VC++ Redistributable required**. Distribution is
  just the exe + `config.json` + the model folder.

## Quick start (prebuilt exe)

Nothing to install — the exe is self-contained. Two ways to get it:

- **`voice-inputter.exe` alone (~19 MB)** — run it; on the first launch it
  offers to download the recognition model (~23 MB) and shows the progress,
  then works offline forever. The download uses the system HTTPS stack
  (WinHTTP) and Windows' own `tar.exe`; nothing else is required.
- **`VoiceInputter-win-x64.zip` (~47 MB)** — exe + model + `config.json` for
  machines without internet access. Unpack anywhere and run.

A waveform tray icon appears. Put the cursor in any text field, say
**"Джарвис …"** and dictate. Say **"стоп"** to finish.

Data (the `config.json` and the downloaded model) is kept next to the exe when
that folder is writable — so a portable folder stays portable — and falls back
to `%LOCALAPPDATA%\VoiceInputter` otherwise (e.g. when installed under
`Program Files`). "Запускать при входе в Windows" in the settings window adds
the app to `HKCU\...\CurrentVersion\Run`.

### Standalone distribution
```powershell
powershell -ExecutionPolicy Bypass -File package.ps1
```
Produces a `dist\` folder (exe + model + `config.json`, ~47 MB) that can be
moved as a whole. No DLLs needed.

## Building from source

Building requires **MSVC**, because sherpa-onnx links against MSVC prebuilt
libraries (it does **not** build on the GNU/MinGW toolchain):

1. Rust MSVC toolchain:
   ```
   rustup toolchain install stable-x86_64-pc-windows-msvc
   rustup default stable-x86_64-pc-windows-msvc
   ```
2. Visual Studio Build Tools 2022 with the C++ workload:
   ```
   winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
3. `cargo build --release` (`+crt-static` is set in `.cargo/config.toml`, required
   because sherpa's static libraries use the static MT CRT).

Quick recognition check without a microphone:
```
cargo test transcribe_sample -- --ignored --nocapture
```
(runs `test_wavs/0.wav` through the model).

## Models

Uses [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) with the streaming
model **`sherpa-onnx-streaming-zipformer-small-ru-vosk`** (~28 MB, by the Vosk /
alphacep team). Streaming keeps the live word-by-word typing; it is light on RAM
(hundreds of MB, not the gigabytes a large Vosk HCLG graph needs).

Other models: <https://github.com/k2-fsa/sherpa-onnx/releases> (tag
`asr-models`). Drop the extracted folder into `models\` and point `model_path`
at it — the encoder/decoder/joiner and `tokens.txt` files are detected
automatically (int8 variants preferred).

## Configuration — `config.json`

Everything except `model_path` is editable in the Settings window (tray →
"Настройки…", or the ⚙ button on the overlay); the rarely-changed knobs
(`silence_timeout`, `hotwords_score`, `overlay_scale`, `append_space`,
`capitalize`, `check_updates`) live under "Дополнительно" there. Saving rewrites `config.json`
and applies the change without a restart.

If `config.json` is absent, built-in defaults are used. See
[`config.example.json`](config.example.json).

```jsonc
{
  "model_path": "models/sherpa-onnx-streaming-zipformer-small-ru-vosk-int8-2025-08-16",
  "device_name": null,               // microphone (null = default); easier to set in the Settings window
  "wake_words": ["джарвис", "компьютер"],
  "stop_words": ["стоп", "хватит", "достаточно"],
  "silence_timeout": 6.0,            // seconds of silence before dictation auto-stops
  "hotkey": "ctrl+alt+space",        // ctrl/alt/shift/win + letter/digit/space/F1..F12
  "overlay_scale": 1.0,              // extra overlay size multiplier on top of DPI
  "live_typing": true,               // true = type as you speak; false = after a pause
  "check_updates": true,             // daily check for a newer GitHub release
  "append_space": true,
  "capitalize": true
}
```

> If the hotkey fails to register ("already in use"), another app owns that
> combo — change `hotkey`. Voice and tray keep working regardless.

## Architecture

| Module | Responsibility |
|---|---|
| `audio.rs` | Microphone capture (cpal), downmix, resample to 16 kHz f32 |
| `stt.rs` | sherpa-onnx `OnlineRecognizer` (streaming transcription) |
| `engine.rs` | Worker thread: live typing (growing text), endpoint, wake/stop words |
| `typer.rs` | Types into the focused window via `SendInput` (+ Backspace for edits) |
| `ui.rs` | WinAPI: tray, menu, waveform overlay, hotkey, DPI |
| `overlay.rs` | Software renderer for the waveform (pill, bars, mic) with anti-aliasing |
| `settings.rs` | Settings window: custom-drawn layered window, DPI-scaled layout |
| `model.rs` / `model_ui.rs` | Model download (WinHTTP + `tar.exe`) and the first-run window |
| `win_ui.rs` | Shared window kit: palette, fonts, GDI text, layered surface |
| `startup.rs` | "Run at Windows startup" toggle (`HKCU\...\Run`) |
| `update.rs` / `http.rs` | Daily release check on GitHub / tiny WinHTTP client |
| `paint.rs` | Shared drawing primitives (rounded rects, gradients, blur, easing) |
| `icons.rs` | Tray icons from the design PNGs, `.ico` builder for the exe |
| `shared.rs` / `config.rs` | Shared state / `config.json` |

## Tray icon states

| Icon | Meaning |
|---|---|
| White waveform | Waiting for the wake word |
| Colored waveform with a pink dot | Dictating |
| Dimmed waveform | Listening disabled |

On a light Windows theme the white waveform would be invisible, so it is
re-inked dark; the app follows theme switches live (`WM_SETTINGCHANGE` →
`ImmersiveColorSet`).

Icons live in [`assets/icons`](assets/icons) (PNG, embedded into the exe);
`assets/app.ico` is the executable icon, rebuilt from those PNGs with
`cargo test build_app_ico -- --ignored`. Preview all tray states and both
themes with `cargo test tray_preview -- --ignored`.

## License

Released into the public domain under [The Unlicense](LICENSE).
