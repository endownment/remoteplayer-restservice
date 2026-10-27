//! MD5 of files that may have arrived without this service (scp, USB copy...).
//! The digest is cached in an extended attribute of the file itself, keyed by
//! size and modification time, so no database is needed. Filesystems without
//! xattr support (FAT, exFAT) simply never hit the cache.

use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use md5::{Digest, Md5};

const ATTRIBUTE_NAME: &str = "user.remoteplayer.md5";
const READ_BUFFER_BYTES: usize = 1024 * 1024;

/// Returns `(md5, size, served_from_cache)`.
pub fn md5_with_cache(path: &Path) -> std::io::Result<(String, u64, bool)> {
    let metadata: std::fs::Metadata = std::fs::metadata(path)?;
    let fingerprint: String = fingerprint(&metadata);
    if let Some(cached) = read_cached(path, &fingerprint) {
        return Ok((cached, metadata.len(), true));
    }
    let md5: String = compute_md5(path)?;
    store(path, &md5, &fingerprint);
    Ok((md5, metadata.len(), false))
}

pub fn remember(path: &Path, md5: &str) {
    if let Ok(metadata) = std::fs::metadata(path) {
        store(path, md5, &fingerprint(&metadata));
    }
}

fn fingerprint(metadata: &std::fs::Metadata) -> String {
    format!("{}:{}.{}", metadata.len(), metadata.mtime(), metadata.mtime_nsec())
}

fn read_cached(path: &Path, fingerprint: &str) -> Option<String> {
    let raw: Vec<u8> = xattr::get(path, ATTRIBUTE_NAME).ok()??;
    let text: String = String::from_utf8(raw).ok()?;
    let (md5, stored_fingerprint): (&str, &str) = text.split_once(' ')?;
    (stored_fingerprint == fingerprint).then(|| md5.to_string())
}

fn store(path: &Path, md5: &str, fingerprint: &str) {
    let value: String = format!("{md5} {fingerprint}");
    if let Err(error) = xattr::set(path, ATTRIBUTE_NAME, value.as_bytes()) {
        tracing::debug!(path = %path.display(), %error, "cannot cache md5 in an extended attribute");
    }
}

fn compute_md5(path: &Path) -> std::io::Result<String> {
    let mut file: std::fs::File = std::fs::File::open(path)?;
    let mut hasher: Md5 = Md5::new();
    let mut buffer: Vec<u8> = vec![0u8; READ_BUFFER_BYTES];
    loop {
        let count: usize = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        // Borrowing: `&buffer[..count]` is a slice — a pointer + length view into
        // the vector, no copy (like std::span in C++20).
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
pub fn xattrs_work_in(directory: &Path) -> bool {
    let probe: std::path::PathBuf = directory.join(".xattr-probe");
    let works: bool = std::fs::write(&probe, b"").is_ok() && xattr::set(&probe, ATTRIBUTE_NAME, b"probe").is_ok();
    let _ignored: std::io::Result<()> = std::fs::remove_file(&probe);
    works
}
