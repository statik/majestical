//! The guard on the Keychain seam. After phase 7G a `maj` child reads the
//! macOS Keychain whenever its backend is `OpenRouter` and the env key is
//! unset, and `describer clear-key` deletes from it — so a test that spawns
//! the binary without `MAJ_KEYCHAIN_SERVICE` would reach the item the
//! developer's own `maj` keeps. Every spawn goes through `common::maj_bin`,
//! which sets a throwaway service (pinned by `common`'s own
//! `every_child_gets_its_own_throwaway_keychain_service`); this test fails
//! when a file goes around it.

/// How a test names the binary. `cargo_bin` is the bare token, so it covers
/// `Command::cargo_bin(..)` and the `cargo_bin!`/`cargo_bin_cmd!` macros
/// alike — a macro form slipped past an earlier `cargo_bin("maj")` spelling.
/// Split so this file does not match itself.
const SPAWN_TOKENS: [&str; 2] = [concat!("cargo_", "bin"), concat!("CARGO_BIN_", "EXE_maj")];

/// Files that name the binary themselves, each for a reason `common` cannot
/// serve — and each must still set the service variable, checked below.
/// - `acceptance.rs`, `inbox_acceptance.rs`: `harness = false` cucumber
///   binaries, built without `cfg(test)`, which every `common` helper needs.
/// - `mcp_smoke.rs`: drives a long-lived child over piped stdio with
///   `std::process::Command`; `common` hands out `assert_cmd` commands.
const ALLOWED: [&str; 3] = ["acceptance.rs", "inbox_acceptance.rs", "mcp_smoke.rs"];

/// The one file allowed to name the binary without setting the service: it
/// is what sets it for everyone else.
const SEAM: &str = "common/mod.rs";

/// Drops `//` line and `/* */` block comments, so a mention of the token or
/// of `SERVICE_ENV` in prose neither trips the guard nor satisfies it. Not a
/// Rust lexer: a token inside a string literal still counts, which errs
/// toward failing rather than passing.
#[cfg(test)]
fn without_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..]
            .split_once("*/")
            .map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out.lines()
        .map(|line| line.split_once("//").map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `tests/`, at any depth: a spawn hidden in
/// `tests/<dir>/mod.rs` is as dangerous as one at the top level.
#[cfg(test)]
fn sources_under(dir: &std::path::Path, found: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("read tests dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            sources_under(&path, found);
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .strip_prefix(
                dir.ancestors()
                    .find(|a| a.ends_with("tests"))
                    .unwrap_or(dir),
            )
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        found.push((name, std::fs::read_to_string(&path).expect("read source")));
    }
}

#[test]
fn no_test_names_the_binary_without_a_throwaway_keychain_service() {
    let tests_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources = Vec::new();
    sources_under(&tests_dir, &mut sources);
    assert!(
        sources.len() > 10,
        "looked in the wrong place: {tests_dir:?}"
    );
    for (name, source) in sources {
        if name == SEAM {
            continue;
        }
        let code = without_comments(&source);
        if !SPAWN_TOKENS.iter().any(|token| code.contains(token)) {
            continue;
        }
        assert!(
            ALLOWED.contains(&name.as_str()),
            "{name} names the maj binary itself: build the command with common::maj_bin() \
             (or maj/maj_as/maj_with_keychain), which sets a throwaway Keychain service"
        );
        // The `.env(` call, not the bare path: a file that only mentions the
        // constant, or passes it to something else, does not set it on the
        // child. Whitespace goes first because the call spans lines.
        let dense = code.replace(char::is_whitespace, "");
        assert!(
            dense.contains(".env(majestical_secrets::SERVICE_ENV"),
            "{name} is allowed to name the maj binary but never passes \
             majestical_secrets::SERVICE_ENV to .env()"
        );
    }
}
