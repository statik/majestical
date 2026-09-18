//! The one place a secret store is touched. Head-side only: `services`
//! and `describe` never depend on this crate — the heads resolve a key
//! here and pass it in, exactly as they pass the environment's.
mod system;
pub use system::{SUPPORTED, SystemKeyStore};

use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("this platform has no supported secret store")]
    Unsupported,
    /// The store's own message. Never contains the secret.
    #[error("secret store: {0}")]
    Store(String),
}

/// One stored secret: the `OpenRouter` API key.
pub trait KeyStore {
    fn supported(&self) -> bool;
    /// # Errors
    /// `Unsupported` off macOS; `Store` when the Keychain refuses (locked,
    /// or the user denied the prompt).
    fn read(&self) -> Result<Option<String>, SecretError>;
    /// # Errors
    /// As [`Self::read`].
    fn store(&self, key: &str) -> Result<(), SecretError>;
    /// `Ok(false)` when nothing was stored.
    /// # Errors
    /// As [`Self::read`].
    fn delete(&self) -> Result<bool, SecretError>;
}

/// The in-memory test double: the heads' tests exercise their read and
/// write paths over this, never over a real Keychain.
#[derive(Default)]
pub struct MemoryKeyStore {
    pub key: Mutex<Option<String>>,
    /// When set, every call returns `SecretError::Store(fail)`.
    pub fail: Option<String>,
    /// When true, `supported()` is false and every call is `Unsupported`.
    pub unsupported: bool,
}

impl KeyStore for MemoryKeyStore {
    fn supported(&self) -> bool {
        !self.unsupported
    }

    fn read(&self) -> Result<Option<String>, SecretError> {
        self.refusal()?;
        Ok(self.slot().clone())
    }

    fn store(&self, key: &str) -> Result<(), SecretError> {
        self.refusal()?;
        *self.slot() = Some(key.to_string());
        Ok(())
    }

    fn delete(&self) -> Result<bool, SecretError> {
        self.refusal()?;
        Ok(self.slot().take().is_some())
    }
}

impl MemoryKeyStore {
    fn refusal(&self) -> Result<(), SecretError> {
        if self.unsupported {
            return Err(SecretError::Unsupported);
        }
        match &self.fail {
            Some(message) => Err(SecretError::Store(message.clone())),
            None => Ok(()),
        }
    }

    fn slot(&self) -> MutexGuard<'_, Option<String>> {
        self.key.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Where the head found the key it will pass to services.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadKeySource {
    Env,
    Keychain,
    None,
}

#[derive(Clone)]
pub struct ResolvedKey {
    pub key: Option<String>,
    pub source: HeadKeySource,
    /// Set when the store was consulted and failed; the head pushes it as
    /// a notice. Never contains a key.
    pub notice: Option<String>,
}

/// By hand, not derived: a derived `Debug` would print the key.
impl fmt::Debug for ResolvedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedKey")
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .field("source", &self.source)
            .field("notice", &self.notice)
            .finish()
    }
}

/// `env`, then the store. The store is read ONLY when `env` is absent and
/// `wants_keychain` (the configured backend is `OpenRouter`) — so a
/// local-backend user never sees a macOS prompt, and neither does anyone
/// who set the variable.
#[must_use]
pub fn resolve(env: Option<String>, wants_keychain: bool, store: &dyn KeyStore) -> ResolvedKey {
    if let Some(key) = env {
        return ResolvedKey {
            key: Some(key),
            source: HeadKeySource::Env,
            notice: None,
        };
    }
    let none = |notice| ResolvedKey {
        key: None,
        source: HeadKeySource::None,
        notice,
    };
    if !wants_keychain || !store.supported() {
        return none(None);
    }
    match store.read() {
        Ok(Some(key)) => ResolvedKey {
            key: Some(key),
            source: HeadKeySource::Keychain,
            notice: None,
        },
        Ok(None) | Err(SecretError::Unsupported) => none(None),
        Err(SecretError::Store(message)) => none(Some(format!(
            "note: the macOS Keychain could not be read ({message}) — \
             set MAJ_OPENROUTER_KEY to supply the key without it"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{HeadKeySource, KeyStore, MemoryKeyStore, ResolvedKey, SecretError, resolve};

    fn holding(key: &str) -> MemoryKeyStore {
        MemoryKeyStore {
            key: std::sync::Mutex::new(Some(key.to_string())),
            ..MemoryKeyStore::default()
        }
    }

    fn failing(message: &str) -> MemoryKeyStore {
        MemoryKeyStore {
            fail: Some(message.to_string()),
            ..MemoryKeyStore::default()
        }
    }

    #[test]
    fn env_wins_and_the_store_is_never_read() {
        let resolved = resolve(Some("sk-test".to_string()), true, &failing("denied"));
        assert_eq!(resolved.source, HeadKeySource::Env);
        assert_eq!(resolved.key.as_deref(), Some("sk-test"));
        assert_eq!(resolved.notice, None);
    }

    #[test]
    fn the_store_is_not_read_for_a_local_backend() {
        let resolved = resolve(None, false, &failing("denied"));
        assert_eq!(resolved.source, HeadKeySource::None);
        assert_eq!(resolved.key, None);
        assert_eq!(resolved.notice, None);
    }

    #[test]
    fn a_stored_key_resolves_as_keychain() {
        let resolved = resolve(None, true, &holding("sk-test"));
        assert_eq!(resolved.source, HeadKeySource::Keychain);
        assert_eq!(resolved.key.as_deref(), Some("sk-test"));
        assert_eq!(resolved.notice, None);
    }

    #[test]
    fn an_empty_store_resolves_as_none() {
        let resolved = resolve(None, true, &MemoryKeyStore::default());
        assert_eq!(resolved.source, HeadKeySource::None);
        assert_eq!(resolved.key, None);
        assert_eq!(resolved.notice, None);
    }

    #[test]
    fn a_store_failure_is_a_notice_not_an_error_and_names_the_env_var() {
        let resolved = resolve(None, true, &failing("denied"));
        assert_eq!(resolved.source, HeadKeySource::None);
        assert_eq!(resolved.key, None);
        let notice = resolved.notice.expect("a notice");
        assert!(notice.contains("MAJ_OPENROUTER_KEY"), "{notice}");
        assert!(notice.contains("denied"), "{notice}");
    }

    #[test]
    fn an_unsupported_store_is_silent() {
        let store = MemoryKeyStore {
            unsupported: true,
            ..holding("sk-test")
        };
        let resolved = resolve(None, true, &store);
        assert_eq!(resolved.source, HeadKeySource::None);
        assert_eq!(resolved.key, None);
        assert_eq!(resolved.notice, None);
    }

    #[test]
    fn a_resolved_key_never_debug_prints_the_key() {
        let resolved = ResolvedKey {
            key: Some("sk-test".to_string()),
            source: HeadKeySource::Keychain,
            notice: Some("a notice".to_string()),
        };
        let rendered = format!("{resolved:?} {resolved:#?}");
        assert!(!rendered.contains("sk-test"));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("Keychain"));
        assert!(rendered.contains("a notice"));
        let absent = ResolvedKey {
            key: None,
            ..resolved
        };
        assert!(!format!("{absent:?}").contains("<redacted>"));
    }

    #[test]
    fn the_memory_store_round_trips_and_reports_whether_delete_found_a_key() {
        let store = MemoryKeyStore::default();
        assert!(store.supported());
        assert_eq!(store.read().expect("read"), None);
        store.store("sk-test").expect("store");
        assert_eq!(store.read().expect("read").as_deref(), Some("sk-test"));
        store.store("sk-test-2").expect("overwrite");
        assert_eq!(store.read().expect("read").as_deref(), Some("sk-test-2"));
        assert!(store.delete().expect("delete"));
        assert!(!store.delete().expect("second delete"));
        assert_eq!(store.read().expect("read"), None);
    }

    #[test]
    fn the_memory_store_fails_or_is_unsupported_on_every_call_when_told_to() {
        let store = failing("denied");
        assert!(store.supported());
        assert!(matches!(store.read(), Err(SecretError::Store(m)) if m == "denied"));
        assert!(matches!(store.store("sk-test"), Err(SecretError::Store(m)) if m == "denied"));
        assert!(matches!(store.delete(), Err(SecretError::Store(m)) if m == "denied"));

        let store = MemoryKeyStore {
            unsupported: true,
            ..MemoryKeyStore::default()
        };
        assert!(!store.supported());
        assert!(matches!(store.read(), Err(SecretError::Unsupported)));
        assert!(matches!(
            store.store("sk-test"),
            Err(SecretError::Unsupported)
        ));
        assert!(matches!(store.delete(), Err(SecretError::Unsupported)));
    }
}
