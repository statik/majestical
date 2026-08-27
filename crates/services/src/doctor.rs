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
    let checks = vec![
        check_ffmpeg(),
        check_imagemagick(),
        check_models(),
        check_state_dir(catalog, &notices),
        check_catalog(catalog, &notices),
        check_blob_residue(catalog, &notices),
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
    let mut check = probe_binary("magick", &["-version"]);
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
        for file in spec.files {
            let path = dir.join(file.name);
            let present = std::fs::metadata(&path).is_ok_and(|meta| meta.len() == file.bytes);
            if !present {
                missing_files.push(path.display().to_string());
            }
        }
    }

    if missing_files.is_empty() {
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
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            DoctorCheck {
                name: "state_dir".to_string(),
                status: CheckStatus::Ok,
                detail: format!("writable at {}", dir.display()),
                remedy: None,
            }
        }
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
/// `matches` — used for both halves of [`check_blob_residue`]'s scan. A
/// missing or unreadable directory yields no entries rather than an error:
/// a blob store or runs dir that doesn't exist yet legitimately has zero
/// residue, and this check is read-only by design (it never creates one).
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
/// read-only scans: this check never deletes what it finds. `BlobStore`
/// exposes no accessor for its root path, so the blob store root is derived
/// the same way `crates/services/src/sync.rs`'s own location check already
/// does (`<catalog_root>/blobs`) rather than through `BlobStore` itself.
fn check_blob_residue(catalog: Option<&Path>, notices: &Notices) -> DoctorCheck {
    let Some(catalog) = catalog else {
        return DoctorCheck {
            name: "blob_residue".to_string(),
            status: CheckStatus::Warn,
            detail: "no catalog selected".to_string(),
            remedy: None,
        };
    };

    let blob_root = catalog.join("blobs");
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

    #[test]
    fn doctor_with_real_catalog_reports_ok_catalog() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("cat");
        FsApp::init(&root, "m1", "m1").expect("init");
        let req = DoctorRequest {
            catalog: Some(root),
        };
        let outcome = doctor(&req).expect("doctor");
        assert_eq!(find(&outcome.checks, "catalog").status, CheckStatus::Ok);
    }

    #[test]
    fn doctor_with_missing_catalog_path_fails_catalog_row() {
        let req = DoctorRequest {
            catalog: Some(PathBuf::from("/definitely/not/a/real/maj/catalog/path-xyz")),
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
        };
        let outcome = doctor(&req).expect("doctor");
        let row = find(&outcome.checks, "blob_residue");
        assert_eq!(row.status, CheckStatus::Warn);
        assert!(
            row.detail.contains('1') && row.detail.contains(".tmp-1234-0"),
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
}
