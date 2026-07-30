//! ASC MHL create + verify (standard tier, flat histories, single hash
//! format per generation). Conformance oracle: the Python reference
//! implementation (`ascmhl` on `PyPI`, tested against v1.2) — `just
//! conformance` round-trips both directions against it in CI.
//!
//! Oracle findings pinned here (established empirically against `ascmhl
//! create`/`ascmhl-debug verify` — the main `ascmhl` CLI has no `verify`
//! subcommand; hash-vs-history verification without writing a new
//! generation lives on the separate `ascmhl-debug` console script):
//!
//! - Root element: `<hashlist version="2.0" xmlns="urn:ASC:MHL:v2.0">`.
//! - `creatorinfo` needs `creationdate`, `hostname`, `tool version="...">`;
//!   `processinfo` needs a `process` element (we always write `in-place`,
//!   the value for hashing files where they already live).
//! - `roothash`, `directoryhash`, and `ignore` are OPTIONAL — a manifest
//!   built from an empty `creatorinfo`/`processinfo`/`hashes` shape with
//!   only `<hash>` entries (no directory hashes at all) passes
//!   `ascmhl-debug verify` cleanly. This crate never computes or writes
//!   them: the task's model has no per-directory hash concept, and the
//!   oracle proves that's a legitimate (if minimal) standard-tier history,
//!   not a phantom feature standing in for one the spec requires.
//! - Hash entry shape: `<hash><path size="N">rel/path</path><xxh64
//!   action="original|verified|failed" hashdate="ISO8601">HEX</xxh64></hash>`.
//!   `path`'s `lastmodificationdate` attribute is accepted if present but
//!   never checked by verify (confirmed: touching a file's mtime without
//!   touching its bytes still verifies clean) — we omit it.
//! - Relative paths are POSIX (`/`)-separated, un-encoded (a literal space
//!   in a filename appears as a literal space in the element text).
//! - Generation file naming: the oracle's own loader only requires a
//!   `\d{4,}` prefix before an optional `_suffix` and a `.mhl` extension
//!   (`MHLHistory.history_file_name_regex`); we still emit the full
//!   oracle-style `NNNN_<root-name>_<date>_<time>Z.mhl` for interop with
//!   real ASC MHL tooling, not just our own reader.
//! - THE CHAIN FILE IS THE HEADLINE DIVERGENCE FROM THIS TASK'S ORIGINAL
//!   SKETCH. `ascmhl/ascmhl_chain.xml` is required once any generation
//!   exists, and on load the oracle recomputes a hash of each referenced
//!   generation file's raw bytes and compares it to the chain's stored
//!   value, hard-failing ("Modified ASC MHL manifest") on any mismatch —
//!   including ours, when the reference tool reads a history we wrote.
//!   That hash is **always c4** (SHA-512 digest, encoded as Bitcoin-alphabet
//!   base58, left-padded with the alphabet's zero-character `'1'` to 88
//!   characters, prefixed `c4`) regardless of which hash format the
//!   generation's file entries use — confirmed by reading
//!   `ascmhl.hasher.C4`, `ascmhl.hashlist.generate_reference_hash`, and
//!   `ascmhl.history.MHLHistory.load_from_path`. So `WrittenGeneration.roothash`
//!   here is the c4 hash of the manifest file's own bytes, not an xxh64 of
//!   them — the oracle wins over the task sketch's `xxh64` suggestion.
//! - A file present in the previous generation but missing from disk is
//!   dropped from the new generation's `<hashes>` entirely (confirmed:
//!   `ascmhl create` over a deleted file emits no `<hash>` for it at all,
//!   only a stderr warning and a non-zero exit) — it is not carried
//!   forward as a "failed" entry. We match that for the on-disk manifest;
//!   `VerifyReport.missing` still reports it to the caller in memory.
use crate::IngestError;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::name::QName;
use quick_xml::reader::Reader as QReader;
use quick_xml::writer::Writer as QWriter;
use std::path::{Path, PathBuf};

/// What produced a recorded hash: a first sighting, a re-verification that
/// matched, or a re-verification that didn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAction {
    Original,
    Verified,
    Failed,
}

impl HashAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HashAction::Original => "original",
            HashAction::Verified => "verified",
            HashAction::Failed => "failed",
        }
    }

    fn parse(value: &str, path: &Path) -> Result<Self, IngestError> {
        match value {
            "original" => Ok(HashAction::Original),
            "verified" => Ok(HashAction::Verified),
            "failed" => Ok(HashAction::Failed),
            other => Err(IngestError::Mhl {
                path: path.to_path_buf(),
                msg: format!("unknown hash action {other:?} in <xxh64 action=...>"),
            }),
        }
    }
}

/// One recorded file hash, keyed by path relative to the history root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MhlEntry {
    pub rel: String,
    pub size: u64,
    pub xxh64: String,
    pub action: HashAction,
    pub hashdate: String,
}

/// The parsed or to-be-written contents of one `.mhl` generation file
/// (minus its own file name and generation number, which the history — not
/// the hash list — owns).
#[derive(Debug, Clone)]
pub struct HashList {
    pub creation_date: String,
    pub hostname: String,
    pub tool_version: String,
    pub entries: Vec<MhlEntry>,
}

/// What `write_generation` produced.
#[derive(Debug, Clone)]
pub struct WrittenGeneration {
    pub path: PathBuf,
    pub generation: u32,
    /// c4 hash (SHA-512 -> base58) of the manifest file's own bytes, as
    /// recorded in `ascmhl_chain.xml` for tamper-evidence. See the module
    /// doc comment: the oracle always uses c4 here, never the file entries'
    /// hash format.
    pub roothash: String,
}

/// The result of comparing a directory's current state against its latest
/// ASC MHL generation, plus the new generation that recorded it.
#[derive(Debug)]
pub struct VerifyReport {
    pub verified: Vec<String>,
    pub altered: Vec<String>,
    pub missing: Vec<String>,
    pub new_files: Vec<String>,
    pub written: WrittenGeneration,
}

const ASCMHL_DIR: &str = "ascmhl";
const CHAIN_FILE: &str = "ascmhl_chain.xml";

/// Scans `root/ascmhl/*.mhl` for the leading `NNNN` generation number and
/// returns `max + 1`, or `1` if no history exists yet.
///
/// # Errors
/// Returns [`IngestError::Mhl`] if `root/ascmhl` exists but can't be read.
pub fn next_generation(root: &Path) -> Result<u32, IngestError> {
    Ok(generation_files(root)?
        .into_iter()
        .map(|(n, _)| n)
        .max()
        .map_or(1, |n| n + 1))
}

/// Lists every `(generation_number, path)` pair under `root/ascmhl`,
/// ignoring files that don't match the `NNNN...` naming convention.
fn generation_files(root: &Path) -> Result<Vec<(u32, PathBuf)>, IngestError> {
    let ascmhl_dir = root.join(ASCMHL_DIR);
    if !ascmhl_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    let read_dir = std::fs::read_dir(&ascmhl_dir).map_err(|source| IngestError::Mhl {
        path: ascmhl_dir.clone(),
        msg: format!("reading history directory: {source}"),
    })?;
    for entry in read_dir {
        let entry = entry.map_err(|source| IngestError::Mhl {
            path: ascmhl_dir.clone(),
            msg: format!("reading history directory: {source}"),
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name.strip_suffix(".mhl") else {
            continue;
        };
        let digits: String = stem.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() >= 4
            && let Ok(n) = digits.parse::<u32>()
        {
            found.push((n, entry.path()));
        }
    }
    Ok(found)
}

/// Path of the highest-numbered generation file, or `None` if there is no
/// history yet.
fn latest_generation_path(root: &Path) -> Result<Option<PathBuf>, IngestError> {
    Ok(generation_files(root)?
        .into_iter()
        .max_by_key(|(n, _)| *n)
        .map(|(_, path)| path))
}

/// Best-effort local hostname for `creatorinfo/hostname`. Informational
/// only — the oracle never validates it — so a lookup failure falls back
/// to a placeholder rather than failing the whole operation.
#[must_use]
pub fn local_hostname() -> String {
    hostname::get().map_or_else(
        |_| "unknown-host".to_string(),
        |h| h.to_string_lossy().into_owned(),
    )
}

/// Hashes every file under `root` into a fresh [`HashList`], skipping the
/// `ascmhl/` history directory itself, dotfiles, and (by the same
/// starts-with-`.` rule) the copy engine's `.maj-partial-<token>-<name>`
/// quarantine files — a partial file crossing the wire mid-copy must never
/// be recorded as if it were the finished asset.
///
/// # Errors
/// Returns [`IngestError::Walk`] on a directory walk failure,
/// [`IngestError::NonUtf8Path`] for a non-UTF-8 relative path, or
/// [`IngestError::Read`] if a file can't be hashed.
pub fn hash_dir(root: &Path, hashdate: &str) -> Result<HashList, IngestError> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.map_err(|source| IngestError::Walk {
            path: root.to_path_buf(),
            source,
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel_path = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if is_ignored(rel_path) {
            continue;
        }
        let rel = rel_path
            .to_str()
            .ok_or_else(|| IngestError::NonUtf8Path {
                path: entry.path().to_path_buf(),
            })?
            .replace('\\', "/");
        let size = entry
            .metadata()
            .map_err(|source| IngestError::Walk {
                path: entry.path().to_path_buf(),
                source,
            })?
            .len();
        let xxh64 =
            crate::hashing::xxh64_file(entry.path()).map_err(|source| IngestError::Read {
                path: entry.path().to_path_buf(),
                source,
            })?;
        entries.push(MhlEntry {
            rel,
            size,
            xxh64,
            action: HashAction::Original,
            hashdate: hashdate.to_string(),
        });
    }
    Ok(HashList {
        creation_date: hashdate.to_string(),
        hostname: local_hostname(),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        entries,
    })
}

/// True if any path component is the `ascmhl` history directory or starts
/// with `.` (dotfiles, `.DS_Store`, and the engine's `.maj-partial-*`
/// quarantine names all share that prefix).
fn is_ignored(rel_path: &Path) -> bool {
    rel_path.components().any(|component| {
        let s = component.as_os_str().to_string_lossy();
        s == ASCMHL_DIR || s.starts_with('.')
    })
}

/// Reads the latest generation, hashes the directory's current state, and
/// writes a new generation recording the diff: matching hashes are
/// `verified`, differing hashes are `failed` (with the new hash recorded),
/// files missing from disk are dropped from the manifest but reported in
/// [`VerifyReport::missing`], and untracked files are added as `original`.
///
/// # Errors
/// Returns [`IngestError::Mhl`] if no history exists yet under `root`, or
/// any error from the underlying hash/read/write steps.
pub fn verify_dir(root: &Path, hashdate: &str) -> Result<VerifyReport, IngestError> {
    let previous_path = latest_generation_path(root)?.ok_or_else(|| IngestError::Mhl {
        path: root.to_path_buf(),
        msg: "no ASC MHL history found — run an initial hash/create first".to_string(),
    })?;
    let previous = read_generation(&previous_path)?;
    let present = hash_dir(root, hashdate)?;

    let mut previous_by_rel: std::collections::HashMap<&str, &MhlEntry> = previous
        .entries
        .iter()
        .map(|e| (e.rel.as_str(), e))
        .collect();
    let mut verified = Vec::new();
    let mut altered = Vec::new();
    let mut new_files = Vec::new();
    let mut new_entries = Vec::with_capacity(present.entries.len());

    for entry in present.entries {
        match previous_by_rel.remove(entry.rel.as_str()) {
            Some(prev) if prev.xxh64 == entry.xxh64 => {
                verified.push(entry.rel.clone());
                new_entries.push(MhlEntry {
                    action: HashAction::Verified,
                    ..entry
                });
            }
            Some(_) => {
                altered.push(entry.rel.clone());
                new_entries.push(MhlEntry {
                    action: HashAction::Failed,
                    ..entry
                });
            }
            None => {
                new_files.push(entry.rel.clone());
                new_entries.push(entry);
            }
        }
    }
    let mut missing: Vec<String> = previous_by_rel.keys().map(|s| (*s).to_string()).collect();
    missing.sort();

    let new_hash_list = HashList {
        creation_date: hashdate.to_string(),
        hostname: present.hostname,
        tool_version: present.tool_version,
        entries: new_entries,
    };
    let written = write_generation(root, &new_hash_list)?;

    Ok(VerifyReport {
        verified,
        altered,
        missing,
        new_files,
        written,
    })
}

/// Writes `hash_list` as the next generation under `root/ascmhl`, updates
/// `ascmhl_chain.xml` with the new generation's c4 hash, and returns the
/// written path, generation number, and c4 roothash.
///
/// # Errors
/// Returns [`IngestError::Mhl`] on any I/O or XML-serialization failure.
pub fn write_generation(
    root: &Path,
    hash_list: &HashList,
) -> Result<WrittenGeneration, IngestError> {
    let ascmhl_dir = root.join(ASCMHL_DIR);
    std::fs::create_dir_all(&ascmhl_dir).map_err(|source| IngestError::Mhl {
        path: ascmhl_dir.clone(),
        msg: format!("creating history directory: {source}"),
    })?;

    let generation = next_generation(root)?;
    let root_name = root.file_name().and_then(|s| s.to_str()).unwrap_or("root");
    let timestamp = filename_timestamp(&hash_list.creation_date);
    let filename = format!("{generation:04}_{root_name}_{timestamp}.mhl");
    let manifest_path = ascmhl_dir.join(&filename);

    let xml_bytes = build_manifest_xml(hash_list, &manifest_path)?;
    std::fs::write(&manifest_path, &xml_bytes).map_err(|source| IngestError::Mhl {
        path: manifest_path.clone(),
        msg: format!("writing manifest: {source}"),
    })?;

    let roothash = c4_hash(&xml_bytes);
    let chain_path = ascmhl_dir.join(CHAIN_FILE);
    let mut chain_entries = read_chain(&chain_path)?;
    chain_entries.push(ChainEntry {
        sequencenr: generation,
        path: filename,
        c4: roothash.clone(),
    });
    write_chain(&chain_path, &chain_entries)?;

    Ok(WrittenGeneration {
        path: manifest_path,
        generation,
        roothash,
    })
}

/// Reformats a `creation_date` (expected to start with an ISO-8601
/// `YYYY-MM-DDTHH:MM:SS` prefix, `Z` or offset-suffixed) into the oracle's
/// filename timestamp shape: `YYYY-MM-DD_HHMMSSZ`.
fn filename_timestamp(creation_date: &str) -> String {
    let prefix: String = creation_date.chars().take(19).collect();
    let mut out = String::with_capacity(20);
    for ch in prefix.chars() {
        match ch {
            'T' => out.push('_'),
            ':' => {}
            other => out.push(other),
        }
    }
    out.push('Z');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_timestamp_from_iso8601_z() {
        assert_eq!(
            filename_timestamp("2026-07-30T00:41:10Z"),
            "2026-07-30_004110Z"
        );
    }

    #[test]
    fn filename_timestamp_from_offset_suffixed() {
        assert_eq!(
            filename_timestamp("2026-07-30T00:41:10.589766-04:00"),
            "2026-07-30_004110Z"
        );
    }
}

// ---------------------------------------------------------------------
// XML manifest writer
// ---------------------------------------------------------------------

fn xml_err(path: &Path, source: quick_xml::Error) -> IngestError {
    IngestError::MhlXml {
        path: path.to_path_buf(),
        source,
    }
}

/// `Writer::write_event` reports failures as `std::io::Error` (it's just
/// writing to the underlying `Write`, here an in-memory `Vec<u8>` that
/// can't actually fail) — wrapped as [`IngestError::Mhl`] rather than
/// [`IngestError::MhlXml`], which is reserved for parse-side XML errors.
fn write_err(path: &Path, source: &std::io::Error) -> IngestError {
    IngestError::Mhl {
        path: path.to_path_buf(),
        msg: format!("writing XML: {source}"),
    }
}

fn write_start(
    writer: &mut QWriter<Vec<u8>>,
    tag: &str,
    attrs: &[(&str, &str)],
    path: &Path,
) -> Result<(), IngestError> {
    let mut start = BytesStart::new(tag);
    for (key, value) in attrs {
        start.push_attribute((*key, *value));
    }
    writer
        .write_event(Event::Start(start))
        .map_err(|source| write_err(path, &source))
}

fn write_end(writer: &mut QWriter<Vec<u8>>, tag: &str, path: &Path) -> Result<(), IngestError> {
    writer
        .write_event(Event::End(BytesEnd::new(tag)))
        .map_err(|source| write_err(path, &source))
}

fn write_text(writer: &mut QWriter<Vec<u8>>, text: &str, path: &Path) -> Result<(), IngestError> {
    writer
        .write_event(Event::Text(BytesText::new(text)))
        .map_err(|source| write_err(path, &source))
}

fn write_creatorinfo(
    writer: &mut QWriter<Vec<u8>>,
    hash_list: &HashList,
    path: &Path,
) -> Result<(), IngestError> {
    write_start(writer, "creatorinfo", &[], path)?;
    write_start(writer, "creationdate", &[], path)?;
    write_text(writer, &hash_list.creation_date, path)?;
    write_end(writer, "creationdate", path)?;
    write_start(writer, "hostname", &[], path)?;
    write_text(writer, &hash_list.hostname, path)?;
    write_end(writer, "hostname", path)?;
    write_start(
        writer,
        "tool",
        &[("version", hash_list.tool_version.as_str())],
        path,
    )?;
    write_text(writer, "majestical", path)?;
    write_end(writer, "tool", path)?;
    write_end(writer, "creatorinfo", path)
}

fn write_hash_entry(
    writer: &mut QWriter<Vec<u8>>,
    entry: &MhlEntry,
    path: &Path,
) -> Result<(), IngestError> {
    let size = entry.size.to_string();
    write_start(writer, "hash", &[], path)?;
    write_start(writer, "path", &[("size", size.as_str())], path)?;
    write_text(writer, &entry.rel, path)?;
    write_end(writer, "path", path)?;
    write_start(
        writer,
        "xxh64",
        &[
            ("action", entry.action.as_str()),
            ("hashdate", entry.hashdate.as_str()),
        ],
        path,
    )?;
    write_text(writer, &entry.xxh64, path)?;
    write_end(writer, "xxh64", path)?;
    write_end(writer, "hash", path)
}

/// Builds the full `.mhl` XML document for `hash_list`. `path` is only
/// used to attribute XML-writer errors to a file.
fn build_manifest_xml(hash_list: &HashList, path: &Path) -> Result<Vec<u8>, IngestError> {
    let mut writer = QWriter::new_with_indent(Vec::new(), b' ', 2);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(|source| write_err(path, &source))?;

    write_start(
        &mut writer,
        "hashlist",
        &[("version", "2.0"), ("xmlns", "urn:ASC:MHL:v2.0")],
        path,
    )?;
    write_creatorinfo(&mut writer, hash_list, path)?;
    write_start(&mut writer, "processinfo", &[], path)?;
    write_start(&mut writer, "process", &[], path)?;
    write_text(&mut writer, "in-place", path)?;
    write_end(&mut writer, "process", path)?;
    write_end(&mut writer, "processinfo", path)?;

    write_start(&mut writer, "hashes", &[], path)?;
    for entry in &hash_list.entries {
        write_hash_entry(&mut writer, entry, path)?;
    }
    write_end(&mut writer, "hashes", path)?;
    write_end(&mut writer, "hashlist", path)?;

    let mut bytes = writer.into_inner();
    bytes.push(b'\n');
    Ok(bytes)
}

// ---------------------------------------------------------------------
// XML manifest reader
// ---------------------------------------------------------------------

fn local_name(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.local_name().as_ref()).into_owned()
}

/// Decodes and unescapes a text event's content (`decode()` handles the
/// document encoding, `unescape()` resolves entity references like
/// `&amp;`).
fn unescape_text(text: &BytesText<'_>, path: &Path) -> Result<String, IngestError> {
    let decoded = text
        .decode()
        .map_err(|source| xml_err(path, source.into()))?;
    let unescaped =
        quick_xml::escape::unescape(&decoded).map_err(|source| xml_err(path, source.into()))?;
    Ok(unescaped.into_owned())
}

fn attr_value(
    start: &BytesStart<'_>,
    key: &str,
    path: &Path,
) -> Result<Option<String>, IngestError> {
    for attr in start.attributes() {
        let attr = attr.map_err(|source| xml_err(path, quick_xml::Error::InvalidAttr(source)))?;
        if attr.key.as_ref() == key.as_bytes() {
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|source| xml_err(path, source))?
                .into_owned();
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// Scratch state for the one-pass pull parser below: which container
/// element we're inside (only `creatorinfo`/`tool`/`hash` matter; every
/// other element, including `directoryhash`/`roothash`/`ignore`, is
/// tracked just enough to be skipped for forward compatibility) and the
/// in-progress hash entry's fields.
#[derive(Default)]
struct ParseState {
    in_creatorinfo: bool,
    in_hash: bool,
    in_directoryhash: bool,
    cur_rel: Option<String>,
    cur_size: Option<u64>,
    cur_xxh64: Option<String>,
    cur_action: Option<String>,
    cur_hashdate: Option<String>,
}

/// Reads an `.mhl` generation file into a [`HashList`]. Unknown elements
/// (directory hashes, root hashes, ignore patterns, authors, ...) are
/// silently skipped rather than erroring, so this reads oracle-produced
/// histories — which include all of those — as readily as our own.
///
/// # Errors
/// Returns [`IngestError::Read`] if the file can't be read,
/// [`IngestError::MhlXml`] on malformed XML, or [`IngestError::Mhl`] if a
/// required field (a hash entry's path or digest) is missing.
pub fn read_generation(path: &Path) -> Result<HashList, IngestError> {
    let bytes = std::fs::read(path).map_err(|source| IngestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader =
        QReader::from_str(
            std::str::from_utf8(&bytes).map_err(|source| IngestError::Mhl {
                path: path.to_path_buf(),
                msg: format!("manifest is not valid UTF-8: {source}"),
            })?,
        );
    reader.config_mut().trim_text(true);

    let mut creation_date = None;
    let mut hostname = None;
    let mut tool_version = None;
    let mut entries = Vec::new();
    let mut state = ParseState::default();
    let mut tag_stack: Vec<String> = Vec::new();

    loop {
        match reader
            .read_event()
            .map_err(|source| xml_err(path, source))?
        {
            Event::Eof => break,
            Event::Start(start) => {
                let tag = local_name(start.name());
                handle_start(&tag, &start, path, &mut state, &mut tool_version)?;
                tag_stack.push(tag);
            }
            Event::Empty(start) => {
                let tag = local_name(start.name());
                handle_start(&tag, &start, path, &mut state, &mut tool_version)?;
                handle_end(&tag, path, &mut state, &mut entries)?;
            }
            Event::Text(text) => {
                let text = unescape_text(&text, path)?;
                handle_text(
                    tag_stack.last().map(String::as_str),
                    &text,
                    &mut state,
                    &mut creation_date,
                    &mut hostname,
                );
            }
            Event::End(end) => {
                let tag = local_name(end.name());
                tag_stack.pop();
                handle_end(&tag, path, &mut state, &mut entries)?;
            }
            _ => {}
        }
    }

    Ok(HashList {
        creation_date: creation_date.unwrap_or_default(),
        hostname: hostname.unwrap_or_default(),
        tool_version: tool_version.unwrap_or_default(),
        entries,
    })
}

fn handle_start(
    tag: &str,
    start: &BytesStart<'_>,
    path: &Path,
    state: &mut ParseState,
    tool_version: &mut Option<String>,
) -> Result<(), IngestError> {
    match tag {
        "creatorinfo" => state.in_creatorinfo = true,
        "hash" => state.in_hash = true,
        "directoryhash" => state.in_directoryhash = true,
        "tool" if state.in_creatorinfo => {
            *tool_version = attr_value(start, "version", path)?;
        }
        "path" if state.in_hash && !state.in_directoryhash => {
            state.cur_size = attr_value(start, "size", path)?.and_then(|s| s.parse().ok());
        }
        "xxh64" if state.in_hash && !state.in_directoryhash => {
            state.cur_action = attr_value(start, "action", path)?;
            state.cur_hashdate = attr_value(start, "hashdate", path)?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_text(
    current_tag: Option<&str>,
    text: &str,
    state: &mut ParseState,
    creation_date: &mut Option<String>,
    hostname: &mut Option<String>,
) {
    match current_tag {
        Some("creationdate") if state.in_creatorinfo => *creation_date = Some(text.to_string()),
        Some("hostname") if state.in_creatorinfo => *hostname = Some(text.to_string()),
        Some("path") if state.in_hash && !state.in_directoryhash => {
            state.cur_rel = Some(text.to_string());
        }
        Some("xxh64") if state.in_hash && !state.in_directoryhash => {
            state.cur_xxh64 = Some(text.to_string());
        }
        _ => {}
    }
}

fn handle_end(
    tag: &str,
    path: &Path,
    state: &mut ParseState,
    entries: &mut Vec<MhlEntry>,
) -> Result<(), IngestError> {
    match tag {
        "creatorinfo" => state.in_creatorinfo = false,
        "directoryhash" => state.in_directoryhash = false,
        "hash" if !state.in_directoryhash => {
            entries.push(finish_hash_entry(state, path)?);
            state.in_hash = false;
        }
        _ => {}
    }
    Ok(())
}

fn finish_hash_entry(state: &mut ParseState, path: &Path) -> Result<MhlEntry, IngestError> {
    let rel = state.cur_rel.take().ok_or_else(|| IngestError::Mhl {
        path: path.to_path_buf(),
        msg: "<hash> missing <path>".to_string(),
    })?;
    let xxh64 = state.cur_xxh64.take().ok_or_else(|| IngestError::Mhl {
        path: path.to_path_buf(),
        msg: format!("<hash> for {rel:?} missing <xxh64>"),
    })?;
    let action_str = state.cur_action.take().ok_or_else(|| IngestError::Mhl {
        path: path.to_path_buf(),
        msg: format!("<hash> for {rel:?} missing xxh64 action attribute"),
    })?;
    let hashdate = state.cur_hashdate.take().unwrap_or_default();
    let size = state.cur_size.take().unwrap_or(0);
    Ok(MhlEntry {
        rel,
        size,
        xxh64,
        action: HashAction::parse(&action_str, path)?,
        hashdate,
    })
}

// ---------------------------------------------------------------------
// Chain file (ascmhl_chain.xml): required once any generation exists,
// and checked byte-for-byte on load by the oracle (see module doc).
// ---------------------------------------------------------------------

struct ChainEntry {
    sequencenr: u32,
    path: String,
    c4: String,
}

fn read_chain(chain_path: &Path) -> Result<Vec<ChainEntry>, IngestError> {
    if !chain_path.is_file() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(chain_path).map_err(|source| IngestError::Read {
        path: chain_path.to_path_buf(),
        source,
    })?;
    let text_content = std::str::from_utf8(&bytes).map_err(|source| IngestError::Mhl {
        path: chain_path.to_path_buf(),
        msg: format!("chain file is not valid UTF-8: {source}"),
    })?;
    let mut reader = QReader::from_str(text_content);
    reader.config_mut().trim_text(true);

    let mut entries = Vec::new();
    let mut cur_sequencenr: Option<u32> = None;
    let mut cur_path: Option<String> = None;
    let mut cur_c4: Option<String> = None;
    let mut tag_stack: Vec<String> = Vec::new();

    loop {
        match reader
            .read_event()
            .map_err(|source| xml_err(chain_path, source))?
        {
            Event::Eof => break,
            Event::Start(start) => {
                let tag = local_name(start.name());
                if tag == "hashlist" {
                    cur_sequencenr =
                        attr_value(&start, "sequencenr", chain_path)?.and_then(|s| s.parse().ok());
                }
                tag_stack.push(tag);
            }
            Event::Text(text) => {
                let text = unescape_text(&text, chain_path)?;
                match tag_stack.last().map(String::as_str) {
                    Some("path") => cur_path = Some(text),
                    Some("c4") => cur_c4 = Some(text),
                    _ => {}
                }
            }
            Event::End(end) => {
                let tag = local_name(end.name());
                tag_stack.pop();
                if tag == "hashlist"
                    && let (Some(sequencenr), Some(path), Some(c4)) =
                        (cur_sequencenr.take(), cur_path.take(), cur_c4.take())
                {
                    entries.push(ChainEntry {
                        sequencenr,
                        path,
                        c4,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(entries)
}

fn write_chain(chain_path: &Path, entries: &[ChainEntry]) -> Result<(), IngestError> {
    let mut writer = QWriter::new_with_indent(Vec::new(), b' ', 2);
    writer
        .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
        .map_err(|source| write_err(chain_path, &source))?;
    write_start(
        &mut writer,
        "ascmhldirectory",
        &[("xmlns", "urn:ASC:MHL:DIRECTORY:v2.0")],
        chain_path,
    )?;
    for entry in entries {
        let seq = entry.sequencenr.to_string();
        write_start(
            &mut writer,
            "hashlist",
            &[("sequencenr", seq.as_str())],
            chain_path,
        )?;
        write_start(&mut writer, "path", &[], chain_path)?;
        write_text(&mut writer, &entry.path, chain_path)?;
        write_end(&mut writer, "path", chain_path)?;
        write_start(&mut writer, "c4", &[], chain_path)?;
        write_text(&mut writer, &entry.c4, chain_path)?;
        write_end(&mut writer, "c4", chain_path)?;
        write_end(&mut writer, "hashlist", chain_path)?;
    }
    write_end(&mut writer, "ascmhldirectory", chain_path)?;

    let mut bytes = writer.into_inner();
    bytes.push(b'\n');
    std::fs::write(chain_path, &bytes).map_err(|source| IngestError::Mhl {
        path: chain_path.to_path_buf(),
        msg: format!("writing chain file: {source}"),
    })
}

// ---------------------------------------------------------------------
// c4 hash: SHA-512 digest, base58-encoded (Bitcoin alphabet), left-padded
// with the alphabet's zero character '1' to a fixed 88 characters, then
// prefixed "c4". Ported from `ascmhl.hasher.C4.string_digest` — see the
// module doc comment for why this, not xxh64, backs the chain file.
// ---------------------------------------------------------------------

const C4_CHARSET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const C4_ZERO: u8 = b'1';
const C4_DIGITS: usize = 88;

fn c4_hash(data: &[u8]) -> String {
    use sha2::{Digest, Sha512};
    let mut hasher = Sha512::new();
    hasher.update(data);
    let digest = hasher.finalize();

    let mut remainder_bytes: Vec<u8> = digest.to_vec();
    let mut digits: Vec<u8> = Vec::with_capacity(C4_DIGITS);
    while remainder_bytes.iter().any(|&b| b != 0) {
        let mut carry: u32 = 0;
        for byte in &mut remainder_bytes {
            let acc = (carry << 8) | u32::from(*byte);
            *byte = u8::try_from(acc / 58).unwrap_or(0);
            carry = acc % 58;
        }
        let index = usize::try_from(carry).unwrap_or(0);
        digits.push(C4_CHARSET[index]);
    }
    digits.reverse();

    let pad = C4_DIGITS.saturating_sub(digits.len());
    let mut out = String::with_capacity(90);
    out.push('c');
    out.push('4');
    for _ in 0..pad {
        out.push(char::from(C4_ZERO));
    }
    out.push_str(&String::from_utf8_lossy(&digits));
    out
}

#[cfg(test)]
mod c4_tests {
    use super::c4_hash;

    /// Known-answer tests computed once against the installed oracle via
    /// `ascmhl.hasher.C4.hash_data(...)` and pinned here.
    #[test]
    fn c4_of_empty_bytes_matches_oracle() {
        assert_eq!(
            c4_hash(b""),
            "c459dsjfscH38cYeXXYogktxf4Cd9ibshE3BHUo6a58hBXmRQdZrAkZzsWcbWtDg5oQstpDuni4Hirj75GEmTc1sFT"
        );
    }

    #[test]
    fn c4_of_short_bytes_matches_oracle() {
        assert_eq!(
            c4_hash(b"AAAA"),
            "c42g5VuiVU5krU8rQTJw7qNw4C1zjSu8Uy68H7tXWe7VvKqt2Y7ws3fmLtzV7FHw2Rz1SHD4BpVDsVNNWfTceC4Q47"
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use std::fs;

    fn write_fixture(root: &Path) {
        fs::create_dir_all(root.join("clips")).expect("mkdir clips");
        fs::write(root.join("clips/a.mov"), b"AAAA").expect("write a.mov");
        fs::write(root.join("b space.wav"), b"BBBBBB").expect("write b space.wav");
    }

    /// `hash_dir` must record only the one real asset — everything else
    /// here is something it's specifically supposed to skip: `.DS_Store`,
    /// an arbitrary dotfile, the copy engine's `.maj-partial-*` quarantine
    /// naming, and a file living inside the `ascmhl/` history directory
    /// itself (which would otherwise get hashed as if it were content).
    #[test]
    fn hash_dir_skips_quarantine_dot_and_history_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("real.mov"), b"REAL").expect("write real.mov");
        fs::write(dir.path().join(".DS_Store"), b"junk").expect("write .DS_Store");
        fs::write(dir.path().join(".hidden"), b"junk").expect("write .hidden");
        fs::write(dir.path().join(".maj-partial-xyz-a.mov"), b"partial").expect("write partial");
        fs::create_dir_all(dir.path().join("ascmhl")).expect("mkdir ascmhl");
        fs::write(
            dir.path().join("ascmhl/0001_x_2026-07-30_000000Z.mhl"),
            b"<hashlist/>",
        )
        .expect("write history file");

        let hash_list = hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        let rels: Vec<&str> = hash_list.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(
            rels,
            vec!["real.mov"],
            "expected only the real asset, got {rels:?}"
        );
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path());

        let hash_list = hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        assert_eq!(hash_list.entries.len(), 2);

        let written = write_generation(dir.path(), &hash_list).expect("write_generation");
        assert_eq!(written.generation, 1);
        assert!(written.path.is_file());

        let read_back = read_generation(&written.path).expect("read_generation");
        assert_eq!(read_back.creation_date, hash_list.creation_date);
        assert_eq!(read_back.hostname, hash_list.hostname);
        assert_eq!(read_back.entries.len(), 2);
        let mut rels: Vec<&str> = read_back.entries.iter().map(|e| e.rel.as_str()).collect();
        rels.sort_unstable();
        assert_eq!(rels, vec!["b space.wav", "clips/a.mov"]);
        for entry in &read_back.entries {
            assert_eq!(entry.action, HashAction::Original);
        }
    }

    #[test]
    fn sequential_generation_numbering_and_chain_records_both() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path());

        let first = hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        let written_one = write_generation(dir.path(), &first).expect("write generation 1");
        assert_eq!(written_one.generation, 1);

        let second = hash_dir(dir.path(), "2026-07-30T00:01:00Z").expect("hash_dir");
        let written_two = write_generation(dir.path(), &second).expect("write generation 2");
        assert_eq!(written_two.generation, 2);
        assert_ne!(written_one.path, written_two.path);
        assert_eq!(next_generation(dir.path()).expect("next_generation"), 3);

        let chain_path = dir.path().join("ascmhl/ascmhl_chain.xml");
        let chain_entries = read_chain(&chain_path).expect("read_chain");
        assert_eq!(chain_entries.len(), 2);
        assert_eq!(chain_entries[0].sequencenr, 1);
        assert_eq!(chain_entries[1].sequencenr, 2);
        assert_eq!(chain_entries[0].c4, written_one.roothash);
        assert_eq!(chain_entries[1].c4, written_two.roothash);
    }

    #[test]
    fn verify_scenario_alter_delete_add() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path());

        let baseline = hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        write_generation(dir.path(), &baseline).expect("write baseline generation");

        // alter a, delete b, add c
        fs::write(dir.path().join("clips/a.mov"), b"ZZZZZZZZ").expect("alter a.mov");
        fs::remove_file(dir.path().join("b space.wav")).expect("delete b space.wav");
        fs::write(dir.path().join("c.txt"), b"NEWFILE").expect("add c.txt");

        let report = verify_dir(dir.path(), "2026-07-30T00:01:00Z").expect("verify_dir");
        assert_eq!(report.altered, vec!["clips/a.mov".to_string()]);
        assert_eq!(report.missing, vec!["b space.wav".to_string()]);
        assert_eq!(report.new_files, vec!["c.txt".to_string()]);
        assert!(report.verified.is_empty());
        assert_eq!(report.written.generation, 2);

        let second = read_generation(&report.written.path).expect("read new generation");
        let by_rel: std::collections::HashMap<&str, &MhlEntry> =
            second.entries.iter().map(|e| (e.rel.as_str(), e)).collect();
        assert_eq!(
            by_rel.len(),
            2,
            "missing file must be dropped, not carried forward"
        );
        assert_eq!(by_rel["clips/a.mov"].action, HashAction::Failed);
        assert_eq!(by_rel["c.txt"].action, HashAction::Original);
        assert!(!by_rel.contains_key("b space.wav"));
    }

    #[test]
    fn verify_dir_with_no_history_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_fixture(dir.path());
        let result = verify_dir(dir.path(), "2026-07-30T00:00:00Z");
        assert!(result.is_err());
    }
}
