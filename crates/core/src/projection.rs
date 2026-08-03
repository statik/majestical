//! In-memory CRDT projection of an event set. Apply is commutative and
//! idempotent: tombstoned add-ids are remembered so a remove arriving
//! before its add still wins over exactly that add and nothing else.
use crate::clock::Hlc;
use crate::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Which projected entity an applied event changed. `Nothing` means the event
/// was a duplicate (idempotent replay) and no state moved.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Touched {
    Nothing,
    Asset(AssetId),
    Volume(String),
    ParaNode(String),
    /// Manifest set for a volume id changed.
    Manifests(String),
    /// A named saved search was set or removed.
    SavedSearch(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetState {
    /// (volume, path) -> newest observed instance attributes for this
    /// content hash. HLC-LWW: a rescan of the same (volume, path) updates
    /// the entry in place rather than duplicating it.
    ///
    /// Serialized via `instance_map` as an array of entries: JSON object
    /// keys must be strings, and `(String, String)` isn't one.
    #[serde(with = "instance_map")]
    pub instances: BTreeMap<(String, String), InstanceInfo>,
    /// tag -> live add-event ids; never holds an empty set — `TagRemove`'s
    /// retain drops emptied entries.
    tag_adds: BTreeMap<String, BTreeSet<EventId>>,
    /// add-event ids tombstoned by observed removes.
    removed_adds: BTreeSet<EventId>,
    /// field -> (hlc, value); higher tuple wins deterministically.
    fields: BTreeMap<String, (Hlc, String)>,
    /// PARA assignment: (hlc, node id); higher tuple wins.
    para: Option<(Hlc, String)>,
    /// Hash-history facts observed for this asset's instances.
    verifications: BTreeSet<VerificationRecord>,
}

/// One file instance's LWW attributes. Ord is (hlc, size, `mtime_ms`) so the
/// derived comparison matches the projection-wide LWW rule (HLC first).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub hlc: Hlc,
    pub size: u64,
    pub mtime_ms: u64,
}

/// Serializes `AssetState::instances` as a JSON array of entries rather than
/// an object, since `serde_json` rejects non-string map keys.
mod instance_map {
    use super::InstanceInfo;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    #[derive(Serialize, Deserialize)]
    struct Entry {
        volume: String,
        path: String,
        #[serde(flatten)]
        info: InstanceInfo,
    }

    pub fn serialize<S: Serializer>(
        map: &BTreeMap<(String, String), InstanceInfo>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let entries: Vec<Entry> = map
            .iter()
            .map(|((volume, path), info)| Entry {
                volume: volume.clone(),
                path: path.clone(),
                info: info.clone(),
            })
            .collect();
        entries.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<(String, String), InstanceInfo>, D::Error> {
        let entries = Vec::<Entry>::deserialize(deserializer)?;
        Ok(entries
            .into_iter()
            .map(|e| ((e.volume, e.path), e.info))
            .collect())
    }
}

/// One verification observation; a plain fact, deduped by full value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VerificationRecord {
    pub volume: String,
    pub path: String,
    pub algo: String,
    pub value: String,
    pub outcome: VerifyOutcome,
    pub hashdate_ms: u64,
}

impl VerificationRecord {
    /// Builds the record from `op`. Kept off `apply_tracking`'s match arm so
    /// that function stays under the crate's max-function-length lint.
    fn from_op(op: &Op) -> Self {
        let Op::VerificationRecorded {
            volume,
            path,
            algo,
            value,
            outcome,
            hashdate_ms,
            ..
        } = op
        else {
            unreachable!("from_op is only called for Op::VerificationRecorded")
        };
        Self {
            volume: volume.clone(),
            path: path.clone(),
            algo: algo.clone(),
            value: value.clone(),
            outcome: *outcome,
            hashdate_ms: *hashdate_ms,
        }
    }
}

/// One recorded ASC MHL generation; a plain fact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ManifestRecord {
    pub generation: u32,
    pub mhl_path: String,
    pub roothash: String,
}

impl ManifestRecord {
    /// Builds the record from `op`. Kept off `apply_tracking`'s match arm so
    /// that function stays under the crate's max-function-length lint.
    fn from_op(op: &Op) -> Self {
        let Op::ManifestRecorded {
            mhl_path,
            generation,
            roothash,
            ..
        } = op
        else {
            unreachable!("from_op is only called for Op::ManifestRecorded")
        };
        Self {
            generation: *generation,
            mhl_path: mhl_path.clone(),
            roothash: roothash.clone(),
        }
    }
}

/// PARA node folded state. `kind` is meant to be immutable in legitimate
/// histories (node ids are minted once, at creation), but is folded as LWW
/// rather than first-write-wins: two concurrent `ParaNodeCreate`s for the
/// same node id must still resolve to the same winner regardless of apply
/// order. `name` is LWW across create+rename; `archived` is monotonic.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParaNodeState {
    kind: Option<(Hlc, ParaKind)>,
    name: Option<(Hlc, String)>,
    archived: bool,
}

impl ParaNodeState {
    #[must_use]
    pub fn kind(&self) -> Option<ParaKind> {
        self.kind.as_ref().map(|(_, k)| *k)
    }
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|(_, n)| n.as_str())
    }
    #[must_use]
    pub fn archived(&self) -> bool {
        self.archived
    }
}

/// Tracked state for one volume, folded from every `VolumeSeen` observed.
///
/// Label and last-seen are folded from the same LWW winner rather than
/// tracked as separate fields: an `Hlc` totally orders (wall, counter,
/// machine), so the observation with the highest `Hlc` is unambiguously
/// both the freshest label and the most recent sighting.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeState {
    seen: Option<(Hlc, String)>,
}

impl VolumeState {
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.seen.as_ref().map(|(_, l)| l.as_str())
    }

    #[must_use]
    pub fn last_seen(&self) -> Option<&Hlc> {
        self.seen.as_ref().map(|(hlc, _)| hlc)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projection {
    assets: BTreeMap<AssetId, AssetState>,
    volumes: BTreeMap<String, VolumeState>,
    applied: BTreeSet<EventId>,
    para_nodes: BTreeMap<String, ParaNodeState>,
    /// volume id -> recorded manifest generations.
    manifests: BTreeMap<String, BTreeSet<ManifestRecord>>,
    /// name -> (hlc, query); `None` query means an LWW tombstone (removed).
    /// `#[serde(default)]` lets pre-phase-4 snapshots deserialize — no old
    /// event can be a saved-search op, so the empty default is still correct.
    #[serde(default)]
    saved_searches: BTreeMap<String, (Hlc, Option<String>)>,
}

impl Projection {
    /// Thin wrapper over [`Self::apply_tracking`] for callers that don't need
    /// to know which entity changed.
    pub fn apply(&mut self, event: &Event) {
        let _ = self.apply_tracking(event);
    }

    /// Applies `event` and reports which entity it changed, or
    /// [`Touched::Nothing`] if this event id was already applied.
    #[must_use]
    pub fn apply_tracking(&mut self, event: &Event) -> Touched {
        if !self.applied.insert(event.id) {
            return Touched::Nothing;
        }
        match &event.op {
            Op::AssetSeen {
                asset,
                volume,
                path,
                size,
                mtime_ms,
            } => {
                let candidate = InstanceInfo {
                    hlc: event.hlc.clone(),
                    size: *size,
                    mtime_ms: *mtime_ms,
                };
                self.apply_asset_seen(asset, volume, path, candidate);
                Touched::Asset(asset.clone())
            }
            Op::TagAdd { asset, tag } => {
                self.apply_tag_add(event.id, asset, tag);
                Touched::Asset(asset.clone())
            }
            Op::TagRemove {
                asset, observed, ..
            } => {
                self.apply_tag_remove(asset, observed);
                Touched::Asset(asset.clone())
            }
            Op::FieldSet {
                asset,
                field,
                value,
            } => {
                self.apply_field_set(asset, event.hlc.clone(), field, value);
                Touched::Asset(asset.clone())
            }
            Op::VolumeSeen { volume, label } => {
                let st = self.volumes.entry(volume.clone()).or_default();
                Self::lww(&mut st.seen, event.hlc.clone(), label.clone());
                Touched::Volume(volume.clone())
            }
            Op::ParaNodeCreate { node, kind, name } => {
                self.apply_para_create(node, *kind, event.hlc.clone(), name);
                Touched::ParaNode(node.clone())
            }
            Op::ParaNodeRename { node, name } => {
                self.apply_para_rename(node, event.hlc.clone(), name);
                Touched::ParaNode(node.clone())
            }
            Op::ParaNodeArchive { node } => {
                self.para_nodes.entry(node.clone()).or_default().archived = true;
                Touched::ParaNode(node.clone())
            }
            Op::AssetParaSet { asset, node } => {
                let st = self.assets.entry(asset.clone()).or_default();
                Self::lww(&mut st.para, event.hlc.clone(), node.clone());
                Touched::Asset(asset.clone())
            }
            Op::VerificationRecorded { asset, .. } => {
                self.insert_verification(asset, VerificationRecord::from_op(&event.op));
                Touched::Asset(asset.clone())
            }
            Op::ManifestRecorded { volume, .. } => {
                self.insert_manifest(volume, ManifestRecord::from_op(&event.op));
                Touched::Manifests(volume.clone())
            }
            Op::SavedSearchSet { name, query } => {
                self.apply_saved_search(name, event.hlc.clone(), Some(query.clone()));
                Touched::SavedSearch(name.clone())
            }
            Op::SavedSearchRemove { name } => {
                self.apply_saved_search(name, event.hlc.clone(), None);
                Touched::SavedSearch(name.clone())
            }
        }
    }

    /// HLC-LWW upsert of one (volume, path) instance: the newer `(hlc, size,
    /// mtime_ms)` tuple wins, so a rescan of the same instance updates it in
    /// place instead of duplicating it, regardless of apply order.
    fn apply_asset_seen(
        &mut self,
        asset: &AssetId,
        volume: &str,
        path: &str,
        candidate: InstanceInfo,
    ) {
        let st = self.assets.entry(asset.clone()).or_default();
        match st.instances.entry((volume.to_string(), path.to_string())) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                if candidate > *slot.get() {
                    slot.insert(candidate);
                }
            }
        }
    }

    fn apply_tag_add(&mut self, id: EventId, asset: &AssetId, tag: &str) {
        let st = self.assets.entry(asset.clone()).or_default();
        // Guards against a remove that arrives before this add: if the id is
        // already tombstoned, the add must not resurrect it.
        if !st.removed_adds.contains(&id) {
            st.tag_adds.entry(tag.to_string()).or_default().insert(id);
        }
    }

    fn apply_tag_remove(&mut self, asset: &AssetId, observed: &[EventId]) {
        let st = self.assets.entry(asset.clone()).or_default();
        for add_id in observed {
            st.removed_adds.insert(*add_id);
        }
        // Evicts an observed id from every tag's live set, not just the tag
        // named on this event: if the add already applied under a different
        // tag (a malformed or adversarial remove), it must still be evicted
        // so the result stays independent of delivery order.
        st.tag_adds.retain(|_, ids| {
            for add_id in observed {
                ids.remove(add_id);
            }
            !ids.is_empty()
        });
    }

    fn apply_field_set(&mut self, asset: &AssetId, hlc: Hlc, field: &str, value: &str) {
        let st = self.assets.entry(asset.clone()).or_default();
        let candidate = (hlc, value.to_string());
        match st.fields.get(field) {
            Some(current) if *current >= candidate => {}
            _ => {
                st.fields.insert(field.to_string(), candidate);
            }
        }
    }

    /// HLC-LWW slot update: the higher `(hlc, value)` tuple wins, matching
    /// the ordering every LWW field in this projection uses.
    fn lww<T: Ord>(slot: &mut Option<(Hlc, T)>, hlc: Hlc, value: T) {
        let candidate = (hlc, value);
        match slot {
            Some(current) if *current >= candidate => {}
            _ => *slot = Some(candidate),
        }
    }

    /// `kind` is meant to be set once in legitimate histories (node ids are
    /// minted once), but is folded as LWW — like `name` — so that two
    /// concurrent creates for the same node still converge regardless of
    /// apply order.
    fn apply_para_create(&mut self, node: &str, kind: ParaKind, hlc: Hlc, name: &str) {
        let st = self.para_nodes.entry(node.to_string()).or_default();
        Self::lww(&mut st.kind, hlc.clone(), kind);
        Self::lww(&mut st.name, hlc, name.to_string());
    }

    fn apply_para_rename(&mut self, node: &str, hlc: Hlc, name: &str) {
        let st = self.para_nodes.entry(node.to_string()).or_default();
        Self::lww(&mut st.name, hlc, name.to_string());
    }

    /// Grow-only insert: a verification is a plain fact, so this is
    /// commutative and idempotent by construction.
    fn insert_verification(&mut self, asset: &AssetId, record: VerificationRecord) {
        self.assets
            .entry(asset.clone())
            .or_default()
            .verifications
            .insert(record);
    }

    /// HLC-LWW upsert of a saved search slot: `query` is `Some` for a Set,
    /// `None` for a Remove tombstone — either way the higher `(hlc, query)`
    /// tuple wins, so a later Set revives a name a Remove tombstoned.
    fn apply_saved_search(&mut self, name: &str, hlc: Hlc, query: Option<String>) {
        let candidate = (hlc, query);
        match self.saved_searches.get(name) {
            Some(current) if *current >= candidate => {}
            _ => {
                self.saved_searches.insert(name.to_string(), candidate);
            }
        }
    }

    /// Grow-only insert: a manifest generation is a plain fact, so this is
    /// commutative and idempotent by construction.
    fn insert_manifest(&mut self, volume: &str, record: ManifestRecord) {
        self.manifests
            .entry(volume.to_string())
            .or_default()
            .insert(record);
    }

    #[must_use]
    pub fn tags(&self, asset: &AssetId) -> BTreeSet<String> {
        self.assets
            .get(asset)
            .map(|s| s.tag_adds.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Live add-event ids for a tag — what a remove must cite as observed.
    #[must_use]
    pub fn tag_add_ids(&self, asset: &AssetId, tag: &str) -> Vec<EventId> {
        self.assets
            .get(asset)
            .and_then(|s| s.tag_adds.get(tag))
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn field<'a>(&'a self, asset: &AssetId, field: &str) -> Option<&'a str> {
        self.assets
            .get(asset)?
            .fields
            .get(field)
            .map(|(_, v)| v.as_str())
    }

    /// Every field name/value pair currently set for `asset` (the LWW
    /// winners), in field-name order.
    pub fn fields<'a>(&'a self, asset: &AssetId) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.assets
            .get(asset)
            .into_iter()
            .flat_map(|s| s.fields.iter().map(|(k, (_, v))| (k.as_str(), v.as_str())))
    }

    /// True when `asset` has at least one physical observation
    /// (`AssetSeen`) on record — i.e. it was actually scanned, not merely
    /// referenced by a tag or field mutation.
    #[must_use]
    pub fn has_instances(&self, asset: &AssetId) -> bool {
        self.assets
            .get(asset)
            .is_some_and(|s| !s.instances.is_empty())
    }

    pub fn assets(&self) -> impl Iterator<Item = (&AssetId, &AssetState)> {
        self.assets.iter()
    }

    pub fn volumes(&self) -> impl Iterator<Item = (&String, &VolumeState)> {
        self.volumes.iter()
    }

    #[must_use]
    pub fn para_node(&self, node: &str) -> Option<&ParaNodeState> {
        self.para_nodes.get(node)
    }

    pub fn para_nodes(&self) -> impl Iterator<Item = (&String, &ParaNodeState)> {
        self.para_nodes.iter()
    }

    /// The asset's current PARA node id (LWW winner).
    #[must_use]
    pub fn asset_para<'a>(&'a self, asset: &AssetId) -> Option<&'a str> {
        self.assets
            .get(asset)?
            .para
            .as_ref()
            .map(|(_, n)| n.as_str())
    }

    pub fn verifications<'a>(
        &'a self,
        asset: &AssetId,
    ) -> impl Iterator<Item = &'a VerificationRecord> {
        self.assets
            .get(asset)
            .into_iter()
            .flat_map(|s| s.verifications.iter())
    }

    pub fn manifests<'a>(&'a self, volume: &str) -> impl Iterator<Item = &'a ManifestRecord> {
        self.manifests
            .get(volume)
            .into_iter()
            .flat_map(|s| s.iter())
    }

    /// Every recorded manifest generation, across every volume, paired with
    /// its volume id.
    pub fn all_manifests(&self) -> impl Iterator<Item = (&String, &ManifestRecord)> {
        self.manifests
            .iter()
            .flat_map(|(volume, records)| records.iter().map(move |r| (volume, r)))
    }

    /// Live saved searches (tombstones excluded), name-ordered.
    pub fn saved_searches(&self) -> impl Iterator<Item = (&str, &str)> {
        self.saved_searches
            .iter()
            .filter_map(|(name, (_, query))| query.as_deref().map(|q| (name.as_str(), q)))
    }

    /// The current query for `name`, or `None` if never set or tombstoned.
    #[must_use]
    pub fn saved_search(&self, name: &str) -> Option<&str> {
        self.saved_searches
            .get(name)
            .and_then(|(_, q)| q.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};

    fn ev(n: u128, wall: u64, machine: &str, op: Op) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(wall, n)),
            hlc: Hlc {
                wall_ms: wall,
                counter: 0,
                machine: MachineId(machine.into()),
            },
            author: machine.into(),
            op,
        }
    }
    fn asset() -> AssetId {
        AssetId("xxh3:aa".into())
    }

    fn test_event(n: u64, op: Op) -> Event {
        Event {
            id: EventId(ulid::Ulid::from_parts(1, n.into())),
            hlc: Hlc {
                wall_ms: n,
                counter: 0,
                machine: MachineId("m1".into()),
            },
            author: "t".into(),
            op,
        }
    }

    /// One op of every current `Op` variant, values borrowed from the golden
    /// wire-format tests in `event.rs`, paired with the `Touched` value
    /// `apply_tracking` must report for it — so both the serde round-trip
    /// and the touched-entity mapping are pinned against the same list.
    /// This list is the project's op-variant absence assertion: phase 5
    /// (describers) added no new `Op` variants (a describer-generated tag
    /// suggestion emits a plain `TagAdd`), phase 6 (sync + inbox) adds
    /// none either — sync moves existing segment/blob files without
    /// minting events, and `maj inbox process` re-emits the same
    /// pre-existing ops the verified-ingest pipeline already produces — and
    /// phase 7A (services extraction + `maj mcp`) adds none either: the
    /// services crate and the MCP server call the same verbs the CLI
    /// already called, and expose no new mutation. If a future phase adds
    /// a variant, it must be added here too.
    ///
    /// Split across two functions purely to stay under the crate's
    /// max-function-length lint; the two lists together are one logical
    /// sample set.
    fn sample_ops() -> Vec<(Op, Touched)> {
        let mut ops = sample_ops_facts();
        ops.extend(sample_ops_saved_search());
        ops
    }

    fn sample_ops_facts() -> Vec<(Op, Touched)> {
        vec![
            (
                Op::AssetSeen {
                    asset: asset(),
                    volume: "uuid:abc".into(),
                    path: "clips/a.mov".into(),
                    size: 4,
                    mtime_ms: 5,
                },
                Touched::Asset(asset()),
            ),
            (
                Op::VolumeSeen {
                    volume: "uuid:abc".into(),
                    label: "card1".into(),
                },
                Touched::Volume("uuid:abc".into()),
            ),
            (
                Op::TagAdd {
                    asset: asset(),
                    tag: "person/dana".into(),
                },
                Touched::Asset(asset()),
            ),
            (
                Op::TagRemove {
                    asset: asset(),
                    tag: "t".into(),
                    observed: vec![EventId(ulid::Ulid::from_parts(1, 2))],
                },
                Touched::Asset(asset()),
            ),
            (
                Op::FieldSet {
                    asset: asset(),
                    field: "rating".into(),
                    value: "5".into(),
                },
                Touched::Asset(asset()),
            ),
            (
                Op::ParaNodeCreate {
                    node: "00000000010000000000000002".into(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                Touched::ParaNode("00000000010000000000000002".into()),
            ),
            (
                Op::ParaNodeRename {
                    node: "00000000010000000000000002".into(),
                    name: "client-y".into(),
                },
                Touched::ParaNode("00000000010000000000000002".into()),
            ),
            (
                Op::ParaNodeArchive {
                    node: "00000000010000000000000002".into(),
                },
                Touched::ParaNode("00000000010000000000000002".into()),
            ),
            (
                Op::AssetParaSet {
                    asset: asset(),
                    node: "00000000010000000000000002".into(),
                },
                Touched::Asset(asset()),
            ),
            (
                Op::VerificationRecorded {
                    asset: asset(),
                    volume: "uuid:abc".into(),
                    path: "clips/a.mov".into(),
                    algo: "xxh64".into(),
                    value: "0011223344556677".into(),
                    outcome: VerifyOutcome::Verified,
                    hashdate_ms: 42,
                },
                Touched::Asset(asset()),
            ),
            (
                Op::ManifestRecorded {
                    volume: "uuid:abc".into(),
                    mhl_path: "ascmhl/0001_dest_2026-07-29_120000.mhl".into(),
                    generation: 1,
                    roothash: "xxh64:8899aabbccddeeff".into(),
                },
                Touched::Manifests("uuid:abc".into()),
            ),
        ]
    }

    fn sample_ops_saved_search() -> Vec<(Op, Touched)> {
        vec![
            (
                Op::SavedSearchSet {
                    name: "n1".into(),
                    query: "tag:x sunset".into(),
                },
                Touched::SavedSearch("n1".into()),
            ),
            (
                Op::SavedSearchRemove { name: "n1".into() },
                Touched::SavedSearch("n1".into()),
            ),
        ]
    }

    #[test]
    fn apply_tracking_reports_the_touched_entity() {
        let mut p = Projection::default();
        let e = test_event(
            1,
            Op::VolumeSeen {
                volume: "v1".into(),
                label: "V".into(),
            },
        );
        assert_eq!(p.apply_tracking(&e), Touched::Volume("v1".into()));
        let e2 = test_event(
            2,
            Op::TagAdd {
                asset: AssetId("xxh3:a".into()),
                tag: "t".into(),
            },
        );
        assert_eq!(
            p.apply_tracking(&e2),
            Touched::Asset(AssetId("xxh3:a".into()))
        );
    }

    #[test]
    fn reapplying_an_event_touches_nothing() {
        let mut p = Projection::default();
        let e = test_event(
            1,
            Op::VolumeSeen {
                volume: "v1".into(),
                label: "V".into(),
            },
        );
        assert_ne!(p.apply_tracking(&e), Touched::Nothing);
        assert_eq!(p.apply_tracking(&e), Touched::Nothing);
    }

    #[test]
    fn projection_round_trips_through_serde_json() {
        let mut p = Projection::default();
        for (n, (op, _)) in sample_ops().into_iter().enumerate() {
            let n = u64::try_from(n).unwrap_or(0) + 1;
            p.apply(&test_event(n, op));
        }
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Projection = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    /// `projection_round_trips_through_serde_json` only ever exercises the
    /// `instance_map` serde adapter with exactly one entry (via
    /// `sample_ops`'s single `AssetSeen`). This pins the adapter at both
    /// remaining shapes it must handle: zero entries (an asset with metadata
    /// but no physical observation) and multiple entries (two distinct
    /// (volume, path) instances on one asset).
    #[test]
    fn instances_round_trip_through_serde_json_when_empty_and_when_multi_entry() {
        let mut empty = Projection::default();
        let a = AssetId("xxh3:a".into());
        empty.apply(&test_event(
            1,
            Op::TagAdd {
                asset: a.clone(),
                tag: "t".into(),
            },
        ));
        let json = serde_json::to_string(&empty).expect("serialize empty instances");
        let back: Projection = serde_json::from_str(&json).expect("deserialize empty instances");
        assert_eq!(empty, back);
        assert!(
            back.assets().next().expect("asset").1.instances.is_empty(),
            "no AssetSeen observed, so instances must round-trip empty"
        );

        let mut multi = Projection::default();
        multi.apply(&test_event(
            1,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v1".into(),
                path: "a.mov".into(),
                size: 4,
                mtime_ms: 5,
            },
        ));
        multi.apply(&test_event(
            2,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v2".into(),
                path: "b.mov".into(),
                size: 8,
                mtime_ms: 9,
            },
        ));
        let json = serde_json::to_string(&multi).expect("serialize multi-entry instances");
        let back: Projection =
            serde_json::from_str(&json).expect("deserialize multi-entry instances");
        assert_eq!(multi, back);
        assert_eq!(
            back.assets().next().expect("asset").1.instances.len(),
            2,
            "two distinct (volume, path) instances must both round-trip"
        );
    }

    /// Pins the exact `Touched` value for one op of every variant — without
    /// this, a wrong mapping on an untested arm (e.g. `ManifestRecorded`
    /// reporting `Touched::Volume` instead of `Touched::Manifests`) would
    /// compile and silently corrupt the incremental-apply path that consumes
    /// `Touched`.
    #[test]
    fn apply_tracking_touches_the_correct_entity_for_every_op_variant() {
        let mut p = Projection::default();
        for (n, (op, expected)) in sample_ops().into_iter().enumerate() {
            let n = u64::try_from(n).unwrap_or(0) + 1;
            assert_eq!(p.apply_tracking(&test_event(n, op)), expected);
        }
    }

    #[test]
    fn concurrent_add_wins_over_remove() {
        let add1 = ev(
            1,
            1,
            "m1",
            Op::TagAdd {
                asset: asset(),
                tag: "keep".into(),
            },
        );
        let rm = ev(
            2,
            2,
            "m2",
            Op::TagRemove {
                asset: asset(),
                tag: "keep".into(),
                observed: vec![add1.id],
            },
        );
        let add2 = ev(
            3,
            2,
            "m1",
            Op::TagAdd {
                asset: asset(),
                tag: "keep".into(),
            },
        );
        let mut p = Projection::default();
        for e in [&add1, &rm, &add2] {
            p.apply(e);
        }
        assert!(
            p.tags(&asset()).contains("keep"),
            "unobserved add survives remove"
        );
    }

    #[test]
    fn remove_arriving_before_its_add_still_wins_over_that_add() {
        let add = ev(
            1,
            1,
            "m1",
            Op::TagAdd {
                asset: asset(),
                tag: "t".into(),
            },
        );
        let rm = ev(
            2,
            2,
            "m2",
            Op::TagRemove {
                asset: asset(),
                tag: "t".into(),
                observed: vec![add.id],
            },
        );
        let mut p = Projection::default();
        p.apply(&rm); // remove first
        p.apply(&add); // its add arrives late
        assert!(
            !p.tags(&asset()).contains("t"),
            "tombstoned add must stay dead"
        );
    }

    #[test]
    fn remove_citing_unknown_id_is_a_harmless_tombstone() {
        let unknown = EventId(ulid::Ulid::from_parts(9, 9));
        let add = ev(
            1,
            1,
            "m1",
            Op::TagAdd {
                asset: asset(),
                tag: "t".into(),
            },
        );
        let rm = ev(
            2,
            2,
            "m2",
            Op::TagRemove {
                asset: asset(),
                tag: "t".into(),
                observed: vec![unknown],
            },
        );
        let mut fwd = Projection::default();
        fwd.apply(&add);
        fwd.apply(&rm);
        let mut rev = Projection::default();
        rev.apply(&rm);
        rev.apply(&add);
        assert_eq!(fwd, rev);
        assert!(
            fwd.tags(&asset()).contains("t"),
            "add for an id never cited by any remove must survive"
        );
    }

    #[test]
    fn volume_label_and_last_seen_are_lww_and_order_independent() {
        // The later observation comes from "amy" — lexically smaller than
        // "bob" — so this discriminates a real (wall, counter) comparison
        // from a bug that picked the winner by machine-id tiebreak alone;
        // that mutation would keep bob's label and fail this test.
        let early = ev(
            1,
            1,
            "bob",
            Op::VolumeSeen {
                volume: "V1".into(),
                label: "card-a".into(),
            },
        );
        let late = ev(
            2,
            2,
            "amy",
            Op::VolumeSeen {
                volume: "V1".into(),
                label: "card-a-renamed".into(),
            },
        );
        let mut fwd = Projection::default();
        fwd.apply(&early);
        fwd.apply(&late);
        let mut rev = Projection::default();
        rev.apply(&late);
        rev.apply(&early);
        assert_eq!(fwd, rev);
        for p in [&fwd, &rev] {
            let (id, state) = p.volumes().next().expect("one volume");
            assert_eq!(id, "V1");
            assert_eq!(state.label(), Some("card-a-renamed"));
            assert_eq!(state.last_seen(), Some(&late.hlc));
        }
    }

    #[test]
    fn apply_is_idempotent_and_order_independent() {
        let a = asset();
        let add = ev(
            1,
            1,
            "m1",
            Op::TagAdd {
                asset: a.clone(),
                tag: "t".into(),
            },
        );
        let events = vec![
            add.clone(),
            ev(
                2,
                2,
                "m2",
                Op::TagRemove {
                    asset: a.clone(),
                    tag: "t".into(),
                    observed: vec![add.id],
                },
            ),
            ev(
                3,
                3,
                "m1",
                Op::FieldSet {
                    asset: a.clone(),
                    field: "rating".into(),
                    value: "5".into(),
                },
            ),
            ev(
                4,
                1,
                "m2",
                Op::FieldSet {
                    asset: a.clone(),
                    field: "rating".into(),
                    value: "2".into(),
                },
            ),
        ];
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in &events {
            fwd.apply(e);
            fwd.apply(e);
        }
        for e in events.iter().rev() {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        assert_eq!(fwd.field(&a, "rating"), Some("5"));
        assert!(!fwd.tags(&a).contains("t"));
    }

    #[test]
    fn fields_lists_every_field_set_on_an_asset() {
        let a = asset();
        let mut p = Projection::default();
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::FieldSet {
                asset: a.clone(),
                field: "rating".into(),
                value: "5".into(),
            },
        ));
        p.apply(&ev(
            2,
            2,
            "m1",
            Op::FieldSet {
                asset: a.clone(),
                field: "title".into(),
                value: "Sunset".into(),
            },
        ));
        let mut fields: Vec<(&str, &str)> = p.fields(&a).collect();
        fields.sort_unstable();
        assert_eq!(fields, vec![("rating", "5"), ("title", "Sunset")]);
        assert!(p.fields(&AssetId("xxh3:unknown".into())).next().is_none());
    }

    #[test]
    fn a_rescan_of_the_same_path_updates_in_place_instead_of_duplicating() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:a".into());
        p.apply(&test_event(
            1,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 3,
                mtime_ms: 10,
            },
        ));
        p.apply(&test_event(
            2,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 9,
                mtime_ms: 20,
            },
        ));
        let state = p.assets().find(|(id, _)| **id == a).expect("asset").1;
        assert_eq!(
            state.instances.len(),
            1,
            "same (volume, path) must not duplicate"
        );
        let info = state.instances.values().next().expect("instance");
        assert_eq!((info.size, info.mtime_ms), (9, 20), "newer HLC wins");
    }

    #[test]
    fn instance_lww_is_hlc_ordered_not_arrival_ordered() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:a".into());
        p.apply(&test_event(
            2,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 9,
                mtime_ms: 20,
            },
        ));
        p.apply(&test_event(
            1,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 3,
                mtime_ms: 10,
            },
        ));
        let state = p.assets().find(|(id, _)| **id == a).expect("asset").1;
        let info = state.instances.values().next().expect("instance");
        assert_eq!((info.size, info.mtime_ms), (9, 20));
    }

    /// The other two LWW tests both pick payloads where `size` happens to
    /// rank the same way as the HLC, so a mutation reordering
    /// `InstanceInfo`'s fields to `(size, mtime_ms, hlc)` — making the
    /// derived `Ord` compare size first — would still pass them. Here the
    /// later event carries the smaller size, so only a genuine HLC-first
    /// comparison picks it.
    #[test]
    fn newer_hlc_wins_even_when_size_is_smaller() {
        let mut p = Projection::default();
        let a = AssetId("xxh3:a".into());
        p.apply(&test_event(
            1,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 9,
                mtime_ms: 99,
            },
        ));
        p.apply(&test_event(
            2,
            Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "p".into(),
                size: 1,
                mtime_ms: 1,
            },
        ));
        let state = p.assets().find(|(id, _)| **id == a).expect("asset").1;
        let info = state.instances.values().next().expect("instance");
        assert_eq!((info.size, info.mtime_ms), (1, 1), "HLC must dominate size");
    }

    #[test]
    fn has_instances_requires_an_asset_seen_observation() {
        let a = asset();
        let mut p = Projection::default();
        assert!(!p.has_instances(&a), "unscanned asset has no instances");
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::TagAdd {
                asset: a.clone(),
                tag: "t".into(),
            },
        ));
        assert!(
            !p.has_instances(&a),
            "a tag alone must not count as an instance"
        );
        p.apply(&ev(
            2,
            2,
            "m1",
            Op::AssetSeen {
                asset: a.clone(),
                volume: "V1".into(),
                path: "a.mov".into(),
                size: 1,
                mtime_ms: 0,
            },
        ));
        assert!(p.has_instances(&a));
    }

    #[test]
    fn para_node_create_rename_archive_are_lww_and_order_independent() {
        let node = "N1".to_string();
        let create = ev(
            1,
            1,
            "bob",
            Op::ParaNodeCreate {
                node: node.clone(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            },
        );
        // Later rename from lexically-smaller machine: discriminates real
        // (wall, counter) LWW from a machine-id tiebreak bug (the same
        // confound the volume-label test guards).
        let rename = ev(
            2,
            2,
            "amy",
            Op::ParaNodeRename {
                node: node.clone(),
                name: "client-y".into(),
            },
        );
        let archive = ev(3, 3, "bob", Op::ParaNodeArchive { node: node.clone() });
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in [&create, &rename, &archive] {
            fwd.apply(e);
        }
        for e in [&archive, &rename, &create] {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        let st = fwd.para_node(&node).expect("node exists");
        assert_eq!(st.kind(), Some(ParaKind::Project));
        assert_eq!(st.name(), Some("client-y"));
        assert!(st.archived());
    }

    #[test]
    fn stale_rename_loses_to_newer_name() {
        let node = "N1".to_string();
        let create = ev(
            1,
            5,
            "m1",
            Op::ParaNodeCreate {
                node: node.clone(),
                kind: ParaKind::Area,
                name: "newer".into(),
            },
        );
        let stale = ev(
            2,
            1,
            "m2",
            Op::ParaNodeRename {
                node: node.clone(),
                name: "older".into(),
            },
        );
        let mut p = Projection::default();
        p.apply(&create);
        p.apply(&stale);
        assert_eq!(p.para_node(&node).expect("node").name(), Some("newer"));
    }

    #[test]
    fn asset_para_assignment_is_lww() {
        let a = asset();
        let first = ev(
            1,
            1,
            "m1",
            Op::AssetParaSet {
                asset: a.clone(),
                node: "N1".into(),
            },
        );
        let second = ev(
            2,
            2,
            "m2",
            Op::AssetParaSet {
                asset: a.clone(),
                node: "N2".into(),
            },
        );
        let mut fwd = Projection::default();
        fwd.apply(&first);
        fwd.apply(&second);
        let mut rev = Projection::default();
        rev.apply(&second);
        rev.apply(&first);
        assert_eq!(fwd, rev);
        assert_eq!(fwd.asset_para(&a), Some("N2"));
    }

    /// `para_node_create_rename_archive_are_lww_and_order_independent` only
    /// ever checks `archived()` after an explicit archive op — no test
    /// confirms a freshly created node starts non-archived, so a mutation
    /// hardcoding `archived()` to always return `true` survives unnoticed.
    #[test]
    fn a_freshly_created_node_is_not_archived() {
        let node = "N1".to_string();
        let create = ev(
            1,
            1,
            "m1",
            Op::ParaNodeCreate {
                node: node.clone(),
                kind: ParaKind::Project,
                name: "client-x".into(),
            },
        );
        let mut p = Projection::default();
        p.apply(&create);
        assert!(!p.para_node(&node).expect("node").archived());
    }

    /// `Projection::assets`/`para_nodes`/`all_manifests` are never called
    /// anywhere else in this crate (only by downstream crates, which
    /// `cargo mutants -p majestical-core` doesn't exercise) — each is
    /// covered here directly so a mutation replacing any of them with an
    /// empty iterator is caught within this crate's own test suite.
    #[test]
    fn assets_lists_every_asset_with_a_recorded_instance() {
        let a = asset();
        let mut p = Projection::default();
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::AssetSeen {
                asset: a.clone(),
                volume: "V1".into(),
                path: "a.mov".into(),
                size: 4,
                mtime_ms: 0,
            },
        ));
        let ids: Vec<&AssetId> = p.assets().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![&a]);
    }

    #[test]
    fn para_nodes_lists_every_created_node() {
        let mut p = Projection::default();
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::ParaNodeCreate {
                node: "N1".into(),
                kind: ParaKind::Area,
                name: "client-x".into(),
            },
        ));
        let ids: Vec<&String> = p.para_nodes().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![&"N1".to_string()]);
    }

    #[test]
    fn all_manifests_lists_every_recorded_generation_across_volumes() {
        let mut p = Projection::default();
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::ManifestRecorded {
                volume: "V1".into(),
                mhl_path: "ascmhl/0001_x_2026-07-30_000000Z.mhl".into(),
                generation: 1,
                roothash: "c4xxx".into(),
            },
        ));
        let all: Vec<(&String, &ManifestRecord)> = p.all_manifests().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "V1");
        assert_eq!(all[0].1.generation, 1);
    }

    #[test]
    fn verifications_and_manifests_accumulate_as_sets() {
        let a = asset();
        let v1 = ev(
            1,
            1,
            "m1",
            Op::VerificationRecorded {
                asset: a.clone(),
                volume: "V1".into(),
                path: "a.mov".into(),
                algo: "xxh64".into(),
                value: "00".into(),
                outcome: VerifyOutcome::Original,
                hashdate_ms: 1,
            },
        );
        let v2 = ev(
            2,
            2,
            "m1",
            Op::VerificationRecorded {
                asset: a.clone(),
                volume: "V1".into(),
                path: "a.mov".into(),
                algo: "xxh64".into(),
                value: "00".into(),
                outcome: VerifyOutcome::Verified,
                hashdate_ms: 2,
            },
        );
        let m = ev(
            3,
            3,
            "m1",
            Op::ManifestRecorded {
                volume: "V1".into(),
                mhl_path: "ascmhl/0001_d_x.mhl".into(),
                generation: 1,
                roothash: "xxh64:aa".into(),
            },
        );
        let mut fwd = Projection::default();
        let mut rev = Projection::default();
        for e in [&v1, &v2, &m] {
            fwd.apply(e);
        }
        for e in [&m, &v2, &v1] {
            rev.apply(e);
        }
        assert_eq!(fwd, rev);
        assert_eq!(fwd.verifications(&a).count(), 2);
        assert_eq!(fwd.manifests("V1").count(), 1);
    }

    /// Every field of `VerificationRecord`/`ManifestRecord` gets a distinct
    /// value here, so a field-mapping swap inside `from_op` (e.g. `algo` and
    /// `value` transposed, or `mhl_path` and `roothash` transposed) fails
    /// this test — a bug otherwise invisible to `majestical-core` itself,
    /// only ever caught by accident downstream in catalog-sqlite's column
    /// assertions.
    #[test]
    fn verification_and_manifest_records_map_every_field_correctly() {
        let a = asset();
        let mut p = Projection::default();
        p.apply(&ev(
            1,
            1,
            "m1",
            Op::VerificationRecorded {
                asset: a.clone(),
                volume: "vol-v".into(),
                path: "path-p".into(),
                algo: "algo-g".into(),
                value: "value-l".into(),
                outcome: VerifyOutcome::Failed,
                hashdate_ms: 111,
            },
        ));
        p.apply(&ev(
            2,
            2,
            "m1",
            Op::ManifestRecorded {
                volume: "vol-m".into(),
                mhl_path: "mhl-path".into(),
                generation: 7,
                roothash: "roothash-r".into(),
            },
        ));

        let v = p.verifications(&a).next().expect("verification recorded");
        assert_eq!(v.volume, "vol-v");
        assert_eq!(v.path, "path-p");
        assert_eq!(v.algo, "algo-g");
        assert_eq!(v.value, "value-l");
        assert_eq!(v.outcome, VerifyOutcome::Failed);
        assert_eq!(v.hashdate_ms, 111);

        let m = p.manifests("vol-m").next().expect("manifest recorded");
        assert_eq!(m.generation, 7);
        assert_eq!(m.mhl_path, "mhl-path");
        assert_eq!(m.roothash, "roothash-r");
    }

    #[test]
    fn saved_search_set_remove_is_lww_per_name() {
        let mut p = Projection::default();
        p.apply(&test_event(
            1,
            Op::SavedSearchSet {
                name: "picks".into(),
                query: "tag:a".into(),
            },
        ));
        p.apply(&test_event(
            3,
            Op::SavedSearchSet {
                name: "picks".into(),
                query: "tag:b".into(),
            },
        ));
        p.apply(&test_event(
            2,
            Op::SavedSearchRemove {
                name: "picks".into(),
            },
        ));
        assert_eq!(
            p.saved_search("picks"),
            Some("tag:b"),
            "later set beats earlier remove"
        );
        p.apply(&test_event(
            4,
            Op::SavedSearchRemove {
                name: "picks".into(),
            },
        ));
        assert_eq!(p.saved_search("picks"), None);
        assert_eq!(p.saved_searches().count(), 0);
    }
}
