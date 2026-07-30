//! Transfer journal: JSONL checkpoints making a run resumable at file
//! granularity. One line per state transition; the tail line may be torn
//! (crash mid-write) and is skipped on load.
use crate::IngestError;
use crate::plan::PlannedFile;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "rec", rename_all = "snake_case")]
pub enum Record {
    RunStarted {
        run: String,
        source: String,
        dests: Vec<String>,
    },
    FilePlanned {
        file: PlannedFile,
    },
    FileCopied {
        rel: String,
    },
    FileVerified {
        rel: String,
    },
    FilePlaced {
        rel: String,
    },
    FileFailed {
        rel: String,
        reason: String,
    },
}

/// Folded view of a journal: what a resume needs to know.
#[derive(Debug, Default)]
pub struct Folded {
    pub planned: BTreeMap<String, PlannedFile>,
    pub placed: BTreeSet<String>,
    pub failed: BTreeMap<String, String>,
}

/// A resumable, append-only JSONL log of file-transfer state transitions.
pub struct Journal {
    file: std::fs::File,
    path: PathBuf,
}

impl Journal {
    /// Opens `path` for append, creating its parent directory and the file
    /// itself if needed. This is also how a resumed run reopens an
    /// existing journal: the name says `open_append` rather than `create`
    /// because the common case on a resume is a journal that already has
    /// records in it.
    ///
    /// If the file already ends mid-record — a previous run crashed after
    /// `write_all` started but before the terminating newline landed — that
    /// torn tail is repaired here by appending the missing newline before
    /// this run writes anything. Without that repair, this run's first
    /// appended record would be glued onto the torn line, becoming
    /// undecodable itself; `load` stops at the first line it can't decode,
    /// so every record this run subsequently writes would be invisible on
    /// the next load, not just the torn one.
    ///
    /// # Errors
    /// Returns `IngestError::Journal` if the parent directory cannot be
    /// created, the file cannot be opened for append, or the torn-tail
    /// repair's read/write/fsync fails.
    pub fn open_append(path: &Path) -> Result<Self, IngestError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)
            .map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
        repair_torn_tail(&mut file, path)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Appends `record` as one JSON line and fsyncs before returning.
    ///
    /// A checkpoint is only a checkpoint if it survives the crash that
    /// follows it, so this does not return until the line is durable on
    /// disk — callers rely on that to decide what's safe to resume from.
    /// The engine serializes every append behind one mutex, so this fsync
    /// happens on the critical path of every worker thread rather than in
    /// parallel; that trades throughput for the simplicity of a single
    /// linearized journal, a tradeoff worth revisiting with benchmarks if
    /// journal I/O ever shows up as the bottleneck.
    ///
    /// # Errors
    /// Returns `IngestError::JournalEncode` if `record` cannot be
    /// serialized, or `IngestError::Journal` if the write or fsync fails.
    pub fn append(&mut self, record: &Record) -> Result<(), IngestError> {
        let mut line = serde_json::to_string(record).map_err(IngestError::JournalEncode)?;
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .map_err(|source| IngestError::Journal {
                path: self.path.clone(),
                source,
            })?;
        self.file
            .sync_data()
            .map_err(|source| IngestError::Journal {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }

    /// Reads and folds every complete record in the journal at `path`.
    ///
    /// A missing file folds to an empty `Folded` (nothing has run yet). Any
    /// line that fails to decode — a torn tail from a crash mid-`write_all`,
    /// or an old torn line `open_append` has since newline-terminated in
    /// place — is skipped rather than treated as a fatal error, and folding
    /// continues with whatever lines follow it. Now that `fold_one` lets a
    /// later `FileFailed` demote an earlier `placed` entry, losing a
    /// demotion's line to corruption leaves that rel in `placed` when it
    /// should not be — but such a loss can only be a torn write of that
    /// very record (the one case corruption can strike is the record that
    /// never finished landing), which `break` would have lost identically;
    /// `continue` is no worse than the alternative it replaced.
    ///
    /// # Errors
    /// Returns `IngestError::Journal` if the file exists but cannot be read.
    pub fn load(path: &Path) -> Result<Folded, IngestError> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Folded::default());
            }
            Err(source) => {
                return Err(IngestError::Journal {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let mut folded = Folded::default();
        for line in contents.lines() {
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                // An undecodable line is either a genuine torn tail (a
                // crash mid-`write_all`, before the terminating newline
                // landed) or — once `open_append`'s repair has terminated
                // such a line with a newline of its own — an inert garbled
                // line sitting in the middle of the file's history rather
                // than at its end. Either way the record is unrecoverable
                // but not fatal: skip it and keep folding whatever comes
                // after, rather than stopping the whole load there.
                continue;
            };
            fold_one(&mut folded, record);
        }
        Ok(folded)
    }
}

/// Terminates a torn last line left by a crash mid-`write_all`. The file
/// was opened with `.append(true)`, so this write always lands at the true
/// end of file regardless of the seek used to read the last byte — append
/// mode ignores the current position for writes.
fn repair_torn_tail(file: &mut std::fs::File, path: &Path) -> Result<(), IngestError> {
    let to_journal_err = |source| IngestError::Journal {
        path: path.to_path_buf(),
        source,
    };
    let len = file.metadata().map_err(to_journal_err)?.len();
    if len == 0 {
        return Ok(());
    }
    let mut last_byte = [0u8; 1];
    file.seek(SeekFrom::End(-1)).map_err(to_journal_err)?;
    file.read_exact(&mut last_byte).map_err(to_journal_err)?;
    if last_byte[0] != b'\n' {
        file.write_all(b"\n").map_err(to_journal_err)?;
        file.sync_data().map_err(to_journal_err)?;
    }
    Ok(())
}

/// Folds one record into `folded`. `placed` and `failed` are mutually
/// exclusive per rel and order-sensitive: a later `FileFailed` (e.g. from
/// an end-of-run sweep demoting a file whose final path vanished) removes
/// that rel from `placed`, and symmetrically a later `FilePlaced` (a
/// successful retry after a prior failure) clears any stale entry from
/// `failed`. Without this, a demotion recorded after the original
/// `FilePlaced` line would leave the rel in both sets — and since resume
/// only ever consults `placed`, the stale entry there is the one that
/// matters: it would make resume skip re-copying a file the journal itself
/// says is missing.
fn fold_one(folded: &mut Folded, record: Record) {
    match record {
        Record::FilePlanned { file } => {
            folded.planned.insert(file.rel.clone(), file);
        }
        Record::FilePlaced { rel } => {
            folded.failed.remove(&rel);
            folded.placed.insert(rel);
        }
        Record::FileFailed { rel, reason } => {
            folded.placed.remove(&rel);
            folded.failed.insert(rel, reason);
        }
        Record::RunStarted { .. } | Record::FileCopied { .. } | Record::FileVerified { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Decision, PlannedFile};
    use proptest::prop_assert;

    fn planned(rel: &str) -> PlannedFile {
        PlannedFile {
            source: std::path::PathBuf::from(format!("/src/{rel}")),
            rel: rel.into(),
            size: 4,
            prehash: None,
            decision: Decision::Copy,
        }
    }

    #[test]
    fn journal_round_trips_and_folds_placed_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        let mut journal = Journal::open_append(&path).expect("open_append");
        journal
            .append(&Record::FilePlanned { file: planned("a") })
            .expect("append a planned");
        journal
            .append(&Record::FilePlanned { file: planned("b") })
            .expect("append b planned");
        journal
            .append(&Record::FileCopied { rel: "a".into() })
            .expect("append a copied");
        journal
            .append(&Record::FileVerified { rel: "a".into() })
            .expect("append a verified");
        journal
            .append(&Record::FilePlaced { rel: "a".into() })
            .expect("append a placed");
        journal
            .append(&Record::FileFailed {
                rel: "b".into(),
                reason: "verify mismatch".into(),
            })
            .expect("append b failed");

        let folded = Journal::load(&path).expect("load");
        assert!(folded.placed.contains("a"));
        assert!(!folded.placed.contains("b"));
        assert_eq!(folded.failed.get("b"), Some(&"verify mismatch".to_string()));
        assert_eq!(folded.planned.len(), 2);
    }

    /// `load`'s `NotFound` guard exists to distinguish "no journal written
    /// yet" (fold to empty, not an error) from a genuine I/O problem, which
    /// must still propagate. A directory where a file is expected produces
    /// a `read_to_string` error whose kind is not `NotFound`, so this must
    /// surface as `Err`, not silently fold to an empty `Folded` — the
    /// discriminator for a guard mutated to always match.
    #[test]
    fn load_propagates_a_non_missing_error_instead_of_folding_to_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("not-a-file");
        std::fs::create_dir(&path).expect("mkdir");
        let result = Journal::load(&path);
        assert!(
            result.is_err(),
            "a directory at the journal path must not silently fold to an empty journal"
        );
    }

    #[test]
    fn corrupt_trailing_line_is_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        let mut journal = Journal::open_append(&path).expect("open_append");
        journal
            .append(&Record::FilePlanned { file: planned("a") })
            .expect("append a planned");
        {
            let mut raw = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("reopen raw");
            raw.write_all(b"{\"rec\":\"file_pl")
                .expect("write torn tail");
        }
        let folded = Journal::load(&path).expect("load tolerates torn tail");
        assert_eq!(folded.planned.len(), 1);
    }

    #[test]
    fn reopening_after_a_torn_tail_repairs_it_before_appending() {
        // Without the repair, appending "b" glues it onto the torn line,
        // producing one undecodable line; `load` would then stop at that
        // line and see neither "a" (fine, it's the untouched prior line)
        // nor "b" (the real bug: a resumed run's own new record vanishes).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        {
            let mut journal = Journal::open_append(&path).expect("open_append");
            journal
                .append(&Record::FilePlanned { file: planned("a") })
                .expect("append a planned");
        }
        {
            let mut raw = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("reopen raw");
            raw.write_all(b"{\"rec\":\"file_pl")
                .expect("write torn tail");
        }
        {
            let mut journal = Journal::open_append(&path).expect("reopen repairs torn tail");
            journal
                .append(&Record::FilePlanned { file: planned("b") })
                .expect("append b planned");
        }
        let folded = Journal::load(&path).expect("load");
        assert_eq!(
            folded.planned.len(),
            2,
            "both real records must fold, not just the one before the torn tail"
        );
    }

    proptest::proptest! {
        /// Any prefix of a journal folds without panicking. Since a later
        /// `FileFailed` can now demote an earlier `FilePlaced` (fold_one is
        /// order-sensitive), a prefix's placed set is NOT generally a
        /// subset of the full fold's placed set: "b" below is placed
        /// within a short prefix but demoted to failed by a line that only
        /// appears in the full sequence, so `prefix.placed` contains "b"
        /// while `full.placed` does not. What does still hold is the
        /// mutual-exclusion invariant `fold_one` maintains per rel: any rel
        /// a prefix ever placed ends up in exactly one of the full fold's
        /// `placed` or `failed` sets, never neither.
        #[test]
        fn any_prefix_folds_consistently(cut in 0usize..8) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("run.jsonl");
            {
                let mut journal = Journal::open_append(&path).expect("open_append");
                journal.append(&Record::FilePlanned { file: planned("a") }).expect("planned a");
                journal.append(&Record::FileCopied { rel: "a".into() }).expect("copied a");
                journal.append(&Record::FileVerified { rel: "a".into() }).expect("verified a");
                journal.append(&Record::FilePlaced { rel: "a".into() }).expect("placed a");
                journal.append(&Record::FilePlanned { file: planned("b") }).expect("planned b");
                journal.append(&Record::FilePlaced { rel: "b".into() }).expect("placed b");
                journal.append(&Record::FileFailed {
                    rel: "b".into(),
                    reason: "demoted".into(),
                }).expect("failed b");
            }
            let full = Journal::load(&path).expect("load full");

            let contents = std::fs::read_to_string(&path).expect("read for truncation");
            let lines: Vec<&str> = contents.lines().collect();
            let mut prefix_text = String::new();
            for line in lines.iter().take(cut) {
                prefix_text.push_str(line);
                prefix_text.push('\n');
            }
            std::fs::write(&path, prefix_text).expect("write prefix");

            let prefix = Journal::load(&path).expect("load prefix");
            let full_placed_or_failed: BTreeSet<String> = full
                .placed
                .iter()
                .cloned()
                .chain(full.failed.keys().cloned())
                .collect();
            prop_assert!(prefix.placed.is_subset(&full_placed_or_failed));
        }
    }
}
