use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use nix::sys::statfs::{FsType, OVERLAYFS_SUPER_MAGIC, Statfs, TMPFS_MAGIC};

/// Writes the whole file or nothing: readers never observe a half-written file,
/// because `rename` within one filesystem is atomic on Linux.
pub fn write_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary_path: PathBuf = temporary_sibling(path);
    let mut file: std::fs::File = std::fs::File::create(&temporary_path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    // Ownership: `drop` closes the file descriptor here (RAII, like a C++ destructor),
    // before the rename, instead of at the end of the scope.
    drop(file);
    std::fs::rename(&temporary_path, path)
}

fn temporary_sibling(path: &Path) -> PathBuf {
    let mut name: OsString = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".tmp");
    path.with_file_name(name)
}

/// Creates and deletes a probe file in `directory`: the only reliable test,
/// because permissions, read-only mounts and ACLs all play a part.
pub fn probe_writable(directory: &Path) -> std::io::Result<()> {
    let probe: PathBuf = directory.join(format!(".remoteplayer-write-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe")?;
    std::fs::remove_file(&probe)
}

/// tmpfs or an overlay (e.g. the Raspberry Pi OS overlay file system): what is
/// written there lives in RAM and is gone after a power-off.
pub fn is_volatile(directory: &Path) -> bool {
    let Ok(statistics): nix::Result<Statfs> = nix::sys::statfs::statfs(directory) else {
        return false;
    };
    let kind: FsType = statistics.filesystem_type();
    kind == TMPFS_MAGIC || kind == OVERLAYFS_SUPER_MAGIC
}

pub fn describe_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ReadOnlyFilesystem => "the filesystem is mounted read-only".to_string(),
        std::io::ErrorKind::PermissionDenied => "no permission to write".to_string(),
        std::io::ErrorKind::NotFound => "the directory does not exist".to_string(),
        _ => error.to_string(),
    }
}

pub fn is_write_refusal(error: &std::io::Error) -> bool {
    matches!(error.kind(), std::io::ErrorKind::ReadOnlyFilesystem | std::io::ErrorKind::PermissionDenied)
}

pub fn read_optional_to_string(path: &Path) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomically_replaces_content_and_leaves_no_temporary_file() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let target: PathBuf = directory.path().join("nested").join("file.json");
        write_atomically(&target, b"first").expect("first write");
        write_atomically(&target, b"second").expect("second write");
        assert_eq!(std::fs::read_to_string(&target).expect("read back"), "second");
        let leftovers: usize = std::fs::read_dir(target.parent().expect("parent")).expect("list").count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn probe_succeeds_in_a_writable_directory_and_leaves_nothing() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        probe_writable(directory.path()).expect("writable");
        assert_eq!(std::fs::read_dir(directory.path()).expect("list").count(), 0);
        assert!(probe_writable(&directory.path().join("missing")).is_err());
    }

    #[test]
    fn missing_file_reads_as_none() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let result: Option<String> = read_optional_to_string(&directory.path().join("absent")).expect("no error");
        assert!(result.is_none());
    }
}
