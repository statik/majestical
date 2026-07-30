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

/// PARA node kind. Serialized lowercase; pinned by golden tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParaKind {
    Project,
    Area,
    Resource,
    Archive,
}

impl ParaKind {
    /// The on-disk directory a node of this kind materializes under.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Project => "Projects",
            Self::Area => "Areas",
            Self::Resource => "Resources",
            Self::Archive => "Archives",
        }
    }
}

/// Outcome of one hash verification of one file instance (spec §2 hash history).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    Original,
    Verified,
    Failed,
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
        /// File modification time (ms since epoch). Additive field: events
        /// written before phase 4 parse as 0 (meaning "unknown").
        #[serde(default)]
        mtime_ms: u64,
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
    /// A PARA node exists. `node` is a ULID minted once at creation; in
    /// legitimate histories only one create exists per node, but `kind` and
    /// `name` still fold as HLC-LWW (like `ParaNodeRename`) so pathological
    /// concurrent creates for the same node id still converge.
    ParaNodeCreate {
        node: String,
        kind: ParaKind,
        name: String,
    },
    /// HLC-LWW rename of a node.
    ParaNodeRename { node: String, name: String },
    /// Marks a node archived. Monotonic: no unarchive op this phase.
    ParaNodeArchive { node: String },
    /// HLC-LWW assignment of an asset to a PARA node.
    AssetParaSet { asset: AssetId, node: String },
    /// Physical observation: this instance's bytes hashed to `value` at
    /// `hashdate_ms`, with `outcome` per the ASC MHL action model.
    VerificationRecorded {
        asset: AssetId,
        volume: String,
        path: String,
        algo: String,
        value: String,
        outcome: VerifyOutcome,
        hashdate_ms: u64,
    },
    /// An ASC MHL generation was written for `volume`; `roothash` is a hash
    /// of the manifest file's own bytes, so on-disk tampering is detectable.
    /// The writer chooses the algorithm and self-describes it in the value
    /// (in practice c4, per the ASC MHL chain-file requirement — see
    /// `majestical-ingest`'s `mhl` module).
    ManifestRecorded {
        volume: String,
        mhl_path: String,
        generation: u32,
        roothash: String,
    },
    /// Save (or overwrite) a named search query. HLC-LWW per name.
    SavedSearchSet { name: String, query: String },
    /// Remove a named search. An LWW tombstone: a later Set revives the name.
    SavedSearchRemove { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};

    #[test]
    fn dir_name_maps_every_kind_to_its_para_directory() {
        assert_eq!(ParaKind::Project.dir_name(), "Projects");
        assert_eq!(ParaKind::Area.dir_name(), "Areas");
        assert_eq!(ParaKind::Resource.dir_name(), "Resources");
        assert_eq!(ParaKind::Archive.dir_name(), "Archives");
    }

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

    fn golden(op: Op) -> String {
        let e = Event {
            id: EventId(ulid::Ulid::from_parts(1, 1)),
            hlc: Hlc {
                wall_ms: 1,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "elliot".into(),
            op,
        };
        serde_json::to_string(&e).expect("serialize")
    }

    const PREFIX: &str = r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":"#;

    #[test]
    fn para_and_ingest_ops_wire_formats_are_stable() {
        let node = "00000000010000000000000002".to_string();
        for (op, want) in [
            (
                Op::AssetSeen {
                    asset: AssetId("xxh3:aa".into()),
                    volume: "uuid:abc".into(),
                    path: "clips/a.mov".into(),
                    size: 4,
                    mtime_ms: 5,
                },
                r#"{"type":"asset_seen","asset":"xxh3:aa","volume":"uuid:abc","path":"clips/a.mov","size":4,"mtime_ms":5}"#,
            ),
            (
                Op::ParaNodeCreate {
                    node: node.clone(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                r#"{"type":"para_node_create","node":"00000000010000000000000002","kind":"project","name":"client-x"}"#,
            ),
            (
                Op::ParaNodeRename {
                    node: node.clone(),
                    name: "client-y".into(),
                },
                r#"{"type":"para_node_rename","node":"00000000010000000000000002","name":"client-y"}"#,
            ),
            (
                Op::ParaNodeArchive { node: node.clone() },
                r#"{"type":"para_node_archive","node":"00000000010000000000000002"}"#,
            ),
            (
                Op::AssetParaSet {
                    asset: AssetId("xxh3:aa".into()),
                    node: node.clone(),
                },
                r#"{"type":"asset_para_set","asset":"xxh3:aa","node":"00000000010000000000000002"}"#,
            ),
            (
                Op::VerificationRecorded {
                    asset: AssetId("xxh3:aa".into()),
                    volume: "uuid:abc".into(),
                    path: "clips/a.mov".into(),
                    algo: "xxh64".into(),
                    value: "0011223344556677".into(),
                    outcome: VerifyOutcome::Verified,
                    hashdate_ms: 42,
                },
                r#"{"type":"verification_recorded","asset":"xxh3:aa","volume":"uuid:abc","path":"clips/a.mov","algo":"xxh64","value":"0011223344556677","outcome":"verified","hashdate_ms":42}"#,
            ),
            (
                Op::ManifestRecorded {
                    volume: "uuid:abc".into(),
                    mhl_path: "ascmhl/0001_dest_2026-07-29_120000.mhl".into(),
                    generation: 1,
                    roothash: "xxh64:8899aabbccddeeff".into(),
                },
                r#"{"type":"manifest_recorded","volume":"uuid:abc","mhl_path":"ascmhl/0001_dest_2026-07-29_120000.mhl","generation":1,"roothash":"xxh64:8899aabbccddeeff"}"#,
            ),
            (
                Op::SavedSearchSet {
                    name: "n1".into(),
                    query: "tag:x sunset".into(),
                },
                r#"{"type":"saved_search_set","name":"n1","query":"tag:x sunset"}"#,
            ),
            (
                Op::SavedSearchRemove { name: "n1".into() },
                r#"{"type":"saved_search_remove","name":"n1"}"#,
            ),
        ] {
            let json = golden(op);
            assert_eq!(json, format!("{PREFIX}{want}}}"));
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(
                golden(back.op),
                json,
                "round trip must reproduce the wire format"
            );
        }
    }

    #[test]
    fn asset_seen_without_mtime_still_parses() {
        let old = r#"{"id":"00000000010000000000000001","hlc":{"wall_ms":1,"counter":0,"machine":"m1"},"author":"elliot","op":{"type":"asset_seen","asset":"xxh3:aa","volume":"uuid:abc","path":"clips/a.mov","size":4}}"#;
        let event: Event = serde_json::from_str(old).expect("old wire format must parse");
        let Op::AssetSeen { mtime_ms, .. } = event.op else {
            panic!("wrong variant");
        };
        assert_eq!(mtime_ms, 0);
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
