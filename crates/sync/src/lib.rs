//! File-based event log: `events/<machine-id>/NNNN.jsonl` under a sync
//! root. Append-only; reading merges every machine's segments. Designed
//! so dumb transports (Dropbox, rsync, a shuttle drive) can carry it.
use majestical_core::clock::MachineId;
use majestical_core::event::Event;
use majestical_core::ports::{EventLog, LogCursor, PortError};
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
    #[error("no event log at {} — initialize the catalog first", path.display())]
    NotInitialized { path: PathBuf },
    #[error("serializing event: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cursor for {machine}/{segment} is stale (offset {offset}): full rebuild required")]
    StaleCursor {
        machine: String,
        segment: String,
        offset: u64,
    },
}

pub struct FileEventLog {
    root: PathBuf,
    machine_dir: PathBuf,
}

impl FileEventLog {
    /// Initializes a fresh catalog root: creates `<root>/events` and
    /// `machine`'s own segment directory under it. Call once per catalog
    /// root (`maj catalog init`), before any `open`.
    ///
    /// Idempotent, git-init style: re-running against an already
    /// initialized root just creates any missing directories and never
    /// touches existing segment files, so it's always safe to call again.
    ///
    /// # Errors
    /// Returns [`LogError::Io`] if the directories can't be created.
    pub fn init(root: &Path, machine: &MachineId) -> Result<Self, LogError> {
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

    /// Opens the segment directory for `machine` under an already
    /// initialized `root`. Creates this machine's own subdirectory if it
    /// doesn't exist yet — a new machine joining an existing catalog — but
    /// requires `<root>/events` itself to already exist; use [`Self::init`]
    /// to create a catalog root from scratch.
    ///
    /// # Errors
    /// Returns [`LogError::NotInitialized`] if `<root>/events` is missing,
    /// or [`LogError::Io`] if this machine's segment directory can't be
    /// created.
    pub fn open(root: &Path, machine: &MachineId) -> Result<Self, LogError> {
        let events_dir = root.join("events");
        if !events_dir.is_dir() {
            return Err(LogError::NotInitialized { path: events_dir });
        }
        let machine_dir = events_dir.join(&machine.0);
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
    /// No fsync: a crash mid-write may drop the tail of the batch, and
    /// readers tolerate torn tails by treating the incomplete line as a bad
    /// line rather than failing the whole read.
    ///
    /// # Errors
    /// Returns [`LogError::Serde`] if an event can't be serialized, or
    /// [`LogError::Io`] if the segment file can't be opened or written to.
    pub fn append(&mut self, events: &[Event]) -> Result<(), LogError> {
        let seg = self.machine_dir.join("0001.jsonl");
        let mut batch = String::new();
        for e in events {
            let line = serde_json::to_string(e)?;
            batch.push_str(&line);
            batch.push('\n');
        }
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg)
            .map_err(|source| LogError::Io {
                path: seg.clone(),
                source,
            })?;
        f.write_all(batch.as_bytes())
            .map_err(|source| LogError::Io {
                path: seg.clone(),
                source,
            })?;
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
    /// Returned order is grouped by machine (directory iteration order,
    /// which is unspecified), with segments sorted within each machine —
    /// there is no global HLC order. Callers must not assume one; the CRDT
    /// projection this feeds is order-independent by design.
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
                let file_type = entry.file_type().map_err(|source| LogError::Io {
                    path: entry.path(),
                    source,
                })?;
                let path = entry.path();
                if file_type.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
                    segments.push(path);
                }
            }
            // Lexicographic sort: segment names must stay equal-width
            // (NNNN.jsonl) for this to also be numeric order. The rotation
            // implementer must preserve zero-padding or switch to a
            // numeric-aware sort.
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

    /// `.jsonl` segments directly under `machine_dir`, as (file name, path)
    /// pairs sorted lexicographically — same ordering constraint as
    /// [`Self::read_all_reporting`]: segment names must stay zero-padded and
    /// equal-width for lexicographic order to also be numeric order.
    fn list_segments(machine_dir: &Path) -> Result<Vec<(String, PathBuf)>, LogError> {
        let entries = fs::read_dir(machine_dir).map_err(|source| LogError::Io {
            path: machine_dir.to_path_buf(),
            source,
        })?;
        let mut segments = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| LogError::Io {
                path: machine_dir.to_path_buf(),
                source,
            })?;
            let file_type = entry.file_type().map_err(|source| LogError::Io {
                path: entry.path(),
                source,
            })?;
            let path = entry.path();
            if file_type.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
                segments.push((entry.file_name().to_string_lossy().into_owned(), path));
            }
        }
        segments.sort();
        Ok(segments)
    }

    /// Reads one segment from byte offset `from` to its last complete line,
    /// reporting parse failures through `on_bad_line`. Returns the parsed
    /// events plus the new offset (`from` plus whole bytes consumed); a torn
    /// tail after the last `\n` is left unconsumed.
    fn read_segment_since(
        seg: &Path,
        from: u64,
        mut on_bad_line: impl FnMut(&str),
    ) -> Result<(Vec<Event>, u64), LogError> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut f = fs::File::open(seg).map_err(|source| LogError::Io {
            path: seg.to_path_buf(),
            source,
        })?;
        f.seek(SeekFrom::Start(from))
            .map_err(|source| LogError::Io {
                path: seg.to_path_buf(),
                source,
            })?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(|source| LogError::Io {
            path: seg.to_path_buf(),
            source,
        })?;
        let consumed = buf.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let mut events = Vec::new();
        for line in buf[..consumed].split(|&b| b == b'\n') {
            if line.is_empty() {
                continue;
            }
            match std::str::from_utf8(line) {
                Ok(text) => match serde_json::from_str::<Event>(text) {
                    Ok(event) => events.push(event),
                    Err(_) => on_bad_line(text),
                },
                Err(_) => on_bad_line(&String::from_utf8_lossy(line)),
            }
        }
        let new_offset = from + u64::try_from(consumed).unwrap_or(u64::MAX);
        Ok((events, new_offset))
    }

    /// Reads only events past `cursors`, returning the new events plus
    /// updated cursors covering every segment seen (unknown segments read
    /// from 0 and gain a cursor of their own).
    ///
    /// Mirrors [`Self::read_all_reporting`]'s directory walk, but seeks each
    /// segment to its cursor offset instead of reading from the start, and
    /// stops at the last complete line: a torn tail (a write in progress)
    /// stays unconsumed so the cursor never advances past it, and it's
    /// re-read — and only then possibly reported as a bad line — on the
    /// next call once the write completes.
    ///
    /// Cursors are returned sorted by machine then segment, so two calls
    /// that see no new data produce equal cursor lists.
    ///
    /// # Errors
    /// Returns [`LogError::Io`] if the events directory or a machine's
    /// segments can't be read, or [`LogError::StaleCursor`] if a cursor
    /// points past the end of its segment, or names a segment that no
    /// longer exists.
    pub fn read_since_reporting(
        &self,
        cursors: &[LogCursor],
        mut on_bad_line: impl FnMut(&str),
    ) -> Result<(Vec<Event>, Vec<LogCursor>), LogError> {
        let mut start: std::collections::BTreeMap<(String, String), u64> = cursors
            .iter()
            .map(|c| ((c.machine.clone(), c.segment.clone()), c.offset))
            .collect();

        let events_dir = self.root.join("events");
        let mut events = Vec::new();
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
            let machine_name = machine.file_name().to_string_lossy().into_owned();
            for (segment_name, seg) in Self::list_segments(&machine.path())? {
                let from = start
                    .remove(&(machine_name.clone(), segment_name.clone()))
                    .unwrap_or(0);
                let len = fs::metadata(&seg)
                    .map_err(|source| LogError::Io {
                        path: seg.clone(),
                        source,
                    })?
                    .len();
                if from > len {
                    return Err(LogError::StaleCursor {
                        machine: machine_name,
                        segment: segment_name,
                        offset: from,
                    });
                }
                let (segment_events, offset) =
                    Self::read_segment_since(&seg, from, &mut on_bad_line)?;
                events.extend(segment_events);
                out.push(LogCursor {
                    machine: machine_name.clone(),
                    segment: segment_name,
                    offset,
                });
            }
        }
        if let Some(((machine, segment), offset)) = start.into_iter().next() {
            return Err(LogError::StaleCursor {
                machine,
                segment,
                offset,
            });
        }
        out.sort_by(|a, b| (&a.machine, &a.segment).cmp(&(&b.machine, &b.segment)));
        Ok((events, out))
    }
}

impl EventLog for FileEventLog {
    fn append(&mut self, events: &[Event]) -> Result<(), PortError> {
        Self::append(self, events).map_err(|e| PortError::new("event log", e))
    }

    fn read_all_reporting(
        &self,
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<Vec<Event>, PortError> {
        Self::read_all_reporting(self, |line| on_bad_line(line))
            .map_err(|e| PortError::new("event log", e))
    }

    fn read_since_reporting(
        &self,
        cursors: &[LogCursor],
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<(Vec<Event>, Vec<LogCursor>), PortError> {
        Self::read_since_reporting(self, cursors, |line| on_bad_line(line))
            .map_err(|e| PortError::new("reading new events", e))
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
    fn open_errors_when_root_not_initialized() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = FileEventLog::open(dir.path(), &MachineId("m1".into()));
        assert!(
            matches!(result, Err(LogError::NotInitialized { .. })),
            "open must fail on an uninitialized root"
        );
    }

    #[test]
    fn append_then_read_all_machines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log1 = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init m1");
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
        let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
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
        let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
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
        let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1)]).expect("append");
        std::fs::create_dir(dir.path().join("events/m1/0002.jsonl")).expect("mkdir");
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn events_merge_across_segments_in_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1)]).expect("append segment 1");
        let seg2 = dir.path().join("events/m1/0002.jsonl");
        let line = serde_json::to_string(&ev(2)).expect("serialize");
        std::fs::write(&seg2, format!("{line}\n")).expect("write segment 2");
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].hlc.wall_ms, 1);
        assert_eq!(all[1].hlc.wall_ms, 2);
    }

    #[test]
    fn read_since_empty_cursors_returns_everything_with_cursors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1), ev(2)]).expect("append");
        let (events, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
        assert_eq!(events.len(), 2);
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].machine, "m1");
        assert_eq!(cursors[0].segment, "0001.jsonl");
        let len = std::fs::metadata(dir.path().join("events/m1/0001.jsonl"))
            .expect("meta")
            .len();
        assert_eq!(cursors[0].offset, len);
    }

    #[test]
    fn read_since_returns_only_new_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append");
        let (_, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
        log.append(&[ev(2), ev(3)]).expect("append");
        let (events, cursors2) = log.read_since_reporting(&cursors, |_| {}).expect("read");
        assert_eq!(events.len(), 2);
        assert!(cursors2[0].offset > cursors[0].offset);
        let (empty, cursors3) = log.read_since_reporting(&cursors2, |_| {}).expect("read");
        assert!(empty.is_empty());
        assert_eq!(cursors2, cursors3);
    }

    #[test]
    fn a_torn_tail_is_not_consumed_until_completed() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append");
        let seg = dir.path().join("events/m1/0001.jsonl");
        let complete_len = std::fs::metadata(&seg).expect("meta").len();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&seg)
            .expect("open");
        f.write_all(b"{\"id\":\"torn").expect("write");
        let (events, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
        assert_eq!(events.len(), 1, "torn tail is deferred, not reported bad");
        assert_eq!(
            cursors[0].offset, complete_len,
            "cursor stops at the last newline"
        );
    }

    #[test]
    fn a_stale_cursor_past_the_end_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append");
        let stale = LogCursor {
            machine: "m1".into(),
            segment: "0001.jsonl".into(),
            offset: 999_999,
        };
        assert!(log.read_since_reporting(&[stale], |_| {}).is_err());
    }

    #[test]
    fn a_cursor_for_a_vanished_segment_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let log = FileEventLog::init(dir.path(), &m).expect("init");
        let stale = LogCursor {
            machine: "mgone".into(),
            segment: "0001.jsonl".into(),
            offset: 1,
        };
        assert!(log.read_since_reporting(&[stale], |_| {}).is_err());
    }

    #[test]
    fn a_new_machine_appearing_after_cursors_were_taken_is_read_from_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m1 = MachineId("m1".into());
        let mut log1 = FileEventLog::init(dir.path(), &m1).expect("init m1");
        log1.append(&[ev(1)]).expect("append m1");
        let (_, cursors) = log1.read_since_reporting(&[], |_| {}).expect("read");
        assert_eq!(cursors.len(), 1);

        let m2 = MachineId("m2".into());
        let mut log2 = FileEventLog::open(dir.path(), &m2).expect("open m2");
        log2.append(&[ev(2)]).expect("append m2");

        let (events, cursors2) = log1.read_since_reporting(&cursors, |_| {}).expect("read");
        assert_eq!(events.len(), 1, "only the new machine's event is new");
        assert_eq!(cursors2.len(), 2, "the new machine gains a cursor too");
        let m2_cursor = cursors2
            .iter()
            .find(|c| c.machine == "m2")
            .expect("m2 cursor present");
        assert_eq!(m2_cursor.segment, "0001.jsonl");
        let len = std::fs::metadata(dir.path().join("events/m2/0001.jsonl"))
            .expect("meta")
            .len();
        assert_eq!(m2_cursor.offset, len);
    }
}
