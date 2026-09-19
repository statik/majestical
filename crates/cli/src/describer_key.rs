//! The CLI and MCP heads' reading and writing of the `OpenRouter` key:
//! `MAJ_OPENROUTER_KEY`, then the Keychain, through `majestical_secrets`.
//! Services is handed the result and never looks for itself.

use std::path::Path;

use anyhow::Context as _;
use majestical_secrets::{HeadKeySource, KeyStore, ResolvedKey, SystemKeyStore};
use majestical_services::describer_config::{self, ClearKeyOutcome, FileKey, KeyPresence};
use majestical_services::notices::Notices;

/// Where this head looks for a key outside `describer.toml`. A struct so a
/// test can say what the environment held instead of mutating the process's.
/// No `Debug`: `env` is a real key.
pub(crate) struct KeySources<'a> {
    /// `MAJ_OPENROUTER_KEY`'s value, when set and not empty.
    pub(crate) env: Option<String>,
    pub(crate) store: &'a dyn KeyStore,
}

impl<'a> KeySources<'a> {
    /// This process's environment, and `store`.
    pub(crate) fn ambient(store: &'a dyn KeyStore) -> Self {
        Self {
            env: env_api_key(),
            store,
        }
    }
}

fn env_api_key() -> Option<String> {
    std::env::var(majestical_describe::config::OPENROUTER_KEY_ENV)
        .ok()
        .filter(|k| !k.is_empty())
}

/// The system store under the service name `MAJ_KEYCHAIN_SERVICE` names, or
/// the default.
pub(crate) fn system_store() -> SystemKeyStore {
    SystemKeyStore::new(std::env::var(majestical_secrets::SERVICE_ENV).ok())
}

/// Resolves the key for `catalog_root` and pushes the store's failure, if
/// any, as a notice. Reads the STORED backend, so a verb that changes the
/// backend resolves after it has written the new config.
pub(crate) fn resolve(
    catalog_root: &Path,
    sources: &KeySources<'_>,
    notices: &Notices,
) -> ResolvedKey {
    let wants = describer_config::wants_keychain(catalog_root, notices);
    let resolved = majestical_secrets::resolve(sources.env.clone(), wants, sources.store);
    if let Some(notice) = &resolved.notice {
        notices.push(notice.clone());
    }
    resolved
}

/// What doctor's describer row is told. With no catalog there is no backend
/// to ask about, so the environment alone answers and the store is never
/// touched.
pub(crate) fn presence_for(
    catalog: Option<&Path>,
    sources: &KeySources<'_>,
    notices: &Notices,
) -> KeyPresence {
    let resolved = match catalog {
        Some(catalog_root) => resolve(catalog_root, sources, notices),
        None => majestical_secrets::resolve(sources.env.clone(), false, sources.store),
    };
    presence(&resolved)
}

pub(crate) fn presence(resolved: &ResolvedKey) -> KeyPresence {
    match resolved.source {
        HeadKeySource::Env => KeyPresence::Env,
        HeadKeySource::Keychain => KeyPresence::Keychain,
        HeadKeySource::Absent => KeyPresence::Absent,
    }
}

/// Executes `describer_config::plan_key_write`: the Keychain write comes
/// FIRST and its failure stops everything, so the file and the Keychain
/// never disagree. Returns what `set` should do with the file's key.
///
/// # Errors
/// The Keychain refused the write. The message never contains the key.
pub(crate) fn store(key: Option<String>, store: &dyn KeyStore) -> anyhow::Result<FileKey> {
    let write = describer_config::plan_key_write(store.supported(), key);
    if let Some(key) = &write.keychain {
        store
            .store(key)
            .context("storing the key in the macOS Keychain")?;
    }
    Ok(write.file)
}

/// Removes the stored key from both places it can live: the Keychain first,
/// so a refusal leaves `describer.toml` as it was.
///
/// # Errors
/// The Keychain refused the delete, or `describer.toml` could not be
/// rewritten.
pub(crate) fn clear(
    catalog_root: &Path,
    sources: &KeySources<'_>,
    notices: &Notices,
) -> anyhow::Result<ClearKeyOutcome> {
    let keychain_cleared = if sources.store.supported() {
        sources
            .store
            .delete()
            .context("removing the key from the macOS Keychain")?
    } else {
        false
    };
    let file_cleared = describer_config::clear_file_key(catalog_root, notices)?;
    Ok(ClearKeyOutcome {
        keychain_cleared,
        file_cleared,
        env_still_supplies: sources.env.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use majestical_describe::BackendKind;
    use majestical_secrets::{MemoryKeyStore, SecretError};
    use majestical_services::describer_config::{KeySource, SetArgs};
    use std::sync::Mutex;

    fn holding(key: &str) -> MemoryKeyStore {
        MemoryKeyStore {
            key: Mutex::new(Some(key.to_string())),
            ..MemoryKeyStore::default()
        }
    }

    fn failing(message: &str) -> MemoryKeyStore {
        MemoryKeyStore {
            fail: Some(message.to_string()),
            ..MemoryKeyStore::default()
        }
    }

    fn held(store: &MemoryKeyStore) -> Option<String> {
        store.key.lock().expect("lock").clone()
    }

    fn no_env(store: &dyn KeyStore) -> KeySources<'_> {
        KeySources { env: None, store }
    }

    fn configure(root: &Path, backend: BackendKind, file_key: FileKey) {
        describer_config::set(
            root,
            &SetArgs {
                backend,
                model: "m".to_string(),
                base_url: None,
                file_key,
            },
            &Notices::new(),
        )
        .expect("set");
    }

    fn key_source(root: &Path) -> KeySource {
        describer_config::show(root, KeyPresence::Absent, &Notices::new())
            .expect("show")
            .expect("configured")
            .key_source
    }

    /// Proves a "never touched" claim: in production a read is a macOS prompt.
    struct PanickingStore;

    #[expect(
        clippy::panic_in_result_fn,
        reason = "the panic is the assertion: any call is the test's failure"
    )]
    impl KeyStore for PanickingStore {
        fn supported(&self) -> bool {
            true
        }

        fn read(&self) -> Result<Option<String>, SecretError> {
            panic!("the store must not be touched");
        }

        fn store(&self, _key: &str) -> Result<(), SecretError> {
            panic!("the store must not be touched");
        }

        fn delete(&self) -> Result<bool, SecretError> {
            panic!("the store must not be touched");
        }
    }

    #[test]
    fn store_puts_the_key_in_a_supported_store_and_clears_the_file_key() {
        let keychain = MemoryKeyStore::default();
        let file_key = store(Some("sk-test".to_string()), &keychain).expect("store");
        assert_eq!(file_key, FileKey::Clear);
        assert_eq!(held(&keychain).as_deref(), Some("sk-test"));
    }

    #[test]
    fn store_puts_the_key_in_the_file_when_unsupported() {
        let keychain = MemoryKeyStore {
            unsupported: true,
            ..MemoryKeyStore::default()
        };
        let file_key = store(Some("sk-test".to_string()), &keychain).expect("store");
        assert_eq!(file_key, FileKey::Set("sk-test".to_string()));
        assert_eq!(held(&keychain), None);
    }

    #[test]
    fn store_without_a_key_changes_nothing() {
        assert_eq!(store(None, &PanickingStore).expect("store"), FileKey::Keep);
        let keychain = holding("sk-test");
        assert_eq!(store(None, &keychain).expect("store"), FileKey::Keep);
        assert_eq!(held(&keychain).as_deref(), Some("sk-test"));
    }

    #[test]
    fn a_refused_keychain_write_is_an_error_without_the_key_in_it() {
        let err = store(Some("sk-test".to_string()), &failing("denied")).expect_err("refused");
        let rendered = format!("{err} {err:#} {err:?}");
        assert!(!rendered.contains("sk-test"), "{rendered}");
        assert!(rendered.contains("Keychain"), "{rendered}");
        assert!(rendered.contains("denied"), "{rendered}");
    }

    #[test]
    fn resolve_reads_the_store_only_for_openrouter() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::Ollama, FileKey::Keep);
        let notices = Notices::new();
        let resolved = resolve(dir.path(), &no_env(&PanickingStore), &notices);
        assert_eq!(resolved.source, HeadKeySource::Absent);
        assert_eq!(notices.drain(), Vec::<String>::new());

        configure(dir.path(), BackendKind::OpenRouter, FileKey::Keep);
        let resolved = resolve(dir.path(), &no_env(&holding("sk-test")), &notices);
        assert_eq!(resolved.source, HeadKeySource::Keychain);
        assert_eq!(resolved.key.as_deref(), Some("sk-test"));
        assert_eq!(presence(&resolved), KeyPresence::Keychain);
        assert_eq!(notices.drain(), Vec::<String>::new());
    }

    #[test]
    fn resolve_without_a_config_never_touches_the_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let resolved = resolve(dir.path(), &no_env(&PanickingStore), &Notices::new());
        assert_eq!(resolved.source, HeadKeySource::Absent);
    }

    #[test]
    fn resolve_prefers_the_env_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, FileKey::Keep);
        let sources = KeySources {
            env: Some("sk-test".to_string()),
            store: &PanickingStore,
        };
        let resolved = resolve(dir.path(), &sources, &Notices::new());
        assert_eq!(resolved.source, HeadKeySource::Env);
        assert_eq!(resolved.key.as_deref(), Some("sk-test"));
        assert_eq!(presence(&resolved), KeyPresence::Env);
    }

    #[test]
    fn a_store_failure_becomes_a_notice_and_the_run_carries_on() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, FileKey::Keep);
        let notices = Notices::new();
        let resolved = resolve(dir.path(), &no_env(&failing("denied")), &notices);
        assert_eq!(resolved.source, HeadKeySource::Absent);
        assert_eq!(presence(&resolved), KeyPresence::Absent);
        let notices = notices.drain();
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].contains("denied"), "{notices:?}");
        assert!(notices[0].contains("MAJ_OPENROUTER_KEY"), "{notices:?}");
    }

    #[test]
    fn doctor_without_a_catalog_asks_the_environment_alone() {
        let notices = Notices::new();
        assert_eq!(
            presence_for(None, &no_env(&PanickingStore), &notices),
            KeyPresence::Absent
        );
        let sources = KeySources {
            env: Some("sk-test".to_string()),
            store: &PanickingStore,
        };
        assert_eq!(presence_for(None, &sources, &notices), KeyPresence::Env);
    }

    #[test]
    fn doctor_with_a_catalog_resolves_as_every_other_verb_does() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, FileKey::Keep);
        assert_eq!(
            presence_for(
                Some(dir.path()),
                &no_env(&holding("sk-test")),
                &Notices::new()
            ),
            KeyPresence::Keychain
        );
    }

    #[test]
    fn clear_removes_both_and_reports_each() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(
            dir.path(),
            BackendKind::OpenRouter,
            FileKey::Set("sk-test".to_string()),
        );
        let keychain = holding("sk-test-2");
        let sources = KeySources {
            env: Some("sk-test".to_string()),
            store: &keychain,
        };
        let outcome = clear(dir.path(), &sources, &Notices::new()).expect("clear");
        assert!(outcome.keychain_cleared);
        assert!(outcome.file_cleared);
        assert!(outcome.env_still_supplies);
        assert_eq!(held(&keychain), None);
        assert_eq!(key_source(dir.path()), KeySource::Absent);
    }

    #[test]
    fn clear_with_nothing_stored_is_a_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(dir.path(), BackendKind::OpenRouter, FileKey::Keep);
        let keychain = MemoryKeyStore::default();
        let outcome = clear(dir.path(), &no_env(&keychain), &Notices::new()).expect("clear");
        assert!(!outcome.keychain_cleared);
        assert!(!outcome.file_cleared);
        assert!(!outcome.env_still_supplies);
    }

    #[test]
    fn clear_never_calls_an_unsupported_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(
            dir.path(),
            BackendKind::OpenRouter,
            FileKey::Set("sk-test".to_string()),
        );
        let keychain = MemoryKeyStore {
            unsupported: true,
            ..MemoryKeyStore::default()
        };
        let outcome = clear(dir.path(), &no_env(&keychain), &Notices::new()).expect("clear");
        assert!(!outcome.keychain_cleared);
        assert!(outcome.file_cleared);
    }

    #[test]
    fn a_refused_keychain_delete_is_an_error_and_leaves_the_file_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        configure(
            dir.path(),
            BackendKind::OpenRouter,
            FileKey::Set("sk-test".to_string()),
        );
        let err =
            clear(dir.path(), &no_env(&failing("denied")), &Notices::new()).expect_err("refused");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("Keychain"), "{rendered}");
        assert!(rendered.contains("denied"), "{rendered}");
        assert_eq!(key_source(dir.path()), KeySource::File);
    }
}
