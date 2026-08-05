//! Direct tests over the command impls with real (fixture) catalogs — the
//! `#[tauri::command]` wrappers are one-liners over these, so driving the
//! impls tests everything but Tauri's own argument plumbing without needing
//! a webview.
//!
//! `MAJ_STATE_DIR` is process-global env, so every test takes `ENV_LOCK` and
//! points the var at its own tempdir — the same reason the CLI's suites set
//! it per child process; here the "process" is this test binary.
use majestical_desktop::commands::{
    AppState, CatalogCfg, CommandError, adopt_catalog, app_status_impl, get_asset_impl,
    initialize_catalog_impl, list_saved_searches_impl, list_volumes_impl, run_saved_search_impl,
    search_assets_impl, use_existing_catalog_impl,
};
use majestical_desktop::thumb_protocol;
use std::path::Path;
use std::sync::{Mutex, RwLock};

// `#[cfg(test)]` on the helpers below is not redundant despite this file
// already building with `--cfg test`: clippy's in-test detection for
// `allow-expect-in-tests` keys off `#[test]`/`#[cfg(test)]` directly on the
// item — the full rationale lives in `crates/cli/tests/common/mod.rs`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const SEEDED_ASSET: &str = "xxh3:0123456789abcdef0123456789abcdef";

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

#[cfg(test)]
fn cfg_for(dir: &Path) -> CatalogCfg {
    CatalogCfg {
        catalog: dir.join("cat"),
        machine_id: "gui-test".into(),
        author: "gui-test".into(),
    }
}

/// A catalog holding one asset on one volume: the same `Op::VolumeSeen` +
/// `Op::AssetSeen` literals the services search and volumes tests seed
/// with, so a query for "clip" finds it here for the same reason it does
/// there.
#[cfg(test)]
fn seeded_cfg(dir: &Path) -> CatalogCfg {
    let cfg = cfg_for(dir);
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

/// Adds `count` more assets whose names match the "clip" query, so a search
/// has more hits than one page.
#[cfg(test)]
fn seed_extra_clips(cfg: &CatalogCfg, count: usize) {
    let mut app = majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
        .expect("open");
    let ops = (0..count)
        .map(|n| majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId(format!("xxh3:{n:032x}")),
            volume: "vol1".into(),
            path: format!("clip-{n}.txt"),
            size: 5,
            mtime_ms: 1000,
        })
        .collect();
    app.emit(ops).expect("emit");
}

/// Appends an unparseable line to this machine's event segment, so every
/// later read of the log skips it and records the warning — the same
/// corrupt-log trigger `services_parity.rs` uses.
#[cfg(test)]
fn corrupt_the_event_log(cfg: &CatalogCfg) {
    let machine_dir = cfg.catalog.join("events").join(&cfg.machine_id);
    let segment = std::fs::read_dir(&machine_dir)
        .expect("machine events dir")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .expect("one events jsonl");
    let mut bytes = std::fs::read(&segment).expect("read segment");
    bytes.extend_from_slice(b"this is not json\n");
    std::fs::write(&segment, bytes).expect("re-write segment");
}

#[test]
fn search_finds_the_seeded_asset() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let outcome =
            search_assets_impl(&cfg, Some("clip".into()), None, Some(10)).expect("search");
        assert_eq!(outcome.count, 1);
        assert_eq!(outcome.results[0].name, "clip.txt");
        assert_eq!(outcome.results[0].asset, SEEDED_ASSET);
    });
}

#[test]
fn search_honors_the_limit_it_is_given() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let outcome = search_assets_impl(&cfg, Some("clip".into()), None, Some(0)).expect("search");
        assert_eq!(outcome.count, 0, "limit 0 must return nothing");
    });
}

/// A caller that omits `limit` gets one page of 50 — the impls apply that
/// default, so this pins the number the frontend can rely on without a
/// running webview.
#[test]
fn search_without_a_limit_returns_one_default_page() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        seed_extra_clips(&cfg, 60);
        let outcome = search_assets_impl(&cfg, Some("clip".into()), None, None).expect("search");
        assert_eq!(outcome.count, 50);
    });
}

#[test]
fn search_carries_notices_from_a_corrupt_log() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        corrupt_the_event_log(&cfg);
        let outcome =
            search_assets_impl(&cfg, Some("clip".into()), None, Some(10)).expect("search");
        assert!(
            outcome.notices.iter().any(|n| n.contains("skipped")),
            "search must hand the GUI its diagnostics: {:?}",
            outcome.notices
        );
    });
}

#[test]
fn run_saved_search_runs_the_saved_query() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let mut app =
            majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
                .expect("open");
        majestical_services::search::search(
            &mut app,
            &cfg.catalog,
            &majestical_services::search::SearchRequest {
                query: Some("clip".into()),
                limit: 10,
                saved: None,
                save: Some("clips".into()),
            },
        )
        .expect("save the search");
        drop(app);

        let outcome = run_saved_search_impl(&cfg, "clips".into(), Some(10)).expect("run saved");
        assert_eq!(outcome.count, 1);
        assert_eq!(outcome.results[0].name, "clip.txt");

        let listed = list_saved_searches_impl(&cfg).expect("list saved");
        assert_eq!(listed.saved.len(), 1);
        assert_eq!(listed.saved[0].name, "clips");
        assert_eq!(listed.saved[0].query, "clip");
    });
}

#[test]
fn run_saved_search_on_an_unknown_name_reports_the_name() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let Err(err) = run_saved_search_impl(&cfg, "nope".into(), Some(10)) else {
            panic!("an unknown saved search must fail");
        };
        assert!(err.message.contains("nope"), "{}", err.message);
    });
}

#[test]
fn get_asset_returns_detail_for_known_and_none_for_unknown() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let found = get_asset_impl(&cfg, SEEDED_ASSET).expect("get_asset");
        let detail = found.expect("the seeded asset is known");
        assert_eq!(detail.asset, SEEDED_ASSET);
        assert_eq!(detail.instances.len(), 1);
        assert_eq!(detail.instances[0].path, "clip.txt");

        let missing = get_asset_impl(&cfg, "xxh3:ffffffffffffffffffffffffffffffff")
            .expect("get_asset on an unknown id is a value, not an error");
        assert!(missing.is_none());
    });
}

#[test]
fn list_volumes_lists_the_seeded_volume() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let outcome = list_volumes_impl(&cfg).expect("list_volumes");
        let ids: Vec<&str> = outcome.volumes.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, vec!["vol1"]);
        assert_eq!(outcome.volumes[0].asset_count, 1);
    });
}

#[test]
fn list_saved_searches_carries_notices() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        corrupt_the_event_log(&cfg);
        let listed = list_saved_searches_impl(&cfg).expect("list_saved_searches");
        assert!(listed.saved.is_empty());
        assert!(
            listed.notices.iter().any(|n| n.contains("skipped")),
            "the wrapper must drain the app's notices: {:?}",
            listed.notices
        );
    });
}

#[test]
fn app_status_reports_no_catalog_then_missing_then_ready() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let none = app_status_impl(None);
        assert!(!none.catalog_ready);
        assert!(none.catalog_path.is_empty());

        let cfg = cfg_for(dir.path());
        assert!(!app_status_impl(Some(&cfg)).catalog_ready);
        initialize_catalog_impl(&cfg).expect("init");
        let ready = app_status_impl(Some(&cfg));
        assert!(ready.catalog_ready);
        assert_eq!(ready.catalog_path, cfg.catalog.display().to_string());
    });
}

#[test]
fn initialize_refuses_an_existing_catalog() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let err = initialize_catalog_impl(&cfg).expect_err("must refuse");
        assert!(err.message.contains("already exists"), "{}", err.message);
    });
}

#[test]
fn use_existing_refuses_a_root_with_no_catalog_and_names_the_remedy() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_for(dir.path());
        let err = use_existing_catalog_impl(&cfg).expect_err("must refuse");
        assert!(
            err.message.contains("maj catalog init"),
            "the service's own remedy must survive: {}",
            err.message
        );
    });
}

#[test]
fn adopting_a_catalog_persists_it_and_publishes_it_to_the_state() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        let state = AppState(RwLock::new(None));
        let catalog = dir.path().join("cat");

        let status = adopt_catalog(
            &config_dir,
            &state,
            catalog.clone(),
            initialize_catalog_impl,
        )
        .expect("initialize");
        assert!(status.catalog_ready);
        assert_eq!(
            majestical_desktop::config::load(&config_dir).catalog,
            Some(catalog.clone())
        );
        let published = state.0.read().expect("state").clone();
        assert_eq!(published.expect("published cfg").catalog, catalog);
    });
}

#[test]
fn a_refused_catalog_is_neither_persisted_nor_published() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().join("config");
        let state = AppState(RwLock::new(None));

        let err = adopt_catalog(
            &config_dir,
            &state,
            dir.path().join("nothing-here"),
            use_existing_catalog_impl,
        )
        .expect_err("no catalog there");
        assert!(err.message.contains("maj catalog init"), "{}", err.message);
        assert_eq!(majestical_desktop::config::load(&config_dir).catalog, None);
        assert!(state.0.read().expect("state").is_none());
    });
}

/// Plants a blob at the real `BlobStore` path, so the test computes the same
/// path the reader does rather than hand-deriving the layout (the same
/// reason `mcp_smoke.rs`'s resource tests depend on `majestical-index`).
#[cfg(test)]
fn plant_blob(cfg: &CatalogCfg, kind: &majestical_index::blob::Derivation<'_>, bytes: &[u8]) {
    let hex = majestical_index::blob::asset_hex(SEEDED_ASSET).expect("xxh3 asset id");
    let path = majestical_index::blob::BlobStore::new(&cfg.catalog).path_for(hex, kind);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, bytes).expect("plant blob");
}

#[cfg(test)]
fn content_type(response: &tauri::http::Response<Vec<u8>>) -> Option<&str> {
    response.headers().get("content-type")?.to_str().ok()
}

#[cfg(test)]
fn body_of(response: &tauri::http::Response<Vec<u8>>) -> String {
    String::from_utf8_lossy(response.body()).into_owned()
}

#[test]
fn thumb_route_serves_the_planted_webp_bytes() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::Thumb,
            b"RIFFfakewebp",
        );

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/thumb/{}", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 200);
        assert_eq!(content_type(&response), Some("image/webp"));
        assert_eq!(response.body(), b"RIFFfakewebp");
    });
}

#[test]
fn keyframes_route_serves_the_manifest_as_json() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeManifest {
                model_tag: majestical_index::model::MODEL_TAG,
            },
            br#"{"keyframes":[0,1500]}"#,
        );

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/keyframes/{}", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 200);
        assert_eq!(content_type(&response), Some("application/json"));
        assert_eq!(body_of(&response), r#"{"keyframes":[0,1500]}"#);
    });
}

#[test]
fn an_underived_thumb_is_a_404_naming_the_remedy() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/thumb/{}", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 404);
        assert!(
            body_of(&response).contains("maj index run --kinds thumbs"),
            "{}",
            body_of(&response)
        );
    });
}

#[test]
fn a_malformed_asset_id_is_a_400() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let response =
            thumb_protocol::handle(Some(&cfg), "thumb://localhost/thumb/..%2F..%2Fetc%2Fpasswd");
        assert_eq!(response.status(), 400);
        assert!(
            body_of(&response).contains("xxh3"),
            "{}",
            body_of(&response)
        );
    });
}

#[test]
fn an_unknown_route_is_a_404() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        let response = thumb_protocol::handle(Some(&cfg), "thumb://localhost/nope/whatever");
        assert_eq!(response.status(), 404);
    });
}

/// Every event this app writes is stamped with this identity, and an empty
/// machine id would make the event log's per-machine segment path
/// nonsensical — so the hostname fallback has to produce something.
#[test]
fn machine_identity_is_never_empty() {
    assert!(!majestical_desktop::commands::machine_identity().is_empty());
}

#[test]
fn no_catalog_selected_is_a_503_with_the_reason() {
    let response = thumb_protocol::handle(None, "thumb://localhost/thumb/whatever");
    assert_eq!(response.status(), 503);
    assert!(
        body_of(&response).contains("no catalog selected"),
        "{}",
        body_of(&response)
    );
}

#[test]
fn a_selected_but_missing_catalog_is_a_503_naming_the_remedy() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = cfg_for(dir.path());
        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/thumb/{}", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 503);
        assert!(
            body_of(&response).contains("maj catalog init"),
            "{}",
            body_of(&response)
        );
    });
}

/// What `convertFileSrc` does to an asset id on the frontend side: the `:`
/// in `xxh3:<hex>` arrives percent-encoded.
#[cfg(test)]
fn encoded(asset_id: &str) -> String {
    asset_id.replace(':', "%3A")
}

/// Every command error carries the whole `{err:#}` chain, which is where a
/// `ServiceError`'s remedy text lives — a head that kept only the top
/// message would drop the remedy on the floor.
#[test]
fn command_error_carries_the_full_display_chain() {
    let err: CommandError = anyhow::anyhow!("inner cause")
        .context("outer operation")
        .into();
    assert_eq!(err.message, "outer operation: inner cause");
}
