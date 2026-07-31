//! CLI acceptance: real `maj` binary processes over real temp-dir catalogs,
//! exercising the layered search flows end to end (filters, name ranking,
//! saved searches, and the semantic layer's degrade-with-a-notice path).
//!
//! Steps return `Result` instead of asserting/panicking: this binary is a
//! `harness = false` integration test (see `crates/core/tests/acceptance.rs`
//! and `crates/ingest/tests/acceptance.rs` for the same pattern), so it is
//! not compiled under `cfg(test)` the way `#[test]` functions are, and the
//! workspace denies `panic`/`unwrap_used` outside test code.
use assert_cmd::Command;
use cucumber::{World, given, then, when};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The machine every "I search"/"a catalog with assets" step acts as, absent
/// an explicit `machine "x"` in the Gherkin text.
const DEFAULT_MACHINE: &str = "acceptance";

#[derive(Debug, World)]
#[world(init = Self::new)]
struct SearchWorld {
    /// Holds `cat/` (the shared catalog root every machine points `MAJ_CATALOG`
    /// at) and `media/` (the source files `scan` reads from) for the
    /// scenario's lifetime — dropped, and thus cleaned up, when the World is.
    root: Option<tempfile::TempDir>,
    /// Per-machine `MAJ_STATE_DIR`, lazily created on first use so distinct
    /// machine names never share local sqlite/journal state even though they
    /// share one catalog root — mirrors real separate machines, unlike
    /// `cli_smoke.rs`'s single-process convergence tests which reuse one
    /// state dir across machines for brevity.
    machine_state_dirs: BTreeMap<String, PathBuf>,
    /// Always set as `MAJ_MODEL_DIR` for every invocation: a permanently
    /// empty directory, so the encoder model is deterministically absent and
    /// the semantic layer never joins a search in this suite.
    empty_model_dir: Option<tempfile::TempDir>,
    last_stdout: String,
    last_stderr: String,
}

impl SearchWorld {
    fn new() -> Self {
        Self {
            root: None,
            machine_state_dirs: BTreeMap::new(),
            empty_model_dir: None,
            last_stdout: String::new(),
            last_stderr: String::new(),
        }
    }

    fn root_path(&self) -> Result<&Path, String> {
        self.root
            .as_ref()
            .map(tempfile::TempDir::path)
            .ok_or_else(|| "no catalog set up yet".to_string())
    }

    fn catalog_dir(&self) -> Result<PathBuf, String> {
        Ok(self.root_path()?.join("cat"))
    }

    fn model_dir(&self) -> Result<&Path, String> {
        self.empty_model_dir
            .as_ref()
            .map(tempfile::TempDir::path)
            .ok_or_else(|| "no model dir set up yet".to_string())
    }

    /// Resolves (creating on first use) the state dir a given machine name
    /// gets for the rest of the scenario — see `machine_state_dirs`'s doc
    /// comment for why this is per-machine rather than shared.
    fn state_dir_for(&mut self, machine: &str) -> Result<PathBuf, String> {
        if let Some(dir) = self.machine_state_dirs.get(machine) {
            return Ok(dir.clone());
        }
        let dir = self.root_path()?.join("state").join(machine);
        self.machine_state_dirs
            .insert(machine.to_string(), dir.clone());
        Ok(dir)
    }

    /// Builds a `maj` invocation as `machine`, with every env var a real
    /// invocation needs already set: catalog root, per-machine identity and
    /// state dir, and the always-empty model dir.
    fn maj_as(&mut self, machine: &str) -> Result<Command, String> {
        let catalog = self.catalog_dir()?;
        let state = self.state_dir_for(machine)?;
        let model = self.model_dir()?.to_path_buf();
        let mut cmd = Command::cargo_bin("maj").map_err(|e| e.to_string())?;
        cmd.env("MAJ_CATALOG", catalog)
            .env("MAJ_MACHINE_ID", machine)
            .env("MAJ_STATE_DIR", state)
            .env("MAJ_MODEL_DIR", model);
        Ok(cmd)
    }

    /// Runs `maj` as `machine` with `args`, recording stdout/stderr for
    /// later `Then` steps and failing the step (not panicking) on a nonzero
    /// exit.
    fn exec(&mut self, machine: &str, args: &[&str]) -> Result<(), String> {
        let output = self
            .maj_as(machine)?
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        self.last_stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        self.last_stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(format!(
                "`maj {}` (machine {machine}) failed: {}\nstdout: {}\nstderr: {}",
                args.join(" "),
                output.status,
                self.last_stdout,
                self.last_stderr
            ));
        }
        Ok(())
    }

    fn last_results_json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.last_stdout).map_err(|e| {
            format!(
                "parsing search --json output: {e}\nstdout: {}",
                self.last_stdout
            )
        })
    }
}

/// Writes `name` under `dir`, with the name itself as content — enough
/// distinct, non-empty bytes for `scan` to hash without any file colliding
/// with another.
fn write_asset(dir: &Path, name: &str) -> Result<(), String> {
    std::fs::write(dir.join(name), name.as_bytes()).map_err(|e| e.to_string())
}

#[given(expr = "a catalog with assets {string} and {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn catalog_with_two_assets(world: &mut SearchWorld, a: String, b: String) -> Result<(), String> {
    let root = tempfile::tempdir().map_err(|e| e.to_string())?;
    let media = root.path().join("media");
    std::fs::create_dir_all(&media).map_err(|e| e.to_string())?;
    write_asset(&media, &a)?;
    write_asset(&media, &b)?;
    world.root = Some(root);
    world.empty_model_dir = Some(tempfile::tempdir().map_err(|e| e.to_string())?);

    world.exec(DEFAULT_MACHINE, &["catalog", "init"])?;
    let media_str = media.to_string_lossy().into_owned();
    world.exec(
        DEFAULT_MACHINE,
        &["scan", &media_str, "--volume", "acceptance-vol"],
    )?;
    Ok(())
}

#[given(expr = "{string} is tagged {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn tag_asset(world: &mut SearchWorld, name: String, tag: String) -> Result<(), String> {
    let stem = name
        .rsplit_once('.')
        .map_or(name.as_str(), |(stem, _)| stem);
    world.exec(DEFAULT_MACHINE, &["search", stem, "--json"])?;
    let hits = world.last_results_json()?;
    let results = hits["results"]
        .as_array()
        .ok_or_else(|| format!("no results array in: {hits}"))?;
    let asset_id = results
        .iter()
        .find(|r| r["name"] == name)
        .and_then(|r| r["asset"].as_str())
        .ok_or_else(|| format!("no result named {name:?} in: {hits}"))?
        .to_string();
    world.exec(DEFAULT_MACHINE, &["tag", "add", &asset_id, &tag])?;
    Ok(())
}

#[given("no encoder model is installed")]
fn no_encoder_model(_world: &mut SearchWorld) {
    // A no-op: `MAJ_MODEL_DIR` is always pointed at a permanently empty
    // directory (see `SearchWorld::empty_model_dir`'s doc comment), so this
    // scenario's precondition already holds for every step in this suite.
    // The step exists so the feature file can state the precondition
    // explicitly rather than leaving it implicit.
}

#[when(expr = "I search {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn search(world: &mut SearchWorld, query: String) -> Result<(), String> {
    world.exec(DEFAULT_MACHINE, &["search", &query, "--json"])
}

#[then(expr = "the results contain {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn results_contain(world: &mut SearchWorld, name: String) -> Result<(), String> {
    let hits = world.last_results_json()?;
    let results = hits["results"]
        .as_array()
        .ok_or_else(|| format!("no results array in: {hits}"))?;
    let found = results.iter().any(|r| r["name"] == name);
    if !found {
        return Err(format!("expected {name:?} among results, got: {hits}"));
    }
    Ok(())
}

#[then(expr = "the results do not contain {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn results_do_not_contain(world: &mut SearchWorld, name: String) -> Result<(), String> {
    let hits = world.last_results_json()?;
    let results = hits["results"]
        .as_array()
        .ok_or_else(|| format!("no results array in: {hits}"))?;
    let found = results.iter().any(|r| r["name"] == name);
    if found {
        return Err(format!(
            "expected {name:?} absent from results, got: {hits}"
        ));
    }
    Ok(())
}

#[then("the results are empty")]
fn results_are_empty(world: &mut SearchWorld) -> Result<(), String> {
    let hits = world.last_results_json()?;
    let count = hits["count"]
        .as_u64()
        .ok_or_else(|| format!("no count field in: {hits}"))?;
    if count != 0 {
        return Err(format!("expected zero results, got: {hits}"));
    }
    Ok(())
}

#[then(expr = "the notice mentions {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn notice_mentions(world: &mut SearchWorld, needle: String) -> Result<(), String> {
    if !world.last_stderr.contains(&needle) {
        return Err(format!(
            "expected stderr to mention {needle:?}, got: {}",
            world.last_stderr
        ));
    }
    Ok(())
}

#[when(expr = "machine {string} saves the search {string} as {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn save_search(
    world: &mut SearchWorld,
    machine: String,
    query: String,
    name: String,
) -> Result<(), String> {
    world.exec(&machine, &["search", &query, "--save", &name])
}

#[then(expr = "machine {string} lists a saved search named {string}")]
#[expect(
    clippy::needless_pass_by_value,
    reason = "cucumber's {string} captures always bind as owned String"
)]
fn lists_saved_search(
    world: &mut SearchWorld,
    machine: String,
    name: String,
) -> Result<(), String> {
    world.exec(&machine, &["searches", "list", "--json"])?;
    let listed = world.last_results_json()?;
    let saved = listed["saved"]
        .as_array()
        .ok_or_else(|| format!("no saved array in: {listed}"))?;
    let found = saved.iter().any(|s| s["name"] == name);
    if !found {
        return Err(format!(
            "expected a saved search named {name:?} on machine {machine}, got: {listed}"
        ));
    }
    Ok(())
}

fn main() {
    futures::executor::block_on(SearchWorld::run("tests/features"));
}
