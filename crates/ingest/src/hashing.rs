//! Shared streaming xxh64 file hasher. Lifted out of the copy engine's
//! read-back verification so ASC MHL's directory hashing can use the exact
//! same one-file-at-a-time streaming pass instead of duplicating it.
use std::io::Read;
use std::path::Path;

/// Streams `path` with a 1 MiB buffer and returns its xxh64 digest as a
/// lowercase 16-hex-digit string (matching the ASC MHL and journal
/// encodings, which both use `{:016x}`).
///
/// # Errors
/// Returns any I/O error from opening or reading the file.
pub fn xxh64_file(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = xxhash_rust::xxh64::Xxh64::new(0);
    let mut buf = vec![0u8; 1024 * 1024].into_boxed_slice();
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:016x}", hasher.digest()))
}
