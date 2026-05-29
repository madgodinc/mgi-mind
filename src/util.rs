//! Shared utilities: atomic file writes, integrity verification, native HTTP downloads.
//! Addresses audit #4 (atomic writes), #6 (download integrity), #19 (no curl/tar shellout).

use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Atomically write bytes to `path`: write a temp file in the same directory,
/// fsync it, then rename over the target. A crash/power-loss mid-write leaves
/// either the old file or the new one intact — never a truncated/corrupt file.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).ok();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("Failed to create temp file in {}", dir.display()))?;
    tmp.write_all(data).context("Failed to write temp file")?;
    tmp.as_file()
        .sync_all()
        .context("Failed to fsync temp file")?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("Failed to persist {}: {}", path.display(), e.error))?;
    Ok(())
}

/// Convenience wrapper for string payloads.
pub fn atomic_write_str(path: &Path, data: &str) -> Result<()> {
    atomic_write(path, data.as_bytes())
}

/// Compute the SHA-256 of a file as a lowercase hex string.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut file =
        fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).context("Failed to hash file")?;
    Ok(hex::encode(hasher.finalize()))
}

/// Verify a file matches the expected SHA-256 (hex). Fail-closed on mismatch.
pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if !actual.eq_ignore_ascii_case(expected_hex) {
        anyhow::bail!(
            "Integrity check FAILED for {}\n  expected: {}\n  actual:   {}",
            path.display(),
            expected_hex,
            actual
        );
    }
    Ok(())
}

/// Download `url` to `dest` over HTTPS using reqwest (no `curl` shellout).
/// Streams to a temp file, optionally verifies SHA-256, then atomically renames
/// into place. On checksum mismatch the temp file is dropped and nothing is installed.
pub async fn download_file(url: &str, dest: &Path, expected_sha256: Option<&str>) -> Result<()> {
    use futures_util::StreamExt;

    let client = reqwest::Client::builder()
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Request failed: {url}"))?
        .error_for_status()
        .with_context(|| format!("Bad HTTP status from {url}"))?;

    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).ok();

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("Failed to create temp file in {}", dir.display()))?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("Download stream error")?;
        tmp.write_all(&chunk).context("Failed writing download")?;
    }
    tmp.as_file().sync_all().ok();
    let tmp_path = tmp.into_temp_path();

    if let Some(expected) = expected_sha256 {
        verify_sha256(&tmp_path, expected).context("Refusing to install file with bad checksum")?;
    }

    tmp_path.persist(dest).map_err(|e| {
        anyhow::anyhow!("Failed to move download to {}: {}", dest.display(), e.error)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested/file.txt");
        atomic_write_str(&p, "hello").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "hello");
        // overwrite
        atomic_write_str(&p, "world").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "world");
    }

    #[test]
    fn sha256_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        atomic_write_str(&p, "abc").unwrap();
        // sha256("abc")
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(
            verify_sha256(
                &p,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            )
            .is_ok()
        );
        assert!(verify_sha256(&p, "deadbeef").is_err());
    }
}
