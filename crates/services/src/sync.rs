//! Sync location config, `maj sync push`/`pull`/`location add`/`location
//! rm`, and `maj sync status`/`maj sync location list` compute. Moved from
//! `crates/cli/src/sync_cmd.rs`.
use crate::app::FsApp;
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
pub fn config_path(catalog: &Path, notices: &crate::notices::Notices) -> Result<PathBuf> {
    Ok(crate::state_dir::state_dir_for(catalog, notices)?.join("sync.toml"))
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
/// never `maj catalog init`ed, via `crate::catalog::ensure_catalog` (the one
/// shared predicate — see its doc). This can't just call `FsApp::open`
/// itself: the transfer engine creates `events/<machine>/` on the
/// destination as a side effect of copying a segment there, so a pull in
/// particular must check the predicate before it transfers anything, not
/// open a whole `FsApp` for it.
fn ensure_catalog(catalog: &Path) -> Result<(), ServiceError> {
    crate::catalog::ensure_catalog(catalog)
}

/// Registers a new sync location: validates `location` is an accessible,
/// UTF-8-representable directory, canonicalizes it (locations are mount
/// points and must be absolute at rest — a relative path would resolve
/// against whatever CWD a later push/pull happens to run from), idempotently
/// creates the `events/`/`blobs/` skeleton so the first push has somewhere
/// to land, and appends it to this catalog's `sync.toml`. Moved from
/// `crates/cli/src/sync_cmd.rs::add_location`/`cmd_location_add`.
///
/// # Errors
/// Returns an error when `name` is empty, `location` is not an accessible
/// directory, `location` is not valid UTF-8, `name` is already configured,
/// the skeleton directories can't be created, or the config can't be
/// stored.
pub fn location_add(
    catalog: &Path,
    name: &str,
    location: &Path,
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    location_add_impl(catalog, name, location, notices).map_err(ServiceError::from)
}

fn location_add_impl(
    catalog: &Path,
    name: &str,
    location: &Path,
    notices: &crate::notices::Notices,
) -> Result<()> {
    let config = config_path(catalog, notices)?;
    let name = name.trim();
    anyhow::ensure!(!name.is_empty(), "sync location name must not be empty");
    anyhow::ensure!(
        location.is_dir(),
        "{} is not an accessible directory — mount it or check the path",
        location.display()
    );
    let canonical = location
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", location.display()))?;
    anyhow::ensure!(
        canonical.to_str().is_some(),
        "{} is not valid UTF-8 — sync locations must have UTF-8 paths so they can be stored in sync.toml",
        canonical.display()
    );
    let mut cfg = SyncConfig::load(&config)?;
    anyhow::ensure!(
        !cfg.locations.iter().any(|l| l.name == name),
        "sync location '{name}' is already configured — remove it first with `maj sync location rm {name}`"
    );
    // Git-init style: idempotently create the layout so the first push
    // has somewhere to land. Never touches existing files.
    for sub in ["events", "blobs"] {
        let dir = canonical.join(sub);
        std::fs::create_dir_all(&dir).with_context(|| format!("initializing {}", dir.display()))?;
    }
    cfg.locations.push(Location {
        name: name.to_string(),
        path: canonical,
        extra: toml::Table::new(),
    });
    cfg.store(&config)
}

/// Removes the location named `name` from this catalog's `sync.toml`. Never
/// touches the location's own files (its `events/`/`blobs/` directories, and
/// anything a prior sync landed there) — only the config entry is dropped.
/// Moved from `crates/cli/src/sync_cmd.rs::remove_location`/`cmd_location_rm`.
///
/// # Errors
/// Returns an error when no location named `name` exists, or the config
/// can't be stored.
pub fn location_rm(
    catalog: &Path,
    name: &str,
    notices: &crate::notices::Notices,
) -> Result<(), ServiceError> {
    location_rm_impl(catalog, name, notices).map_err(ServiceError::from)
}

fn location_rm_impl(catalog: &Path, name: &str, notices: &crate::notices::Notices) -> Result<()> {
    let config = config_path(catalog, notices)?;
    let mut cfg = SyncConfig::load(&config)?;
    let before = cfg.locations.len();
    cfg.locations.retain(|l| l.name != name);
    anyhow::ensure!(
        cfg.locations.len() < before,
        "no sync location named '{name}' — see `maj sync location list`"
    );
    cfg.store(&config)
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
pub fn status(
    catalog_dir: &Path,
    notices: &crate::notices::Notices,
) -> Result<SyncStatusOutcome, ServiceError> {
    status_impl(catalog_dir, notices).map_err(ServiceError::from)
}

fn status_impl(catalog_dir: &Path, notices: &crate::notices::Notices) -> Result<SyncStatusOutcome> {
    ensure_catalog(catalog_dir)?;
    let cfg = SyncConfig::load(&config_path(catalog_dir, notices)?)?;
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
pub fn locations_list(
    catalog_dir: &Path,
    notices: &crate::notices::Notices,
) -> Result<LocationsOutcome, ServiceError> {
    locations_list_impl(catalog_dir, notices).map_err(ServiceError::from)
}

fn locations_list_impl(
    catalog_dir: &Path,
    notices: &crate::notices::Notices,
) -> Result<LocationsOutcome> {
    let cfg = SyncConfig::load(&config_path(catalog_dir, notices)?)?;
    Ok(LocationsOutcome {
        readonly: cfg.readonly,
        locations: cfg.locations,
    })
}

/// Narrows a transfer to one class of file — the service-level counterpart
/// of the CLI's clap-driven `--only` flag (kept in the CLI since it derives
/// `clap::ValueEnum`, which this crate has no reason to depend on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Only {
    Segments,
    Thumbs,
    Metadata,
    Vectors,
    Transcripts,
}

/// Narrows `plan` to one transfer class. `None` (no `--only`) returns the
/// plan unchanged. Moved from `crates/cli/src/sync_cmd.rs::filter_plan`.
fn filter_plan(plan: transfer::TransferPlan, only: Option<Only>) -> transfer::TransferPlan {
    let Some(only) = only else { return plan };
    let class = match only {
        Only::Segments => {
            return transfer::TransferPlan {
                segments: plan.segments,
                blobs: Vec::new(),
            };
        }
        Only::Thumbs => transfer::BlobClass::Thumbs,
        Only::Metadata => transfer::BlobClass::Metadata,
        Only::Vectors => transfer::BlobClass::Vectors,
        Only::Transcripts => transfer::BlobClass::Transcripts,
    };
    transfer::TransferPlan {
        segments: Vec::new(),
        blobs: plan
            .blobs
            .into_iter()
            .filter(|b| b.class == class)
            .collect(),
    }
}

/// Which side of a location pair is the transfer source. Push sends the
/// catalog's own state out to the location; pull fetches the location's
/// state into the catalog — otherwise identical plumbing (same plan/execute
/// call, same [`LocationRow`] shape), so [`location_row`] takes this instead
/// of duplicating itself per direction.
#[derive(Clone, Copy)]
enum Direction {
    Push,
    Pull,
}

/// One planned-but-failed transfer, as recorded in
/// [`transfer::TransferOutcome::failures`].
#[derive(Debug, serde::Serialize)]
pub struct FileFailure {
    pub path: PathBuf,
    pub error: String,
}

/// One location's push or pull result: it either ran — with an outcome that
/// may itself carry per-file failures — or it never ran at all, for one of
/// two distinct reasons: the location's mount wasn't there (`Skipped`) or the
/// transfer engine itself failed setting up or running the transfer
/// (`Failed`, e.g. `plan_transfer`/`execute`'s own error). Moved from
/// `crates/cli/src/sync_cmd.rs::LocationResult`.
#[derive(Debug, serde::Serialize)]
pub enum LocationRow {
    Ran {
        name: String,
        segments_copied: usize,
        segment_bytes: u64,
        blobs_copied: usize,
        blob_bytes: u64,
        /// `(machine, events)` this location contributed — only meaningful
        /// for a pull, which sums it into [`PullOutcome::applied_events`]/
        /// [`PullOutcome::machines`]; a push's rendering ignores it.
        events_added: Vec<(String, usize)>,
        failures: Vec<FileFailure>,
    },
    Skipped {
        name: String,
        reason: String,
    },
    Failed {
        name: String,
        error: String,
    },
}

impl LocationRow {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Ran { name, .. } | Self::Skipped { name, .. } | Self::Failed { name, .. } => name,
        }
    }
}

/// Runs one location's transfer in `direction`, converting an unreachable
/// location into a `Skipped` row and any `plan_transfer`/`execute` setup
/// error into a `Failed` row, rather than propagating an error out of this
/// function — one bad location must never abort every other location's
/// transfer. Moved from `crates/cli/src/sync_cmd.rs::transfer_one`.
fn location_row(
    catalog: &Path,
    loc: &Location,
    only: Option<Only>,
    direction: Direction,
) -> LocationRow {
    if !loc.path.is_dir() {
        return LocationRow::Skipped {
            name: loc.name.clone(),
            reason: format!("unreachable at {} — skipped", loc.path.display()),
        };
    }
    let (src, dst) = match direction {
        Direction::Push => (catalog, loc.path.as_path()),
        Direction::Pull => (loc.path.as_path(), catalog),
    };
    let run = transfer::plan_transfer(src, dst)
        .map(|plan| filter_plan(plan, only))
        .and_then(|plan| transfer::execute(src, dst, &plan));
    match run {
        Ok(outcome) => LocationRow::Ran {
            name: loc.name.clone(),
            segments_copied: outcome.segments_copied,
            segment_bytes: outcome.segment_bytes,
            blobs_copied: outcome.blobs_copied,
            blob_bytes: outcome.blob_bytes,
            events_added: outcome.events_added,
            failures: outcome
                .failures
                .into_iter()
                .map(|(path, error)| FileFailure { path, error })
                .collect(),
        },
        Err(e) => LocationRow::Failed {
            name: loc.name.clone(),
            error: e.to_string(),
        },
    }
}

/// True when not one requested location ever ran (every one was skipped or
/// failed outright before/during its own transfer setup) — the first of the
/// two conditions [`overall_failed`] checks. Named so a head (the CLI's
/// `check_exit_policy`, an MCP tool later) can report exactly which of the
/// two distinct exit-policy conditions fired, rather than re-deriving the
/// boolean from `rows` itself.
fn no_location_ran(rows: &[LocationRow]) -> bool {
    !rows.iter().any(|r| matches!(r, LocationRow::Ran { .. }))
}

/// The names of every location that DID run but had per-file failures
/// within its own transfer — the second of the two conditions
/// [`overall_failed`] checks. That location's other files still copied (the
/// engine records and continues past per-file errors), but a sync that
/// could not move everything must not exit 0 under cron.
fn failing_locations(rows: &[LocationRow]) -> Vec<&str> {
    rows.iter()
        .filter_map(|r| match r {
            LocationRow::Ran { name, failures, .. } if !failures.is_empty() => Some(name.as_str()),
            LocationRow::Ran { .. } | LocationRow::Skipped { .. } | LocationRow::Failed { .. } => {
                None
            }
        })
        .collect()
}

/// The exit-policy boolean shared by [`PushOutcome::overall_failed`] and
/// [`PullOutcome::overall_failed`]: nonzero when EVERY requested location
/// was skipped, failed outright, or otherwise never ran ([`no_location_ran`]),
/// or when a location that DID run had per-file failures within its own
/// transfer ([`failing_locations`]). Moved verbatim (as a boolean, rather
/// than the original `Result<()>` with its location-naming error message —
/// heads that need the exact failing-location text use [`failing_locations`]
/// directly) from `crates/cli/src/sync_cmd.rs::check_exit_policy`.
fn overall_failed(rows: &[LocationRow]) -> bool {
    no_location_ran(rows) || !failing_locations(rows).is_empty()
}

/// Everything `maj sync push` renders: one row per targeted location.
#[derive(Debug, serde::Serialize)]
pub struct PushOutcome {
    pub rows: Vec<LocationRow>,
}

impl PushOutcome {
    /// True when not one requested location ever ran. See [`no_location_ran`].
    #[must_use]
    pub fn no_location_ran(&self) -> bool {
        no_location_ran(&self.rows)
    }

    /// The names of every location that ran but had per-file failures. See
    /// [`failing_locations`].
    #[must_use]
    pub fn failing_locations(&self) -> Vec<&str> {
        failing_locations(&self.rows)
    }

    /// See [`overall_failed`].
    #[must_use]
    pub fn overall_failed(&self) -> bool {
        overall_failed(&self.rows)
    }
}

/// `maj sync push` request: which location(s) to target and which transfer
/// class to narrow to (`None` for both flags means every location, every
/// class).
pub struct PushRequest<'a> {
    pub location: Option<&'a str>,
    pub only: Option<Only>,
}

/// `maj sync push`: replicate everything this catalog has (segments +
/// blobs) to configured locations. Refuses outright when this machine is a
/// read-only sync member; otherwise every reachable location gets its own
/// independent transfer attempt, and per-file failures within a transfer
/// are recorded in that location's row rather than aborting it — see
/// [`transfer::TransferOutcome`]. Moved from
/// `crates/cli/src/sync_cmd.rs::cmd_push`.
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, this machine is
/// readonly, or no sync locations are configured (or none match
/// `req.location`). Per-location failures are reported inside the returned
/// outcome, never as an `Err` — see [`PushOutcome::overall_failed`].
pub fn push(
    catalog: &Path,
    req: &PushRequest<'_>,
    notices: &crate::notices::Notices,
) -> Result<PushOutcome, ServiceError> {
    push_impl(catalog, req, notices).map_err(ServiceError::from)
}

fn push_impl(
    catalog: &Path,
    req: &PushRequest<'_>,
    notices: &crate::notices::Notices,
) -> Result<PushOutcome> {
    ensure_catalog(catalog)?;
    let config = config_path(catalog, notices)?;
    let cfg = SyncConfig::load(&config)?;
    anyhow::ensure!(
        !cfg.readonly,
        "readonly = true in {} — this machine is a read-only sync member and never pushes — set `readonly = false` there to push from this machine",
        config.display()
    );
    let targets = resolve_targets(&cfg, req.location)?;
    let rows = targets
        .into_iter()
        .map(|loc| location_row(catalog, loc, req.only, Direction::Push))
        .collect();
    Ok(PushOutcome { rows })
}

/// Everything `maj sync pull` renders: one row per targeted location, plus
/// the tally `cmd_pull`'s summary reports — events newly landed and which
/// machines they came from (aggregated across locations; see
/// `summarize_pull`'s original doc for why a landed event can never be
/// double-counted across two locations), and how many blobs landed.
#[derive(Debug, serde::Serialize)]
pub struct PullOutcome {
    pub rows: Vec<LocationRow>,
    pub applied_events: usize,
    pub machines: Vec<String>,
    pub blobs_fetched: usize,
}

impl PullOutcome {
    /// True when not one requested location ever ran. See [`no_location_ran`].
    #[must_use]
    pub fn no_location_ran(&self) -> bool {
        no_location_ran(&self.rows)
    }

    /// The names of every location that ran but had per-file failures. See
    /// [`failing_locations`].
    #[must_use]
    pub fn failing_locations(&self) -> Vec<&str> {
        failing_locations(&self.rows)
    }

    /// See [`overall_failed`].
    #[must_use]
    pub fn overall_failed(&self) -> bool {
        overall_failed(&self.rows)
    }
}

/// `maj sync pull` request: mirrors [`PushRequest`] (no readonly gate —
/// unlike push, a read-only member still needs to pull).
pub struct PullRequest<'a> {
    pub location: Option<&'a str>,
    pub only: Option<Only>,
}

/// Aggregates `rows` into `(applied_events, machines, blobs_fetched)`. A
/// pulled event can never be double-counted across two locations holding
/// the same segment tail: `events_added` is already aggregated per machine
/// within one location's own outcome (the transfer engine's own accounting,
/// counted from the destination after each copy lands), and the second
/// location's own plan is measured against that already-caught-up
/// destination — its range has shrunk to nothing by the time it's diffed,
/// so it contributes nothing further. This only sums across locations.
fn summarize_pull(rows: &[LocationRow]) -> (usize, Vec<String>, usize) {
    let mut per_machine: BTreeMap<&str, usize> = BTreeMap::new();
    let mut blobs_fetched = 0usize;
    for r in rows {
        if let LocationRow::Ran {
            blobs_copied,
            events_added,
            ..
        } = r
        {
            blobs_fetched += blobs_copied;
            for (machine, n) in events_added {
                *per_machine.entry(machine.as_str()).or_default() += n;
            }
        }
    }
    let applied = per_machine.values().sum();
    let machines = per_machine.into_keys().map(str::to_string).collect();
    (applied, machines, blobs_fetched)
}

/// Internal carrier for [`pull_impl`]'s early return when the local-catalog
/// apply fails after every location's transfer already completed: downcast
/// back out of the `anyhow` chain by [`pull`] and turned into
/// [`ServiceError::SyncPullApplyFailed`] so the completed transfer rows
/// survive to the caller instead of being silently dropped by the early
/// `Err`. Not part of the public API — [`pull`]'s callers only ever see the
/// typed [`ServiceError`] variant. Mirrors `para::PartialArchiveFailure`.
#[derive(Debug)]
struct PullApplyFailure {
    rows: Vec<LocationRow>,
    source: anyhow::Error,
}

impl std::fmt::Display for PullApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for PullApplyFailure {}

/// `maj sync pull`: fetch everything configured locations have that this
/// catalog doesn't (segments + blobs), then apply the newly landed events to
/// the local sqlite catalog. Like [`push`], refuses outright when there's no
/// catalog at `catalog`; unlike [`push`], there is no readonly refusal.
///
/// Order matters: transfer every location, THEN apply. A per-file blob
/// failure at one location must never block already-landed segments from
/// being applied — opening the sqlite catalog applies past its saved
/// cursor, so the open below IS the apply; there is no separate step to
/// call. If THAT apply step itself fails, the transfer rows already
/// completed must still reach the caller — see
/// [`ServiceError::SyncPullApplyFailed`]. Moved from
/// `crates/cli/src/sync_cmd.rs::cmd_pull`.
///
/// # Errors
/// Returns an error when there's no catalog at `catalog`, no sync locations
/// are configured (or none match `req.location`), or the local sqlite
/// catalog can't be opened/synced — the latter as
/// [`ServiceError::SyncPullApplyFailed`], carrying the transfer rows that
/// already completed. Per-location transfer failures are reported inside
/// the returned outcome, never as an `Err` — see
/// [`PullOutcome::overall_failed`].
pub fn pull(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    req: &PullRequest<'_>,
    notices: &crate::notices::Notices,
) -> Result<PullOutcome, ServiceError> {
    pull_impl(catalog, machine_id, author, req, notices).map_err(|err| {
        match err.downcast::<PullApplyFailure>() {
            Ok(partial) => ServiceError::SyncPullApplyFailed {
                rows: partial.rows,
                source: partial.source,
            },
            Err(err) => ServiceError::from(err),
        }
    })
}

fn pull_impl(
    catalog: &Path,
    machine_id: &str,
    author: &str,
    req: &PullRequest<'_>,
    notices: &crate::notices::Notices,
) -> Result<PullOutcome> {
    ensure_catalog(catalog)?;
    let cfg = SyncConfig::load(&config_path(catalog, notices)?)?;
    let targets = resolve_targets(&cfg, req.location)?;
    let rows: Vec<LocationRow> = targets
        .into_iter()
        .map(|loc| location_row(catalog, loc, req.only, Direction::Pull))
        .collect();

    // Apply pulled events to the local catalog BEFORE tallying the summary
    // below: a per-file blob failure elsewhere must still let already-
    // landed segments become searchable rather than leaving them stranded
    // on disk unapplied. A failure here must not lose `rows` — every
    // location's transfer already genuinely completed by this point.
    if let Err(source) = apply_pulled_events(catalog, machine_id, author) {
        return Err(anyhow::Error::new(PullApplyFailure { rows, source }));
    }

    let (applied_events, machines, blobs_fetched) = summarize_pull(&rows);
    Ok(PullOutcome {
        rows,
        applied_events,
        machines,
        blobs_fetched,
    })
}

/// Opens the local sqlite catalog, applying past its saved cursor — the
/// open IS the apply; there is no separate step to call. Split out of
/// [`pull_impl`] purely so that function can attach `rows` to this specific
/// failure via [`PullApplyFailure`] without the `?` operator discarding
/// them.
fn apply_pulled_events(catalog: &Path, machine_id: &str, author: &str) -> Result<()> {
    let app = FsApp::open(catalog, machine_id, author)?;
    crate::catalog::open_catalog(&app, catalog)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notices::Notices;

    #[test]
    fn status_of_a_catalog_with_no_locations_errors_naming_the_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        std::fs::create_dir_all(root.join("events")).expect("mkdir");
        let err = status(&root, &Notices::new()).expect_err("no locations must error");
        assert!(err.to_string().contains("no sync locations configured"));
    }

    #[test]
    fn status_of_a_nonexistent_catalog_names_catalog_init() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("nope");
        let err = status(&root, &Notices::new()).expect_err("missing catalog must error");
        assert!(err.to_string().contains("maj catalog init"));
    }

    #[test]
    fn locations_list_of_an_unconfigured_catalog_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = locations_list(dir.path(), &Notices::new()).expect("locations_list");
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

#[cfg(test)]
mod location_add_rm_tests {
    use super::*;
    use crate::notices::Notices;

    fn catalog_dir(base: &Path) -> PathBuf {
        let root = base.join("cat");
        std::fs::create_dir_all(&root).expect("mkdir");
        root
    }

    #[test]
    fn add_rejects_duplicate_names_and_rm_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_dir(dir.path());
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        location_add(&catalog, "nas", &loc, &Notices::new()).expect("first add");
        assert!(
            loc.join("events").is_dir() && loc.join("blobs").is_dir(),
            "add initializes the events/ + blobs/ skeleton"
        );
        let err = location_add(&catalog, "nas", &loc, &Notices::new()).expect_err("dup must fail");
        assert!(err.to_string().contains("already configured"));
        let err = location_rm(&catalog, "ghost", &Notices::new()).expect_err("unknown rm");
        assert!(err.to_string().contains("no sync location named"));
    }

    #[test]
    fn add_rejects_an_unreachable_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_dir(dir.path());
        let err = location_add(
            &catalog,
            "nas",
            &dir.path().join("missing"),
            &Notices::new(),
        )
        .expect_err("unreachable path must fail");
        assert!(err.to_string().contains("not an accessible directory"));
    }

    #[test]
    fn add_rejects_an_empty_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_dir(dir.path());
        let err = location_add(&catalog, "  ", dir.path(), &Notices::new())
            .expect_err("empty name must fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn add_stores_a_canonicalized_absolute_trimmed_location() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_dir(dir.path());
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        location_add(&catalog, "  nas  ", &loc, &Notices::new()).expect("add");
        let cfg = SyncConfig::load(&config_path(&catalog, &Notices::new()).expect("config_path"))
            .expect("load");
        assert_eq!(cfg.locations[0].name, "nas");
        assert!(cfg.locations[0].path.is_absolute());
        assert_eq!(
            cfg.locations[0].path,
            loc.canonicalize().expect("canonicalize")
        );
    }

    #[test]
    fn rm_removes_a_configured_location_without_touching_its_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog_dir(dir.path());
        let loc = dir.path().join("remote");
        std::fs::create_dir(&loc).expect("mkdir");
        location_add(&catalog, "nas", &loc, &Notices::new()).expect("add");
        location_rm(&catalog, "nas", &Notices::new()).expect("rm");
        let cfg = SyncConfig::load(&config_path(&catalog, &Notices::new()).expect("config_path"))
            .expect("load");
        assert!(cfg.locations.is_empty());
        assert!(loc.join("events").is_dir(), "rm must not touch the files");
    }
}

#[cfg(test)]
mod push_pull_tests {
    use super::*;
    use crate::app::FsApp;
    use crate::notices::Notices;
    use majestical_core::event::{Op, ParaKind};

    fn init_catalog(base: &Path, name: &str, machine: &str) -> (FsApp, PathBuf) {
        let root = base.join(name);
        let app = FsApp::init(&root, machine, machine).expect("init");
        (app, root)
    }

    #[test]
    fn overall_failed_is_true_when_every_location_is_skipped_or_failed() {
        let rows = vec![
            LocationRow::Skipped {
                name: "a".into(),
                reason: "unreachable".into(),
            },
            LocationRow::Failed {
                name: "b".into(),
                error: "boom".into(),
            },
        ];
        assert!(overall_failed(&rows));
    }

    #[test]
    fn overall_failed_is_false_when_a_location_ran_cleanly() {
        let rows = vec![LocationRow::Ran {
            name: "a".into(),
            segments_copied: 1,
            segment_bytes: 10,
            blobs_copied: 0,
            blob_bytes: 0,
            events_added: vec![],
            failures: vec![],
        }];
        assert!(!overall_failed(&rows));
    }

    #[test]
    fn overall_failed_is_true_when_a_ran_location_has_per_file_failures() {
        let rows = vec![LocationRow::Ran {
            name: "a".into(),
            segments_copied: 1,
            segment_bytes: 10,
            blobs_copied: 0,
            blob_bytes: 0,
            events_added: vec![],
            failures: vec![FileFailure {
                path: "x".into(),
                error: "boom".into(),
            }],
        }];
        assert!(overall_failed(&rows));
    }

    #[test]
    fn summarize_pull_sums_events_and_blobs_across_locations() {
        let rows = vec![
            LocationRow::Ran {
                name: "a".into(),
                segments_copied: 0,
                segment_bytes: 0,
                blobs_copied: 2,
                blob_bytes: 0,
                events_added: vec![("m1".into(), 3)],
                failures: vec![],
            },
            LocationRow::Ran {
                name: "b".into(),
                segments_copied: 0,
                segment_bytes: 0,
                blobs_copied: 5,
                blob_bytes: 0,
                events_added: vec![("m1".into(), 4), ("m2".into(), 1)],
                failures: vec![],
            },
        ];
        let (applied, machines, blobs) = summarize_pull(&rows);
        assert_eq!(applied, 8, "events must sum across locations");
        assert_eq!(blobs, 7, "blobs must sum across locations");
        assert_eq!(machines, vec!["m1".to_string(), "m2".to_string()]);
    }

    #[test]
    fn push_of_an_unconfigured_catalog_errors_naming_the_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_app, root) = init_catalog(dir.path(), "cat", "m1");
        let err = push(
            &root,
            &PushRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect_err("no locations must error");
        assert!(err.to_string().contains("no sync locations configured"));
    }

    #[test]
    fn push_refuses_when_this_machine_is_readonly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_app, root) = init_catalog(dir.path(), "cat", "m1");
        // Directly write a readonly=true config with one location — the
        // refusal must happen before any location is even looked at.
        let mut cfg = SyncConfig::load(&config_path(&root, &Notices::new()).expect("config_path"))
            .expect("load");
        cfg.readonly = true;
        cfg.locations.push(Location {
            name: "nas".into(),
            path: dir.path().to_path_buf(),
            extra: toml::Table::new(),
        });
        cfg.store(&config_path(&root, &Notices::new()).expect("config_path"))
            .expect("store");
        let err = push(
            &root,
            &PushRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect_err("readonly must refuse");
        assert!(err.to_string().contains("read-only sync member"));
    }

    #[test]
    fn push_reports_a_skipped_row_for_an_unreachable_location() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (_app, root) = init_catalog(dir.path(), "cat", "m1");
        let mut cfg = SyncConfig::load(&config_path(&root, &Notices::new()).expect("config_path"))
            .expect("load");
        cfg.locations.push(Location {
            name: "gone".into(),
            path: dir.path().join("does-not-exist"),
            extra: toml::Table::new(),
        });
        cfg.store(&config_path(&root, &Notices::new()).expect("config_path"))
            .expect("store");
        let outcome = push(
            &root,
            &PushRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect("push");
        assert_eq!(outcome.rows.len(), 1);
        assert!(matches!(outcome.rows[0], LocationRow::Skipped { .. }));
        assert!(outcome.overall_failed(), "an all-skipped push must fail");
    }

    #[test]
    fn push_then_pull_round_trips_an_event_through_a_location() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut source, source_root) = init_catalog(dir.path(), "source", "m1");
        source
            .emit(vec![Op::ParaNodeCreate {
                node: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            }])
            .expect("emit");

        let loc = dir.path().join("shuttle");
        std::fs::create_dir_all(&loc).expect("mkdir");
        location_add(&source_root, "shuttle", &loc, &Notices::new()).expect("add");
        let push_outcome = push(
            &source_root,
            &PushRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect("push");
        assert!(!push_outcome.overall_failed());
        assert!(matches!(push_outcome.rows[0], LocationRow::Ran { .. }));

        let dest_root = dir.path().join("dest");
        std::fs::create_dir_all(&dest_root).expect("mkdir");
        let dest_app = FsApp::init(&dest_root, "m2", "m2").expect("init dest");
        location_add(&dest_root, "shuttle", &loc, &Notices::new()).expect("add on dest");
        let pull_outcome = pull(
            &dest_root,
            "m2",
            "m2",
            &PullRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect("pull");
        assert!(!pull_outcome.overall_failed());
        assert_eq!(pull_outcome.applied_events, 1);
        assert_eq!(pull_outcome.machines, vec!["m1".to_string()]);

        let projection = dest_app.projection().expect("projection");
        assert!(
            projection.para_node("01ARZ3NDEKTSV4RRFFQ69G5FAV").is_some(),
            "the pulled event must be visible in the destination's own projection"
        );
    }

    /// Pins `ServiceError::SyncPullApplyFailed`'s shape: when the local
    /// sqlite apply fails AFTER every location's transfer already
    /// completed, the completed transfer rows must still reach the caller
    /// rather than being silently dropped by the early `Err`. Forces the
    /// apply to fail deterministically by pre-creating a directory at the
    /// exact path `open_synced` will try to open as a sqlite file —
    /// `Connection::open` on a directory fails every time, on every
    /// platform, with no reliance on corrupting real bytes.
    #[test]
    fn pull_carries_completed_rows_when_the_local_apply_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut source, source_root) = init_catalog(dir.path(), "source", "m1");
        source
            .emit(vec![Op::ParaNodeCreate {
                node: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            }])
            .expect("emit");

        let loc = dir.path().join("shuttle");
        std::fs::create_dir_all(&loc).expect("mkdir");
        location_add(&source_root, "shuttle", &loc, &Notices::new()).expect("add");
        push(
            &source_root,
            &PushRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect("push");

        let dest_root = dir.path().join("dest");
        std::fs::create_dir_all(&dest_root).expect("mkdir");
        FsApp::init(&dest_root, "m2", "m2").expect("init dest");
        location_add(&dest_root, "shuttle", &loc, &Notices::new()).expect("add on dest");

        // Block the apply: put a directory where `open_synced` expects to
        // open a sqlite file.
        let paths =
            crate::state_dir::catalog_paths(&dest_root, &Notices::new()).expect("catalog_paths");
        std::fs::create_dir_all(&paths.db_path).expect("mkdir catalog.db");

        let err = pull(
            &dest_root,
            "m2",
            "m2",
            &PullRequest {
                location: None,
                only: None,
            },
            &Notices::new(),
        )
        .expect_err("apply must fail against a directory in place of catalog.db");
        let ServiceError::SyncPullApplyFailed { rows, source } = err else {
            panic!("expected SyncPullApplyFailed, got a different ServiceError variant");
        };
        assert_eq!(
            rows.len(),
            1,
            "the completed transfer row must survive the apply failure"
        );
        assert!(matches!(rows[0], LocationRow::Ran { .. }));
        assert!(!source.to_string().is_empty());
    }
}
