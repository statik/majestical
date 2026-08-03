//! `maj verify` compute: re-verifies a directory against its own ASC MHL
//! history and appends a new generation recording the result. Moved from
//! `crates/cli/src/commands.rs::cmd_verify`; the CLI keeps the text/`json!`
//! rendering and the pass/fail exit-code policy, fed from [`VerifyReport`].
//! Named to avoid clashing with `majestical_core::event::VerifyOutcome`.
use crate::app::physical_now_ms;
use crate::error::ServiceError;
use crate::iso8601::iso8601_ms;
use anyhow::{Context, Result};
use majestical_ingest::mhl;
use std::path::Path;

/// Everything `maj verify` renders: which relative paths matched their
/// recorded hash, which were altered or went missing, which are new since
/// the last generation, and the generation number this run wrote.
#[derive(Debug, serde::Serialize)]
pub struct VerifyReport {
    pub verified: Vec<String>,
    pub altered: Vec<String>,
    pub missing: Vec<String>,
    pub new_files: Vec<String>,
    pub generation: u32,
}

/// `maj verify`: re-verifies `dir` against its own ASC MHL history and
/// appends a new generation recording the result. Needs no catalog — the
/// history lives entirely under `dir/ascmhl`. Whether `altered`/`missing`
/// being non-empty should fail the caller's process is a rendering-layer
/// policy decision, not this function's — it always returns the report.
///
/// # Errors
/// Returns an error if `dir` has no ASC MHL history yet, or a filesystem
/// operation (hashing, reading/writing the history) fails.
pub fn verify_dir_op(dir: &Path) -> Result<VerifyReport, ServiceError> {
    verify_dir_op_impl(dir).map_err(ServiceError::from)
}

fn verify_dir_op_impl(dir: &Path) -> Result<VerifyReport> {
    let hashdate = iso8601_ms(physical_now_ms());
    let report =
        mhl::verify_dir(dir, &hashdate).with_context(|| format!("verifying {}", dir.display()))?;
    Ok(VerifyReport {
        verified: report.verified,
        altered: report.altered,
        missing: report.missing,
        new_files: report.new_files,
        generation: report.written.generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_dir_op_of_an_untampered_history_reports_no_altered_or_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.mov"), b"hello").expect("write");
        let hash_list = mhl::hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        mhl::write_generation(dir.path(), &hash_list).expect("write_generation");

        let report = verify_dir_op(dir.path()).expect("verify_dir_op");
        assert_eq!(report.verified, vec!["a.mov".to_string()]);
        assert!(report.altered.is_empty());
        assert!(report.missing.is_empty());
        assert!(report.new_files.is_empty());
        assert_eq!(report.generation, 2);
    }

    #[test]
    fn verify_dir_op_reports_an_altered_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.mov"), b"hello").expect("write");
        let hash_list = mhl::hash_dir(dir.path(), "2026-07-30T00:00:00Z").expect("hash_dir");
        mhl::write_generation(dir.path(), &hash_list).expect("write_generation");
        std::fs::write(dir.path().join("a.mov"), b"ZZZZZ").expect("tamper");

        let report = verify_dir_op(dir.path()).expect("verify_dir_op");
        assert_eq!(report.altered, vec!["a.mov".to_string()]);
    }

    #[test]
    fn verify_dir_op_with_no_history_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = verify_dir_op(dir.path()).expect_err("must fail");
        assert!(!err.to_string().is_empty());
    }
}
