//! File-based event log: `events/<machine-id>/NNNN.jsonl` under a sync
//! root. Append-only; reading merges every machine's segments. Designed
//! so dumb transports (Dropbox, rsync, a shuttle drive) can carry it.
use majestical_core::clock::MachineId;
use majestical_core::event::Event;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error(
        "event log io at {}: {source} — check the sync root is accessible",
        path.display()
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serializing event: {0}")]
    Serde(#[from] serde_json::Error),
}

pub struct FileEventLog {
    root: PathBuf,
    machine_dir: PathBuf,
}

impl FileEventLog {
    /// Opens (creating if needed) the segment directory for `machine` under `root`.
    ///
    /// # Errors
    /// Returns [`LogError::Io`] if the machine's segment directory can't be created.
    pub fn open(root: &Path, machine: &MachineId) -> Result<Self, LogError> {
        let machine_dir = root.join("events").join(&machine.0);
        fs::create_dir_all(&machine_dir).map_err(|source| LogError::Io {
            path: machine_dir.clone(),
            source,
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            machine_dir,
        })
    }

    /// Append to this machine's current segment (0001.jsonl for phase 1;
    /// segment rotation arrives with sync push/pull in a later phase).
    ///
    /// # Errors
    /// Returns [`LogError::Serde`] if an event can't be serialized, or
    /// [`LogError::Io`] if the segment file can't be opened or written to.
    pub fn append(&mut self, events: &[Event]) -> Result<(), LogError> {
        let seg = self.machine_dir.join("0001.jsonl");
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg)
            .map_err(|source| LogError::Io {
                path: seg.clone(),
                source,
            })?;
        for e in events {
            let line = serde_json::to_string(e)?;
            writeln!(f, "{line}").map_err(|source| LogError::Io {
                path: seg.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// # Errors
    /// Returns [`LogError::Io`] if the events directory or a machine's
    /// segments can't be read.
    pub fn read_all(&self) -> Result<Vec<Event>, LogError> {
        self.read_all_reporting(|_| {})
    }

    /// Corrupt lines are skipped and reported, never fatal: one bad byte
    /// on a shuttle drive must not take down the whole catalog.
    ///
    /// # Errors
    /// Returns [`LogError::Io`] if the events directory or a machine's
    /// segments can't be read.
    pub fn read_all_reporting(
        &self,
        mut on_bad_line: impl FnMut(&str),
    ) -> Result<Vec<Event>, LogError> {
        let events_dir = self.root.join("events");
        let mut out = Vec::new();
        let machines = fs::read_dir(&events_dir).map_err(|source| LogError::Io {
            path: events_dir.clone(),
            source,
        })?;
        for machine in machines {
            let machine = machine.map_err(|source| LogError::Io {
                path: events_dir.clone(),
                source,
            })?;
            let is_dir = machine.file_type().map_err(|source| LogError::Io {
                path: machine.path(),
                source,
            })?;
            if !is_dir.is_dir() {
                continue;
            }
            let segment_entries = fs::read_dir(machine.path()).map_err(|source| LogError::Io {
                path: machine.path(),
                source,
            })?;
            let mut segments: Vec<PathBuf> = Vec::new();
            for entry in segment_entries {
                let entry = entry.map_err(|source| LogError::Io {
                    path: machine.path(),
                    source,
                })?;
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
                    segments.push(path);
                }
            }
            segments.sort();
            for seg in segments {
                let text = fs::read_to_string(&seg).map_err(|source| LogError::Io {
                    path: seg.clone(),
                    source,
                })?;
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    match serde_json::from_str::<Event>(line) {
                        Ok(e) => out.push(e),
                        Err(_) => on_bad_line(line),
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{AssetId, Event, EventId, Op};

    fn ev(n: u64) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(n, u128::from(n))),
            hlc: Hlc {
                wall_ms: n,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: AssetId("xxh3:aa".into()),
                tag: "t".into(),
            },
        }
    }

    #[test]
    fn append_then_read_all_machines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log1 = FileEventLog::open(dir.path(), &MachineId("m1".into())).expect("open m1");
        let mut log2 = FileEventLog::open(dir.path(), &MachineId("m2".into())).expect("open m2");
        log1.append(&[ev(1), ev(2)]).expect("append m1");
        log2.append(&[ev(3)]).expect("append m2");
        let all = FileEventLog::open(dir.path(), &MachineId("m3".into()))
            .expect("open m3")
            .read_all()
            .expect("read");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn corrupt_line_is_skipped_and_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::open(dir.path(), &MachineId("m1".into())).expect("open");
        log.append(&[ev(1)]).expect("append");
        let seg = dir.path().join("events/m1/0001.jsonl");
        let good = std::fs::read_to_string(&seg).expect("read seg");
        std::fs::write(&seg, format!("{}\nnot json\n", good.trim())).expect("write");
        let mut skipped = 0;
        let all = log.read_all_reporting(|_line| skipped += 1).expect("read");
        assert_eq!((all.len(), skipped), (1, 1));
    }

    #[test]
    fn stray_files_in_events_root_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::open(dir.path(), &MachineId("m1".into())).expect("open");
        log.append(&[ev(1)]).expect("append");
        let events_dir = dir.path().join("events");
        std::fs::write(events_dir.join(".DS_Store"), b"junk").expect("write DS_Store");
        std::fs::write(events_dir.join("conflicted copy.jsonl"), b"junk").expect("write stray");
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn directory_named_like_segment_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::open(dir.path(), &MachineId("m1".into())).expect("open");
        log.append(&[ev(1)]).expect("append");
        std::fs::create_dir(dir.path().join("events/m1/0002.jsonl")).expect("mkdir");
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 1);
    }
}
