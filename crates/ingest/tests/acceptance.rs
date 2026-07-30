//! Ingest acceptance: real files in temp dirs, exercised at the hexagon
//! boundary (`plan_source`/`engine::run`/`mhl::write_generation`/
//! `mhl::read_generation` called directly — no CLI process).
//!
//! Steps return `Result` instead of asserting/panicking: this binary is a
//! `harness = false` integration test (see `crates/core/tests/acceptance.rs`
//! for the same pattern), so it is not compiled the way `#[test]` functions
//! are, and the workspace denies `panic`/`unwrap_used` outside test code.
mod common;

use common::CorruptingSinks;
use cucumber::gherkin::Step;
use cucumber::{World, given, then, when};
use majestical_ingest::engine::{self, DestSpec, EngineConfig, RealSinks};
use majestical_ingest::journal::Journal;
use majestical_ingest::mhl::{self, HashAction, HashList, MhlEntry};
use majestical_ingest::plan::{
    Decision, DedupeMode, IngestPlan, KnownAssets, PlannedFile, plan_source,
};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Every scenario in `ingest.feature` ingests to this same subdir, so the
/// resume scenario's "previous run" setup step (which has no subdir of its
/// own in the Gherkin text) hardcodes it too — the physical layout must
/// agree with the run that follows.
const INGEST_SUBDIR: &str = "Projects/x/day1";

#[derive(Debug, World)]
#[world(init = Self::new)]
struct IngestWorld {
    source: Option<tempfile::TempDir>,
    dests: Vec<tempfile::TempDir>,
    corrupt_dest_index: Option<usize>,
    known_pairs: Vec<(String, u64)>,
    subdir: String,
    outcome: Option<engine::Outcome>,
    generations: Vec<(PathBuf, mhl::WrittenGeneration)>,
}

impl IngestWorld {
    fn new() -> Self {
        Self {
            source: None,
            dests: Vec::new(),
            corrupt_dest_index: None,
            known_pairs: Vec::new(),
            subdir: String::new(),
            outcome: None,
            generations: Vec::new(),
        }
    }

    fn source_path(&self) -> Result<&std::path::Path, String> {
        self.source
            .as_ref()
            .map(tempfile::TempDir::path)
            .ok_or_else(|| "no source card given yet".to_string())
    }

    /// One journal shared by every engine run in a scenario — the resume
    /// scenario's "previous run" step and the "When" step must agree on
    /// where it lives so the second run's resume set reflects the first.
    fn journal_path(&self) -> Result<PathBuf, String> {
        let first = self
            .dests
            .first()
            .ok_or_else(|| "no destination roots given yet".to_string())?;
        Ok(first.path().join("run.jsonl"))
    }
}

fn build_dest_specs(dests: &[tempfile::TempDir], subdir: &str) -> Vec<DestSpec> {
    dests
        .iter()
        .map(|d| DestSpec {
            root: d.path().to_path_buf(),
            subdir: subdir.to_string(),
        })
        .collect()
}

#[given("a source card with files")]
fn source_card(world: &mut IngestWorld, step: &Step) -> Result<(), String> {
    let table = step.table.as_ref().ok_or("expected a data table")?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    for row in table.rows.iter().skip(1) {
        let rel = row.first().ok_or("table row missing a path column")?;
        let bytes = row.get(1).ok_or("table row missing a bytes column")?;
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, bytes.as_bytes()).map_err(|e| e.to_string())?;
    }
    world.source = Some(dir);
    Ok(())
}

#[given(expr = "{int} destination root(s)")]
fn destination_roots(world: &mut IngestWorld, count: usize) -> Result<(), String> {
    for _ in 0..count {
        world
            .dests
            .push(tempfile::tempdir().map_err(|e| e.to_string())?);
    }
    Ok(())
}

#[given(expr = "{int} destination roots where destination 1 corrupts writes")]
fn destination_roots_with_corruption(world: &mut IngestWorld, count: usize) -> Result<(), String> {
    for _ in 0..count {
        world
            .dests
            .push(tempfile::tempdir().map_err(|e| e.to_string())?);
    }
    world.corrupt_dest_index = Some(0);
    Ok(())
}

#[given(expr = "the catalog already knows content {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn known_content(world: &mut IngestWorld, content: String) {
    let hash = format!("{:032x}", xxhash_rust::xxh3::xxh3_128(content.as_bytes()));
    world.known_pairs.push((hash, content.len() as u64));
}

/// Simulates an earlier, separate run that placed exactly `rel` (and
/// nothing else) at every destination, using the same journal the later
/// "When" step will resume from — mirrors `resume_skips_placed_files` in
/// `tests/engine.rs`, but scoped to a single file via a hand-built
/// single-entry plan rather than filtering the full plan.
#[given(expr = "a previous run already placed {string}")]
fn previous_run_placed(world: &mut IngestWorld, rel: String) -> Result<(), String> {
    let source = world.source_path()?.to_path_buf();
    let path = source.join(&rel);
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let mini_plan = IngestPlan {
        files: vec![PlannedFile {
            source: path,
            rel,
            size,
            prehash: None,
            decision: Decision::Copy,
        }],
    };
    let dests = build_dest_specs(&world.dests, INGEST_SUBDIR);
    let journal_path = world.journal_path()?;
    let mut journal = Journal::open_append(&journal_path).map_err(|e| e.to_string())?;
    engine::run(
        &mini_plan,
        &dests,
        &BTreeSet::new(),
        &mut journal,
        &RealSinks,
        &EngineConfig { jobs: 1 },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Runs the plan/copy/verify pipeline and, when anything was placed, writes
/// an MHL generation per destination straight from the placed files — the
/// same shape `cmd_ingest` uses, exercised here without the CLI or catalog
/// events.
#[when(expr = "the card is ingested to {string}")]
fn ingest_card(world: &mut IngestWorld, subdir: String) -> Result<(), String> {
    let source = world.source_path()?.to_path_buf();
    let known = KnownAssets::from_pairs(world.known_pairs.clone());
    let ingest_plan = plan_source(&source, &known, DedupeMode::Skip).map_err(|e| e.to_string())?;
    let dests = build_dest_specs(&world.dests, &subdir);
    let journal_path = world.journal_path()?;
    let resume_set = Journal::load(&journal_path)
        .map_err(|e| e.to_string())?
        .placed;
    let mut journal = Journal::open_append(&journal_path).map_err(|e| e.to_string())?;
    let config = EngineConfig { jobs: 1 };

    let outcome = if let Some(idx) = world.corrupt_dest_index {
        let target = world
            .dests
            .get(idx)
            .ok_or("corrupt destination index out of range")?
            .path()
            .to_string_lossy()
            .into_owned();
        let sinks = CorruptingSinks { target };
        engine::run(
            &ingest_plan,
            &dests,
            &resume_set,
            &mut journal,
            &sinks,
            &config,
        )
    } else {
        engine::run(
            &ingest_plan,
            &dests,
            &resume_set,
            &mut journal,
            &RealSinks,
            &config,
        )
    }
    .map_err(|e| e.to_string())?;

    if !outcome.placed.is_empty() {
        let hashdate = "2026-07-30T00:00:00Z".to_string();
        let entries = outcome
            .placed
            .iter()
            .map(|p| MhlEntry {
                rel: p.dest_rel.clone(),
                size: p.size,
                xxh64: p.xxh64.clone(),
                action: HashAction::Original,
                hashdate: hashdate.clone(),
            })
            .collect();
        let hash_list = HashList {
            creation_date: hashdate,
            hostname: mhl::local_hostname(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            entries,
        };
        for dest in &dests {
            let written =
                mhl::write_generation(&dest.root, &hash_list).map_err(|e| e.to_string())?;
            world.generations.push((dest.root.clone(), written));
        }
    }

    world.subdir = subdir;
    world.outcome = Some(outcome);
    Ok(())
}

#[then("every destination holds identical verified copies")]
fn every_destination_identical(world: &mut IngestWorld) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    if !outcome.failed.is_empty() {
        return Err(format!("expected no failures, got {:?}", outcome.failed));
    }
    if outcome.placed.is_empty() {
        return Err("expected at least one placed file".to_string());
    }
    for placed in &outcome.placed {
        let mut reference: Option<Vec<u8>> = None;
        for dest in &world.dests {
            let path = dest.path().join(&placed.dest_rel);
            let content =
                std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
            match &reference {
                None => reference = Some(content),
                Some(expected) if *expected == content => {}
                Some(_) => return Err(format!("destinations disagree on {}", placed.rel)),
            }
        }
    }
    Ok(())
}

#[then(expr = "every destination has an ASC MHL generation covering {int} files")]
fn every_destination_has_generation(world: &mut IngestWorld, count: usize) -> Result<(), String> {
    if world.generations.len() != world.dests.len() {
        return Err(format!(
            "expected a generation per destination ({} dests), got {}",
            world.dests.len(),
            world.generations.len()
        ));
    }
    for (root, written) in &world.generations {
        let hash_list = mhl::read_generation(&written.path).map_err(|e| e.to_string())?;
        if hash_list.entries.len() != count {
            return Err(format!(
                "expected the generation at {} ({}) to cover {count} file(s), got {}",
                written.path.display(),
                root.display(),
                hash_list.entries.len()
            ));
        }
    }
    Ok(())
}

#[then("no files are placed")]
fn no_files_placed(world: &mut IngestWorld) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    if !outcome.placed.is_empty() {
        return Err(format!(
            "expected no placed files, got {:?}",
            outcome.placed
        ));
    }
    Ok(())
}

#[then(expr = "{int} duplicate is reported")]
fn duplicate_is_reported(world: &mut IngestWorld, count: usize) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    if outcome.skipped_duplicates.len() != count {
        return Err(format!(
            "expected {count} duplicate(s) reported, got {}",
            outcome.skipped_duplicates.len()
        ));
    }
    Ok(())
}

#[then(
    expr = "destination {int} reports a verification failure and holds only a quarantined partial"
)]
fn destination_reports_failure(world: &mut IngestWorld, index: usize) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    let dest = world
        .dests
        .get(index - 1)
        .ok_or("destination index out of range")?;
    if outcome.failed.is_empty() {
        return Err("expected a verification failure to be reported".to_string());
    }
    let dest_str = dest.path().to_string_lossy().into_owned();
    let mentions_dest = outcome.failed.iter().any(|f| f.reason.contains(&dest_str));
    if !mentions_dest {
        return Err(format!(
            "expected a failure reason naming destination {index}, got {:?}",
            outcome.failed
        ));
    }
    let has_partial = walkdir::WalkDir::new(dest.path())
        .into_iter()
        .filter_map(Result::ok)
        .any(|e| e.file_name().to_string_lossy().contains(".maj-partial-"));
    if !has_partial {
        return Err(format!(
            "expected a quarantined partial under {}",
            dest.path().display()
        ));
    }
    let final_path_exists = outcome
        .placed
        .iter()
        .any(|p| dest.path().join(&p.dest_rel).is_file());
    if final_path_exists {
        return Err("a corrupted destination must never have a file at its final path".to_string());
    }
    Ok(())
}

#[then(expr = "destination {int} holds an identical verified copy")]
fn destination_holds_copy(world: &mut IngestWorld, index: usize) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    let dest = world
        .dests
        .get(index - 1)
        .ok_or("destination index out of range")?;
    // Single-file scenario: the rel this run touched is present in exactly
    // one of `placed`/`failed`, regardless of which destination failed.
    let rel = outcome
        .placed
        .first()
        .map(|p| p.rel.clone())
        .or_else(|| outcome.failed.first().map(|f| f.rel.clone()))
        .ok_or("no file was attempted this run")?;
    let source_bytes = std::fs::read(world.source_path()?.join(&rel)).map_err(|e| e.to_string())?;
    let dest_path = dest.path().join(&world.subdir).join(&rel);
    let dest_bytes =
        std::fs::read(&dest_path).map_err(|e| format!("reading {}: {e}", dest_path.display()))?;
    if source_bytes != dest_bytes {
        return Err(format!("destination {index} content differs from source"));
    }
    Ok(())
}

#[then(expr = "only {string} is copied")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn only_rel_is_copied(world: &mut IngestWorld, rel: String) -> Result<(), String> {
    let outcome = world.outcome.as_ref().ok_or("no outcome recorded yet")?;
    let placed_rels: Vec<&str> = outcome.placed.iter().map(|p| p.rel.as_str()).collect();
    if placed_rels != [rel.as_str()] {
        return Err(format!(
            "expected only {rel:?} copied this run, got {placed_rels:?}"
        ));
    }
    Ok(())
}

fn main() {
    futures::executor::block_on(IngestWorld::run("tests/features"));
}
