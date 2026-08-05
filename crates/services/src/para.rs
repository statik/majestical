//! PARA node reference resolution, shared by `maj para` and `search`'s
//! `para:` filter. Moved verbatim from `crates/cli/src/commands.rs`.
use crate::app::FsApp;
use crate::catalog::open_catalog;
use crate::error::ServiceError;
use anyhow::{Context, Result};
use majestical_core::event::{Op, ParaKind};
use majestical_core::projection::Projection;
use std::path::{Path, PathBuf};

/// # Errors
/// Returns an error if `kind` isn't one of `project`, `area`, `resource`, or
/// `archive`.
pub fn parse_kind(kind: &str) -> Result<ParaKind> {
    match kind {
        "project" => Ok(ParaKind::Project),
        "area" => Ok(ParaKind::Area),
        "resource" => Ok(ParaKind::Resource),
        "archive" => Ok(ParaKind::Archive),
        other => {
            anyhow::bail!("unknown PARA kind '{other}' — one of: project, area, resource, archive")
        }
    }
}

/// Resolves `<kind>/<name>` or a raw node ULID against non-archived nodes.
/// The non-archived restriction applies only to the `<kind>/<name>` form; a
/// raw node id resolves an archived node too (intentional — once a node is
/// archived, its id is the only way left to address it).
///
/// # Errors
/// Returns an error if `reference` is neither a known node id nor a
/// `<kind>/<name>` pair naming exactly one active node.
pub fn resolve_para_node(projection: &Projection, reference: &str) -> Result<String> {
    if projection.para_node(reference).is_some() {
        return Ok(reference.to_string());
    }
    let Some((kind_str, name)) = reference.split_once('/') else {
        anyhow::bail!(
            "unknown PARA node '{reference}' — use <kind>/<name> or a node id from `maj para list`"
        );
    };
    let kind = parse_kind(kind_str)?;
    let matches: Vec<&String> = projection
        .para_nodes()
        .filter(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name))
        .map(|(id, _)| id)
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no active PARA node '{reference}' — see `maj para list`"),
        [id] => Ok((*id).clone()),
        many => anyhow::bail!(
            "'{reference}' is ambiguous (concurrent creates); use a node id: {}",
            many.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// A freshly created (or resolved) PARA node's id.
#[derive(Debug, serde::Serialize)]
pub struct NodeId(pub String);

/// `maj para add`: creates a node, rejecting a duplicate non-archived
/// `(kind, name)` — two active nodes with the same reference would be
/// indistinguishable to [`resolve_para_node`]. Moved from
/// `crates/cli/src/commands.rs::cmd_para_add`.
///
/// # Errors
/// Returns an error if `kind_str` isn't a known PARA kind, an active node
/// already exists at `<kind_str>/<name>`, or the event log can't be read or
/// appended to.
pub fn add(app: &mut FsApp, kind_str: &str, name: &str) -> Result<NodeId, ServiceError> {
    add_impl(app, kind_str, name).map_err(ServiceError::from)
}

fn add_impl(app: &mut FsApp, kind_str: &str, name: &str) -> Result<NodeId> {
    let kind = parse_kind(kind_str)?;
    let projection = app.projection()?;
    let duplicate = projection
        .para_nodes()
        .any(|(_, st)| !st.archived() && st.kind() == Some(kind) && st.name() == Some(name));
    anyhow::ensure!(
        !duplicate,
        "a PARA node '{kind_str}/{name}' already exists — see `maj para list`"
    );
    let node_id = ulid::Ulid::generate().to_string();
    app.emit(vec![Op::ParaNodeCreate {
        node: node_id.clone(),
        kind,
        name: name.to_string(),
    }])?;
    Ok(NodeId(node_id))
}

/// `maj para rename`: renames a node (last-write-wins across machines).
/// Moved from `crates/cli/src/commands.rs::cmd_para_rename`.
///
/// # Errors
/// Returns an error if `node` doesn't resolve to a known active node, or the
/// event log can't be read or appended to.
pub fn rename(app: &mut FsApp, node: &str, name: &str) -> Result<(), ServiceError> {
    rename_impl(app, node, name).map_err(ServiceError::from)
}

fn rename_impl(app: &mut FsApp, node: &str, name: &str) -> Result<()> {
    let projection = app.projection()?;
    let node_id = resolve_para_node(&projection, node)?;
    app.emit(vec![Op::ParaNodeRename {
        node: node_id,
        name: name.to_string(),
    }])?;
    Ok(())
}

/// One root's materialized-directory move, or what would happen to it under
/// `--dry-run`.
#[derive(Debug, serde::Serialize)]
pub struct ArchiveMove {
    pub from: PathBuf,
    pub to: PathBuf,
    pub status: MoveStatus,
}

/// What happened (or would happen) to one root's materialized directory.
#[derive(serde::Serialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum MoveStatus {
    /// Actually moved this run.
    Moved,
    /// Source already gone and target already present — an earlier partial
    /// run already moved this root; skipped so a re-run converges.
    AlreadyArchived,
    /// `--dry-run`: this is what would happen, nothing touched on disk.
    Planned,
}

/// Everything [`archive`] did (or planned). `executed` is `false` in
/// dry-run, in which case the `ParaNodeArchive` event was NOT emitted —
/// callers that need to gate a confirm step (e.g. an MCP tool) on whether
/// the archive actually happened should check this rather than re-deriving
/// it from `moves`.
#[derive(Debug, serde::Serialize)]
pub struct ArchiveOutcome {
    pub moves: Vec<ArchiveMove>,
    pub executed: bool,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// Internal carrier for [`archive_impl`]'s early return when a multi-root
/// run fails partway through: downcast back out of the `anyhow` chain by
/// [`archive`] and turned into [`ServiceError::ParaArchivePartial`] so the
/// moves already completed survive to the caller instead of being silently
/// dropped by the early `Err`. Not part of the public API — [`archive`]'s
/// callers only ever see the typed [`ServiceError`] variant.
#[derive(Debug)]
struct PartialArchiveFailure {
    moves: Vec<ArchiveMove>,
    source: anyhow::Error,
}

impl std::fmt::Display for PartialArchiveFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl std::error::Error for PartialArchiveFailure {}

/// `maj para archive`: archives a node. With `--root`s, each root's
/// materialized directory (`<root>/<KindDir>/<name>`) is moved to
/// `<root>/Archives/<name>` before the archive event is emitted; with no
/// roots, only the event is emitted (skipped in `--dry-run`) — `moves` is
/// empty in that case, which the CLI uses to tell the two shapes apart when
/// rendering. Moved from `crates/cli/src/commands.rs::cmd_para_archive`.
///
/// If a move fails partway through a multi-root run, the roots already
/// moved stay moved and the archive event is NOT emitted. A root whose
/// source is gone and target already exists is treated as already archived
/// and skipped rather than re-erroring — so re-running the exact same
/// command converges instead of failing forever on the root that succeeded
/// last time.
///
/// # Errors
/// Returns an error if `node` doesn't resolve, the resolved node has no
/// recorded kind/name, the node is of kind `archive` (already under
/// `Archives/`), a root's source directory is missing (not `--dry-run`,
/// not already archived), a root's archive target already exists, or a
/// filesystem operation fails.
pub fn archive(
    app: &mut FsApp,
    node: &str,
    roots: &[PathBuf],
    dry_run: bool,
) -> Result<ArchiveOutcome, ServiceError> {
    archive_impl(app, node, roots, dry_run).map_err(|err| {
        match err.downcast::<PartialArchiveFailure>() {
            Ok(partial) => ServiceError::ParaArchivePartial {
                moves: partial.moves,
                source: partial.source,
            },
            Err(err) => ServiceError::from(err),
        }
    })
}

/// Classifies (or performs) one root's move: already archived, planned
/// (`--dry-run`), or actually moved. Split out of [`archive_impl`] so the
/// loop there can catch a per-root failure and attach the moves already
/// completed for the OTHER roots before propagating it — see
/// [`PartialArchiveFailure`].
///
/// # Errors
/// Returns an error if `dry_run` is false and the source directory is
/// missing, the archive target already exists, or a filesystem operation
/// fails.
fn archive_one_root(root: &Path, kind: ParaKind, name: &str, dry_run: bool) -> Result<ArchiveMove> {
    let source = root.join(kind.dir_name()).join(name);
    let archives_dir = root.join("Archives");
    let target = archives_dir.join(name);
    // Source gone, target present: an earlier partial run already moved
    // this root. Skip rather than erroring, so a plain re-run of the same
    // command converges instead of failing on the root that already
    // succeeded.
    if !source.is_dir() && target.is_dir() {
        return Ok(ArchiveMove {
            from: source,
            to: target,
            status: MoveStatus::AlreadyArchived,
        });
    }
    if dry_run {
        return Ok(ArchiveMove {
            from: source,
            to: target,
            status: MoveStatus::Planned,
        });
    }
    anyhow::ensure!(
        source.is_dir(),
        "source directory {} does not exist — nothing to archive",
        source.display()
    );
    anyhow::ensure!(
        !target.exists(),
        "archive target {} already exists",
        target.display()
    );
    std::fs::create_dir_all(&archives_dir)
        .with_context(|| format!("creating {}", archives_dir.display()))?;
    std::fs::rename(&source, &target)
        .with_context(|| format!("moving {} to {}", source.display(), target.display()))?;
    Ok(ArchiveMove {
        from: source,
        to: target,
        status: MoveStatus::Moved,
    })
}

fn archive_impl(
    app: &mut FsApp,
    node: &str,
    roots: &[PathBuf],
    dry_run: bool,
) -> Result<ArchiveOutcome> {
    let projection = app.projection()?;
    let node_id = resolve_para_node(&projection, node)?;
    let state = projection
        .para_node(&node_id)
        .context("resolved node vanished from the projection")?;
    let Some(kind) = state.kind() else {
        anyhow::bail!("PARA node {node_id} has no kind recorded — its create event may be missing");
    };
    let Some(name) = state.name() else {
        anyhow::bail!("PARA node {node_id} has no name recorded — its create event may be missing");
    };
    let name = name.to_string();

    if roots.is_empty() {
        if !dry_run {
            app.emit(vec![Op::ParaNodeArchive { node: node_id }])?;
        }
        return Ok(ArchiveOutcome {
            moves: Vec::new(),
            executed: !dry_run,
            notices: app.notices().drain(),
        });
    }
    // A node of kind `archive` already materializes under `Archives/` (its
    // own `dir_name()`), so source and target would be the same path for
    // every root — reject up front rather than reporting a no-op "move" in
    // dry-run and a target-already-exists error in the real run.
    anyhow::ensure!(
        kind != ParaKind::Archive,
        "node of kind archive is already under Archives/ — nothing to move"
    );

    let mut moves = Vec::new();
    for root in roots {
        match archive_one_root(root, kind, &name, dry_run) {
            Ok(mv) => moves.push(mv),
            // A root failing partway through a multi-root run must not
            // silently drop the OTHER roots' completed moves — those are
            // real filesystem mutations a head still needs to report.
            Err(source) => return Err(anyhow::Error::new(PartialArchiveFailure { moves, source })),
        }
    }

    if !dry_run {
        // Every root's directory move already succeeded by this point; an
        // emit failure here still must not lose those completed moves.
        if let Err(source) = app.emit(vec![Op::ParaNodeArchive { node: node_id }]) {
            return Err(anyhow::Error::new(PartialArchiveFailure { moves, source }));
        }
    }
    Ok(ArchiveOutcome {
        moves,
        executed: !dry_run,
        notices: app.notices().drain(),
    })
}

/// One PARA node row, as returned by the sqlite catalog's `para_nodes`
/// query.
#[derive(serde::Serialize)]
pub struct ParaNodeRow {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub archived: bool,
}

/// Everything `maj para list` renders.
#[derive(serde::Serialize)]
pub struct ParaOutcome {
    pub nodes: Vec<ParaNodeRow>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// `maj para list`: every PARA node the catalog has ever created.
///
/// # Errors
/// Returns an error if the sqlite catalog can't be opened/synced or the
/// para-nodes query fails.
pub fn para_list(app: &FsApp, catalog_dir: &Path) -> Result<ParaOutcome, ServiceError> {
    para_list_impl(app, catalog_dir).map_err(ServiceError::from)
}

fn para_list_impl(app: &FsApp, catalog_dir: &Path) -> Result<ParaOutcome> {
    let (db, _projection) = open_catalog(app, catalog_dir)?;
    let nodes = db.para_nodes().context("querying para nodes")?;
    let nodes = nodes
        .into_iter()
        .map(|(id, kind, name, archived)| ParaNodeRow {
            id,
            kind,
            name,
            archived,
        })
        .collect();
    Ok(ParaOutcome {
        nodes,
        notices: app.notices().drain(),
    })
}

#[cfg(test)]
mod para_list_tests {
    use super::*;
    use majestical_core::event::{Op, ParaKind};

    #[test]
    fn para_list_reports_every_created_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = FsApp::init(&root, "m1", "m1").expect("init");
        app.emit(vec![Op::ParaNodeCreate {
            node: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            kind: ParaKind::Project,
            name: "client-x".into(),
        }])
        .expect("emit");
        let outcome = para_list(&app, &root).expect("para_list");
        assert_eq!(outcome.nodes.len(), 1);
        let row = &outcome.nodes[0];
        assert_eq!(row.name, "client-x");
        assert_eq!(row.kind, "project");
        assert!(!row.archived);
    }
}

#[cfg(test)]
mod mutation_tests {
    use super::*;

    fn init_app(dir: &std::path::Path) -> FsApp {
        FsApp::init(&dir.join("cat"), "m1", "m1").expect("init")
    }

    #[test]
    fn add_creates_a_node_addressable_by_kind_and_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let projection = app.projection().expect("projection");
        assert_eq!(
            resolve_para_node(&projection, "project/client-x").expect("resolve"),
            id
        );
    }

    #[test]
    fn add_rejects_a_duplicate_active_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        add(&mut app, "project", "client-x").expect("first add");
        let err = add(&mut app, "project", "client-x").expect_err("must fail");
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn rename_updates_the_node_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        rename(&mut app, &id, "client-y").expect("rename");
        let projection = app.projection().expect("projection");
        let state = projection.para_node(&id).expect("node");
        assert_eq!(state.name(), Some("client-y"));
    }

    #[test]
    fn rename_of_an_unknown_node_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let err = rename(&mut app, "project/nope", "y").expect_err("must fail");
        assert!(err.to_string().contains("no active PARA node"));
    }

    #[test]
    fn rename_of_a_reference_with_no_slash_and_no_matching_id_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let err = rename(&mut app, "not-a-node-id", "y").expect_err("must fail");
        assert!(err.to_string().contains("unknown PARA node"));
    }

    #[test]
    fn archive_with_no_roots_emits_the_event_and_reports_no_moves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let outcome = archive(&mut app, &id, &[], false).expect("archive");
        assert!(outcome.moves.is_empty());
        assert!(outcome.executed);
        let projection = app.projection().expect("projection");
        assert!(projection.para_node(&id).expect("node").archived());
    }

    #[test]
    fn archive_dry_run_with_no_roots_does_not_emit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let outcome = archive(&mut app, &id, &[], true).expect("archive");
        assert!(outcome.moves.is_empty());
        assert!(!outcome.executed);
        let projection = app.projection().expect("projection");
        assert!(
            !projection.para_node(&id).expect("node").archived(),
            "dry run must not archive"
        );
    }

    #[test]
    fn archive_with_a_root_moves_the_materialized_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let materialized = tempfile::tempdir().expect("tempdir");
        let node_dir = materialized.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&node_dir).expect("mkdir");
        std::fs::write(node_dir.join("a.txt"), b"hello").expect("write");

        let outcome =
            archive(&mut app, &id, &[materialized.path().to_path_buf()], false).expect("archive");
        assert_eq!(outcome.moves.len(), 1);
        assert_eq!(outcome.moves[0].status, MoveStatus::Moved);
        assert!(!node_dir.exists());
        let archived = materialized.path().join("Archives").join("client-x");
        assert!(archived.join("a.txt").is_file());
        assert!(outcome.executed);
    }

    #[test]
    fn archive_dry_run_with_a_root_plans_without_moving() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let materialized = tempfile::tempdir().expect("tempdir");
        let node_dir = materialized.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&node_dir).expect("mkdir");

        let outcome =
            archive(&mut app, &id, &[materialized.path().to_path_buf()], true).expect("archive");
        assert_eq!(outcome.moves[0].status, MoveStatus::Planned);
        assert!(node_dir.is_dir(), "dry run must not move");
        assert!(!outcome.executed);
    }

    #[test]
    fn archive_skips_a_root_already_archived_and_moves_the_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");

        let root1 = tempfile::tempdir().expect("tempdir");
        let root1_archived = root1.path().join("Archives").join("client-x");
        std::fs::create_dir_all(&root1_archived).expect("mkdir");

        let root2 = tempfile::tempdir().expect("tempdir");
        let root2_source = root2.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&root2_source).expect("mkdir");
        std::fs::write(root2_source.join("b.txt"), b"world").expect("write");

        let outcome = archive(
            &mut app,
            &id,
            &[root1.path().to_path_buf(), root2.path().to_path_buf()],
            false,
        )
        .expect("archive");
        assert_eq!(outcome.moves[0].status, MoveStatus::AlreadyArchived);
        assert_eq!(outcome.moves[1].status, MoveStatus::Moved);
        assert!(!root2_source.exists());
        assert!(outcome.executed);
    }

    #[test]
    fn archive_of_an_archive_kind_node_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "archive", "old-stuff").expect("add");
        let root = tempfile::tempdir().expect("tempdir");
        let err =
            archive(&mut app, &id, &[root.path().to_path_buf()], false).expect_err("must fail");
        assert!(err.to_string().contains("already under Archives/"));
    }

    /// A missing source directory (not already-archived, not dry-run) must
    /// fail the whole call and must NOT emit `ParaNodeArchive` — a partial
    /// multi-root run's later roots stay un-erased and the node stays
    /// non-archived so a corrected re-run can still act on it.
    #[test]
    fn archive_of_a_missing_source_errors_and_does_not_emit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");
        let root = tempfile::tempdir().expect("tempdir");
        let err =
            archive(&mut app, &id, &[root.path().to_path_buf()], false).expect_err("must fail");
        let ServiceError::ParaArchivePartial { source, .. } = err else {
            panic!("expected ParaArchivePartial, got a different ServiceError variant");
        };
        assert!(source.to_string().contains("does not exist"));
        let projection = app.projection().expect("projection");
        assert!(
            !projection.para_node(&id).expect("node").archived(),
            "a failed move must not archive the node"
        );
    }

    /// A multi-root run failing on the SECOND root must not silently drop
    /// the first root's already-completed move: the completed moves must
    /// travel with the error (via `ServiceError::ParaArchivePartial`) so a
    /// head can still report them — never lie about partial progress.
    #[test]
    fn archive_failing_on_a_later_root_carries_the_earlier_roots_completed_moves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = init_app(dir.path());
        let NodeId(id) = add(&mut app, "project", "client-x").expect("add");

        let root1 = tempfile::tempdir().expect("tempdir");
        let root1_source = root1.path().join("Projects").join("client-x");
        std::fs::create_dir_all(&root1_source).expect("mkdir");
        std::fs::write(root1_source.join("a.txt"), b"hello").expect("write");

        // root2 has no materialized directory at all, so its move fails.
        let root2 = tempfile::tempdir().expect("tempdir");

        let err = archive(
            &mut app,
            &id,
            &[root1.path().to_path_buf(), root2.path().to_path_buf()],
            false,
        )
        .expect_err("must fail on root2");
        let ServiceError::ParaArchivePartial { moves, source } = err else {
            panic!("expected ParaArchivePartial, got a different ServiceError variant");
        };
        assert_eq!(
            moves.len(),
            1,
            "root1's completed move must survive the error"
        );
        assert_eq!(moves[0].status, MoveStatus::Moved);
        assert_eq!(moves[0].to, root1.path().join("Archives").join("client-x"));
        assert!(source.to_string().contains("does not exist"));
        assert!(
            root1.path().join("Archives").join("client-x").is_dir(),
            "root1's move actually happened on disk"
        );
        let projection = app.projection().expect("projection");
        assert!(
            !projection.para_node(&id).expect("node").archived(),
            "the archive event must not be emitted when a later root fails"
        );
    }
}
