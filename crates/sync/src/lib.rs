//! File-based event log: `events/<machine-id>/NNNN.jsonl` under a sync
//! root. Append-only; reading merges every machine's segments. Designed
//! so dumb transports (Dropbox, rsync, a shuttle drive) can carry it.
use majestical_core::clock::MachineId;
use majestical_core::event::Event;
use majestical_core::ports::{EventLog, LogCursor, PortError};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub mod transfer;

/// Rotation threshold: an append that would grow the active segment past
/// this starts the next `NNNN.jsonl` instead, bounding the whole-file
/// re-copy cost of `maj sync push` (segments transfer longer-wins as whole
/// files). Rotated segments are immutable thereafter, for as long as the
/// higher-numbered segment that superseded them still exists; if it is
/// deleted, the previous segment becomes the tip again and grows.
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
        "machine {machine} reached segment {MAX_SEGMENT} — the log segment namespace is exhausted; this catalog needs a new machine id"
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

/// Single writer per machine directory: two processes appending under the
/// same machine id can interleave their batches mid-line, corrupting both.
/// Multiple machine ids under one catalog root are fine — each gets its own
/// directory and its own writer.
pub struct FileEventLog {
    root: PathBuf,
    machine_dir: PathBuf,
    machine: String,
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
            machine: machine.0.clone(),
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
            machine: machine.0.clone(),
        })
    }

    /// Append to this machine's active segment. Batches that would grow the
    /// active segment past [`ROTATE_BYTES`] start the next `NNNN.jsonl`
    /// instead — except when the active segment is still empty, in which
    /// case the batch lands there regardless of size: an over-threshold
    /// batch has to land somewhere, and an empty segment can't be made to
    /// hold it by rotating again. A fresh machine directory starts at
    /// `0001.jsonl`. Rotated segments are never appended to again while the
    /// higher-numbered segment that superseded them still exists; if it is
    /// deleted, the previous segment becomes the tip again and grows.
    ///
    /// No fsync: a crash mid-write may drop the tail of the batch. See
    /// [`Self::read_segment_since`] for how readers handle the resulting
    /// torn tail.
    ///
    /// # Errors
    /// Returns [`LogError::Serde`] if an event can't be serialized,
    /// [`LogError::Io`] if the segment file can't be opened, written to, or
    /// stat'd, or [`LogError::SegmentOverflow`] if this machine has already
    /// filled segment `MAX_SEGMENT`.
    pub fn append(&mut self, events: &[Event]) -> Result<(), LogError> {
        if events.is_empty() {
            return Ok(());
        }
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
    /// in which case the next number starts fresh — unless that
    /// highest-numbered segment is still empty, in which case the batch
    /// lands there regardless of size. A brand-new machine dir starts at
    /// `0001.jsonl`. Non-numeric or non-width-4 `.jsonl` names (a sync
    /// tool's "conflicted copy", or a stray `9.jsonl`/`10000.jsonl`) never
    /// become the active tip, though readers still read them.
    ///
    /// The `len == 0` exception above is also a crash-recovery guarantee:
    /// if a rotation's new segment file got created but the crash happened
    /// before any bytes landed in it, the next append sees it as the
    /// existing tip at length 0 and reuses it rather than rotating past a
    /// segment that was never written to. Do not remove this guard.
    fn active_segment(&self, batch_len: u64) -> Result<PathBuf, LogError> {
        let segments = Self::list_segments(&self.machine_dir)?;
        let current = segments
            .iter()
            .filter_map(|(name, path)| {
                let stem = name.strip_suffix(".jsonl")?;
                if stem.len() != 4 || !stem.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                let n: u32 = stem.parse().ok()?;
                Some((n, path.clone()))
            })
            .next_back();
        let Some((num, path)) = current else {
            return Ok(self.machine_dir.join("0001.jsonl"));
        };
        let len = fs::metadata(&path).map_err(LogError::io(&path))?.len();
        if len == 0 || len.saturating_add(batch_len) <= ROTATE_BYTES {
            return Ok(path);
        }
        let next = num + 1;
        if next > MAX_SEGMENT {
            return Err(LogError::SegmentOverflow {
                machine: self.machine.clone(),
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
    pub(crate) fn read_segment_since(
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
        ev_with_tag(n, "t".into())
    }

    fn ev_with_tag(n: u64, tag: String) -> Event {
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
                tag,
            },
        }
    }

    #[test]
    fn rotate_bytes_is_four_mebibytes() {
        // Every other test only ever uses ROTATE_BYTES symbolically
        // (`ROTATE_BYTES + 1`, `ROTATE_BYTES - 1`, …), so an arithmetic slip
        // in the constant's own definition (`4 * 1024 * 1024` mutated to
        // `4 * 1024 + 1024`, etc.) would be self-consistent with every one
        // of them. Pin the actual value.
        assert_eq!(ROTATE_BYTES, 4_194_304);
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
        match log.append(&[ev(1)]) {
            Err(LogError::SegmentOverflow { machine }) => assert_eq!(machine, "m1"),
            other => panic!("expected SegmentOverflow, got {other:?}"),
        }
    }

    #[test]
    fn an_oversized_batch_still_lands_in_the_empty_active_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        // Pre-create 0001.jsonl empty (len 0) so the `len == 0 ||` guard is
        // the only thing keeping a batch that's already over ROTATE_BYTES
        // from rotating: without the guard, len(0) + batch_len > ROTATE_BYTES
        // would push straight past it and start 0002.jsonl, then 0003.jsonl,
        // forever, since no single segment can ever hold the batch.
        let seg = dir.path().join("events/m1/0001.jsonl");
        std::fs::File::create(&seg).expect("pre-create empty 0001.jsonl");
        let huge_tag = "x".repeat(usize::try_from(ROTATE_BYTES).expect("fits usize") + 1);
        log.append(&[ev_with_tag(1, huge_tag)])
            .expect("append oversized batch");
        assert!(
            !dir.path().join("events/m1/0002.jsonl").exists(),
            "an oversized batch must land in the empty active segment, not rotate forever"
        );
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 1, "the oversized event must still be readable");
    }

    #[test]
    fn non_conforming_names_never_become_the_active_tip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        log.append(&[ev(1)]).expect("append segment 1");
        let seg2 = dir.path().join("events/m1/0002.jsonl");
        let line = serde_json::to_string(&ev(2)).expect("serialize");
        std::fs::write(&seg2, format!("{line}\n")).expect("write segment 2");
        let seg2_len_before = std::fs::metadata(&seg2).expect("meta").len();
        // "9.jsonl" sorts after "0002.jsonl" lexicographically, and
        // "10000.jsonl" both lexicographically and numerically — either
        // would wrongly become the tip without the width-4 filter.
        std::fs::write(dir.path().join("events/m1/9.jsonl"), b"").expect("write 9.jsonl");
        std::fs::write(dir.path().join("events/m1/10000.jsonl"), b"").expect("write 10000.jsonl");
        log.append(&[ev(3)]).expect("append after strays");
        assert!(
            !dir.path().join("events/m1/0003.jsonl").exists(),
            "the highest-numbered CONFORMING segment is 0002.jsonl; append must not rotate past a stray"
        );
        let seg2_len_after = std::fs::metadata(&seg2).expect("meta").len();
        assert!(
            seg2_len_after > seg2_len_before,
            "the new event must land in 0002.jsonl, not a stray"
        );
        let all = log.read_all().expect("read");
        assert_eq!(all.len(), 3, "all three real events must still be readable");
    }

    #[test]
    fn segment_9999_is_legal_only_10000_would_overflow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = MachineId("m1".into());
        let mut log = FileEventLog::init(dir.path(), &m).expect("init");
        // 9998, not 9999: distinguishes `next > MAX_SEGMENT` from
        // `next >= MAX_SEGMENT` — next here is 9999, which must still
        // succeed since only 10000 is out of the namespace.
        let seg = dir.path().join("events/m1/9998.jsonl");
        let f = std::fs::File::create(&seg).expect("create");
        f.set_len(ROTATE_BYTES + 1).expect("grow past threshold");
        log.append(&[ev(1)])
            .expect("rolling from 9998 to 9999 must succeed");
        assert!(
            dir.path().join("events/m1/9999.jsonl").is_file(),
            "segment 9999 is legal; only 10000 overflows"
        );
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
