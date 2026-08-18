//! Direct tests over the command impls with real (fixture) catalogs — the
//! `#[tauri::command]` wrappers are one-liners over these, so driving the
//! impls tests everything but Tauri's own argument plumbing without needing
//! a webview.
//!
//! `MAJ_STATE_DIR` is process-global env, so every test takes `ENV_LOCK` and
//! points the var at its own tempdir — the same reason the CLI's suites set
//! it per child process; here the "process" is this test binary.
use majestical_desktop::commands::{
    AppState, CatalogCfg, CommandError, add_para_node_impl, adopt_catalog, app_status_impl,
    archive_node_impl, assign_tags_impl, browse_list_impl, browse_tree_impl, file_assets_impl,
    get_asset_impl, initialize_catalog_impl, list_mounted_roots_impl, list_para_impl,
    list_saved_searches_impl, list_tags_impl, list_volumes_impl, merge_tags_impl,
    rename_para_node_impl, rename_tag_impl, run_saved_search_impl, search_assets_impl,
    use_existing_catalog_impl,
};
use majestical_desktop::ingest::{
    DEFAULT_INGEST_TEMPLATE, FinishedIngest, IngestJob, IngestProgress, IngestState, ProgressSink,
    StartIngest, cancel_ingest_impl, ingest_state_impl, list_unfinished_ingests_impl,
    plan_ingest_impl, start_ingest_impl,
};
use majestical_desktop::thumb_protocol;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

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
        asset: majestical_core::event::AssetId(asset_at(n)),
        volume: "vol1".into(),
        path: format!("item-{n}.txt"),
        size: 5,
        mtime_ms: 1000,
    }));
    fresh_cfg_with(dir, ops)
}

/// The asset id [`browse_cfg_with_assets`] mints for index `n` — computed
/// the same way rather than hand-typed, so a test asking for asset 0 or 1
/// can't silently miscount the hex padding.
#[cfg(test)]
fn asset_at(n: usize) -> String {
    format!("xxh3:{n:032x}")
}

/// Opens `cfg`'s catalog and calls `tags::tag_add` for each `(asset, tag)`
/// pair — the shared preamble the tag command tests below all used to
/// repeat by hand (open, `tag_add`, `tag_add`, `drop(app)`).
#[cfg(test)]
fn tag_assets(cfg: &CatalogCfg, pairs: &[(&str, &str)]) {
    let mut app = majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
        .expect("open");
    for (asset, tag) in pairs {
        majestical_services::tags::tag_add(&mut app, asset, tag).expect("tag_add");
    }
}

/// Adds `count` more assets whose names match the "clip" query, so a search
/// has more hits than one page.
#[cfg(test)]
fn seed_extra_clips(cfg: &CatalogCfg, count: usize) {
    let ops = (0..count)
        .map(|n| majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId(asset_at(n)),
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

/// `limit` and `offset` are both `Option<usize>` and adjacent in the
/// parameter list, so a transposed pair compiles clean; the test above can't
/// see it, because it passes `Some(1)` for both. Six same-shaped assets sorted
/// by name make the window itself the assertion: `limit(2)` + `offset(3)` is
/// `item-3`/`item-4`, where the swap (`limit(3)` + `offset(2)`) is a
/// three-row page starting at `item-2`.
#[test]
fn browse_list_paginates_with_limit_and_offset_in_their_own_slots() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 6);
        let out = browse_list_impl(
            &cfg,
            "vol1".into(),
            None,
            None,
            Some("name".into()),
            None,
            Some(2),
            Some(3),
        )
        .expect("browse_list");
        assert_eq!(out.count, 6, "count is pre-pagination");
        let names: Vec<&str> = out.results.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["item-3.txt", "item-4.txt"]);
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

#[test]
fn list_tags_reports_every_live_tag_with_its_asset_count() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 2);
        let (a0, a1) = (asset_at(0), asset_at(1));
        tag_assets(&cfg, &[(a0.as_str(), "x"), (a1.as_str(), "x")]);

        let outcome = list_tags_impl(&cfg).expect("list_tags");
        assert_eq!(outcome.tags.len(), 1);
        assert_eq!(outcome.tags[0].tag, "x");
        assert_eq!(outcome.tags[0].count, 2);
    });
}

#[test]
fn rename_tag_moves_the_vocabulary_a_follow_up_list_tags_proves_it() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 1);
        tag_assets(&cfg, &[(asset_at(0).as_str(), "old")]);

        let outcome = rename_tag_impl(&cfg, "old", "new").expect("rename_tag");
        assert_eq!(outcome.from, "old");
        assert_eq!(outcome.to, "new");
        assert_eq!(outcome.rewritten, 1);

        let listed = list_tags_impl(&cfg).expect("list_tags");
        let names: Vec<&str> = listed.tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(
            names,
            vec!["new"],
            "the old name must be gone from the vocabulary"
        );
    });
}

#[test]
fn merge_tags_folds_the_source_into_the_target() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 2);
        let (a0, a1) = (asset_at(0), asset_at(1));
        tag_assets(&cfg, &[(a0.as_str(), "a"), (a1.as_str(), "b")]);

        let outcome = merge_tags_impl(&cfg, "a", "b").expect("merge_tags");
        assert_eq!(outcome.from, "a");
        assert_eq!(outcome.to, "b");
        assert_eq!(outcome.rewritten, 1);

        let listed = list_tags_impl(&cfg).expect("list_tags");
        let names: Vec<&str> = listed.tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(names, vec!["b"]);
        assert_eq!(listed.tags[0].count, 2);
    });
}

#[test]
fn assign_tags_applies_every_pair_and_reports_an_unknown_asset() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 1);
        let outcome = assign_tags_impl(
            &cfg,
            &[asset_at(0), "xxh3:never-scanned".to_string()],
            &["x".to_string(), "y".to_string()],
        )
        .expect("assign_tags");
        assert_eq!(
            outcome.applied, 2,
            "two tags applied to the one known asset"
        );
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.failed[0].asset, "xxh3:never-scanned");
        assert!(!outcome.failed[0].reason.is_empty());
    });
}

/// The all-failed guard lives inside `tags_assign` itself; this pins that
/// `assign_tags_impl` inherits it rather than silently reporting `applied:
/// 0` — the `CommandError` must carry the joined per-asset reasons.
#[test]
fn assign_tags_when_every_asset_fails_is_a_command_error_with_the_reasons() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 0);
        let err = assign_tags_impl(
            &cfg,
            &[
                "xxh3:never-scanned-1".to_string(),
                "xxh3:never-scanned-2".to_string(),
            ],
            &["x".to_string()],
        )
        .expect_err("every asset must fail");
        assert!(
            err.message.contains("every requested asset"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("xxh3:never-scanned-1"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("xxh3:never-scanned-2"),
            "{}",
            err.message
        );
    });
}

#[test]
fn file_assets_files_the_known_asset_under_the_node() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 1);
        let node = add_para_node_impl(&cfg, "project", "client-x").expect("add node");

        let outcome = file_assets_impl(&cfg, &[asset_at(0)], &node).expect("file_assets");
        assert_eq!(outcome.applied, 1);
        assert!(outcome.failed.is_empty());
    });
}

#[test]
fn file_assets_into_an_unknown_node_is_a_command_error() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 1);
        let err = file_assets_impl(&cfg, &[asset_at(0)], "project/nope")
            .expect_err("an unknown node must fail");
        assert!(
            err.message.contains("no active PARA node"),
            "{}",
            err.message
        );
    });
}

#[test]
fn para_add_rename_list_round_trips() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 0);
        let node = add_para_node_impl(&cfg, "project", "client-x").expect("add node");
        assert!(!node.is_empty());

        rename_para_node_impl(&cfg, &node, "client-y").expect("rename node");

        let listed = list_para_impl(&cfg).expect("list_para");
        assert_eq!(listed.nodes.len(), 1);
        assert_eq!(listed.nodes[0].id, node);
        assert_eq!(listed.nodes[0].name, "client-y");
        assert_eq!(listed.nodes[0].kind, "project");
        assert!(!listed.nodes[0].archived);
    });
}

/// `dry_run: true` must plan the move without touching disk — the source
/// directory has to survive the call, which the next assertion checks
/// directly rather than trusting the outcome's own `status` field.
#[test]
fn archive_dry_run_plans_without_moving_then_execute_moves() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 0);
        let node = add_para_node_impl(&cfg, "project", "client-x").expect("add node");
        let materialized = tempfile::tempdir().expect("tempdir");
        let source = materialized.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&source).expect("mkdir");
        std::fs::write(source.join("a.txt"), b"hello").expect("write");
        let roots = vec![materialized.path().display().to_string()];

        let preview = archive_node_impl(&cfg, &node, &roots, true).expect("dry-run archive");
        assert!(!preview.executed);
        assert_eq!(preview.moves.len(), 1);
        assert!(
            source.is_dir(),
            "a dry-run preview must not move the source directory"
        );

        let executed = archive_node_impl(&cfg, &node, &roots, false).expect("real archive");
        assert!(executed.executed);
        assert_eq!(executed.moves.len(), 1);
        assert!(
            !source.exists(),
            "the real run must move the source directory"
        );
        let archived = materialized.path().join("Archives").join("client-x");
        assert!(archived.join("a.txt").is_file());
    });
}

/// A multi-root run failing on the SECOND root must not silently drop the
/// FIRST root's already-completed move: `ServiceError::ParaArchivePartial`
/// carries it, and `archive_node_impl` must fold it into the
/// `CommandError`'s `notices` (the wire has no separate `moves` field) —
/// quality-review follow-up to Task 14, where the blanket `From<E>` impl
/// swallowed it because it has no arm for that variant.
#[test]
fn archive_partial_failure_reports_the_completed_move_via_notices() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = browse_cfg_with_assets(dir.path(), 0);
        let node = add_para_node_impl(&cfg, "project", "client-x").expect("add node");

        let root1 = tempfile::tempdir().expect("tempdir");
        let root1_source = root1.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&root1_source).expect("mkdir");
        std::fs::write(root1_source.join("a.txt"), b"hello").expect("write");
        // root2 has no materialized directory at all, so its move fails —
        // root1's move must already have happened by the time that error
        // is raised.
        let root2 = tempfile::tempdir().expect("tempdir");
        let roots = vec![
            root1.path().display().to_string(),
            root2.path().display().to_string(),
        ];

        let err = archive_node_impl(&cfg, &node, &roots, false)
            .expect_err("root2's missing source must fail the call");
        assert!(
            err.message.contains("does not exist"),
            "the failure must name root2's missing source: {}",
            err.message
        );
        let root1_archived = root1.path().join("Archives").join("client-x");
        let expected_notice = format!(
            "moved {} -> {}",
            root1_source.display(),
            root1_archived.display()
        );
        assert!(
            err.notices.contains(&expected_notice),
            "notices must carry root1's completed move: {:?}",
            err.notices
        );
        assert!(
            root1_archived.join("a.txt").is_file(),
            "root1's move must have really happened on disk, not just been reported"
        );
        assert!(
            !root1_source.exists(),
            "root1's source directory must be gone after the real move"
        );
    });
}

/// The archive modal's candidate roots. "/" is mounted on any machine this
/// suite runs on, so the list is never empty, and every row has to carry a
/// path that really is a directory — the modal hands these straight to
/// `archive_node` as the roots to move a materialized node from.
#[test]
fn list_mounted_roots_reports_the_root_volume_as_a_real_directory() {
    let roots = list_mounted_roots_impl();

    assert!(
        roots.iter().any(|root| root.path == "/"),
        "the root volume is always mounted: {roots:?}"
    );
    for root in &roots {
        assert!(!root.volume.is_empty(), "{root:?} has no volume id");
        assert!(!root.label.is_empty(), "{root:?} has no label");
        assert!(
            Path::new(&root.path).is_dir(),
            "{root:?} does not name a mounted directory"
        );
    }
}

/// The root volume's label is `volume_identity`'s own `ROOT_LABEL`, not the
/// empty string `"/".file_name()` yields — the modal prints this label
/// beside the path, and a blank one would read as a missing drive.
#[test]
fn the_root_volumes_label_is_the_shared_root_label() {
    let roots = list_mounted_roots_impl();

    let root = roots
        .iter()
        .find(|root| root.path == "/")
        .expect("the root volume");
    assert_eq!(root.label, majestical_services::volume_identity::ROOT_LABEL);
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

// --- ingest -----------------------------------------------------------
//
// The run lives in the backend, on its own thread, so these tests drive the
// same three-part seam the commands do: a `CatalogCfg`, an `IngestState`
// the job publishes itself into, and a `ProgressSink` standing in for the
// webview's event bridge.

/// A catalog with one active PARA node, a source directory holding
/// `files`, one destination root, and an empty ingest slot.
#[cfg(test)]
struct IngestFixture {
    cfg: CatalogCfg,
    source: tempfile::TempDir,
    dest: tempfile::TempDir,
    state: Arc<IngestState>,
}

#[cfg(test)]
fn ingest_fixture(dir: &Path, files: &[(String, Vec<u8>)]) -> IngestFixture {
    let cfg = cfg_for(dir);
    initialize_catalog_impl(&cfg).expect("init");
    add_para_node_impl(&cfg, "project", "client-x").expect("para add");
    let source = tempfile::tempdir().expect("source dir");
    for (name, bytes) in files {
        std::fs::write(source.path().join(name), bytes).expect("write source file");
    }
    IngestFixture {
        cfg,
        source,
        dest: tempfile::tempdir().expect("dest dir"),
        state: Arc::new(IngestState(RwLock::new(None))),
    }
}

/// `count` one-line files, each with distinct bytes so nothing dedupes
/// against anything else.
#[cfg(test)]
fn tiny_files(count: usize) -> Vec<(String, Vec<u8>)> {
    (0..count)
        .map(|n| {
            (
                format!("clip-{n}.mov"),
                format!("bytes for {n}").into_bytes(),
            )
        })
        .collect()
}

#[cfg(test)]
impl IngestFixture {
    /// A start request for this fixture's source and destination. The
    /// template is a literal so the rendered subdir is stable in
    /// assertions — the real default (`{date}/{source-label}`) renders
    /// today's date and the source volume's label.
    fn request(&self, resume: Option<String>) -> StartIngest {
        StartIngest {
            source: self.source.path().to_path_buf(),
            dests: vec![self.dest.path().to_path_buf()],
            para: "project/client-x".to_string(),
            template: Some("raw".to_string()),
            resume,
        }
    }

    /// Blocks until the job's thread publishes its outcome. Polls rather
    /// than joins: the thread handle is the backend's, not the caller's —
    /// `ingest_state` is the only way the GUI learns a run ended, so it is
    /// the only way this suite learns it either.
    fn await_finished(&self) -> Arc<FinishedIngest> {
        for _ in 0..2000 {
            if let Some(finished) = ingest_state_impl(&self.state).finished {
                return finished;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the ingest run never finished");
    }

    /// The run this fixture just finished, or a panic naming the failure.
    fn finished_run(&self) -> Arc<FinishedIngest> {
        let finished = self.await_finished();
        if let FinishedIngest::Failed { error } = finished.as_ref() {
            panic!("the run failed: {}", error.message);
        }
        finished
    }
}

/// The placed count of a finished run.
#[cfg(test)]
fn placed_count(finished: &FinishedIngest) -> usize {
    match finished {
        FinishedIngest::Done { run } => run.outcome.placed.len(),
        FinishedIngest::Failed { error } => panic!("the run failed: {}", error.message),
    }
}

/// A sink that records every forwarded notification, the stand-in for the
/// webview's `ingest-progress` listener.
#[cfg(test)]
fn collecting_sink(into: &Arc<Mutex<Vec<IngestProgress>>>) -> ProgressSink {
    let into = Arc::clone(into);
    Arc::new(move |progress| {
        into.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(progress);
    })
}

#[cfg(test)]
fn collected(events: &Arc<Mutex<Vec<IngestProgress>>>) -> Vec<IngestProgress> {
    events
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The run journal `run_id` must have written under this machine's state
/// directory — the on-disk proof that the id the command returned is the
/// id the run really used.
#[cfg(test)]
fn journal_path(cfg: &CatalogCfg, run_id: &str) -> std::path::PathBuf {
    let notices = majestical_services::notices::Notices::new();
    majestical_services::state_dir::catalog_paths(&cfg.catalog, &notices)
        .expect("catalog paths")
        .runs_dir
        .join(format!("{run_id}.jsonl"))
}

#[test]
fn plan_ingest_counts_every_file_it_walked() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(3));
        let planned = plan_ingest_impl(
            &fixture.cfg,
            fixture.source.path(),
            "project/client-x",
            Some("raw".to_string()),
        )
        .expect("plan_ingest");
        assert_eq!(planned.plan.files.len(), 3);
        assert_eq!(planned.subdir, "Projects/client-x/raw");
        assert!(!planned.node_id.is_empty());
    });
}

/// The default template is the one every other head applies when its own
/// flag is omitted, so an unconfigured GUI ingest lands where an
/// unconfigured `maj ingest` would.
#[test]
fn plan_ingest_without_a_template_uses_the_shared_default() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let planned = plan_ingest_impl(
            &fixture.cfg,
            fixture.source.path(),
            "project/client-x",
            None,
        )
        .expect("plan_ingest");
        let with_default = plan_ingest_impl(
            &fixture.cfg,
            fixture.source.path(),
            "project/client-x",
            Some(DEFAULT_INGEST_TEMPLATE.to_string()),
        )
        .expect("plan_ingest");
        assert_eq!(planned.subdir, with_default.subdir);
        assert_ne!(planned.subdir, "Projects/client-x/");
    });
}

#[test]
fn plan_ingest_of_an_unknown_para_target_names_it() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let Err(err) = plan_ingest_impl(&fixture.cfg, fixture.source.path(), "project/nope", None)
        else {
            panic!("an unknown PARA target must fail");
        };
        assert!(err.message.contains("nope"), "{}", err.message);
    });
}

#[test]
fn start_ingest_places_every_file_and_forwards_the_run_id_with_each_event() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(2));
        let events = Arc::new(Mutex::new(Vec::new()));
        let run_id = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            collecting_sink(&events),
        )
        .expect("start_ingest");

        assert_eq!(run_id.len(), 26, "a ULID run id: {run_id}");
        assert!(
            run_id
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()),
            "a ULID run id: {run_id}"
        );
        let finished = fixture.finished_run();
        assert_eq!(placed_count(&finished), 2);
        assert!(
            journal_path(&fixture.cfg, &run_id).is_file(),
            "the returned id must be the id the run journaled under"
        );

        let events = collected(&events);
        assert!(
            events.iter().all(|p| p.run_id == run_id),
            "every forwarded event carries the run it belongs to: {events:?}"
        );
        let kinds: Vec<&majestical_ingest::engine::ProgressEvent> =
            events.iter().map(|p| &p.event).collect();
        assert!(
            matches!(
                kinds.first(),
                Some(majestical_ingest::engine::ProgressEvent::RunStarted { files_total: 2, .. })
            ),
            "{kinds:?}"
        );
        assert!(
            matches!(
                kinds.last(),
                Some(majestical_ingest::engine::ProgressEvent::RunStopped { cancelled: false })
            ),
            "{kinds:?}"
        );
        let mut placed: Vec<&str> = kinds
            .iter()
            .filter_map(|event| match event {
                majestical_ingest::engine::ProgressEvent::FilePlaced { rel } => Some(rel.as_str()),
                _ => None,
            })
            .collect();
        placed.sort_unstable();
        assert_eq!(placed, vec!["clip-0.mov", "clip-1.mov"]);
    });
}

/// The engine emits one `BytesCopied` per 1 MiB buffer and leaves
/// coalescing to whoever renders them. This head coalesces: a file copied
/// in four chunks inside one throttle window forwards its first and its
/// last, not all four.
#[test]
fn start_ingest_coalesces_the_bytes_copied_firehose() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let size = 4 * 1024 * 1024;
        let fixture = ingest_fixture(dir.path(), &[("big.mov".to_string(), vec![7u8; size])]);
        let events = Arc::new(Mutex::new(Vec::new()));
        start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            collecting_sink(&events),
        )
        .expect("start_ingest");
        assert_eq!(placed_count(&fixture.finished_run()), 1);

        let bytes: Vec<u64> = collected(&events)
            .iter()
            .filter_map(|p| match p.event {
                majestical_ingest::engine::ProgressEvent::BytesCopied { bytes_done, .. } => {
                    Some(bytes_done)
                }
                _ => None,
            })
            .collect();
        assert!(
            bytes.len() < 4,
            "four 1 MiB chunks copied inside one throttle window must not forward four \
             events: {bytes:?}"
        );
        assert_eq!(
            bytes.last().copied(),
            Some(size as u64),
            "the final byte count always lands, however tight the window: {bytes:?}"
        );
    });
}

/// One job at a time: a second start while a run is live is refused, and
/// the refusal names the run holding the slot so the surface can say which.
#[test]
fn start_ingest_refuses_a_second_run_and_names_the_live_one() {
    let refusal = refused_start_message(Some("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
    assert!(refusal.contains("01ARZ3NDEKTSV4RRFFQ69G5FAV"), "{refusal}");
}

/// The same refusal in the window before the run has named itself — a job
/// holds the slot from the moment `start_ingest` claims it, so the check
/// has to answer with something honest rather than skip a nameless run.
#[test]
fn start_ingest_refuses_a_run_that_has_not_named_itself_yet() {
    let refusal = refused_start_message(None);
    assert!(
        refusal.contains("(starting)"),
        "a claimed but unnamed run still refuses the next one: {refusal}"
    );
}

/// Plants a live job (finished: none) carrying `run_id`, tries to start
/// another, and hands back the refusal's message.
#[cfg(test)]
fn refused_start_message(run_id: Option<&str>) -> String {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let named = Arc::new(std::sync::OnceLock::new());
        if let Some(run_id) = run_id {
            named.set(run_id.to_string()).expect("a fresh OnceLock");
        }
        let live = IngestJob {
            run_id: named,
            cancel: Arc::new(majestical_ingest::engine::CancelFlag::new(false)),
            finished: Arc::new(Mutex::new(None)),
        };
        *fixture
            .state
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(live);

        let events = Arc::new(Mutex::new(Vec::new()));
        let Err(err) = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            collecting_sink(&events),
        ) else {
            panic!("a second run must be refused while one is live");
        };
        assert!(
            collected(&events).is_empty(),
            "the refused start must not have run anything"
        );
        err.message
    })
}

/// Cancelling between files leaves the queue unplaced and the run
/// resumable — and the run that comes back is listed by
/// `list_unfinished_ingests` under the same id.
#[test]
fn cancel_ingest_stops_the_run_and_leaves_it_resumable() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(24));
        let events: Arc<Mutex<Vec<IngestProgress>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let into = Arc::clone(&events);
            let state = Arc::clone(&fixture.state);
            let sink: ProgressSink = Arc::new(move |progress: IngestProgress| {
                if matches!(
                    progress.event,
                    majestical_ingest::engine::ProgressEvent::FilePlaced { .. }
                ) {
                    cancel_ingest_impl(&state);
                }
                into.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(progress);
            });
            sink
        };
        let run_id = start_ingest_impl(&fixture.cfg, &fixture.state, fixture.request(None), sink)
            .expect("start_ingest");
        let placed = placed_count(&fixture.finished_run());
        assert!(
            placed < 24,
            "cancelling after the first placed file must leave work queued, placed {placed}"
        );
        assert!(
            collected(&events).iter().any(|p| matches!(
                p.event,
                majestical_ingest::engine::ProgressEvent::RunStopped { cancelled: true }
            )),
            "the run must report itself cancelled"
        );

        let unfinished =
            list_unfinished_ingests_impl(&fixture.cfg).expect("list_unfinished_ingests");
        let listed = unfinished
            .runs
            .iter()
            .find(|run| run.run_id == run_id)
            .unwrap_or_else(|| panic!("the cancelled run must be resumable: {unfinished:?}"));
        assert_eq!(listed.planned, 24);
        assert_eq!(
            usize::try_from(listed.placed).expect("a placed count fits a usize"),
            placed
        );
        assert_eq!(listed.source, fixture.source.path().display().to_string());

        // Resuming finishes it under its own id, and nothing is left over.
        let resumed = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(Some(run_id.clone())),
            collecting_sink(&events),
        )
        .expect("resume");
        assert_eq!(resumed, run_id, "a resume keeps the original run's id");
        assert_eq!(placed_count(&fixture.finished_run()), 24 - placed);
        assert!(
            list_unfinished_ingests_impl(&fixture.cfg)
                .expect("list_unfinished_ingests")
                .runs
                .iter()
                .all(|run| run.run_id != run_id),
            "a resumed run that placed everything is finished"
        );
    });
}

/// Cancelling with nothing running is a no-op, not an error — the surface
/// can wire Stop unconditionally.
#[test]
fn cancel_ingest_with_nothing_running_does_nothing() {
    let state = IngestState(RwLock::new(None));
    cancel_ingest_impl(&state);
    cancel_ingest_impl(&state);
    let wire = ingest_state_impl(&state);
    assert!(!wire.busy);
    assert!(wire.running.is_none());
    assert!(wire.finished.is_none());
}

/// The run outlives the webview: a reload throws away every event the
/// surface accumulated, so `ingest_state` must keep answering with the
/// finished run — as many times as it is asked.
#[test]
fn ingest_state_keeps_reporting_the_finished_run() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(2));
        let events = Arc::new(Mutex::new(Vec::new()));
        let run_id = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            collecting_sink(&events),
        )
        .expect("start_ingest");
        assert_eq!(placed_count(&fixture.finished_run()), 2);

        for _ in 0..2 {
            let wire = ingest_state_impl(&fixture.state);
            assert!(!wire.busy, "the run is over");
            assert!(wire.running.is_none());
            let Some(FinishedIngest::Done { run }) = wire.finished.as_deref() else {
                panic!("a reloaded surface must still find the finished run");
            };
            assert_eq!(run.run_id, run_id);
            assert_eq!(run.outcome.placed.len(), 2);
            assert_eq!(run.generations.len(), 1, "one ASC MHL generation per dest");
        }
    });
}

/// A run that fails before it is ever named answers the caller AND lands in
/// the state, so a surface that reloaded mid-failure still renders it.
#[test]
fn a_run_that_fails_before_it_starts_lands_in_the_state() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let mut req = fixture.request(None);
        req.para = "project/nope".to_string();
        let events = Arc::new(Mutex::new(Vec::new()));
        let Err(err) =
            start_ingest_impl(&fixture.cfg, &fixture.state, req, collecting_sink(&events))
        else {
            panic!("an unknown PARA target must fail the start");
        };
        assert!(err.message.contains("nope"), "{}", err.message);

        let finished = fixture.await_finished();
        let FinishedIngest::Failed { error } = finished.as_ref() else {
            panic!("the failure must be what the state reports");
        };
        assert_eq!(error.message, err.message);
        assert!(!ingest_state_impl(&fixture.state).busy);
    });
}

/// An unknown `--resume` id is the CLI's own guard reaching the GUI: the
/// error names the remedy, and the slot is free again afterwards.
#[test]
fn start_ingest_with_an_unknown_resume_id_reports_the_remedy() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let events = Arc::new(Mutex::new(Vec::new()));
        let Err(err) = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string())),
            collecting_sink(&events),
        ) else {
            panic!("an unknown resume id must fail");
        };
        assert!(
            err.message.contains("no journal for run"),
            "{}",
            err.message
        );
        fixture.await_finished();
        assert!(!ingest_state_impl(&fixture.state).busy);
    });
}

#[test]
fn list_unfinished_ingests_on_a_fresh_catalog_is_empty() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let outcome = list_unfinished_ingests_impl(&fixture.cfg).expect("list_unfinished_ingests");
        assert!(outcome.runs.is_empty(), "{outcome:?}");
    });
}

/// A panicking run must fail like any other failed run, not wedge the app:
/// without the thread's `catch_unwind` the unwind skips the publish, the
/// slot reads `busy` forever, and every later start is refused with "is
/// still going" for a run that died minutes ago. The panic is injected
/// where a real one is most plausible — our own progress sink, which the
/// engine calls on a worker thread.
#[test]
fn a_run_whose_progress_sink_panics_fails_and_frees_the_slot() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(2));
        let exploding: ProgressSink = Arc::new(|progress: IngestProgress| {
            assert!(
                !matches!(
                    progress.event,
                    majestical_ingest::engine::ProgressEvent::FilePlaced { .. }
                ),
                "boom in the progress sink"
            );
        });
        let run_id = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            exploding,
        )
        .expect("the run is named before anything can panic");
        assert_eq!(run_id.len(), 26);

        let finished = fixture.await_finished();
        let FinishedIngest::Failed { error } = finished.as_ref() else {
            panic!("a panicked run is a failed run");
        };
        assert!(
            error.message.contains("the ingest run panicked"),
            "{}",
            error.message
        );

        // The whole point: the slot is free again.
        let wire = ingest_state_impl(&fixture.state);
        assert!(!wire.busy, "a panicked run must not hold the slot");
        let events = Arc::new(Mutex::new(Vec::new()));
        let next = start_ingest_impl(
            &fixture.cfg,
            &fixture.state,
            fixture.request(None),
            collecting_sink(&events),
        )
        .expect("a new run is accepted after a panicked one");
        assert_ne!(next, run_id);
        fixture.await_finished();
    });
}

/// `maj ingest`'s own guard, on both ingest paths: a file is not a source
/// tree, and walking one would plan a single-entry copy to a destination
/// nobody chose.
#[test]
fn a_source_that_is_not_a_directory_is_refused_by_both_ingest_paths() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = ingest_fixture(dir.path(), &tiny_files(1));
        let file = fixture.source.path().join("clip-0.mov");

        let Err(planned) =
            plan_ingest_impl(&fixture.cfg, &file, "project/client-x", Some("raw".into()))
        else {
            panic!("planning a file must fail");
        };
        assert!(
            planned.message.contains("source must be a directory"),
            "{}",
            planned.message
        );
        assert!(
            planned.message.contains("clip-0.mov"),
            "{}",
            planned.message
        );

        let mut req = fixture.request(None);
        req.source = file;
        let events = Arc::new(Mutex::new(Vec::new()));
        let Err(started) =
            start_ingest_impl(&fixture.cfg, &fixture.state, req, collecting_sink(&events))
        else {
            panic!("starting from a file must fail");
        };
        assert!(
            started.message.contains("source must be a directory"),
            "{}",
            started.message
        );
        assert!(
            !ingest_state_impl(&fixture.state).busy,
            "a refused source must not leave the slot held"
        );
    });
}
