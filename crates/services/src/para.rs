//! PARA node reference resolution, shared by `maj para` and `search`'s
//! `para:` filter. Moved verbatim from `crates/cli/src/commands.rs`.
use anyhow::Result;
use majestical_core::event::ParaKind;
use majestical_core::projection::Projection;

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
