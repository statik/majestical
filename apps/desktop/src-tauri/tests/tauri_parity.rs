//! The GUI analogue of `crates/cli/tests/services_parity.rs`: a command's
//! serialized outcome must be the same JSON the other heads render from.
//!
//! Two kinds of check, because the command layer can drift in two
//! directions. The first compares a command impl's serialized outcome
//! against a direct `majestical_services` call — both wrap the SAME
//! function, so what this pins is that the command layer adds, renames, or
//! loses nothing. The rest spawn the real `maj` binary: the only
//! cross-binary proof that the GUI and the CLI describe the same data the
//! same way.
//!
//! How much of the CLI's JSON is the contract depends on the verb. `maj
//! search --json` is hand-rendered (it predates the services extraction, and
//! its rendering WAS the wire contract), so only row content is compared
//! there. Every phase 7D read verb prints its outcome struct as-is — the
//! same struct the command returns — so those rows compare the whole
//! document.
//!
//! The cross-binary tests need a built `maj`: `just gui-test` builds one and
//! points `MAJ_BIN` at it. Without it they skip loudly rather than failing,
//! the same rule `services_parity.rs` follows for `/tmp/maj-ref`.
use majestical_desktop::commands::{CatalogCfg, initialize_catalog_impl, search_assets_impl};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// `#[cfg(test)]` on the helpers below is not redundant despite this file
// already building with `--cfg test`: clippy's in-test detection for
// `allow-expect-in-tests` keys off `#[test]`/`#[cfg(test)]` directly on the
// item — the full rationale lives in `crates/cli/tests/common/mod.rs`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const SEEDED_ASSET: &str = "xxh3:0123456789abcdef0123456789abcdef";
const QUERY: &str = "clip";
const LIMIT: usize = 10;

#[cfg(test)]
fn with_state_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let state = tempfile::tempdir().expect("state dir");
    // SAFETY: serialized by ENV_LOCK; no other thread reads env mid-test.
    unsafe { std::env::set_var("MAJ_STATE_DIR", state.path()) };
    let out = f();
    drop(state);
    out
}

/// Same seeding as `tests/commands.rs` — one asset on one volume, from the
/// same `Op` literals the services tests use.
#[cfg(test)]
fn seeded_cfg(catalog: PathBuf) -> CatalogCfg {
    let cfg = CatalogCfg {
        catalog,
        machine_id: "gui-test".into(),
        author: "gui-test".into(),
    };
    initialize_catalog_impl(&cfg).expect("init");
    let mut app = majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
        .expect("open");
    app.emit(vec![
        majestical_core::event::Op::VolumeSeen {
            volume: "vol1".into(),
            label: "vol1".into(),
        },
        majestical_core::event::Op::AssetSeen {
            asset: majestical_core::event::AssetId(SEEDED_ASSET.into()),
            volume: "vol1".into(),
            path: "clip.txt".into(),
            size: 5,
            mtime_ms: 1000,
        },
    ])
    .expect("emit");
    cfg
}

/// The command's outcome and a direct services call's outcome, serialized,
/// against two identically-seeded catalogs — two catalogs rather than one
/// because each call drains the notices it collected, so running both
/// against the same catalog would compare a first look at a log with a
/// second one.
#[test]
fn command_serializes_outcome_verbatim() {
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let via_command = seeded_cfg(dir.path().join("a"));
        let direct = seeded_cfg(dir.path().join("b"));

        let from_command = search_assets_impl(&via_command, Some(QUERY.into()), None, Some(LIMIT))
            .expect("command");
        let from_service = majestical_services::runtime::run_off_tokio_runtime(|| {
            let mut app = majestical_services::app::FsApp::open(
                &direct.catalog,
                &direct.machine_id,
                &direct.author,
            )?;
            Ok(majestical_services::search::search(
                &mut app,
                &direct.catalog,
                &majestical_services::search::SearchRequest {
                    query: Some(QUERY.into()),
                    limit: LIMIT,
                    saved: None,
                    save: None,
                },
            )?)
        })
        .expect("service");

        assert_eq!(
            serde_json::to_value(&from_command).expect("serialize command outcome"),
            serde_json::to_value(&from_service).expect("serialize service outcome"),
            "the command layer must add, rename, and lose nothing"
        );
    });
}

/// Every field `maj search --json` prints per row, compared against the
/// command's row for the same query on the same catalog.
#[test]
fn search_rows_match_cli_json() {
    let Some(maj) = maj_or_skip("search rows vs `maj search --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome =
            search_assets_impl(&cfg, Some(QUERY.into()), None, Some(LIMIT)).expect("command");
        let cli = cli_search_json(&maj, &cfg);

        assert_eq!(
            cli["count"],
            serde_json::json!(outcome.count),
            "count must agree: {cli}"
        );
        let rows = cli["results"].as_array().expect("results array");
        assert_eq!(rows.len(), outcome.results.len(), "row count: {cli}");
        for (row, hit) in rows.iter().zip(&outcome.results) {
            assert_eq!(row["asset"], serde_json::json!(hit.asset));
            assert_eq!(row["score"], serde_json::json!(hit.score));
            assert_eq!(row["name"], serde_json::json!(hit.name));
            assert_eq!(row["tags"], serde_json::json!(hit.tags));
            assert_eq!(row["para"], serde_json::json!(hit.para));
        }
    });
}

/// The only route from this suite to a `maj` child process.
///
/// [`Maj`] holds the binary's path in a field private to THIS module and
/// exposes no way to read it, so a spawn site outside the module cannot
/// build a `std::process::Command` of its own: every child necessarily goes
/// through [`Maj::run`], which applies both protections and then checks
/// them on the built command.
///
/// The check is not a statement that can be dropped: [`guarded`] CONSUMES
/// the `Command` and hands back a [`Guarded`], and only a [`Guarded`] can
/// be spawned. Deleting the call leaves a bare `Command`, whose `output`
/// returns `io::Result<Output>` where this returns `Output` — a type error,
/// the same way the private path field makes a bypassing spawn site a
/// privacy error.
///
/// What that does NOT cover, stated so nobody reads more into it: an edit
/// INSIDE this module can still drop the guard and re-unwrap
/// (`command.output().unwrap_or_else(..)`), because `Command::output` is
/// inherent and the binding is in scope. Types cannot close that — any
/// expression yielding a `Command` can spawn one — so what covers this
/// module is its size and the three `should_panic` tests below. The type
/// and privacy walls are what protect the SIX spawn sites outside it, which
/// is where a second unguarded site would realistically appear.
///
/// Checking every spawn is the point. Asserting on a command built
/// specially for a test proves only that one construction. The CLI's
/// `keychain_guard.rs` scans source text instead and is weaker twice over —
/// one `.env(` occurrence satisfies it for a whole file, and it only scans
/// `crates/cli/tests`, so since Task 7 it says nothing about this head.
mod guarded {
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::process::{Command, Output};

    /// A located `maj` binary. Construct with [`Maj::find`], run with
    /// [`Maj::run`]; there is deliberately no accessor for the path.
    pub(super) struct Maj(PathBuf);

    impl Maj {
        /// `MAJ_BIN`, else the workspace's own debug build (this test binary
        /// runs with the package directory as its working directory).
        /// `None` when neither exists.
        #[cfg(test)]
        pub(super) fn find() -> Option<Self> {
            let path = std::env::var_os("MAJ_BIN")
                .map_or_else(|| PathBuf::from("../../../target/debug/maj"), PathBuf::from);
            path.is_file().then_some(Self(path))
        }

        /// Runs `maj <args>` to completion under a throwaway Keychain
        /// service and with `MAJ_OPENROUTER_KEY` removed, so no parity row
        /// can read, write or delete the item the developer's own `maj`
        /// keeps under the default name, and no ambient key can decide a
        /// result. `MAJ_STATE_DIR` is inherited — [`super::with_state_dir`]
        /// has already pointed it at the calling test's tempdir.
        #[cfg(test)]
        pub(super) fn run(&self, args: &[&OsStr]) -> Output {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let mut command = Command::new(&self.0);
            command
                .args(args)
                .env(
                    majestical_secrets::SERVICE_ENV,
                    format!(
                        "majestical-test-{}-{}",
                        std::process::id(),
                        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    ),
                )
                .env_remove(majestical_describe::config::OPENROUTER_KEY_ENV);
            guarded(command).output(args)
        }
    }

    /// A command that has passed [`guarded`]. The only thing in this file
    /// that can spawn, and [`guarded`] is its only constructor.
    #[cfg(test)]
    pub(super) struct Guarded(Command);

    impl Guarded {
        /// Spawns the child and waits for it. `args` is for the panic
        /// message only — the command already carries them.
        #[cfg(test)]
        fn output(mut self, args: &[&OsStr]) -> Output {
            self.0
                .output()
                .unwrap_or_else(|err| panic!("run maj {args:?}: {err}"))
        }
    }

    /// Refuses to let a child be spawned unless it carries both
    /// protections. Reads `get_envs`, which reports what the child would
    /// actually receive — a set variable as `Some`, an explicitly removed
    /// one as `None` — rather than what the source says was asked for.
    #[cfg(test)]
    fn guarded(command: Command) -> Guarded {
        let envs: std::collections::HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        let service = envs
            .get(OsStr::new(majestical_secrets::SERVICE_ENV))
            .copied()
            .flatten()
            .expect("a maj child must be given MAJ_KEYCHAIN_SERVICE")
            .to_str()
            .expect("a UTF-8 Keychain service name");
        assert!(
            service.starts_with("majestical-test-"),
            "a maj child must never address the developer's own Keychain item: {service}"
        );
        assert_eq!(
            envs.get(OsStr::new(majestical_describe::config::OPENROUTER_KEY_ENV)),
            Some(&None),
            "a maj child must have MAJ_OPENROUTER_KEY explicitly removed, so no ambient key \
             can decide a parity result"
        );
        Guarded(command)
    }

    /// What [`guarded`] refuses — the tests that fail if the guard itself is
    /// deleted or weakened. None of these spawns anything: each builds a
    /// command and hands it to the guard, and the [`Guarded`] it would
    /// return is never run.
    #[cfg(test)]
    mod tests {
        use super::{Command, guarded};

        #[test]
        #[should_panic(expected = "must be given MAJ_KEYCHAIN_SERVICE")]
        fn a_child_with_no_service_override_is_refused() {
            let _refused = guarded(Command::new("maj"));
        }

        #[test]
        #[should_panic(expected = "never address the developer's own Keychain item")]
        fn a_child_pointed_at_the_real_service_is_refused() {
            let mut command = Command::new("maj");
            command
                .env(majestical_secrets::SERVICE_ENV, "majestical")
                .env_remove(majestical_describe::config::OPENROUTER_KEY_ENV);
            let _refused = guarded(command);
        }

        #[test]
        #[should_panic(expected = "MAJ_OPENROUTER_KEY explicitly removed")]
        fn a_child_that_would_inherit_the_ambient_key_is_refused() {
            let mut command = Command::new("maj");
            command.env(majestical_secrets::SERVICE_ENV, "majestical-test-guard");
            let _refused = guarded(command);
        }
    }
}

use guarded::Maj;

/// A located `maj`, or `None` after saying loudly in the test log which
/// check is being skipped and how to stop skipping it. `what` names the
/// comparison, e.g. "browse tree vs `maj browse tree --json`".
#[cfg(test)]
#[expect(
    clippy::print_stderr,
    reason = "a skipped parity check must say so in the test log"
)]
fn maj_or_skip(what: &str) -> Option<Maj> {
    let found = Maj::find();
    if found.is_none() {
        eprintln!(
            "SKIP parity({what}): no maj binary at MAJ_BIN or ../../../target/debug/maj — run \
             `just gui-test` to build one"
        );
    }
    found
}

#[cfg(test)]
fn cli_search_json(maj: &Maj, cfg: &CatalogCfg) -> serde_json::Value {
    let limit = LIMIT.to_string();
    let output = maj.run(&[
        OsStr::new("--catalog"),
        cfg.catalog.as_os_str(),
        OsStr::new("--machine-id"),
        OsStr::new(&cfg.machine_id),
        OsStr::new("search"),
        OsStr::new(QUERY),
        OsStr::new("--json"),
        OsStr::new("--limit"),
        OsStr::new(&limit),
    ]);
    assert!(
        output.status.success(),
        "maj search failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("maj search --json prints one JSON object")
}

/// Runs `maj doctor --catalog <catalog> --json` and parses its one JSON
/// line. Unlike [`cli_json`], this does NOT pass the top-level `--catalog`:
/// `Cmd::Doctor` has its own `--catalog` flag, independent of (and parsed in
/// a different scope from) the top-level one `require_catalog_and_machine_id`
/// resolves for every other verb — see `Cmd::Doctor`'s own doc in
/// `crates/cli/src/main.rs`. Passing the top-level flag here would set a
/// field `cmd_doctor` never reads, silently comparing against a
/// no-catalog-selected doctor run instead of `catalog`'s.
#[cfg(test)]
fn cli_doctor_json(maj: &Maj, catalog: &Path) -> serde_json::Value {
    let output = maj.run(&[
        OsStr::new("doctor"),
        OsStr::new("--catalog"),
        catalog.as_os_str(),
        OsStr::new("--json"),
    ]);
    assert!(
        output.status.success(),
        "maj doctor failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("maj doctor --json prints one JSON object")
}

/// Runs `maj <args>` against `cfg`'s catalog and parses its one JSON line.
#[cfg(test)]
fn cli_json(maj: &Maj, cfg: &CatalogCfg, args: &[&str]) -> serde_json::Value {
    let mut argv: Vec<&OsStr> = vec![
        OsStr::new("--catalog"),
        cfg.catalog.as_os_str(),
        OsStr::new("--machine-id"),
        OsStr::new(&cfg.machine_id),
    ];
    argv.extend(args.iter().map(|arg| OsStr::new(*arg)));
    let output = maj.run(&argv);
    assert!(
        output.status.success(),
        "maj {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|err| panic!("maj {args:?} must print one JSON object: {err}"))
}

// ---------------------------------------------------------------------------
// Phase 7D read verbs. Unlike `search` above — whose CLI JSON is
// hand-rendered, so only row CONTENT is the contract — every verb below
// prints `serde_json::to_string(&outcome)` of the SAME outcome struct the
// command returns (see `cli/src/commands.rs::cmd_browse_tree`'s doc for the
// as-is policy). For these the whole payload is the contract, so these rows
// compare the whole document and would catch a field the GUI drops, renames,
// or adds on its way out.
//
// The ingest commands `ingest_state` and `start_ingest` get no row here on
// purpose: neither has a CLI JSON twin to compare against. `ingest_state`
// reports the state of a run owned by THIS process's `IngestState` (a
// separate `maj` invocation has no such run and no verb that would print
// one), and `start_ingest` streams its outcome as Tauri events rather than
// returning a payload. `tests/commands.rs` covers both end to end instead.
// ---------------------------------------------------------------------------

/// `browse_tree` against `maj browse tree --json`: the whole payload,
/// including each volume's online flag and per-folder recursive counts.
#[test]
fn browse_tree_matches_cli_json() {
    let Some(maj) = maj_or_skip("browse_tree vs `maj browse tree --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome = majestical_desktop::commands::browse_tree_impl(&cfg).expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(&maj, &cfg, &["browse", "tree", "--json"]),
            "browse_tree and `maj browse tree --json` must render the same document"
        );
    });
}

/// `browse_list` against `maj browse list --json`, with every knob left at
/// its default — the defaults themselves are part of what this pins, since
/// each head applies them independently (`limit`/`offset`/`flatten`).
#[test]
fn browse_list_matches_cli_json() {
    let Some(maj) = maj_or_skip("browse_list vs `maj browse list --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let outcome = majestical_desktop::commands::browse_list_impl(
            &cfg,
            "vol1".into(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(
                &maj,
                &cfg,
                &["browse", "list", "--volume", "vol1", "--json"]
            ),
            "browse_list and `maj browse list --json` must render the same document"
        );
    });
}

/// `list_tags` against `maj tags list --json`, on a catalog carrying one
/// tag — an empty vocabulary would compare two empty arrays and prove
/// nothing about the row shape.
#[test]
fn list_tags_matches_cli_json() {
    let Some(maj) = maj_or_skip("list_tags vs `maj tags list --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let mut app =
            majestical_services::app::FsApp::open(&cfg.catalog, &cfg.machine_id, &cfg.author)
                .expect("open");
        app.emit(vec![majestical_core::event::Op::TagAdd {
            asset: majestical_core::event::AssetId(SEEDED_ASSET.into()),
            tag: "b-roll".into(),
        }])
        .expect("emit");
        drop(app);

        let outcome = majestical_desktop::commands::list_tags_impl(&cfg).expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_json(&maj, &cfg, &["tags", "list", "--json"]),
            "list_tags and `maj tags list --json` must render the same document"
        );
    });
}

/// `list_unfinished_ingests` against `maj ingest unfinished --json`. Both
/// heads read the same per-machine run journals, so this needs one on disk:
/// a `RunStarted` record promising more files than were ever placed, which
/// is what a run cancelled before its first file leaves behind.
#[test]
fn list_unfinished_ingests_matches_cli_json() {
    let Some(maj) = maj_or_skip("list_unfinished_ingests vs `maj ingest unfinished --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        let runs_dir = majestical_services::state_dir::catalog_paths(
            &cfg.catalog,
            &majestical_services::notices::Notices::new(),
        )
        .expect("state dir")
        .runs_dir;
        std::fs::write(
            runs_dir.join("01JABCDEFGHJKMNPQRSTVWXYZ0.jsonl"),
            b"{\"rec\":\"run_started\",\"run\":\"01JABCDEFGHJKMNPQRSTVWXYZ0\",\
              \"source\":\"/cards/A001\",\"dests\":[\"/media/raid\"],\"planned\":2}\n",
        )
        .expect("write journal");

        let outcome =
            majestical_desktop::ingest::list_unfinished_ingests_impl(&cfg).expect("command");
        let payload = serde_json::to_value(&outcome).expect("serialize command outcome");
        assert_eq!(
            payload["runs"][0]["run_id"],
            serde_json::json!("01JABCDEFGHJKMNPQRSTVWXYZ0"),
            "the seeded journal must be listed, or this row proves nothing: {payload}"
        );
        assert_eq!(
            payload,
            cli_json(&maj, &cfg, &["ingest", "unfinished", "--json"]),
            "list_unfinished_ingests and `maj ingest unfinished --json` must render the same \
             document"
        );
    });
}

/// `doctor_report` against `maj doctor --catalog <catalog> --json`: the
/// whole payload, including the environment rows (ffmpeg/imagemagick/models/
/// platform) that don't depend on the catalog at all — both binaries run on
/// this machine, so those rows must agree byte for byte too.
#[test]
fn doctor_matches_cli_json() {
    let Some(maj) = maj_or_skip("doctor vs `maj doctor --json`") else {
        return;
    };
    with_state_dir(|| {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = seeded_cfg(dir.path().join("cat"));
        // The head's own reading, taken the way the app takes it: from the
        // key cache the setup hook filled. No environment key and no real
        // store on either side — `Maj::run` strips both from the child —
        // so both binaries report the same absent key.
        let cache = majestical_desktop::captions::DescriberKeyCache::default();
        majestical_desktop::captions::refresh_key(
            &majestical_desktop::captions::KeyRefresh {
                cache: &cache,
                store: &majestical_secrets::MemoryKeyStore::default(),
                env: None,
            },
            Some(&cfg),
        );
        let outcome =
            majestical_desktop::commands::doctor_report_impl(Some(&cfg), cache.presence())
                .expect("command");
        assert_eq!(
            serde_json::to_value(&outcome).expect("serialize command outcome"),
            cli_doctor_json(&maj, &cfg.catalog),
            "doctor_report and `maj doctor --json` must render the same document"
        );
    });
}
