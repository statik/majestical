//! The guard on the Keychain seam. After phase 7G a `maj` child reads the
//! macOS Keychain whenever its backend is `OpenRouter` and the env key is
//! unset, and `describer clear-key` deletes from it — so a test that spawns
//! the binary without `MAJ_KEYCHAIN_SERVICE` would reach the item the
//! developer's own `maj` keeps. Every spawn goes through `common::maj_bin`,
//! which sets a throwaway service (pinned by `common`'s own
//! `every_child_gets_its_own_throwaway_keychain_service`); this test fails
//! when a file goes around it.

/// How a test names the binary. Split so this file does not match itself.
const SPAWN_TOKENS: [&str; 2] = [
    concat!("cargo_", "bin(\"maj\")"),
    concat!("CARGO_BIN_", "EXE_maj"),
];

/// Files that name the binary themselves, each for a reason `common` cannot
/// serve — and each must still set the service variable, checked below.
/// - `acceptance.rs`, `inbox_acceptance.rs`: `harness = false` cucumber
///   binaries, built without `cfg(test)`, which every `common` helper needs.
/// - `mcp_smoke.rs`: drives a long-lived child over piped stdio with
///   `std::process::Command`; `common` hands out `assert_cmd` commands.
const ALLOWED: [&str; 3] = ["acceptance.rs", "inbox_acceptance.rs", "mcp_smoke.rs"];

#[test]
fn no_test_names_the_binary_without_a_throwaway_keychain_service() {
    let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut checked = 0;
    for entry in std::fs::read_dir(&tests_dir).expect("read tests/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8 file name")
            .to_string();
        let source = std::fs::read_to_string(&path).expect("read test source");
        checked += 1;
        if !SPAWN_TOKENS.iter().any(|token| source.contains(token)) {
            continue;
        }
        assert!(
            ALLOWED.contains(&name.as_str()),
            "{name} names the maj binary itself: build the command with common::maj_bin() \
             (or maj/maj_as/maj_with_keychain), which sets a throwaway Keychain service"
        );
        assert!(
            source.contains("majestical_secrets::SERVICE_ENV"),
            "{name} is allowed to name the maj binary but never sets MAJ_KEYCHAIN_SERVICE"
        );
    }
    assert!(checked > 10, "looked in the wrong place: {tests_dir:?}");
}
