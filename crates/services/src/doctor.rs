//! `maj doctor`: a diagnostic sweep of the environment and (optionally) one
//! catalog. Unlike every other verb in this crate, doctor does not take an
//! `App` — a missing catalog is one of the things it reports, not a
//! precondition it requires — so it opens the catalog itself, via
//! [`crate::app::FsApp::open`], only for the checks that need one.
//!
//! Exit-code polarity: [`doctor`] returns `Ok(outcome)` whenever the checks
//! actually ran, even if every one of them failed — findings are rows, not
//! errors. `Err` is reserved for "could not check at all", which in practice
//! is near-unreachable: every check here catches its own failures and turns
//! them into a `Fail`/`Warn` row instead of propagating.
use crate::error::ServiceError;
use crate::notices::Notices;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, serde::Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    /// What was observed, concretely ("ffmpeg 7.1 at /opt/homebrew/bin/ffmpeg").
    pub detail: String,
    /// The command or action that fixes it. Absent when `Ok`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub remedy: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct DoctorOutcome {
    pub checks: Vec<DoctorCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub notices: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct DoctorRequest {
    /// Catalog to health-check; `None` skips catalog checks with a Warn row.
    pub catalog: Option<PathBuf>,
    /// The head's reading of
    /// [`majestical_describe::config::OPENROUTER_KEY_ENV`], passed in
    /// because this crate never reads the environment. `None` = not set.
    #[serde(default)]
    pub describer_env_key: Option<String>,
}

/// `maj doctor`: runs every check below, in order, and reports the row each
/// produced. Never fails outright — see the module doc for the polarity
/// rule.
///
/// # Errors
/// In practice, never — every check here handles its own failure as a
/// `Fail`/`Warn` row rather than propagating. The `Result` exists for the
/// same reason every other verb's does: a future check that genuinely can't
/// run at all (as opposed to running and finding a problem) has somewhere to
/// put that.
pub fn doctor(req: &DoctorRequest) -> Result<DoctorOutcome, ServiceError> {
    let notices = Notices::new();
    let catalog = req.catalog.as_deref();
    let env_key = req.describer_env_key.as_deref();
    let checks = vec![
        check_ffmpeg(),
        check_imagemagick(),
        check_models(),
        check_state_dir(catalog, &notices),
        check_catalog(catalog, &notices),
        check_blob_residue(catalog, &notices),
        check_failed_items(catalog, env_key, &notices),
        check_describer(catalog, env_key, &notices),
        check_platform(),
    ];
    Ok(DoctorOutcome {
        checks,
        notices: notices.drain(),
    })
}

/// Runs `name` with `args` and treats a clean exit as `Ok`, anything else
/// (spawn failure or a nonzero exit) as `Fail` with a generic Homebrew
/// remedy naming `name` itself. Shared by [`check_ffmpeg`] and
/// [`check_imagemagick`] — the latter runs the `magick` binary but reports
/// under the `imagemagick` row name, so it overrides `name`/`remedy` on the
/// result rather than passing its own row name in here.
fn probe_binary(name: &str, args: &[&str]) -> DoctorCheck {
    match Command::new(name).args(args).output() {
        Ok(output) if output.status.success() => {
            let detail = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .map_or_else(|| format!("{name} ran with no output"), str::to_string);
            DoctorCheck {
                name: name.to_string(),
                status: CheckStatus::Ok,
                detail,
                remedy: None,
            }
        }
        Ok(output) => {
            let stderr_first_line = String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .map_or_else(String::new, str::to_string);
            DoctorCheck {
                name: name.to_string(),
                status: CheckStatus::Fail,
                detail: format!("{name} exited with {}: {stderr_first_line}", output.status),
                remedy: Some(format!("brew install {name}")),
            }
        }
        Err(err) => DoctorCheck {
            name: name.to_string(),
            status: CheckStatus::Fail,
            detail: format!("could not run {name}: {err}"),
            remedy: Some(format!("brew install {name}")),
        },
    }
}

fn check_ffmpeg() -> DoctorCheck {
    probe_binary("ffmpeg", &["-version"])
}

/// The imagemagick check runs the `magick` binary (that's what the
/// `imagemagick` formula installs on `PATH`) but reports under the row name
/// and Homebrew formula name the table above specifies — both differ from
/// the binary name, so `probe_binary`'s generic result is patched rather
/// than reused verbatim.
fn check_imagemagick() -> DoctorCheck {
    as_imagemagick_row(probe_binary("magick", &["-version"]))
}

/// Re-labels a `magick` probe result as the `imagemagick` row: the name
/// always, the remedy only on failure (an `Ok` row carries none).
fn as_imagemagick_row(mut check: DoctorCheck) -> DoctorCheck {
    "imagemagick".clone_into(&mut check.name);
    if check.status == CheckStatus::Fail {
        check.remedy = Some("brew install imagemagick".to_string());
    }
    check
}

/// Every model file [`majestical_index::model::ALL_MODELS`] resolves exists
/// on disk at its expected byte size — the same presence definition `search`
/// and `index status` use ([`majestical_index::model::model_present_for`]),
/// so this can never disagree with what indexing itself would see. Missing
/// files are named individually in `detail`; the remedy fetches exactly the
/// model tags that need it.
fn check_models() -> DoctorCheck {
    use majestical_index::model::{ALL_MODELS, model_dir_for, model_present_for};

    let mut missing_files = Vec::new();
    let mut missing_tags = Vec::new();
    for spec in ALL_MODELS {
        let dir = match model_dir_for(spec) {
            Ok(dir) => dir,
            Err(err) => {
                missing_tags.push(spec.tag);
                missing_files.push(format!("{}: cache dir unresolved ({err})", spec.tag));
                continue;
            }
        };
        if model_present_for(spec, &dir) {
            continue;
        }
        missing_tags.push(spec.tag);
        missing_files.extend(missing_model_files(spec, &dir));
    }

    // Gate on both vectors, not just `missing_files`: they're built from two
    // separate predicates (`model_dir_for`'s success and `model_present_for`'s
    // result) over the same loop, and should always agree — but if presence
    // ever gains a criterion this check's own file-by-file re-derivation
    // doesn't cover, `missing_tags` could end up non-empty while
    // `missing_files` stays empty. Checking both keeps that drift from
    // reporting `Ok`.
    if missing_files.is_empty() && missing_tags.is_empty() {
        return DoctorCheck {
            name: "models".to_string(),
            status: CheckStatus::Ok,
            detail: format!("{} model(s) installed", ALL_MODELS.len()),
            remedy: None,
        };
    }

    let only_flags: Vec<String> = missing_tags
        .iter()
        .map(|tag| format!("--only {tag}"))
        .collect();
    DoctorCheck {
        name: "models".to_string(),
        status: CheckStatus::Fail,
        detail: format!("missing model file(s): {}", missing_files.join(", ")),
        remedy: Some(format!("run `maj model fetch {}`", only_flags.join(" "))),
    }
}

/// The files of one model that are absent or the wrong size in `dir`, as
/// display paths. Mirrors `model_present_for`'s criterion (exact byte
/// length) file by file, so `detail` can name what to fetch.
fn missing_model_files(spec: &majestical_index::model::ModelSpec, dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for file in spec.files {
        let path = dir.join(file.name);
        let present = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == file.bytes);
        if !present {
            missing.push(path.display().to_string());
        }
    }
    missing
}

/// The per-machine local state dir exists and is writable — resolved via
/// [`crate::state_dir::state_dir_for`], which needs a catalog to derive its
/// key, so no catalog means `Warn` rather than a resolvable path.
fn check_state_dir(catalog: Option<&Path>, notices: &Notices) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "state_dir".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: None,
        };
    };
    let dir = match crate::state_dir::state_dir_for(catalog, notices) {
        Ok(dir) => dir,
        Err(err) => {
            return DoctorCheck {
                name: "state_dir".to_string(),
                status: CheckStatus::Fail,
                detail: format!("{err:#}"),
                remedy: Some(format!("check the state dir for {}", catalog.display())),
            };
        }
    };
    let probe = dir.join(".doctor-probe");
    match std::fs::write(&probe, b"doctor probe") {
        Ok(()) => match std::fs::remove_file(&probe) {
            Ok(()) => DoctorCheck {
                name: "state_dir".to_string(),
                status: CheckStatus::Ok,
                detail: format!("writable at {}", dir.display()),
                remedy: None,
            },
            Err(err) => DoctorCheck {
                name: "state_dir".to_string(),
                status: CheckStatus::Fail,
                detail: format!("wrote {} but could not delete it: {err}", probe.display()),
                remedy: Some(format!("check permissions on {}", dir.display())),
            },
        },
        Err(err) => DoctorCheck {
            name: "state_dir".to_string(),
            status: CheckStatus::Fail,
            detail: format!("{} not writable: {err}", dir.display()),
            remedy: Some(format!("check permissions on {}", dir.display())),
        },
    }
}

/// `App::open` succeeds and the sqlite catalog opens/syncs — the same open
/// path [`crate::volumes::volumes_list`] uses
/// ([`crate::catalog::open_catalog`]). The machine/author strings are fixed
/// placeholders: doctor never emits events, so nothing depends on their
/// identity beyond `FsApp::open` creating (idempotently) a segment
/// directory for them.
fn check_catalog(catalog: Option<&Path>, notices: &Notices) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "catalog".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: Some("pass --catalog or run `maj catalog init`".to_string()),
        };
    };
    let app = match crate::app::FsApp::open(catalog, "doctor", "doctor") {
        Ok(app) => app,
        Err(err) => {
            return DoctorCheck {
                name: "catalog".to_string(),
                status: CheckStatus::Fail,
                detail: format!("{err:#}"),
                remedy: Some("run `maj catalog init`".to_string()),
            };
        }
    };
    let result = crate::catalog::open_catalog(&app, catalog);
    for line in app.notices().drain() {
        notices.push(line);
    }
    match result {
        Ok(_) => DoctorCheck {
            name: "catalog".to_string(),
            status: CheckStatus::Ok,
            detail: format!("opens and syncs at {}", catalog.display()),
            remedy: None,
        },
        Err(err) => DoctorCheck {
            name: "catalog".to_string(),
            status: CheckStatus::Fail,
            detail: format!("{err:#}"),
            remedy: Some(format!("check catalog state at {}", catalog.display())),
        },
    }
}

/// Recursively collects every file under `root` whose name satisfies
/// `matches` — used for both halves of [`check_blob_residue`]'s scan. Must
/// walk every level, not just a fixed depth: most derivations write under
/// `<hex>/<model_tag>/`, but [`majestical_index::blob::Derivation::Thumb`]
/// writes directly at `<hex>/` with no `model_tag` subdir, so a temp file
/// stranded there would sit one level shallower than the rest. A missing or
/// unreadable directory yields no entries rather than an error: a blob store
/// or runs dir that doesn't exist yet legitimately has zero residue, and
/// this check is read-only by design (it never creates one).
fn collect_matching_files(root: &Path, matches: &dyn Fn(&str) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_matching_files(&path, matches, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(matches)
        {
            out.push(path);
        }
    }
}

/// Interrupted-write orphans: leftover temp files a crash can strand mid
/// write. The blob store writes via `.tmp-{pid}-{seq}` names renamed into
/// place ([`majestical_index::blob::BlobStore::write_atomic`]); a killed
/// process leaves the temp file behind. Legacy journal migration in
/// `crate::state_dir` does the same with `<name>.partial` files under the
/// state dir's runs directory before renaming them into place. Both are
/// read-only scans: this check never deletes what it finds. The blob store
/// root comes from [`majestical_index::blob::BlobStore::root`] rather than
/// a re-derived `<catalog>/blobs` join, so this can never drift from the
/// layout the store itself uses.
fn check_blob_residue(catalog: Option<&Path>, notices: &Notices) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "blob_residue".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: None,
        };
    };

    let blob_root = majestical_index::blob::BlobStore::new(catalog)
        .root()
        .to_path_buf();
    // `catalog_paths` isn't strictly side-effect-free — it can create the
    // runs dir and migrate legacy journals into it — but it's the resolver
    // `check_state_dir` already calls with this same catalog, and the spec
    // forbids re-deriving paths a resolver already owns, so reusing it here
    // (rather than hand-rolling the runs-dir path) is accepted.
    let runs_dir = match crate::state_dir::catalog_paths(catalog, notices) {
        Ok(paths) => paths.runs_dir,
        Err(err) => {
            return DoctorCheck {
                name: "blob_residue".to_string(),
                status: CheckStatus::Warn,
                detail: format!("could not resolve the state dir to scan: {err:#}"),
                remedy: None,
            };
        }
    };

    let mut orphans = Vec::new();
    collect_matching_files(&blob_root, &|name| name.starts_with(".tmp-"), &mut orphans);
    collect_matching_files(&runs_dir, &|name| name.ends_with(".partial"), &mut orphans);

    if orphans.is_empty() {
        return DoctorCheck {
            name: "blob_residue".to_string(),
            status: CheckStatus::Ok,
            detail: format!(
                "no orphaned temp files under {} or {}",
                blob_root.display(),
                runs_dir.display()
            ),
            remedy: None,
        };
    }

    let samples: Vec<String> = orphans
        .iter()
        .take(3)
        .map(|p| p.display().to_string())
        .collect();
    DoctorCheck {
        name: "blob_residue".to_string(),
        status: CheckStatus::Warn,
        detail: format!(
            "{} orphaned temp file(s), e.g. {}",
            orphans.len(),
            samples.join(", ")
        ),
        remedy: Some(
            "delete the leftover temp files (safe while nothing is indexing or syncing)"
                .to_string(),
        ),
    }
}

/// The failure ledger — every item the planner is holding back because it
/// failed permanently ([`crate::index::known_failures`]) — reported as a
/// `Warn` with the first few paths, never as a silent zero. `env_key` is
/// unused here: both catalog-and-key checks share one signature so
/// [`doctor`] can call them the same way.
///
/// A ledger that can't be read at all (an unresolvable state dir) is a
/// `Fail` carrying the error verbatim and no remedy — doctor does not
/// invent a fix for a condition it can't name.
fn check_failed_items(
    catalog: Option<&Path>,
    _env_key: Option<&str>,
    notices: &Notices,
) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "failed_items".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: None,
        };
    };
    let ledger = match crate::index::known_failures(catalog, notices) {
        Ok(ledger) => ledger,
        Err(err) => {
            return DoctorCheck {
                name: "failed_items".to_string(),
                status: CheckStatus::Fail,
                detail: format!("{err:#}"),
                remedy: None,
            };
        }
    };

    // Flattened across kinds: the row is about how many items the catalog is
    // holding back, not about which kind holds them.
    let rows: Vec<&crate::index::LedgerRow> = ledger.values().flatten().collect();
    if rows.is_empty() {
        return DoctorCheck {
            name: "failed_items".to_string(),
            status: CheckStatus::Ok,
            detail: "no known failures".to_string(),
            remedy: None,
        };
    }
    let samples: Vec<&str> = rows.iter().take(3).map(|row| row.path.as_str()).collect();
    DoctorCheck {
        name: "failed_items".to_string(),
        status: CheckStatus::Warn,
        detail: format!(
            "{} item(s) skipped after failing permanently: {}",
            rows.len(),
            samples.join(", ")
        ),
        remedy: Some(
            "maj index run --retry-failed (or Retry failed items in Settings → Always-on)"
                .to_string(),
        ),
    }
}

/// The configured describer, and — for `OpenRouter` — whether a key will
/// actually be available when captions run. No describer at all is `Ok`:
/// captions are optional, and "off" is a legitimate configuration, not a
/// fault. An unreadable or unparsable `describer.toml` is a `Warn`: nothing
/// is broken until captions are attempted, and the file is rewritable.
fn check_describer(
    catalog: Option<&Path>,
    env_key: Option<&str>,
    notices: &Notices,
) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "describer".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: None,
        };
    };
    match crate::describer_config::load_config(catalog, notices) {
        Ok(None) => DoctorCheck {
            name: "describer".to_string(),
            status: CheckStatus::Ok,
            detail: "no describer configured — captions off".to_string(),
            remedy: None,
        },
        Ok(Some(config)) => describer_config_row(&config, env_key),
        Err(err) => DoctorCheck {
            name: "describer".to_string(),
            status: CheckStatus::Warn,
            detail: format!("describer config unreadable: {err:#}"),
            remedy: Some(
                "fix or remove describer.toml (`maj describer set …` rewrites it)".to_string(),
            ),
        },
    }
}

/// The row a stored describer config produces. The local backends need
/// nothing beyond an endpoint, so they are always `Ok`; `OpenRouter` is
/// `Ok` only when [`majestical_describe::DescriberConfig::effective_api_key`]
/// — the same resolution the caption runner performs, so this can't report
/// a key the run wouldn't find — yields one.
fn describer_config_row(
    config: &majestical_describe::DescriberConfig,
    env_key: Option<&str>,
) -> DoctorCheck {
    use majestical_describe::BackendKind;
    use majestical_describe::config::OPENROUTER_KEY_ENV;

    let backend = config.backend.as_str();
    let model = &config.model;
    let ok = |detail: String| DoctorCheck {
        name: "describer".to_string(),
        status: CheckStatus::Ok,
        detail,
        remedy: None,
    };
    match config.backend {
        BackendKind::Ollama | BackendKind::LmStudio => ok(format!("{backend} · {model}")),
        BackendKind::OpenRouter => {
            if config
                .effective_api_key(env_key.map(str::to_string))
                .is_some()
            {
                ok(format!("{backend} · {model} · key configured"))
            } else {
                DoctorCheck {
                    name: "describer".to_string(),
                    status: CheckStatus::Fail,
                    detail: format!(
                        "{backend} · {model} · no API key from describer.toml or \
                         {OPENROUTER_KEY_ENV}"
                    ),
                    remedy: Some(crate::capability::OPENROUTER_KEY_MISSING_REASON.to_string()),
                }
            }
        }
    }
}

/// Always `Ok` on macOS; on any other platform, `Warn`s listing every Apple
/// capability that is honestly unavailable there rather than failing —
/// absence of a macOS-only capability on a non-macOS build is expected, not
/// an error.
fn check_platform() -> DoctorCheck {
    let absent: Vec<&str> = [
        ("ocr::AVAILABLE", majestical_index::ocr::AVAILABLE),
        ("pdf::AVAILABLE", majestical_index::pdf::AVAILABLE),
    ]
    .into_iter()
    .filter_map(|(name, available)| (!available).then_some(name))
    .collect();

    if absent.is_empty() {
        DoctorCheck {
            name: "platform".to_string(),
            status: CheckStatus::Ok,
            detail: "macOS — OCR and PDF text extraction available".to_string(),
            remedy: None,
        }
    } else {
        DoctorCheck {
            name: "platform".to_string(),
            status: CheckStatus::Warn,
            detail: format!("{} — expected on this platform", absent.join(", ")),
            remedy: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::FsApp;
    use majestical_describe::config::OPENROUTER_KEY_ENV;
    use majestical_describe::{BackendKind, DescriberConfig};

    /// A freshly initialized catalog under `dir`, the arrangement every
    /// catalog-dependent check here needs.
    fn fixture_catalog(dir: &tempfile::TempDir) -> PathBuf {
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        root
    }

    /// Writes a real `describer.toml` for `root` the way `maj describer set`
    /// would — through [`crate::describer_config::config_path`], so the file
    /// lands exactly where [`check_describer`] looks for it.
    fn store_describer(root: &Path, backend: BackendKind, api_key: Option<&str>) {
        let notices = Notices::new();
        let path = crate::describer_config::config_path(root, &notices).expect("config path");
        DescriberConfig {
            backend,
            base_url: backend.default_base_url().to_string(),
            model: "test-model".to_string(),
            api_key: api_key.map(str::to_string),
        }
        .store(&path)
        .expect("store describer config");
    }

    /// Records `count` permanent thumb failures into `root`'s failure ledger
    /// through the public [`crate::index::record_failures`] path — the same
    /// one a real run writes through, so this can't pin a shape the
    /// production writer doesn't produce. Returns the paths recorded.
    fn record_permanent_failures(root: &Path, count: usize) -> Vec<String> {
        let mut outcome = crate::index::IndexRunOutcome::default();
        let mut paths = Vec::new();
        for i in 0..count {
            let path = root.join(format!("broken-{i}.jpg"));
            paths.push(path.display().to_string());
            let failure = crate::index::ItemFailure {
                asset: format!("asset{i}"),
                path,
                error: "decode failed".to_string(),
                transient: false,
            };
            // Alternating kinds, so the count the check reports is proven to
            // flatten across kinds rather than read the first kind only.
            if i % 2 == 0 {
                outcome.thumbs.failed.push(failure);
            } else {
                outcome.embed.failed.push(failure);
            }
        }
        let notices = Notices::new();
        crate::index::record_failures(root, &outcome, &notices).expect("record failures");
        paths
    }

    fn find<'a>(checks: &'a [DoctorCheck], name: &str) -> &'a DoctorCheck {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no `{name}` row in {checks:?}"))
    }

    #[test]
    fn doctor_with_no_catalog_warns_but_runs() {
        let outcome = doctor(&DoctorRequest::default()).expect("doctor must run with no catalog");
        assert_eq!(find(&outcome.checks, "catalog").status, CheckStatus::Warn);
        assert_eq!(find(&outcome.checks, "state_dir").status, CheckStatus::Warn);
        assert_eq!(
            find(&outcome.checks, "blob_residue").status,
            CheckStatus::Warn
        );
        // The row must exist; the machine may or may not actually have ffmpeg.
        let _ = find(&outcome.checks, "ffmpeg");
    }

    /// Pins the full emission sequence, not just individual rows' presence —
    /// a mutation that swaps two checks' order (e.g. `ffmpeg`/`imagemagick`)
    /// would still pass every other test here, since none of them assert
    /// order.
    #[test]
    fn doctor_emits_checks_in_documented_order() {
        let outcome = doctor(&DoctorRequest::default()).expect("doctor");
        let names: Vec<&str> = outcome.checks.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "ffmpeg",
                "imagemagick",
                "models",
                "state_dir",
                "catalog",
                "blob_residue",
                "failed_items",
                "describer",
                "platform",
            ]
        );
    }

    #[test]
    fn failed_items_is_ok_on_an_empty_ledger() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        let check = check_failed_items(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "no known failures");
        assert_eq!(check.remedy, None);
    }

    #[test]
    fn failed_items_warns_with_count_and_remedy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        let paths = record_permanent_failures(&root, 2);

        let check = check_failed_items(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.detail.contains("2 item(s)"), "{}", check.detail);
        for path in &paths {
            assert!(check.detail.contains(path), "{}", check.detail);
        }
        assert_eq!(
            check.remedy.as_deref(),
            Some("maj index run --retry-failed (or Retry failed items in Settings → Always-on)")
        );
    }

    /// Only the first three paths are named, however many rows there are —
    /// the count still reports every one of them.
    #[test]
    fn failed_items_names_at_most_three_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        let paths = record_permanent_failures(&root, 5);

        let check = check_failed_items(Some(&root), None, &Notices::new());
        assert!(check.detail.contains("5 item(s)"), "{}", check.detail);
        let named = paths
            .iter()
            .filter(|p| check.detail.contains(p.as_str()))
            .count();
        assert_eq!(named, 3, "{}", check.detail);
    }

    #[test]
    fn failed_items_warns_without_a_catalog() {
        let check = check_failed_items(None, None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(check.detail, "no catalog selected");
        assert_eq!(check.remedy, None);
    }

    /// An unresolvable state dir (here, a catalog path that cannot be
    /// canonicalized) is a `Fail` with the error verbatim and no invented
    /// remedy — not a silent "no known failures".
    #[test]
    fn failed_items_fails_when_the_ledger_cannot_be_read() {
        let missing = PathBuf::from("/definitely/not/a/real/maj/catalog/path-xyz");
        let check = check_failed_items(Some(&missing), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(!check.detail.is_empty());
        assert_eq!(check.remedy, None);
    }

    #[test]
    fn describer_is_ok_when_unconfigured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        let check = check_describer(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "no describer configured — captions off");
        assert_eq!(check.remedy, None);
    }

    #[test]
    fn describer_is_ok_for_a_local_backend() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::Ollama, None);
        let check = check_describer(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "ollama · test-model");
        assert_eq!(check.remedy, None);
    }

    /// A local backend never consults the environment override — the key
    /// env var names `OpenRouter`'s host, so it must not turn an LM Studio
    /// row into a "key configured" one either.
    #[test]
    fn describer_is_ok_for_lm_studio_even_with_an_env_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::LmStudio, None);
        let check = check_describer(Some(&root), Some("sk-env"), &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "lm-studio · test-model");
    }

    #[test]
    fn describer_fails_for_openrouter_without_any_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::OpenRouter, None);
        let check = check_describer(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.remedy.as_deref(),
            Some(crate::capability::OPENROUTER_KEY_MISSING_REASON)
        );
    }

    /// The `Fail` detail names the environment variable so the reader knows
    /// the second place a key can come from.
    #[test]
    fn describer_fail_detail_names_the_env_var() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::OpenRouter, None);
        let check = check_describer(Some(&root), None, &Notices::new());
        assert!(
            check.detail.contains(OPENROUTER_KEY_ENV),
            "{}",
            check.detail
        );
    }

    #[test]
    fn describer_is_ok_for_openrouter_with_only_the_env_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::OpenRouter, None);
        let check = check_describer(Some(&root), Some("sk-env"), &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "open-router · test-model · key configured");
        assert_eq!(check.remedy, None);
    }

    #[test]
    fn describer_is_ok_for_openrouter_with_only_the_file_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::OpenRouter, Some("sk-file"));
        let check = check_describer(Some(&root), None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.detail, "open-router · test-model · key configured");
    }

    #[test]
    fn describer_warns_without_a_catalog() {
        let check = check_describer(None, None, &Notices::new());
        assert_eq!(check.status, CheckStatus::Warn);
        assert_eq!(check.detail, "no catalog selected");
    }

    #[test]
    fn describer_warns_on_an_unreadable_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        let notices = Notices::new();
        let path = crate::describer_config::config_path(&root, &notices).expect("config path");
        std::fs::write(&path, b"this is not = valid toml [[[").expect("plant garbage config");

        let check = check_describer(Some(&root), None, &notices);
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(
            check.detail.starts_with("describer config unreadable:"),
            "{}",
            check.detail
        );
        assert_eq!(
            check.remedy.as_deref(),
            Some("fix or remove describer.toml (`maj describer set …` rewrites it)")
        );
    }

    /// The request's env key reaches the describer row: the same catalog
    /// reports `Fail` without it and `Ok` with it, through `doctor` itself
    /// rather than the check function.
    #[test]
    fn doctor_passes_the_request_env_key_to_the_describer_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = fixture_catalog(&dir);
        store_describer(&root, BackendKind::OpenRouter, None);

        let without = doctor(&DoctorRequest {
            catalog: Some(root.clone()),
            describer_env_key: None,
        })
        .expect("doctor");
        assert_eq!(find(&without.checks, "describer").status, CheckStatus::Fail);

        let with = doctor(&DoctorRequest {
            catalog: Some(root),
            describer_env_key: Some("sk-env".to_string()),
        })
        .expect("doctor");
        assert_eq!(find(&with.checks, "describer").status, CheckStatus::Ok);
    }

    #[test]
    fn doctor_with_real_catalog_reports_ok_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let req = DoctorRequest {
            catalog: Some(root),
            describer_env_key: None,
        };
        let outcome = doctor(&req).expect("doctor");
        assert_eq!(find(&outcome.checks, "catalog").status, CheckStatus::Ok);
    }

    #[test]
    fn doctor_with_missing_catalog_path_fails_catalog_row() {
        let req = DoctorRequest {
            catalog: Some(PathBuf::from("/definitely/not/a/real/maj/catalog/path-xyz")),
            describer_env_key: None,
        };
        let outcome =
            doctor(&req).expect("a bad catalog path is a row, not an Err — polarity doctrine");
        let catalog_row = find(&outcome.checks, "catalog");
        assert_eq!(catalog_row.status, CheckStatus::Fail);
        assert!(
            catalog_row.remedy.is_some(),
            "a Fail row must carry a remedy"
        );
    }

    #[test]
    fn blob_residue_clean_catalog_is_ok() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        std::fs::create_dir_all(root.join("blobs")).expect("mkdir blobs");
        let req = DoctorRequest {
            catalog: Some(root),
            describer_env_key: None,
        };
        let outcome = doctor(&req).expect("doctor");
        assert_eq!(
            find(&outcome.checks, "blob_residue").status,
            CheckStatus::Ok
        );
    }

    #[test]
    fn blob_residue_counts_orphaned_tmp_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let blobs = root.join("blobs").join("ab").join("abc123");
        std::fs::create_dir_all(&blobs).expect("mkdir blobs asset dir");
        std::fs::write(blobs.join(".tmp-1234-0"), b"orphaned").expect("plant orphan");
        let req = DoctorRequest {
            catalog: Some(root),
            describer_env_key: None,
        };
        let outcome = doctor(&req).expect("doctor");
        let row = find(&outcome.checks, "blob_residue");
        assert_eq!(row.status, CheckStatus::Warn);
        assert!(
            row.detail.contains("1 orphaned") && row.detail.contains(".tmp-1234-0"),
            "detail must name the count and the orphaned file: {}",
            row.detail
        );
        assert!(row.remedy.is_some());
    }

    #[test]
    fn probe_binary_missing_names_remedy() {
        let check = probe_binary("definitely-not-a-real-binary-xyz", &["-version"]);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.remedy.is_some());
    }

    #[test]
    fn check_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(CheckStatus::Ok).expect("serialize"),
            serde_json::json!("ok")
        );
        assert_eq!(
            serde_json::to_value(CheckStatus::Warn).expect("serialize"),
            serde_json::json!("warn")
        );
        assert_eq!(
            serde_json::to_value(CheckStatus::Fail).expect("serialize"),
            serde_json::json!("fail")
        );
    }

    /// `true`/`false` ship with every Unix; they pin the success-status
    /// branch of the probe without depending on ffmpeg being installed.
    #[test]
    fn probe_binary_reports_ok_on_zero_exit_and_fail_otherwise() {
        let ok = probe_binary("true", &[]);
        assert_eq!(ok.status, CheckStatus::Ok);
        assert_eq!(ok.detail, "true ran with no output");
        assert_eq!(ok.remedy, None);

        let fail = probe_binary("false", &[]);
        assert_eq!(fail.status, CheckStatus::Fail);
        assert!(
            fail.detail.starts_with("false exited with"),
            "{}",
            fail.detail
        );
        assert_eq!(fail.remedy.as_deref(), Some("brew install false"));
    }

    #[test]
    fn probe_binary_reports_fail_when_the_binary_is_absent() {
        let check = probe_binary("majestical-no-such-binary-2026", &[]);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(
            check.detail.starts_with("could not run"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn imagemagick_row_is_renamed_and_only_a_failure_gets_the_remedy() {
        let ok = as_imagemagick_row(DoctorCheck {
            name: "magick".to_string(),
            status: CheckStatus::Ok,
            detail: "Version: ImageMagick 7".to_string(),
            remedy: None,
        });
        assert_eq!(ok.name, "imagemagick");
        assert_eq!(ok.remedy, None);

        let fail = as_imagemagick_row(DoctorCheck {
            name: "magick".to_string(),
            status: CheckStatus::Fail,
            detail: "could not run magick".to_string(),
            remedy: Some("brew install magick".to_string()),
        });
        assert_eq!(fail.name, "imagemagick");
        assert_eq!(fail.remedy.as_deref(), Some("brew install imagemagick"));
    }

    /// A file of the wrong length counts as missing, exactly as
    /// `model_present_for` would judge it; a right-length file does not.
    #[test]
    fn missing_model_files_uses_exact_byte_length() {
        use majestical_index::model::ALL_MODELS;
        let spec = ALL_MODELS.first().expect("at least one model spec");
        let file = spec.files.first().expect("at least one file");
        let dir = tempfile::tempdir().expect("tempdir");

        let absent = missing_model_files(spec, dir.path());
        assert_eq!(absent.len(), spec.files.len(), "{absent:?}");

        std::fs::write(dir.path().join(file.name), vec![0u8; 1]).expect("write short file");
        let short = missing_model_files(spec, dir.path());
        assert!(
            short.iter().any(|p| p.ends_with(file.name)),
            "wrong-length file must be reported: {short:?}"
        );

        let len = usize::try_from(file.bytes).expect("fixture size fits usize");
        std::fs::write(dir.path().join(file.name), vec![0u8; len]).expect("write full file");
        let exact = missing_model_files(spec, dir.path());
        assert!(
            !exact.iter().any(|p| p.ends_with(file.name)),
            "right-length file must not be reported: {exact:?}"
        );
    }

    /// On macOS both OCR and PDF extraction are compiled in, so the platform
    /// row is `Ok`; this pins the availability polarity. Other platforms
    /// take the `Warn` branch, which this suite does not run.
    #[cfg(target_os = "macos")]
    #[test]
    fn platform_row_is_ok_on_macos() {
        let check = check_platform();
        assert_eq!(check.status, CheckStatus::Ok);
        assert_eq!(check.remedy, None);
    }
}
