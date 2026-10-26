//! Resolve executable files without evaluating another shell or importing its state.
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::Ordering};

use ply_libc::{ExecSpec, Io};
use ply_sh::{ExternalCommands, UtilCtx};

pub(crate) fn bin_path() -> String {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/usr/local/sbin",
        "/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.extend([".local/bin", ".cargo/bin", ".bun/bin", "bin", ".npm-global/bin", ".volta/bin",
            ".asdf/shims", ".pyenv/shims"].map(|dir| home.join(dir)));
    }
    let mut unique = Vec::new();
    for dir in dirs { if !unique.contains(&dir) { unique.push(dir); } }
    std::env::join_paths(unique).unwrap_or_default().to_string_lossy().into_owned()
}

pub(crate) struct HostCommands {
    io: Io,
    initial_home: Option<String>,
    host_home: Option<String>,
}

impl HostCommands {
    pub fn new(io: Io, initial_home: Option<String>) -> Self {
        Self { io, initial_home, host_home: std::env::var("HOME").ok() }
    }

    fn candidate(&self, path: &Path, cwd: &str) -> std::io::Result<PathBuf> {
        let logical = ply_libc::path::resolve(cwd, &path.to_string_lossy());
        match self.io.host_path(&logical) {
            Ok(host) => Ok(host),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                if path.is_absolute() { Ok(path.to_path_buf()) }
                else { Ok(self.io.host_path(cwd)?.join(path)) }
            }
            Err(error) => Err(error),
        }
    }

    fn locate(&self, name: &str, search_path: &str, cwd: &str) -> Result<(String, PathBuf), (i32, String)> {
        let explicit = name.contains('/');
        let candidates = if explicit { vec![PathBuf::from(name)] } else {
            std::env::split_paths(search_path).map(|dir| dir.join(name)).collect()
        };
        let mut denied = false;
        for path in candidates {
            let host = match self.candidate(&path, cwd) {
                Ok(host) => host,
                Err(error) if explicit => return Err((126, error.to_string())),
                Err(_) => continue,
            };
            let metadata = match std::fs::metadata(&host) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => { denied = true; continue; }
                Err(_) => continue,
            };
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                denied = true; continue;
            }
            return Ok((path.display().to_string(), host));
        }
        Err(if denied { (126, "permission denied".into()) } else { (127, "command not found".into()) })
    }
}

#[async_trait::async_trait]
impl ExternalCommands for HostCommands {
    fn resolve(&self, name: &str, search_path: &str, cwd: &str) -> Option<String> {
        self.locate(name, search_path, cwd).ok().map(|(display, _)| display)
    }

    async fn run(&self, ctx: UtilCtx<'_>) -> i32 {
        let (_, program) = match self.locate(ctx.name, ctx.search_path, ctx.fs.cwd()) {
            Ok(found) => found,
            Err((status, message)) => { ctx.error(&message); return status; }
        };
        let mut env = ctx.env.clone();
        // A native CLI uses the host account's config; do not mount credentials
        // into the plugin namespace or copy arbitrary host environment variables.
        if env.get("HOME") == self.initial_home.as_ref() {
            if let Some(home) = &self.host_home { env.insert("HOME".into(), home.clone()); }
        }
        let out = ctx.fds.out.clone();
        let err = ctx.fds.err.clone();
        let spec = ExecSpec { argv: std::iter::once(program.display().to_string()).chain(ctx.args.iter().cloned()).collect(),
            cwd: ctx.fs.cwd().into(), env, clear_env: true, stdin: Some(ctx.stdin()), ..ExecSpec::default() };
        match ctx.fs.io().exec_with_sinks(spec, Some(Arc::new(move |bytes| out.write(bytes))), Some(Arc::new(move |bytes| err.write(bytes)))).await {
            Ok(result) => {
                if result.truncated { ctx.output_truncated.store(true, Ordering::Relaxed); }
                result.code.unwrap_or_else(|| 128 + result.signal.unwrap_or(0))
            }
            Err(error) => { ctx.error(&error.to_string()); if error.kind() == std::io::ErrorKind::NotFound { 127 } else { 126 } }
        }
    }
}
