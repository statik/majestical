//! `maj search`/`maj searches`: the query-language search command and its
//! saved-search management. Parsing lives in `query`; this module resolves
//! parsed filters against the catalog and renders results.
use crate::SearchesCmd;
use crate::commands::{open_catalog, resolve_para_node};
use anyhow::{Context, Result, bail};
use majestical_catalog_sqlite::SqliteCatalog;
use majestical_core::event::{AssetId, Op};
use majestical_core::media_kind::{MediaKind, media_kind};
use majestical_core::ports::{AssetSummary, Filter};
use majestical_core::projection::Projection;
use majestical_index::model::{MINILM, SIGLIP};
use majestical_index::text_encoder::TextEncoder;
use majestical_index::vector_store::{TextChunkHit, TextVectorStore, VectorHit, VectorStore};
use majestical_services::app::FsApp;
use majestical_services::volume_identity;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
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
const FILTER_KEYS: &str = "tag, vol/volume, para, kind, online, before, after, in";

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
    // Resolved once and shared: `resolve_filter`'s `online:` arm and
    // `print_search_results`'s per-volume online flag both need the mounted
    // set, and each call shells out to `diskutil` per mount — computing it
    // twice would double a search's latency for no benefit.
    let mounted = volume_identity::mounted_volumes();
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
                terms: &parsed.terms,
                allowed: allowed.as_ref(),
                limit,
                projection: &projection,
                sources: sources.as_ref(),
            },
        )?
    };
    print_search_results(
        &db,
        &out.ranked,
        &PrintOptions {
            keyframe_ts: &out.keyframe_ts,
            mounted: &mounted,
            limit,
            json,
            coverage: out.semantic_coverage,
            text_meta: &out.text_meta,
            text_coverage: &out.text_coverage,
        },
    )
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
    terms: &'a [String],
    allowed: Option<&'a BTreeSet<AssetId>>,
    limit: usize,
    projection: &'a Projection,
    /// The `in:` source restriction — `None` searches every source.
    sources: Option<&'a BTreeSet<String>>,
}

/// The text-row detail printed alongside a hit that matched (or
/// semantically resembled) indexed text: which source, that row's locator
/// (ms timestamp, PDF page, or -1 for none), and a short snippet.
struct TextMeta {
    source: String,
    locator: i64,
    snippet: String,
}

/// One per-source coverage notice: how much of the eligible catalog this
/// text source has actually indexed, and the command that closes the gap.
struct TextCoverage {
    label: &'static str,
    noun: &'static str,
    covered: usize,
    eligible: usize,
    remedy: String,
}

/// Everything a terms-bearing search produces beyond the ranking itself.
#[derive(Default)]
struct TermSearchOutput {
    ranked: Vec<(AssetId, f64)>,
    /// Each ranked asset's nearest keyframe timestamp (image-semantic
    /// keyframe hits only).
    keyframe_ts: HashMap<AssetId, i64>,
    /// `Some((embedded, eligible))` when the image-semantic layer ran.
    semantic_coverage: Option<(u64, u64)>,
    /// Per-asset text detail for printing (FTS row, else best chunk).
    text_meta: HashMap<AssetId, TextMeta>,
    /// Per-source text coverage notices, in [`TEXT_SOURCE_INFO`] order.
    text_coverage: Vec<TextCoverage>,
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

    let state_dir = majestical_services::state_dir::state_dir_for(args.catalog_dir)?;
    let query_text = args.terms.join(" ");
    let (image_ids, keyframe_ts, embedded) = if image_semantic_enabled(args.sources) {
        semantic_candidates(&state_dir, &query_text, semantic_limit)
    } else {
        (Vec::new(), HashMap::new(), None)
    };
    let (chunk_ids, chunk_meta) = if args.sources.is_none_or(|s| s.contains("transcript")) {
        text_semantic_candidates(&state_dir, &query_text, semantic_limit)
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

    let semantic_coverage =
        embedded.map(|embedded| (embedded, eligible_asset_count(args.projection)));
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

/// Ranked text-FTS hits plus each hit's per-asset print detail.
type RankedTextHits = (Vec<(AssetId, f64)>, HashMap<AssetId, TextMeta>);

/// Ranked text-FTS hits (best row per asset, raw bm25 scores — same
/// convention as `search_names_ranked`) plus each hit's source/locator/
/// snippet detail for printing.
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
/// and eligible-population noun the notice prints, and which media kinds
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
) -> Result<Vec<TextCoverage>> {
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
            notices.push(TextCoverage {
                label: info.label,
                noun: info.noun,
                covered,
                eligible: eligible.len(),
                remedy: source_remedy(info.source, args.catalog_dir),
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
/// status` strings (see `index_cmd::transcript_model_remedy` and
/// `index_cmd::DESCRIBER_REMEDY` — shared consts so the two surfaces can't
/// drift) when a capability is missing, otherwise plain
/// [`INDEX_RUN_REMEDY`].
fn source_remedy(source: &str, catalog_dir: &Path) -> String {
    match source {
        "transcript" => {
            let whisper = crate::index_cmd::whisper_model_dir_if_present().is_some();
            let text_model = crate::index_cmd::minilm_model_dir_if_present().is_some();
            crate::index_cmd::transcript_model_remedy(whisper, text_model)
                .unwrap_or_else(|| INDEX_RUN_REMEDY.to_string())
        }
        "caption" => {
            // An unreadable describer config degrades to "unconfigured"
            // here, matching `index_cmd::capabilities`' treatment — this
            // only selects which remedy line to print.
            let configured = crate::describer_cmd::load_config(catalog_dir)
                .ok()
                .flatten()
                .is_some();
            if configured {
                INDEX_RUN_REMEDY.to_string()
            } else {
                crate::index_cmd::DESCRIBER_REMEDY.to_string()
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
            eprintln!("{}", SemanticMiss::Unreadable(err.to_string()).note());
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
/// sentences, far too long to append to every result line verbatim.
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
/// chunk's rank and its `(start_ms, text)` for printing — same
/// score-inflation rationale as [`dedupe_hits`]: an asset with many
/// near-matching chunks must rank by how well it matched, not how often.
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
/// requirement — printing the specific stderr note for the reason (see
/// [`TextSemanticMiss`]), mirroring [`semantic_candidates`].
fn text_semantic_candidates(
    state_dir: &Path,
    query: &str,
    limit: usize,
) -> (Vec<AssetId>, HashMap<AssetId, TextMeta>) {
    let (model_dir, store) = match open_text_semantic_index(state_dir) {
        Ok(opened) => opened,
        Err(miss) => {
            eprintln!("{}", miss.note());
            return (Vec::new(), HashMap::new());
        }
    };
    let Some(vector) = embed_text_query(&model_dir, query) else {
        eprintln!("{}", TextSemanticMiss::NoModel.note());
        return (Vec::new(), HashMap::new());
    };
    let hits = match store.search(&vector, MINILM.tag, limit) {
        Ok(hits) => hits,
        Err(err) => {
            // Same open-passed-but-read-failed reasoning as
            // `semantic_candidates`: `Unreadable`, never relabeled empty.
            eprintln!("{}", TextSemanticMiss::Unreadable(err.to_string()).note());
            return (Vec::new(), HashMap::new());
        }
    };
    dedupe_text_chunk_hits(hits)
}

/// Formats a millisecond timestamp (keyframe or transcript/OCR locator) as
/// `@MmSSs` (e.g. `@1m05s`), text-mode only.
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
    /// Per-asset text detail (source, locator, snippet) for hits that
    /// matched or resembled indexed text; empty otherwise.
    text_meta: &'a HashMap<AssetId, TextMeta>,
    /// Per-source text coverage notices; empty for filter-only queries.
    text_coverage: &'a [TextCoverage],
}

/// Renders one hit's text detail: locator (` @MmSSs` for a ms timestamp,
/// ` p<page>` for a PDF page, nothing for locator -1) followed by the
/// quoted snippet.
fn render_text_meta(meta: &TextMeta) -> String {
    let mut out = String::new();
    if meta.source == "pdf" {
        let _ = write!(out, "  p{}", meta.locator);
    } else if meta.locator >= 0 {
        let _ = write!(out, "  {}", format_ts(meta.locator));
    }
    let _ = write!(out, "  \"{}\"", meta.snippet);
    out
}

/// Renders ranked results: JSON prints one object per hit with its volumes
/// (online/offline per the currently mounted set), tags, (for a semantic
/// keyframe hit) `timestamp_ms`, and (for a text hit) `source`/`locator`/
/// `snippet`; text prints one line per hit (`{asset} {name}  [label●|○,...]`,
/// `tags:`, `@MmSSs`, and the text hit's locator + quoted snippet appended
/// when present) followed by a `"{n} results"` summary line, a truncation
/// hint when the result count hit `limit` exactly, and — when a layer ran
/// but hasn't indexed every eligible asset yet — its coverage notice.
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
            if let Some(meta) = opts.text_meta.get(asset) {
                result["source"] = serde_json::json!(meta.source);
                result["locator"] = serde_json::json!(meta.locator);
                result["snippet"] = serde_json::json!(meta.snippet);
            }
            result
        })
        .collect();
    let mut payload = serde_json::json!({ "count": ranked.len(), "results": results });
    if let Some((embedded, eligible)) = opts.coverage {
        payload["semantic_coverage"] =
            serde_json::json!({ "embedded": embedded, "eligible": eligible });
    }
    if !opts.text_coverage.is_empty() {
        let notices: Vec<_> = opts
            .text_coverage
            .iter()
            .map(|notice| {
                serde_json::json!({
                    "source": notice.label,
                    "covered": notice.covered,
                    "eligible": notice.eligible,
                    "remedy": notice.remedy,
                })
            })
            .collect();
        payload["text_coverage"] = serde_json::json!(notices);
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
        if let Some(meta) = opts.text_meta.get(asset) {
            print!("{}", render_text_meta(meta));
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
    for notice in opts.text_coverage {
        println!(
            "{}: {} of {} {} — {}",
            notice.label, notice.covered, notice.eligible, notice.noun, notice.remedy
        );
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
    use super::{
        AssetId, FuseInputs, dedupe_hits, dedupe_text_chunk_hits, eligible_asset_count, format_ts,
        fuse_ranked_n, in_sources, render_text_meta, rrf_merge, snippet_text,
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
    fn render_text_meta_formats_each_locator_kind() {
        let meta = |source: &str, locator: i64| super::TextMeta {
            source: source.into(),
            locator,
            snippet: "quarterly budget".into(),
        };
        assert_eq!(
            render_text_meta(&meta("transcript", 5000)),
            "  @0m05s  \"quarterly budget\""
        );
        assert_eq!(
            render_text_meta(&meta("pdf", 3)),
            "  p3  \"quarterly budget\""
        );
        assert_eq!(
            render_text_meta(&meta("caption", -1)),
            "  \"quarterly budget\"",
            "locator -1 renders no position"
        );
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

    #[test]
    fn format_ts_renders_minutes_and_seconds() {
        assert_eq!(format_ts(0), "@0m00s");
        assert_eq!(format_ts(65_000), "@1m05s");
        assert_eq!(format_ts(3_661_000), "@61m01s");
    }
}
