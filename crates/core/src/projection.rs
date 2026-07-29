//! In-memory CRDT projection of an event set. Apply is commutative and
//! idempotent: tombstoned add-ids are remembered so a remove arriving
//! before its add still wins over exactly that add and nothing else.
use crate::clock::Hlc;
use crate::event::{AssetId, Event, EventId, Op};
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
            } => {
                self.assets
                    .entry(asset.clone())
                    .or_default()
                    .instances
                    .insert((volume.clone(), path.clone(), *size));
            }
            Op::TagAdd { asset, tag } => {
                let st = self.assets.entry(asset.clone()).or_default();
                // Guards against a remove that arrives before this add: if the
                // id is already tombstoned, the add must not resurrect it.
                if !st.removed_adds.contains(&event.id) {
                    st.tag_adds.entry(tag.clone()).or_default().insert(event.id);
                }
            }
            Op::TagRemove {
                asset,
                tag: _,
                observed,
            } => {
                let st = self.assets.entry(asset.clone()).or_default();
                for add_id in observed {
                    st.removed_adds.insert(*add_id);
                }
                // Evicts an observed id from every tag's live set, not just
                // the tag named on this event: if the add already applied
                // under a different tag (a malformed or adversarial remove),
                // it must still be evicted so the result stays independent
                // of delivery order.
                st.tag_adds.retain(|_, ids| {
                    for add_id in observed {
                        ids.remove(add_id);
                    }
                    !ids.is_empty()
                });
            }
            Op::FieldSet {
                asset,
                field,
                value,
            } => {
                let st = self.assets.entry(asset.clone()).or_default();
                let candidate = (event.hlc.clone(), value.clone());
                match st.fields.get(field) {
                    Some(current) if *current >= candidate => {}
                    _ => {
                        st.fields.insert(field.clone(), candidate);
                    }
                }
            }
            Op::VolumeSeen { volume, label } => {
                let st = self.volumes.entry(volume.clone()).or_default();
                let candidate = (event.hlc.clone(), label.clone());
                match &st.seen {
                    Some(current) if *current >= candidate => {}
                    _ => st.seen = Some(candidate),
                }
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{Hlc, MachineId};
    use crate::event::{AssetId, Event, EventId, Op};

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
        let early = ev(
            1,
            1,
            "amy",
            Op::VolumeSeen {
                volume: "V1".into(),
                label: "card-a".into(),
            },
        );
        let late = ev(
            2,
            2,
            "bob",
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
}
