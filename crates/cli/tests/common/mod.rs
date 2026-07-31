//! Shared test-only fixtures for the CLI's integration tests. `mod common;`
//! in each test file pulls this in; cargo does not treat a subdirectory
//! under `tests/` as its own test binary, so this file is never itself
//! discovered as a separate target — see the same pattern in
//! `crates/ingest/tests/common/mod.rs`.
use assert_cmd::Command;

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
    use super::walkdir_find;

    #[test]
    fn walkdir_find_returns_empty_when_name_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(walkdir_find(dir.path(), "no-such-file").is_empty());
    }
}
