use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use md5::{Digest, Md5};
use serde::Serialize;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::storage::{EntryName, MediaLibrary, StorageError, md5_cache};

const WRITE_BUFFER_BYTES: usize = 1024 * 1024;

static UPLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct UploadRequest {
    pub folder: EntryName,
    pub name: EntryName,
    pub overwrite: bool,
    pub expected_md5: Option<String>,
    pub content_length: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct UploadResult {
    pub folder: String,
    pub name: String,
    pub size: u64,
    pub md5: String,
}

/// RAII guard for the temporary file of an upload in progress.
///
/// If the client disconnects, axum simply drops the request future — none of
/// the code after the current `.await` runs. Dropping the future drops this
/// guard, and `Drop` removes the half-written file. It is the Rust version of a
/// C++ destructor / Java try-with-resources, but it also covers cancellation.
struct PartialFile {
    path: PathBuf,
    committed: bool,
}

impl PartialFile {
    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PartialFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ignored: std::io::Result<()> = std::fs::remove_file(&self.path);
        }
    }
}

impl MediaLibrary {
    /// Streams the body straight to disk while hashing it, so memory use stays at
    /// one buffer no matter how large the file is.
    pub async fn receive_upload<S, E>(&self, request: UploadRequest, mut body: S) -> Result<UploadResult, StorageError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let expected_md5: Option<String> = request.expected_md5.as_deref().map(normalize_md5).transpose()?;
        let root: PathBuf = self.writable_root()?;
        let folder_path: PathBuf = self.ensure_folder_path(&request.folder).await?;
        let target: PathBuf = folder_path.join(request.name.as_str());
        let display_name: String = format!("{}/{}", request.folder, request.name);
        if !request.overwrite && tokio::fs::try_exists(&target).await? {
            return Err(StorageError::AlreadyExists(display_name));
        }
        if let Some(length) = request.content_length {
            self.check_space_for(&root, length)?;
        }

        let partial: PartialFile = PartialFile { path: partial_path(&folder_path, &request.name), committed: false };
        let file: tokio::fs::File = tokio::fs::File::create(&partial.path).await?;
        let mut writer: BufWriter<tokio::fs::File> = BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
        let mut hasher: Md5 = Md5::new();
        let mut size: u64 = 0;

        while let Some(chunk) = body.next().await {
            let chunk: Bytes = chunk.map_err(|error: E| StorageError::Interrupted(error.to_string()))?;
            size += chunk.len() as u64;
            if let Some(limit) = self.max_upload_bytes {
                if size > limit {
                    return Err(StorageError::TooLarge { limit });
                }
            }
            hasher.update(&chunk);
            writer.write_all(&chunk).await?;
        }
        if let Some(length) = request.content_length {
            if size != length {
                return Err(StorageError::Interrupted(format!("received {size} of {length} bytes")));
            }
        }
        writer.flush().await?;
        writer.into_inner().sync_all().await?;

        let md5: String = format!("{:x}", hasher.finalize());
        if let Some(expected) = expected_md5 {
            if expected != md5 {
                return Err(StorageError::ChecksumMismatch { expected, actual: md5 });
            }
        }
        // Checked again because another upload may have finished meanwhile.
        if !request.overwrite && tokio::fs::try_exists(&target).await? {
            return Err(StorageError::AlreadyExists(display_name));
        }
        tokio::fs::rename(&partial.path, &target).await?;
        partial.commit();

        let cached_target: PathBuf = target.clone();
        let cached_md5: String = md5.clone();
        let _cache_result: Result<(), tokio::task::JoinError> =
            tokio::task::spawn_blocking(move || md5_cache::remember(&cached_target, &cached_md5)).await;

        Ok(UploadResult { folder: request.folder.to_string(), name: request.name.to_string(), size, md5 })
    }
}

const PARTIAL_MARKER: &str = ".part-";

fn partial_path(folder_path: &Path, name: &EntryName) -> PathBuf {
    let sequence: u64 = UPLOAD_COUNTER.fetch_add(1, Ordering::Relaxed);
    folder_path.join(format!(".{}{}{}-{}", name, PARTIAL_MARKER, std::process::id(), sequence))
}

/// Recognises the names `partial_path` produces.
pub fn is_partial_upload_name(name: &str) -> bool {
    name.starts_with('.') && name.contains(PARTIAL_MARKER)
}

fn normalize_md5(raw: &str) -> Result<String, StorageError> {
    let normalized: String = raw.trim().to_ascii_lowercase();
    let is_hex_digest: bool =
        normalized.len() == 32 && normalized.chars().all(|character: char| character.is_ascii_hexdigit());
    if is_hex_digest { Ok(normalized) } else { Err(StorageError::InvalidChecksum(raw.to_string())) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StorageConfig;
    use futures_util::stream;

    fn library_in(directory: &Path, max_upload_bytes: Option<u64>) -> MediaLibrary {
        MediaLibrary::open(&StorageConfig {
            upload_root: Some(directory.to_path_buf()),
            default_folder: "inbox".to_string(),
            min_free_bytes: 0,
            max_upload_bytes,
            allow_volatile: true,
        })
        .expect("open library")
    }

    fn request(name: &str, overwrite: bool, expected_md5: Option<&str>) -> UploadRequest {
        UploadRequest {
            folder: EntryName::parse("movies").expect("folder"),
            name: EntryName::parse(name).expect("name"),
            overwrite,
            expected_md5: expected_md5.map(str::to_string),
            content_length: None,
        }
    }

    fn body(parts: &[&'static str]) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Unpin {
        let chunks: Vec<Result<Bytes, std::io::Error>> =
            parts.iter().map(|part: &&'static str| Ok(Bytes::from_static(part.as_bytes()))).collect();
        stream::iter(chunks)
    }

    fn leftover_names(directory: &Path) -> Vec<String> {
        std::fs::read_dir(directory.join("movies"))
            .expect("list")
            .map(|entry: std::io::Result<std::fs::DirEntry>| {
                entry.expect("entry").file_name().to_string_lossy().to_string()
            })
            .collect()
    }

    #[tokio::test]
    async fn upload_creates_folder_writes_file_and_returns_md5() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path(), None);
        let result: UploadResult =
            library.receive_upload(request("a.txt", false, None), body(&["hel", "lo"])).await.expect("upload");
        assert_eq!(result.md5, "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(result.size, 5);
        assert_eq!(std::fs::read(directory.path().join("movies/a.txt")).expect("read"), b"hello");
        assert_eq!(leftover_names(directory.path()), vec!["a.txt".to_string()]);
    }

    #[tokio::test]
    async fn existing_file_is_kept_unless_overwrite_is_requested() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path(), None);
        library.receive_upload(request("a.txt", false, None), body(&["one"])).await.expect("first");
        let second: Result<UploadResult, StorageError> =
            library.receive_upload(request("a.txt", false, None), body(&["two"])).await;
        assert!(matches!(second, Err(StorageError::AlreadyExists(_))));
        library.receive_upload(request("a.txt", true, None), body(&["two"])).await.expect("overwrite");
        assert_eq!(std::fs::read(directory.path().join("movies/a.txt")).expect("read"), b"two");
    }

    #[tokio::test]
    async fn checksum_mismatch_leaves_no_file_behind() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path(), None);
        let result: Result<UploadResult, StorageError> = library
            .receive_upload(request("a.txt", false, Some("00000000000000000000000000000000")), body(&["hello"]))
            .await;
        assert!(matches!(result, Err(StorageError::ChecksumMismatch { .. })));
        assert!(leftover_names(directory.path()).is_empty());
    }

    #[tokio::test]
    async fn uploads_left_over_by_a_power_cut_are_removed_on_start() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(directory.path().join("movies")).expect("folder");
        std::fs::write(directory.path().join("movies/.a.mkv.part-123-0"), b"half").expect("seed");
        std::fs::write(directory.path().join("movies/.hidden"), b"keep").expect("seed");
        library_in(directory.path(), None);
        assert_eq!(leftover_names(directory.path()), vec![".hidden".to_string()]);
    }

    #[tokio::test]
    async fn without_an_upload_root_uploads_are_refused() {
        let library: MediaLibrary = MediaLibrary::open(&StorageConfig::default()).expect("open library");
        let result: Result<UploadResult, StorageError> =
            library.receive_upload(request("a.txt", false, None), body(&["x"])).await;
        assert!(matches!(result, Err(StorageError::NotConfigured)));
    }

    #[tokio::test]
    async fn broken_stream_and_size_limit_clean_up_the_partial_file() {
        let directory: tempfile::TempDir = tempfile::tempdir().expect("temp dir");
        let library: MediaLibrary = library_in(directory.path(), Some(4));
        let too_large: Result<UploadResult, StorageError> =
            library.receive_upload(request("big.bin", false, None), body(&["12345"])).await;
        assert!(matches!(too_large, Err(StorageError::TooLarge { limit: 4 })));

        let broken: Vec<Result<Bytes, std::io::Error>> =
            vec![Ok(Bytes::from_static(b"12")), Err(std::io::Error::other("client went away"))];
        let interrupted: Result<UploadResult, StorageError> =
            library.receive_upload(request("cut.bin", false, None), stream::iter(broken)).await;
        assert!(matches!(interrupted, Err(StorageError::Interrupted(_))));
        assert!(leftover_names(directory.path()).is_empty());
    }
}
