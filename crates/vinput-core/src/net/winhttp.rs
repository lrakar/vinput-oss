//! The calls into `WinHTTP` behind [`super::Request`].
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_CANNOT_CONNECT, ERROR_WINHTTP_CONNECTION_ERROR, ERROR_WINHTTP_NAME_NOT_RESOLVED,
    ERROR_WINHTTP_REDIRECT_FAILED, ERROR_WINHTTP_SECURE_FAILURE, ERROR_WINHTTP_TIMEOUT,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_CALLBACK_FLAG_SECURE_FAILURE,
    WINHTTP_CALLBACK_STATUS_FLAG_CERT_REV_FAILED, WINHTTP_CALLBACK_STATUS_SECURE_FAILURE,
    WINHTTP_ENABLE_SSL_REVOCATION, WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
    WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3, WINHTTP_OPTION_ENABLE_FEATURE,
    WINHTTP_OPTION_REDIRECT_POLICY, WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP,
    WINHTTP_OPTION_REDIRECT_POLICY_NEVER, WINHTTP_OPTION_SECURE_PROTOCOLS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetStatusCallback, WinHttpSetTimeouts,
};

use super::{Request, Target, USER_AGENT};

/// The size of the numbers `WinHTTP` takes and gives as options.
const U32_SIZE: u32 = 4;

/// A `WinHTTP` handle, closed when dropped.
#[derive(Debug)]
struct Handle(*mut c_void);

// SAFETY: a `WinHTTP` handle is not tied to the thread that opened it, and
// each one here is used by one thread at a time.
unsafe impl Send for Handle {}

impl Handle {
    /// Wraps what an open call returned; null means the call failed.
    fn new(raw: *mut c_void, doing: &str) -> Result<Self, String> {
        if raw.is_null() {
            Err(last_error(doing))
        } else {
            Ok(Self(raw))
        }
    }

    /// Sets a numeric option and says whether Windows took it.
    fn set(&self, option: u32, value: u32) -> bool {
        // SAFETY: the handle is open, and the buffer is a `u32` that lives
        // through the call and is described with its own size.
        unsafe { WinHttpSetOption(self.0, option, (&raw const value).cast(), U32_SIZE) != 0 }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `WinHTTP` and is closed exactly once.
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}

/// An answer that is being read. The handles are closed in the order request,
/// connection, session, which is the order of the fields.
#[derive(Debug)]
pub(super) struct Response {
    request: Handle,
    _connection: Handle,
    _session: Handle,
    status: u16,
}

impl Response {
    pub(super) fn status(&self) -> u16 {
        self.status
    }

    pub(super) fn read(&mut self, buffer: &mut [u8]) -> Result<usize, String> {
        let mut read = 0u32;
        let room = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the request handle is open; `buffer` is writable for `room`
        // bytes and `read` lives through the call.
        let done = unsafe {
            WinHttpReadData(
                self.request.0,
                buffer.as_mut_ptr().cast(),
                room,
                &raw mut read,
            )
        };
        if done == 0 {
            return Err(last_error("receiving the answer"));
        }
        Ok(read as usize)
    }
}

/// A string as Windows takes it: UTF-16 with a zero at the end.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn millis(time: Duration) -> i32 {
    i32::try_from(time.as_millis()).unwrap_or(i32::MAX).max(1)
}

/// The last error of this thread, as a sentence.
fn last_error(doing: &str) -> String {
    // SAFETY: reads the calling thread's last error code.
    let code = unsafe { GetLastError() };
    let why = match code {
        ERROR_WINHTTP_TIMEOUT => "the server took too long to answer".to_owned(),
        ERROR_WINHTTP_NAME_NOT_RESOLVED => "the server's name could not be found".to_owned(),
        ERROR_WINHTTP_CANNOT_CONNECT => "the server could not be reached".to_owned(),
        ERROR_WINHTTP_CONNECTION_ERROR => "the connection was lost".to_owned(),
        ERROR_WINHTTP_SECURE_FAILURE => {
            "the server's certificate was not accepted by Windows".to_owned()
        }
        ERROR_WINHTTP_REDIRECT_FAILED => {
            "the server redirected to an address that is not HTTPS".to_owned()
        }
        other => format!("Windows error {other}"),
    };
    format!("{doing} failed: {why}")
}

/// Called by `WinHTTP` on the requesting thread when a certificate is not
/// accepted: notes what was wrong with it where [`send`] can read it.
unsafe extern "system" fn on_secure_failure(
    _request: *mut c_void,
    context: usize,
    status: u32,
    information: *mut c_void,
    length: u32,
) {
    if status == WINHTTP_CALLBACK_STATUS_SECURE_FAILURE
        && context != 0
        && !information.is_null()
        && length as usize >= size_of::<u32>()
    {
        // SAFETY: for this status `WinHTTP` passes a `u32` of flags, and
        // `context` is the address of the `AtomicU32` that `attempt` passed
        // to the send call and keeps alive until that call has returned.
        unsafe {
            let flags = information.cast::<u32>().read_unaligned();
            (*(context as *const AtomicU32)).fetch_or(flags, Ordering::Relaxed);
        }
    }
}

/// Makes the request and waits for the head of the answer.
///
/// Windows is asked to check that the server's certificate has not been
/// revoked. If the only thing wrong is that the revocation service could not
/// be reached, the request is made once more without that check, which is
/// what browsers do; a certificate that is revoked, expired, for another
/// name or from an unknown issuer is always refused.
pub(super) fn send(request: &Request<'_>, target: &Target) -> Result<Response, String> {
    let failure = AtomicU32::new(0);
    match attempt(request, target, true, &failure) {
        Err(_)
            if target.secure
                && failure.load(Ordering::Relaxed)
                    == WINHTTP_CALLBACK_STATUS_FLAG_CERT_REV_FAILED =>
        {
            attempt(request, target, false, &AtomicU32::new(0))
        }
        result => result,
    }
}

/// The header lines of a request, each ended the way HTTP ends a line.
fn header_lines(request: &Request<'_>) -> Result<String, String> {
    let mut lines = String::new();
    if request.form.is_some() {
        lines.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
    } else if request.json.is_some_and(|json| !json.is_empty()) {
        lines.push_str("Content-Type: application/json\r\n");
    }
    for (name, value) in request.headers {
        // A line break in a header would start another header.
        if name.contains(['\r', '\n', ':']) || value.contains(['\r', '\n']) {
            return Err(format!("the header {name} cannot be sent"));
        }
        lines.push_str(name);
        lines.push_str(": ");
        lines.push_str(value);
        lines.push_str("\r\n");
    }
    Ok(lines)
}

/// Opens a session with the system's proxy settings, TLS 1.2 and 1.3 only,
/// and the request's timeouts.
fn open_session(request: &Request<'_>) -> Result<Handle, String> {
    let agent = wide(USER_AGENT);
    // SAFETY: `agent` is NUL-terminated and outlives the call. The session
    // is opened without the asynchronous flag, so every call on it and on
    // what is opened from it returns when its work is done.
    let session = Handle::new(
        unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            )
        },
        "starting the connection",
    )?;
    // A Windows too old to know TLS 1.3 gets 1.2 alone.
    if !session.set(
        WINHTTP_OPTION_SECURE_PROTOCOLS,
        WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
    ) && !session.set(
        WINHTTP_OPTION_SECURE_PROTOCOLS,
        WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2,
    ) {
        return Err(last_error("choosing the TLS version"));
    }
    let stall = millis(request.stall_timeout);
    let connect = millis(request.connect_timeout);
    // SAFETY: the session handle is open.
    if unsafe { WinHttpSetTimeouts(session.0, connect, connect, stall, stall) } == 0 {
        return Err(last_error("setting the timeouts"));
    }
    Ok(session)
}

/// The HTTP status of the answer that has just been received.
fn status_of(request: &Handle) -> Result<u16, String> {
    let mut status = 0u32;
    let mut size = U32_SIZE;
    // SAFETY: the request handle is open and has its answer; the buffer is a
    // `u32`, which is what is asked for, and both live through the call.
    let read = unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            (&raw mut status).cast(),
            &raw mut size,
            std::ptr::null_mut(),
        )
    };
    if read == 0 {
        return Err(last_error("reading the status of the answer"));
    }
    Ok(u16::try_from(status).unwrap_or(0))
}

fn attempt(
    request: &Request<'_>,
    target: &Target,
    check_revocation: bool,
    failure: &AtomicU32,
) -> Result<Response, String> {
    let host = wide(&target.host);
    let path = wide(&target.path);
    let posted = request.form.or(request.json);
    let verb = wide(if posted.is_some() { "POST" } else { "GET" });
    let headers = header_lines(request)?;
    let header_units = u32::try_from(headers.encode_utf16().count())
        .map_err(|_| "the headers are too long".to_owned())?;
    let headers = wide(&headers);
    let body = posted.unwrap_or_default().as_bytes();
    let body_length = u32::try_from(body.len()).map_err(|_| "the form is too large".to_owned())?;
    let session = open_session(request)?;

    // SAFETY: every pointer passed below is to a buffer that lives until the
    // end of this function, strings are NUL-terminated UTF-16, and each
    // handle is checked before it is used and closed by `Handle`. The calls
    // are synchronous, so the status callback runs on this thread, inside
    // the call that triggers it, while `failure` is borrowed; it is taken
    // off the request before this function returns.
    unsafe {
        let connection = Handle::new(
            WinHttpConnect(session.0, host.as_ptr(), target.port, 0),
            "starting the connection",
        )?;
        let opened = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                if target.secure {
                    WINHTTP_FLAG_SECURE
                } else {
                    0
                },
            ),
            "preparing the request",
        )?;
        // An encrypted request never continues unencrypted, and a request to
        // this computer is never sent on to another one.
        let redirects = if target.secure {
            WINHTTP_OPTION_REDIRECT_POLICY_DISALLOW_HTTPS_TO_HTTP
        } else {
            WINHTTP_OPTION_REDIRECT_POLICY_NEVER
        };
        if !opened.set(WINHTTP_OPTION_REDIRECT_POLICY, redirects) {
            return Err(last_error("setting the redirect rule"));
        }
        if target.secure {
            if check_revocation
                && !opened.set(WINHTTP_OPTION_ENABLE_FEATURE, WINHTTP_ENABLE_SSL_REVOCATION)
            {
                return Err(last_error("asking for the revocation check"));
            }
            // Returns the callback that was set before, which is none.
            let _ = WinHttpSetStatusCallback(
                opened.0,
                Some(on_secure_failure),
                WINHTTP_CALLBACK_FLAG_SECURE_FAILURE,
                0,
            );
        }
        let sent = WinHttpSendRequest(
            opened.0,
            if header_units == 0 {
                std::ptr::null()
            } else {
                headers.as_ptr()
            },
            header_units,
            if body.is_empty() {
                std::ptr::null()
            } else {
                body.as_ptr().cast()
            },
            body_length,
            body_length,
            std::ptr::from_ref(failure) as usize,
        ) != 0;
        let answered = sent && WinHttpReceiveResponse(opened.0, std::ptr::null_mut()) != 0;
        // The error is read before any other call can replace it.
        let error = (!answered).then(|| {
            last_error(if sent {
                "waiting for the answer"
            } else {
                "connecting to the server"
            })
        });
        let _ = WinHttpSetStatusCallback(opened.0, None, WINHTTP_CALLBACK_FLAG_SECURE_FAILURE, 0);
        if let Some(error) = error {
            return Err(error);
        }
        let status = status_of(&opened)?;
        Ok(Response {
            request: opened,
            _connection: connection,
            _session: session,
            status,
        })
    }
}
