//! Fresh WinHTTP sessions honor Windows proxy/PAC settings and use the OS TLS stack.
//! No credentials enter command lines, logs, or a child process.
use std::{ffi::c_void, ptr, time::{Duration, Instant}};
use windows::{core::{w, PCWSTR}, Win32::Networking::WinHttp::*};

struct Handle(*mut c_void);
impl Handle {
    fn new(raw: *mut c_void) -> Result<Self, String> {
        if raw.is_null() { Err(format!("Windows HTTP: {}", windows::core::Error::from_win32())) }
        else { Ok(Self(raw)) }
    }
}
impl Drop for Handle {
    fn drop(&mut self) { unsafe { let _ = WinHttpCloseHandle(self.0); } }
}

pub struct Response {
    pub status: u32,
    pub retry_after: Option<u64>,
    pub body: Vec<u8>,
}

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

pub fn get(host: &str, port: u16, path: &str, secure: bool, headers: &str) -> Result<Response, String> {
    // Redirects are disabled: bearer credentials belong only to this origin.
    let result = unsafe {
        let session = Handle::new(WinHttpOpen(w!("codenotch/0.3.0 (Windows)"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0))?;
        let winerr = |e: windows::core::Error| format!("Windows HTTP: {e}");
        WinHttpSetTimeouts(session.0, 5_000, 5_000, 10_000, 15_000).map_err(winerr)?;
        let host = wide(host);
        let connection = Handle::new(WinHttpConnect(session.0, PCWSTR(host.as_ptr()), port, 0))?;
        let path = wide(path);
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = Handle::new(WinHttpOpenRequest(connection.0, w!("GET"), PCWSTR(path.as_ptr()),
            PCWSTR::null(), PCWSTR::null(), ptr::null(), flags))?;
        WinHttpSetOption(Some(request.0), WINHTTP_OPTION_REDIRECT_POLICY,
            Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_ne_bytes())).map_err(winerr)?;
        let headers: Vec<u16> = headers.encode_utf16().collect();
        let request_headers = if headers.is_empty() { None } else { Some(headers.as_slice()) };
        WinHttpSendRequest(request.0, request_headers, None, 0, 0, 0).map_err(winerr)?;
        WinHttpReceiveResponse(request.0, ptr::null_mut()).map_err(winerr)?;
        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(request.0, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(), Some((&mut status as *mut u32).cast()), &mut size, ptr::null_mut()).map_err(winerr)?;
        let mut retry = [0u16; 128];
        let mut size = std::mem::size_of_val(&retry) as u32;
        let retry_after = WinHttpQueryHeaders(request.0, WINHTTP_QUERY_RETRY_AFTER,
            PCWSTR::null(), Some(retry.as_mut_ptr().cast()), &mut size, ptr::null_mut())
            .ok().and_then(|_| {
                let len = retry.iter().position(|c| *c == 0).unwrap_or(retry.len());
                String::from_utf16_lossy(&retry[..len]).trim().parse().ok()
            });
        let mut body = Vec::new();
        let started = Instant::now();
        loop {
            let mut buf = [0u8; 8192];
            let mut count = 0;
            WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut count).map_err(winerr)?;
            if count == 0 { break; }
            body.extend_from_slice(&buf[..count as usize]);
            if body.len() > 1024 * 1024 || started.elapsed() > Duration::from_secs(20) {
                return Err("Windows HTTP: response exceeded limit".into());
            }
        }
        Response { status, retry_after, body }
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, net::TcpListener};

    #[test]
    fn preserves_status_retry_and_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut raw = Vec::new();
            while !raw.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if socket.read(&mut byte).unwrap() == 0 { break; }
                raw.push(byte[0]);
            }
            assert!(String::from_utf8_lossy(&raw).contains("Bearer fake-test-token"));
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
        });
        let response = get("127.0.0.1", port, "/usage", false, "Authorization: Bearer fake-test-token\r\n").unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(response.retry_after, Some(120));
        assert_eq!(response.body, b"{}");
    }

    #[test]
    fn does_not_follow_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0; 4096];
            let _ = socket.read(&mut buf);
            socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let response = get("127.0.0.1", port, "/usage", false, "").unwrap();
        server.join().unwrap();
        assert_eq!(response.status, 302);
    }
}
