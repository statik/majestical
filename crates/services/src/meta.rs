//! `maj meta get` compute: field lookup against the projection. Moved from
//! `crates/cli/src/commands.rs::print_meta_get`'s lookup half; the CLI keeps
//! both print styles (including the empty-line/`null` behavior for a
//! missing single field — driven by whether `field` was given, which the
//! CLI already has and doesn't need this outcome to repeat).
use crate::app::FsApp;
use crate::error::ServiceError;
use anyhow::Result;
use majestical_core::event::AssetId;

/// A single requested `field` (0-or-1 entries — present only if set), or
/// every field currently set on the asset.
pub struct MetaOutcome {
    pub fields: Vec<(String, String)>,
}

/// `maj meta get <asset> [field]`: looks up one field, or every field set on
/// `asset`.
///
/// # Errors
/// Returns an error if the event log cannot be read.
pub fn meta_get(
    app: &FsApp,
    asset: &str,
    field: Option<&str>,
) -> Result<MetaOutcome, ServiceError> {
    meta_get_impl(app, asset, field).map_err(ServiceError::from)
}

fn meta_get_impl(app: &FsApp, asset: &str, field: Option<&str>) -> Result<MetaOutcome> {
    let projection = app.projection()?;
    let asset_id = AssetId(asset.to_string());
    let fields = if let Some(field) = field {
        projection
            .field(&asset_id, field)
            .map(|value| vec![(field.to_string(), value.to_string())])
            .unwrap_or_default()
    } else {
        projection
            .fields(&asset_id)
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    Ok(MetaOutcome { fields })
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_core::event::Op;

    fn seeded_app(dir: &std::path::Path) -> (FsApp, AssetId) {
        let root = dir.join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        let asset = AssetId("xxh3:0123456789abcdef0123456789abcdef".into());
        app.emit(vec![
            Op::AssetSeen {
                asset: asset.clone(),
                volume: "vol1".into(),
                path: "clip.txt".into(),
                size: 5,
                mtime_ms: 1000,
            },
            Op::FieldSet {
                asset: asset.clone(),
                field: "rating".into(),
                value: "5".into(),
            },
        ])
        .expect("emit");
        (app, asset)
    }

    #[test]
    fn meta_get_of_a_set_field_returns_one_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (app, asset) = seeded_app(dir.path());
        let outcome = meta_get(&app, &asset.0, Some("rating")).expect("meta_get");
        assert_eq!(
            outcome.fields,
            vec![("rating".to_string(), "5".to_string())]
        );
    }

    #[test]
    fn meta_get_of_an_unset_field_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (app, asset) = seeded_app(dir.path());
        let outcome = meta_get(&app, &asset.0, Some("missing")).expect("meta_get");
        assert!(outcome.fields.is_empty());
    }

    #[test]
    fn meta_get_of_no_field_returns_every_set_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (app, asset) = seeded_app(dir.path());
        let outcome = meta_get(&app, &asset.0, None).expect("meta_get");
        assert_eq!(
            outcome.fields,
            vec![("rating".to_string(), "5".to_string())]
        );
    }
}
