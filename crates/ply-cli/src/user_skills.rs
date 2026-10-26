//! The CLI exposes user-selected skill directories through read-only storage.
//! Metadata discovery and context construction stay in the skill plugin.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use ply_kernel::{DiskStorage, Io, MountMode};

pub fn mount(io: &Io, home: Option<PathBuf>) -> Result<Option<String>> {
    let Some(home) = home.filter(|path| path.is_absolute()) else { return Ok(None) };
    let root = home.join(".agents/skills");
    match std::fs::metadata(&root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("opening user skills directory"),
    }
    let at = root.to_str().context("user skills path must be UTF-8")?.to_string();
    let existing = io.mounts();
    // Explicit mounts retain their source and permissions.
    if existing.iter().any(|mount| mount.at == at) { return Ok(Some(at)); }

    let mut entries = std::fs::read_dir(&root)?
        .take(2049)
        .collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() > 2048 { bail!("user skills collection exceeds 2048 entries"); }
    entries.sort_by_key(|entry| entry.file_name());
    io.mount(&at, Arc::new(DiskStorage::open(&root)?), MountMode::ReadOnly)?;
    for entry in entries {
        if entry.file_name().to_string_lossy().starts_with('.') || !entry.file_type()?.is_symlink() {
            continue;
        }
        let target = entry.path();
        if !target.is_dir() || !target.join("SKILL.md").is_file() { continue; }
        let linked_at = PathBuf::from(&at).join(entry.file_name());
        let linked_at = linked_at.to_str().context("linked skill path must be UTF-8")?;
        if existing.iter().any(|mount| mount.at == linked_at) { continue; }
        // Mount only each linked skill itself, never its target's parent.
        io.mount(linked_at, Arc::new(DiskStorage::open(&target)?), MountMode::ReadOnly)?;
    }
    Ok(Some(at))
}
