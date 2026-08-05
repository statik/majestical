//! The GUI analogue of `crates/cli/tests/services_parity.rs`: a command's
//! serialized outcome must be the same JSON the other heads render from.
//!
//! Two checks, because the command layer can drift in two directions. The
//! first compares a command impl's serialized outcome against a direct
//! `majestical_services` call — both wrap the SAME function, so what this
//! pins is that the command layer adds, renames, or loses nothing. The
//! second spawns the real `maj` binary and compares row content against
//! `maj search --json`, the only cross-binary proof that the GUI and the
//! CLI describe the same hit the same way. (The CLI hand-renders its JSON,
//! so full-payload equality is not the contract — row content is.)
//!
//! The cross-binary test needs a built `maj`: `just gui-test` builds one and
//! points `MAJ_BIN` at it. Without it the test skips loudly rather than
//! failing, the same rule `services_parity.rs` follows for `/tmp/maj-ref`.
use majestical_desktop::commands::{CatalogCfg, initialize_catalog_impl, search_assets_impl};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// `#[cfg(test)]` on the helpers below is not redundant despite this file
// already building with `--cfg test`: clippy's in-test detection for
// `allow-expect-in-tests` keys off `#[test]`/`#[cfg(test)]` directly on the
// item — the full rationale lives in `crates/cli/tests/common/mod.rs`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const SEEDED_ASSET: &str = "xxh3:0123456789abcdef0123456789abcdef";
const QUERY: &str = "clip";
const LIMIT: usize = 10;

#[cfg(test)]
fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = tempfile::tempdir().expect("state dir");
    // SAFETY: serialized by ENV_LOCK; no other thread reads env mid-test.
    unsafe { std::env::set_var("MAJ_STATE_DIR", state.path()) };
    let out = f();
    drop(state);
    out
}

/// Same seeding as `tests/commands.rs` — one asset on one volume, from the
/// same `Op` literals the services tests use.
#[cfg(test)]
fn seeded_cfg(catalog: PathBuf) -> CatalogCfg {
    let cfg = CatalogCfg {
        catalog,
        machine_id: "gui-test".into(),
        author: "gui-test".into(),
    };
    initialize_catalog_impl(&cfg).expect("init");
    let mut app = majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
        .expect("open");
    app.emit(vec![
        majestical_core::event::Op::VolumeSeen {
            volume: "vol1".into(),
            label: "vol1".into(),
        },
        majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId(SEEDED_ASSET.into()),
            volume: "vol1".into(),
            path: "clip.txt".into(),
            size: 5,
            mtime_ms: 1000,
        },
    ])
    .expect("emit");
    cfg
}

/// The command's outcome and a direct services call's outcome, serialized,
/// against two identically-seeded catalogs — two catalogs rather than one
/// because each call drains the notices it collected, so running both
/// against the same catalog would compare a first look at a log with a
/// second one.
#[test]
fn command_serializes_outcome_verbatim() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let via_command = seeded_cfg(dir.path().join("a"));
        let direct = seeded_cfg(dir.path().join("b"));

        let from_command = search_assets_impl(&via_command, Some(QUERY.into()), None, Some(LIMIT))
            .expect("command");
        let from_service = majestical_services::runtime::run_off_tokio_runtime(|| {
            let mut app = majestical_services::app::FsApp::open(
                &direct.catalog,
                &direct.machine_id,
                &direct.author,
            )?;
            Ok(majestical_services::search::search(
                &mut app,
                &direct.catalog,
                &majestical_services::search::SearchRequest {
                    query: Some(QUERY.into()),
                    limit: LIMIT,
                    saved: None,
                    save: None,
                },
            )?)
        })
        .expect("service");

        assert_eq!(
            serde_json::to_value(&from_command).expect("serialize command outcome"),
            serde_json::to_value(&from_service).expect("serialize service outcome"),
            "the command layer must add, rename, and lose nothing"
        );
    });
}

/// Every field `maj search --json` prints per row, compared against the
/// command's row for the same query on the same catalog.
#[expect(
    clippy::print_stderr,
    reason = "a skipped parity check must say so in the test log"
)]
#[test]
fn search_rows_match_cli_json() {
    let Some(maj) = maj_binary() else {
        eprintln!(
            "SKIP parity(search rows vs `maj search --json`): no maj binary at MAJ_BIN or \
             ../../../target/debug/maj — run `just gui-test` to build one"
        );
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome =
            search_assets_impl(&cfg, Some(QUERY.into()), None, Some(LIMIT)).expect("command");
        let cli = cli_search_json(&maj, &cfg);

        assert_eq!(
            cli["count"],
            serde_json::json!(outcome.count),
            "count must agree: {cli}"
        );
        let rows = cli["results"].as_array().expect("results array");
        assert_eq!(rows.len(), outcome.results.len(), "row count: {cli}");
        for (row, hit) in rows.iter().zip(&outcome.results) {
            assert_eq!(row["asset"], serde_json::json!(hit.asset));
            assert_eq!(row["score"], serde_json::json!(hit.score));
            assert_eq!(row["name"], serde_json::json!(hit.name));
            assert_eq!(row["tags"], serde_json::json!(hit.tags));
            assert_eq!(row["para"], serde_json::json!(hit.para));
        }
    });
}

/// The `maj` binary to compare against: `MAJ_BIN`, else the workspace's own
/// debug build (this test binary runs with the package directory as its
/// working directory).
#[cfg(test)]
fn maj_binary() -> Option<PathBuf> {
    let path = std::env::var_os("MAJ_BIN")
        .map_or_else(|| PathBuf::from("../../../target/debug/maj"), PathBuf::from);
    path.is_file().then_some(path)
}

#[cfg(test)]
fn cli_search_json(maj: &Path, cfg: &CatalogCfg) -> serde_json::Value {
    let output = std::process::Command::new(maj)
        .arg("--catalog")
        .arg(&cfg.catalog)
        .arg("--machine-id")
        .arg(&cfg.machine_id)
        .args(["search", QUERY, "--json", "--limit"])
        .arg(LIMIT.to_string())
        .output()
        .expect("run maj search");
    assert!(
        output.status.success(),
        "maj search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("maj search --json prints one JSON object")
}
