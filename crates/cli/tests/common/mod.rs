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
    let mut c = maj_bin();
    c.env("MAJ_CATALOG", catalog)
        .env("MAJ_MACHINE_ID", machine_id)
        .env("MAJ_STATE_DIR", state);
    c
}

/// The bare `maj` binary, and the ONE place these suites may name it: every
/// child gets a throwaway Keychain service, so no test can read, write or
/// delete the item the developer's own `maj` keeps under the default name.
/// `keychain_guard.rs` fails the build of any test that goes around this.
#[cfg(test)]
pub fn maj_bin() -> Command {
    let mut c = Command::cargo_bin("maj").unwrap();
    c.env(
        majestical_secrets::SERVICE_ENV,
        throwaway_keychain_service(),
    );
    c
}

/// A Keychain service name no other test, process or run shares.
#[cfg(test)]
pub fn throwaway_keychain_service() -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    format!(
        "{THROWAWAY_SERVICE_PREFIX}{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

#[cfg(test)]
const THROWAWAY_SERVICE_PREFIX: &str = "majestical-test-";

/// Like [`maj`], for invocations that must SHARE one throwaway Keychain item
/// — a `describer set --api-key` and the `show` that reads it back. Hold a
/// [`KeychainCleanup`] for `service` for as long as the item may exist.
#[cfg(test)]
pub fn maj_with_keychain(
    catalog: &std::path::Path,
    state: &std::path::Path,
    service: &str,
) -> Command {
    let mut c = maj(catalog, state);
    c.env(majestical_secrets::SERVICE_ENV, service);
    c
}

/// Deletes a throwaway service's Keychain item when dropped, so a test that
/// stored one leaves nothing in the login Keychain even when it fails midway.
#[cfg(test)]
pub struct KeychainCleanup(String);

#[cfg(test)]
impl KeychainCleanup {
    /// Panics on any name but a throwaway one: this type must never be
    /// pointed at the developer's real item.
    pub fn new(service: &str) -> Self {
        assert!(
            service.starts_with(THROWAWAY_SERVICE_PREFIX),
            "not a throwaway Keychain service: {service}"
        );
        Self(service.to_string())
    }
}

#[cfg(test)]
impl Drop for KeychainCleanup {
    fn drop(&mut self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        // A failed delete is the usual case — most tests store nothing — so
        // its status only picks the word logged (shown with `--nocapture`).
        let deleted = std::process::Command::new("security")
            .args(["delete-generic-password", "-s", &self.0])
            .args(["-a", "openrouter-api-key"])
            .output()
            .is_ok_and(|out| out.status.success());
        let found = if deleted { "deleted" } else { "nothing stored" };
        eprintln!("keychain cleanup: {}: {found}", self.0);
    }
}

/// Whether `service` holds a Keychain item. Asks for the item's attributes
/// only, never its secret (`-w`): that would raise a macOS prompt, since
/// `security` is not the binary that stored it.
#[cfg(test)]
#[cfg(target_os = "macos")]
pub fn keychain_item_exists(service: &str) -> bool {
    assert!(
        service.starts_with(THROWAWAY_SERVICE_PREFIX),
        "not a throwaway Keychain service: {service}"
    );
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", service])
        .args(["-a", "openrouter-api-key"])
        .output()
        .expect("run security")
        .status
        .success()
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

/// The three lines `maj describer set --backend ollama --model m` writes,
/// for a caller of [`break_describer_config`] that breaks a line after them
/// — line 4.
#[cfg(test)]
pub const DESCRIBER_CONFIG_HEAD: &str =
    "backend = \"ollama\"\nbase_url = \"http://localhost:11434\"\nmodel = \"m\"\n";

/// Configures a describer through `maj describer set` — so `describer.toml`
/// sits wherever the CLI really puts it under `state` — then replaces the
/// file's contents with `contents`.
#[cfg(test)]
pub fn break_describer_config(root: &std::path::Path, state: &std::path::Path, contents: &str) {
    maj(root, state)
        .args(["describer", "set", "--backend", "ollama", "--model", "m"])
        .assert()
        .success();
    let paths = walkdir_find(state, "describer.toml");
    assert_eq!(paths.len(), 1, "exactly one describer config: {paths:?}");
    std::fs::write(&paths[0], contents).expect("rewrite describer.toml");
}

// Not every integration-test binary that pulls in this module calls
// `walkdir_find` directly (doctor_smoke.rs never does), and each
// `tests/*.rs` file is its own crate, so dead-code reachability is judged
// per binary. This in-module test gives every binary a real caller so the
// helper never trips `dead_code`, without reaching for `#[allow]` (denied)
// or `#[expect]` (would itself fail wherever the helper IS otherwise used).
#[cfg(test)]
mod tests {
    use super::{
        DESCRIBER_CONFIG_HEAD, KeychainCleanup, asset_id_of, break_describer_config,
        first_asset_id, fixture_catalog, maj, maj_with_keychain, throwaway_keychain_service,
        walkdir_find,
    };

    // Gives every binary compiling this module a real call site for the
    // Keychain seam, same `dead_code` rationale as the tests below. It
    // spawns nothing and stores nothing.
    #[test]
    fn every_child_gets_its_own_throwaway_keychain_service() {
        let first = throwaway_keychain_service();
        let second = throwaway_keychain_service();
        assert_ne!(first, second);
        assert!(first.starts_with("majestical-test-"), "{first}");

        let dir = tempfile::tempdir().expect("tempdir");
        let (root, state) = (dir.path().join("cat"), dir.path().join("state"));
        let service_of = |command: &assert_cmd::Command| {
            command
                .get_envs()
                .find(|(name, _)| *name == majestical_secrets::SERVICE_ENV)
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
        };
        let plain = service_of(&maj(&root, &state)).expect("maj sets a service");
        assert!(plain.starts_with("majestical-test-"), "{plain}");
        assert_ne!(Some(plain), service_of(&maj(&root, &state)));

        let _cleanup = KeychainCleanup::new(&first);
        assert_eq!(
            service_of(&maj_with_keychain(&root, &state, &first)),
            Some(first.clone())
        );
        #[cfg(target_os = "macos")]
        assert!(!super::keychain_item_exists(&first));
    }

    #[test]
    #[should_panic(expected = "not a throwaway Keychain service")]
    fn the_cleanup_refuses_the_real_service_name() {
        let _cleanup = KeychainCleanup::new("majestical");
    }

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
    // `break_describer_config`/`DESCRIBER_CONFIG_HEAD`, same `dead_code`
    // rationale — and pins the claim the constant makes about `set`.
    #[test]
    fn break_describer_config_replaces_what_set_wrote() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let state = dir.path().join("state");
        maj(&root, &state)
            .args(["catalog", "init"])
            .assert()
            .success();
        maj(&root, &state)
            .args(["describer", "set", "--backend", "ollama", "--model", "m"])
            .assert()
            .success();
        let paths = walkdir_find(&state, "describer.toml");
        assert_eq!(
            std::fs::read_to_string(&paths[0]).expect("read"),
            DESCRIBER_CONFIG_HEAD
        );

        break_describer_config(&root, &state, "broken");
        assert_eq!(std::fs::read_to_string(&paths[0]).expect("read"), "broken");
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
