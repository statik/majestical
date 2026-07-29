//! Ports: the traits adapters implement. The core knows these shapes,
//! never the concrete adapters behind them.
use crate::event::{AssetId, Event};
use crate::projection::Projection;

/// Adapter errors crossing a port boundary keep their message and source
/// but drop the concrete type, so core-level code never names an adapter.
#[derive(Debug, thiserror::Error)]
#[error("{context}: {source}")]
pub struct PortError {
    pub context: String,
    #[source]
    pub source: Box<dyn std::error::Error + Send + Sync>,
}

impl PortError {
    pub fn new(
        context: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            context: context.into(),
            source: Box::new(source),
        }
    }
}

/// Durable append-only event storage.
pub trait EventLog {
    /// # Errors
    /// Returns `PortError` when the underlying storage cannot be written.
    fn append(&mut self, events: &[Event]) -> Result<(), PortError>;
    /// Reads every event from every machine. Corrupt entries are skipped
    /// and reported through `on_bad_line`, never fatal.
    /// # Errors
    /// Returns `PortError` when the underlying storage cannot be read.
    fn read_all_reporting(
        &self,
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<Vec<Event>, PortError>;
}

/// Queryable projection storage, disposable and rebuildable.
pub trait CatalogStore {
    /// # Errors
    /// Returns `PortError` when the store cannot be rebuilt.
    fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError>;
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn search_by_tag(&self, tag: &str) -> Result<Vec<AssetId>, PortError>;
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn search_by_name(&self, needle: &str) -> Result<Vec<AssetId>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op};

    #[derive(Default)]
    struct MemLog(Vec<Event>);
    impl EventLog for MemLog {
        fn append(&mut self, events: &[Event]) -> Result<(), PortError> {
            self.0.extend(events.iter().cloned());
            Ok(())
        }
        fn read_all_reporting(
            &self,
            _on_bad_line: &mut dyn FnMut(&str),
        ) -> Result<Vec<Event>, PortError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn event_log_port_is_object_safe_and_round_trips() {
        let mut log: Box<dyn EventLog> = Box::<MemLog>::default();
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m".into()),
            },
            author: "t".into(),
            op: Op::TagAdd {
                asset: AssetId("xxh3:aa".into()),
                tag: "t".into(),
            },
        };
        log.append(std::slice::from_ref(&e)).expect("append");
        let mut bad = 0;
        let all = log.read_all_reporting(&mut |_| bad += 1).expect("read");
        assert_eq!((all.len(), bad), (1, 0));
    }
}
