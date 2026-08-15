//! Direct tests over the command impls with real (fixture) catalogs — the
//! `#[tauri::command]` wrappers are one-liners over these, so driving the
//! impls tests everything but Tauri's own argument plumbing without needing
//! a webview.
//!
//! `MAJ_STATE_DIR` is process-global env, so every test takes `ENV_LOCK` and
//! points the var at its own tempdir — the same reason the CLI's suites set
//! it per child process; here the "process" is this test binary.
use majestical_desktop::commands::{
    AppState, CatalogCfg, CommandError, adopt_catalog, app_status_impl, browse_list_impl,
    browse_tree_impl, get_asset_impl, initialize_catalog_impl, list_saved_searches_impl,
    list_volumes_impl, run_saved_search_impl, search_assets_impl, use_existing_catalog_impl,
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

/// Initializes a fresh catalog at `dir` and emits `ops` into it — the
/// common preamble every from-scratch seeding helper below starts with.
#[cfg(test)]
fn fresh_cfg_with(dir: &Path, ops: Vec<majestical_core::event::Op>) -> CatalogCfg {
    let cfg = cfg_for(dir);
    initialize_catalog_impl(&cfg).expect("init");
    emit_into(&cfg, ops);
    cfg
}

/// Opens `cfg`'s catalog and emits `ops` into it — the shared tail every
/// seeding helper below ends with, whether building a fresh catalog
/// ([`fresh_cfg_with`]) or adding more assets to an existing one.
#[cfg(test)]
fn emit_into(cfg: &CatalogCfg, ops: Vec<majestical_core::event::Op>) {
    let mut app = majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
        .expect("open");
    app.emit(ops).expect("emit");
}

/// A catalog holding one asset on one volume: the same `Op::VolumeSeen` +
/// `Op::AssetSeen` literals the services search and volumes tests seed
/// with, so a query for "clip" finds it here for the same reason it does
/// there.
#[cfg(test)]
fn seeded_cfg(dir: &Path) -> CatalogCfg {
    fresh_cfg_with(
        dir,
        vec![
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
        ],
    )
}

/// A catalog holding one online volume with assets at `A/x.mov`, `A/B/y.jpg`,
/// and `C/z.pdf` — mirrors `majestical_services::browse::tests::seed_fixture`'s
/// shape (arrange helper copied from there), just built through a
/// `CatalogCfg`/`FsApp::open` the way this file's other tests do.
#[cfg(test)]
fn browse_seeded_cfg(dir: &Path) -> CatalogCfg {
    fresh_cfg_with(
        dir,
        vec![
            majestical_core::event::Op::VolumeSeen {
                volume: "vol1".into(),
                label: "vol1".into(),
            },
            majestical_core::event::Op::AssetSeen {
                asset: majestical_core::event::AssetId(
                    "xxh3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                ),
                volume: "vol1".into(),
                path: "A/x.mov".into(),
                size: 10,
                mtime_ms: 3000,
            },
            majestical_core::event::Op::AssetSeen {
                asset: majestical_core::event::AssetId(
                    "xxh3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                ),
                volume: "vol1".into(),
                path: "A/B/y.jpg".into(),
                size: 20,
                mtime_ms: 1000,
            },
            majestical_core::event::Op::AssetSeen {
                asset: majestical_core::event::AssetId(
                    "xxh3:cccccccccccccccccccccccccccccccc".into(),
                ),
                volume: "vol1".into(),
                path: "C/z.pdf".into(),
                size: 30,
                mtime_ms: 2000,
            },
        ],
    )
}

/// A catalog with `count` assets on one volume, all at the volume root —
/// enough to page through `browse_list`'s default limit. Same id/path
/// pattern as `seed_extra_clips`, just seeded from scratch rather than added
/// to an existing catalog.
#[cfg(test)]
fn browse_cfg_with_assets(dir: &Path, count: usize) -> CatalogCfg {
    let mut ops = vec![majestical_core::event::Op::VolumeSeen {
        volume: "vol1".into(),
        label: "vol1".into(),
    }];
    ops.extend((0..count).map(|n| majestical_core::event::Op::AssetSeen {
        asset: majestical_core::event::AssetId(format!("xxh3:{n:032x}")),
        volume: "vol1".into(),
        path: format!("item-{n}.txt"),
        size: 5,
        mtime_ms: 1000,
    }));
    fresh_cfg_with(dir, ops)
}

/// Adds `count` more assets whose names match the "clip" query, so a search
/// has more hits than one page.
#[cfg(test)]
fn seed_extra_clips(cfg: &CatalogCfg, count: usize) {
    let ops = (0..count)
        .map(|n| majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId(format!("xxh3:{n:032x}")),
            volume: "vol1".into(),
            path: format!("clip-{n}.txt"),
            size: 5,
            mtime_ms: 1000,
        })
        .collect();
    emit_into(cfg, ops);
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
fn browse_tree_computes_exact_recursive_counts() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_seeded_cfg(dir.path());
        let out = browse_tree_impl(&cfg).expect("browse_tree");
        assert_eq!(out.volumes.len(), 1);
        let v = &out.volumes[0];
        let folder = |path: &str| {
            v.folders
                .iter()
                .find(|f| f.path == path)
                .unwrap_or_else(|| panic!("no folder '{path}' in {:?}", v.folders))
        };
        assert_eq!(v.folders.len(), 4, "'', A, A/B, C");
        assert_eq!(folder("").recursive_count, 3);
        assert_eq!(folder("A").recursive_count, 2);
        assert_eq!(folder("A/B").recursive_count, 1);
        assert_eq!(folder("C").recursive_count, 1);
    });
}

#[test]
fn browse_list_returns_rows_with_size_mtime_ms_and_kind() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_seeded_cfg(dir.path());
        let out = browse_list_impl(
            &cfg,
            "vol1".into(),
            None,
            None,
            Some("name".into()),
            None,
            None,
            None,
        )
        .expect("browse_list");
        assert_eq!(out.count, 3);
        assert_eq!(out.folder_count, 3, "A, A/B, C");
        let names: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["x.mov", "y.jpg", "z.pdf"]);
        let x = &out.results[0];
        assert_eq!(x.size, Some(10), "the instance's own size");
        assert_eq!(x.mtime_ms, Some(3000), "the instance's own mtime");
        assert_eq!(x.kind.as_deref(), Some("video"));
    });
}

/// A caller that omits `limit` gets exactly one page of
/// `majestical_services::browse::DEFAULT_LIMIT` rows, not the whole catalog
/// — the same strong shape as `search_without_a_limit_returns_one_default_page`:
/// seeding one more asset than the default page holds means a mutated
/// default (e.g. `unwrap_or(1)`, or any other wrong constant) fails this
/// test, where a catalog with only one asset couldn't tell the difference.
#[test]
fn browse_list_without_a_limit_uses_the_browse_default() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let total = majestical_services::browse::DEFAULT_LIMIT + 1;
        let cfg = browse_cfg_with_assets(dir.path(), total);
        let out = browse_list_impl(&cfg, "vol1".into(), None, None, None, None, None, None)
            .expect("browse_list");
        assert_eq!(out.count, total as u64, "count is pre-pagination");
        assert_eq!(
            out.results.len(),
            majestical_services::browse::DEFAULT_LIMIT,
            "one default-sized page, not the whole catalog"
        );
    });
}

/// One call giving every argument (but `sort`, left at its default) a value
/// that differs from its default — the file's only positional 7-arg
/// forward, so a transposed pair (`path`/`kind` are both `Option<String>`
/// and could swap without a type error) would otherwise compile clean and
/// only misbehave at runtime. Pins the exact outcome for this exact
/// combination against `browse_seeded_cfg`'s fixed catalog: `path("A")` +
/// `flatten(false)` scope the match down to `A/x.mov` alone (`A/B/y.jpg`
/// sits one level deeper, `C/z.pdf` is a different folder entirely),
/// `kind("video")` still lets it through, and `offset(1)` then skips past
/// that one match — a swapped `path`/`kind` would instead scope to a
/// nonexistent folder or reject `"A"` as an unknown kind, either visible
/// here as a wrong `count`/`folder_count` or an `Err` where this expects
/// `Ok`.
#[test]
fn browse_list_wires_every_argument_to_its_own_slot() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_seeded_cfg(dir.path());
        let out = browse_list_impl(
            &cfg,
            "vol1".into(),
            Some("A".into()),
            Some(false),
            None,
            Some("video".into()),
            Some(1),
            Some(1),
        )
        .expect("browse_list");
        assert_eq!(
            out.count, 1,
            "only A/x.mov sits directly in A and is a video"
        );
        assert_eq!(out.folder_count, 1, "just A");
        assert!(
            out.results.is_empty(),
            "the one match is skipped by offset 1: {:?}",
            out.results
        );
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

/// The real webview request shape: `convertFileSrc` percent-encodes the
/// WHOLE path (`thumb%2Fxxh3%3A...`), not just the asset id's `:`
/// (`thumb/xxh3%3A...`, [`encoded`]'s shape) — a version of `path_of` that
/// decoded only after matching a literal `/thumb/` prefix would 404 every
/// request a packaged app actually sends. Regression test for the bug that
/// shipped in phase 7B: this exact request 404'd before `path_of` decoded
/// the whole path up front.
#[test]
fn thumb_route_serves_the_planted_webp_bytes_when_the_whole_path_is_percent_encoded() {
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
            &format!(
                "thumb://localhost/{}",
                fully_encoded(&format!("thumb/{SEEDED_ASSET}"))
            ),
        );
        assert_eq!(response.status(), 200, "{}", body_of(&response));
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

/// The real webview request shape for `keyframes` — see
/// `thumb_route_serves_the_planted_webp_bytes_when_the_whole_path_is_percent_encoded`.
#[test]
fn keyframes_route_serves_the_manifest_as_json_when_the_whole_path_is_percent_encoded() {
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
            &format!(
                "thumb://localhost/{}",
                fully_encoded(&format!("keyframes/{SEEDED_ASSET}"))
            ),
        );
        assert_eq!(response.status(), 200, "{}", body_of(&response));
        assert_eq!(content_type(&response), Some("application/json"));
        assert_eq!(body_of(&response), r#"{"keyframes":[0,1500]}"#);
    });
}

/// Plants a keyframe manifest with two timestamps and an extracted image
/// blob for each — `keyframe/{asset}/{index}` selects by position, so this
/// pins that index 0 and index 1 each serve the byte content planted at
/// THEIR OWN timestamp's blob, not the other one's.
#[test]
fn keyframe_route_serves_the_extracted_image_at_each_index() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeManifest {
                model_tag: majestical_index::model::MODEL_TAG,
            },
            br#"{"model_tag":"m","detected":2,"timestamps":[1500,4500]}"#,
        );
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeImage {
                model_tag: majestical_index::model::MODEL_TAG,
                timestamp_ms: 1500,
            },
            b"frame-0",
        );
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeImage {
                model_tag: majestical_index::model::MODEL_TAG,
                timestamp_ms: 4500,
            },
            b"frame-1",
        );

        let first = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/keyframe/{}/0", encoded(SEEDED_ASSET)),
        );
        assert_eq!(first.status(), 200);
        assert_eq!(content_type(&first), Some("image/webp"));
        assert_eq!(first.body(), b"frame-0");

        let second = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/keyframe/{}/1", encoded(SEEDED_ASSET)),
        );
        assert_eq!(second.status(), 200);
        assert_eq!(content_type(&second), Some("image/webp"));
        assert_eq!(second.body(), b"frame-1");
    });
}

/// The real webview request shape for `keyframe/<asset_id>/<index>` — see
/// `thumb_route_serves_the_planted_webp_bytes_when_the_whole_path_is_percent_encoded`.
/// Also proves the two-segment split still lands on the right boundary once
/// the WHOLE path (asset id, `/`, and index alike) has gone through one
/// decode pass: `%2F` between the asset id and `0` decodes back to the `/`
/// `keyframe_asset_and_index` splits on.
#[test]
fn keyframe_route_serves_the_extracted_image_when_the_whole_path_is_percent_encoded() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeManifest {
                model_tag: majestical_index::model::MODEL_TAG,
            },
            br#"{"model_tag":"m","detected":2,"timestamps":[1500,4500]}"#,
        );
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeImage {
                model_tag: majestical_index::model::MODEL_TAG,
                timestamp_ms: 1500,
            },
            b"frame-0",
        );

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!(
                "thumb://localhost/{}",
                fully_encoded(&format!("keyframe/{SEEDED_ASSET}/0"))
            ),
        );
        assert_eq!(response.status(), 200, "{}", body_of(&response));
        assert_eq!(content_type(&response), Some("image/webp"));
        assert_eq!(response.body(), b"frame-0");
    });
}

/// An index the manifest has no timestamp at is a 404, never a panic.
#[test]
fn keyframe_route_out_of_range_index_is_a_404() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());
        plant_blob(
            &cfg,
            &majestical_index::blob::Derivation::KeyframeManifest {
                model_tag: majestical_index::model::MODEL_TAG,
            },
            br#"{"model_tag":"m","detected":1,"timestamps":[1500]}"#,
        );

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/keyframe/{}/1", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 404);
        assert!(
            body_of(&response).contains("no keyframe image at index 1"),
            "{}",
            body_of(&response)
        );
    });
}

/// A malformed index (not an integer) is a 404, never a panic, and reached
/// without ever joining a path — the manifest here is never even planted, so
/// a version that read the manifest before validating the index would 404
/// with the WRONG reason (`NotDerived`, not `MalformedKeyframeIndex`).
#[test]
fn keyframe_route_malformed_index_is_a_404() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!("thumb://localhost/keyframe/{}/x", encoded(SEEDED_ASSET)),
        );
        assert_eq!(response.status(), 404);
        assert!(
            body_of(&response).contains("not a valid keyframe index"),
            "{}",
            body_of(&response)
        );
    });
}

/// The same traversal-payload guard `a_malformed_asset_id_is_a_400` pins for
/// `/thumb/`, mirrored for `/keyframe/`: an asset id containing a raw `/`
/// (here, unencoded on purpose) must not be mis-split into a bogus
/// asset-id/index pair and must never reach a path join. Pins the REASON,
/// not just a non-200 status: the payload must be reported as an invalid
/// ASSET ID (400, same as `/thumb/` and `/keyframes/` give for the same
/// payload) — not misattributed to the index, which a version that always
/// routed through `read_keyframe_image` would report instead (its own
/// integer parse runs first and fails on the mis-split remainder, a 404
/// naming "keyframe index" rather than "asset id").
#[test]
fn keyframe_route_traversal_payload_in_asset_id_is_a_clean_failure() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());

        let response = thumb_protocol::handle(
            Some(&cfg),
            "thumb://localhost/keyframe/xxh3:../../../etc/passwd/0",
        );
        assert_eq!(
            response.status(),
            400,
            "a malformed asset id is the caller's mistake, same as /thumb/ and /keyframes/: {}",
            body_of(&response)
        );
        assert!(
            body_of(&response).contains("not a valid asset id"),
            "must report the asset id as the failure, not the index: {}",
            body_of(&response)
        );
    });
}

/// The same guard, composed with the whole-path decode: a traversal
/// payload's `/`s arrive as `%2F` (the real wire shape — see
/// `thumb_route_serves_the_planted_webp_bytes_when_the_whole_path_is_percent_encoded`),
/// decode back to real `/`s in `path_of`'s single decode pass, and
/// `is_well_formed_asset_id` still rejects the result exactly as it does
/// the literal-slash form above — the decode step doesn't weaken the guard,
/// it just lets a REAL request ever reach it.
#[test]
fn keyframe_route_traversal_payload_arriving_fully_percent_encoded_is_a_clean_failure() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path());

        let response = thumb_protocol::handle(
            Some(&cfg),
            &format!(
                "thumb://localhost/{}",
                fully_encoded("keyframe/xxh3:../../../etc/passwd/0")
            ),
        );
        assert_eq!(
            response.status(),
            400,
            "a malformed asset id is the caller's mistake, same as the literal-slash form: {}",
            body_of(&response)
        );
        assert!(
            body_of(&response).contains("not a valid asset id"),
            "must report the asset id as the failure, not the index: {}",
            body_of(&response)
        );
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
/// in `xxh3:<hex>` arrives percent-encoded. Historical test convention —
/// only the asset id's own `:` is encoded, the surrounding `/`s left
/// literal — which is NOT the real wire shape (see [`fully_encoded`]), but
/// decodes to the identical path either way, so both stay valid inputs.
#[cfg(test)]
fn encoded(asset_id: &str) -> String {
    asset_id.replace(':', "%3A")
}

/// What `convertFileSrc(filePath, protocol)` actually does, per Tauri
/// 2.11.5's `core.js`: `encodeURIComponent` runs over the WHOLE `filePath`
/// argument, not just an asset id inside it, so every `/` in `path` is
/// `%2F` right along with any `:` becoming `%3A` — the real request shape a
/// packaged app's webview sends, and what `Inspector.test.ts`'s vitest
/// assertions compute for the same inputs via `mockConvertFileSrc`. A plain
/// two-step replace is exact for every path this module builds (only `:`
/// and `/` ever appear in one), without pulling in a JS-compatible
/// percent-encoding crate feature just for tests.
#[cfg(test)]
fn fully_encoded(path: &str) -> String {
    path.replace(':', "%3A").replace('/', "%2F")
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

#[test]
fn command_error_splits_a_notices_carrier() {
    let err = majestical_services::error::ServiceError::WithNotices {
        notices: vec!["warned".to_string()],
        source: Box::new(majestical_services::error::ServiceError::NoCatalog {
            root: std::path::PathBuf::from("/nowhere"),
        }),
    };
    let converted = CommandError::from(err);
    assert_eq!(converted.notices, vec!["warned"]);
    assert!(converted.message.contains("no catalog"));
    assert!(
        !converted.message.contains("diagnostic(s) were collected"),
        "the carrier's own label must never reach a user"
    );
    // `CommandError::new` is private to `commands.rs`; this integration test
    // crate reaches the same empty-notices shape through `From` instead.
    let plain: CommandError = anyhow::anyhow!("plain").into();
    let wire = serde_json::to_value(plain).expect("serialize");
    assert!(
        wire.get("notices").is_none(),
        "empty notices must be absent from the wire, not []"
    );
}
