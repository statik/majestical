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
use majestical_services::catalog::{AssetDetail, AssetInstance, AssetVerification};
use majestical_services::search::{
    SavedSearch, SearchHit, SearchOutcome, SemanticCoverage, TextCoverageNotice, VolumeRef,
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
