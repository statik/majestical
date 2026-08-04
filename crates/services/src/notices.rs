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
}

#[cfg(test)]
mod tests {
    use super::Notices;

    #[test]
    fn push_then_drain_preserves_order_and_empties() {
        let notices = Notices::default();
        notices.push("first");
        notices.push("second".to_string());
        assert_eq!(notices.drain(), vec!["first", "second"]);
        assert!(notices.drain().is_empty(), "drain must empty the buffer");
    }
}
