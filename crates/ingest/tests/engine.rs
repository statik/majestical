//! Engine acceptance: real files in temp dirs, fault injection via `SinkFactory`.
mod common;

use common::{CorruptingSinks, silent_control};
use majestical_ingest::engine::{
    CancelFlag, DestSpec, EngineConfig, ProgressEvent, RealSinks, RunControl, Sink, SinkFactory,
    run,
};
use majestical_ingest::journal::Journal;
use majestical_ingest::plan::{DedupeMode, KnownAssets, plan_source};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

// These helpers need `#[cfg(test)]` even though this whole file only ever
// compiles as a test binary: clippy's expect/unwrap-in-tests exemption
// (clippy.toml's `allow-expect-in-tests`) keys off the literal attribute on
// each item, not the ambient `cfg(test)` the compiler already applies here
// — confirmed by removing them and observing `expect_used` reappear.
#[cfg(test)]
fn write(dir: &Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
    std::fs::write(p, bytes).expect("write");
}

#[cfg(test)]
fn setup(files: &[(&str, &[u8])]) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let src = tempfile::tempdir().expect("src");
    for (rel, bytes) in files {
        write(src.path(), rel, bytes);
    }
    (
        src,
        tempfile::tempdir().expect("d1"),
        tempfile::tempdir().expect("d2"),
    )
}

#[cfg(test)]
fn dests(d1: &Path, d2: &Path) -> Vec<DestSpec> {
    vec![
        DestSpec {
            root: d1.to_path_buf(),
            subdir: "Projects/x/day1".into(),
        },
        DestSpec {
            root: d2.to_path_buf(),
            subdir: "Projects/x/day1".into(),
        },
    ]
}

/// No prior journal: empty resume set.
#[cfg(test)]
fn fresh() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn copies_verifies_and_places_to_every_destination() {
    let (src, d1, d2) = setup(&[("clips/a.mov", b"AAAA"), ("b.wav", b"BBBBBB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::open_append(&jpath).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 2 },
        &silent_control(),
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 2);
    assert!(outcome.failed.is_empty());
    for d in [d1.path(), d2.path()] {
        assert_eq!(
            std::fs::read(d.join("Projects/x/day1/clips/a.mov")).expect("placed"),
            b"AAAA"
        );
        assert_eq!(
            std::fs::read(d.join("Projects/x/day1/b.wav")).expect("placed"),
            b"BBBBBB"
        );
    }
    let placed_a = outcome
        .placed
        .iter()
        .find(|p| p.rel == "clips/a.mov")
        .expect("a placed");
    // The accumulator in `stream_to_sinks` starts at 0 and only ever grows
    // by `+=`; asserting the real byte count (not just that hashes match)
    // catches a `+=` -> `*=` mutation, which would leave `size` stuck at 0.
    assert_eq!(placed_a.size, 4);
    assert_eq!(
        placed_a.xxh64,
        format!("{:016x}", xxhash_rust::xxh64::xxh64(b"AAAA", 0))
    );
    assert_eq!(
        placed_a.xxh3,
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"AAAA"))
    );
}

#[test]
fn corrupted_destination_fails_verification_and_stays_quarantined() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &CorruptingSinks {
            target: d1.path().to_string_lossy().into_owned(),
        },
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("run");
    assert_eq!(outcome.failed.len(), 1, "corrupted dest fails the file");
    assert!(
        !d1.path().join("Projects/x/day1/a.mov").exists(),
        "corrupt copy must never be renamed into place"
    );
    let quarantined: Vec<_> = walkdir::WalkDir::new(d1.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".maj-partial-"))
        .collect();
    assert_eq!(quarantined.len(), 1, "partial stays under its temp name");
    // The healthy destination is independent: it still gets its verified copy.
    assert!(d2.path().join("Projects/x/day1/a.mov").exists());
    assert_eq!(
        std::fs::read(d2.path().join("Projects/x/day1/a.mov")).expect("read"),
        b"AAAA"
    );
}

#[test]
fn resume_skips_placed_files() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::open_append(&jpath).expect("journal");
    run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("first run");
    let folded = Journal::load(&jpath).expect("fold");
    assert_eq!(folded.placed.len(), 2);
    let mut journal = Journal::open_append(&jpath).expect("reopen appends");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &folded.placed,
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("resume");
    assert!(outcome.placed.is_empty(), "everything already placed");
    assert_eq!(outcome.skipped_resumed, 2);
}

/// Deletes a "victim" file's already-placed final path in the same
/// directory the moment a temp sink is opened for a "trigger" filename.
/// Used to simulate a file vanishing from a destination between its own
/// placement and the run's end-of-run sweep — a hazard nothing but that
/// sweep can catch.
struct DeletingSinks {
    trigger_filename: &'static str,
    victim_filename: &'static str,
}

impl SinkFactory for DeletingSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        if path.to_string_lossy().contains(self.trigger_filename)
            && let Some(parent) = path.parent()
        {
            let _ = std::fs::remove_file(parent.join(self.victim_filename));
        }
        RealSinks.open(path)
    }
}

#[test]
fn sweep_missing_demotes_a_placed_file_that_vanished_before_the_sweep() {
    // jobs: 1 and walkdir's sorted order guarantee a.mov is fully copied,
    // verified, and placed at both destinations before b.mov's sinks ever
    // open — so the deletion below always hits a real, already-placed file.
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::open_append(&jpath).expect("journal");
    let sinks = DeletingSinks {
        trigger_filename: "b.mov",
        victim_filename: "a.mov",
    };
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &sinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 1, "only b survives the sweep");
    assert_eq!(outcome.placed[0].rel, "b.mov");
    assert_eq!(outcome.failed.len(), 1, "a is demoted by the sweep");
    assert_eq!(outcome.failed[0].rel, "a.mov");
    assert!(
        outcome.failed[0].reason.contains("end-of-run sweep"),
        "reason should name the sweep: {}",
        outcome.failed[0].reason
    );

    // The journal must agree with the Outcome: without this, a resumed run
    // would still see the stale FilePlaced record for "a.mov" and skip
    // re-copying a file that the sweep just proved is actually gone.
    let folded = Journal::load(&jpath).expect("load journal");
    assert!(
        folded.failed.contains_key("a.mov"),
        "journal must record the sweep demotion so resume re-copies it"
    );
    // Recording the FileFailed line is not enough on its own: resume reads
    // `placed`, not `failed`, so the demotion only matters if it also
    // removes "a.mov" from `placed`. An insert-only fold would leave it in
    // both sets, and resume would still (wrongly) skip it.
    assert!(
        !folded.placed.contains("a.mov"),
        "the sweep's FileFailed must remove a.mov from placed, not just add it to failed"
    );
}

#[test]
fn duplicate_skip_does_not_copy() {
    let (src, d1, d2) = setup(&[("dup.mov", b"AAAA")]);
    let known = KnownAssets::from_pairs(vec![(
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"AAAA")),
        4,
    )]);
    let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("run");
    assert!(outcome.placed.is_empty());
    assert_eq!(outcome.skipped_duplicates.len(), 1);
    assert!(!d1.path().join("Projects/x/day1/dup.mov").exists());
}

/// Every other test in this file seeds `KnownAssets::default()` or a
/// candidate whose content genuinely matches, so `pf.prehash` is always
/// `None` for a `Copy` decision — the `Some(prehash)` branch in `copy_one`
/// that re-checks the source hasn't changed since planning never runs.
/// Seeding a same-size candidate with a *different* hash forces the planner
/// to hash the source during planning (`Decision::Copy` with `prehash:
/// Some(..)`) without it being a duplicate, exercising that branch's normal
/// (non-corrupted) path: the prehash matches the copy-time hash, so the
/// file must still be placed, not failed. This discriminates a `!=` -> `==`
/// mutation in that check, which would instead fail every such file.
#[test]
fn a_correctly_predicted_prehash_does_not_block_the_copy() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA")]);
    let known = KnownAssets::from_pairs(vec![(
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"ZZZZ")),
        4,
    )]);
    let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
    assert!(
        plan.files[0].prehash.is_some(),
        "the size-prefilter match must have hashed the source during planning"
    );
    assert!(matches!(
        plan.files[0].decision,
        majestical_ingest::plan::Decision::Copy
    ));
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("run");
    assert_eq!(
        outcome.placed.len(),
        1,
        "a prehash that matches the actual bytes must not block the copy: {:?}",
        outcome.failed
    );
    assert!(outcome.failed.is_empty());
}

/// One file's slice of a run's event stream. Workers race globally, so a
/// run's events only interleave *between* files — everything asserted here
/// is filtered down to a single `rel` first.
#[cfg(test)]
#[derive(Default)]
struct FileTrace {
    /// `FileStarted` sizes: exactly one per attempted file.
    started: Vec<u64>,
    /// Cumulative source bytes carried by each `BytesCopied`.
    bytes: Vec<u64>,
    /// Destination roots that passed read-back verification.
    verified: Vec<String>,
    placed: usize,
    /// `FileFailed` reasons.
    failed: Vec<String>,
    /// Each event's variant, in emission order, for the ordering assertions.
    order: Vec<&'static str>,
}

#[cfg(test)]
fn trace(events: &[ProgressEvent], want: &str) -> FileTrace {
    let mut found = FileTrace::default();
    for event in events {
        match event {
            ProgressEvent::RunStarted { .. } | ProgressEvent::RunStopped { .. } => {}
            ProgressEvent::FileStarted { rel, size } => {
                if rel == want {
                    found.started.push(*size);
                    found.order.push("started");
                }
            }
            ProgressEvent::BytesCopied { rel, bytes_done } => {
                if rel == want {
                    found.bytes.push(*bytes_done);
                    found.order.push("bytes");
                }
            }
            ProgressEvent::FileVerified { rel, dest_root } => {
                if rel == want {
                    found.verified.push(dest_root.clone());
                    found.order.push("verified");
                }
            }
            ProgressEvent::FilePlaced { rel } => {
                if rel == want {
                    found.placed += 1;
                    found.order.push("placed");
                }
            }
            ProgressEvent::FileFailed { rel, reason } => {
                if rel == want {
                    found.failed.push(reason.clone());
                    found.order.push("failed");
                }
            }
        }
    }
    found
}

/// Asserts the per-file ordering guarantee for a file that was placed:
/// `FileStarted` first, every `BytesCopied` before every `FileVerified`,
/// `FilePlaced` last, and the byte counts cumulative up to `size`.
#[cfg(test)]
fn assert_placed_trace(found: &FileTrace, size: u64, roots: &[String]) {
    assert_eq!(found.started, vec![size], "one FileStarted with the size");
    assert!(!found.bytes.is_empty(), "at least one BytesCopied");
    assert!(
        found.bytes.windows(2).all(|w| w[0] < w[1]),
        "BytesCopied must be cumulative and strictly increasing: {:?}",
        found.bytes
    );
    assert_eq!(
        found.bytes.last().copied(),
        Some(size),
        "the last BytesCopied is the whole file, counted once — not once \
         per destination: {:?}",
        found.bytes
    );
    let mut verified = found.verified.clone();
    verified.sort();
    let mut want_roots = roots.to_vec();
    want_roots.sort();
    assert_eq!(verified, want_roots, "one FileVerified per destination");
    assert_eq!(found.placed, 1);
    assert!(found.failed.is_empty(), "{:?}", found.failed);
    assert_eq!(found.order.first(), Some(&"started"));
    assert_eq!(found.order.last(), Some(&"placed"));
    let last_bytes = found
        .order
        .iter()
        .rposition(|label| *label == "bytes")
        .expect("a bytes event");
    let first_verified = found
        .order
        .iter()
        .position(|label| *label == "verified")
        .expect("a verified event");
    assert!(
        last_bytes < first_verified,
        "every BytesCopied precedes every FileVerified: {:?}",
        found.order
    );
}

#[cfg(test)]
fn root_names(d1: &Path, d2: &Path) -> Vec<String> {
    vec![
        d1.to_string_lossy().into_owned(),
        d2.to_string_lossy().into_owned(),
    ]
}

#[test]
fn progress_events_bracket_the_run_and_narrate_every_copied_file() {
    // `already.mov` is resumed and `empty.mov` is rejected by planning, so
    // neither may show up in the RunStarted totals: 3 files, 4+6+2 bytes.
    let (src, d1, d2) = setup(&[
        ("clips/a.mov", b"AAAA"),
        ("b.wav", b"BBBBBB"),
        ("c.mov", b"CC"),
        ("already.mov", b"DDDDDDDD"),
        ("empty.mov", b""),
    ]);
    // Bigger than the engine's 1 MiB copy buffer, so this one file spans
    // three chunks and its BytesCopied really do have to accumulate.
    let big = vec![7u8; 2_500_000];
    write(src.path(), "big.mov", &big);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut resume = BTreeSet::new();
    resume.insert("already.mov".to_string());
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let events = Mutex::new(Vec::new());
    let cancel = CancelFlag::new(false);
    let progress = |event: ProgressEvent| events.lock().expect("events lock").push(event);
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &resume,
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 2 },
        &RunControl {
            progress: &progress,
            cancel: &cancel,
        },
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 4);
    assert_eq!(outcome.skipped_resumed, 1);
    assert_eq!(outcome.rejected.len(), 1);

    let events = events.into_inner().expect("events");
    let started = ProgressEvent::RunStarted {
        files_total: 4,
        bytes_total: 12 + 2_500_000,
    };
    let stopped = ProgressEvent::RunStopped { cancelled: false };
    assert_eq!(
        events.first(),
        Some(&started),
        "totals cover queued work only"
    );
    assert_eq!(events.iter().filter(|e| **e == started).count(), 1);
    assert_eq!(events.last(), Some(&stopped));
    assert_eq!(events.iter().filter(|e| **e == stopped).count(), 1);

    let roots = root_names(d1.path(), d2.path());
    for (rel, size) in [
        ("clips/a.mov", 4u64),
        ("b.wav", 6),
        ("c.mov", 2),
        ("big.mov", 2_500_000),
    ] {
        assert_placed_trace(&trace(&events, rel), size, &roots);
    }
    // Cumulative source bytes, one buffer at a time — not per destination,
    // which with two destinations would double every number here.
    assert_eq!(
        trace(&events, "big.mov").bytes,
        vec![1_048_576, 2_097_152, 2_500_000]
    );
}

/// Fails to open any temp sink whose name contains `filename`, so exactly
/// one file of a multi-file run fails while the rest place normally.
struct FailingOpenSinks {
    filename: &'static str,
}

impl SinkFactory for FailingOpenSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        if path.to_string_lossy().contains(self.filename) {
            return Err(std::io::Error::other("injected open failure"));
        }
        RealSinks.open(path)
    }
}

#[test]
fn a_failing_file_emits_file_failed_without_disturbing_the_others() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB"), ("c.mov", b"CCC")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let events = Mutex::new(Vec::new());
    let cancel = CancelFlag::new(false);
    let progress = |event: ProgressEvent| events.lock().expect("events lock").push(event);
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &FailingOpenSinks { filename: "b.mov" },
        &EngineConfig { jobs: 2 },
        &RunControl {
            progress: &progress,
            cancel: &cancel,
        },
    )
    .expect("run");
    assert_eq!(outcome.failed.len(), 1);
    assert_eq!(outcome.failed[0].rel, "b.mov");

    let events = events.into_inner().expect("events");
    let failed = trace(&events, "b.mov");
    assert_eq!(failed.placed, 0);
    assert_eq!(failed.failed.len(), 1);
    assert!(
        failed.failed[0].contains("injected open failure"),
        "the event carries the real reason: {}",
        failed.failed[0]
    );
    assert_eq!(failed.order.first(), Some(&"started"));
    assert_eq!(failed.order.last(), Some(&"failed"));

    let roots = root_names(d1.path(), d2.path());
    assert_placed_trace(&trace(&events, "a.mov"), 4, &roots);
    assert_placed_trace(&trace(&events, "c.mov"), 3, &roots);
    assert_eq!(
        events
            .iter()
            .filter(|e| **e == ProgressEvent::RunStopped { cancelled: false })
            .count(),
        1,
        "a per-file failure is not a cancellation"
    );
}

#[test]
fn cancelling_mid_run_stops_between_files_and_leaves_the_rest_resumable() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB"), ("c.mov", b"CCC")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let jpath = d1.path().join("run.jsonl");
    let mut journal = Journal::open_append(&jpath).expect("journal");
    let events = Mutex::new(Vec::new());
    let cancel = CancelFlag::new(false);
    // One worker, cancelled from inside the first FilePlaced emission: the
    // remaining two queue entries are never popped.
    let progress = |event: ProgressEvent| {
        if let ProgressEvent::FilePlaced { .. } = &event {
            cancel.store(true, Ordering::Relaxed);
        }
        events.lock().expect("events lock").push(event);
    };
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &RunControl {
            progress: &progress,
            cancel: &cancel,
        },
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 1, "the in-flight file finishes");
    assert!(outcome.failed.is_empty(), "cancelling is not failing");

    let events = events.into_inner().expect("events");
    assert_eq!(
        events.last(),
        Some(&ProgressEvent::RunStopped { cancelled: true })
    );
    assert_eq!(
        trace(&events, "b.mov").order,
        Vec::<&str>::new(),
        "a queue entry left unpopped emits nothing"
    );

    // The journal is consistent: a resumed run finishes exactly the two
    // files cancellation left behind.
    let folded = Journal::load(&jpath).expect("fold");
    assert_eq!(folded.placed.len(), 1);
    let mut journal = Journal::open_append(&jpath).expect("reopen appends");
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &folded.placed,
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &silent_control(),
    )
    .expect("resume");
    assert_eq!(outcome.placed.len(), 2);
    assert_eq!(outcome.skipped_resumed, 1);
    for d in [d1.path(), d2.path()] {
        for rel in ["a.mov", "b.mov", "c.mov"] {
            assert!(
                d.join("Projects/x/day1").join(rel).is_file(),
                "resume must complete {rel}"
            );
        }
    }
}

#[test]
fn a_flag_set_after_the_last_file_reports_an_uncancelled_stop() {
    // The flag is set, but the queue drained anyway: no work was left
    // undone, so the run did not stop *because* of the cancellation.
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let events = Mutex::new(Vec::new());
    let cancel = CancelFlag::new(false);
    let progress = |event: ProgressEvent| {
        if let ProgressEvent::FilePlaced { .. } = &event {
            cancel.store(true, Ordering::Relaxed);
        }
        events.lock().expect("events lock").push(event);
    };
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &RunControl {
            progress: &progress,
            cancel: &cancel,
        },
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 1);
    let events = events.into_inner().expect("events");
    assert_eq!(
        events.last(),
        Some(&ProgressEvent::RunStopped { cancelled: false })
    );
}

#[test]
fn the_last_queued_file_survives_a_cancel_raised_while_its_predecessor_ran() {
    // Pins *where* the flag is read: before a queue entry is taken, not
    // after. With two files and one worker, cancelling from the first
    // FilePlaced must leave the second still sitting in the queue — which
    // is what makes the stop a cancellation. Reading the flag after the pop
    // would consume that entry and discard it, reporting a drained queue
    // and an uncancelled stop while a planned file silently went nowhere.
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA"), ("b.mov", b"BB")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::open_append(&d1.path().join("run.jsonl")).expect("journal");
    let events = Mutex::new(Vec::new());
    let cancel = CancelFlag::new(false);
    let progress = |event: ProgressEvent| {
        if let ProgressEvent::FilePlaced { .. } = &event {
            cancel.store(true, Ordering::Relaxed);
        }
        events.lock().expect("events lock").push(event);
    };
    let outcome = run(
        &plan,
        &dests(d1.path(), d2.path()),
        &fresh(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
        &RunControl {
            progress: &progress,
            cancel: &cancel,
        },
    )
    .expect("run");
    assert_eq!(outcome.placed.len(), 1);
    let events = events.into_inner().expect("events");
    assert_eq!(
        events.last(),
        Some(&ProgressEvent::RunStopped { cancelled: true }),
        "one file of two is left queued, so the flag really did cut the run short"
    );
    assert!(
        !d2.path().join("Projects/x/day1/b.mov").exists(),
        "the queued file must not have been copied"
    );
}

#[test]
fn progress_events_round_trip_through_their_json_wire_form() {
    // The heads receive these as JSON, so the tag name and the snake_case
    // variant names are a contract, not an implementation detail.
    let events = [
        ProgressEvent::RunStarted {
            files_total: 3,
            bytes_total: 12,
        },
        ProgressEvent::FileStarted {
            rel: "clips/a.mov".to_string(),
            size: 4,
        },
        ProgressEvent::BytesCopied {
            rel: "clips/a.mov".to_string(),
            bytes_done: 4,
        },
        ProgressEvent::FileVerified {
            rel: "clips/a.mov".to_string(),
            dest_root: "/Volumes/one".to_string(),
        },
        ProgressEvent::FilePlaced {
            rel: "clips/a.mov".to_string(),
        },
        ProgressEvent::FileFailed {
            rel: "b.mov".to_string(),
            reason: "injected open failure".to_string(),
        },
        ProgressEvent::RunStopped { cancelled: true },
    ];
    for event in &events {
        let json = serde_json::to_string(event).expect("serialize");
        let back: ProgressEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, event, "{json}");
    }
    assert_eq!(
        serde_json::to_string(&events[3]).expect("serialize"),
        r#"{"type":"file_verified","rel":"clips/a.mov","dest_root":"/Volumes/one"}"#
    );
}
