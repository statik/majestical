//! Pins the wire shape of every outcome struct the GUI consumes against
//! committed JSON fixtures in `apps/desktop/src/lib/fixtures/`. A renamed
//! or retyped serde field fails here (Rust side) and fails
//! `fixtures.test.ts` / `svelte-check` (TS side). Regenerate deliberately:
//! `MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures`.
//!
//! Every struct pinned here has entirely `pub` fields, so each builder is a
//! plain struct literal rather than a real catalog driven through
//! `*_impl` (the path `tests/commands.rs` uses) — there is nothing private
//! to round-trip through a service call for.
use majestical_core::event::VerifyOutcome;
use majestical_desktop::commands::{AppStatus, CommandError, SavedSearches};
use majestical_services::browse::{
    BrowseFolder, BrowseListOutcome, BrowseTreeOutcome, BrowseVolume,
};
use majestical_services::catalog::{AssetDetail, AssetInstance, AssetVerification};
use majestical_services::para::{
    ArchiveMove, ArchiveOutcome, MoveStatus, ParaNodeRow, ParaOutcome,
};
use majestical_services::search::{
    SavedSearch, SearchHit, SearchOutcome, SemanticCoverage, TextCoverageNotice, VolumeRef,
};
use majestical_services::tags::{
    AssignFailure, AssignOutcome, TagRenameOutcome, TagRow, TagsListOutcome,
};
use majestical_services::volumes::{VolumeRow, VolumesOutcome};
use std::path::PathBuf;

// `#[cfg(test)]` on these helpers is not redundant despite this file already
// building with `--cfg test`: clippy's in-test detection for
// `allow-expect-in-tests` keys off `#[test]`/`#[cfg(test)]` directly on the
// item — see `tests/commands.rs` for the full rationale.
#[cfg(test)]
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/lib/fixtures")
}

/// Recursively refuses empty arrays, empty objects, empty strings, and
/// nulls: a fixture that under-populates a field pins nothing about it.
#[cfg(test)]
fn assert_fully_populated(name: &str, value: &serde_json::Value) {
    match value {
        serde_json::Value::Array(items) => {
            assert!(!items.is_empty(), "{name}: empty array pins nothing");
            for item in items {
                assert_fully_populated(name, item);
            }
        }
        serde_json::Value::Object(map) => {
            assert!(!map.is_empty(), "{name}: empty object pins nothing");
            for (key, item) in map {
                assert_fully_populated(&format!("{name}.{key}"), item);
            }
        }
        serde_json::Value::String(s) => {
            assert!(!s.is_empty(), "{name}: empty string pins nothing");
        }
        serde_json::Value::Null => panic!("{name}: null pins nothing"),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

#[cfg(test)]
fn check_or_update(name: &str, value: &serde_json::Value) {
    assert_fully_populated(name, value);
    let path = fixtures_dir().join(format!("{name}.json"));
    let rendered = serde_json::to_string_pretty(value).expect("fixture serializes");
    if std::env::var_os("MAJ_UPDATE_FIXTURES").is_some() {
        std::fs::write(&path, format!("{rendered}\n")).expect("write fixture");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing fixture {} — regenerate with \
             MAJ_UPDATE_FIXTURES=1 cargo test --test wire_fixtures",
            path.display()
        )
    });
    assert_eq!(
        committed.trim_end(),
        rendered,
        "wire shape drifted from {} — if the Rust change is intentional, \
         regenerate with MAJ_UPDATE_FIXTURES=1 and update api.ts to match",
        path.display()
    );
}

#[test]
fn app_status_fixture() {
    let app_status = AppStatus {
        catalog_path: "/fixtures/catalog".to_string(),
        catalog_ready: true,
    };
    check_or_update(
        "app_status",
        &serde_json::to_value(&app_status).expect("serialize"),
    );
}

#[test]
fn search_outcome_fixture() {
    let search_outcome = SearchOutcome {
        count: 1,
        results: vec![SearchHit {
            asset: "xxh3:0123456789abcdef0123456789abcdef".to_string(),
            score: 0.987,
            known: true,
            name: "clip.mov".to_string(),
            volumes: vec![VolumeRef {
                id: "vol1".to_string(),
                label: "vol1".to_string(),
                online: true,
            }],
            tags: vec!["demo".to_string()],
            para: Some("project/client-x".to_string()),
            timestamp_ms: Some(1500),
            source: Some("transcript".to_string()),
            locator: Some(2),
            snippet: Some("a matching snippet".to_string()),
            // Search never populates these — only browse rows do. Left
            // `None` so this fixture's JSON stays byte-identical.
            size: None,
            mtime_ms: None,
            kind: None,
        }],
        semantic_coverage: Some(SemanticCoverage {
            embedded: 10,
            eligible: 20,
        }),
        text_coverage: vec![TextCoverageNotice {
            label: "transcripts",
            noun: "media assets",
            covered: 5,
            eligible: 10,
            remedy: "run `maj index run --transcribe`".to_string(),
            source: "transcript".to_string(),
        }],
        notices: vec!["a warning the search call collected".to_string()],
    };
    check_or_update(
        "search_outcome",
        &serde_json::to_value(&search_outcome).expect("serialize"),
    );
}

#[test]
fn asset_detail_fixture() {
    let asset_detail = AssetDetail {
        asset: "xxh3:0123456789abcdef0123456789abcdef".to_string(),
        instances: vec![AssetInstance {
            volume: "vol1".to_string(),
            volume_label: "vol1".to_string(),
            online: true,
            path: "clips/a.mov".to_string(),
            size: 5,
            mtime_ms: 1000,
        }],
        tags: vec!["demo".to_string()],
        para: Some("project/client-x".to_string()),
        fields: vec![("shot".to_string(), "sunset".to_string())],
        verifications: vec![AssetVerification {
            volume: "vol1".to_string(),
            path: "clips/a.mov".to_string(),
            algo: "c4".to_string(),
            value: "abc123".to_string(),
            outcome: VerifyOutcome::Verified,
            hashdate_ms: 1000,
        }],
        has_thumb: true,
        notices: vec!["a warning the get_asset call collected".to_string()],
    };
    check_or_update(
        "asset_detail",
        &serde_json::to_value(&asset_detail).expect("serialize"),
    );
}

#[test]
fn volumes_outcome_fixture() {
    let volumes_outcome = VolumesOutcome {
        volumes: vec![VolumeRow {
            id: "vol1".to_string(),
            label: "vol1".to_string(),
            last_seen_ms: 1000,
            online: true,
            asset_count: 1,
            clock_suspect: false,
        }],
        notices: vec!["a warning the list_volumes call collected".to_string()],
    };
    check_or_update(
        "volumes_outcome",
        &serde_json::to_value(&volumes_outcome).expect("serialize"),
    );
}

#[test]
fn saved_searches_fixture() {
    let saved_searches = SavedSearches {
        saved: vec![SavedSearch {
            name: "clips".to_string(),
            query: "clip".to_string(),
        }],
        notices: vec!["a warning the list_saved_searches call collected".to_string()],
    };
    check_or_update(
        "saved_searches",
        &serde_json::to_value(&saved_searches).expect("serialize"),
    );
}

#[test]
fn browse_tree_fixture() {
    let browse_tree_outcome = BrowseTreeOutcome {
        volumes: vec![BrowseVolume {
            id: "vol1".to_string(),
            label: "vol1".to_string(),
            online: true,
            folders: vec![BrowseFolder {
                path: "clips".to_string(),
                children: vec!["raw".to_string()],
                recursive_count: 3,
            }],
        }],
        notices: vec!["a warning the browse_tree call collected".to_string()],
    };
    check_or_update(
        "browse_tree",
        &serde_json::to_value(&browse_tree_outcome).expect("serialize"),
    );
}

/// synthetic-maximal: browse itself never populates
/// `timestamp_ms`/`source`/`locator`/`snippet` — they're `Some` here purely
/// so the TS side type-checks the full `SearchHit` surface; see
/// `search.rs`'s field docs (where those four fields are declared) for what
/// real browse rows carry — `browse.rs`'s `build_rows` is where the `None`
/// literals for all four actually live.
#[test]
fn browse_list_fixture() {
    // `count`/`folder_count` outrun `results.len()` on purpose — one row
    // shown out of 3 total matches across 2 folders — so the fixture is
    // self-documenting about the load-more math a paginated view needs.
    let browse_list_outcome = BrowseListOutcome {
        count: 3,
        folder_count: 2,
        results: vec![SearchHit {
            asset: "xxh3:0123456789abcdef0123456789abcdef".to_string(),
            score: 0.0,
            known: true,
            name: "clip.mov".to_string(),
            volumes: vec![VolumeRef {
                id: "vol1".to_string(),
                label: "vol1".to_string(),
                online: true,
            }],
            tags: vec!["demo".to_string()],
            para: Some("project/client-x".to_string()),
            timestamp_ms: Some(1500),
            source: Some("transcript".to_string()),
            locator: Some(2),
            snippet: Some("a matching snippet".to_string()),
            size: Some(1024),
            mtime_ms: Some(1_700_000_000_000),
            kind: Some("video".to_string()),
        }],
        notices: vec!["a warning the browse_list call collected".to_string()],
    };
    check_or_update(
        "browse_list",
        &serde_json::to_value(&browse_list_outcome).expect("serialize"),
    );
}

#[test]
fn tags_list_outcome_fixture() {
    let tags_list_outcome = TagsListOutcome {
        tags: vec![TagRow {
            tag: "golden-hour".to_string(),
            count: 3,
            last_used_ms: 1_700_000_000_000,
        }],
        notices: vec!["a warning the list_tags call collected".to_string()],
    };
    check_or_update(
        "tags_list_outcome",
        &serde_json::to_value(&tags_list_outcome).expect("serialize"),
    );
}

#[test]
fn tag_rename_outcome_fixture() {
    let tag_rename_outcome = TagRenameOutcome {
        from: "goldenhour".to_string(),
        to: "golden-hour".to_string(),
        rewritten: 3,
        notices: vec!["a warning the rename_tag call collected".to_string()],
    };
    check_or_update(
        "tag_rename_outcome",
        &serde_json::to_value(&tag_rename_outcome).expect("serialize"),
    );
}

/// synthetic-maximal: a bulk assignment's `failed` is populated here purely
/// so the TS side type-checks `AssignFailure`'s full surface — a real
/// success carries an empty `failed`, which `assign_tags`/`file_assets`
/// still return (just not pinned by this fixture; see `fixtures.test.ts`).
#[test]
fn assign_outcome_fixture() {
    let assign_outcome = AssignOutcome {
        applied: 2,
        failed: vec![AssignFailure {
            asset: "xxh3:never-scanned".to_string(),
            reason: "unknown asset xxh3:never-scanned".to_string(),
        }],
        notices: vec!["a warning the assign_tags call collected".to_string()],
    };
    check_or_update(
        "assign_outcome",
        &serde_json::to_value(&assign_outcome).expect("serialize"),
    );
}

#[test]
fn para_outcome_fixture() {
    let para_outcome = ParaOutcome {
        nodes: vec![ParaNodeRow {
            id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
            kind: "project".to_string(),
            name: "client-x".to_string(),
            archived: false,
        }],
        notices: vec!["a warning the list_para call collected".to_string()],
    };
    check_or_update(
        "para_outcome",
        &serde_json::to_value(&para_outcome).expect("serialize"),
    );
}

#[test]
fn archive_outcome_fixture() {
    // Two moves, two statuses: a multi-root archive can mix a genuine move
    // with a root an earlier partial run already handled — pins
    // `already_archived`'s serde spelling on the Rust side (the TS side is
    // pinned by `fixtures.test.ts`'s `allMoveStatuses` literal).
    let archive_outcome = ArchiveOutcome {
        moves: vec![
            ArchiveMove {
                from: PathBuf::from("/fixtures/root/Projects/client-x"),
                to: PathBuf::from("/fixtures/root/Archives/client-x"),
                status: MoveStatus::Moved,
            },
            ArchiveMove {
                from: PathBuf::from("/fixtures/root2/Projects/client-x"),
                to: PathBuf::from("/fixtures/root2/Archives/client-x"),
                status: MoveStatus::AlreadyArchived,
            },
        ],
        executed: true,
        notices: vec!["a warning the archive_node call collected".to_string()],
    };
    check_or_update(
        "archive_outcome",
        &serde_json::to_value(&archive_outcome).expect("serialize"),
    );
}

#[test]
fn command_error_fixture() {
    let command_error = CommandError::from(majestical_services::error::ServiceError::WithNotices {
        notices: vec!["a warning the failing call collected".to_string()],
        source: Box::new(majestical_services::error::ServiceError::NoCatalog {
            root: std::path::PathBuf::from("/fixtures/catalog"),
        }),
    });
    check_or_update(
        "command_error",
        &serde_json::to_value(&command_error).expect("serialize"),
    );
}
