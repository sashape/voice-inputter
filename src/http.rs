//! Минимальный HTTPS-клиент на WinHTTP: системный стек, без новых крейтов и
//! DLL. Хватает на две задачи — скачать модель и спросить у GitHub про релиз.

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Что делать с очередным куском ответа. `false` — прервать загрузку.
pub type OnChunk<'a> = &'a mut dyn FnMut(&[u8], u64, u64) -> bool;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// GET по HTTPS с отдачей тела кусками (got/total — для прогресса).
/// Редиректы WinHTTP проходит сам (важно: GitHub уводит на objects.*).
pub fn get(host: &str, path: &str, on_chunk: OnChunk) -> Result<(), String> {
    let (host_w, path_w, agent) = (wide(host), wide(path), wide("VoiceInputter"));
    let verb = wide("GET");

    unsafe {
        let session = WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        );
        if session.is_null() {
            return Err("нет доступа к сети".into());
        }
        let _s = Handle(session);
        let _ = WinHttpSetTimeouts(session, 30_000, 30_000, 30_000, 30_000);

        let conn = WinHttpConnect(session, PCWSTR(host_w.as_ptr()), 443, 0);
        if conn.is_null() {
            return Err(format!("не подключиться к {host}"));
        }
        let _c = Handle(conn);

        let req = WinHttpOpenRequest(
            conn,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path_w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null_mut(),
            WINHTTP_FLAG_SECURE,
        );
        if req.is_null() {
            return Err("не создать запрос".into());
        }
        let _r = Handle(req);

        WinHttpSendRequest(req, None, None, 0, 0, 0).map_err(|e| format!("запрос не отправлен: {e}"))?;
        WinHttpReceiveResponse(req, std::ptr::null_mut()).map_err(|e| format!("нет ответа: {e}"))?;

        let status = query_number(req, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        if status != 200 {
            return Err(format!("сервер ответил {status}"));
        }
        let total = query_number(req, WINHTTP_QUERY_CONTENT_LENGTH).unwrap_or(0) as u64;

        let mut buf = vec![0u8; 64 * 1024];
        let mut got = 0u64;
        loop {
            let mut read = 0u32;
            WinHttpReadData(req, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read)
                .map_err(|e| format!("обрыв загрузки: {e}"))?;
            if read == 0 {
                break;
            }
            got += read as u64;
            if !on_chunk(&buf[..read as usize], got, total) {
                return Err("отменено".into());
            }
        }
        if total > 0 && got < total {
            return Err("загрузка прервалась".into());
        }
    }
    Ok(())
}

/// GET, возвращающий тело строкой (для небольших ответов вроде JSON).
pub fn get_string(host: &str, path: &str, limit: usize) -> Result<String, String> {
    let mut body = Vec::new();
    let mut sink = |chunk: &[u8], _: u64, _: u64| {
        if body.len() + chunk.len() > limit {
            return false;
        }
        body.extend_from_slice(chunk);
        true
    };
    get(host, path, &mut sink)?;
    String::from_utf8(body).map_err(|_| "ответ не в UTF-8".to_string())
}

/// Числовой заголовок ответа (код статуса, длина содержимого).
unsafe fn query_number(req: *mut c_void, level: u32) -> Option<u32> {
    let mut v = 0u32;
    let mut len = std::mem::size_of::<u32>() as u32;
    WinHttpQueryHeaders(
        req,
        level | WINHTTP_QUERY_FLAG_NUMBER,
        PCWSTR::null(),
        Some(&mut v as *mut u32 as *mut c_void),
        &mut len,
        std::ptr::null_mut(),
    )
    .ok()
    .map(|_| v)
}

/// Закрывает HINTERNET на выходе из области видимости.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}
