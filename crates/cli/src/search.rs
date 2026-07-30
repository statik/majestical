//! `maj search`: the query-language search command. Parsing lives in
//! `query`; this module resolves parsed filters against the catalog and
//! renders results.
use crate::app::FsApp;
use crate::commands::{open_catalog, resolve_para_node};
use crate::volume_identity;
use anyhow::Result;
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::AssetId;
use majestical_core::media_kind::MediaKind;
use majestical_core::ports::{AssetSummary, Filter};
use majestical_core::projection::Projection;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// Args for `maj search`, bundled to keep `cmd_search`'s own signature
/// within the house 5-positional-parameter limit.
pub(crate) struct SearchArgs {
    pub(crate) query: String,
    pub(crate) limit: usize,
    pub(crate) json: bool,
}

/// The filter keys `search` understands, listed in error messages so a typo'd
/// key points straight at the fix instead of a silent zero-result search.
const FILTER_KEYS: &str = "tag, vol/volume, para, kind, online, before, after";

/// Searches the catalog: bare terms are ranked by `search_names_ranked` (best
/// match first); `key:value` tokens resolve to hard `Filter`s and narrow the
/// result to their conjunction. Terms and filters combine by intersection —
/// a term match that fails a filter is dropped, never re-ranked above it. With
/// filters present, every ranked match is fetched and intersected before
/// `limit` is applied — a filter-matching asset that ranks outside a small
/// prefetch window must still be found, not silently dropped by a pre-filter
/// slice.
///
/// # Errors
/// Returns an error if the query fails to parse, names an unknown or
/// malformed filter, or (once parsed) carries neither terms nor filters.
pub(crate) fn cmd_search(app: &FsApp, catalog_dir: &Path, args: &SearchArgs) -> Result<()> {
    let parsed = crate::query::parse_query(&args.query)?;
    anyhow::ensure!(
        !parsed.terms.is_empty() || !parsed.filters.is_empty(),
        "empty query: give search terms or at least one filter"
    );
    let (db, projection) = open_catalog(app, catalog_dir)?;
    // Resolved once and shared: `resolve_filter`'s `online:` arm and
    // `print_search_results`'s per-volume online flag both need the mounted
    // set, and each call shells out to `diskutil` per mount — computing it
    // twice would double a search's latency for no benefit.
    let mounted = volume_identity::mounted_volumes();
    let filters = resolve_filters(&projection, &parsed.filters, &mounted)?;
    let allowed = if filters.is_empty() {
        None
    } else {
        Some(db.assets_matching(&filters)?)
    };
    let ranked: Vec<(AssetId, f64)> = if parsed.terms.is_empty() {
        let Some(set) = &allowed else {
            unreachable!("empty query is rejected above before a catalog is even opened");
        };
        set.iter()
            .map(|a| (a.clone(), 0.0))
            .take(args.limit)
            .collect()
    } else {
        let search_limit = if allowed.is_some() {
            usize::MAX
        } else {
            args.limit
        };
        db.search_names_ranked(&parsed.terms, search_limit)?
            .into_iter()
            .filter(|(a, _)| allowed.as_ref().is_none_or(|s| s.contains(a)))
            .take(args.limit)
            .collect()
    };
    print_search_results(&db, &ranked, &mounted, args)
}

/// Resolves parsed `key:value` tokens against `majestical_core::ports::Filter`'s
/// per-variant contracts. `vol`/`volume` both address `Filter::Volume`;
/// `before`/`after` reject a `-` negation (there's no negated form) rather
/// than silently ignoring it.
fn resolve_filters(
    projection: &Projection,
    raw: &[crate::query::RawFilter],
    mounted: &BTreeMap<String, PathBuf>,
) -> Result<Vec<Filter>> {
    raw.iter()
        .map(|f| resolve_filter(projection, f, mounted))
        .collect()
}

fn resolve_filter(
    projection: &Projection,
    raw: &crate::query::RawFilter,
    mounted: &BTreeMap<String, PathBuf>,
) -> Result<Filter> {
    let crate::query::RawFilter {
        key,
        value,
        negated,
    } = raw;
    let negated = *negated;
    match key.as_str() {
        "tag" => Ok(Filter::Tag {
            value: value.clone(),
            negated,
        }),
        "vol" | "volume" => Ok(Filter::Volume {
            value: value.clone(),
            negated,
        }),
        "para" => Ok(Filter::Para {
            node: resolve_para_node(projection, value)?,
            negated,
        }),
        "kind" => {
            let valid = MediaKind::ALL.map(MediaKind::as_str);
            anyhow::ensure!(
                valid.contains(&value.as_str()),
                "unknown kind '{value}' — one of: {}",
                valid.join(", ")
            );
            Ok(Filter::Kind {
                value: value.clone(),
                negated,
            })
        }
        "online" => {
            let want = match value.as_str() {
                "yes" => true,
                "no" => false,
                other => anyhow::bail!("online: expects 'yes' or 'no', got '{other}'"),
            };
            Ok(Filter::Online {
                ids: mounted.keys().cloned().collect(),
                want: want != negated,
            })
        }
        "before" | "after" => {
            anyhow::ensure!(
                !negated,
                "'-{key}:' has no meaning — use '{}:' instead",
                if key.as_str() == "before" {
                    "after"
                } else {
                    "before"
                }
            );
            let ms = crate::query::parse_date_ms(value)?;
            if key.as_str() == "before" {
                Ok(Filter::Before(ms))
            } else {
                Ok(Filter::After(ms))
            }
        }
        other => anyhow::bail!("unknown filter '{other}:'; valid filters: {FILTER_KEYS}"),
    }
}

/// Renders ranked results: JSON prints one object per hit with its volumes
/// (online/offline per the currently mounted set) and tags; text prints one
/// line per hit (`{asset}  {name}  [label●|○,...]`, `tags:` appended when
/// non-empty) followed by a `"{n} results"` summary line, plus a truncation
/// hint when the result count hit `args.limit` exactly.
fn print_search_results(
    db: &SqliteCatalog,
    ranked: &[(AssetId, f64)],
    mounted: &BTreeMap<String, PathBuf>,
    args: &SearchArgs,
) -> Result<()> {
    let ids: Vec<AssetId> = ranked.iter().map(|(a, _)| a.clone()).collect();
    let summaries = db.asset_summaries(&ids)?;
    let by_id: HashMap<&AssetId, &AssetSummary> = summaries.iter().map(|s| (&s.asset, s)).collect();

    if args.json {
        print_search_results_json(ranked, &by_id, mounted);
    } else {
        print_search_results_text(ranked, &by_id, mounted, args.limit);
    }
    Ok(())
}

fn print_search_results_json(
    ranked: &[(AssetId, f64)],
    by_id: &HashMap<&AssetId, &AssetSummary>,
    mounted: &BTreeMap<String, PathBuf>,
) {
    let results: Vec<_> = ranked
        .iter()
        .map(|(asset, score)| {
            let empty = AssetSummary {
                asset: asset.clone(),
                name: String::new(),
                volumes: Vec::new(),
                tags: Vec::new(),
                para: None,
            };
            let summary = by_id.get(asset).copied().unwrap_or(&empty);
            let volumes: Vec<_> = summary
                .volumes
                .iter()
                .map(|(id, label)| {
                    serde_json::json!({ "id": id, "label": label, "online": mounted.contains_key(id) })
                })
                .collect();
            serde_json::json!({
                "asset": asset.0,
                "score": score,
                "name": summary.name,
                "volumes": volumes,
                "tags": summary.tags,
                "para": summary.para,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({ "count": ranked.len(), "results": results })
    );
}

fn print_search_results_text(
    ranked: &[(AssetId, f64)],
    by_id: &HashMap<&AssetId, &AssetSummary>,
    mounted: &BTreeMap<String, PathBuf>,
    limit: usize,
) {
    for (asset, _score) in ranked {
        let Some(summary) = by_id.get(asset).copied() else {
            println!("{}", asset.0);
            continue;
        };
        let volumes: Vec<String> = summary
            .volumes
            .iter()
            .map(|(id, label)| {
                let dot = if mounted.contains_key(id) {
                    '\u{25cf}'
                } else {
                    '\u{25cb}'
                };
                format!("{label}{dot}")
            })
            .collect();
        print!("{}  {}  [{}]", asset.0, summary.name, volumes.join(","));
        if !summary.tags.is_empty() {
            print!("  tags:{}", summary.tags.join(","));
        }
        println!();
    }
    println!("{} results", ranked.len());
    // A result count exactly at `limit` almost always means more matches
    // exist past it — say so, rather than letting a truncated list look
    // like the complete answer.
    if ranked.len() == limit {
        println!("note: results truncated at {limit}; raise --limit to see more");
    }
}
