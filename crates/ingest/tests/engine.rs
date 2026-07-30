//! Engine acceptance: real files in temp dirs, fault injection via `SinkFactory`.
mod common;

use common::CorruptingSinks;
use majestical_ingest::engine::{DestSpec, EngineConfig, RealSinks, Sink, SinkFactory, run};
use majestical_ingest::journal::Journal;
use majestical_ingest::plan::{DedupeMode, KnownAssets, plan_source};
use std::collections::BTreeSet;
use std::path::Path;

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
    )
    .expect("run");
    assert!(outcome.placed.is_empty());
    assert_eq!(outcome.skipped_duplicates.len(), 1);
    assert!(!d1.path().join("Projects/x/day1/dup.mov").exists());
}
