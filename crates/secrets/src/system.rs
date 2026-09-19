//! The operating system's secret store: the macOS Keychain, and a stub
//! that reports itself unsupported everywhere else.
use crate::{KeyStore, SecretError};
#[cfg(target_os = "macos")]
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, generic_password, set_generic_password,
};
#[cfg(target_os = "macos")]
use security_framework_sys::base::errSecItemNotFound;

pub const SUPPORTED: bool = cfg!(target_os = "macos");

/// The real store. Platform selection is `cfg(target_os)`, never a feature.
pub struct SystemKeyStore;

#[cfg(target_os = "macos")]
const SERVICE: &str = "majestical";
#[cfg(target_os = "macos")]
const ACCOUNT: &str = "openrouter-api-key";

#[cfg(target_os = "macos")]
impl KeyStore for SystemKeyStore {
    fn supported(&self) -> bool {
        true
    }

    fn read(&self) -> Result<Option<String>, SecretError> {
        read_item(SERVICE)
    }

    fn store(&self, key: &str) -> Result<(), SecretError> {
        store_item(SERVICE, key)
    }

    fn delete(&self) -> Result<bool, SecretError> {
        delete_item(SERVICE)
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
    // with the next, and the user's real item is never touched.
    fn service() -> String {
        format!("majestical-test-{}", std::process::id())
    }

    /// Deletes the throwaway item even when an assertion fails midway, so a
    /// failed run leaves nothing in the login Keychain.
    struct Cleanup(String);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = super::delete_item(&self.0);
        }
    }

    #[test]
    fn the_system_store_is_switched_on() {
        use crate::KeyStore;
        assert_eq!(
            (super::SUPPORTED, super::SystemKeyStore.supported()),
            (true, true)
        );
    }

    // Talks to the real login Keychain, under the throwaway per-process
    // service name above. It can take about a minute when `target/debug/deps`
    // is very large, because the Security framework scans the calling
    // binary's directory (measured 2026-09-18: 60 s from a deps dir with
    // 1.8 M entries, 0.1 s from anywhere else).
    #[test]
    fn store_read_overwrite_delete_round_trip() {
        let service = service();
        let _cleanup = Cleanup(service.clone());
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
        let service = format!("majestical-test-nonutf8-{}", std::process::id());
        let _cleanup = Cleanup(service.clone());
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
        assert!(!super::SUPPORTED);
        assert!(!SystemKeyStore.supported());
        assert!(matches!(
            SystemKeyStore.read(),
            Err(SecretError::Unsupported)
        ));
        assert!(matches!(
            SystemKeyStore.store("sk-test"),
            Err(SecretError::Unsupported)
        ));
        assert!(matches!(
            SystemKeyStore.delete(),
            Err(SecretError::Unsupported)
        ));
    }
}
