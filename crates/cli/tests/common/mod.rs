//! Shared test-only fixtures for the CLI's integration tests. `mod common;`
//! in each test file pulls this in; cargo does not treat a subdirectory
//! under `tests/` as its own test binary, so this file is never itself
//! discovered as a separate target — see the same pattern in
//! `crates/ingest/tests/common/mod.rs`.
use assert_cmd::Command;

// `#[cfg(test)]` on these helpers is not redundant despite every file under
// `tests/` already building with `--cfg test`: this repo's `clippy.toml`
// sets `allow-expect-in-tests`/`allow-unwrap-in-tests`/`allow-panic-in-tests`,
// and clippy's in-test detection for those configs keys off `#[test]`/
// `#[cfg(test)]` directly on the item, not on the ambient test-binary cfg —
// dropping it reintroduces `expect_used`/`unwrap_used`/`panic` errors under
// `-D warnings` (verified: removing it here makes `cargo clippy --test
// sync_smoke` fail). Every `#[cfg(test)]` helper in this crate's `tests/`
// tree — here and in `sync_smoke.rs`, `convergence.rs` (a different crate,
// same clippy.toml-driven reason) — follows this same pattern; this is the
// one place the full rationale is spelled out.
#[cfg(test)]
pub fn maj_as(catalog: &std::path::Path, state: &std::path::Path, machine_id: &str) -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", machine_id)
        .env("MAJ_STATE_DIR", state);
    c
}

#[cfg(test)]
pub fn maj(catalog: &std::path::Path, state: &std::path::Path) -> Command {
    maj_as(catalog, state, "test-machine")
}

/// Parses a `search --json` asset id out of the first result.
#[cfg(test)]
pub fn first_asset_id(out: &std::process::Output) -> String {
    let hits: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    hits["results"][0]["asset"].as_str().unwrap().to_string()
}

/// A small deterministic catalog for the services-extraction parity harness
/// (and any later suite needing a minimal seeded catalog): one volume
/// (`vol1`), two scanned files, and a `demo` tag on the first. Returns the
/// catalog root and its isolated state dir, both under `dir`.
#[cfg(test)]
pub fn fixture_catalog(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let root = dir.join("cat");
    let state = dir.join("state");
    maj(&root, &state)
        .args(["catalog", "init"])
        .assert()
        .success();
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("a.txt"), b"alpha").expect("write");
    std::fs::write(src.join("b.txt"), b"beta").expect("write");
    maj(&root, &state)
        .args(["scan", src.to_str().expect("utf8"), "--volume", "vol1"])
        .assert()
        .success();
    let asset = asset_id_of(&root, &state, "a.txt");
    maj(&root, &state)
        .args(["tag", "add", &asset, "demo"])
        .assert()
        .success();
    (root, state)
}

/// Finds an asset id via `search --json` — keeps a fixture independent of
/// hash literals.
#[cfg(test)]
pub fn asset_id_of(root: &std::path::Path, state: &std::path::Path, name: &str) -> String {
    let out = maj(root, state)
        .args(["search", name, "--json"])
        .output()
        .expect("run");
    first_asset_id(&out)
}

#[cfg(test)]
pub fn walkdir_find(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == name)
        .map(walkdir::DirEntry::into_path)
        .collect()
}

// Not every integration-test binary that pulls in this module calls
// `walkdir_find` directly (describer_smoke.rs uses only `maj`), and each
// `tests/*.rs` file is its own crate, so dead-code reachability is judged
// per binary. This in-module test gives every binary a real caller so the
// helper never trips `dead_code`, without reaching for `#[allow]` (denied)
// or `#[expect]` (would itself fail wherever the helper IS otherwise used).
#[cfg(test)]
mod tests {
    use super::{asset_id_of, first_asset_id, fixture_catalog, walkdir_find};

    #[test]
    fn walkdir_find_returns_empty_when_name_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(walkdir_find(dir.path(), "no-such-file").is_empty());
    }

    // Gives every binary compiling this module a real call site for
    // `fixture_catalog`/`asset_id_of` — only `services_parity.rs` calls
    // them directly today, same `dead_code` rationale as the tests above.
    #[test]
    fn fixture_catalog_seeds_two_tagged_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (root, state) = fixture_catalog(dir.path());
        let asset = asset_id_of(&root, &state, "a.txt");
        assert!(asset.starts_with("xxh3:"));
    }

    // Gives every binary compiling this module a real call site for
    // `first_asset_id`, same rationale as the test above for
    // `walkdir_find` — not every `tests/*.rs` file that pulls in `common`
    // calls it directly (e.g. `describer_smoke.rs`), so this keeps it off
    // `dead_code` without an `#[allow]` (denied by house lint policy).
    #[test]
    fn first_asset_id_reads_the_first_result() {
        let json = serde_json::json!({"results": [{"asset": "xxh3:deadbeef"}]});
        let output = std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: json.to_string().into_bytes(),
            stderr: Vec::new(),
        };
        assert_eq!(first_asset_id(&output), "xxh3:deadbeef");
    }
}
