//! `maj index run`/`maj index status`: the derived-data queue, worked from
//! the catalog projection against the on-disk blob store. main.rs owns the
//! clap definitions; this module owns arg parsing, the `--watch` loop, and
//! rendering — the derivation engine itself (every per-kind runner, the
//! worker pool, the `text_fts` heal) lives in
//! `majestical_services::index::run`, following `search.rs`'s precedent of
//! keeping non-trivial verbs out of `commands.rs`.
use anyhow::Result;
use majestical_services::app::FsApp;
use majestical_services::index::{IndexRunOutcome, IndexRunReq, VALID_KINDS};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Args for `maj index run`, bundled to keep `cmd_index_run`'s own signature
/// within the house 5-positional-parameter limit.
pub(crate) struct IndexRunArgs {
    pub(crate) watch: bool,
    pub(crate) threads: Option<usize>,
    pub(crate) limit: Option<usize>,
    pub(crate) kinds: Option<Vec<String>>,
    pub(crate) json: bool,
}

/// Validates `--kinds`, defaulting to every kind when omitted.
fn parse_kinds(kinds: Option<&[String]>) -> Result<BTreeSet<String>> {
    let Some(kinds) = kinds else {
        return Ok(VALID_KINDS.iter().map(|s| (*s).to_string()).collect());
    };
    for kind in kinds {
        anyhow::ensure!(
            VALID_KINDS.contains(&kind.as_str()),
            "unknown --kinds value '{kind}' — one of: {}",
            VALID_KINDS.join(", ")
        );
    }
    Ok(kinds.iter().cloned().collect())
}

fn failed_json(failed: &[(PathBuf, String)]) -> Vec<serde_json::Value> {
    failed
        .iter()
        .map(|(path, err)| serde_json::json!({ "path": path.display().to_string(), "error": err }))
        .collect()
}

fn run_result_json(o: &IndexRunOutcome) -> serde_json::Value {
    serde_json::json!({
        "thumbnails": { "written": o.thumbs.written, "failed": failed_json(&o.thumbs.failed) },
        "embeddings": {
            "written": o.embed.written,
            "loaded_from_blobs": o.embed.loaded,
            "failed": failed_json(&o.embed.failed),
        },
        "keyframes": {
            "videos_done": o.keyframes.videos_done,
            "keyframes_written": o.keyframes.keyframes_written,
            "keyframes_failed": o.keyframes.keyframes_failed,
            "failed": failed_json(&o.keyframes.failed),
        },
        "transcripts": {
            "transcribed": o.transcribe.written,
            "chunks_written": o.transcript_embed.chunks_written,
            "chunks_loaded_from_blobs": o.transcript_embed.loaded,
            "chunks_empty": o.transcript_embed.empty,
            "failed": failed_json(&o.transcript_failures()),
        },
        "ocr": {
            "images_written": o.ocr.images_written,
            "videos_done": o.ocr.videos_done,
            "keyframes_written": o.ocr.keyframes_written,
            "failed": failed_json(&o.ocr.failed),
        },
        "pdf": { "written": o.pdf.written, "failed": failed_json(&o.pdf.failed) },
        "captions": {
            "written": o.captions.written,
            "failed": failed_json(&o.captions.failed),
        },
    })
}

fn print_run_result(o: &IndexRunOutcome, json: bool) {
    if json {
        println!("{}", run_result_json(o));
    } else {
        println!(
            "thumbnails: {} written, {} failed",
            o.thumbs.written,
            o.thumbs.failed.len()
        );
        println!(
            "embeddings: {} written, {} loaded from blobs, {} failed",
            o.embed.written,
            o.embed.loaded,
            o.embed.failed.len()
        );
        println!(
            "keyframes: {} videos, {} frames embedded, {} frame failures, {} videos failed",
            o.keyframes.videos_done,
            o.keyframes.keyframes_written,
            o.keyframes.keyframes_failed,
            o.keyframes.failed.len()
        );
        println!(
            "transcripts: {} transcribed, {} chunks embedded, {} loaded from blobs, {} empty, \
             {} failed",
            o.transcribe.written,
            o.transcript_embed.chunks_written,
            o.transcript_embed.loaded,
            o.transcript_embed.empty,
            o.transcribe.failed.len() + o.transcript_embed.failed.len()
        );
        println!(
            "ocr: {} images, {} videos completed, {} keyframes, {} failed",
            o.ocr.images_written,
            o.ocr.videos_done,
            o.ocr.keyframes_written,
            o.ocr.failed.len()
        );
        println!(
            "pdf: {} written, {} failed",
            o.pdf.written,
            o.pdf.failed.len()
        );
        println!(
            "captions: {} written, {} failed",
            o.captions.written,
            o.captions.failed.len()
        );
    }
    // No path prefix here: every `IndexError` display already embeds the
    // path it failed on (the structured path is still available in the
    // `--json` branch above, for callers that want it out-of-band).
    for (_, err) in o
        .thumbs
        .failed
        .iter()
        .chain(&o.embed.failed)
        .chain(&o.keyframes.failed)
        .chain(&o.transcribe.failed)
        .chain(&o.transcript_embed.failed)
        .chain(&o.ocr.failed)
        .chain(&o.pdf.failed)
        .chain(&o.captions.failed)
    {
        eprintln!("failed: {err}");
    }
}

/// True when `--kinds` was passed explicitly and names `keyframes` — the one
/// case where a missing ffmpeg is a hard error rather than a degrade: an
/// unqualified `index run` silently reports `needs_ffmpeg` in its kind
/// status, but asking for keyframes by name and getting nothing back, with
/// no explanation, is a worse experience than failing loudly.
fn explicitly_requested_keyframes(kinds: Option<&[String]>) -> bool {
    kinds.is_some_and(|kinds| kinds.iter().any(|k| k == "keyframes"))
}

/// One `index run` pass: calls the services engine, updates the on-disk
/// failure marker `index status` reads back (state, folded into the
/// services layer — see `majestical_services::index::update_failure_report`
/// — so any head calling `index::run` keeps `index status` truthful, not
/// just this CLI), and renders the result.
///
/// # Errors
/// Returns an error if the engine fails, or the failure marker can't be
/// read/written.
fn run_once(app: &FsApp, catalog_dir: &Path, req: &IndexRunReq, json: bool) -> Result<()> {
    let outcome = majestical_services::index::run(app, catalog_dir, req)?;
    majestical_services::index::update_failure_report(
        catalog_dir,
        &outcome,
        &req.kinds,
        app.notices(),
    )?;
    print_run_result(&outcome, json);
    Ok(())
}

/// Works the derivation queue once, or repeatedly (`--watch`, a 5s poll
/// loop) so newly scanned assets get picked up without a manual re-run.
///
/// # Errors
/// Returns an error if `--kinds` names an unknown kind, if `--kinds`
/// explicitly names `keyframes` while ffmpeg is absent, or the catalog can't
/// be opened/synced.
pub(crate) fn cmd_index_run(app: &FsApp, catalog_dir: &Path, args: &IndexRunArgs) -> Result<()> {
    let kinds = parse_kinds(args.kinds.as_deref())?;
    if explicitly_requested_keyframes(args.kinds.as_deref())
        && !majestical_index::video::ffmpeg_available()
    {
        anyhow::bail!("--kinds keyframes requires ffmpeg/ffprobe on PATH (brew install ffmpeg)");
    }
    loop {
        // Rebuilt every pass (not hoisted above the loop): the describer API
        // key is read fresh each time, same as before this extraction, when
        // `run_caption_items` read it via `crate::describer_cmd::env_api_key()`
        // on every `run_once` call.
        let req = IndexRunReq {
            kinds: kinds.clone(),
            limit: args.limit,
            threads: args.threads,
            api_key: crate::describer_cmd::env_api_key(),
        };
        run_once(app, catalog_dir, &req, args.json)?;
        if !args.watch {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
    Ok(())
}

/// Prints one line per derivation kind: `done`, `pending`, `offline`,
/// `unsupported`, `needs_ffmpeg` (need ffmpeg), `needs_model` (need model).
fn print_kind_status(name: &str, status: &majestical_services::index::KindStatusRow) {
    println!(
        "{name}: {} done, {} pending, {} offline, {} unsupported, {} need ffmpeg, {} need model",
        status.done,
        status.pending,
        status.offline,
        status.unsupported,
        status.needs_ffmpeg,
        status.needs_model,
    );
}

fn kind_status_json(status: &majestical_services::index::KindStatusRow) -> serde_json::Value {
    serde_json::json!({
        "done": status.done,
        "pending": status.pending,
        "offline": status.offline,
        "unsupported": status.unsupported,
        "needs_ffmpeg": status.needs_ffmpeg,
        "needs_model": status.needs_model,
    })
}

/// Remedy lines for capability gaps, printed under the per-kind status
/// lines: each names the exact command that closes the gap. The gating
/// (whether a kind actually has anything waiting on a missing model) is
/// compute, already decided in `outcome.{transcripts,captions}_remedy`;
/// this only prints whichever remedies came back `Some`.
fn print_status_remedies(outcome: &majestical_services::index::IndexStatusOutcome) {
    if let Some(remedy) = &outcome.transcripts_remedy {
        println!("transcripts needs model: {remedy}");
    }
    if let Some(remedy) = &outcome.captions_remedy {
        println!("captions needs model: {remedy}");
    }
}

/// Per-kind failure lines from the last run's marker, e.g.
/// `pdf failed last run: 1 (broken.pdf: not a valid pdf)`.
fn print_last_run_failures(failures: &serde_json::Value) {
    let Some(failures) = failures.as_object() else {
        return;
    };
    for (kind, list) in failures {
        let Some(entries) = list.as_array() else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }
        let first = entries[0]["error"].as_str().unwrap_or("<unknown reason>");
        println!("{kind} failed last run: {} ({first})", entries.len());
    }
}

/// Reports the queue's current state per derivation kind without doing any
/// work — a diff against the blob store, same as `run`, just not executed —
/// plus the last run's per-item failures from the failure marker. Compute
/// lives in `majestical_services::index::status`; this renders its
/// [`majestical_services::index::IndexStatusOutcome`].
///
/// # Errors
/// Returns an error if the catalog can't be opened/synced or the state dir
/// can't be resolved.
pub(crate) fn cmd_index_status(app: &FsApp, catalog_dir: &Path, json: bool) -> Result<()> {
    let outcome = majestical_services::index::status(app, catalog_dir)?;
    if json {
        println!(
            "{}",
            serde_json::json!({
                "thumbs": kind_status_json(&outcome.thumbs),
                "embeddings": kind_status_json(&outcome.embeddings),
                "keyframes": kind_status_json(&outcome.keyframes),
                "transcripts": kind_status_json(&outcome.transcripts),
                "ocr": kind_status_json(&outcome.ocr),
                "pdf": kind_status_json(&outcome.pdf),
                "captions": kind_status_json(&outcome.captions),
                "failed_last_run": outcome.failed_last_run,
            })
        );
    } else {
        print_kind_status("thumbs", &outcome.thumbs);
        print_kind_status("embeddings", &outcome.embeddings);
        print_kind_status("keyframes", &outcome.keyframes);
        print_kind_status("transcripts", &outcome.transcripts);
        print_kind_status("ocr", &outcome.ocr);
        print_kind_status("pdf", &outcome.pdf);
        print_kind_status("captions", &outcome.captions);
        print_status_remedies(&outcome);
        print_last_run_failures(&outcome.failed_last_run);
    }
    Ok(())
}

/// Downloads model weights into the shared cache (`MAJ_MODEL_DIR`, or the
/// platform data dir — see [`majestical_index::model::model_dir_for`]),
/// verifying every file's sha256 before it's installed. Fetches every
/// registered model unless `only` narrows it to specific tags.
///
/// # Errors
/// Returns an error if `only` names an unknown tag, the cache directory
/// can't be resolved, or any file fails to download or verify.
pub(crate) fn cmd_model_fetch(verify: bool, only: &[String]) -> Result<()> {
    majestical_services::index::model_fetch(verify, only, &mut |line| println!("{line}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kinds_defaults_to_every_kind() {
        let kinds = parse_kinds(None).expect("default kinds");
        assert_eq!(kinds.len(), VALID_KINDS.len());
        for kind in VALID_KINDS {
            assert!(kinds.contains(*kind), "default must include {kind}");
        }
    }

    #[test]
    fn parse_kinds_rejects_an_unknown_value() {
        let err = parse_kinds(Some(&["thumbs".to_string(), "bogus".to_string()]))
            .expect_err("must reject unknown kind");
        assert!(err.to_string().contains("bogus"));
        assert!(err.to_string().contains("thumbs"));
    }
}
