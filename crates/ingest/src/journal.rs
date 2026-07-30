//! Transfer journal: JSONL checkpoints making a run resumable at file
//! granularity. One line per state transition; the tail line may be torn
//! (crash mid-write) and is skipped on load.
use crate::IngestError;
use crate::plan::PlannedFile;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
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
    /// itself if needed.
    ///
    /// # Errors
    /// Returns `IngestError::Journal` if the parent directory cannot be
    /// created or the file cannot be opened for append.
    pub fn create(path: &Path) -> Result<Self, IngestError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| IngestError::Journal {
                path: path.to_path_buf(),
                source,
            })?;
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
    /// A missing file folds to an empty `Folded` (nothing has run yet). A
    /// torn tail line — the file ends mid-record because a previous run
    /// crashed between `write_all` and the next line's start — is not an
    /// error: everything up to that line still counts, so parsing stops at
    /// the first line that fails to decode rather than failing the whole
    /// load.
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
                // Torn tail: a crash mid-write can leave a truncated last
                // line. Everything decoded so far is a valid checkpoint;
                // stop folding rather than treating this as a fatal error.
                break;
            };
            fold_one(&mut folded, record);
        }
        Ok(folded)
    }
}

fn fold_one(folded: &mut Folded, record: Record) {
    match record {
        Record::FilePlanned { file } => {
            folded.planned.insert(file.rel.clone(), file);
        }
        Record::FilePlaced { rel } => {
            folded.placed.insert(rel);
        }
        Record::FileFailed { rel, reason } => {
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
        let mut journal = Journal::create(&path).expect("create");
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

    #[test]
    fn corrupt_trailing_line_is_tolerated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("run.jsonl");
        let mut journal = Journal::create(&path).expect("create");
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

    proptest::proptest! {
        /// Any prefix of a journal folds without panicking, and its placed
        /// set is a subset of the full journal's placed set.
        #[test]
        fn any_prefix_folds_consistently(cut in 0usize..6) {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("run.jsonl");
            {
                let mut journal = Journal::create(&path).expect("create");
                journal.append(&Record::FilePlanned { file: planned("a") }).expect("planned a");
                journal.append(&Record::FileCopied { rel: "a".into() }).expect("copied a");
                journal.append(&Record::FileVerified { rel: "a".into() }).expect("verified a");
                journal.append(&Record::FilePlaced { rel: "a".into() }).expect("placed a");
                journal.append(&Record::FilePlanned { file: planned("b") }).expect("planned b");
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
            prop_assert!(prefix.placed.is_subset(&full.placed));
        }
    }
}
