//! One cmd_* handler per CLI verb. main.rs owns clap definitions and dispatch;
//! handlers own behavior.
use crate::app::{FsApp, physical_now_ms};
use crate::iso8601::iso8601_ms;
use crate::volume_identity;
use crate::{MetaCmd, TagCmd};
use anyhow::{Context, Result};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::clock::MAX_DRIFT_MS;
use majestical_core::event::{AssetId, Op};
use majestical_core::projection::Projection;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn cmd_catalog_init(catalog: &Path, machine_id: &str, author: &str) -> Result<()> {
    FsApp::init(catalog, machine_id, author)?;
    println!("initialized catalog at {}", catalog.display());
    Ok(())
}

/// Resolves the (id, label) pair a scan should tag its events with. An
/// explicit `--volume` is used as both id and label — an override that
/// keeps e2e tests deterministic. Omitted, the volume's physical identity
/// is auto-detected (see `volume_identity`).
pub(crate) fn resolve_volume(dir: &Path, volume: Option<String>) -> (String, String) {
    if let Some(v) = volume {
        return (v.clone(), v);
    }
    let identity = volume_identity::resolve(dir);
    (identity.id, identity.label)
}

pub(crate) fn cmd_scan(app: &mut FsApp, dir: &Path, volume: Option<String>) -> Result<()> {
    let (volume_id, volume_label) = resolve_volume(dir, volume);
    let mut ops = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.context("walking scan directory")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let size = entry
            .metadata()
            .with_context(|| format!("reading metadata for {}", entry.path().display()))?
            .len();
        let file = std::fs::File::open(entry.path())
            .with_context(|| format!("reading {}", entry.path().display()))?;
        // Stream the hash rather than loading the whole file: media
        // assets can be multi-gigabyte, so a `Vec<u8>` per file would
        // blow up memory on a scan of a card full of video.
        let mut hasher = xxhash_rust::xxh3::Xxh3::new();
        let mut reader = std::io::BufReader::new(file);
        let mut buf = vec![0u8; 64 * 1024].into_boxed_slice();
        loop {
            let n = reader
                .read(&mut buf)
                .with_context(|| format!("reading {}", entry.path().display()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let hash = hasher.digest128();
        // Phase 1: lossy UTF-8 conversion of the relative path. JSON
        // events force UTF-8 anyway, so a non-UTF-8 path can't round
        // trip through the log yet; revisit once ingest needs to
        // preserve exact bytes.
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        ops.push(Op::AssetSeen {
            asset: AssetId(format!("xxh3:{hash:032x}")),
            volume: volume_id.clone(),
            path: rel,
            size,
        });
    }
    let n = ops.len();
    ops.insert(
        0,
        Op::VolumeSeen {
            volume: volume_id,
            label: volume_label,
        },
    );
    app.emit(ops)?;
    println!("scanned: {n} assets");
    Ok(())
}

/// Both `tag add` and `meta set` write metadata about an asset that must
/// already have a physical observation on record — otherwise a typo'd id
/// silently creates a phantom catalog entry that `search` and `scan` can
/// never produce, and would look scanned when it never was.
pub(crate) fn ensure_asset_known(projection: &Projection, asset: &AssetId) -> Result<()> {
    anyhow::ensure!(
        projection.has_instances(asset),
        "unknown asset {} — scan its volume first, or check `maj search`",
        asset.0
    );
    Ok(())
}

pub(crate) fn cmd_tag(app: &mut FsApp, cmd: TagCmd) -> Result<()> {
    match cmd {
        TagCmd::Add { asset, tag } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            ensure_asset_known(&p, &asset)?;
            app.emit(vec![Op::TagAdd { asset, tag }])?;
        }
        TagCmd::Rm { asset, tag } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            let observed = p.tag_add_ids(&asset, &tag);
            anyhow::ensure!(
                !observed.is_empty(),
                "tag '{tag}' is not set on {} — nothing to remove",
                asset.0
            );
            app.emit(vec![Op::TagRemove {
                asset,
                tag,
                observed,
            }])?;
        }
    }
    println!("ok");
    Ok(())
}

pub(crate) fn cmd_meta(app: &mut FsApp, cmd: MetaCmd) -> Result<()> {
    match cmd {
        MetaCmd::Set {
            asset,
            field,
            value,
        } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            ensure_asset_known(&p, &asset)?;
            app.emit(vec![Op::FieldSet {
                asset,
                field,
                value,
            }])?;
            println!("ok");
        }
        MetaCmd::Get { asset, field, json } => {
            let p = app.projection()?;
            let asset = AssetId(asset);
            print_meta_get(&p, &asset, field.as_deref(), json);
        }
    }
    Ok(())
}

/// Prints either a single field's value or every field set on `asset`.
/// A single missing field prints nothing (an empty line in text mode, `null`
/// in JSON) rather than erroring — mirroring `search`'s "zero hits" style
/// rather than treating "not set yet" as a failure.
pub(crate) fn print_meta_get(
    projection: &Projection,
    asset: &AssetId,
    field: Option<&str>,
    json: bool,
) {
    if let Some(field) = field {
        let value = projection.field(asset, field);
        if json {
            println!("{}", serde_json::json!({ field: value }));
        } else if let Some(value) = value {
            println!("{value}");
        } else {
            println!();
        }
        return;
    }
    let fields: Vec<(&str, &str)> = projection.fields(asset).collect();
    if json {
        let obj: serde_json::Map<String, serde_json::Value> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect();
        println!("{}", serde_json::Value::Object(obj));
    } else {
        for (k, v) in fields {
            println!("{k}\t{v}");
        }
    }
}

pub(crate) fn cmd_search(
    app: &FsApp,
    catalog_dir: &Path,
    name: Option<String>,
    tag: Option<String>,
    json: bool,
) -> Result<()> {
    let projection = app.projection()?;
    let db_path = catalog_dir.join("catalog.db");
    let mut db = SqliteCatalog::open(&db_path).context("opening sqlite catalog")?;
    db.rebuild(&projection)
        .context("rebuilding sqlite projection")?;
    let ids = match (name, tag) {
        (Some(n), None) => db.search_by_name(&n)?,
        (None, Some(t)) => db.search_by_tag(&t)?,
        // The `search_by` ArgGroup (required, mutually exclusive)
        // guarantees clap rejects these combinations before `main`
        // ever runs, so this arm can't be reached.
        (Some(_), Some(_)) | (None, None) => {
            unreachable!("clap's search_by ArgGroup allows exactly one of these")
        }
    };
    if json {
        let results: Vec<_> = ids
            .iter()
            .map(|a| serde_json::json!({ "asset": a.0 }))
            .collect();
        println!(
            "{}",
            serde_json::json!({ "count": ids.len(), "results": results })
        );
    } else {
        for a in &ids {
            println!("{}", a.0);
        }
        println!("{} results", ids.len());
    }
    Ok(())
}

/// Cheap phase-2 "is this volume mounted right now" heuristic, not true
/// device enumeration. `label:`-id volumes are considered online if
/// `/Volumes/<label>` exists (or the label is the root volume's, which is
/// always present). `uuid:`-id volumes are considered online only if a
/// mount at `/Volumes/<label>` exists *and* resolving its identity still
/// yields the same id — so a same-named but different card reads offline.
/// False negative: a volume mounted somewhere other than `/Volumes` reads
/// offline even when present.
pub(crate) fn volume_is_online(id: &str, label: &str) -> bool {
    if label == volume_identity::ROOT_LABEL {
        return true;
    }
    let candidate = PathBuf::from("/Volumes").join(label);
    if !candidate.exists() {
        return false;
    }
    if id.starts_with("uuid:") {
        return volume_identity::resolve(&candidate).id == id;
    }
    true
}

pub(crate) fn cmd_volumes_list(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let projection = app.projection()?;
    let db_path = catalog_dir.join("catalog.db");
    let mut db = SqliteCatalog::open(&db_path).context("opening sqlite catalog")?;
    db.rebuild(&projection)
        .context("rebuilding sqlite projection")?;
    let volumes = db.volumes().context("querying volumes")?;
    let counts: HashMap<String, u64> = db
        .volume_asset_counts()
        .context("querying volume asset counts")?
        .into_iter()
        .collect();
    // A stored last-seen wall time past this ceiling could only have come
    // from a clock more than MAX_DRIFT_MS ahead of physical now — the HLC
    // clamp bounds the *local* clock's adoption of such a timestamp, but
    // doesn't touch what's already durable in the event log, so a poisoned
    // VolumeSeen can still win the LWW max and display forever unflagged.
    let suspect_ceiling = physical_now_ms().saturating_add(MAX_DRIFT_MS);

    if json {
        let rows: Vec<_> = volumes
            .iter()
            .map(|(id, label, last_seen_ms)| {
                serde_json::json!({
                    "id": id,
                    "label": label,
                    "last_seen": iso8601_ms(*last_seen_ms),
                    "online": volume_is_online(id, label),
                    "asset_count": counts.get(id).copied().unwrap_or(0),
                    "clock_suspect": *last_seen_ms > suspect_ceiling,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "volumes": rows }));
    } else {
        print_volumes_table(&volumes, &counts, suspect_ceiling);
    }
    Ok(())
}

/// Renders the human-readable volumes table with column widths sized to
/// the widest cell in each column (header included) — a fixed width breaks
/// alignment once an auto-detected `uuid:` id (41 chars) or a
/// "(clock suspect)"-annotated last-seen cell appears.
pub(crate) fn print_volumes_table(
    volumes: &[(String, String, u64)],
    counts: &HashMap<String, u64>,
    suspect_ceiling: u64,
) {
    let rows: Vec<(String, String, String, &'static str, u64)> = volumes
        .iter()
        .map(|(id, label, last_seen_ms)| {
            let mut last_seen = iso8601_ms(*last_seen_ms);
            if *last_seen_ms > suspect_ceiling {
                last_seen.push_str(" (clock suspect)");
            }
            let online = if volume_is_online(id, label) {
                "online"
            } else {
                "offline"
            };
            let count = counts.get(id).copied().unwrap_or(0);
            (id.clone(), label.clone(), last_seen, online, count)
        })
        .collect();
    let id_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0).max(2);
    let label_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(0).max(5);
    let seen_w = rows.iter().map(|r| r.2.len()).max().unwrap_or(0).max(9);
    let online_w = rows.iter().map(|r| r.3.len()).max().unwrap_or(0).max(6);
    println!(
        "{:<id_w$} {:<label_w$} {:<seen_w$} {:<online_w$} ASSETS",
        "ID", "LABEL", "LAST SEEN", "ONLINE"
    );
    for (id, label, last_seen, online, count) in &rows {
        println!("{id:<id_w$} {label:<label_w$} {last_seen:<seen_w$} {online:<online_w$} {count}");
    }
}
