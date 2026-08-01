//! Model artifacts (encoder, transcriber, text embedder): pinned URLs +
//! sha256 digests + cache resolution, fetched with system curl into a
//! shared cache. Every artifact is verified before it is placed; nothing
//! unverified ever sits at a final path.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest as _, Sha256};

use crate::error::IndexError;

pub const MODEL_TAG: &str = "siglip2-b16-v1";

pub struct ModelFile {
    pub name: &'static str,
    pub repo_path: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

/// One fetchable model: `tag` doubles as cache-dir leaf and blob model tag.
pub struct ModelSpec {
    pub tag: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub files: &'static [ModelFile],
}

/// Vision tower fp32 (Core ML handles precision internally), text tower fp16
/// (the Gemma 256k vocab makes fp32 a 1.13 GB download), tokenizer.
///
/// These hashes are the `HuggingFace` API's LFS sha256 oids at the pinned
/// revision, verified 2026-07-30 — do NOT change them.
pub const MODEL_FILES: [ModelFile; 3] = [
    ModelFile {
        name: "vision_model.onnx",
        repo_path: "onnx/vision_model.onnx",
        sha256: "f5cb16728a704703f05516ded628397e11dbca4de2eb5db04b0c0bcee988aa7a",
        bytes: 371_992_072,
    },
    ModelFile {
        name: "text_model_fp16.onnx",
        repo_path: "onnx/text_model_fp16.onnx",
        sha256: "80954edffdc689599e5d5bc6a1738380bc9e8139a18e5c8892485f248b6b4890",
        bytes: 564_862_230,
    },
    ModelFile {
        name: "tokenizer.json",
        repo_path: "tokenizer.json",
        sha256: "cb9140fae3ac5122c972d37adf83e1248471a38147ad76f8215c8872c6fd8322",
        bytes: 34_363_039,
    },
];

pub const SIGLIP: ModelSpec = ModelSpec {
    tag: MODEL_TAG,
    repo: "onnx-community/siglip2-base-patch16-256-ONNX",
    revision: "d1114256522a37ffa257a0a58017348ab0058db2",
    files: &MODEL_FILES,
};

/// Whisper large-v3-turbo, `q5_0` quantization (ggml/whisper.cpp format).
///
/// Pin verified 2026-07-31 via the `HuggingFace` API's `sha` field and the
/// file tree's LFS sha256 oid — do NOT change without re-verifying.
pub const WHISPER: ModelSpec = ModelSpec {
    tag: "whisper-large-v3-turbo-q5-v1",
    repo: "ggerganov/whisper.cpp",
    revision: "5359861c739e955e79d9a303bcbc70fb988958b1",
    files: &[ModelFile {
        name: "ggml-large-v3-turbo-q5_0.bin",
        repo_path: "ggml-large-v3-turbo-q5_0.bin",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
        bytes: 574_041_195,
    }],
};

/// `sentence-transformers/all-MiniLM-L6-v2`, ONNX export.
///
/// Pin verified 2026-07-31 via the `HuggingFace` API's `sha` field and the
/// file tree's sha256 (LFS oid for `model.onnx`; `tokenizer.json` isn't
/// LFS-tracked in this repo, so its hash was computed locally from a
/// downloaded copy at this revision) — do NOT change without re-verifying.
pub const MINILM: ModelSpec = ModelSpec {
    tag: "minilm-l6-v2-v1",
    repo: "sentence-transformers/all-MiniLM-L6-v2",
    revision: "1110a243fdf4706b3f48f1d95db1a4f5529b4d41",
    files: &[
        ModelFile {
            name: "model.onnx",
            repo_path: "onnx/model.onnx",
            sha256: "6fd5d72fe4589f189f8ebc006442dbb529bb7ce38f8082112682524616046452",
            bytes: 90_405_214,
        },
        ModelFile {
            name: "tokenizer.json",
            repo_path: "tokenizer.json",
            sha256: "be50c3628f2bf5bb5e3a7f17b1f74611b2561a3a27eeab05e5aa30f411572037",
            bytes: 466_247,
        },
    ],
};

pub const ALL_MODELS: [&ModelSpec; 3] = [&SIGLIP, &WHISPER, &MINILM];

/// The shared model cache root: `MAJ_MODEL_DIR` when set, else the platform
/// data dir joined with `majestical/models`.
///
/// # Errors
/// Returns [`IndexError::Model`] if no platform data directory can be
/// resolved and `MAJ_MODEL_DIR` isn't set.
fn base_dir() -> Result<PathBuf, IndexError> {
    if let Some(dir) = std::env::var_os("MAJ_MODEL_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let data_dir = dirs::data_dir()
        .ok_or_else(|| IndexError::Model("no platform data directory available".to_string()))?;
    Ok(data_dir.join("majestical").join("models"))
}

/// Composes a cache directory for `spec` under `base` — pure path joining,
/// factored out so tests can exercise it without touching process env.
pub(crate) fn dir_under_base(base: &Path, spec: &ModelSpec) -> PathBuf {
    base.join(spec.tag)
}

/// Where this model's files live on disk: `MAJ_MODEL_DIR` (joined with
/// [`MODEL_TAG`]) when set, else the platform data dir.
///
/// # Errors
/// Returns [`IndexError::Model`] if no platform data directory can be
/// resolved and `MAJ_MODEL_DIR` isn't set.
pub fn model_dir() -> Result<PathBuf, IndexError> {
    model_dir_for(&SIGLIP)
}

/// Cache dir for one model spec (`MAJ_MODEL_DIR` override honored).
///
/// # Errors
/// Returns [`IndexError::Model`] when no data dir can be resolved.
pub fn model_dir_for(spec: &ModelSpec) -> Result<PathBuf, IndexError> {
    Ok(dir_under_base(&base_dir()?, spec))
}

pub(crate) fn file_url(spec: &ModelSpec, file: &ModelFile) -> String {
    format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        spec.repo, spec.revision, file.repo_path
    )
}

/// Cheap presence check: every one of `spec`'s files exists in `dir` at its
/// exact byte size. Hashes are verified at fetch time, not here —
/// re-hashing hundreds of megabytes on every `index status` call would be
/// far too slow for a check that runs on every invocation. The single
/// "installed" definition for every model: capability checks, remedies, and
/// search-time gates must all go through this so they can never disagree.
#[must_use]
pub fn model_present_for(spec: &ModelSpec, dir: &Path) -> bool {
    spec.files.iter().all(|file| {
        std::fs::metadata(dir.join(file.name)).is_ok_and(|meta| meta.len() == file.bytes)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    AlreadyPresent,
    Downloaded,
}

/// One file to fetch into a cache directory. Bundled into a struct (rather
/// than passed as five positional args) to stay under the house 5-param
/// function limit.
pub struct FetchSpec<'a> {
    pub dir: &'a Path,
    pub name: &'a str,
    pub url: &'a str,
    pub sha256: &'a str,
    pub bytes: u64,
    pub verify: bool,
}

/// Streams `path` through SHA-256 in fixed-size chunks without loading it
/// fully into memory.
///
/// # Errors
/// Returns [`IndexError::Model`] if the file can't be opened or read.
pub fn sha256_file(path: &Path) -> Result<String, IndexError> {
    use std::fmt::Write as _;
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)
        .map_err(|source| IndexError::Model(format!("reading {}: {source}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|source| IndexError::Model(format!("hashing {}: {source}", path.display())))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in &digest {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn already_present(spec: &FetchSpec<'_>, dest: &Path) -> Result<bool, IndexError> {
    let Ok(meta) = std::fs::metadata(dest) else {
        return Ok(false);
    };
    if meta.len() != spec.bytes {
        return Ok(false);
    }
    if !spec.verify {
        return Ok(true);
    }
    if sha256_file(dest)? == spec.sha256 {
        return Ok(true);
    }
    // Right size, wrong hash: remove it now. Otherwise, if the redownload
    // below also fails, this corrupt file would stay behind at `dest` and
    // `model_present_for`'s size-only check would wrongly accept it later.
    std::fs::remove_file(dest).map_err(|source| {
        IndexError::Model(format!("removing corrupt {}: {source}", dest.display()))
    })?;
    Ok(false)
}

/// Downloads one file into `spec.dir` via system `curl`, verifying its
/// sha256 before renaming it into place. Skips the download entirely when a
/// file of the right size (and, when `spec.verify` is set, the right hash)
/// already sits at the destination.
///
/// # Errors
/// Returns [`IndexError::Model`] if `curl` can't be spawned, the download
/// fails, or the downloaded bytes don't match `spec.sha256` — in the hash
/// mismatch case, the partial download is removed and nothing lands at the
/// final path.
pub fn fetch_one(spec: &FetchSpec<'_>) -> Result<FetchOutcome, IndexError> {
    let dest = spec.dir.join(spec.name);
    if already_present(spec, &dest)? {
        return Ok(FetchOutcome::AlreadyPresent);
    }

    std::fs::create_dir_all(spec.dir).map_err(|source| {
        IndexError::Model(format!("creating {}: {source}", spec.dir.display()))
    })?;
    let temp_path = spec.dir.join(format!(".fetch-{}", spec.name));

    let status = Command::new("curl")
        .args(["--fail", "--location", "--progress-bar", "--show-error"])
        .arg("--output")
        .arg(&temp_path)
        .arg(spec.url)
        .status()
        .map_err(|source| {
            IndexError::Model(format!("spawning curl to fetch {}: {source}", spec.url))
        })?;
    if !status.success() {
        let _ = std::fs::remove_file(&temp_path);
        return Err(IndexError::Model(format!(
            "downloading {} failed: curl exited with {status}",
            spec.url
        )));
    }

    let actual_sha = match sha256_file(&temp_path) {
        Ok(sha) => sha,
        Err(source) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(source);
        }
    };
    if actual_sha != spec.sha256 {
        let _ = std::fs::remove_file(&temp_path);
        return Err(IndexError::Model(format!(
            "{}: hash mismatch (expected {}, got {actual_sha}) — refusing to install; re-run to retry",
            spec.name, spec.sha256
        )));
    }

    std::fs::rename(&temp_path, &dest)
        .map_err(|source| IndexError::Model(format!("installing {}: {source}", dest.display())))?;
    Ok(FetchOutcome::Downloaded)
}

/// Fetches every file of `spec` into `dir`, reporting each file's name, size
/// in MB, and outcome via `progress`. Libraries never print — all progress
/// rendering happens in the CLI callback.
///
/// # Errors
/// Returns the first [`IndexError::Model`] hit fetching any file.
fn fetch_into(
    spec: &ModelSpec,
    dir: &Path,
    verify: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<(), IndexError> {
    for file in spec.files {
        let url = file_url(spec, file);
        let mb = file.bytes / 1_000_000;
        let outcome = fetch_one(&FetchSpec {
            dir,
            name: file.name,
            url: &url,
            sha256: file.sha256,
            bytes: file.bytes,
            verify,
        })?;
        let status = match outcome {
            FetchOutcome::AlreadyPresent => "already present",
            FetchOutcome::Downloaded => "downloaded",
        };
        progress(&format!("{} ({mb} MB): {status}", file.name));
    }
    Ok(())
}

/// Fetches every file of `spec` into its cache dir ([`model_dir_for`]).
///
/// # Errors
/// Returns the first [`IndexError::Model`] hit resolving the cache dir or
/// fetching any file.
pub fn fetch_spec(
    spec: &ModelSpec,
    verify: bool,
    progress: &mut dyn FnMut(&str),
) -> Result<(), IndexError> {
    let dir = model_dir_for(spec)?;
    fetch_into(spec, &dir, verify, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").expect("write");
        assert_eq!(
            sha256_file(&p).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    fn fetch_one_downloads_verifies_and_skips_when_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("weights.bin");
        std::fs::write(&src, b"model-bytes").expect("write");
        let sha = sha256_file(&src).expect("hash");
        let url = format!("file://{}", src.display());
        let dest_dir = dir.path().join("cache");
        let outcome = fetch_one(&FetchSpec {
            dir: &dest_dir,
            name: "weights.bin",
            url: &url,
            sha256: &sha,
            bytes: 11,
            verify: false,
        })
        .expect("fetch");
        assert_eq!(outcome, FetchOutcome::Downloaded);
        assert_eq!(
            std::fs::read(dest_dir.join("weights.bin")).expect("read"),
            b"model-bytes"
        );
        let again = fetch_one(&FetchSpec {
            dir: &dest_dir,
            name: "weights.bin",
            url: &url,
            sha256: &sha,
            bytes: 11,
            verify: false,
        })
        .expect("fetch");
        assert_eq!(again, FetchOutcome::AlreadyPresent);
    }

    #[test]
    fn fetch_one_rejects_a_bad_hash_and_places_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("weights.bin");
        std::fs::write(&src, b"tampered").expect("write");
        let url = format!("file://{}", src.display());
        let err = fetch_one(&FetchSpec {
            dir: &dir.path().join("cache"),
            name: "weights.bin",
            url: &url,
            sha256: &"0".repeat(64),
            bytes: 8,
            verify: false,
        })
        .expect_err("must fail");
        assert!(err.to_string().contains("hash mismatch"));
        assert!(
            !dir.path().join("cache/weights.bin").exists(),
            "no unverified file placed"
        );
        // And no temp leftovers either:
        let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("cache"))
            .map(|rd| rd.flatten().collect())
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "no partial downloads left behind: {leftovers:?}"
        );
    }

    #[test]
    fn fetch_one_re_downloads_a_wrong_sized_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("weights.bin");
        std::fs::write(&src, b"correct-bytes").expect("write");
        let sha = sha256_file(&src).expect("hash");
        let url = format!("file://{}", src.display());
        let dest_dir = dir.path().join("cache");
        std::fs::create_dir_all(&dest_dir).expect("mkdir");
        std::fs::write(dest_dir.join("weights.bin"), b"wrong-size").expect("seed wrong size");

        let outcome = fetch_one(&FetchSpec {
            dir: &dest_dir,
            name: "weights.bin",
            url: &url,
            sha256: &sha,
            bytes: 13,
            verify: false,
        })
        .expect("fetch");
        assert_eq!(
            outcome,
            FetchOutcome::Downloaded,
            "wrong size must re-download"
        );
        assert_eq!(
            std::fs::read(dest_dir.join("weights.bin")).expect("read"),
            b"correct-bytes"
        );
    }

    #[test]
    fn a_right_sized_but_wrong_hash_dest_is_removed_even_if_the_redownload_also_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_dir = dir.path().join("cache");
        std::fs::create_dir_all(&dest_dir).expect("mkdir");
        let dest = dest_dir.join("weights.bin");
        std::fs::write(&dest, b"corrupt-byte").expect("seed corrupt file");

        // The redownload source is also wrong (doesn't match `sha256`
        // below), so this must fail — proving the original corrupt file at
        // `dest` was removed proactively by `already_present`, not merely
        // overwritten by a successful download.
        let src = dir.path().join("bad-source.bin");
        std::fs::write(&src, b"also-corrupt").expect("write bad source");
        let url = format!("file://{}", src.display());

        let err = fetch_one(&FetchSpec {
            dir: &dest_dir,
            name: "weights.bin",
            url: &url,
            sha256: &"f".repeat(64),
            bytes: 12,
            verify: true,
        })
        .expect_err("redownload must fail its own hash check");

        assert!(err.to_string().contains("hash mismatch"));
        assert!(
            !dest.exists(),
            "corrupt original must be gone even though the redownload also failed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_temp_file_is_removed_before_the_error_is_returned() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("weights.bin");
        std::fs::write(&src, b"some-bytes").expect("write source");
        let url = format!("file://{}", src.display());
        let dest_dir = dir.path().join("cache");
        std::fs::create_dir_all(&dest_dir).expect("mkdir");

        // Pre-seed the temp path write-only: curl can still open and
        // truncate it (write permission), but the post-download
        // `sha256_file` read fails (no read permission).
        let temp_path = dest_dir.join(".fetch-weights.bin");
        std::fs::write(&temp_path, b"").expect("seed temp");
        std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o200))
            .expect("chmod write-only");

        let _ = fetch_one(&FetchSpec {
            dir: &dest_dir,
            name: "weights.bin",
            url: &url,
            sha256: &"0".repeat(64),
            bytes: 10,
            verify: false,
        })
        .expect_err("unreadable temp file must surface as an error");

        assert!(
            !temp_path.exists(),
            "unreadable temp file must be removed, not left behind"
        );
    }

    #[test]
    fn registry_contains_three_models_with_distinct_tags() {
        let tags: Vec<&str> = ALL_MODELS.iter().map(|m| m.tag).collect();
        assert_eq!(
            tags,
            vec![
                "siglip2-b16-v1",
                "whisper-large-v3-turbo-q5-v1",
                "minilm-l6-v2-v1"
            ]
        );
    }

    #[test]
    fn model_dir_for_appends_tag_under_maj_model_dir() {
        // MAJ_MODEL_DIR handling is exercised through the public fn; this test
        // must not mutate the process env (tests run in parallel) — call the
        // path-composition helper directly.
        let base = std::path::Path::new("/models");
        assert_eq!(
            dir_under_base(base, &WHISPER),
            std::path::PathBuf::from("/models/whisper-large-v3-turbo-q5-v1")
        );
    }

    #[test]
    fn spec_urls_pin_repo_and_revision() {
        let url = file_url(&WHISPER, &WHISPER.files[0]);
        assert!(url.starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/"));
        assert!(url.contains(WHISPER.revision));
        assert!(url.ends_with("ggml-large-v3-turbo-q5_0.bin"));
    }

    /// A tiny two-file spec so the presence rule is pinned independently of
    /// any real model's registry entry.
    const TWO_FILE_SPEC: ModelSpec = ModelSpec {
        tag: "test-two-files",
        repo: "example/repo",
        revision: "deadbeef",
        files: &[
            ModelFile {
                name: "a.bin",
                repo_path: "a.bin",
                sha256: "00",
                bytes: 8,
            },
            ModelFile {
                name: "b.bin",
                repo_path: "b.bin",
                sha256: "11",
                bytes: 16,
            },
        ],
    };

    fn write_at_len(dir: &Path, name: &str, len: u64) {
        let f = std::fs::File::create(dir.join(name)).expect("create");
        f.set_len(len).expect("set_len");
    }

    #[test]
    fn model_present_for_requires_every_spec_file_at_its_exact_size() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_at_len(dir.path(), "a.bin", 8);
        assert!(
            !model_present_for(&TWO_FILE_SPEC, dir.path()),
            "a missing file must fail presence"
        );

        write_at_len(dir.path(), "b.bin", 17);
        assert!(
            !model_present_for(&TWO_FILE_SPEC, dir.path()),
            "one wrong-sized file must fail presence"
        );

        write_at_len(dir.path(), "b.bin", 16);
        assert!(
            model_present_for(&TWO_FILE_SPEC, dir.path()),
            "every file at its exact size passes"
        );
    }

    #[test]
    fn model_present_for_checks_the_given_specs_own_file_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        for file in &MODEL_FILES {
            write_at_len(dir.path(), file.name, file.bytes);
        }
        assert!(
            model_present_for(&SIGLIP, dir.path()),
            "the siglip file list at exact sizes passes"
        );
        assert!(
            !model_present_for(&MINILM, dir.path()),
            "a different spec's files are not satisfied by siglip's"
        );
    }
}
