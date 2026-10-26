use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    fn missing_file_reads_as_none() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let result: Option<String> = read_optional_to_string(&directory.path().join("absent")).expect("no error");
        assert!(result.is_none());
    }
}
