//! Read-side queries: name search, filter-driven asset lookup, presentation
//! summaries, and the small volume/PARA listing queries.
use crate::{CatalogError, SqliteCatalog};
use majestical_core::event::AssetId;
use majestical_core::ports::{AssetSummary, Filter};
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params_from_iter};
use std::collections::BTreeSet;

impl SqliteCatalog {
    /// Assets whose basename matches any of `terms` by word-prefix, ranked
    /// best-first (FTS5's `rank`), capped at `limit` rows — one row per
    /// asset, at its best-matching basename's rank, even when several of its
    /// instances have basenames that all match. Unicode-aware and
    /// case-insensitive via the `unicode61 remove_diacritics 2` tokenizer.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn search_names_ranked(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<(AssetId, f64)>, CatalogError> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        // Each term quoted (embedded quotes doubled) with a prefix star,
        // OR-joined: beach -> "beach"* — FTS5 string syntax, immune to
        // operator injection.
        let match_expr = terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        // GROUP BY asset + min(rank): an asset with several matching
        // basenames (e.g. beach.mov and beach_copy.mov) otherwise comes back
        // as one row per matching basename. bm25 ranks are negative with
        // more-negative meaning better, so min() picks the best match.
        let mut stmt = self.conn.prepare(
            "SELECT asset, min(rank) FROM names_fts WHERE names_fts MATCH ?1
             GROUP BY asset ORDER BY min(rank) LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            (&match_expr, i64::try_from(limit).unwrap_or(i64::MAX)),
            |r| Ok((AssetId(r.get(0)?), r.get::<_, f64>(1)?)),
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Assets satisfying every filter in `filters` (conjunction).
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn assets_matching(&self, filters: &[Filter]) -> Result<BTreeSet<AssetId>, CatalogError> {
        let mut sql = String::from("SELECT a.id FROM assets a WHERE 1=1 ");
        let mut params: Vec<Value> = Vec::new();
        for filter in filters {
            sql.push_str(&Self::filter_clause(filter, &mut params));
            sql.push(' ');
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(0))?;
        let mut out = BTreeSet::new();
        for row in rows {
            out.insert(AssetId(row?));
        }
        Ok(out)
    }

    /// Builds one filter's SQL clause (starting with `AND`), pushing its
    /// bind values onto `params` in the same left-to-right order the clause's
    /// `?` placeholders appear in — `SQLite` numbers unnamed placeholders
    /// sequentially by appearance, and `assets_matching` builds the full SQL
    /// strictly left-to-right, so appearance order and push order line up
    /// without either side tracking an index.
    fn filter_clause(filter: &Filter, params: &mut Vec<Value>) -> String {
        match filter {
            Filter::Tag { value, negated } => {
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM tags t \
                     WHERE t.asset = a.id AND t.tag = ?)"
                )
            }
            Filter::Volume { value, negated } => {
                params.push(Value::Text(value.clone()));
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM instances i \
                     LEFT JOIN volumes v ON v.id = i.volume \
                     WHERE i.asset = a.id AND (v.label = ? OR i.volume = ?))"
                )
            }
            Filter::Para { node, negated } => {
                params.push(Value::Text(node.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM asset_para ap \
                     WHERE ap.asset = a.id AND ap.node = ?)"
                )
            }
            Filter::Kind { value, negated } => {
                params.push(Value::Text(value.clone()));
                let not = if *negated { "NOT " } else { "" };
                format!(
                    "AND {not}EXISTS (SELECT 1 FROM instances i \
                     WHERE i.asset = a.id AND i.kind = ?)"
                )
            }
            Filter::Online { ids, want } => Self::online_clause(ids, *want, params),
            Filter::Before(ms) => {
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
                "AND EXISTS (SELECT 1 FROM instances i \
                 WHERE i.asset = a.id AND i.mtime_ms < ?)"
                    .to_string()
            }
            Filter::After(ms) => {
                params.push(Value::Integer(i64::try_from(*ms).unwrap_or(i64::MAX)));
                "AND EXISTS (SELECT 1 FROM instances i \
                 WHERE i.asset = a.id AND i.mtime_ms > ?)"
                    .to_string()
            }
        }
    }

    /// `Online` clause: empty `ids` short-circuits (no volume can ever be
    /// "one of zero mounted volumes"), non-empty binds one placeholder per
    /// id.
    fn online_clause(ids: &[String], want: bool, params: &mut Vec<Value>) -> String {
        if ids.is_empty() {
            return if want {
                "AND 0=1".to_string()
            } else {
                "AND EXISTS (SELECT 1 FROM instances i WHERE i.asset = a.id)".to_string()
            };
        }
        let placeholders: Vec<&str> = ids
            .iter()
            .map(|id| {
                params.push(Value::Text(id.clone()));
                "?"
            })
            .collect();
        let not = if want { "" } else { "NOT " };
        format!(
            "AND {not}EXISTS (SELECT 1 FROM instances i \
             WHERE i.asset = a.id AND i.volume IN ({}))",
            placeholders.join(", ")
        )
    }

    /// Presentation rows for exactly the given asset ids: name (the first
    /// instance's basename, ordered by volume then path), the distinct
    /// volumes holding an instance (label falling back to the volume id),
    /// tags in alphabetical order, and the current PARA assignment.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn asset_summaries(&self, ids: &[AssetId]) -> Result<Vec<AssetSummary>, CatalogError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(AssetSummary {
                asset: id.clone(),
                name: self.first_instance_name(id)?,
                volumes: self.instance_volumes(id)?,
                tags: self.query_asset_tags(id)?,
                para: self.query_asset_para(id)?,
            });
        }
        Ok(out)
    }

    fn first_instance_name(&self, id: &AssetId) -> Result<String, CatalogError> {
        let path: Option<String> = self
            .conn
            .prepare_cached(
                "SELECT path FROM instances WHERE asset = ?1 ORDER BY volume, path LIMIT 1",
            )?
            .query_row([&id.0], |r| r.get(0))
            .optional()?;
        Ok(path
            .and_then(|p| p.rsplit('/').next().map(str::to_string))
            .unwrap_or_default())
    }

    fn instance_volumes(&self, id: &AssetId) -> Result<Vec<(String, String)>, CatalogError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT i.volume, COALESCE(v.label, i.volume) FROM instances i \
             LEFT JOIN volumes v ON v.id = i.volume WHERE i.asset = ?1 ORDER BY i.volume",
        )?;
        let rows = stmt.query_map([&id.0], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn query_asset_tags(&self, id: &AssetId) -> Result<Vec<String>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT tag FROM tags WHERE asset = ?1 ORDER BY tag")?;
        let rows = stmt.query_map([&id.0], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn query_asset_para(&self, id: &AssetId) -> Result<Option<String>, CatalogError> {
        Ok(self
            .conn
            .prepare_cached("SELECT node FROM asset_para WHERE asset = ?1")?
            .query_row([&id.0], |r| r.get(0))
            .optional()?)
    }

    /// Every volume the catalog has ever seen: (id, label, `last_seen_ms`),
    /// ordered by id.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn volumes(&self) -> Result<Vec<(String, String, u64)>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, last_seen_ms FROM volumes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, label, last_seen_ms) = row?;
            out.push((id, label, u64::try_from(last_seen_ms).unwrap_or(0)));
        }
        Ok(out)
    }

    /// Every PARA node: (id, kind, name, archived), ordered by id.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn para_nodes(&self) -> Result<Vec<(String, String, String, bool)>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, name, archived FROM para_nodes ORDER BY id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Distinct asset count per volume, derived from `instances`, ordered by
    /// volume.
    ///
    /// # Errors
    /// Returns an error if the underlying query fails.
    pub fn volume_asset_counts(&self) -> Result<Vec<(String, u64)>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT volume, COUNT(DISTINCT asset) FROM instances GROUP BY volume ORDER BY volume",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (volume, count) = row?;
            out.push((volume, u64::try_from(count).unwrap_or(0)));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::tests::rebuild_from_ops;
    use majestical_core::event::{Op, ParaKind};

    #[test]
    fn fts_name_search_is_unicode_case_insensitive_and_ranked() {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v".into(),
                    path: "clips/Café-sunset.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: b.clone(),
                    volume: "v".into(),
                    path: "docs/readme.txt".into(),
                    size: 1,
                    mtime_ms: 0,
                },
            ],
        );
        let hits = db
            .search_names_ranked(&["cafe".to_string()], 10)
            .expect("name search");
        let ids: Vec<AssetId> = hits.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![a],
            "unicode61 remove_diacritics must fold Café to cafe"
        );
    }

    /// Pins `remove_diacritics 2` specifically (not the default level 1):
    /// U+1EC6 (Ệ) carries two combining marks a circumflex and a dot below)
    /// that level 1's smaller diacritic table doesn't fold away, so this
    /// only matches under level 2.
    #[test]
    fn diacritics_level_2_folds_marks_level_1_does_not() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "clips/Ệlan.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );
        let hits = db
            .search_names_ranked(&["elan".to_string()], 10)
            .expect("name search");
        let ids: Vec<AssetId> = hits.into_iter().map(|(id, _)| id).collect();
        assert_eq!(
            ids,
            vec![a],
            "remove_diacritics 2 must fold Ệ (circumflex + dot below) to e"
        );
    }

    #[test]
    fn prefix_terms_match() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "v".into(),
                path: "clips/beach_day.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );
        let hits = db
            .search_names_ranked(&["beach".to_string()], 10)
            .expect("prefix search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, a);
    }

    /// An asset with two instances whose basenames both match the search
    /// term must come back as one row, not one per matching basename — a
    /// naive `SELECT asset, rank FROM names_fts WHERE ... MATCH` returns one
    /// row per matching (name, asset) pair in the FTS index.
    #[test]
    fn ranked_search_returns_one_row_per_asset_not_per_matching_basename() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v".into(),
                    path: "clips/beach.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v2".into(),
                    path: "clips/beach_copy.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
            ],
        );
        let hits = db
            .search_names_ranked(&["beach".to_string()], 10)
            .expect("search");
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one row for one asset, got {hits:?}"
        );
        assert_eq!(hits[0].0, a);
    }

    /// Shared fixture for the `filters_*` tests below: asset `a` is tagged
    /// "keep", lives on volume v1 ("Card A") as a video, mtime 1000, and is
    /// assigned to PARA node N1; asset `b` is tagged "rejected", lives on
    /// volume v2 ("Card B") as a non-media file, mtime 2000, and has no PARA
    /// assignment.
    fn filters_fixture(dir: &std::path::Path) -> (SqliteCatalog, AssetId, AssetId) {
        let a = AssetId("xxh3:a".into());
        let b = AssetId("xxh3:b".into());
        let db = rebuild_from_ops(
            &dir.join("catalog.db"),
            vec![
                Op::VolumeSeen {
                    volume: "v1".into(),
                    label: "Card A".into(),
                },
                Op::VolumeSeen {
                    volume: "v2".into(),
                    label: "Card B".into(),
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v1".into(),
                    path: "beach.mov".into(),
                    size: 1,
                    mtime_ms: 1000,
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "keep".into(),
                },
                Op::ParaNodeCreate {
                    node: "N1".into(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                Op::AssetParaSet {
                    asset: a.clone(),
                    node: "N1".into(),
                },
                Op::AssetSeen {
                    asset: b.clone(),
                    volume: "v2".into(),
                    path: "notes.txt".into(),
                    size: 1,
                    mtime_ms: 2000,
                },
                Op::TagAdd {
                    asset: b.clone(),
                    tag: "rejected".into(),
                },
            ],
        );
        (db, a, b)
    }

    #[test]
    fn tag_filters_match_and_negate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Tag {
                value: "keep".into(),
                negated: false,
            }])
            .expect("tag"),
            BTreeSet::from([a.clone()])
        );
        let rejected_negated = db
            .assets_matching(&[Filter::Tag {
                value: "rejected".into(),
                negated: true,
            }])
            .expect("negated tag");
        assert!(rejected_negated.contains(&a));
        assert!(!rejected_negated.contains(&b));
    }

    #[test]
    fn time_filters_bound_by_mtime_and_combine_with_other_filters() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[
                Filter::Tag {
                    value: "keep".into(),
                    negated: false,
                },
                Filter::Before(1500),
            ])
            .expect("tag and before"),
            BTreeSet::from([a])
        );
        assert!(
            db.assets_matching(&[Filter::After(1500)])
                .expect("after")
                .contains(&b)
        );
    }

    /// `Before`/`After` are strict inequalities: an instance whose `mtime_ms`
    /// exactly equals the bound matches neither.
    #[test]
    fn before_and_after_are_strict_at_the_exact_boundary() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a,
                volume: "v".into(),
                path: "a.mov".into(),
                size: 1,
                mtime_ms: 1000,
            }],
        );
        assert_eq!(
            db.assets_matching(&[Filter::Before(1000)]).expect("before"),
            BTreeSet::new(),
            "Before(1000) must not match an instance with mtime_ms == 1000"
        );
        assert_eq!(
            db.assets_matching(&[Filter::After(1000)]).expect("after"),
            BTreeSet::new(),
            "After(1000) must not match an instance with mtime_ms == 1000"
        );
    }

    #[test]
    fn kind_and_volume_filters_match_by_id_or_label() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Kind {
                value: "video".into(),
                negated: false,
            }])
            .expect("kind video"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Kind {
                value: "other".into(),
                negated: false,
            }])
            .expect("kind other"),
            BTreeSet::from([b])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "v1".into(),
                negated: false,
            }])
            .expect("volume by id"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "Card A".into(),
                negated: false,
            }])
            .expect("volume by label"),
            BTreeSet::from([a])
        );
    }

    #[test]
    fn para_filter_matches_assigned_node() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, _b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Para {
                node: "N1".into(),
                negated: false,
            }])
            .expect("para"),
            BTreeSet::from([a])
        );
    }

    #[test]
    fn online_filter_handles_mounted_ids_and_the_empty_set() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (db, a, b) = filters_fixture(dir.path());

        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec!["v1".into()],
                want: true,
            }])
            .expect("online true"),
            BTreeSet::from([a.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec!["v1".into()],
                want: false,
            }])
            .expect("online false"),
            BTreeSet::from([b.clone()])
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec![],
                want: true,
            }])
            .expect("online empty want true"),
            BTreeSet::new()
        );
        assert_eq!(
            db.assets_matching(&[Filter::Online {
                ids: vec![],
                want: false,
            }])
            .expect("online empty want false"),
            BTreeSet::from([a, b])
        );
    }

    #[test]
    fn asset_summaries_carry_name_volumes_tags_para() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![
                Op::VolumeSeen {
                    volume: "v1".into(),
                    label: "Card A".into(),
                },
                Op::VolumeSeen {
                    volume: "v2".into(),
                    label: "Card B".into(),
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v1".into(),
                    path: "clips/beach.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::AssetSeen {
                    asset: a.clone(),
                    volume: "v2".into(),
                    path: "clips/beach_copy.mov".into(),
                    size: 1,
                    mtime_ms: 0,
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "keep".into(),
                },
                Op::TagAdd {
                    asset: a.clone(),
                    tag: "topic/drone".into(),
                },
                Op::ParaNodeCreate {
                    node: "N1".into(),
                    kind: ParaKind::Project,
                    name: "client-x".into(),
                },
                Op::AssetParaSet {
                    asset: a.clone(),
                    node: "N1".into(),
                },
            ],
        );

        let summaries = db
            .asset_summaries(std::slice::from_ref(&a))
            .expect("summaries");
        assert_eq!(summaries.len(), 1);
        let s = &summaries[0];
        assert_eq!(s.asset, a);
        assert_eq!(
            s.name, "beach.mov",
            "name must be the first instance's basename, ordered by volume then path"
        );
        assert_eq!(
            s.volumes,
            vec![
                ("v1".to_string(), "Card A".to_string()),
                ("v2".to_string(), "Card B".to_string()),
            ]
        );
        assert_eq!(s.tags, vec!["keep".to_string(), "topic/drone".to_string()]);
        assert_eq!(s.para.as_deref(), Some("N1"));
    }

    /// A "ghost" volume — an instance whose volume id has no `VolumeSeen`
    /// row — is reachable via partial cross-machine sync (one machine's log
    /// carries an `AssetSeen` for a volume the reader never observed a
    /// `VolumeSeen` for). `Filter::Volume` must still match it by instance
    /// volume id, and `asset_summaries` must fall back to the id as the
    /// label rather than erroring on a NULL join.
    #[test]
    fn ghost_volume_is_findable_and_summarizable_without_a_volume_seen_row() {
        let a = AssetId("xxh3:a".into());
        let dir = tempfile::tempdir().expect("tempdir");
        let db = rebuild_from_ops(
            &dir.path().join("catalog.db"),
            vec![Op::AssetSeen {
                asset: a.clone(),
                volume: "ghost".into(),
                path: "a.mov".into(),
                size: 1,
                mtime_ms: 0,
            }],
        );

        assert_eq!(
            db.assets_matching(&[Filter::Volume {
                value: "ghost".into(),
                negated: false,
            }])
            .expect("volume filter on a ghost volume"),
            BTreeSet::from([a.clone()])
        );

        let summaries = db
            .asset_summaries(std::slice::from_ref(&a))
            .expect("summaries must not error on a ghost volume");
        assert_eq!(
            summaries[0].volumes,
            vec![("ghost".to_string(), "ghost".to_string())],
            "label must fall back to the volume id when there's no VolumeSeen row"
        );
    }
}
