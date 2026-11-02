//! Host standard streams granted to a selected top-level command.
use std::io::{Read, Write};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use crate::runtime::OpResult;

#[derive(Default)]
pub(crate) struct Stdio { pub enabled: AtomicBool }

struct Stop(Arc<AtomicBool>);
impl Drop for Stop { fn drop(&mut self) { self.0.store(true, Ordering::Release); } }

impl Stdio {
    pub async fn read(self: Arc<Self>) -> OpResult {
        if !self.enabled.load(Ordering::Acquire) { return OpResult::Bytes(Vec::new()); }
        let stop = Stop(Arc::new(AtomicBool::new(false)));
        let stopped = stop.0.clone();
        let result = tokio::task::spawn_blocking(move || {
            let mut buffer = vec![0; 16 * 1024];
            while !stopped.load(Ordering::Acquire) && self.enabled.load(Ordering::Acquire) {
                let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
                let ready = unsafe { libc::poll(&mut fd, 1, 50) };
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted { continue; }
                    return OpResult::error("ERR_PLY_IO", error.to_string());
                }
                if ready == 0 { continue; }
                return match std::io::stdin().read(&mut buffer) {
                    Ok(length) => { buffer.truncate(length); OpResult::Bytes(buffer) }
                    Err(error) => OpResult::error("ERR_PLY_IO", error.to_string()),
                };
            }
            OpResult::Bytes(Vec::new())
        }).await.unwrap_or_else(|error| OpResult::error("ERR_PLY_IO", error.to_string()));
        drop(stop);
        result
    }

    pub fn write(&self, fd: u64, text: &str) -> OpResult {
        let result = if fd == 2 {
            let mut stream = std::io::stderr().lock();
            stream.write_all(text.as_bytes()).and_then(|_| stream.flush())
        } else {
            let mut stream = std::io::stdout().lock();
            stream.write_all(text.as_bytes()).and_then(|_| stream.flush())
        };
        match result { Ok(()) => OpResult::Json(serde_json::Value::Null), Err(error) => OpResult::error("ERR_PLY_IO", error.to_string()) }
    }
}
