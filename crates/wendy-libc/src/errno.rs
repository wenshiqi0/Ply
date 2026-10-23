//! One error vocabulary for both outlets: sh prints [`Errno::message`]
//! (`No such file or directory`), node surfaces [`Errno::code`] (`ENOENT`).

use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Errno {
    ENOENT,
    EACCES,
    EEXIST,
    ENOTDIR,
    EISDIR,
    ENOTEMPTY,
    EROFS,
    EXDEV,
    EINVAL,
    EIO,
}

impl Errno {
    pub fn code(self) -> &'static str {
        match self {
            Errno::ENOENT => "ENOENT",
            Errno::EACCES => "EACCES",
            Errno::EEXIST => "EEXIST",
            Errno::ENOTDIR => "ENOTDIR",
            Errno::EISDIR => "EISDIR",
            Errno::ENOTEMPTY => "ENOTEMPTY",
            Errno::EROFS => "EROFS",
            Errno::EXDEV => "EXDEV",
            Errno::EINVAL => "EINVAL",
            Errno::EIO => "EIO",
        }
    }

    /// The `strerror` text, as bash and coreutils print it.
    pub fn message(self) -> &'static str {
        match self {
            Errno::ENOENT => "No such file or directory",
            Errno::EACCES => "Permission denied",
            Errno::EEXIST => "File exists",
            Errno::ENOTDIR => "Not a directory",
            Errno::EISDIR => "Is a directory",
            Errno::ENOTEMPTY => "Directory not empty",
            Errno::EROFS => "Read-only file system",
            Errno::EXDEV => "Invalid cross-device link",
            Errno::EINVAL => "Invalid argument",
            Errno::EIO => "Input/output error",
        }
    }

    /// libuv's text, as node prints it (`ENOENT: no such file or directory, open 'x'`).
    pub fn uv_message(self) -> &'static str {
        match self {
            Errno::ENOENT => "no such file or directory",
            Errno::EACCES => "permission denied",
            Errno::EEXIST => "file already exists",
            Errno::ENOTDIR => "not a directory",
            Errno::EISDIR => "illegal operation on a directory",
            Errno::ENOTEMPTY => "directory not empty",
            Errno::EROFS => "read-only file system",
            Errno::EXDEV => "cross-device link not permitted",
            Errno::EINVAL => "invalid argument",
            Errno::EIO => "i/o error",
        }
    }

    /// Linux errno numbers (node exposes them negated as `err.errno`).
    pub fn number(self) -> i32 {
        match self {
            Errno::ENOENT => 2,
            Errno::EACCES => 13,
            Errno::EEXIST => 17,
            Errno::ENOTDIR => 20,
            Errno::EISDIR => 21,
            Errno::ENOTEMPTY => 39,
            Errno::EROFS => 30,
            Errno::EXDEV => 18,
            Errno::EINVAL => 22,
            Errno::EIO => 5,
        }
    }

    pub fn from_kind(kind: io::ErrorKind) -> Self {
        use io::ErrorKind as K;
        match kind {
            K::NotFound => Errno::ENOENT,
            K::PermissionDenied => Errno::EACCES,
            K::AlreadyExists => Errno::EEXIST,
            K::NotADirectory => Errno::ENOTDIR,
            K::IsADirectory => Errno::EISDIR,
            K::DirectoryNotEmpty => Errno::ENOTEMPTY,
            K::ReadOnlyFilesystem => Errno::EROFS,
            K::CrossesDevices => Errno::EXDEV,
            K::InvalidInput | K::InvalidData => Errno::EINVAL,
            _ => Errno::EIO,
        }
    }
}

/// A failed system call: the errno plus the layer's own detail (e.g. which
/// policy rule denied it), which goes to the trace rather than to stderr.
#[derive(Debug, Clone)]
pub struct SysError {
    pub errno: Errno,
    pub detail: String,
}

impl From<io::Error> for SysError {
    fn from(err: io::Error) -> Self {
        SysError { errno: Errno::from_kind(err.kind()), detail: err.to_string() }
    }
}

impl SysError {
    pub fn new(errno: Errno, detail: impl Into<String>) -> Self {
        SysError { errno, detail: detail.into() }
    }
}

impl std::fmt::Display for SysError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.errno.message())
    }
}

impl std::error::Error for SysError {}
