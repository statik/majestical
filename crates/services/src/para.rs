//! PARA node reference resolution, shared by `maj para` and `search`'s
//! `para:` filter. Moved verbatim from `crates/cli/src/commands.rs`.
use crate::app::FsApp;
use crate::catalog::open_catalog;
use crate::error::ServiceError;
use anyhow::{Context, Result};
use majestical_core::event::ParaKind;
use majestical_core::projection::Projection;
use std::path::Path;

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
    Ok(ParaOutcome { nodes })
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
