//! `maj search`/`maj searches`: the query-language search command and its
//! saved-search management. Parsing lives in `query`; this module resolves
//! parsed filters against the catalog and renders results.
use crate::SearchesCmd;
use crate::app::FsApp;
use crate::commands::{open_catalog, resolve_para_node};
use crate::volume_identity;
use anyhow::{Context, Result, bail};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::{AssetId, Op};
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::ports::{AssetSummary, Filter};
use majestical_core::projection::Projection;
use majestical_index::vector_store::{VectorHit, VectorStore};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Args for `maj search`, bundled to keep `cmd_search`'s own signature
/// within the house 5-positional-parameter limit.
pub(crate) struct SearchArgs {
    pub(crate) query: Option<String>,
    pub(crate) limit: usize,
    pub(crate) json: bool,
    pub(crate) save: Option<String>,
    pub(crate) saved: Option<String>,
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
/// Returns an error if `--save` names an empty string, the query fails to
/// parse, names an unknown or malformed filter, or (once parsed) carries
/// neither terms nor filters.
///
/// Resolves `args`' query — a literal string, or a `--saved` name looked up
/// in the projection — then runs it. Only once `run_search` succeeds does a
/// `--save` get emitted as a `SavedSearchSet`: an invalid query must never
/// poison the append-only, replicated event log with a saved search that
/// can never itself be run. The confirmation goes to stderr, not stdout —
/// `--json` callers get pure JSON on stdout even when also saving.
pub(crate) fn cmd_search(app: &mut FsApp, catalog_dir: &Path, args: &SearchArgs) -> Result<()> {
    if let Some(name) = &args.save {
        anyhow::ensure!(!name.is_empty(), "saved search name must not be empty");
    }
    let query = if let Some(q) = &args.query {
        q.clone()
    } else if let Some(name) = &args.saved {
        let projection = app.projection()?;
        projection
            .saved_search(name)
            .with_context(|| format!("no saved search named '{name}'"))?
            .to_string()
    } else {
        bail!("give a query string or --saved <name>");
    };
    run_search(&*app, catalog_dir, &query, args.limit, args.json)?;
    if let Some(name) = &args.save {
        app.emit(vec![Op::SavedSearchSet {
            name: name.clone(),
            query: query.clone(),
        }])?;
        eprintln!("saved search '{name}'");
    }
    Ok(())
}

/// Resolves filters against the catalog and prints results for `query`.
/// Split out of `cmd_search` so the read-only search path — the bulk of the
/// logic — stays borrowable as `&FsApp` even though `cmd_search` itself
/// needs `&mut FsApp` to emit a `--save`.
///
/// # Errors
/// Returns an error if the query fails to parse, names an unknown or
/// malformed filter, or (once parsed) carries neither terms nor filters.
fn run_search(
    app: &FsApp,
    catalog_dir: &Path,
    query: &str,
    limit: usize,
    json: bool,
) -> Result<()> {
    let parsed = crate::query::parse_query(query)?;
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

    // Filter-only queries never touch the semantic layer: there is no query
    // text to embed, and every allowed asset is already an exact match.
    let (ranked, keyframe_ts, coverage) = if parsed.terms.is_empty() {
        let Some(set) = &allowed else {
            unreachable!("empty query is rejected above before a catalog is even opened");
        };
        let ranked = set.iter().map(|a| (a.clone(), 0.0)).take(limit).collect();
        (ranked, HashMap::new(), None)
    } else {
        term_search(
            &db,
            &TermSearchArgs {
                catalog_dir,
                terms: &parsed.terms,
                allowed: allowed.as_ref(),
                limit,
                projection: &projection,
            },
        )?
    };
    print_search_results(
        &db,
        &ranked,
        &PrintOptions {
            keyframe_ts: &keyframe_ts,
            mounted: &mounted,
            limit,
            json,
            coverage,
        },
    )
}

/// Args for `term_search`, bundled to keep its own signature within the
/// house 5-positional-parameter limit.
struct TermSearchArgs<'a> {
    catalog_dir: &'a Path,
    terms: &'a [String],
    allowed: Option<&'a BTreeSet<AssetId>>,
    limit: usize,
    projection: &'a Projection,
}

/// Ranked results, each ranked asset's nearest keyframe timestamp, and
/// `Some((embedded, eligible))` semantic coverage counts when the semantic
/// layer ran this search.
type TermSearchResult = (
    Vec<(AssetId, f64)>,
    HashMap<AssetId, i64>,
    Option<(u64, u64)>,
);

/// Runs a terms-bearing search: FTS name-ranked hits fused with semantic
/// (embedding) hits via [`rrf_merge`], both intersected against `allowed`
/// first. Falls back to FTS-only — exactly today's ranking and truncation —
/// when the semantic layer contributes nothing (no model installed, nothing
/// indexed yet, or a query that simply matched nothing there).
///
/// # Errors
/// Returns an error if the FTS query or the local state dir can't be
/// resolved.
fn term_search(db: &SqliteCatalog, args: &TermSearchArgs<'_>) -> Result<TermSearchResult> {
    let fts_search_limit = if args.allowed.is_some() {
        usize::MAX
    } else {
        args.limit
    };
    let fts_hits: Vec<(AssetId, f64)> = db
        .search_names_ranked(args.terms, fts_search_limit)?
        .into_iter()
        .filter(|(a, _)| args.allowed.is_none_or(|s| s.contains(a)))
        .collect();

    // A wider net than `limit` for each ranking source: fusing two rankings
    // needs headroom past the final cut, or an asset either source ranks
    // just outside its own top `limit` — but that would rank first once
    // fused — never gets the chance to.
    let semantic_limit = if args.allowed.is_some() {
        usize::MAX >> 1
    } else {
        args.limit.saturating_mul(4)
    };
    let state_dir = crate::state_dir::state_dir_for(args.catalog_dir)?;
    let query_text = args.terms.join(" ");
    let (semantic_ids, keyframe_ts, embedded) =
        semantic_candidates(&state_dir, &query_text, semantic_limit);
    let semantic_ids: Vec<AssetId> = semantic_ids
        .into_iter()
        .filter(|a| args.allowed.is_none_or(|s| s.contains(a)))
        .collect();

    let ranked = if semantic_ids.is_empty() {
        fts_hits.into_iter().take(args.limit).collect()
    } else {
        let fts_ids: Vec<AssetId> = fts_hits.into_iter().map(|(a, _)| a).collect();
        rrf_merge(&[fts_ids, semantic_ids], args.limit)
    };

    let coverage = embedded.map(|embedded| (embedded, eligible_asset_count(args.projection)));
    Ok((ranked, keyframe_ts, coverage))
}

/// Counts catalog assets eligible for semantic embedding: any asset whose
/// first recorded instance classifies as `Image` or `Video` — the same
/// classification the index planner uses (any instance's basename decides,
/// since every instance of one asset shares content).
fn eligible_asset_count(projection: &Projection) -> u64 {
    let mut count = 0u64;
    for (_, state) in projection.assets() {
        let Some((_, path)) = state.instances.keys().next() else {
            continue;
        };
        match media_kind(path) {
            MediaKind::Image | MediaKind::Video => count += 1,
            MediaKind::Other => {}
        }
    }
    count
}

/// Reciprocal Rank Fusion: merges ranked `lists` into one ranking by
/// `1/(k+rank)` summed across every list `k=60` (the standard RRF constant —
/// large enough that rank 1 and rank 2 contribute nearly the same score, so
/// fusion isn't dominated by whichever single list happens to rank one
/// asset first). Ties are broken by asset id for deterministic output.
fn rrf_merge(lists: &[Vec<AssetId>], limit: usize) -> Vec<(AssetId, f64)> {
    const K: f64 = 60.0;
    let mut scores: BTreeMap<AssetId, f64> = BTreeMap::new();
    for list in lists {
        for (rank, asset) in list.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "rank is a small search-result index"
            )]
            let contribution = 1.0 / (K + (rank as f64 + 1.0));
            *scores.entry(asset.clone()).or_insert(0.0) += contribution;
        }
    }
    let mut ranked: Vec<(AssetId, f64)> = scores.into_iter().collect();
    ranked.sort_by(|(id_a, score_a), (id_b, score_b)| {
        score_b
            .partial_cmp(score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| id_a.0.cmp(&id_b.0))
    });
    ranked.truncate(limit);
    ranked
}

/// Why the semantic layer couldn't run this search. Each variant carries its
/// own actionable stderr note — the fix differs (fetch the model vs. index
/// the catalog), so a single generic "unavailable" message would send half
/// of readers to the wrong command.
enum SemanticMiss {
    NoModel,
    EmptyIndex,
}

impl SemanticMiss {
    fn note(&self) -> &'static str {
        match self {
            SemanticMiss::NoModel => "semantic search unavailable — run `maj model fetch`",
            SemanticMiss::EmptyIndex => "semantic index is empty — run `maj index run`",
        }
    }
}

/// Resolves the model, opens the local Lance store, and confirms it holds at
/// least one embedding for `MODEL_TAG` — the three preconditions for a
/// semantic query to mean anything.
fn open_semantic_index(state_dir: &Path) -> Result<(PathBuf, VectorStore, u64), SemanticMiss> {
    let model_dir = majestical_index::model::model_dir()
        .ok()
        .filter(|dir| majestical_index::model::model_present(dir))
        .ok_or(SemanticMiss::NoModel)?;
    let store =
        VectorStore::open(&state_dir.join("lance")).map_err(|_| SemanticMiss::EmptyIndex)?;
    let embedded = store
        .distinct_assets(majestical_index::model::MODEL_TAG)
        .map_or(0, |s| s.len());
    if embedded == 0 {
        return Err(SemanticMiss::EmptyIndex);
    }
    Ok((
        model_dir,
        store,
        u64::try_from(embedded).unwrap_or(u64::MAX),
    ))
}

/// Loads only the text tower (query-time embedding never touches the vision
/// tower, so this skips loading its 372 MB ONNX graph entirely) and embeds
/// `query`.
fn embed_query(model_dir: &Path, query: &str) -> Option<Vec<f32>> {
    let mut encoder = majestical_index::encoder::Encoder::load_text_only(model_dir).ok()?;
    encoder.embed_text(query).ok()
}

/// Maps Lance hits to catalog asset ids, deduped preserving nearest-first
/// order — an asset can appear as both its whole-image embedding and one or
/// more keyframe hits, and only the nearest counts for ranking. Also
/// collects each asset's nearest keyframe timestamp (keyframe hits only; a
/// whole-image hit has none).
fn dedupe_hits(hits: Vec<VectorHit>) -> (Vec<AssetId>, HashMap<AssetId, i64>) {
    let mut ranked = Vec::new();
    let mut seen = HashSet::new();
    let mut keyframe_ts = HashMap::new();
    for hit in hits {
        let asset = AssetId(format!("xxh3:{}", hit.asset_hex));
        if hit.kind == "keyframe" {
            keyframe_ts.entry(asset.clone()).or_insert(hit.ts_ms);
        }
        if seen.insert(asset.clone()) {
            ranked.push(asset);
        }
    }
    (ranked, keyframe_ts)
}

/// Runs the semantic side of a search: embeds `query` with the text tower
/// and nearest-neighbor searches the local Lance vector store. Degrades to
/// `(empty, empty, None)` on any miss — semantic search is additive to name
/// search, never a hard requirement — printing the specific stderr note for
/// the reason (see [`SemanticMiss`]).
///
/// Returns ranked asset ids (nearest first, deduped), each ranked asset's
/// nearest keyframe timestamp, and `Some(embedded distinct asset count)`
/// when the layer actually ran (used for the coverage notice).
fn semantic_candidates(
    state_dir: &Path,
    query: &str,
    limit: usize,
) -> (Vec<AssetId>, HashMap<AssetId, i64>, Option<u64>) {
    let (model_dir, store, embedded) = match open_semantic_index(state_dir) {
        Ok(opened) => opened,
        Err(miss) => {
            eprintln!("{}", miss.note());
            return (Vec::new(), HashMap::new(), None);
        }
    };
    let Some(vector) = embed_query(&model_dir, query) else {
        eprintln!("{}", SemanticMiss::NoModel.note());
        return (Vec::new(), HashMap::new(), None);
    };
    let Ok(hits) = store.search(&vector, majestical_index::model::MODEL_TAG, limit) else {
        eprintln!("{}", SemanticMiss::EmptyIndex.note());
        return (Vec::new(), HashMap::new(), None);
    };
    let (ranked, keyframe_ts) = dedupe_hits(hits);
    (ranked, keyframe_ts, Some(embedded))
}

/// Formats a keyframe timestamp as `@MmSSs` (e.g. `@1m05s`), text-mode only.
fn format_ts(ts_ms: i64) -> String {
    let total_secs = ts_ms.max(0) / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("@{minutes}m{seconds:02}s")
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

/// Everything [`print_search_results`] needs besides `db` and `ranked`,
/// bundled to keep its signature (and its two per-format renderers') within
/// the house 5-positional-parameter limit.
struct PrintOptions<'a> {
    /// Each ranked asset's nearest keyframe timestamp — keyframe semantic
    /// hits only; empty when the search had no semantic component.
    keyframe_ts: &'a HashMap<AssetId, i64>,
    mounted: &'a BTreeMap<String, PathBuf>,
    limit: usize,
    json: bool,
    /// `(embedded, eligible)` distinct asset counts when the semantic layer
    /// ran this search; `None` when it didn't (filter-only query, or a
    /// degraded miss already noted on stderr).
    coverage: Option<(u64, u64)>,
}

/// Renders ranked results: JSON prints one object per hit with its volumes
/// (online/offline per the currently mounted set), tags, and (for a semantic
/// keyframe hit) `timestamp_ms`; text prints one line per hit (`{asset}
/// {name}  [label●|○,...]`, `tags:` and `@MmSSs` appended when present)
/// followed by a `"{n} results"` summary line, a truncation hint when the
/// result count hit `limit` exactly, and — when the semantic layer ran but
/// hasn't embedded every eligible asset yet — a coverage notice.
fn print_search_results(
    db: &SqliteCatalog,
    ranked: &[(AssetId, f64)],
    opts: &PrintOptions<'_>,
) -> Result<()> {
    let ids: Vec<AssetId> = ranked.iter().map(|(a, _)| a.clone()).collect();
    let summaries = db.asset_summaries(&ids)?;
    let by_id: HashMap<&AssetId, &AssetSummary> = summaries.iter().map(|s| (&s.asset, s)).collect();

    if opts.json {
        print_search_results_json(ranked, &by_id, opts);
    } else {
        print_search_results_text(ranked, &by_id, opts);
    }
    Ok(())
}

fn print_search_results_json(
    ranked: &[(AssetId, f64)],
    by_id: &HashMap<&AssetId, &AssetSummary>,
    opts: &PrintOptions<'_>,
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
                    serde_json::json!({ "id": id, "label": label, "online": opts.mounted.contains_key(id) })
                })
                .collect();
            let mut result = serde_json::json!({
                "asset": asset.0,
                "score": score,
                "name": summary.name,
                "volumes": volumes,
                "tags": summary.tags,
                "para": summary.para,
            });
            if let Some(ts) = opts.keyframe_ts.get(asset) {
                result["timestamp_ms"] = serde_json::json!(ts);
            }
            result
        })
        .collect();
    let mut payload = serde_json::json!({ "count": ranked.len(), "results": results });
    if let Some((embedded, eligible)) = opts.coverage {
        payload["semantic_coverage"] =
            serde_json::json!({ "embedded": embedded, "eligible": eligible });
    }
    println!("{payload}");
}

fn print_search_results_text(
    ranked: &[(AssetId, f64)],
    by_id: &HashMap<&AssetId, &AssetSummary>,
    opts: &PrintOptions<'_>,
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
                let dot = if opts.mounted.contains_key(id) {
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
        if let Some(ts) = opts.keyframe_ts.get(asset) {
            print!("  {}", format_ts(*ts));
        }
        println!();
    }
    println!("{} results", ranked.len());
    // A result count exactly at `limit` almost always means more matches
    // exist past it — say so, rather than letting a truncated list look
    // like the complete answer.
    if ranked.len() == opts.limit {
        println!(
            "note: results truncated at {}; raise --limit to see more",
            opts.limit
        );
    }
    if let Some((embedded, eligible)) = opts.coverage
        && embedded < eligible
    {
        println!("semantic index: {embedded} of {eligible} eligible assets");
    }
}

/// `maj searches list`/`maj searches rm`: manage saved searches directly,
/// without running one. Reads/writes the projection through the event log —
/// saved searches never touch the sqlite catalog from the CLI's side, so
/// unlike `cmd_search` this needs no `catalog_dir`.
///
/// # Errors
/// Returns an error if `Rm` names an empty string or a search that doesn't
/// exist, or the event log can't be read or appended to.
pub(crate) fn cmd_searches(app: &mut FsApp, cmd: SearchesCmd) -> Result<()> {
    match cmd {
        SearchesCmd::List { json } => {
            print_saved_searches(&app.projection()?, json);
            Ok(())
        }
        SearchesCmd::Rm { name } => {
            anyhow::ensure!(!name.is_empty(), "saved search name must not be empty");
            let projection = app.projection()?;
            projection
                .saved_search(&name)
                .with_context(|| format!("no saved search named '{name}'"))?;
            app.emit(vec![Op::SavedSearchRemove { name: name.clone() }])?;
            println!("removed saved search '{name}'");
            Ok(())
        }
    }
}

fn print_saved_searches(projection: &Projection, json: bool) {
    let saved: Vec<(&str, &str)> = projection.saved_searches().collect();
    if json {
        let items: Vec<_> = saved
            .iter()
            .map(|(name, query)| serde_json::json!({ "name": name, "query": query }))
            .collect();
        println!("{}", serde_json::json!({ "saved": items }));
    } else if saved.is_empty() {
        println!("no saved searches");
    } else {
        for (name, query) in saved {
            println!("{name}: {query}");
        }
    }
}

#[cfg(test)]
mod semantic_tests {
    use super::{AssetId, format_ts, rrf_merge};

    #[test]
    fn rrf_merge_ranks_an_asset_present_in_both_lists_above_a_single_list_hit() {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let c = AssetId("xxh3:c".into());
        let fts = vec![a.clone(), c.clone()];
        let semantic = vec![b, a.clone()];
        let merged = rrf_merge(&[fts, semantic], 10);
        assert_eq!(merged[0].0, a, "present in both lists must rank first");
    }

    #[test]
    fn rrf_merge_of_no_lists_or_only_empty_lists_is_empty() {
        assert!(rrf_merge(&[], 10).is_empty());
        assert!(rrf_merge(&[vec![], vec![]], 10).is_empty());
    }

    #[test]
    fn rrf_merge_breaks_tied_scores_by_asset_id() {
        let low = AssetId("xxh3:aaa".into());
        let high = AssetId("xxh3:bbb".into());
        // Each ranks first in its own single list, so their RRF scores tie.
        let merged = rrf_merge(&[vec![high.clone()], vec![low.clone()]], 10);
        let ids: Vec<&AssetId> = merged.iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![&low, &high],
            "tied scores break by asset id ascending"
        );
    }

    #[test]
    fn rrf_merge_truncates_at_limit() {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let merged = rrf_merge(&[vec![a, b]], 1);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn format_ts_renders_minutes_and_seconds() {
        assert_eq!(format_ts(0), "@0m00s");
        assert_eq!(format_ts(65_000), "@1m05s");
        assert_eq!(format_ts(3_661_000), "@61m01s");
    }
}
