mod md5_cache;
mod names;
mod upload;

pub use names::{EntryName, NameError};
pub use upload::{UploadRequest, UploadResult};

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::StorageConfig;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("invalid name: {0}")]
    InvalidName(#[from] NameError),
    #[error("folder `{0}` does not exist")]
    FolderNotFound(String),
    #[error("file `{0}` does not exist")]
    FileNotFound(String),
    #[error("`{0}` already exists")]
    AlreadyExists(String),
    #[error("folder `{0}` is not empty")]
    FolderNotEmpty(String),
    #[error("`{0}` resolves outside of the upload root")]
    OutsideRoot(String),
    #[error("upload exceeds the limit of {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("not enough free space: {required} bytes required, {available} available")]
    InsufficientSpace { required: u64, available: u64 },
    #[error("the disk is full")]
    DiskFull,
    #[error("checksum mismatch: expected {expected}, received data has {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("`{0}` is not a valid hexadecimal MD5")]
    InvalidChecksum(String),
    #[error("upload interrupted: {0}")]
    Interrupted(String),
    #[error("storage I/O error: {0}")]
    Io(std::io::Error),
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> StorageError {
        if error.kind() == std::io::ErrorKind::StorageFull { StorageError::DiskFull } else { StorageError::Io(error) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileKind {
    Video,
    Audio,
    Subtitle,
    Image,
    Playlist,
    Other,
}

impl FileKind {
    pub fn from_name(name: &str) -> FileKind {
        let extension: String = Path::new(name)
            .extension()
            .and_then(|extension: &std::ffi::OsStr| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "mkv" | "mp4" | "m4v" | "avi" | "mov" | "webm" | "ts" | "m2ts" | "mts" | "mpg" | "mpeg" | "wmv" | "flv"
            | "3gp" | "ogv" | "vob" => FileKind::Video,
            "mp3" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wav" | "wma" | "ape" | "alac" | "dsf"
            | "mka" => FileKind::Audio,
            "srt" | "ass" | "ssa" | "sub" | "idx" | "vtt" | "sup" => FileKind::Subtitle,
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" => FileKind::Image,
            "m3u" | "m3u8" | "pls" | "cue" => FileKind::Playlist,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FolderInfo {
    pub name: String,
    pub modified_unix_ms: Option<u64>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    pub modified_unix_ms: Option<u64>,
    pub kind: FileKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct Md5Info {
    pub folder: String,
    pub name: String,
    pub size: u64,
    pub md5: String,
    pub cached: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct StorageUsage {
    pub root: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub reserved_bytes: u64,
}

/// The upload root and its first-level folders. Files are never stored in the
/// root itself and folders are never nested.
pub struct MediaLibrary {
    canonical_root: PathBuf,
    default_folder: EntryName,
    min_free_bytes: u64,
    max_upload_bytes: Option<u64>,
}

impl MediaLibrary {
    pub fn open(config: &StorageConfig) -> Result<MediaLibrary, StorageError> {
        let default_folder: EntryName = EntryName::parse(&config.default_folder)?;
        std::fs::create_dir_all(&config.upload_root)?;
        let canonical_root: PathBuf = std::fs::canonicalize(&config.upload_root)?;
        std::fs::create_dir_all(canonical_root.join(default_folder.as_str()))?;
        Ok(MediaLibrary {
            canonical_root,
            default_folder,
            min_free_bytes: config.min_free_bytes,
            max_upload_bytes: config.max_upload_bytes,
        })
    }

    pub fn default_folder(&self) -> &EntryName {
        &self.default_folder
    }

    async fn existing_folder_path(&self, folder: &EntryName) -> Result<PathBuf, StorageError> {
        let candidate: PathBuf = self.canonical_root.join(folder.as_str());
        let canonical: PathBuf = match tokio::fs::canonicalize(&candidate).await {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StorageError::FolderNotFound(folder.to_string()));
            }
            Err(error) => return Err(error.into()),
        };
        // A folder may be a symlink created by hand; it still has to point inside the root.
        if !canonical.starts_with(&self.canonical_root) || canonical == self.canonical_root {
            return Err(StorageError::OutsideRoot(folder.to_string()));
        }
        if !tokio::fs::metadata(&canonical).await?.is_dir() {
            return Err(StorageError::FolderNotFound(folder.to_string()));
        }
        Ok(canonical)
    }

    async fn ensure_folder_path(&self, folder: &EntryName) -> Result<PathBuf, StorageError> {
        match self.existing_folder_path(folder).await {
            Err(StorageError::FolderNotFound(_)) => {
                tokio::fs::create_dir_all(self.canonical_root.join(folder.as_str())).await?;
                self.existing_folder_path(folder).await
            }
            other => other,
        }
    }

    pub async fn existing_file_path(&self, folder: &EntryName, name: &EntryName) -> Result<PathBuf, StorageError> {
        let path: PathBuf = self.existing_folder_path(folder).await?.join(name.as_str());
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => Ok(path),
            Ok(_) => Err(StorageError::FileNotFound(format!("{folder}/{name}"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::FileNotFound(format!("{folder}/{name}")))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn list_folders(&self) -> Result<Vec<FolderInfo>, StorageError> {
        let mut folders: Vec<FolderInfo> = Vec::new();
        let mut entries: tokio::fs::ReadDir = tokio::fs::read_dir(&self.canonical_root).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(name): Option<String> = entry.file_name().to_str().map(str::to_string) else { continue };
            if name.starts_with('.') {
                continue;
            }
            let metadata: std::fs::Metadata = match tokio::fs::metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_dir() {
                let is_default: bool = name == self.default_folder.as_str();
                folders.push(FolderInfo { name, modified_unix_ms: modified_unix_ms(&metadata), is_default });
            }
        }
        folders.sort_by(|left: &FolderInfo, right: &FolderInfo| left.name.cmp(&right.name));
        Ok(folders)
    }

    pub async fn create_folder(&self, folder: &EntryName) -> Result<(), StorageError> {
        let path: PathBuf = self.canonical_root.join(folder.as_str());
        match tokio::fs::create_dir(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(StorageError::AlreadyExists(folder.to_string()))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Only empty folders can be deleted, so one request can never wipe a library.
    pub async fn delete_folder(&self, folder: &EntryName) -> Result<(), StorageError> {
        let path: PathBuf = self.existing_folder_path(folder).await?;
        match tokio::fs::remove_dir(&path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                Err(StorageError::FolderNotEmpty(folder.to_string()))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub async fn list_files(&self, folder: &EntryName) -> Result<Vec<FileInfo>, StorageError> {
        let folder_path: PathBuf = self.existing_folder_path(folder).await?;
        let mut files: Vec<FileInfo> = Vec::new();
        let mut entries: tokio::fs::ReadDir = tokio::fs::read_dir(&folder_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(name): Option<String> = entry.file_name().to_str().map(str::to_string) else { continue };
            // Hidden names include our own in-progress uploads (".name.part-...").
            if name.starts_with('.') {
                continue;
            }
            let metadata: std::fs::Metadata = match tokio::fs::metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.is_file() {
                let kind: FileKind = FileKind::from_name(&name);
                files.push(FileInfo {
                    name,
                    size: metadata.len(),
                    modified_unix_ms: modified_unix_ms(&metadata),
                    kind,
                });
            }
        }
        files.sort_by(|left: &FileInfo, right: &FileInfo| left.name.cmp(&right.name));
        Ok(files)
    }

    pub async fn delete_file(&self, folder: &EntryName, name: &EntryName) -> Result<(), StorageError> {
        let path: PathBuf = self.existing_file_path(folder, name).await?;
        tokio::fs::remove_file(&path).await?;
        Ok(())
    }

    pub async fn move_file(
        &self,
        source_folder: &EntryName,
        source_name: &EntryName,
        target_folder: &EntryName,
        target_name: &EntryName,
        overwrite: bool,
    ) -> Result<FileInfo, StorageError> {
        let source: PathBuf = self.existing_file_path(source_folder, source_name).await?;
        let target: PathBuf = self.ensure_folder_path(target_folder).await?.join(target_name.as_str());
        if !overwrite && tokio::fs::try_exists(&target).await? {
            return Err(StorageError::AlreadyExists(format!("{target_folder}/{target_name}")));
        }
        tokio::fs::rename(&source, &target).await?;
        let metadata: std::fs::Metadata = tokio::fs::metadata(&target).await?;
        Ok(FileInfo {
            name: target_name.to_string(),
            size: metadata.len(),
            modified_unix_ms: modified_unix_ms(&metadata),
            kind: FileKind::from_name(target_name.as_str()),
        })
    }

    pub async fn file_md5(&self, folder: &EntryName, name: &EntryName) -> Result<Md5Info, StorageError> {
        let path: PathBuf = self.existing_file_path(folder, name).await?;
        // Hashing gigabytes is blocking CPU + disk work; spawn_blocking moves it to
        // a thread pool reserved for that, so the async worker threads stay free.
        let (md5, size, cached): (String, u64, bool) =
            tokio::task::spawn_blocking(move || -> std::io::Result<(String, u64, bool)> {
                md5_cache::md5_with_cache(&path)
            })
            .await
            .map_err(|join_error: tokio::task::JoinError| StorageError::Io(std::io::Error::other(join_error)))??;
        Ok(Md5Info { folder: folder.to_string(), name: name.to_string(), size, md5, cached })
    }

    // On 64-bit targets these fields already are u64 and clippy calls the
    // conversion useless; on 32-bit ARM they are u32 and it is required.
    #[allow(clippy::useless_conversion)]
    pub fn usage(&self) -> Result<StorageUsage, StorageError> {
        let statistics: nix::sys::statvfs::Statvfs = nix::sys::statvfs::statvfs(&self.canonical_root)
            .map_err(|errno: nix::errno::Errno| StorageError::Io(std::io::Error::from(errno)))?;
        let fragment_size: u64 = u64::from(statistics.fragment_size());
        Ok(StorageUsage {
            root: self.canonical_root.display().to_string(),
            total_bytes: u64::from(statistics.blocks()) * fragment_size,
            available_bytes: u64::from(statistics.blocks_available()) * fragment_size,
            reserved_bytes: self.min_free_bytes,
        })
    }

    fn check_space_for(&self, bytes: u64) -> Result<(), StorageError> {
        if let Some(limit) = self.max_upload_bytes {
            if bytes > limit {
                return Err(StorageError::TooLarge { limit });
            }
        }
        let available: u64 = self.usage()?.available_bytes;
        let required: u64 = bytes.saturating_add(self.min_free_bytes);
        if required > available {
            return Err(StorageError::InsufficientSpace { required, available });
        }
        Ok(())
    }
}

fn modified_unix_ms(metadata: &std::fs::Metadata) -> Option<u64> {
    let modified: SystemTime = metadata.modified().ok()?;
    let since_epoch: std::time::Duration = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since_epoch.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library_in(directory: &Path) -> MediaLibrary {
        let config: StorageConfig = StorageConfig {
            upload_root: directory.to_path_buf(),
            default_folder: "inbox".to_string(),
            min_free_bytes: 0,
            max_upload_bytes: None,
        };
        MediaLibrary::open(&config).expect("open library")
    }

    fn name(raw: &str) -> EntryName {
        EntryName::parse(raw).expect("valid name")
    }

    #[tokio::test]
    async fn default_folder_is_created_and_listed() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path());
        let folders: Vec<FolderInfo> = library.list_folders().await.expect("list");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].name, "inbox");
        assert!(folders[0].is_default);
    }

    #[tokio::test]
    async fn files_are_listed_without_hidden_entries_and_with_kind() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path());
        std::fs::write(directory.path().join("inbox/movie.MKV"), b"x").expect("seed");
        std::fs::write(directory.path().join("inbox/.movie.mkv.part-1"), b"x").expect("seed");
        let files: Vec<FileInfo> = library.list_files(&name("inbox")).await.expect("list");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].kind, FileKind::Video);
    }

    #[tokio::test]
    async fn symlinked_folder_pointing_outside_root_is_refused() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let outside: tempfile::TempDir = tempfile::tempdir().expect("outside dir");
        let library: MediaLibrary = library_in(directory.path());
        std::os::unix::fs::symlink(outside.path(), directory.path().join("escape")).expect("symlink");
        let result: Result<Vec<FileInfo>, StorageError> = library.list_files(&name("escape")).await;
        assert!(matches!(result, Err(StorageError::OutsideRoot(_))));
    }

    #[tokio::test]
    async fn non_empty_folder_cannot_be_deleted() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path());
        std::fs::write(directory.path().join("inbox/a.mp4"), b"x").expect("seed");
        let result: Result<(), StorageError> = library.delete_folder(&name("inbox")).await;
        assert!(matches!(result, Err(StorageError::FolderNotEmpty(_))));
    }

    #[tokio::test]
    async fn md5_is_computed_then_served_from_cache() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path());
        std::fs::write(directory.path().join("inbox/hello.txt"), b"hello").expect("seed");
        let first: Md5Info = library.file_md5(&name("inbox"), &name("hello.txt")).await.expect("md5");
        assert_eq!(first.md5, "5d41402abc4b2a76b9719d911017c592");
        let second: Md5Info = library.file_md5(&name("inbox"), &name("hello.txt")).await.expect("md5");
        assert_eq!(second.md5, first.md5);
        // tmpfs and some container filesystems refuse user xattrs; the cache is best-effort.
        if xattr::SUPPORTED_PLATFORM && md5_cache::xattrs_work_in(directory.path()) {
            assert!(second.cached);
        }
    }
}
