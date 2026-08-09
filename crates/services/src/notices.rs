//! A shared, order-preserving sink for user-facing diagnostics that used to
//! go straight to stderr. Services push; heads drain — the CLI prints each
//! line verbatim to stderr, MCP/GUI serialize them as `notices` fields on
//! outcome structs. Interior mutability (a `Mutex`, so `App` stays `Sync`)
//! lets `&self` methods like `App::events` record without a signature change
//! rippling through every verb.

use std::sync::Mutex;

#[derive(Default)]
pub struct Notices(Mutex<Vec<String>>);

impl Notices {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one diagnostic line, exactly as it used to appear on stderr.
    pub fn push(&self, message: impl Into<String>) {
        // A poisoned lock means another thread panicked mid-push; the
        // buffer itself is still valid Vec data — keep collecting rather
        // than dropping diagnostics on the floor.
        let mut buf = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.push(message.into());
    }

    /// Takes every collected line, in push order, leaving the sink empty.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        let mut buf = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *buf)
    }

    /// Wraps an `Err` leaving a verb while this sink still holds
    /// diagnostics, so the failure carries them instead of dropping them —
    /// the `Err`-path counterpart of folding `drain()` into an `Ok`
    /// outcome's `notices` field. `Ok` passes through untouched, as does an
    /// `Err` when the sink is empty. Wrapping an error that is already a
    /// carrier appends to it rather than nesting.
    ///
    /// # Errors
    ///
    /// Returns `Err` exactly when `result` was already `Err` — this never
    /// turns an `Ok` into a failure. The returned error is
    /// [`crate::error::ServiceError::WithNotices`] whenever the sink held
    /// diagnostics at call time, otherwise `result`'s original error is
    /// passed through unwrapped.
    pub fn attach_on_err<T>(
        &self,
        result: Result<T, crate::error::ServiceError>,
    ) -> Result<T, crate::error::ServiceError> {
        use crate::error::ServiceError;
        match result {
            Ok(value) => Ok(value),
            Err(err) => {
                let drained = self.drain();
                if drained.is_empty() {
                    return Err(err);
                }
                Err(match err {
                    ServiceError::WithNotices {
                        mut notices,
                        source,
                    } => {
                        notices.extend(drained);
                        ServiceError::WithNotices { notices, source }
                    }
                    other => ServiceError::WithNotices {
                        notices: drained,
                        source: Box::new(other),
                    },
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Notices;
    use crate::error::ServiceError;

    #[test]
    fn push_then_drain_preserves_order_and_empties() {
        let notices = Notices::default();
        notices.push("first");
        notices.push("second".to_string());
        assert_eq!(notices.drain(), vec!["first", "second"]);
        assert!(notices.drain().is_empty(), "drain must empty the buffer");
    }

    #[test]
    fn a_poisoned_lock_still_collects_and_drains() {
        let notices = std::sync::Arc::new(Notices::default());
        notices.push("before");
        let poisoner = std::sync::Arc::clone(&notices);
        let joined = std::thread::spawn(move || {
            let _guard = poisoner.0.lock().expect("lock");
            panic!("poisoning the mutex on purpose");
        })
        .join();
        assert!(joined.is_err(), "the spawned thread must have panicked");
        notices.push("after");
        assert_eq!(
            notices.drain(),
            vec!["before", "after"],
            "a panic elsewhere must not cost us diagnostics"
        );
    }

    #[test]
    fn attach_on_err_wraps_a_failure_with_the_sink_contents() {
        let notices = Notices::default();
        notices.push("warning one");
        notices.push("warning two");
        let result: Result<(), ServiceError> = Err(ServiceError::Other(anyhow::anyhow!("boom")));
        let err = notices.attach_on_err(result).expect_err("must stay Err");
        let ServiceError::WithNotices {
            notices: carried,
            source,
        } = err
        else {
            panic!("expected WithNotices, got a different variant");
        };
        assert_eq!(carried, vec!["warning one", "warning two"]);
        assert!(matches!(*source, ServiceError::Other(_)));
        assert!(notices.drain().is_empty(), "attach must drain the sink");
    }

    #[test]
    fn attach_on_err_passes_ok_and_empty_sink_through_untouched() {
        let notices = Notices::default();
        notices.push("still here");
        let ok: Result<u8, ServiceError> = Ok(7);
        assert_eq!(notices.attach_on_err(ok).expect("ok stays ok"), 7);
        assert_eq!(
            notices.drain(),
            vec!["still here"],
            "an Ok result must leave the sink's contents untouched, not drain them"
        );
        let bare: Result<(), ServiceError> = Err(ServiceError::Other(anyhow::anyhow!("boom")));
        let err = notices.attach_on_err(bare).expect_err("must stay Err");
        assert!(
            matches!(err, ServiceError::Other(_)),
            "an empty sink must not wrap"
        );
    }

    #[test]
    fn attach_on_err_merges_into_an_existing_carrier_instead_of_nesting() {
        let notices = Notices::default();
        notices.push("later warning");
        let already: Result<(), ServiceError> = Err(ServiceError::WithNotices {
            notices: vec!["earlier warning".to_string()],
            source: Box::new(ServiceError::Other(anyhow::anyhow!("boom"))),
        });
        let err = notices.attach_on_err(already).expect_err("must stay Err");
        let ServiceError::WithNotices {
            notices: carried,
            source,
        } = err
        else {
            panic!("expected WithNotices");
        };
        assert_eq!(carried, vec!["earlier warning", "later warning"]);
        assert!(
            matches!(*source, ServiceError::Other(_)),
            "must merge, never nest a carrier in a carrier"
        );
    }
}
