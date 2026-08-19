//! The GUI analogue of `crates/cli/tests/services_parity.rs`: a command's
//! serialized outcome must be the same JSON the other heads render from.
//!
//! Two kinds of check, because the command layer can drift in two
//! directions. The first compares a command impl's serialized outcome
//! against a direct `majestical_services` call — both wrap the SAME
//! function, so what this pins is that the command layer adds, renames, or
//! loses nothing. The rest spawn the real `maj` binary: the only
//! cross-binary proof that the GUI and the CLI describe the same data the
//! same way.
//!
//! How much of the CLI's JSON is the contract depends on the verb. `maj
//! search --json` is hand-rendered (it predates the services extraction, and
//! its rendering WAS the wire contract), so only row content is compared
//! there. Every phase 7D read verb prints its outcome struct as-is — the
//! same struct the command returns — so those rows compare the whole
//! document.
//!
//! The cross-binary tests need a built `maj`: `just gui-test` builds one and
//! points `MAJ_BIN` at it. Without it they skip loudly rather than failing,
//! the same rule `services_parity.rs` follows for `/tmp/maj-ref`.
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
#[test]
fn search_rows_match_cli_json() {
    let Some(maj) = maj_or_skip("search rows vs `maj search --json`") else {
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

/// The `maj` binary to compare against, or `None` after saying loudly in
/// the test log which check is being skipped and how to stop skipping it.
/// `what` names the comparison, e.g. "browse tree vs `maj browse tree
/// --json`".
#[cfg(test)]
#[expect(
    clippy::print_stderr,
    reason = "a skipped parity check must say so in the test log"
)]
fn maj_or_skip(what: &str) -> Option<PathBuf> {
    let found = maj_binary();
    if found.is_none() {
        eprintln!(
            "SKIP parity({what}): no maj binary at MAJ_BIN or ../../../target/debug/maj — run \
             `just gui-test` to build one"
        );
    }
    found
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

/// Runs `maj <args>` against `cfg`'s catalog and parses its one JSON line.
/// `MAJ_STATE_DIR` comes from the inherited environment — [`with_state_dir`]
/// has already pointed it at this test's tempdir.
#[cfg(test)]
fn cli_json(maj: &Path, cfg: &CatalogCfg, args: &[&str]) -> serde_json::Value {
    let output = std::process::Command::new(maj)
        .arg("--catalog")
        .arg(&cfg.catalog)
        .arg("--machine-id")
        .arg(&cfg.machine_id)
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("run maj {args:?}: {err}"));
    assert!(
        output.status.success(),
        "maj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("maj {args:?} must print one JSON object: {err}"))
}

// ---------------------------------------------------------------------------
// Phase 7D read verbs. Unlike `search` above — whose CLI JSON is
// hand-rendered, so only row CONTENT is the contract — every verb below
// prints `serde_json::to_string(&outcome)` of the SAME outcome struct the
// command returns (see `cli/src/commands.rs::cmd_browse_tree`'s doc for the
// as-is policy). For these the whole payload is the contract, so these rows
// compare the whole document and would catch a field the GUI drops, renames,
// or adds on its way out.
//
// The ingest commands `ingest_state` and `start_ingest` get no row here on
// purpose: neither has a CLI JSON twin to compare against. `ingest_state`
// reports the state of a run owned by THIS process's `IngestState` (a
// separate `maj` invocation has no such run and no verb that would print
// one), and `start_ingest` streams its outcome as Tauri events rather than
// returning a payload. `tests/commands.rs` covers both end to end instead.
// ---------------------------------------------------------------------------

/// `browse_tree` against `maj browse tree --json`: the whole payload,
/// including each volume's online flag and per-folder recursive counts.
#[test]
fn browse_tree_matches_cli_json() {
    let Some(maj) = maj_or_skip("browse_tree vs `maj browse tree --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome = majestical_desktop::commands::browse_tree_impl(&cfg).expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(&maj, &cfg, &["browse", "tree", "--json"]),
            "browse_tree and `maj browse tree --json` must render the same document"
        );
    });
}

/// `browse_list` against `maj browse list --json`, with every knob left at
/// its default — the defaults themselves are part of what this pins, since
/// each head applies them independently (`limit`/`offset`/`flatten`).
#[test]
fn browse_list_matches_cli_json() {
    let Some(maj) = maj_or_skip("browse_list vs `maj browse list --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome = majestical_desktop::commands::browse_list_impl(
            &cfg,
            "vol1".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(
                &maj,
                &cfg,
                &["browse", "list", "--volume", "vol1", "--json"]
            ),
            "browse_list and `maj browse list --json` must render the same document"
        );
    });
}

/// `list_tags` against `maj tags list --json`, on a catalog carrying one
/// tag — an empty vocabulary would compare two empty arrays and prove
/// nothing about the row shape.
#[test]
fn list_tags_matches_cli_json() {
    let Some(maj) = maj_or_skip("list_tags vs `maj tags list --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let mut app =
            majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
                .expect("open");
        app.emit(vec![majestical_core::event::Op::TagAdd {
            asset: majestical_core::event::AssetId(SEEDED_ASSET.into()),
            tag: "b-roll".into(),
        }])
        .expect("emit");
        drop(app);

        let outcome = majestical_desktop::commands::list_tags_impl(&cfg).expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(&maj, &cfg, &["tags", "list", "--json"]),
            "list_tags and `maj tags list --json` must render the same document"
        );
    });
}

/// `list_unfinished_ingests` against `maj ingest unfinished --json`. Both
/// heads read the same per-machine run journals, so this needs one on disk:
/// a `RunStarted` record promising more files than were ever placed, which
/// is what a run cancelled before its first file leaves behind.
#[test]
fn list_unfinished_ingests_matches_cli_json() {
    let Some(maj) = maj_or_skip("list_unfinished_ingests vs `maj ingest unfinished --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let runs_dir = majestical_services::state_dir::catalog_paths(
            &cfg.catalog,
            &majestical_services::notices::Notices::new(),
        )
        .expect("state dir")
        .runs_dir;
        std::fs::write(
            runs_dir.join("01JABCDEFGHJKMNPQRSTVWXYZ0.jsonl"),
            b"{\"rec\":\"run_started\",\"run\":\"01JABCDEFGHJKMNPQRSTVWXYZ0\",\
              \"source\":\"/cards/A001\",\"dests\":[\"/media/raid\"],\"planned\":2}\n",
        )
        .expect("write journal");

        let outcome =
            majestical_desktop::ingest::list_unfinished_ingests_impl(&cfg).expect("command");
        let payload = serde_json::to_value(&outcome).expect("serialize command outcome");
        assert_eq!(
            payload["runs"][0]["run_id"],
            serde_json::json!("01JABCDEFGHJKMNPQRSTVWXYZ0"),
            "the seeded journal must be listed, or this row proves nothing: {payload}"
        );
        assert_eq!(
            payload,
            cli_json(&maj, &cfg, &["ingest", "unfinished", "--json"]),
            "list_unfinished_ingests and `maj ingest unfinished --json` must render the same \
             document"
        );
    });
}
