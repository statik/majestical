//! Local, disposable `LanceDB` dataset over the vectors that live canonically
//! as blobs. Sync API: an internal current-thread tokio runtime keeps async
//! out of the CLI. Vectors are L2-normalized (encoder invariant), so Dot
//! distance == cosine. Per-machine local BECAUSE the sync transport is dumb
//! files — a Lance dataset with two writers through Dropbox would corrupt;
//! blobs are the exchange format and this dataset rebuilds from them.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{Array, FixedSizeListArray, Float32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{DistanceType, Table, connect};

use crate::error::IndexError;

/// Embedding dimension, pinned to the `SigLIP` 2 encoder's `pooler_output`.
pub const DIM: usize = 768;

// `DIM` is a small, fixed compile-time constant — casting it to `i32` for
// Arrow's `FixedSizeList` width can neither truncate nor wrap.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "DIM (768) fits i32"
)]
const DIM_I32: i32 = DIM as i32;

const TABLE_NAME: &str = "vectors";

/// One embedding to add to the store.
pub struct VectorRow {
    pub asset_hex: String,
    pub kind: String,
    /// Timestamp in milliseconds for a keyframe embedding; -1 for a
    /// whole-image embedding.
    pub ts_ms: i64,
    pub model_tag: String,
    pub vector: Vec<f32>,
}

/// One nearest-neighbor result from [`VectorStore::search`].
pub struct VectorHit {
    pub asset_hex: String,
    pub kind: String,
    pub ts_ms: i64,
    pub distance: f32,
}

/// Sync wrapper around a local `LanceDB` table.
pub struct VectorStore {
    rt: tokio::runtime::Runtime,
    table: Table,
}

impl VectorStore {
    /// Opens the Lance dataset at `dir`, creating an empty `vectors` table
    /// if none exists yet.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the tokio runtime cannot
    /// start, the connection fails, or the table cannot be opened or
    /// created.
    pub fn open(dir: &Path) -> Result<Self, IndexError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| IndexError::VectorStore(format!("start runtime: {e}")))?;
        let uri = dir.to_string_lossy().into_owned();
        let table = rt.block_on(async {
            let db = connect(&uri)
                .execute()
                .await
                .map_err(|e| IndexError::VectorStore(format!("connect {uri}: {e}")))?;
            match db.open_table(TABLE_NAME).execute().await {
                Ok(table) => Ok(table),
                Err(lancedb::Error::TableNotFound { .. }) => db
                    .create_empty_table(TABLE_NAME, schema())
                    .execute()
                    .await
                    .map_err(|e| IndexError::VectorStore(format!("create table: {e}"))),
                Err(e) => Err(IndexError::VectorStore(format!(
                    "open table {TABLE_NAME}: {e}"
                ))),
            }
        })?;
        Ok(Self { rt, table })
    }

    /// Appends `rows` to the table. A no-op for an empty slice.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if any row's vector is not
    /// [`DIM`] elements long, or if the underlying write fails.
    // Taking ownership (rather than `&[VectorRow]`) signals to callers that
    // this consumes the batch; it isn't reused after adding.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "ownership signals the batch is consumed"
    )]
    pub fn add(&self, rows: Vec<VectorRow>) -> Result<(), IndexError> {
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            if row.vector.len() != DIM {
                return Err(IndexError::VectorStore(format!(
                    "row {}: vector has {} elements, expected {DIM}",
                    row.asset_hex,
                    row.vector.len()
                )));
            }
        }
        let count = rows.len();
        let batch = rows_to_batch(&rows)?;
        self.rt.block_on(async {
            self.table
                .add(batch)
                .execute()
                .await
                .map_err(|e| IndexError::VectorStore(format!("add {count} rows: {e}")))?;
            Ok(())
        })
    }

    /// Returns the `limit` nearest rows to `vector` within `model_tag`,
    /// nearest first.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if `vector` is not [`DIM`]
    /// elements long, or if the query fails.
    pub fn search(
        &self,
        vector: &[f32],
        model_tag: &str,
        limit: usize,
    ) -> Result<Vec<VectorHit>, IndexError> {
        if vector.len() != DIM {
            return Err(IndexError::VectorStore(format!(
                "search vector has {} elements, expected {DIM}",
                vector.len()
            )));
        }
        let query_vector = vector.to_vec();
        let predicate = tag_predicate(model_tag);
        let batches = self.rt.block_on(async {
            self.table
                .query()
                .nearest_to(query_vector)
                .map_err(|e| IndexError::VectorStore(format!("build vector query: {e}")))?
                .distance_type(DistanceType::Dot)
                .only_if(predicate)
                .limit(limit)
                .execute()
                .await
                .map_err(|e| IndexError::VectorStore(format!("execute search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| IndexError::VectorStore(format!("collect search results: {e}")))
        })?;
        batches_to_hits(&batches)
    }

    /// Returns the `(asset_hex, kind, ts_ms)` keys already present for
    /// `model_tag`. This is the Lance side of the blob-versus-Lance diff
    /// used to work out what still needs adding; a full scan is fine at
    /// catalog scale.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the scan fails.
    pub fn existing_keys(
        &self,
        model_tag: &str,
    ) -> Result<BTreeSet<(String, String, i64)>, IndexError> {
        let batches = self.scan(model_tag, &["asset_hex", "kind", "ts_ms"])?;
        batches_to_keys(&batches)
    }

    /// Returns the distinct asset hexes present for `model_tag`.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the scan fails.
    pub fn distinct_assets(&self, model_tag: &str) -> Result<BTreeSet<String>, IndexError> {
        let batches = self.scan(model_tag, &["asset_hex"])?;
        let mut assets = BTreeSet::new();
        for batch in &batches {
            let asset_hex = string_column(batch, "asset_hex")?;
            for i in 0..batch.num_rows() {
                assets.insert(asset_hex.value(i).to_string());
            }
        }
        Ok(assets)
    }

    fn scan(&self, model_tag: &str, columns: &[&str]) -> Result<Vec<RecordBatch>, IndexError> {
        let predicate = tag_predicate(model_tag);
        let select = Select::Columns(columns.iter().map(|c| (*c).to_string()).collect());
        self.rt.block_on(async {
            self.table
                .query()
                .only_if(predicate)
                .select(select)
                .execute()
                .await
                .map_err(|e| IndexError::VectorStore(format!("scan: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| IndexError::VectorStore(format!("collect scan results: {e}")))
        })
    }
}

/// Builds the SQL filter restricting a query to one embedding model. Our
/// model tags are our own consts, never user input, so this escaping is
/// belt-and-braces — pinned by
/// `a_malicious_model_tag_cannot_break_the_filter` below.
fn tag_predicate(model_tag: &str) -> String {
    format!("model_tag = '{}'", model_tag.replace('\'', "''"))
}

fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("asset_hex", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("ts_ms", DataType::Int64, false),
        Field::new("model_tag", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                DIM_I32,
            ),
            true,
        ),
    ]))
}

fn rows_to_batch(rows: &[VectorRow]) -> Result<RecordBatch, IndexError> {
    let asset_hex = StringArray::from_iter_values(rows.iter().map(|r| r.asset_hex.as_str()));
    let kind = StringArray::from_iter_values(rows.iter().map(|r| r.kind.as_str()));
    let ts_ms = Int64Array::from_iter_values(rows.iter().map(|r| r.ts_ms));
    let model_tag = StringArray::from_iter_values(rows.iter().map(|r| r.model_tag.as_str()));
    let vector = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter()
            .map(|r| Some(r.vector.iter().copied().map(Some).collect::<Vec<_>>())),
        DIM_I32,
    );
    RecordBatch::try_new(
        schema(),
        vec![
            Arc::new(asset_hex),
            Arc::new(kind),
            Arc::new(ts_ms),
            Arc::new(model_tag),
            Arc::new(vector),
        ],
    )
    .map_err(|e| IndexError::VectorStore(format!("build record batch: {e}")))
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray, IndexError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| IndexError::VectorStore(format!("missing column {name}")))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| IndexError::VectorStore(format!("column {name} is not utf8")))
}

fn i64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, IndexError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| IndexError::VectorStore(format!("missing column {name}")))?
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| IndexError::VectorStore(format!("column {name} is not int64")))
}

fn f32_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Float32Array, IndexError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| IndexError::VectorStore(format!("missing column {name}")))?
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| IndexError::VectorStore(format!("column {name} is not float32")))
}

fn batches_to_hits(batches: &[RecordBatch]) -> Result<Vec<VectorHit>, IndexError> {
    let mut hits = Vec::new();
    for batch in batches {
        let asset_hex = string_column(batch, "asset_hex")?;
        let kind = string_column(batch, "kind")?;
        let ts_ms = i64_column(batch, "ts_ms")?;
        let distance = f32_column(batch, "_distance")?;
        for i in 0..batch.num_rows() {
            hits.push(VectorHit {
                asset_hex: asset_hex.value(i).to_string(),
                kind: kind.value(i).to_string(),
                ts_ms: ts_ms.value(i),
                distance: distance.value(i),
            });
        }
    }
    Ok(hits)
}

fn batches_to_keys(batches: &[RecordBatch]) -> Result<BTreeSet<(String, String, i64)>, IndexError> {
    let mut keys = BTreeSet::new();
    for batch in batches {
        let asset_hex = string_column(batch, "asset_hex")?;
        let kind = string_column(batch, "kind")?;
        let ts_ms = i64_column(batch, "ts_ms")?;
        for i in 0..batch.num_rows() {
            keys.insert((
                asset_hex.value(i).to_string(),
                kind.value(i).to_string(),
                ts_ms.value(i),
            ));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::{DIM, VectorRow, VectorStore};
    use crate::encoder::EMBED_DIM;

    #[test]
    fn dim_matches_encoder_embed_dim() {
        assert_eq!(DIM, EMBED_DIM);
    }

    fn unit(i: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        v[i] = 1.0;
        v
    }

    #[test]
    fn add_search_and_diff_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = VectorStore::open(&dir.path().join("lance")).expect("open");
        store
            .add(vec![
                VectorRow {
                    asset_hex: "aa11".into(),
                    kind: "image".into(),
                    ts_ms: -1,
                    model_tag: "m1".into(),
                    vector: unit(0),
                },
                VectorRow {
                    asset_hex: "bb22".into(),
                    kind: "keyframe".into(),
                    ts_ms: 4500,
                    model_tag: "m1".into(),
                    vector: unit(1),
                },
                VectorRow {
                    asset_hex: "cc33".into(),
                    kind: "image".into(),
                    ts_ms: -1,
                    model_tag: "other".into(),
                    vector: unit(0),
                },
            ])
            .expect("add");

        let hits = store.search(&unit(0), "m1", 10).expect("search");
        assert_eq!(hits[0].asset_hex, "aa11", "nearest by dot product");
        assert!(
            hits.iter().all(|h| h.asset_hex != "cc33"),
            "model_tag filter applies"
        );

        let keys = store.existing_keys("m1").expect("keys");
        assert!(keys.contains(&("aa11".to_string(), "image".to_string(), -1)));
        assert!(keys.contains(&("bb22".to_string(), "keyframe".to_string(), 4500)));
        assert_eq!(keys.len(), 2);
        assert_eq!(store.distinct_assets("m1").expect("assets").len(), 2);
    }

    #[test]
    fn reopen_persists_and_empty_add_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        {
            let store = VectorStore::open(&lance_dir).expect("open");
            store
                .add(vec![VectorRow {
                    asset_hex: "aa11".into(),
                    kind: "image".into(),
                    ts_ms: -1,
                    model_tag: "m1".into(),
                    vector: unit(0),
                }])
                .expect("add");
            store.add(vec![]).expect("empty add is a noop");
        }

        let store = VectorStore::open(&lance_dir).expect("reopen");
        let keys = store.existing_keys("m1").expect("keys");
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&("aa11".to_string(), "image".to_string(), -1)));
    }

    #[test]
    fn a_malicious_model_tag_cannot_break_the_filter() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = VectorStore::open(&dir.path().join("lance")).expect("open");
        store
            .add(vec![VectorRow {
                asset_hex: "aa11".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: unit(0),
            }])
            .expect("add");

        let hits = store
            .search(&unit(0), "m1' OR '1'='1", 10)
            .expect("malicious tag is escaped, not rejected");
        assert!(
            hits.is_empty(),
            "escaping holds: no rows match the literal tag"
        );
    }
}
