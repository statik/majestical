//! Sync location config plus `maj sync status`/`maj sync location list`
//! compute. Moved from `crates/cli/src/sync_cmd.rs`. `push`/`pull` (which
//! transfer files and apply landed events — mutating) stay in the CLI for
//! now, but share [`SyncConfig`]/[`Location`]/[`config_path`]/
//! [`resolve_targets`]/[`NO_LOCATIONS_HINT`] from here rather than each
//! holding its own copy.
use crate::error::ServiceError;
use anyhow::{Context, Result};
use majestical_sync::transfer;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const NO_LOCATIONS_HINT: &str =
    "no sync locations configured — add one with `maj sync location add <name> <path>`";

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SyncConfig {
    /// The read-only-member switch: a machine with `readonly = true` never
    /// pushes (events already carry author identity, so this is the whole
    /// feature — a policy on the push side, not a data concept).
    #[serde(default)]
    pub readonly: bool,
    #[serde(default, rename = "location")]
    pub locations: Vec<Location>,
    /// Fields a newer `maj` wrote that this build doesn't know about.
    /// Flattened so `location add|rm` round-trip them unchanged instead of
    /// silently dropping them.
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Location {
    pub name: String,
    pub path: PathBuf,
    /// See [`SyncConfig::extra`].
    #[serde(flatten)]
    pub extra: toml::Table,
}

impl SyncConfig {
    /// Load config from `path`; a missing file returns `Self::default()` (a
    /// catalog that has never configured sync), never an error.
    ///
    /// # Errors
    /// Returns an error when `path` exists but can't be read, or its
    /// contents don't parse as TOML.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| format!("reading {}", path.display()));
            }
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Serialize and write `path`, replacing it via a same-directory
    /// temp-file-then-rename so a concurrent reader never observes a
    /// partial write. The file is always rewritten wholesale from this
    /// struct: a hand-edited known field (e.g. `readonly`) survives a
    /// later `location add|rm`, but TOML comments do not — they have no
    /// representation in the struct, so they're dropped on the next store.
    ///
    /// # Errors
    /// Returns an error when serialization fails, or the write/rename
    /// fails.
    pub fn store(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).context("serializing sync config")?;
        let file_name = path.file_name().map_or_else(
            || "sync.toml".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let tmp = path.with_file_name(format!("{file_name}.tmp"));
        std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("finalizing {}", path.display()))
    }
}

/// The per-catalog `sync.toml` path in this machine's state dir.
///
/// # Errors
/// Returns an error if the local state dir can't be resolved.
pub fn config_path(catalog: &Path) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog)?.join("sync.toml"))
}

/// Locations to operate on: every configured location, or exactly the named
/// one.
///
/// # Errors
/// Returns an error when no locations are configured at all, or `name` is
/// given but doesn't match any configured location.
pub fn resolve_targets<'a>(cfg: &'a SyncConfig, name: Option<&str>) -> Result<Vec<&'a Location>> {
    anyhow::ensure!(!cfg.locations.is_empty(), "{NO_LOCATIONS_HINT}");
    let Some(name) = name else {
        return Ok(cfg.locations.iter().collect());
    };
    cfg.locations.iter().find(|l| l.name == name).map_or_else(
        || {
            let known: Vec<&str> = cfg.locations.iter().map(|l| l.name.as_str()).collect();
            Err(anyhow::anyhow!(
                "no sync location named '{name}' — configured: {}",
                known.join(", ")
            ))
        },
        |l| Ok(vec![l]),
    )
}

/// Guards every sync entry point against operating on a directory that was
/// never `maj catalog init`ed. See `FsApp::open`'s doc for why this can't
/// just reuse that guard: the transfer engine creates `events/<machine>/`
/// on the destination as a side effect of copying a segment there, so a
/// pull in particular must check before it transfers anything, not after.
fn ensure_catalog(catalog: &Path) -> Result<(), ServiceError> {
    if catalog.join("events").is_dir() {
        Ok(())
    } else {
        Err(ServiceError::NoCatalog {
            root: catalog.to_path_buf(),
        })
    }
}

/// One machine's ahead/behind segment tally within a direction: files
/// pending and the bytes the destination is missing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct SegmentCounts {
    pub files: usize,
    pub bytes: u64,
}

/// Blob counts by [`transfer::BlobClass`], always present — even at zero —
/// so the JSON contract's key set never varies with what's actually
/// pending.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct BlobCounts {
    pub thumbs: usize,
    pub metadata: usize,
    pub vectors: usize,
    pub transcripts: usize,
}

impl BlobCounts {
    fn from_blobs(blobs: &[transfer::BlobCopy]) -> Self {
        let mut counts = Self::default();
        for b in blobs {
            match b.class {
                transfer::BlobClass::Thumbs => counts.thumbs += 1,
                transfer::BlobClass::Metadata => counts.metadata += 1,
                transfer::BlobClass::Vectors => counts.vectors += 1,
                transfer::BlobClass::Transcripts => counts.transcripts += 1,
            }
        }
        counts
    }

    /// True when every class is at zero — the JSON-empty half of a plan's
    /// emptiness (see [`DirectionCounts::is_empty`]).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One direction's (`ahead` or `behind`) pending work: segment counts
/// grouped per machine — the spec's granularity, unlike push/pull's report,
/// which totals every machine's bytes into one figure — plus blob counts by
/// class.
#[derive(Debug, Default, serde::Serialize)]
pub struct DirectionCounts {
    pub segments: BTreeMap<String, SegmentCounts>,
    pub blobs: BlobCounts,
}

impl DirectionCounts {
    fn from_plan(plan: &transfer::TransferPlan) -> Self {
        let mut segments: BTreeMap<String, SegmentCounts> = BTreeMap::new();
        for s in &plan.segments {
            let counts = segments.entry(s.machine.clone()).or_default();
            counts.files += 1;
            counts.bytes += s.src_len.saturating_sub(s.dst_len);
        }
        Self {
            segments,
            blobs: BlobCounts::from_blobs(&plan.blobs),
        }
    }

    /// True when nothing is pending in this direction — the collapse
    /// condition for text mode's `<name>: in sync` line (rendered by the
    /// CLI; this only reports the fact).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() && self.blobs.is_empty()
    }
}

/// One location's status: reachable (both directions' full counts, so text
/// and JSON rendering read the exact same walk and can never disagree),
/// unreachable (the mount isn't there), or failed (`plan_transfer` itself
/// errored — e.g. a permission problem on a location that IS mounted).
#[derive(Debug, serde::Serialize)]
pub enum StatusRow {
    Reachable {
        name: String,
        ahead: DirectionCounts,
        behind: DirectionCounts,
    },
    Unreachable {
        name: String,
        path: PathBuf,
    },
    Failed {
        name: String,
        error: String,
    },
}

/// Everything `maj sync status` renders.
#[derive(Debug, serde::Serialize)]
pub struct SyncStatusOutcome {
    pub rows: Vec<StatusRow>,
    pub readonly: bool,
}

/// Plans both directions for one location, read-only: it only ever calls
/// [`transfer::plan_transfer`], never [`transfer::execute`]. A
/// `plan_transfer` error becomes a [`StatusRow::Failed`] rather than
/// propagating.
fn status_row(catalog: &Path, loc: &Location) -> StatusRow {
    if !loc.path.is_dir() {
        return StatusRow::Unreachable {
            name: loc.name.clone(),
            path: loc.path.clone(),
        };
    }
    match plan_both_directions(catalog, &loc.path) {
        Ok((ahead, behind)) => StatusRow::Reachable {
            name: loc.name.clone(),
            ahead: DirectionCounts::from_plan(&ahead),
            behind: DirectionCounts::from_plan(&behind),
        },
        Err(error) => StatusRow::Failed {
            name: loc.name.clone(),
            error: error.to_string(),
        },
    }
}

/// Plans `ahead` (catalog -> location) then `behind` (location -> catalog),
/// short-circuiting on the first failure.
///
/// # Errors
/// Returns [`transfer::TransferError`] if either direction's plan fails.
fn plan_both_directions(
    catalog: &Path,
    location: &Path,
) -> Result<(transfer::TransferPlan, transfer::TransferPlan), transfer::TransferError> {
    let ahead = transfer::plan_transfer(catalog, location)?;
    let behind = transfer::plan_transfer(location, catalog)?;
    Ok((ahead, behind))
}

/// `maj sync status`: for every configured location, plans BOTH
/// directions — what a push would send (`ahead`) and what a pull would
/// fetch (`behind`) — without executing either. Every count comes from a
/// fresh diff of real files at this moment; nothing is cached. An
/// unreachable location, or one whose plan itself fails, is a reported row,
/// never an error that aborts the report.
///
/// # Errors
/// Returns an error when there's no catalog at `catalog_dir`, or no sync
/// locations are configured.
pub fn status(catalog_dir: &Path) -> Result<SyncStatusOutcome, ServiceError> {
    status_impl(catalog_dir).map_err(ServiceError::from)
}

fn status_impl(catalog_dir: &Path) -> Result<SyncStatusOutcome> {
    ensure_catalog(catalog_dir)?;
    let cfg = SyncConfig::load(&config_path(catalog_dir)?)?;
    let targets = resolve_targets(&cfg, None)?;
    let rows = targets
        .into_iter()
        .map(|loc| status_row(catalog_dir, loc))
        .collect();
    Ok(SyncStatusOutcome {
        rows,
        readonly: cfg.readonly,
    })
}

/// Everything `maj sync location list` renders. `locations` carries the
/// full configured `Location` (including any unknown `extra` fields a
/// newer `maj` wrote) rather than a narrower row type, so a round trip
/// through `location list --json` reproduces exactly what `location add`
/// stored — narrowing to just name/path would silently drop those fields.
#[derive(serde::Serialize)]
pub struct LocationsOutcome {
    pub readonly: bool,
    pub locations: Vec<Location>,
}

/// `maj sync location list`: this machine's configured sync locations.
///
/// # Errors
/// Returns an error if `sync.toml` exists but can't be read or parsed.
pub fn locations_list(catalog_dir: &Path) -> Result<LocationsOutcome, ServiceError> {
    locations_list_impl(catalog_dir).map_err(ServiceError::from)
}

fn locations_list_impl(catalog_dir: &Path) -> Result<LocationsOutcome> {
    let cfg = SyncConfig::load(&config_path(catalog_dir)?)?;
    Ok(LocationsOutcome {
        readonly: cfg.readonly,
        locations: cfg.locations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_of_a_catalog_with_no_locations_errors_naming_the_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        std::fs::create_dir_all(root.join("events")).expect("mkdir");
        let err = status(&root).expect_err("no locations must error");
        assert!(err.to_string().contains("no sync locations configured"));
    }

    #[test]
    fn status_of_a_nonexistent_catalog_names_catalog_init() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("nope");
        let err = status(&root).expect_err("missing catalog must error");
        assert!(err.to_string().contains("maj catalog init"));
    }

    #[test]
    fn locations_list_of_an_unconfigured_catalog_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = locations_list(dir.path()).expect("locations_list");
        assert!(outcome.locations.is_empty());
        assert!(!outcome.readonly);
    }

    #[test]
    fn direction_counts_from_an_empty_plan_is_empty() {
        let plan = transfer::TransferPlan {
            segments: Vec::new(),
            blobs: Vec::new(),
        };
        assert!(DirectionCounts::from_plan(&plan).is_empty());
    }

    #[test]
    fn blob_counts_from_blobs_counts_each_class_independently() {
        let blobs = vec![
            transfer::BlobCopy {
                rel: "a".into(),
                class: transfer::BlobClass::Metadata,
                size: 1,
            },
            transfer::BlobCopy {
                rel: "b".into(),
                class: transfer::BlobClass::Metadata,
                size: 1,
            },
            transfer::BlobCopy {
                rel: "c".into(),
                class: transfer::BlobClass::Vectors,
                size: 1,
            },
            transfer::BlobCopy {
                rel: "d".into(),
                class: transfer::BlobClass::Transcripts,
                size: 1,
            },
        ];
        let counts = BlobCounts::from_blobs(&blobs);
        assert_eq!(
            counts,
            BlobCounts {
                thumbs: 0,
                metadata: 2,
                vectors: 1,
                transcripts: 1,
            }
        );
        assert!(!counts.is_empty());
    }

    #[test]
    fn direction_counts_from_plan_groups_segments_per_machine() {
        let plan = transfer::TransferPlan {
            segments: vec![
                transfer::SegmentCopy {
                    machine: "m1".into(),
                    segment: "events/m1/0000.jsonl".into(),
                    src_len: 100,
                    dst_len: 40,
                },
                transfer::SegmentCopy {
                    machine: "m1".into(),
                    segment: "events/m1/0001.jsonl".into(),
                    src_len: 50,
                    dst_len: 0,
                },
            ],
            blobs: Vec::new(),
        };
        let counts = DirectionCounts::from_plan(&plan);
        let m1 = counts.segments.get("m1").expect("m1 present");
        assert_eq!(
            *m1,
            SegmentCounts {
                files: 2,
                bytes: 110
            }
        );
        assert!(!counts.is_empty());
    }
}
