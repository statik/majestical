//! Engine acceptance: real files in temp dirs, fault injection via `SinkFactory`.
use majestical_ingest::engine::{DestSpec, EngineConfig, RealSinks, Sink, SinkFactory, run};
use majestical_ingest::journal::Journal;
use majestical_ingest::plan::{DedupeMode, KnownAssets, plan_source};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

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
    let mut journal = Journal::create(&jpath).expect("journal");
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

/// Flips the first byte it writes for paths containing `target`, corrupting
/// the destination between write and read-back — exactly the failure
/// read-back verification exists to catch.
struct CorruptingSinks {
    target: String,
}

struct CorruptingSink {
    inner: Box<dyn Sink>,
    corrupt: bool,
    done: bool,
}

impl Write for CorruptingSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.corrupt && !self.done && !buf.is_empty() {
            self.done = true;
            let mut flipped = buf.to_vec();
            flipped[0] ^= 0xFF;
            return self.inner.write(&flipped);
        }
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Sink for CorruptingSink {
    fn finish(&mut self) -> std::io::Result<()> {
        self.inner.finish()
    }
}

impl SinkFactory for CorruptingSinks {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn Sink>> {
        let corrupt = path.to_string_lossy().contains(&self.target);
        Ok(Box::new(CorruptingSink {
            inner: RealSinks.open(path)?,
            corrupt,
            done: false,
        }))
    }
}

#[test]
fn corrupted_destination_fails_verification_and_stays_quarantined() {
    let (src, d1, d2) = setup(&[("a.mov", b"AAAA")]);
    let plan = plan_source(src.path(), &KnownAssets::default(), DedupeMode::Skip).expect("plan");
    let mut journal = Journal::create(&d1.path().join("run.jsonl")).expect("journal");
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
    let mut journal = Journal::create(&jpath).expect("journal");
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
    let mut journal = Journal::create(&jpath).expect("reopen appends");
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

#[test]
fn duplicate_skip_does_not_copy() {
    let (src, d1, d2) = setup(&[("dup.mov", b"AAAA")]);
    let known = KnownAssets::from_pairs(vec![(
        format!("{:032x}", xxhash_rust::xxh3::xxh3_128(b"AAAA")),
        4,
    )]);
    let plan = plan_source(src.path(), &known, DedupeMode::Skip).expect("plan");
    let mut journal = Journal::create(&d1.path().join("run.jsonl")).expect("journal");
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
