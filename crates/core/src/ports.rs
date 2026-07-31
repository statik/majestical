//! Ports: the traits adapters implement. The core knows these shapes,
//! never the concrete adapters behind them.
use crate::event::{AssetId, Event};
use crate::projection::Projection;
use std::collections::BTreeSet;

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

/// Position within one machine's segment file. `offset` is a byte offset that
/// always lands on a line boundary (readers never advance past a torn tail).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogCursor {
    pub machine: String,
    pub segment: String,
    pub offset: u64,
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

    /// Read only events past `cursors` (unknown segments read from 0). Returns
    /// the new events plus updated cursors covering every segment seen. Errors
    /// if a cursor points past the end of (or at a missing) segment — the
    /// caller falls back to a full rebuild.
    /// # Errors
    /// Returns `PortError` when the underlying storage cannot be read, or when
    /// a cursor doesn't correspond to a valid position in its segment.
    fn read_since_reporting(
        &self,
        cursors: &[LogCursor],
        on_bad_line: &mut dyn FnMut(&str),
    ) -> Result<(Vec<Event>, Vec<LogCursor>), PortError>;
}

/// One hard search filter, already resolved to storage terms (para refs are
/// node ids; `Online` carries the currently-mounted volume ids). The
/// contracts below are what the sqlite adapter implements; other adapters
/// must match them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Matches an asset with this exact tag.
    Tag { value: String, negated: bool },
    /// Matches an asset with an instance on a volume whose label OR id
    /// equals `value` — including an instance whose volume has no
    /// `volumes` row at all (a "ghost" volume, reachable via partial
    /// cross-machine sync).
    Volume { value: String, negated: bool },
    /// Matches an asset currently assigned to this PARA node.
    Para { node: String, negated: bool },
    /// Matches an asset with an instance whose kind is `value` — one of
    /// `core::media_kind::MediaKind::as_str()`'s strings ("image", "video",
    /// "other").
    Kind { value: String, negated: bool },
    /// `want: true` matches an asset with an instance on one of `ids` (the
    /// currently-mounted volume set); `want: false` matches an asset with an
    /// instance on none of them. An empty `ids` is the "nothing is mounted"
    /// case: `want: true` then matches nothing, `want: false` matches any
    /// asset that has at least one instance.
    Online { ids: Vec<String>, want: bool },
    /// Matches an asset with an instance whose `mtime_ms` (wall-clock
    /// milliseconds) is strictly less than this bound.
    Before(u64),
    /// Matches an asset with an instance whose `mtime_ms` (wall-clock
    /// milliseconds) is strictly greater than this bound.
    After(u64),
}

/// Presentation row for one search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSummary {
    pub asset: AssetId,
    /// The first instance's basename, or empty for a tag-only asset with no
    /// recorded instance.
    pub name: String,
    /// (volume id, volume label) pairs holding an instance.
    pub volumes: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub para: Option<String>,
}

/// Queryable projection storage, disposable and rebuildable.
pub trait CatalogStore {
    /// # Errors
    /// Returns `PortError` when the store cannot be rebuilt.
    fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError>;
    /// Assets satisfying every filter (conjunction).
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn assets_matching(&self, filters: &[Filter]) -> Result<BTreeSet<AssetId>, PortError>;
    /// Assets whose name matches any of `terms`, ranked best-first, capped at
    /// `limit` rows. One row per asset, at its best-matching name's rank,
    /// even when several of its instance names match.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn search_names_ranked(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<(AssetId, f64)>, PortError>;
    /// Presentation rows for exactly the given asset ids.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn asset_summaries(&self, ids: &[AssetId]) -> Result<Vec<AssetSummary>, PortError>;
    /// Every volume ever seen: (id, label, last-seen wall ms), ordered by id.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError>;
    /// Distinct asset count per volume, ordered by volume.
    /// # Errors
    /// Returns `PortError` when the query fails.
    fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError>;
}

/// A model-produced caption for one asset (or one video keyframe).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Caption {
    pub text: String,
    /// Derivation model tag, e.g. `describe-qwen3-vl-8b`.
    pub model_tag: String,
}

/// One suggested tag, pending human confirmation. Never written to the
/// event log — confirmation emits a plain `TagAdd`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TagSuggestion {
    pub tag: String,
    /// Model-reported confidence in [0, 1].
    pub confidence: f64,
    /// True when `tag` was already in the catalog's folksonomy.
    pub in_vocab: bool,
    pub model_tag: String,
}

/// What tag suggestion looks at: the image itself for stills, the pooled
/// keyframe captions (text-only call) for video.
#[derive(Debug)]
pub enum TagSubject<'a> {
    Image(&'a [u8]),
    Captions(&'a [String]),
}

/// Captions + open-vocabulary tag suggestion via a configured backend.
/// Implementations live outside core (`crates/describe`).
pub trait Describer {
    /// Caption one image (encoded bytes, e.g. WebP).
    ///
    /// # Errors
    /// Returns `PortError` when the backend is unreachable, the model
    /// rejects the request, or the response cannot be parsed.
    fn caption(&self, image: &[u8]) -> Result<Caption, PortError>;

    /// Suggest tags for a subject, classified against `existing_vocab`.
    ///
    /// # Errors
    /// Returns `PortError` when the backend is unreachable, the model
    /// rejects the request, or the response cannot be parsed.
    fn suggest_tags(
        &self,
        subject: TagSubject<'_>,
        existing_vocab: &[String],
    ) -> Result<Vec<TagSuggestion>, PortError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op};

    #[derive(Debug, thiserror::Error)]
    #[error("cursor references a segment this log does not have")]
    struct UnknownSegment;

    /// This test double never keeps segments, so it never has anything to
    /// resume from: any cursor a caller supplies necessarily names a segment
    /// that doesn't exist here, which is exactly the error a real log
    /// returns for a vanished segment.
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
        fn read_since_reporting(
            &self,
            cursors: &[LogCursor],
            on_bad_line: &mut dyn FnMut(&str),
        ) -> Result<(Vec<Event>, Vec<LogCursor>), PortError> {
            if !cursors.is_empty() {
                return Err(PortError::new("reading new events", UnknownSegment));
            }
            let events = self.read_all_reporting(on_bad_line)?;
            Ok((events, Vec::new()))
        }
    }

    #[derive(Default)]
    struct MemStore {
        vols: Vec<(String, String, u64)>,
    }
    impl CatalogStore for MemStore {
        fn rebuild(&mut self, projection: &Projection) -> Result<(), PortError> {
            self.vols = projection
                .volumes()
                .map(|(id, st)| {
                    (
                        id.clone(),
                        st.label().unwrap_or("").to_string(),
                        st.last_seen().map_or(0, |h| h.wall_ms),
                    )
                })
                .collect();
            Ok(())
        }
        fn assets_matching(&self, _filters: &[Filter]) -> Result<BTreeSet<AssetId>, PortError> {
            Ok(BTreeSet::new())
        }
        fn search_names_ranked(
            &self,
            _terms: &[String],
            _limit: usize,
        ) -> Result<Vec<(AssetId, f64)>, PortError> {
            Ok(Vec::new())
        }
        fn asset_summaries(&self, _ids: &[AssetId]) -> Result<Vec<AssetSummary>, PortError> {
            Ok(Vec::new())
        }
        fn volumes(&self) -> Result<Vec<(String, String, u64)>, PortError> {
            Ok(self.vols.clone())
        }
        fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, PortError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn catalog_store_port_serves_volume_queries() {
        let mut p = Projection::default();
        p.apply(&Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 7,
                counter: 0,
                machine: MachineId("m".into()),
            },
            author: "t".into(),
            op: Op::VolumeSeen {
                volume: "V1".into(),
                label: "card-a".into(),
            },
        });
        let mut store: Box<dyn CatalogStore> = Box::<MemStore>::default();
        store.rebuild(&p).expect("rebuild");
        assert_eq!(
            store.volumes().expect("volumes"),
            vec![("V1".to_string(), "card-a".to_string(), 7)]
        );
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

#[cfg(test)]
mod describer_tests {
    use super::{Caption, TagSubject, TagSuggestion};

    #[test]
    fn tag_suggestion_serializes_round_trip() {
        let s = TagSuggestion {
            tag: "person/dana".into(),
            confidence: 0.87,
            in_vocab: true,
            model_tag: "describe-qwen3-vl-8b".into(),
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: TagSuggestion = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tag, "person/dana");
        assert!((back.confidence - 0.87).abs() < f64::EPSILON);
    }

    #[test]
    fn caption_serializes_round_trip() {
        let c = Caption {
            text: "a red barn at dusk".into(),
            model_tag: "describe-x".into(),
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Caption = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.text, "a red barn at dusk");
    }

    #[test]
    fn tag_subject_borrows_without_clone() {
        let captions = vec!["a".to_string(), "b".to_string()];
        let subject = TagSubject::Captions(&captions);
        let TagSubject::Captions(inner) = subject else {
            panic!("wrong variant")
        };
        assert_eq!(inner.len(), 2);
    }
}
