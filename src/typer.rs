//! Ввод текста в активное окно через SendInput (Unicode).
//!
//! KEYEVENTF_UNICODE отправляет символ напрямую, минуя раскладку —
//! кириллица и латиница печатаются одинаково надёжно, буфер обмена не трогаем.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_BACK,
};

pub fn type_text(text: &str) {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        inputs.push(key(unit, false));
        inputs.push(key(unit, true));
    }
    if inputs.is_empty() {
        return;
    }
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Отправить n нажатий Backspace (удаляет n символов перед курсором).
pub fn backspace(n: usize) {
    if n == 0 {
        return;
    }
    let mut inputs: Vec<INPUT> = Vec::with_capacity(n * 2);
    for _ in 0..n {
        inputs.push(vkey(VK_BACK, false));
        inputs.push(vkey(VK_BACK, true));
    }
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn vkey(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key(scan: u16, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_UNICODE;
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

// KEYBD_EVENT_FLAGS реализует BitOr — используется выше.
const _: fn() = || {
    let _ = KEYEVENTF_UNICODE | KEYBD_EVENT_FLAGS(0);
};
