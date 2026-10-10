use crate::model::ApiError;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, ApiError> {
    // Refuse symlink replacements and avoid blocking on a FIFO before metadata.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| {
            ApiError::new(
                "FILE_READ",
                format!("Could not open {}: {e}", path.display()),
            )
        })?;
    if !file
        .metadata()
        .map_err(|e| ApiError::new("FILE_READ", e.to_string()))?
        .is_file()
    {
        return Err(ApiError::new("FILE_READ", "Select a regular local file."));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ApiError::new("FILE_READ", e.to_string()))?;
    if bytes.len() > limit {
        return Err(ApiError::new(
            "FILE_LIMIT",
            format!("{} exceeds the {limit} byte limit.", path.display()),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_reads_reject_symlinks_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        std::fs::write(&path, "too much").unwrap();
        assert!(read_limited(&path, 3).is_err());
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_limited(&link, 100).is_err());
    }
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipe.md");
        let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_limited(&path, 100).is_err());
    }
}
