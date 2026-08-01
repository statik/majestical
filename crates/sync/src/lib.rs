//! File-based event log: `events/<machine-id>/NNNN.jsonl` under a sync
//! root. Append-only; reading merges every machine's segments. Designed
//! so dumb transports (Dropbox, rsync, a shuttle drive) can carry it.
use majestical_core::clock::MachineId;
use majestical_core::event::Event;
use majestical_core::ports::{EventLog, LogCursor, PortError};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Rotation threshold: an append that would grow the active segment past
/// this starts the next `NNNN.jsonl` instead, bounding the whole-file
/// re-copy cost of `maj sync push` (segments transfer longer-wins as whole
/// files). Rotated segments are immutable thereafter.
pub(crate) const ROTATE_BYTES: u64 = 4 * 1024 * 1024;
/// Segment names are zero-padded width-4 so lexicographic order is numeric
/// order (see `list_segments`); 9999 is therefore the namespace's end.
const MAX_SEGMENT: u32 = 9999;

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
    #[error(
        "machine {machine} reached segment 9999 — the log segment namespace is exhausted; this catalog needs a new machine id"
    )]
    SegmentOverflow { machine: String },
}

impl LogError {
    /// `map_err(LogError::io(&path))`.
    fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
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
        fs::create_dir_all(&machine_dir).map_err(LogError::io(&machine_dir))?;
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
        fs::create_dir_all(&machine_dir).map_err(LogError::io(&machine_dir))?;
        Ok(Self {
            root: root.to_path_buf(),
            machine_dir,
        })
    }

    /// Append to this machine's active segment. Batches that would grow the
    /// active segment past [`ROTATE_BYTES`] start the next `NNNN.jsonl`
    /// instead; a fresh machine directory starts at `0001.jsonl`. Rotated
    /// segments are never appended to again.
    ///
    /// No fsync: a crash mid-write may drop the tail of the batch. See
    /// [`Self::read_segment_since`] for how readers handle the resulting
    /// torn tail.
    ///
    /// # Errors
    /// Returns [`LogError::Serde`] if an event can't be serialized,
    /// [`LogError::Io`] if the segment file can't be opened or written to,
    /// or [`LogError::SegmentOverflow`] if this machine has already filled
    /// segment 9999.
    pub fn append(&mut self, events: &[Event]) -> Result<(), LogError> {
        let mut batch = String::new();
        for e in events {
            let line = serde_json::to_string(e)?;
            batch.push_str(&line);
            batch.push('\n');
        }
        let seg = self.active_segment(batch.len() as u64)?;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&seg)
            .map_err(LogError::io(&seg))?;
        f.write_all(batch.as_bytes()).map_err(LogError::io(&seg))?;
        Ok(())
    }

    /// The segment this append should write: the highest-numbered existing
    /// `NNNN.jsonl` unless this batch would push it past [`ROTATE_BYTES`],
    /// in which case the next number starts fresh. A brand-new machine dir
    /// starts at `0001.jsonl`. Non-numeric `.jsonl` names (a sync tool's
    /// "conflicted copy") never become the active tip, though readers
    /// still read them.
    fn active_segment(&self, batch_len: u64) -> Result<PathBuf, LogError> {
        let segments = Self::list_segments(&self.machine_dir)?;
        let current = segments
            .iter()
            .filter_map(|(name, path)| {
                let n: u32 = name.strip_suffix(".jsonl")?.parse().ok()?;
                Some((n, path.clone()))
            })
            .next_back();
        let Some((num, path)) = current else {
            return Ok(self.machine_dir.join("0001.jsonl"));
        };
        let len = fs::metadata(&path).map_or(0, |m| m.len());
        if len == 0 || len + batch_len <= ROTATE_BYTES {
            return Ok(path);
        }
        let next = num + 1;
        if next > MAX_SEGMENT {
            return Err(LogError::SegmentOverflow {
                machine: self
                    .machine_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            });
        }
        Ok(self.machine_dir.join(format!("{next:04}.jsonl")))
    }

    /// # Errors
    /// Returns [`LogError::Io`] if the events directory or a machine's
    /// segments can't be read.
    pub fn read_all(&self) -> Result<Vec<Event>, LogError> {
        self.read_all_reporting(|_| {})
    }

    /// Corrupt lines are skipped and reported, never fatal: one bad byte
    /// on a shuttle drive must not take down the whole catalog. Shares the
    /// segment walk with [`Self::read_since_reporting`] (this is that read
    /// with empty cursors, cursors discarded), so the two paths can no
    /// longer diverge in walk order or UTF-8 handling. See
    /// [`Self::read_segment_since`] for the torn-tail rule.
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
        on_bad_line: impl FnMut(&str),
    ) -> Result<Vec<Event>, LogError> {
        let (events, _cursors) = self.read_since_reporting(&[], on_bad_line)?;
        Ok(events)
    }

    /// `.jsonl` segments directly under `machine_dir`, as (file name, path)
    /// pairs sorted lexicographically — same ordering constraint as
    /// [`Self::read_since_reporting`]: segment names must stay zero-padded
    /// and equal-width for lexicographic order to also be numeric order.
    fn list_segments(machine_dir: &Path) -> Result<Vec<(String, PathBuf)>, LogError> {
        let entries = fs::read_dir(machine_dir).map_err(LogError::io(machine_dir))?;
        let mut segments = Vec::new();
        for entry in entries {
            let entry = entry.map_err(LogError::io(machine_dir))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(LogError::io(&path))?;
            if file_type.is_file() && path.extension().is_some_and(|x| x == "jsonl") {
                segments.push((entry.file_name().to_string_lossy().into_owned(), path));
            }
        }
        segments.sort();
        Ok(segments)
    }

    /// Reads one segment from byte offset `from` to its last complete line,
    /// reporting parse failures through `on_bad_line`. Returns the parsed
    /// events plus the new offset (`from` plus whole bytes consumed).
    ///
    /// Torn-tail rule (the normative statement — [`Self::append`],
    /// [`Self::read_all_reporting`], and [`Self::read_since_reporting`] link
    /// here rather than restating it): any bytes after the last `\n` are
    /// left unconsumed instead of parsed or reported. If the write that
    /// produced them later completes, the next read picks the line up
    /// normally. But nothing distinguishes a write still in progress from
    /// one that never will finish — an interrupted copy, a shuttle drive
    /// pulled mid-write. A permanently truncated tail is therefore deferred
    /// indefinitely and invisible to both readers: no error, no
    /// `on_bad_line` call, no diagnostic at all.
    fn read_segment_since(
        seg: &Path,
        from: u64,
        mut on_bad_line: impl FnMut(&str),
    ) -> Result<(Vec<Event>, u64), LogError> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut f = fs::File::open(seg).map_err(LogError::io(seg))?;
        f.seek(SeekFrom::Start(from)).map_err(LogError::io(seg))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(LogError::io(seg))?;
        let consumed = buf.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let mut events = Vec::new();
        for line in buf[..consumed].split(|&b| b == b'\n') {
            match std::str::from_utf8(line) {
                Ok(text) if text.trim().is_empty() => {}
                Ok(text) => match serde_json::from_str::<Event>(text) {
                    Ok(event) => events.push(event),
                    Err(_) => on_bad_line(text),
                },
                Err(_) => on_bad_line(&String::from_utf8_lossy(line)),
            }
        }
        let new_offset = from.saturating_add(u64::try_from(consumed).unwrap_or(u64::MAX));
        Ok((events, new_offset))
    }

    /// Reads only events past `cursors`, returning the new events plus
    /// updated cursors covering every segment seen (unknown segments read
    /// from 0 and gain a cursor of their own).
    ///
    /// This is the segment walk — [`Self::read_all_reporting`] just calls
    /// this with empty cursors and discards the returned cursors. Each
    /// segment is sought to its cursor offset instead of read from the
    /// start. See [`Self::read_segment_since`] for the torn-tail rule that
    /// keeps the cursor from advancing past an incomplete line.
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
        let machines = fs::read_dir(&events_dir).map_err(LogError::io(&events_dir))?;
        for machine in machines {
            let machine = machine.map_err(LogError::io(&events_dir))?;
            let machine_path = machine.path();
            let is_dir = machine.file_type().map_err(LogError::io(&machine_path))?;
            if !is_dir.is_dir() {
                continue;
            }
            let machine_name = machine.file_name().to_string_lossy().into_owned();
            for (segment_name, seg) in Self::list_segments(&machine_path)? {
                let from = start
                    .remove(&(machine_name.clone(), segment_name.clone()))
                    .unwrap_or(0);
                let len = fs::metadata(&seg).map_err(LogError::io(&seg))?.len();
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
        assert!(matches!(
            log.read_since_reporting(&[stale], |_| {}),
            Err(LogError::StaleCursor { .. })
        ));
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
        assert!(matches!(
            log.read_since_reporting(&[stale], |_| {}),
            Err(LogError::StaleCursor { .. })
        ));
    }

    #[test]
    fn read_since_reports_a_complete_corrupt_line_and_still_advances_past_it() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append");
        let seg = dir.path().join("events/m1/0001.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&seg)
            .expect("open");
        f.write_all(b"not json\n").expect("write corrupt line");
        let mut bad_lines = Vec::new();
        let (events, cursors) = log
            .read_since_reporting(&[], |line| bad_lines.push(line.to_string()))
            .expect("read");
        assert_eq!(events.len(), 1, "the valid event still parses");
        assert_eq!(bad_lines, vec!["not json"], "callback fires exactly once");
        let len = std::fs::metadata(&seg).expect("meta").len();
        assert_eq!(
            cursors[0].offset, len,
            "cursor advances past the valid event and the corrupt line"
        );
    }

    #[test]
    fn read_since_reports_non_utf8_bytes_as_a_bad_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let log = FileEventLog::init(dir.path(), &m).expect("init");
        let seg = dir.path().join("events/m1/0001.jsonl");
        std::fs::write(&seg, [0xFF, 0xFE, b'\n']).expect("write non-utf8 line");
        let mut bad = 0;
        let (events, cursors) = log.read_since_reporting(&[], |_| bad += 1).expect("read");
        assert_eq!(events.len(), 0);
        assert_eq!(
            bad, 1,
            "the non-utf8 line is reported through the lossy branch"
        );
        assert_eq!(
            cursors[0].offset, 3,
            "cursor advances past the complete line"
        );
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

    #[test]
    fn read_all_reports_non_utf8_line_and_keeps_reading() {
        // Invariant: one bad byte must degrade a single line, never the
        // whole segment.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(dir.path(), &MachineId("m1".into())).expect("init");
        log.append(&[ev(1)]).expect("append");
        let seg = dir.path().join("events/m1/0001.jsonl");
        let mut bytes = std::fs::read(&seg).expect("read seg");
        bytes.extend_from_slice(&[0xFF, 0xFE, b'\n']);
        std::fs::write(&seg, bytes).expect("write");
        let mut bad = 0;
        let all = log
            .read_all_reporting(|_| bad += 1)
            .expect("read must not fail");
        assert_eq!((all.len(), bad), (1, 1));
    }

    #[test]
    fn append_rotates_past_the_size_threshold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append");
        // Grow 0001.jsonl past the threshold without writing 4MiB of real
        // events. set_len pads with NUL bytes and no trailing newline, which
        // reads back as a torn tail (deferred, not reported) rather than a
        // complete bad line; write a trailing newline so the padded region
        // terminates and reads as exactly one reported bad line instead.
        let seg = dir.path().join("events/m1/0001.jsonl");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&seg)
            .expect("open");
        f.set_len(ROTATE_BYTES + 1).expect("grow");
        f.write_all(b"\n").expect("terminate the padded region");
        log.append(&[ev(2)]).expect("append after threshold");
        assert!(
            dir.path().join("events/m1/0002.jsonl").is_file(),
            "append past the threshold must start 0002.jsonl"
        );
        let mut bad = 0;
        let all = log.read_all_reporting(|_| bad += 1).expect("read");
        assert_eq!(
            (all.len(), bad),
            (2, 1),
            "events merge across both segments; the NUL padding reads as one reported bad line"
        );
    }

    #[test]
    fn segment_overflow_is_a_hard_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        let seg = dir.path().join("events/m1/9999.jsonl");
        let f = std::fs::File::create(&seg).expect("create");
        f.set_len(ROTATE_BYTES + 1).expect("grow");
        assert!(matches!(
            log.append(&[ev(1)]),
            Err(LogError::SegmentOverflow { .. })
        ));
    }

    #[test]
    fn cursors_are_sorted_across_machines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut log = FileEventLog::init(dir.path(), &MachineId("zebra".into())).expect("init");
        log.append(&[ev(1)]).expect("append");
        for name in ["alpha", "m9", "charlie", "beta", "omega", "m1"] {
            let mut other =
                FileEventLog::open(dir.path(), &MachineId(name.into())).expect("open other");
            other.append(&[ev(2)]).expect("append other");
        }
        let (_, cursors) = log.read_since_reporting(&[], |_| {}).expect("read");
        let names: Vec<&str> = cursors.iter().map(|c| c.machine.as_str()).collect();
        let mut expected = names.clone();
        expected.sort_unstable();
        assert_eq!(names, expected, "cursors must be in canonical sorted order");
    }
}
