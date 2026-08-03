//! `maj search`/`maj searches`: the query-language search command and its
//! saved-search management. The compute (query parse, layered retrieval,
//! ranking, notice assembly) lives in `majestical_services::search`; this
//! module builds requests from CLI args and renders the returned outcome.
use crate::SearchesCmd;
use anyhow::Result;
use majestical_services::app::FsApp;
use majestical_services::search::{SavedSearch, SearchHit, SearchOutcome, SearchRequest};
use std::fmt::Write as _;
use std::path::Path;

/// Args for `maj search`, bundled to keep `cmd_search`'s own signature
/// within the house 5-positional-parameter limit.
pub(crate) struct SearchArgs {
    pub(crate) query: Option<String>,
    pub(crate) limit: usize,
    pub(crate) json: bool,
    pub(crate) save: Option<String>,
    pub(crate) saved: Option<String>,
}

/// Runs `args` against the catalog via `majestical_services::search::search`
/// and renders the result exactly as before the services extraction.
///
/// # Errors
/// Returns an error if the query fails to parse, names an unknown or
/// malformed filter, carries neither terms nor filters, or `--save` names
/// an empty string.
pub(crate) fn cmd_search(app: &mut FsApp, catalog_dir: &Path, args: &SearchArgs) -> Result<()> {
    let req = SearchRequest {
        query: args.query.clone(),
        limit: args.limit,
        saved: args.saved.clone(),
        save: args.save.clone(),
    };
    let outcome = majestical_services::search::search(app, catalog_dir, &req)?;
    print_search_results(&outcome, args.json, args.limit);
    if let Some(name) = &args.save {
        eprintln!("saved search '{name}'");
    }
    Ok(())
}

/// Renders ranked results: JSON prints one object per hit with its volumes
/// (online/offline per the currently mounted set), tags, (for a semantic
/// keyframe hit) `timestamp_ms`, and (for a text hit) `source`/`locator`/
/// `snippet`; text prints one line per hit (`{asset} {name}  [label●|○,...]`,
/// `tags:`, `@MmSSs`, and the text hit's locator + quoted snippet appended
/// when present) followed by a `"{n} results"` summary line, a truncation
/// hint when the result count hit `limit` exactly, and — when a layer ran
/// but hasn't indexed every eligible asset yet — its coverage notice.
fn print_search_results(outcome: &SearchOutcome, json: bool, limit: usize) {
    if json {
        print_search_results_json(outcome);
    } else {
        print_search_results_text(outcome, limit);
    }
}

fn print_search_results_json(outcome: &SearchOutcome) {
    let results: Vec<_> = outcome
        .results
        .iter()
        .map(|hit| {
            let volumes: Vec<_> = hit
                .volumes
                .iter()
                .map(|v| serde_json::json!({ "id": v.id, "label": v.label, "online": v.online }))
                .collect();
            let mut result = serde_json::json!({
                "asset": hit.asset,
                "score": hit.score,
                "name": hit.name,
                "volumes": volumes,
                "tags": hit.tags,
                "para": hit.para,
            });
            if let Some(ts) = hit.timestamp_ms {
                result["timestamp_ms"] = serde_json::json!(ts);
            }
            if let Some(source) = &hit.source {
                result["source"] = serde_json::json!(source);
                result["locator"] = serde_json::json!(hit.locator);
                result["snippet"] = serde_json::json!(hit.snippet);
            }
            result
        })
        .collect();
    let mut payload = serde_json::json!({ "count": outcome.count, "results": results });
    if let Some((embedded, eligible)) = outcome.semantic_coverage {
        payload["semantic_coverage"] =
            serde_json::json!({ "embedded": embedded, "eligible": eligible });
    }
    if !outcome.text_coverage.is_empty() {
        let notices: Vec<_> = outcome
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

fn print_search_results_text(outcome: &SearchOutcome, limit: usize) {
    for hit in &outcome.results {
        if !hit.known {
            println!("{}", hit.asset);
            continue;
        }
        let volumes: Vec<String> = hit
            .volumes
            .iter()
            .map(|v| {
                let dot = if v.online { '\u{25cf}' } else { '\u{25cb}' };
                format!("{}{dot}", v.label)
            })
            .collect();
        print!("{}  {}  [{}]", hit.asset, hit.name, volumes.join(","));
        if !hit.tags.is_empty() {
            print!("  tags:{}", hit.tags.join(","));
        }
        if let Some(ts) = hit.timestamp_ms {
            print!("  {}", format_ts(ts));
        }
        if let Some(meta) = text_meta_of(hit) {
            print!("{}", render_text_meta(&meta));
        }
        println!();
    }
    println!("{} results", outcome.count);
    // A result count exactly at `limit` almost always means more matches
    // exist past it — say so, rather than letting a truncated list look
    // like the complete answer.
    if outcome.count == limit {
        println!("note: results truncated at {limit}; raise --limit to see more");
    }
    if let Some((embedded, eligible)) = outcome.semantic_coverage
        && embedded < eligible
    {
        println!("semantic index: {embedded} of {eligible} eligible assets");
    }
    for notice in &outcome.text_coverage {
        println!(
            "{}: {} of {} {} — {}",
            notice.label, notice.covered, notice.eligible, notice.noun, notice.remedy
        );
    }
}

/// A hit's text detail (source, locator, snippet), rendering-side only —
/// present exactly when the compute side set `source`.
struct TextMeta<'a> {
    source: &'a str,
    locator: i64,
    snippet: &'a str,
}

fn text_meta_of(hit: &SearchHit) -> Option<TextMeta<'_>> {
    let source = hit.source.as_deref()?;
    Some(TextMeta {
        source,
        locator: hit.locator.unwrap_or(-1),
        snippet: hit.snippet.as_deref().unwrap_or(""),
    })
}

/// Renders one hit's text detail: locator (` @MmSSs` for a ms timestamp,
/// ` p<page>` for a PDF page, nothing for locator -1) followed by the
/// quoted snippet.
fn render_text_meta(meta: &TextMeta<'_>) -> String {
    let mut out = String::new();
    if meta.source == "pdf" {
        let _ = write!(out, "  p{}", meta.locator);
    } else if meta.locator >= 0 {
        let _ = write!(out, "  {}", format_ts(meta.locator));
    }
    let _ = write!(out, "  \"{}\"", meta.snippet);
    out
}

/// Formats a millisecond timestamp (keyframe or transcript/OCR locator) as
/// `@MmSSs` (e.g. `@1m05s`), text-mode only.
fn format_ts(ts_ms: i64) -> String {
    let total_secs = ts_ms.max(0) / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("@{minutes}m{seconds:02}s")
}

/// `maj searches list`/`maj searches rm`: manage saved searches directly,
/// without running one.
///
/// # Errors
/// Returns an error if `Rm` names an empty string or a search that doesn't
/// exist, or the event log can't be read or appended to.
pub(crate) fn cmd_searches(app: &mut FsApp, cmd: SearchesCmd) -> Result<()> {
    match cmd {
        SearchesCmd::List { json } => {
            let saved = majestical_services::search::searches_list(app)?;
            print_saved_searches(&saved, json);
            Ok(())
        }
        SearchesCmd::Rm { name } => {
            majestical_services::search::searches_rm(app, &name)?;
            println!("removed saved search '{name}'");
            Ok(())
        }
    }
}

fn print_saved_searches(saved: &[SavedSearch], json: bool) {
    if json {
        let items: Vec<_> = saved
            .iter()
            .map(|s| serde_json::json!({ "name": s.name, "query": s.query }))
            .collect();
        println!("{}", serde_json::json!({ "saved": items }));
    } else if saved.is_empty() {
        println!("no saved searches");
    } else {
        for s in saved {
            println!("{}: {}", s.name, s.query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TextMeta, format_ts, render_text_meta};

    #[test]
    fn format_ts_renders_minutes_and_seconds() {
        assert_eq!(format_ts(0), "@0m00s");
        assert_eq!(format_ts(65_000), "@1m05s");
        assert_eq!(format_ts(3_661_000), "@61m01s");
    }

    #[test]
    fn render_text_meta_formats_each_locator_kind() {
        fn meta(source: &str, locator: i64) -> TextMeta<'_> {
            TextMeta {
                source,
                locator,
                snippet: "quarterly budget",
            }
        }
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
}
