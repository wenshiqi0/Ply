//! Watching host directories while a real process runs (macOS FSEvents).
//!
//! The stream is file-level (`FileEvents`) and marks events caused by wendy
//! itself (`MarkSelf`), so writes wendy makes through its own IO layer, or
//! to its trace file, are not mistaken for the process's. Events from other
//! programs writing the same directories at the same time cannot be told
//! apart; attribution assumes the process is the only other writer.

use std::ffi::{CStr, OsStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

type CFRef = *const c_void;
type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[repr(C)]
struct StreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

#[repr(C)]
struct CFArrayCallBacks {
    _opaque: [u8; 0],
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: CFRef,
        callback: Callback,
        context: *const StreamContext,
        paths: CFRef,
        since_when: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamFlushSync(stream: *mut c_void);
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeArrayCallBacks: CFArrayCallBacks;
    fn CFStringCreateWithBytes(alloc: CFRef, bytes: *const u8, len: isize, encoding: u32, external: u8) -> CFRef;
    fn CFArrayCreate(alloc: CFRef, values: *const CFRef, count: isize, callbacks: *const CFArrayCallBacks) -> CFRef;
    fn CFRelease(cf: CFRef);
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

const SINCE_NOW: u64 = u64::MAX;
const UTF8: u32 = 0x0800_0100;
const NO_DEFER: u32 = 0x02;
const FILE_EVENTS: u32 = 0x10;
const MARK_SELF: u32 = 0x20;

pub(crate) const MUST_SCAN_SUBDIRS: u32 = 0x01;
pub(crate) const USER_DROPPED: u32 = 0x02;
pub(crate) const KERNEL_DROPPED: u32 = 0x04;
pub(crate) const CREATED: u32 = 0x100;
pub(crate) const REMOVED: u32 = 0x200;
pub(crate) const INODE_META: u32 = 0x400;
pub(crate) const RENAMED: u32 = 0x800;
pub(crate) const MODIFIED: u32 = 0x1000;
pub(crate) const IS_DIR: u32 = 0x20000;
pub(crate) const OWN_EVENT: u32 = 0x80000;

type Sink = Mutex<Vec<(PathBuf, u32)>>;

extern "C" fn on_events(
    _stream: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _ids: *const u64,
) {
    // Runs on the stream's dispatch queue. Must not unwind into C.
    let sink = unsafe { &*(info as *const Sink) };
    let paths = paths as *const *const c_char;
    let Ok(mut events) = sink.lock() else { return };
    for i in 0..count {
        let (path, flag) = unsafe { (CStr::from_ptr(*paths.add(i)), *flags.add(i)) };
        events.push((PathBuf::from(OsStr::from_bytes(path.to_bytes())), flag));
    }
}

/// A running FSEvents stream over some directories.
pub(crate) struct Watcher {
    stream: *mut c_void,
    queue: *mut c_void,
    sink: *mut Sink,
}

// The stream and queue are only touched again by `finish`, which takes
// ownership; FSEvents itself is thread-safe.
unsafe impl Send for Watcher {}

impl Watcher {
    pub fn start(roots: &[PathBuf]) -> Option<Watcher> {
        if roots.is_empty() {
            return None;
        }
        unsafe {
            let strings: Vec<CFRef> = roots.iter().map(|r| cf_string(r)).collect();
            let array = CFArrayCreate(std::ptr::null(), strings.as_ptr(), strings.len() as isize, &kCFTypeArrayCallBacks);
            for s in &strings {
                CFRelease(*s);
            }
            let sink = Box::into_raw(Box::new(Sink::default()));
            let context = StreamContext {
                version: 0,
                info: sink as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let stream = FSEventStreamCreate(
                std::ptr::null(),
                on_events,
                &context,
                array,
                SINCE_NOW,
                0.0,
                NO_DEFER | FILE_EVENTS | MARK_SELF,
            );
            CFRelease(array);
            if stream.is_null() {
                drop(Box::from_raw(sink));
                return None;
            }
            let queue = dispatch_queue_create(c"wendy.fsevents".as_ptr(), std::ptr::null());
            FSEventStreamSetDispatchQueue(stream, queue);
            if FSEventStreamStart(stream) == 0 {
                FSEventStreamInvalidate(stream);
                FSEventStreamRelease(stream);
                dispatch_release(queue);
                drop(Box::from_raw(sink));
                return None;
            }
            Some(Watcher { stream, queue, sink })
        }
    }

    /// Deliver everything that happened so far, stop, and return the raw
    /// events (path, flags) in order, plus whether delivery is known to be
    /// complete. Blocks while FSEvents catches up.
    ///
    /// A flush alone does not wait for events still on their way from the
    /// kernel. So wendy writes `barrier` (which must be inside a watched
    /// directory) and flushes until that event arrives: event ids are global
    /// and delivered in order, so everything before it has arrived too.
    pub fn finish(self, barrier: &Path, deadline: Duration) -> (Vec<(PathBuf, u32)>, bool) {
        let started = Instant::now();
        let mut complete = std::fs::write(barrier, b"").is_ok();
        while complete {
            unsafe { FSEventStreamFlushSync(self.stream) };
            let seen = unsafe { &*self.sink }.lock().map(|e| e.iter().any(|(p, _)| p == barrier)).unwrap_or(false);
            if seen {
                break;
            }
            if started.elapsed() > deadline {
                complete = false;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = std::fs::remove_file(barrier);
        unsafe {
            FSEventStreamStop(self.stream);
            FSEventStreamInvalidate(self.stream);
            FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
            let sink = Box::from_raw(self.sink);
            (sink.into_inner().unwrap_or_default(), complete)
        }
    }
}

unsafe fn cf_string(path: &Path) -> CFRef {
    let bytes = path.as_os_str().as_bytes();
    unsafe { CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), bytes.len() as isize, UTF8, 0) }
}
