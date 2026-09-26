use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const OWNER_ONLY: u32 = 0o600;

/// Replaces `path` with `bytes`, readable only by its owner: written to a sibling, synced and
/// renamed over the original, so a crash leaves either the old file or the new one.
///
/// # Errors
///
/// Fails when the directory cannot be created or the file cannot be written.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("crownconnect.tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(OWNER_ONLY)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

/// The file's contents, or `None` when it does not exist yet.
///
/// # Errors
///
/// Fails when the file exists but cannot be read.
pub fn read_if_present(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn files_are_replaced_whole_and_private() -> std::io::Result<()> {
        let path = std::env::temp_dir()
            .join(format!("crownconnect-private-{}", std::process::id()))
            .join("file");
        assert_eq!(read_if_present(&path)?, None);
        write_private(&path, b"first")?;
        write_private(&path, b"second")?;
        assert_eq!(read_if_present(&path)?.as_deref(), Some(&b"second"[..]));
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            OWNER_ONLY
        );
        std::fs::remove_dir_all(path.parent().unwrap_or(&path))
    }
}
