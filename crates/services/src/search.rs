//! Search compute: query parse → layered retrieval → ranked rows + notices.
//! Moved from `crates/cli/src/search.rs`; the CLI keeps rendering
//! (`json!`/text printing) fed from [`SearchOutcome`].
use crate::app::FsApp;
use crate::capability::{
    DESCRIBER_REMEDY, minilm_model_dir_if_present, transcript_model_remedy,
    whisper_model_dir_if_present,
};
use crate::catalog::open_catalog;
use crate::error::ServiceError;
use crate::para::resolve_para_node;
use anyhow::{Context, Result, bail};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::{AssetId, Op};
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::ports::{AssetSummary, Filter};
use majestical_core::projection::Projection;
use majestical_index::model::{MINILM, SIGLIP};
use majestical_index::text_encoder::TextEncoder;
use majestical_index::vector_store::{TextChunkHit, TextVectorStore, VectorHit, VectorStore};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Request for [`search`]: either a literal `query` string or a `--saved`
/// name to resolve and run; `save` additionally persists the resolved query
/// under that name once the search itself succeeds.
pub struct SearchRequest {
    pub query: Option<String>,
    pub limit: usize,
    /// Run a previously saved search by name.
    pub saved: Option<String>,
    /// Save the query under this name (and run it).
    pub save: Option<String>,
}

/// One volume holding an instance of a hit asset, and whether it's currently
/// mounted.
#[derive(Debug, Serialize)]
pub struct VolumeRef {
    pub id: String,
    pub label: String,
    pub online: bool,
}

/// One ranked search result, carrying everything a head needs to render a
/// row without re-querying the catalog.
#[derive(Debug, Serialize)]
pub struct SearchHit {
    pub asset: String,
    pub score: f64,
    /// Whether the catalog resolved a summary for this asset — `false` for
    /// a ranked hit (e.g. from a stale semantic index entry) the catalog no
    /// longer knows about, in which case `name`/`volumes`/`tags`/`para`
    /// below are placeholders, not a genuinely empty summary.
    pub known: bool,
    pub name: String,
    pub volumes: Vec<VolumeRef>,
    pub tags: Vec<String>,
    pub para: Option<String>,
    /// Nearest keyframe timestamp — image-semantic keyframe hits only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<i64>,
    /// Which indexed-text source this hit matched or resembled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// That source row's locator: a ms timestamp, a PDF page, or -1 for none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locator: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Populated by browse rows, from the scoped instance's own attributes
    /// (`browse.rs`'s representative-instance pick). Search hits leave this
    /// absent this phase — absent-when-`None` keeps every existing search
    /// wire shape byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Populated by browse rows the same way as [`Self::size`]; absent for
    /// search hits this phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<u64>,
    /// Populated by browse rows the same way as [`Self::size`] — the
    /// `media_kind` name ("video", "image", "audio", "pdf", "other") of the
    /// representative instance. Absent for search hits this phase.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// One text source's coverage notice: how much of the eligible catalog it
/// has actually indexed, and the command that closes the gap.
#[derive(Serialize)]
pub struct TextCoverageNotice {
    pub label: &'static str,
    pub noun: &'static str,
    pub covered: usize,
    pub eligible: usize,
    pub remedy: String,
    /// The machine-usable `in:` source key this notice is about (e.g.
    /// `"transcript"`, from [`TEXT_SOURCE_INFO`]) — `label` is the
    /// human-facing plural noun ("transcripts"); this is the same value a
    /// caller would pass back as `in:<source>`. CLI rendering ignores it.
    pub source: String,
}

/// Distinct asset counts for the image-semantic layer: how many eligible
/// assets are embedded versus how many are eligible in total. A named
/// struct (rather than a bare `(u64, u64)` tuple) so the MCP wire contract
/// carries labeled fields instead of a positional pair.
#[derive(Serialize)]
pub struct SemanticCoverage {
    pub embedded: u64,
    pub eligible: u64,
}

/// Everything [`search`] produces: the ranked rows plus degradation notices
/// a head should surface (which layers ran, how much of the catalog they
/// cover).
#[derive(Serialize)]
pub struct SearchOutcome {
    pub count: usize,
    pub results: Vec<SearchHit>,
    /// Set when the image-semantic layer ran this search.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_coverage: Option<SemanticCoverage>,
    /// Per-source text coverage notices; empty for filter-only queries.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub text_coverage: Vec<TextCoverageNotice>,
    /// Diagnostics collected during this operation, verbatim — the lines the
    /// CLI prints to stderr. Absent from the wire when empty.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

/// A saved search's name and the query text it runs.
#[derive(Serialize)]
pub struct SavedSearch {
    pub name: String,
    pub query: String,
}

/// The filter keys `search` understands, listed in error messages so a
/// typo'd key points straight at the fix instead of a silent zero-result
/// search.
const FILTER_KEYS: &str = "tag, vol/volume, para, kind, online, before, after, in";

/// Searches the catalog: bare terms are ranked by `search_names_ranked`
/// (best match first); `key:value` tokens resolve to hard `Filter`s and
/// narrow the result to their conjunction. Terms and filters combine by
/// intersection — a term match that fails a filter is dropped, never
/// re-ranked above it. With filters present, every ranked match is fetched
/// and intersected before `limit` is applied — a filter-matching asset that
/// ranks outside a small prefetch window must still be found, not silently
/// dropped by a pre-filter slice.
///
/// Resolves `req`'s query — a literal string, or a `--saved` name looked up
/// in the projection — then runs it. Only once the search itself succeeds
/// does a `--save` get emitted as a `SavedSearchSet`: an invalid query must
/// never poison the append-only, replicated event log with a saved search
/// that can never itself be run.
///
/// # Errors
/// Returns an error if `save` names an empty string, the query fails to
/// parse, names an unknown or malformed filter, carries neither terms nor
/// filters, or the catalog/event log can't be read or appended to.
pub fn search(
    app: &mut FsApp,
    catalog_dir: &Path,
    req: &SearchRequest,
) -> Result<SearchOutcome, ServiceError> {
    search_impl(app, catalog_dir, req).map_err(ServiceError::from)
}

fn search_impl(app: &mut FsApp, catalog_dir: &Path, req: &SearchRequest) -> Result<SearchOutcome> {
    if let Some(name) = &req.save {
        anyhow::ensure!(!name.is_empty(), "saved search name must not be empty");
    }
    let query = if let Some(q) = &req.query {
        q.clone()
    } else if let Some(name) = &req.saved {
        let projection = app.projection()?;
        projection
            .saved_search(name)
            .with_context(|| format!("no saved search named '{name}'"))?
            .to_string()
    } else {
        bail!("give a query string or --saved <name>");
    };
    let mut outcome = run_search(&*app, catalog_dir, &query, req.limit)?;
    if let Some(name) = &req.save {
        app.emit(vec![Op::SavedSearchSet {
            name: name.clone(),
            query: query.clone(),
        }])?;
    }
    // Drained last so a clock-clamp warning from the `--save` emit above is
    // carried home too, and in one place so every line keeps the order it
    // was recorded in.
    outcome.notices = app.notices().drain();
    Ok(outcome)
}

/// Resolves filters against the catalog and builds ranked rows for `query`.
/// Split out of [`search_impl`] so the read-only search path — the bulk of
/// the logic — stays borrowable as `&FsApp` even though `search_impl` itself
/// needs `&mut FsApp` to emit a `--save`.
///
/// # Errors
/// Returns an error if the query fails to parse, names an unknown or
/// malformed filter, or (once parsed) carries neither terms nor filters.
fn run_search(app: &FsApp, catalog_dir: &Path, query: &str, limit: usize) -> Result<SearchOutcome> {
    let parsed = crate::query::parse_query(query)?;
    anyhow::ensure!(
        !parsed.terms.is_empty() || !parsed.filters.is_empty(),
        "empty query: give search terms or at least one filter"
    );
    // `in:` scopes where TERMS match, so it routes around the catalog-filter
    // path entirely (resolve_filters errors on unknown keys) — and without
    // terms there is nothing for it to scope.
    let sources = in_sources(&parsed.filters)?;
    let catalog_raw: Vec<&crate::query::RawFilter> =
        parsed.filters.iter().filter(|f| f.key != "in").collect();
    anyhow::ensure!(
        !(parsed.terms.is_empty() && sources.is_some()),
        "in: requires search terms — it scopes where terms match, not which assets exist"
    );
    let (db, projection) = open_catalog(app, catalog_dir)?;
    // Resolved once and shared: `resolve_filter`'s `online:` arm and the
    // outcome's per-volume online flag both need the mounted set, and each
    // call shells out to `diskutil` per mount — computing it twice would
    // double a search's latency for no benefit.
    let mounted = crate::volume_identity::mounted_volumes();
    let filters = resolve_filters(&projection, &catalog_raw, &mounted)?;
    let allowed = if filters.is_empty() {
        None
    } else {
        Some(db.assets_matching(&filters)?)
    };

    // Filter-only queries never touch the semantic layer: there is no query
    // text to embed, and every allowed asset is already an exact match.
    let out = if parsed.terms.is_empty() {
        let Some(set) = &allowed else {
            unreachable!("empty query is rejected above before a catalog is even opened");
        };
        TermSearchOutput {
            ranked: set.iter().map(|a| (a.clone(), 0.0)).take(limit).collect(),
            ..TermSearchOutput::default()
        }
    } else {
        term_search(
            &db,
            &TermSearchArgs {
                catalog_dir,
                notices: app.notices(),
                terms: &parsed.terms,
                allowed: allowed.as_ref(),
                limit,
                projection: &projection,
                sources: sources.as_ref(),
            },
        )?
    };
    build_outcome(&db, &mounted, out)
}

/// Fetches each ranked asset's catalog summary (name, volumes, tags, para)
/// and folds it together with its ranking score and any text/semantic
/// detail into the final [`SearchOutcome`].
///
/// # Errors
/// Returns an error if the asset summaries can't be fetched.
fn build_outcome(
    db: &SqliteCatalog,
    mounted: &BTreeMap<String, PathBuf>,
    out: TermSearchOutput,
) -> Result<SearchOutcome> {
    let TermSearchOutput {
        ranked,
        keyframe_ts,
        semantic_coverage,
        mut text_meta,
        text_coverage,
    } = out;
    let ids: Vec<AssetId> = ranked.iter().map(|(a, _)| a.clone()).collect();
    let summaries = db.asset_summaries(&ids)?;
    let by_id: HashMap<&AssetId, &AssetSummary> = summaries.iter().map(|s| (&s.asset, s)).collect();

    let results = ranked
        .iter()
        .map(|(asset, score)| {
            let empty = AssetSummary {
                asset: asset.clone(),
                name: String::new(),
                volumes: Vec::new(),
                tags: Vec::new(),
                para: None,
            };
            let known = by_id.contains_key(asset);
            let summary = by_id.get(asset).copied().unwrap_or(&empty);
            let volumes = summary
                .volumes
                .iter()
                .map(|(id, label)| VolumeRef {
                    id: id.clone(),
                    label: label.clone(),
                    online: mounted.contains_key(id),
                })
                .collect();
            let meta = text_meta.remove(asset);
            SearchHit {
                asset: asset.0.clone(),
                score: *score,
                known,
                name: summary.name.clone(),
                volumes,
                tags: summary.tags.clone(),
                para: summary.para.clone(),
                timestamp_ms: keyframe_ts.get(asset).copied(),
                source: meta.as_ref().map(|m| m.source.clone()),
                locator: meta.as_ref().map(|m| m.locator),
                snippet: meta.map(|m| m.snippet),
                // Search carries no per-instance attributes this phase —
                // only browse rows (see `browse.rs::build_rows`) populate
                // these.
                size: None,
                mtime_ms: None,
                kind: None,
            }
        })
        .collect();
    Ok(SearchOutcome {
        count: ranked.len(),
        results,
        semantic_coverage,
        text_coverage,
        // Filled by `search_impl`, which is the only frame here holding the
        // app whose buffer the notices accumulate in.
        notices: Vec::new(),
    })
}

/// The `in:` source values `search` understands, listed in the error for a
/// typo'd value. `name` is the filename FTS index; the other four are the
/// `text_fts` sources.
const IN_SOURCE_VALUES: [&str; 5] = ["transcript", "caption", "ocr", "pdf", "name"];

/// Collects the `in:` filters into the set of sources to search — `None`
/// when the query has no `in:` at all (search everything). Multiple `in:`
/// values union.
///
/// # Errors
/// Returns an error for a negated `in:` (there's no sensible "everywhere
/// but" over ranked fusion) or a value outside [`IN_SOURCE_VALUES`].
fn in_sources(raw: &[crate::query::RawFilter]) -> Result<Option<BTreeSet<String>>> {
    let mut sources = BTreeSet::new();
    let mut any = false;
    for filter in raw.iter().filter(|f| f.key == "in") {
        anyhow::ensure!(
            !filter.negated,
            "in: does not support negation — name only the sources to search"
        );
        anyhow::ensure!(
            IN_SOURCE_VALUES.contains(&filter.value.as_str()),
            "unknown in: source '{}' — one of: {}",
            filter.value,
            IN_SOURCE_VALUES.join(", ")
        );
        any = true;
        sources.insert(filter.value.clone());
    }
    Ok(any.then_some(sources))
}

/// Args for `term_search`, bundled to keep its own signature within the
/// house 5-positional-parameter limit.
struct TermSearchArgs<'a> {
    catalog_dir: &'a Path,
    notices: &'a crate::notices::Notices,
    terms: &'a [String],
    allowed: Option<&'a BTreeSet<AssetId>>,
    limit: usize,
    projection: &'a Projection,
    /// The `in:` source restriction — `None` searches every source.
    sources: Option<&'a BTreeSet<String>>,
}

/// The text-row detail for a hit that matched (or semantically resembled)
/// indexed text: which source, that row's locator (ms timestamp, PDF page,
/// or -1 for none), and a short snippet.
struct TextMeta {
    source: String,
    locator: i64,
    snippet: String,
}

/// Everything a terms-bearing search produces beyond the ranking itself.
#[derive(Default)]
struct TermSearchOutput {
    ranked: Vec<(AssetId, f64)>,
    /// Each ranked asset's nearest keyframe timestamp (image-semantic
    /// keyframe hits only).
    keyframe_ts: HashMap<AssetId, i64>,
    /// Set when the image-semantic layer ran.
    semantic_coverage: Option<SemanticCoverage>,
    /// Per-asset text detail (FTS row, else best chunk).
    text_meta: HashMap<AssetId, TextMeta>,
    /// Per-source text coverage notices, in [`TEXT_SOURCE_INFO`] order.
    text_coverage: Vec<TextCoverageNotice>,
}

/// Runs a terms-bearing search: filename FTS, indexed-text FTS, image
/// vectors, and transcript-chunk vectors, fused via [`fuse_ranked_n`].
/// Falls back to name-FTS-only — exactly the phase-4 ranking and
/// truncation — when every other layer contributes nothing (no model
/// installed, nothing indexed yet, or a query that simply matched nothing
/// there). An `in:` restriction disables every layer outside the named
/// sources — including the image-semantic layer, which matches content
/// rather than any nameable text source.
///
/// # Errors
/// Returns an error if an FTS query, the coverage counts, or the local
/// state dir can't be resolved.
fn term_search(db: &SqliteCatalog, args: &TermSearchArgs<'_>) -> Result<TermSearchOutput> {
    // A wider net than `limit` for every ranking source: fusing rankings
    // needs headroom past the final cut, or an asset one source ranks
    // just outside its own top `limit` — but that would rank first once
    // fused — never gets the chance to. With a hard filter present, every
    // ranked match is fetched (mirroring the filter-only path's own
    // fetch-everything-then-intersect rule) since a small prefetch window
    // could miss a filter-matching asset that ranks outside it.
    //
    // The semantic side gets `usize::MAX >> 1`, not `usize::MAX`:
    // `lancedb` 0.33.0 casts our `usize` limit to `i64` with a raw `as`
    // (`table/query.rs`: `query.base.limit.map(|limit| limit as i64)`), and
    // `usize::MAX >> 1 == i64::MAX` exactly on a 64-bit target — halving it
    // is what keeps that cast lossless instead of silently wrapping to a
    // negative `i64` (verified against the vendored `lancedb` source, not
    // guessed).
    let (fts_limit, semantic_limit) = if args.allowed.is_some() {
        (usize::MAX, usize::MAX >> 1)
    } else {
        let widened = args.limit.saturating_mul(4);
        (widened, widened)
    };

    let name_fts = if args.sources.is_none_or(|s| s.contains("name")) {
        db.search_names_ranked(args.terms, fts_limit)?
    } else {
        Vec::new()
    };
    let text_sources = selected_text_sources(args.sources);
    let (text_fts, mut text_meta) =
        text_fts_search(db, args.terms, text_sources.as_ref(), fts_limit)?;

    let state_dir = crate::state_dir::state_dir_for(args.catalog_dir, args.notices)?;
    let query_text = args.terms.join(" ");
    let (image_ids, keyframe_ts, embedded) = if image_semantic_enabled(args.sources) {
        semantic_candidates(&state_dir, &query_text, semantic_limit, args.notices)
    } else {
        (Vec::new(), HashMap::new(), None)
    };
    let (chunk_ids, chunk_meta) = if args.sources.is_none_or(|s| s.contains("transcript")) {
        text_semantic_candidates(&state_dir, &query_text, semantic_limit, args.notices)
    } else {
        (Vec::new(), HashMap::new())
    };
    // FTS meta wins when both exist: its snippet highlights the actual
    // term match; the chunk fills in for purely semantic hits.
    for (asset, meta) in chunk_meta {
        text_meta.entry(asset).or_insert(meta);
    }

    let ranked = fuse_ranked_n(&FuseInputs {
        name_fts,
        text_fts,
        semantic: vec![image_ids, chunk_ids],
        allowed: args.allowed,
        limit: args.limit,
    });

    let semantic_coverage = embedded.map(|embedded| SemanticCoverage {
        embedded,
        eligible: eligible_asset_count(args.projection),
    });
    let text_coverage = text_coverage_notices(db, args)?;
    Ok(TermSearchOutput {
        ranked,
        keyframe_ts,
        semantic_coverage,
        text_meta,
        text_coverage,
    })
}

/// Whether the image-vector layer participates: only on an unrestricted
/// query. Image vectors match visual content, not any nameable text source
/// — any `in:` restriction (even `in:name`: "name means names") turns them
/// off. A pure function so the gate itself is unit-testable: the CLI smoke
/// tests run without a `SigLIP` model and cannot tell "layer disabled" from
/// "ran and found nothing".
fn image_semantic_enabled(sources: Option<&BTreeSet<String>>) -> bool {
    sources.is_none()
}

/// The four `text_fts` sources restricted to the `in:` set — `None` when
/// the query is unrestricted (search every source). `Some(empty)` (e.g.
/// `in:name`) means "no text source at all".
fn selected_text_sources(sources: Option<&BTreeSet<String>>) -> Option<BTreeSet<String>> {
    sources.map(|selected| {
        TEXT_SOURCE_INFO
            .iter()
            .filter(|info| selected.contains(info.source))
            .map(|info| info.source.to_string())
            .collect()
    })
}

/// Ranked text-FTS hits plus each hit's per-asset detail.
type RankedTextHits = (Vec<(AssetId, f64)>, HashMap<AssetId, TextMeta>);

/// Ranked text-FTS hits (best row per asset, raw bm25 scores — same
/// convention as `search_names_ranked`) plus each hit's source/locator/
/// snippet detail.
///
/// # Errors
/// Returns an error if the underlying FTS query fails.
fn text_fts_search(
    db: &SqliteCatalog,
    terms: &[String],
    sources: Option<&BTreeSet<String>>,
    limit: usize,
) -> Result<RankedTextHits> {
    if sources.is_some_and(BTreeSet::is_empty) {
        return Ok((Vec::new(), HashMap::new()));
    }
    let hits = db.search_text_ranked(terms, sources, limit)?;
    let mut ranked = Vec::new();
    let mut meta = HashMap::new();
    for hit in hits {
        ranked.push((hit.asset.clone(), hit.score));
        meta.insert(
            hit.asset,
            TextMeta {
                source: hit.source,
                locator: hit.locator,
                snippet: hit.snippet,
            },
        );
    }
    Ok((ranked, meta))
}

/// Ranked inputs for [`fuse_ranked_n`], bundled to keep its signature
/// within the house 5-positional-parameter limit.
struct FuseInputs<'a> {
    /// bm25-scored filename hits, best-first (raw ranks: more negative is
    /// better; order is what matters here).
    name_fts: Vec<(AssetId, f64)>,
    /// bm25-scored indexed-text hits, best row per asset.
    text_fts: Vec<(AssetId, f64)>,
    /// Rank-ordered semantic lists (image vectors, transcript chunks).
    semantic: Vec<Vec<AssetId>>,
    allowed: Option<&'a BTreeSet<AssetId>>,
    limit: usize,
}

/// N-way reciprocal-rank fusion with the hard-filter intersection applied
/// to EVERY input list — the phase-4 filter-leak fix, generalized: a hit
/// outside the filter set must never survive fusion no matter which list it
/// arrived on. When only name FTS has results this is exactly the phase-4
/// N=1 behavior: bm25 scores and order, truncated at `limit`.
fn fuse_ranked_n(inputs: &FuseInputs<'_>) -> Vec<(AssetId, f64)> {
    let keep = |asset: &AssetId| inputs.allowed.is_none_or(|allowed| allowed.contains(asset));
    let name: Vec<(AssetId, f64)> = inputs
        .name_fts
        .iter()
        .filter(|(a, _)| keep(a))
        .cloned()
        .collect();
    let text: Vec<(AssetId, f64)> = inputs
        .text_fts
        .iter()
        .filter(|(a, _)| keep(a))
        .cloned()
        .collect();
    let semantic: Vec<Vec<AssetId>> = inputs
        .semantic
        .iter()
        .map(|list| list.iter().filter(|a| keep(a)).cloned().collect::<Vec<_>>())
        .filter(|list: &Vec<AssetId>| !list.is_empty())
        .collect();
    if text.is_empty() && semantic.is_empty() {
        let mut ranked = name;
        ranked.truncate(inputs.limit);
        return ranked;
    }
    let mut lists: Vec<Vec<AssetId>> = Vec::new();
    if !name.is_empty() {
        lists.push(name.into_iter().map(|(a, _)| a).collect());
    }
    if !text.is_empty() {
        lists.push(text.into_iter().map(|(a, _)| a).collect());
    }
    lists.extend(semantic);
    rrf_merge(&lists, inputs.limit)
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
            MediaKind::Audio | MediaKind::Pdf | MediaKind::Other => {}
        }
    }
    count
}

/// One `text_fts` source's coverage-notice metadata: its row key, the label
/// and eligible-population noun the notice carries, and which media kinds
/// the index planner considers eligible for it (mirroring
/// `majestical_index::work`'s per-kind eligibility).
struct TextSourceInfo {
    source: &'static str,
    label: &'static str,
    noun: &'static str,
    kinds: &'static [MediaKind],
}

/// The four text sources, in notice-print order.
const TEXT_SOURCE_INFO: [TextSourceInfo; 4] = [
    TextSourceInfo {
        source: "transcript",
        label: "transcripts",
        noun: "video/audio assets",
        kinds: &[MediaKind::Video, MediaKind::Audio],
    },
    TextSourceInfo {
        source: "caption",
        label: "captions",
        noun: "image/video/pdf assets",
        kinds: &[MediaKind::Image, MediaKind::Video, MediaKind::Pdf],
    },
    TextSourceInfo {
        source: "ocr",
        label: "ocr",
        noun: "image/video assets",
        kinds: &[MediaKind::Image, MediaKind::Video],
    },
    TextSourceInfo {
        source: "pdf",
        label: "pdf",
        noun: "pdf assets",
        kinds: &[MediaKind::Pdf],
    },
];

/// Catalog assets whose media kind (first instance's basename classifies,
/// as everywhere else) is one of `kinds`.
fn eligible_assets(projection: &Projection, kinds: &[MediaKind]) -> BTreeSet<AssetId> {
    let mut eligible = BTreeSet::new();
    for (asset, state) in projection.assets() {
        let Some((_, path)) = state.instances.keys().next() else {
            continue;
        };
        if kinds.contains(&media_kind(path)) {
            eligible.insert(asset.clone());
        }
    }
    eligible
}

/// Builds the per-source text coverage notices: for every text source this
/// search consulted, count the eligible assets versus those with `text_fts`
/// rows, and attach the remedy that closes the gap. A fully covered (or
/// empty-population) source produces no notice.
///
/// # Errors
/// Returns an error if a `text_assets` query fails.
fn text_coverage_notices(
    db: &SqliteCatalog,
    args: &TermSearchArgs<'_>,
) -> Result<Vec<TextCoverageNotice>> {
    let mut notices = Vec::new();
    for info in &TEXT_SOURCE_INFO {
        if !args.sources.is_none_or(|s| s.contains(info.source)) {
            continue;
        }
        let eligible = eligible_assets(args.projection, info.kinds);
        if eligible.is_empty() {
            continue;
        }
        let covered_all = db.text_assets(info.source)?;
        let covered = eligible.iter().filter(|a| covered_all.contains(*a)).count();
        if covered < eligible.len() {
            notices.push(TextCoverageNotice {
                label: info.label,
                noun: info.noun,
                covered,
                eligible: eligible.len(),
                remedy: source_remedy(info.source, args.catalog_dir, args.notices),
                source: info.source.to_string(),
            });
        }
    }
    Ok(notices)
}

/// The generic close-the-gap command for a text source with no capability
/// gate (OCR, PDF — and transcripts/captions once their models/backends are
/// in place).
const INDEX_RUN_REMEDY: &str = "run `maj index run`";

/// The remedy a coverage notice names for `source`: the shared `index
/// status` strings (see [`transcript_model_remedy`] and [`DESCRIBER_REMEDY`]
/// — shared consts so the two surfaces can't drift) when a capability is
/// missing, otherwise plain [`INDEX_RUN_REMEDY`].
fn source_remedy(source: &str, catalog_dir: &Path, notices: &crate::notices::Notices) -> String {
    match source {
        "transcript" => {
            let whisper = whisper_model_dir_if_present().is_some();
            let text_model = minilm_model_dir_if_present().is_some();
            transcript_model_remedy(whisper, text_model)
                .unwrap_or_else(|| INDEX_RUN_REMEDY.to_string())
        }
        "caption" => {
            // An unreadable describer config degrades to "unconfigured"
            // here, matching `index status`'s treatment — this only
            // selects which remedy line to print.
            let configured = crate::describer_config::load_config(catalog_dir, notices)
                .ok()
                .flatten()
                .is_some();
            if configured {
                INDEX_RUN_REMEDY.to_string()
            } else {
                DESCRIBER_REMEDY.to_string()
            }
        }
        _ => INDEX_RUN_REMEDY.to_string(),
    }
}

/// Reciprocal Rank Fusion: merges ranked `lists` into one ranking by
/// `1/(k+rank)` summed across every list. `k=60` is the standard RRF
/// constant — large enough that rank 1 and rank 2 contribute nearly the
/// same score, so fusion isn't dominated by whichever single list happens
/// to rank one asset first. Ties are broken by asset id for deterministic
/// output.
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
/// the catalog vs. rebuild a corrupt index), so a single generic
/// "unavailable" message would send readers to the wrong command.
enum SemanticMiss {
    NoModel,
    EmptyIndex,
    /// The local Lance store exists but couldn't be read — a corrupt
    /// dataset (see `majestical_index::vector_store::catch_corruption`'s
    /// doc comment for why that's even possible). Distinct from
    /// `EmptyIndex`: the fix is the same command (`maj index run`), but
    /// framed as a rebuild rather than a first run, since those read very
    /// differently to an operator. `search` never repairs this itself —
    /// only `index run` writes, so only it may delete and rebuild.
    Unreadable(String),
}

impl SemanticMiss {
    fn note(&self) -> String {
        match self {
            SemanticMiss::NoModel => {
                "semantic search unavailable — run `maj model fetch`".to_string()
            }
            SemanticMiss::EmptyIndex => "semantic index is empty — run `maj index run`".to_string(),
            SemanticMiss::Unreadable(reason) => {
                format!("semantic index unreadable ({reason}) — run `maj index run` to rebuild")
            }
        }
    }
}

/// Resolves the model (cheap: file-size checks only), then opens the local
/// Lance store READ-ONLY — never creating one, since a search must not
/// materialize local state just by running — and confirms it holds at least
/// one embedding for `MODEL_TAG`. All of this (model presence, opening,
/// probing) resolves fully before any caller ever loads the text encoder,
/// so a search against an empty or nonexistent index degrades without
/// paying for a model load at all. The open AND the `distinct_assets` count
/// both run inside the same `catch_corruption` closure — not open-then-
/// probe-then-a-second-separate-scan — so a read error from counting rows
/// is caught as `Unreadable` too, rather than being silently swallowed into
/// `embedded == 0` (indistinguishable from a genuinely empty index) by a
/// `.ok()`/`map_or` outside the guard.
fn open_semantic_index(state_dir: &Path) -> Result<(PathBuf, VectorStore, u64), SemanticMiss> {
    let model_dir = majestical_index::model::model_dir_for(&SIGLIP)
        .ok()
        .filter(|dir| majestical_index::model::model_present_for(&SIGLIP, dir))
        .ok_or(SemanticMiss::NoModel)?;

    let lance_dir = state_dir.join("lance");
    let opened = majestical_index::vector_store::catch_corruption(move || {
        let Some(store) = VectorStore::open_existing(&lance_dir)? else {
            return Ok(None);
        };
        let embedded = store
            .distinct_assets(majestical_index::model::MODEL_TAG)?
            .len();
        Ok(Some((store, embedded)))
    });
    let (store, embedded) = match opened {
        Ok(Some(opened)) => opened,
        Ok(None) => return Err(SemanticMiss::EmptyIndex),
        Err(reason) => return Err(SemanticMiss::Unreadable(reason)),
    };
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

/// Collapses Lance hits to one entry per asset, keeping only the first
/// (nearest, since hits arrive nearest-first) occurrence. An asset can hit
/// more than once — its whole-image embedding plus one or more keyframe
/// hits once video keyframing lands — and feeding every raw hit into
/// `rrf_merge` verbatim would inflate that asset's score by how many times
/// it happened to hit, not by how well it actually matched: `rrf_merge`'s
/// scoring map accumulates one contribution per list occurrence, so it does
/// nothing to prevent that on its own (the final ranking is unique per
/// asset either way, since the map is keyed by asset id — that part is
/// redundant with what's built here; the score would still be wrong without
/// this). Also collects each asset's nearest keyframe timestamp (keyframe
/// hits only; a whole-image hit has none).
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
/// search, never a hard requirement — recording the specific note for the
/// reason (see [`SemanticMiss`]) into `notices`.
///
/// Returns ranked asset ids (nearest first, deduped), each ranked asset's
/// nearest keyframe timestamp, and `Some(embedded distinct asset count)`
/// when the layer actually ran (used for the coverage notice).
fn semantic_candidates(
    state_dir: &Path,
    query: &str,
    limit: usize,
    notices: &crate::notices::Notices,
) -> (Vec<AssetId>, HashMap<AssetId, i64>, Option<u64>) {
    let (model_dir, store, embedded) = match open_semantic_index(state_dir) {
        Ok(opened) => opened,
        Err(miss) => {
            notices.push(miss.note());
            return (Vec::new(), HashMap::new(), None);
        }
    };
    let Some(vector) = embed_query(&model_dir, query) else {
        notices.push(SemanticMiss::NoModel.note());
        return (Vec::new(), HashMap::new(), None);
    };
    let hits = match store.search(&vector, majestical_index::model::MODEL_TAG, limit) {
        Ok(hits) => hits,
        Err(err) => {
            // A real read failure here, not "nothing indexed" — the store
            // passed `open_semantic_index`'s probe, so this is either a
            // transient I/O error or the known gap in that probe's
            // coverage (see `catch_corruption`'s doc comment: it never
            // reads the `vector` column, only `search` does). Either way
            // it's `Unreadable`, not `EmptyIndex` — discarding `err` here
            // would silently relabel "unreadable" as "empty".
            notices.push(SemanticMiss::Unreadable(err.to_string()).note());
            return (Vec::new(), HashMap::new(), None);
        }
    };
    let (ranked, keyframe_ts) = dedupe_hits(hits);
    (ranked, keyframe_ts, Some(embedded))
}

/// Why the transcript-semantic layer couldn't run this search — the text
/// analogue of [`SemanticMiss`], with its own messages because the fixes
/// differ: the model here is `MiniLM`, and the index is the `text_chunks`
/// Lance table rather than the image `vectors` table.
enum TextSemanticMiss {
    NoModel,
    EmptyIndex,
    /// The `text_chunks` table exists but couldn't be read — same corrupt-
    /// dataset possibility as [`SemanticMiss::Unreadable`], same rule:
    /// `search` never repairs, only `index run` may rebuild.
    Unreadable(String),
}

impl TextSemanticMiss {
    fn note(&self) -> String {
        match self {
            TextSemanticMiss::NoModel => format!(
                "transcript search unavailable — run `maj model fetch --only {}`",
                MINILM.tag
            ),
            TextSemanticMiss::EmptyIndex => {
                "transcript index is empty — run `maj index run`".to_string()
            }
            TextSemanticMiss::Unreadable(reason) => {
                format!("transcript index unreadable ({reason}) — run `maj index run` to rebuild")
            }
        }
    }
}

/// Resolves the `MiniLM` model (file-size checks only), then opens the
/// local `text_chunks` Lance table READ-ONLY and confirms it holds at least
/// one chunk for `MINILM.tag` — mirroring [`open_semantic_index`]'s
/// structure exactly, including running the open AND the probe inside one
/// `catch_corruption` closure so a counting-read error surfaces as
/// `Unreadable` instead of masquerading as an empty index.
fn open_text_semantic_index(
    state_dir: &Path,
) -> Result<(PathBuf, TextVectorStore), TextSemanticMiss> {
    let model_dir = majestical_index::model::model_dir_for(&MINILM)
        .ok()
        .filter(|dir| majestical_index::model::model_present_for(&MINILM, dir))
        .ok_or(TextSemanticMiss::NoModel)?;

    let lance_dir = state_dir.join("lance");
    let opened = majestical_index::vector_store::catch_corruption(move || {
        let Some(store) = TextVectorStore::open_existing(&lance_dir)? else {
            return Ok(None);
        };
        let embedded = store.distinct_assets(MINILM.tag)?.len();
        Ok(Some((store, embedded)))
    });
    let (store, embedded) = match opened {
        Ok(Some(opened)) => opened,
        Ok(None) => return Err(TextSemanticMiss::EmptyIndex),
        Err(reason) => return Err(TextSemanticMiss::Unreadable(reason)),
    };
    if embedded == 0 {
        return Err(TextSemanticMiss::EmptyIndex);
    }
    Ok((model_dir, store))
}

/// Loads the `MiniLM` text encoder and embeds `query` (384-d unit-norm).
fn embed_text_query(model_dir: &Path, query: &str) -> Option<Vec<f32>> {
    let mut encoder = TextEncoder::load(model_dir).ok()?;
    encoder.embed(query).ok()
}

/// Truncates chunk text to a short display snippet — chunks run to a few
/// sentences, far too long to attach to every result row verbatim.
fn snippet_text(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    let trimmed = text.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_CHARS).collect();
    format!("{cut}\u{2026}")
}

/// Collapses chunk hits to one entry per asset, keeping the first (nearest)
/// chunk's rank and its `(start_ms, text)` — same score-inflation rationale
/// as [`dedupe_hits`]: an asset with many near-matching chunks must rank by
/// how well it matched, not how often.
fn dedupe_text_chunk_hits(hits: Vec<TextChunkHit>) -> (Vec<AssetId>, HashMap<AssetId, TextMeta>) {
    let mut ranked = Vec::new();
    let mut meta: HashMap<AssetId, TextMeta> = HashMap::new();
    for hit in hits {
        let asset = AssetId(format!("xxh3:{}", hit.asset_hex));
        if meta.contains_key(&asset) {
            continue;
        }
        ranked.push(asset.clone());
        meta.insert(
            asset,
            TextMeta {
                source: hit.source,
                locator: hit.start_ms,
                snippet: snippet_text(&hit.text),
            },
        );
    }
    (ranked, meta)
}

/// Runs the transcript-semantic side of a search: embeds `query` with
/// `MiniLM` and nearest-neighbor searches the local `text_chunks` table.
/// Degrades to `(empty, empty)` on any miss — additive, never a hard
/// requirement — recording the specific note for the reason (see
/// [`TextSemanticMiss`]) into `notices`, mirroring [`semantic_candidates`].
fn text_semantic_candidates(
    state_dir: &Path,
    query: &str,
    limit: usize,
    notices: &crate::notices::Notices,
) -> (Vec<AssetId>, HashMap<AssetId, TextMeta>) {
    let (model_dir, store) = match open_text_semantic_index(state_dir) {
        Ok(opened) => opened,
        Err(miss) => {
            notices.push(miss.note());
            return (Vec::new(), HashMap::new());
        }
    };
    let Some(vector) = embed_text_query(&model_dir, query) else {
        notices.push(TextSemanticMiss::NoModel.note());
        return (Vec::new(), HashMap::new());
    };
    let hits = match store.search(&vector, MINILM.tag, limit) {
        Ok(hits) => hits,
        Err(err) => {
            // Same open-passed-but-read-failed reasoning as
            // `semantic_candidates`: `Unreadable`, never relabeled empty.
            notices.push(TextSemanticMiss::Unreadable(err.to_string()).note());
            return (Vec::new(), HashMap::new());
        }
    };
    dedupe_text_chunk_hits(hits)
}

/// Resolves parsed `key:value` tokens against `majestical_core::ports::Filter`'s
/// per-variant contracts. `vol`/`volume` both address `Filter::Volume`;
/// `before`/`after` reject a `-` negation (there's no negated form) rather
/// than silently ignoring it.
fn resolve_filters(
    projection: &Projection,
    raw: &[&crate::query::RawFilter],
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

/// `maj searches list`: every saved search, name and query text.
///
/// # Errors
/// Returns an error if the event log cannot be read.
pub fn searches_list(app: &FsApp) -> Result<Vec<SavedSearch>, ServiceError> {
    Ok(app
        .projection()?
        .saved_searches()
        .map(|(name, query)| SavedSearch {
            name: name.to_string(),
            query: query.to_string(),
        })
        .collect())
}

/// `maj searches rm`: removes a saved search by name.
///
/// # Errors
/// Returns an error if `name` is empty, names no saved search, or the event
/// log can't be read or appended to.
pub fn searches_rm(app: &mut FsApp, name: &str) -> Result<(), ServiceError> {
    searches_rm_impl(app, name).map_err(ServiceError::from)
}

fn searches_rm_impl(app: &mut FsApp, name: &str) -> Result<()> {
    anyhow::ensure!(!name.is_empty(), "saved search name must not be empty");
    let projection = app.projection()?;
    projection
        .saved_search(name)
        .with_context(|| format!("no saved search named '{name}'"))?;
    app.emit(vec![Op::SavedSearchRemove {
        name: name.to_string(),
    }])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search hit never populates `size`/`mtime_ms`/`kind` (only
    /// `browse_list` does) — pins that those three keys are absent from the
    /// wire entirely when `None`, not emitted as `null`. Protects the
    /// existing `wire_fixtures` search fixtures: if this regresses, the
    /// added fields would land on top of every search result the desktop
    /// app already snapshotted.
    #[test]
    fn a_search_hit_with_no_instance_attributes_omits_those_keys_on_the_wire() {
        let hit = SearchHit {
            asset: "xxh3:aa".into(),
            score: 0.0,
            known: true,
            name: "clip.mov".into(),
            volumes: Vec::new(),
            tags: Vec::new(),
            para: None,
            timestamp_ms: None,
            source: None,
            locator: None,
            snippet: None,
            size: None,
            mtime_ms: None,
            kind: None,
        };
        let json = serde_json::to_string(&hit).expect("serialize");
        assert!(!json.contains("\"size\""), "json: {json}");
        assert!(!json.contains("\"mtime_ms\""), "json: {json}");
        assert!(!json.contains("\"kind\""), "json: {json}");
    }

    #[test]
    fn search_outcome_carries_rows_and_notices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        // Fixture events emitted directly (scan service doesn't exist yet;
        // a later task owns it). Mirrors `cmd_scan`'s `Op` construction.
        app.emit(vec![
            majestical_core::event::Op::VolumeSeen {
                volume: "vol1".into(),
                label: "vol1".into(),
            },
            majestical_core::event::Op::AssetSeen {
                asset: majestical_core::event::AssetId(
                    "xxh3:0123456789abcdef0123456789abcdef".into(),
                ),
                volume: "vol1".into(),
                path: "clip.txt".into(),
                size: 5,
                mtime_ms: 1000,
            },
        ])
        .expect("emit");
        let out = search(
            &mut app,
            &root,
            &SearchRequest {
                query: Some("clip".into()),
                limit: 50,
                saved: None,
                save: None,
            },
        )
        .expect("search");
        assert_eq!(out.count, 1);
        assert_eq!(out.results[0].name, "clip.txt");
        assert!(out.results[0].asset.starts_with("xxh3:"));
        // A plain text asset is not eligible for any text/semantic layer
        // (see TEXT_SOURCE_INFO's kinds and image_semantic_enabled), so a
        // text-only fixture must never carry a coverage notice — pins "no
        // spurious notices for text-only catalogs".
        assert!(out.text_coverage.is_empty());
        assert!(out.semantic_coverage.is_none());
    }

    /// A diagnostic collected while the verb computes must ride the outcome
    /// home, not escape to stderr from inside services. A filter-only query
    /// keeps this off the semantic layer entirely, so the assertion doesn't
    /// depend on which models happen to be installed.
    #[test]
    fn search_outcome_carries_collected_notices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        let mut app = crate::app::FsApp::init(&root, "m1", "m1").expect("init");
        app.emit(vec![majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId("xxh3:0123456789abcdef0123456789abcdef".into()),
            volume: "vol1".into(),
            path: "clip.txt".into(),
            size: 5,
            mtime_ms: 1000,
        }])
        .expect("emit");
        // `FileEventLog` lays segments out as `events/<machine-id>/NNNN.jsonl`.
        let machine_dir = root.join("events").join("m1");
        let log_file = std::fs::read_dir(&machine_dir)
            .expect("machine events dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .expect("one events jsonl");
        let mut bytes = std::fs::read(&log_file).expect("read log");
        bytes.extend_from_slice(b"this is not json\n");
        std::fs::write(&log_file, bytes).expect("re-write log");

        let out = search(
            &mut app,
            &root,
            &SearchRequest {
                query: Some("tag:missing".into()),
                limit: 10,
                saved: None,
                save: None,
            },
        )
        .expect("search");
        assert!(
            out.notices
                .iter()
                .any(|note| note.contains("corrupt event log line")),
            "the corrupt-log warning must reach the caller: {:?}",
            out.notices
        );
    }
}

#[cfg(test)]
mod semantic_tests {
    use super::{
        AssetId, FuseInputs, dedupe_hits, dedupe_text_chunk_hits, eligible_asset_count,
        fuse_ranked_n, in_sources, rrf_merge, snippet_text,
    };
    use majestical_core::clock::{Hlc, MachineId};
    use majestical_core::event::{Event, EventId, Op};
    use majestical_core::projection::Projection;
    use majestical_index::vector_store::{TextChunkHit, VectorHit};
    use std::collections::BTreeSet;

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

    /// Migrated from the phase-4 two-list `fuse_ranked` — pins the same
    /// behavior at N=2 (name FTS + one semantic list).
    #[test]
    fn fuse_n_drops_semantic_hits_outside_the_filter_set() {
        let kept = AssetId("xxh3:aaa".into());
        let excluded = AssetId("xxh3:bbb".into());
        let allowed: BTreeSet<AssetId> = [kept.clone()].into_iter().collect();
        // `excluded` is the top semantic hit and absent from FTS entirely —
        // exactly the shape a `-tag:x` exclusion produces.
        let merged = fuse_ranked_n(&FuseInputs {
            name_fts: vec![(kept.clone(), 1.0)],
            text_fts: Vec::new(),
            semantic: vec![vec![excluded, kept.clone()]],
            allowed: Some(&allowed),
            limit: 10,
        });
        let ids: Vec<&AssetId> = merged.iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![&kept],
            "a hard filter must survive semantic fusion"
        );
    }

    /// Migrated from the phase-4 two-list `fuse_ranked`, same N=2 pin.
    #[test]
    fn fuse_n_drops_fts_hits_outside_the_filter_set() {
        let kept = AssetId("xxh3:aaa".into());
        let excluded = AssetId("xxh3:bbb".into());
        let allowed: BTreeSet<AssetId> = [kept.clone()].into_iter().collect();
        let merged = fuse_ranked_n(&FuseInputs {
            name_fts: vec![(excluded, 2.0), (kept.clone(), 1.0)],
            text_fts: Vec::new(),
            semantic: vec![vec![kept.clone()]],
            allowed: Some(&allowed),
            limit: 10,
        });
        let ids: Vec<&AssetId> = merged.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![&kept]);
    }

    #[test]
    fn fuse_n_hard_filters_every_list() {
        let allowed: BTreeSet<AssetId> = [AssetId("xxh3:aa".into())].into();
        let name_fts = vec![
            (AssetId("xxh3:aa".into()), -1.0),
            (AssetId("xxh3:zz".into()), -2.0),
        ];
        let text_fts = vec![(AssetId("xxh3:zz".into()), -3.0)];
        let semantic_lists = vec![
            vec![AssetId("xxh3:zz".into())],
            vec![AssetId("xxh3:zz".into()), AssetId("xxh3:aa".into())],
        ];
        let fused = fuse_ranked_n(&FuseInputs {
            name_fts,
            text_fts,
            semantic: semantic_lists,
            allowed: Some(&allowed),
            limit: 10,
        });
        assert!(!fused.is_empty(), "the allowed asset itself must survive");
        assert!(
            fused.iter().all(|(asset, _)| asset.0 == "xxh3:aa"),
            "the phase-4 BLOCKER: zz is filtered out of EVERY list, ranked or semantic"
        );
    }

    #[test]
    fn fuse_n_reduces_to_bm25_when_only_name_fts_has_results() {
        let name_fts = vec![
            (AssetId("xxh3:aa".into()), -1.5),
            (AssetId("xxh3:bb".into()), -0.5),
        ];
        let fused = fuse_ranked_n(&FuseInputs {
            name_fts: name_fts.clone(),
            text_fts: vec![],
            semantic: vec![],
            allowed: None,
            limit: 10,
        });
        assert_eq!(
            fused, name_fts,
            "bm25 scores and order preserved (phase-4 behavior at N=1)"
        );
    }

    /// Migrated from the phase-4 `fuse_ranked_without_semantic_hits...`:
    /// the bm25 fallback also truncates at `limit`.
    #[test]
    fn fuse_n_bm25_fallback_truncates_at_limit() {
        let a = AssetId("xxh3:aaa".into());
        let b = AssetId("xxh3:bbb".into());
        let fused = fuse_ranked_n(&FuseInputs {
            name_fts: vec![(b.clone(), 9.0), (a, 1.0)],
            text_fts: vec![],
            semantic: vec![],
            allowed: None,
            limit: 1,
        });
        assert_eq!(
            fused,
            vec![(b, 9.0)],
            "fts-only keeps bm25 scores and order"
        );
    }

    #[test]
    fn fuse_n_rrf_merges_all_nonempty_lists() {
        // An asset ranked in three lists beats one ranked in a single list.
        let name_fts = vec![(AssetId("xxh3:aa".into()), -1.0)];
        let text_fts = vec![
            (AssetId("xxh3:bb".into()), -1.0),
            (AssetId("xxh3:aa".into()), -0.5),
        ];
        let semantic = vec![vec![AssetId("xxh3:aa".into()), AssetId("xxh3:bb".into())]];
        let fused = fuse_ranked_n(&FuseInputs {
            name_fts,
            text_fts,
            semantic,
            allowed: None,
            limit: 10,
        });
        assert_eq!(fused[0].0.0, "xxh3:aa");
    }

    #[test]
    fn fuse_n_limit_truncates() {
        let name_fts: Vec<_> = (0..20)
            .map(|i| (AssetId(format!("xxh3:{i:02}")), -f64::from(i)))
            .collect();
        // A nonempty text list forces the RRF path (not the bm25 fallback),
        // so this pins rrf-side truncation specifically.
        let fused = fuse_ranked_n(&FuseInputs {
            name_fts,
            text_fts: vec![(AssetId("xxh3:00".into()), -1.0)],
            semantic: vec![],
            allowed: None,
            limit: 5,
        });
        assert_eq!(fused.len(), 5);
    }

    fn raw(key: &str, value: &str, negated: bool) -> crate::query::RawFilter {
        crate::query::RawFilter {
            key: key.into(),
            value: value.into(),
            negated,
        }
    }

    #[test]
    fn image_semantic_enabled_only_without_an_in_restriction() {
        assert!(
            super::image_semantic_enabled(None),
            "an unrestricted query includes the image-vector layer"
        );
        let transcript: BTreeSet<String> = ["transcript".to_string()].into();
        assert!(
            !super::image_semantic_enabled(Some(&transcript)),
            "in:transcript must not surface image-vector hits"
        );
        let name: BTreeSet<String> = ["name".to_string()].into();
        assert!(
            !super::image_semantic_enabled(Some(&name)),
            "in:name means names — no image-vector hits"
        );
    }

    #[test]
    fn in_sources_unions_values_and_ignores_other_keys() {
        let filters = vec![
            raw("in", "transcript", false),
            raw("tag", "pets", false),
            raw("in", "ocr", false),
        ];
        let sources = in_sources(&filters).expect("valid").expect("restricted");
        let got: Vec<&str> = sources.iter().map(String::as_str).collect();
        assert_eq!(got, vec!["ocr", "transcript"]);
    }

    #[test]
    fn in_sources_without_in_filters_is_unrestricted() {
        let filters = vec![raw("tag", "pets", false)];
        assert!(in_sources(&filters).expect("valid").is_none());
    }

    #[test]
    fn in_sources_rejects_negation() {
        let err = in_sources(&[raw("in", "ocr", true)]).expect_err("must fail");
        assert!(err.to_string().contains("negation"), "{err}");
    }

    #[test]
    fn in_sources_rejects_unknown_values_naming_the_valid_set() {
        let err = in_sources(&[raw("in", "subtitles", false)]).expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("subtitles") && msg.contains("transcript"),
            "{msg}"
        );
    }

    #[test]
    fn snippet_text_trims_and_truncates_on_a_char_boundary() {
        assert_eq!(snippet_text("  short  "), "short");
        let long = "é".repeat(200);
        let cut = snippet_text(&long);
        assert!(cut.chars().count() == 81 && cut.ends_with('\u{2026}'));
    }

    #[test]
    fn dedupe_text_chunk_hits_keeps_the_nearest_chunk_per_asset() {
        let hit = |ms: i64, text: &str| TextChunkHit {
            asset_hex: "aa11".into(),
            source: "transcript".into(),
            start_ms: ms,
            end_ms: ms + 1000,
            text: text.into(),
            distance: 0.1,
        };
        let (ranked, meta) = dedupe_text_chunk_hits(vec![hit(5000, "best"), hit(9000, "worse")]);
        let asset = AssetId("xxh3:aa11".into());
        assert_eq!(ranked, vec![asset.clone()]);
        let kept = meta.get(&asset).expect("meta");
        assert_eq!((kept.locator, kept.snippet.as_str()), (5000, "best"));
    }

    #[test]
    fn dedupe_hits_keeps_a_multi_hit_asset_at_a_single_rank_not_inflated() {
        let repeated = "aa11".to_string();
        let hits = vec![
            VectorHit {
                asset_hex: repeated.clone(),
                kind: "keyframe".into(),
                ts_ms: 1000,
                distance: 0.1,
            },
            VectorHit {
                asset_hex: repeated.clone(),
                kind: "keyframe".into(),
                ts_ms: 2000,
                distance: 0.2,
            },
            VectorHit {
                asset_hex: repeated.clone(),
                kind: "keyframe".into(),
                ts_ms: 3000,
                distance: 0.3,
            },
        ];
        let (ranked, keyframe_ts) = dedupe_hits(hits);
        let asset = AssetId(format!("xxh3:{repeated}"));
        assert_eq!(
            ranked,
            vec![asset.clone()],
            "three hits for one asset must collapse to one entry — feeding all three into \
             rrf_merge would inflate this asset's score threefold, not reflect how well it \
             actually matched"
        );
        assert_eq!(
            keyframe_ts.get(&asset),
            Some(&1000),
            "the nearest (first-encountered) hit's timestamp wins"
        );
    }

    #[test]
    fn dedupe_hits_on_no_hits_is_empty() {
        let (ranked, keyframe_ts) = dedupe_hits(Vec::new());
        assert!(ranked.is_empty());
        assert!(keyframe_ts.is_empty());
    }

    fn seen_event(n: u64, asset: &str, path: &str) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(1, n.into())),
            hlc: Hlc {
                wall_ms: n,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op: Op::AssetSeen {
                asset: AssetId(asset.to_string()),
                volume: "v1".into(),
                path: path.to_string(),
                size: 10,
                mtime_ms: 0,
            },
        }
    }

    #[test]
    fn eligible_asset_count_counts_only_image_and_video_kinds() {
        let mut projection = Projection::default();
        for (n, asset, path) in [
            (1u64, "xxh3:a", "photo.jpg"),
            (2, "xxh3:b", "clip.mov"),
            (3, "xxh3:c", "notes.txt"),
        ] {
            projection.apply(&seen_event(n, asset, path));
        }
        assert_eq!(
            eligible_asset_count(&projection),
            2,
            "only the image and video assets are embedding-eligible"
        );
    }

    #[test]
    fn eligible_asset_count_of_an_empty_projection_is_zero() {
        assert_eq!(eligible_asset_count(&Projection::default()), 0);
    }
}
