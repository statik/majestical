//! Append-only catalog events. Events are immutable once written; the
//! catalog is a projection of the merged event set.
use crate::clock::Hlc;
use serde::{Deserialize, Serialize};

/// Unique, HLC-sortable event identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId(pub ulid::Ulid);

/// Content-hash identity, e.g. "xxh3:9f2a…". Same bytes = same asset.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AssetId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub hlc: Hlc,
    /// The human identity (person or service) that authored this event,
    /// distinct from the machine id carried inside `hlc`.
    pub author: String,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Op {
    /// Physical observation: a file with this content hash exists here.
    AssetSeen {
        asset: AssetId,
        volume: String,
        path: String,
        size: u64,
    },
    /// Physical observation: a volume was present. `volume` is the stable
    /// identity `AssetSeen.volume` refers to; `label` is the human name at
    /// observation time.
    VolumeSeen { volume: String, label: String },
    /// OR-Set add.
    TagAdd { asset: AssetId, tag: String },
    /// OR-Set remove: tombstones only the add-events it observed.
    TagRemove {
        asset: AssetId,
        tag: String,
        observed: Vec<EventId>,
    },
    /// HLC-LWW scalar (rating, title, para node…).
    FieldSet {
        asset: AssetId,
        field: String,
        value: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};

    #[test]
    fn event_json_round_trips() {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "elliot".into(),
            op: Op::TagAdd {
                asset: AssetId("xxh3:aa".into()),
                tag: "person/dana".into(),
            },
        };
        let json = serde_json::to_string(&e).expect("serialize");
        let back: Event = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(e, back);
    }

    #[test]
    fn event_wire_format_is_stable() {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "elliot".into(),
            op: Op::TagRemove {
                asset: AssetId("xxh3:aa".into()),
                tag: "t".into(),
                observed: vec![EventId(ulid::Ulid::from_parts(1, 2))],
            },
        };
        let json = serde_json::to_string(&e).expect("serialize");
        assert_eq!(
            json,
            r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":{"type":"tag_remove","asset":"xxh3:aa","tag":"t","observed":["00000000010000000000000002"]}}"#
        );
    }

    #[test]
    fn volume_seen_wire_format_is_stable() {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "elliot".into(),
            op: Op::VolumeSeen {
                volume: "uuid:abc".into(),
                label: "card1".into(),
            },
        };
        let json = serde_json::to_string(&e).expect("serialize");
        assert_eq!(
            json,
            r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":{"type":"volume_seen","volume":"uuid:abc","label":"card1"}}"#
        );
    }
}
