//! In-memory CRDT projection of an event set. Apply is commutative and
//! idempotent: tombstoned add-ids are remembered so a remove arriving
//! before its add still wins over exactly that add and nothing else.
use crate::clock::Hlc;
use crate::event::{AssetId, Event, EventId, Op, ParaKind, VerifyOutcome};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AssetState {
    /// (volume, path, size) instances observed for this content hash.
    pub instances: BTreeSet<(String, String, u64)>,
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

/// One verification observation; a plain fact, deduped by full value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerificationRecord {
    pub volume: String,
    pub path: String,
    pub algo: String,
    pub value: String,
    pub outcome: VerifyOutcome,
    pub hashdate_ms: u64,
}

/// One recorded ASC MHL generation; a plain fact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ManifestRecord {
    pub generation: u32,
    pub mhl_path: String,
    pub roothash: String,
}

/// PARA node folded state. Kind is immutable (node ids are minted once);
/// name is LWW across create+rename; archived is monotonic.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParaNodeState {
    kind: Option<ParaKind>,
    name: Option<(Hlc, String)>,
    archived: bool,
}

impl ParaNodeState {
    #[must_use]
    pub fn kind(&self) -> Option<ParaKind> {
        self.kind
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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Projection {
    assets: BTreeMap<AssetId, AssetState>,
    volumes: BTreeMap<String, VolumeState>,
    applied: BTreeSet<EventId>,
    para_nodes: BTreeMap<String, ParaNodeState>,
    /// volume id -> recorded manifest generations.
    manifests: BTreeMap<String, BTreeSet<ManifestRecord>>,
}

impl Projection {
    pub fn apply(&mut self, event: &Event) {
        if !self.applied.insert(event.id) {
            return;
        }
        match &event.op {
            Op::AssetSeen {
                asset,
                volume,
                path,
                size,
            } => self.apply_asset_seen(asset, volume, path, *size),
            Op::TagAdd { asset, tag } => self.apply_tag_add(event.id, asset, tag),
            Op::TagRemove {
                asset, observed, ..
            } => self.apply_tag_remove(asset, observed),
            Op::FieldSet {
                asset,
                field,
                value,
            } => self.apply_field_set(asset, event.hlc.clone(), field, value),
            Op::VolumeSeen { volume, label } => {
                let st = self.volumes.entry(volume.clone()).or_default();
                Self::lww(&mut st.seen, event.hlc.clone(), label.clone());
            }
            Op::ParaNodeCreate { node, kind, name } => {
                self.apply_para_create(node, *kind, event.hlc.clone(), name);
            }
            Op::ParaNodeRename { node, name } => {
                self.apply_para_rename(node, event.hlc.clone(), name);
            }
            Op::ParaNodeArchive { node } => {
                self.para_nodes.entry(node.clone()).or_default().archived = true;
            }
            Op::AssetParaSet { asset, node } => {
                let st = self.assets.entry(asset.clone()).or_default();
                Self::lww(&mut st.para, event.hlc.clone(), node.clone());
            }
            Op::VerificationRecorded {
                asset,
                volume,
                path,
                algo,
                value,
                outcome,
                hashdate_ms,
            } => self.insert_verification(
                asset,
                VerificationRecord {
                    volume: volume.clone(),
                    path: path.clone(),
                    algo: algo.clone(),
                    value: value.clone(),
                    outcome: *outcome,
                    hashdate_ms: *hashdate_ms,
                },
            ),
            Op::ManifestRecorded {
                volume,
                mhl_path,
                generation,
                roothash,
            } => self.insert_manifest(
                volume,
                ManifestRecord {
                    generation: *generation,
                    mhl_path: mhl_path.clone(),
                    roothash: roothash.clone(),
                },
            ),
        }
    }

    fn apply_asset_seen(&mut self, asset: &AssetId, volume: &str, path: &str, size: u64) {
        self.assets
            .entry(asset.clone())
            .or_default()
            .instances
            .insert((volume.to_string(), path.to_string(), size));
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
    fn lww(slot: &mut Option<(Hlc, String)>, hlc: Hlc, value: String) {
        let candidate = (hlc, value);
        match slot {
            Some(current) if *current >= candidate => {}
            _ => *slot = Some(candidate),
        }
    }

    /// `kind` is set at most once (node ids are minted once, so kind is
    /// immutable); `name` follows the same LWW rule as `ParaNodeRename`.
    fn apply_para_create(&mut self, node: &str, kind: ParaKind, hlc: Hlc, name: &str) {
        let st = self.para_nodes.entry(node.to_string()).or_default();
        st.kind.get_or_insert(kind);
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
}
