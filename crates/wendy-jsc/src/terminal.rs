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
    frame: Mutex<Frame>,
}

#[derive(Default)]
struct Frame {
    lines: Vec<String>,
    columns: u64,
    rows: u64,
}

fn style_code(value: &Value) -> Result<&'static str, OpResult> {
    match value
        .get("style")
        .and_then(Value::as_str)
        .unwrap_or("normal")
    {
        "normal" => Ok(""),
        "muted" => Ok("\x1b[90m"),
        "accent" => Ok("\x1b[36m"),
        "title" => Ok("\x1b[1;36m"),
        "warning" => Ok("\x1b[33m"),
        "error" => Ok("\x1b[31m"),
        "code" => Ok("\x1b[37;48;5;236m"),
        _ => Err(failure("unknown terminal text style")),
    }
}

fn styled_span(value: &Value, base: &str) -> Result<String, OpResult> {
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 8192)
        .ok_or_else(|| failure("span text must be a string of at most 8192 bytes"))?;
    let mut out = format!("\x1b[0m{base}{}", style_code(value)?);
    for (key, code) in [("bold", 1), ("italic", 3), ("underline", 4), ("strike", 9)] {
        match value.get(key) {
            None | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => out.push_str(&format!("\x1b[{code}m")),
            _ => return Err(failure("span emphasis flags must be booleans")),
        }
    }
    out.push_str(&visible(text));
    out.push_str("\x1b[0m");
    Ok(out)
}

fn styled_line(value: &Value) -> Result<String, OpResult> {
    if let Some(text) = value.as_str() {
        if text.len() > 8192 {
            return Err(failure("terminal line is too large"));
        }
        return Ok(visible(text));
    }
    let base = style_code(value)?;
    if let Some(spans) = value.get("spans") {
        if value.get("text").is_some() {
            return Err(failure("use text or spans, not both"));
        }
        let spans = spans
            .as_array()
            .filter(|s| s.len() <= 4096)
            .ok_or_else(|| failure("spans must be an array of at most 4096 runs"))?;
        let mut bytes = 0;
        let mut out = String::new();
        for span in spans {
            bytes += span
                .get("text")
                .and_then(Value::as_str)
                .map(str::len)
                .unwrap_or(8193);
            if bytes > 8192 {
                return Err(failure("terminal line is too large"));
            }
            out.push_str(&styled_span(span, base)?);
        }
        return Ok(out);
    }
    styled_span(value, "")
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
            let _ = self.write(b"\x1b[0m\x1b[?2004l\x1b[?25h\x1b[?1049l");
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
                    frame: Mutex::new(Frame::default()),
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
                if lines.len() > 100 {
                    return failure("terminal frame is too large");
                }
                let size = session.size();
                let rows = size["rows"].as_u64().unwrap();
                let columns = size["columns"].as_u64().unwrap();
                let cursor = match args.get("options").and_then(|v| v.get("cursor")) {
                    None | Some(Value::Null) => None,
                    Some(value) => match (
                        value.get("row").and_then(Value::as_u64),
                        value.get("column").and_then(Value::as_u64),
                    ) {
                        (Some(row), Some(column)) if row < rows && column < columns => {
                            Some((row, column))
                        }
                        _ => return failure("cursor must be a valid zero-based terminal cell"),
                    },
                };
                let mut rendered = Vec::new();
                for line in lines.iter().take(rows as usize) {
                    match styled_line(line) {
                        Ok(line) => rendered.push(line),
                        Err(error) => return error,
                    }
                }
                let mut previous = session.frame.lock().unwrap();
                let clear = previous.lines.is_empty()
                    || previous.columns != columns
                    || previous.rows != rows
                    || args
                        .get("options")
                        .and_then(|v| v.get("clear"))
                        .and_then(Value::as_bool)
                        == Some(true);
                let mut frame = String::from("\x1b[?25l");
                if clear {
                    frame.push_str("\x1b[H\x1b[2J");
                }
                for row in 0..rendered.len().max(previous.lines.len()).min(rows as usize) {
                    let line = rendered.get(row).map(String::as_str).unwrap_or("");
                    if clear || previous.lines.get(row).map(String::as_str) != Some(line) {
                        frame.push_str(&format!("\x1b[{};1H\x1b[2K{}\x1b[0m", row + 1, line));
                    }
                }
                if let Some((row, column)) = cursor {
                    frame.push_str(&format!("\x1b[{};{}H\x1b[?25h", row + 1, column + 1));
                }
                match session.write(frame.as_bytes()) {
                    Ok(()) => {
                        previous.lines = rendered;
                        previous.rows = rows;
                        previous.columns = columns;
                        OpResult::Json(size)
                    }
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
