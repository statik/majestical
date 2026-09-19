//! The operating system's secret store: the macOS Keychain, and a stub
//! that reports itself unsupported everywhere else.
use crate::{KeyStore, SecretError};
#[cfg(target_os = "macos")]
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, generic_password, set_generic_password,
};
#[cfg(target_os = "macos")]
use security_framework_sys::base::errSecItemNotFound;

/// Whether this build has a real secret store (macOS only). For UI that
/// must decide before it holds a store.
pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The environment variable a HEAD reads to override the Keychain service
/// name. It exists for the tests, which must never touch the user's real
/// item. This crate itself never reads the environment:
/// the head reads the variable and hands the value to [`SystemKeyStore::new`].
pub const SERVICE_ENV: &str = "MAJ_KEYCHAIN_SERVICE";

/// The real store. Platform selection is `cfg(target_os)`, never a feature.
pub struct SystemKeyStore {
    #[cfg(target_os = "macos")]
    service: String,
}

#[cfg(target_os = "macos")]
const SERVICE: &str = "majestical";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "openrouter-api-key";

impl SystemKeyStore {
    /// The store under `service`'s Keychain item; `None` or a blank name
    /// means the default, `majestical`. Off macOS the name is ignored.
    #[must_use]
    pub fn new(service: Option<String>) -> Self {
        #[cfg(target_os = "macos")]
        {
            let service = service
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| SERVICE.to_string());
            Self { service }
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop(service);
            Self {}
        }
    }
}

impl Default for SystemKeyStore {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(target_os = "macos")]
impl KeyStore for SystemKeyStore {
    fn supported(&self) -> bool {
        true
    }

    fn read(&self) -> Result<Option<String>, SecretError> {
        read_item(&self.service)
    }

    fn store(&self, key: &str) -> Result<(), SecretError> {
        store_item(&self.service, key)
    }

    fn delete(&self) -> Result<bool, SecretError> {
        delete_item(&self.service)
    }
}

#[cfg(not(target_os = "macos"))]
impl KeyStore for SystemKeyStore {
    fn supported(&self) -> bool {
        false
    }

    fn read(&self) -> Result<Option<String>, SecretError> {
        Err(SecretError::Unsupported)
    }

    fn store(&self, _key: &str) -> Result<(), SecretError> {
        Err(SecretError::Unsupported)
    }

    fn delete(&self) -> Result<bool, SecretError> {
        Err(SecretError::Unsupported)
    }
}

#[cfg(target_os = "macos")]
fn read_item(service: &str) -> Result<Option<String>, SecretError> {
    let item = PasswordOptions::new_generic_password(service, ACCOUNT);
    match generic_password(item) {
        // `from_utf8`'s own error can quote bytes of the secret: never format it.
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| SecretError::Store("the stored key is not UTF-8".to_string())),
        Err(err) if err.code() == errSecItemNotFound => Ok(None),
        Err(err) => Err(SecretError::Store(err.to_string())),
    }
}

#[cfg(target_os = "macos")]
fn store_item(service: &str, key: &str) -> Result<(), SecretError> {
    set_generic_password(service, ACCOUNT, key.as_bytes())
        .map_err(|err| SecretError::Store(err.to_string()))
}

#[cfg(target_os = "macos")]
fn delete_item(service: &str) -> Result<bool, SecretError> {
    match delete_generic_password(service, ACCOUNT) {
        Ok(()) => Ok(true),
        Err(err) if err.code() == errSecItemNotFound => Ok(false),
        Err(err) => Err(SecretError::Store(err.to_string())),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    // A throwaway service per test process so a crashed run cannot collide
    // with the next, and the user's real item is never touched. The two
    // Keychain tests need distinct names (the tag) because they run as
    // threads of one process.
    fn service(tag: &str) -> String {
        format!("majestical-test-{tag}-{}", std::process::id())
    }

    /// Deletes the throwaway item even when an assertion fails midway, so a
    /// failed run leaves nothing in the login Keychain.
    struct Cleanup(String);

    impl Cleanup {
        /// Panics on any name but a throwaway one. This is the type nearest
        /// the destructive call — `Drop` deletes whatever it names — so it
        /// refuses the developer's real item here rather than trusting
        /// every caller to pass the right thing.
        fn new(service: &str) -> Self {
            assert!(
                service.starts_with("majestical-test-"),
                "not a throwaway Keychain service: {service}"
            );
            Self(service.to_string())
        }
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = super::delete_item(&self.0);
        }
    }

    #[test]
    #[should_panic(expected = "not a throwaway Keychain service")]
    fn the_cleanup_refuses_a_name_that_is_not_a_throwaway() {
        // Never the real service name: a regressed guard would construct the
        // value and its `Drop` would delete whatever this names.
        let _cleanup = Cleanup::new("not-a-throwaway");
    }

    #[test]
    fn the_system_store_is_switched_on() {
        use crate::KeyStore;
        // A tuple: a bare assert! on a const trips clippy::assertions_on_constants.
        assert_eq!(
            (
                super::SUPPORTED,
                super::SystemKeyStore::default().supported()
            ),
            (true, true)
        );
    }

    #[test]
    fn no_name_or_a_blank_one_means_the_default_service() {
        use super::SystemKeyStore;
        assert_eq!(SystemKeyStore::default().service, "majestical");
        assert_eq!(SystemKeyStore::new(None).service, "majestical");
        assert_eq!(
            SystemKeyStore::new(Some(String::new())).service,
            "majestical"
        );
        assert_eq!(
            SystemKeyStore::new(Some("  \t".to_string())).service,
            "majestical"
        );
        assert_eq!(
            SystemKeyStore::new(Some("majestical-other".to_string())).service,
            "majestical-other"
        );
    }

    // The trait methods, under a throwaway service: the named store is the
    // only way a test may reach them, since the default one is the user's.
    #[test]
    fn a_named_store_round_trips_through_the_trait() {
        use crate::KeyStore;
        let service = service("trait");
        let _cleanup = Cleanup::new(&service);
        let store = super::SystemKeyStore::new(Some(service.clone()));
        // Before any Keychain call: were `new` to ignore the name, every
        // line below would address the developer's real item instead.
        assert_eq!(store.service, service);
        assert_eq!(store.read().expect("read"), None);
        store.store("sk-test").expect("store");
        assert_eq!(store.read().expect("read").as_deref(), Some("sk-test"));
        assert_eq!(
            super::read_item(&service).expect("read").as_deref(),
            Some("sk-test")
        );
        assert!(store.delete().expect("delete"));
        assert!(!store.delete().expect("second delete"));
        assert_eq!(store.read().expect("read"), None);
    }

    // Talks to the real login Keychain, under the throwaway per-process
    // service name above. Slow when the test binary sits in a huge directory:
    // 60 s from a `target/debug/deps` with 1.8 M entries against 0.1 s for the
    // same binary copied to an empty dir (measured 2026-09-18; the cause looks
    // like Security.framework scanning the caller's directory). To run it
    // fast, copy the test executable to a `mktemp -d` dir and run the copy.
    #[test]
    fn the_keychain_round_trips_a_key_and_reports_whether_delete_found_one() {
        let service = service("roundtrip");
        let _cleanup = Cleanup::new(&service);
        assert_eq!(super::read_item(&service).expect("read"), None);
        super::store_item(&service, "sk-test").expect("store");
        assert_eq!(
            super::read_item(&service).expect("read").as_deref(),
            Some("sk-test")
        );
        super::store_item(&service, "sk-test-2").expect("overwrite");
        assert_eq!(
            super::read_item(&service).expect("read").as_deref(),
            Some("sk-test-2")
        );
        assert!(super::delete_item(&service).expect("delete"));
        assert!(!super::delete_item(&service).expect("second delete"));
    }

    #[test]
    fn a_stored_value_that_is_not_utf8_is_a_fixed_error_without_its_bytes() {
        let service = service("nonutf8");
        let _cleanup = Cleanup::new(&service);
        super::set_generic_password(&service, super::ACCOUNT, b"sk-\xff\xfetest").expect("store");
        let read = super::read_item(&service);
        assert!(super::delete_item(&service).expect("delete"));
        let Err(crate::SecretError::Store(message)) = read else {
            panic!("expected a Store error");
        };
        assert_eq!(message, "the stored key is not UTF-8");
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use crate::{KeyStore, SecretError, SystemKeyStore};

    #[test]
    fn the_stub_is_unsupported_everywhere() {
        // A tuple: a bare assert! on a const trips clippy::assertions_on_constants.
        assert_eq!(
            (super::SUPPORTED, SystemKeyStore::default().supported()),
            (false, false)
        );
        let store = SystemKeyStore::new(Some("ignored-off-macos".to_string()));
        assert!(matches!(store.read(), Err(SecretError::Unsupported)));
        assert!(matches!(
            store.store("sk-test"),
            Err(SecretError::Unsupported)
        ));
        assert!(matches!(store.delete(), Err(SecretError::Unsupported)));
    }
}
