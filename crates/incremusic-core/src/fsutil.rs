//! Small filesystem helpers: atomic writes, hashing, collision-free names.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{IoContext, Result};

/// Writes through a temp file in the same directory, then renames (§5.2.1).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).at(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).at(&tmp)?;
        f.write_all(bytes).at(&tmp)?;
        f.sync_all().at(&tmp)?;
    }
    std::fs::rename(&tmp, path).at(path)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).at(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).at(path)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// `dir/stem`, or `dir/stem-2`, `dir/stem-3`, … if taken (§5.1.1). `taken` is checked in
/// addition to the filesystem.
pub fn unique_stem(dir: &Path, stem: &str, taken: impl Fn(&str) -> bool) -> String {
    let free = |s: &str| !dir.join(s).exists() && !taken(s);
    if free(stem) {
        return stem.to_string();
    }
    (2u32..)
        .map(|i| format!("{stem}-{i}"))
        .find(|s| free(s))
        .expect("unbounded")
}

/// Copies a file, creating the parent directory.
pub fn copy_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p).at(p)?;
    }
    std::fs::copy(from, to).at(to)?;
    Ok(())
}

/// Moves a directory. `rename` within one filesystem; falls back to copy + delete across
/// filesystems (only possible for exports and imports, never within the library).
pub fn move_dir(from: &Path, to: &Path) -> Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p).at(p)?;
    }
    if to.exists() {
        return Err(crate::Error::Library(format!(
            "{} already exists",
            to.display()
        )));
    }
    std::fs::rename(from, to).at(from)
}

pub fn extension_lower(p: &Path) -> Option<String> {
    p.extension().map(|e| e.to_string_lossy().to_lowercase())
}

pub fn file_stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn temp_path(dir: &Path, prefix: &str, ext: &str) -> PathBuf {
    dir.join(format!("{prefix}-{}.{ext}", ulid::Ulid::generate()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_stem_counts_up() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(unique_stem(d.path(), "a-1", |_| false), "a-1");
        std::fs::create_dir(d.path().join("a-1")).unwrap();
        std::fs::create_dir(d.path().join("a-1-2")).unwrap();
        assert_eq!(unique_stem(d.path(), "a-1", |_| false), "a-1-3");
        assert_eq!(unique_stem(d.path(), "a-1", |s| s == "a-1-3"), "a-1-4");
    }

    #[test]
    fn atomic_write_and_hash() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/x.txt");
        write_atomic(&p, b"abc").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"abc");
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
    }
}
