//! An explicitly opened, real controlling terminal. Never reads stdin.
//! The session restores termios even when its realm or owning call fails.
use std::{
    fs::{File, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use crate::runtime::OpResult;
use serde_json::{Value, json};

static IN_USE: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(crate) struct Terminal {
    session: Mutex<Option<Arc<Session>>>,
}

struct Session {
    id: u64,
    file: File,
    original: libc::termios,
    active: AtomicBool,
    reader: tokio::sync::Mutex<()>,
    utf8: Mutex<Vec<u8>>,
}

impl Session {
    fn size(&self) -> Value {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), libc::TIOCGWINSZ, &mut size);
        }
        json!({ "columns": if size.ws_col == 0 { 80 } else { (size.ws_col as usize).min(240) },
            "rows": if size.ws_row == 0 { 24 } else { (size.ws_row as usize).min(100) } })
    }

    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = unsafe { libc::write(self.file.as_raw_fd(), rest.as_ptr().cast(), rest.len()) };
            if n > 0 {
                rest = &rest[n as usize..];
                continue;
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) || Instant::now() >= deadline
            {
                return Err(error);
            }
            let mut poll = libc::pollfd {
                fd: self.file.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut poll, 1, 10);
            }
        }
        Ok(())
    }

    fn close(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            let _ = self.write(b"\x1b[?2004l\x1b[?25h\x1b[?1049l");
            unsafe {
                libc::tcsetattr(self.file.as_raw_fd(), libc::TCSANOW, &self.original);
            }
            IN_USE.store(false, Ordering::Release);
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(session) = self.session.get_mut().unwrap().take() {
            session.close();
        }
    }
}

fn failure(error: impl std::fmt::Display) -> OpResult {
    OpResult::error("ERR_WENDY_TERMINAL", error.to_string())
}

// Treat model/command text as data, including ANSI/OSC and bidi controls.
fn visible(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if c == '\t' {
            out.push_str("    ");
        } else if c.is_control()
            || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

impl Terminal {
    pub(crate) fn release(&self) {
        if let Some(session) = self.session.lock().unwrap().take() {
            session.close();
        }
    }

    fn get(&self, args: &Value) -> Result<Arc<Session>, OpResult> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .filter(|s| {
                s.active.load(Ordering::Acquire)
                    && Some(s.id) == args.get("id").and_then(Value::as_u64)
            })
            .cloned()
            .ok_or_else(|| failure("terminal session is not open"))
    }

    pub(crate) fn call(&self, op: &str, args: Value) -> OpResult {
        if op == "terminal.open" {
            let mut slot = self.session.lock().unwrap();
            if slot.is_some()
                || IN_USE
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return failure("another terminal UI is already active");
            }
            let open = || -> io::Result<Session> {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK | libc::O_CLOEXEC)
                    .open("/dev/tty")?;
                let fd = file.as_raw_fd();
                if unsafe { libc::isatty(fd) } != 1
                    || unsafe { libc::tcgetpgrp(fd) } != unsafe { libc::getpgrp() }
                {
                    return Err(io::Error::other(
                        "TUI requires the foreground controlling terminal",
                    ));
                }
                let mut original = unsafe { std::mem::zeroed() };
                if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                let mut raw = original;
                unsafe {
                    libc::cfmakeraw(&mut raw);
                }
                if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Session {
                    id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
                    file,
                    original,
                    active: AtomicBool::new(true),
                    reader: tokio::sync::Mutex::new(()),
                    utf8: Mutex::new(Vec::new()),
                })
            };
            return match open() {
                Ok(session) => {
                    if let Err(error) = session.write(b"\x1b[?1049h\x1b[?25l\x1b[?2004h") {
                        return failure(error);
                    }
                    let mut result = session.size();
                    result["id"] = json!(session.id);
                    *slot = Some(Arc::new(session));
                    OpResult::Json(result)
                }
                Err(error) => {
                    IN_USE.store(false, Ordering::Release);
                    failure(format!("cannot open /dev/tty: {error}"))
                }
            };
        }
        let session = match self.get(&args) {
            Ok(session) => session,
            Err(error) => return error,
        };
        match op {
            "terminal.close" => {
                session.close();
                self.session.lock().unwrap().take();
                OpResult::Json(Value::Null)
            }
            "terminal.flush" => {
                unsafe {
                    libc::tcflush(session.file.as_raw_fd(), libc::TCIFLUSH);
                }
                session.utf8.lock().unwrap().clear();
                OpResult::Json(Value::Null)
            }
            "terminal.render" => {
                let Some(lines) = args.get("lines").and_then(Value::as_array) else {
                    return failure("lines must be an array of strings");
                };
                if lines.len() > 100
                    || lines
                        .iter()
                        .any(|s| s.as_str().is_none_or(|s| s.len() > 8192))
                {
                    return failure("terminal frame is too large");
                }
                let mut frame = String::from("\x1b[H\x1b[2J");
                let limit = session.size()["rows"].as_u64().unwrap() as usize;
                for (i, line) in lines.iter().take(limit).enumerate() {
                    if i > 0 {
                        frame.push_str("\r\n");
                    }
                    frame.push_str(&visible(line.as_str().unwrap()));
                }
                match session.write(frame.as_bytes()) {
                    Ok(()) => OpResult::Json(session.size()),
                    Err(error) => failure(error),
                }
            }
            _ => failure("unknown terminal operation"),
        }
    }

    pub(crate) async fn read(&self, args: Value) -> OpResult {
        let session = match self.get(&args) {
            Ok(session) => session,
            Err(error) => return error,
        };
        let Some(timeout) = args
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|n| (1..=1000).contains(n))
        else {
            return failure("read timeout_ms must be in 1..1000");
        };
        let _reader = session.reader.lock().await;
        let deadline = Instant::now() + Duration::from_millis(timeout);
        loop {
            if !session.active.load(Ordering::Acquire) {
                return failure("terminal session was closed");
            }
            let mut bytes = [0u8; 4096];
            let count = unsafe {
                libc::read(
                    session.file.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count > 0 {
                let mut pending = session.utf8.lock().unwrap();
                pending.extend_from_slice(&bytes[..count as usize]);
                let n = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(_) => pending.len(),
                };
                let data = String::from_utf8_lossy(&pending[..n]).into_owned();
                pending.drain(..n);
                let mut result = session.size();
                result["data"] = json!(data);
                return OpResult::Json(result);
            }
            if count == 0 {
                return failure("terminal input was closed");
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return failure(error);
            }
            if Instant::now() >= deadline {
                let mut result = session.size();
                result["data"] = json!("");
                return OpResult::Json(result);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
