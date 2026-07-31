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
    /// if none exists yet. Write paths (`index run`'s embed executor) use
    /// this; read-only callers should use [`open_existing`](Self::open_existing)
    /// instead, which never materializes state.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the tokio runtime cannot
    /// start, the connection fails, or the table cannot be opened or
    /// created.
    pub fn open(dir: &Path) -> Result<Self, IndexError> {
        let rt = new_runtime()?;
        let uri = dir.to_string_lossy().into_owned();
        let table = rt.block_on(async {
            let db = connect_local(&uri).await?;
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

    /// Opens the Lance dataset at `dir` only if one already exists there —
    /// unlike [`open`](Self::open), never creates it. A read-only query (the
    /// search command's semantic layer) must not materialize local state
    /// just by running; only `index run`'s write path should ever create the
    /// dataset. Returns `Ok(None)` when `dir` doesn't exist yet, or exists
    /// but holds no `vectors` table.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the tokio runtime cannot
    /// start, the connection fails, or opening an existing table fails for a
    /// reason other than "table not found".
    pub fn open_existing(dir: &Path) -> Result<Option<Self>, IndexError> {
        if !dir.is_dir() {
            return Ok(None);
        }
        let rt = new_runtime()?;
        let uri = dir.to_string_lossy().into_owned();
        let table = rt.block_on(async {
            let db = connect_local(&uri).await?;
            match db.open_table(TABLE_NAME).execute().await {
                Ok(table) => Ok(Some(table)),
                Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
                Err(e) => Err(IndexError::VectorStore(format!(
                    "open table {TABLE_NAME}: {e}"
                ))),
            }
        })?;
        Ok(table.map(|table| Self { rt, table }))
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
        // Column projection: a hit only ever needs `asset_hex`/`kind`/`ts_ms`
        // (plus the query's own `_distance`, which `nearest_to` attaches
        // regardless of this projection) — without it, every search
        // materializes the 768-float `vector` column for every hit, ~3 KB
        // of dead weight per row at catalog scale.
        let select = Select::Columns(
            ["asset_hex", "kind", "ts_ms"]
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
        );
        let batches = self.rt.block_on(async {
            self.table
                .query()
                .nearest_to(query_vector)
                .map_err(|e| IndexError::VectorStore(format!("build vector query: {e}")))?
                .distance_type(DistanceType::Dot)
                .only_if(predicate)
                .select(select)
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

fn new_runtime() -> Result<tokio::runtime::Runtime, IndexError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| IndexError::VectorStore(format!("start runtime: {e}")))
}

async fn connect_local(uri: &str) -> Result<lancedb::Connection, IndexError> {
    connect(uri)
        .execute()
        .await
        .map_err(|e| IndexError::VectorStore(format!("connect {uri}: {e}")))
}

/// Runs `f`, catching both an ordinary `Err` and an unwinding panic, and
/// reporting either as one human-readable reason — callers get a single
/// failure path instead of two. This exists because lance's own dataset
/// reader panics rather than returning an `Err` on some corrupt input (an
/// unchecked subtraction while parsing a garbage `.manifest` file; verified
/// against `lance` 9.0.0 — `attempt to subtract with overflow` at
/// `lance::Dataset`'s manifest-loading code), which this crate has no way to
/// prevent short of catching it at the call boundary.
///
/// KNOWN GAP: callers typically pass a cheap probe scan (e.g.
/// `existing_keys`/`distinct_assets`, column-projected to skip `vector`) as
/// part of `f`, to force corruption discovery immediately rather than
/// later. That probe reads every column EXCEPT `vector` — so corruption
/// confined to the vector column's on-disk bytes specifically (data intact
/// for `asset_hex`/`kind`/`ts_ms`) would pass the probe silently and only
/// surface later, at an actual `search()` call. Not yet reproduced or
/// tested against; noted here so the next investigation starts from this,
/// not from zero.
///
/// # Errors
/// Returns `Err` with a human-readable reason if `f` returns an `Err` or
/// panics.
pub fn catch_corruption<T, F>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, IndexError> + std::panic::UnwindSafe,
{
    match std::panic::catch_unwind(f) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(err.to_string()),
        // `&*panic`, not `&panic`: `Box<dyn Any + Send>` itself implements
        // `Any` too (the blanket `impl<T: 'static> Any for T` covers it), so
        // an implicit `&panic` coercion at the call boundary can wrap the
        // BOX as the trait object instead of deref'ing to the payload
        // inside it — `downcast_ref::<&str>()` would then always miss even
        // though the real payload is a `&str`. The explicit deref forces
        // the reference to point at the actual panic payload.
        Err(panic) => Err(panic_message(&*panic)),
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
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

/// Embedding dimension for text-chunk embeddings, pinned to the `MiniLM`
/// encoder's sentence-embedding output. Distinct from [`DIM`] (768, image
/// embeddings) — the two tables never mix vectors of different width.
pub const TEXT_DIM: usize = 384;

// TEXT_DIM (384) is a small, fixed compile-time constant — casting it to
// `i32` for Arrow's `FixedSizeList` width can neither truncate nor wrap.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "TEXT_DIM (384) fits i32"
)]
const TEXT_DIM_I32: i32 = TEXT_DIM as i32;

const TEXT_TABLE_NAME: &str = "text_chunks";

/// One text-chunk embedding to add to the [`TextVectorStore`].
pub struct TextChunkRow {
    pub asset_hex: String,
    pub source: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub model_tag: String,
    pub text: String,
    pub vector: Vec<f32>,
}

/// One nearest-neighbor result from [`TextVectorStore::search`].
pub struct TextChunkHit {
    pub asset_hex: String,
    pub source: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub distance: f32,
}

/// Sync wrapper around a local `LanceDB` `text_chunks` table — a second,
/// 384-d table living alongside [`VectorStore`]'s 768-d `vectors` table in
/// the same Lance dataset directory (different table names, so both
/// coexist). Stores chunk text alongside the vector for snippet display,
/// unlike `VectorStore`, which stores no display text at all.
pub struct TextVectorStore {
    rt: tokio::runtime::Runtime,
    table: Table,
}

impl TextVectorStore {
    /// Opens the Lance dataset at `dir`, creating an empty `text_chunks`
    /// table if none exists yet. Write paths (indexing transcript/caption
    /// chunks) use this; read-only callers should use
    /// [`open_existing`](Self::open_existing) instead, which never
    /// materializes state.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the tokio runtime cannot
    /// start, the connection fails, or the table cannot be opened or
    /// created.
    pub fn open(dir: &Path) -> Result<Self, IndexError> {
        let rt = new_runtime()?;
        let uri = dir.to_string_lossy().into_owned();
        let table = rt.block_on(async {
            let db = connect_local(&uri).await?;
            match db.open_table(TEXT_TABLE_NAME).execute().await {
                Ok(table) => Ok(table),
                Err(lancedb::Error::TableNotFound { .. }) => db
                    .create_empty_table(TEXT_TABLE_NAME, text_schema())
                    .execute()
                    .await
                    .map_err(|e| IndexError::VectorStore(format!("create table: {e}"))),
                Err(e) => Err(IndexError::VectorStore(format!(
                    "open table {TEXT_TABLE_NAME}: {e}"
                ))),
            }
        })?;
        Ok(Self { rt, table })
    }

    /// Opens the Lance dataset at `dir` only if one already exists there —
    /// unlike [`open`](Self::open), never creates it. Returns `Ok(None)`
    /// when `dir` doesn't exist yet, or exists but holds no `text_chunks`
    /// table.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the tokio runtime cannot
    /// start, the connection fails, or opening an existing table fails for a
    /// reason other than "table not found".
    pub fn open_existing(dir: &Path) -> Result<Option<Self>, IndexError> {
        if !dir.is_dir() {
            return Ok(None);
        }
        let rt = new_runtime()?;
        let uri = dir.to_string_lossy().into_owned();
        let table = rt.block_on(async {
            let db = connect_local(&uri).await?;
            match db.open_table(TEXT_TABLE_NAME).execute().await {
                Ok(table) => Ok(Some(table)),
                Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
                Err(e) => Err(IndexError::VectorStore(format!(
                    "open table {TEXT_TABLE_NAME}: {e}"
                ))),
            }
        })?;
        Ok(table.map(|table| Self { rt, table }))
    }

    /// Appends `rows` to the table. A no-op for an empty slice.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if any row's vector is not
    /// [`TEXT_DIM`] elements long, or if the underlying write fails.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "ownership signals the batch is consumed"
    )]
    pub fn add(&self, rows: Vec<TextChunkRow>) -> Result<(), IndexError> {
        if rows.is_empty() {
            return Ok(());
        }
        for row in &rows {
            if row.vector.len() != TEXT_DIM {
                return Err(IndexError::VectorStore(format!(
                    "row {}: vector has {} elements, expected {TEXT_DIM}",
                    row.asset_hex,
                    row.vector.len()
                )));
            }
        }
        let count = rows.len();
        let batch = text_rows_to_batch(&rows)?;
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
    /// Returns [`IndexError::VectorStore`] if `vector` is not [`TEXT_DIM`]
    /// elements long, or if the query fails.
    pub fn search(
        &self,
        vector: &[f32],
        model_tag: &str,
        limit: usize,
    ) -> Result<Vec<TextChunkHit>, IndexError> {
        if vector.len() != TEXT_DIM {
            return Err(IndexError::VectorStore(format!(
                "search vector has {} elements, expected {TEXT_DIM}",
                vector.len()
            )));
        }
        let query_vector = vector.to_vec();
        let predicate = tag_predicate(model_tag);
        // Column projection: a hit only ever needs `asset_hex`/`source`/
        // `start_ms`/`end_ms`/`text` (plus the query's own `_distance`,
        // which `nearest_to` attaches regardless of this projection) —
        // without it, every search materializes the 384-float `vector`
        // column for every hit, dead weight per row at catalog scale.
        let select = Select::Columns(
            ["asset_hex", "source", "start_ms", "end_ms", "text"]
                .iter()
                .map(|c| (*c).to_string())
                .collect(),
        );
        let batches = self.rt.block_on(async {
            self.table
                .query()
                .nearest_to(query_vector)
                .map_err(|e| IndexError::VectorStore(format!("build vector query: {e}")))?
                .distance_type(DistanceType::Dot)
                .only_if(predicate)
                .select(select)
                .limit(limit)
                .execute()
                .await
                .map_err(|e| IndexError::VectorStore(format!("execute search: {e}")))?
                .try_collect::<Vec<_>>()
                .await
                .map_err(|e| IndexError::VectorStore(format!("collect search results: {e}")))
        })?;
        text_batches_to_hits(&batches)
    }

    /// Returns the `(asset_hex, start_ms)` keys already present for
    /// `model_tag`. This is the Lance side of the blob-versus-Lance diff
    /// used to work out what still needs adding; a full scan is fine at
    /// catalog scale. `source` is absent from the key: this table only ever
    /// holds `"transcript"` chunks — captions, OCR, and PDF text land in
    /// `SQLite`'s `text_fts` instead, so there's nothing for `source` to
    /// disambiguate here.
    ///
    /// # Errors
    /// Returns [`IndexError::VectorStore`] if the scan fails.
    pub fn existing_keys(&self, model_tag: &str) -> Result<BTreeSet<(String, i64)>, IndexError> {
        let batches = self.scan(model_tag, &["asset_hex", "start_ms"])?;
        text_batches_to_keys(&batches)
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

fn text_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("asset_hex", DataType::Utf8, false),
        Field::new("source", DataType::Utf8, false),
        Field::new("start_ms", DataType::Int64, false),
        Field::new("end_ms", DataType::Int64, false),
        Field::new("model_tag", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                TEXT_DIM_I32,
            ),
            true,
        ),
    ]))
}

fn text_rows_to_batch(rows: &[TextChunkRow]) -> Result<RecordBatch, IndexError> {
    let asset_hex = StringArray::from_iter_values(rows.iter().map(|r| r.asset_hex.as_str()));
    let source = StringArray::from_iter_values(rows.iter().map(|r| r.source.as_str()));
    let start_ms = Int64Array::from_iter_values(rows.iter().map(|r| r.start_ms));
    let end_ms = Int64Array::from_iter_values(rows.iter().map(|r| r.end_ms));
    let model_tag = StringArray::from_iter_values(rows.iter().map(|r| r.model_tag.as_str()));
    let text = StringArray::from_iter_values(rows.iter().map(|r| r.text.as_str()));
    let vector = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter()
            .map(|r| Some(r.vector.iter().copied().map(Some).collect::<Vec<_>>())),
        TEXT_DIM_I32,
    );
    RecordBatch::try_new(
        text_schema(),
        vec![
            Arc::new(asset_hex),
            Arc::new(source),
            Arc::new(start_ms),
            Arc::new(end_ms),
            Arc::new(model_tag),
            Arc::new(text),
            Arc::new(vector),
        ],
    )
    .map_err(|e| IndexError::VectorStore(format!("build record batch: {e}")))
}

fn text_batches_to_hits(batches: &[RecordBatch]) -> Result<Vec<TextChunkHit>, IndexError> {
    let mut hits = Vec::new();
    for batch in batches {
        let asset_hex = string_column(batch, "asset_hex")?;
        let source = string_column(batch, "source")?;
        let start_ms = i64_column(batch, "start_ms")?;
        let end_ms = i64_column(batch, "end_ms")?;
        let text = string_column(batch, "text")?;
        let distance = f32_column(batch, "_distance")?;
        for i in 0..batch.num_rows() {
            hits.push(TextChunkHit {
                asset_hex: asset_hex.value(i).to_string(),
                source: source.value(i).to_string(),
                start_ms: start_ms.value(i),
                end_ms: end_ms.value(i),
                text: text.value(i).to_string(),
                distance: distance.value(i),
            });
        }
    }
    Ok(hits)
}

fn text_batches_to_keys(batches: &[RecordBatch]) -> Result<BTreeSet<(String, i64)>, IndexError> {
    let mut keys = BTreeSet::new();
    for batch in batches {
        let asset_hex = string_column(batch, "asset_hex")?;
        let start_ms = i64_column(batch, "start_ms")?;
        for i in 0..batch.num_rows() {
            keys.insert((asset_hex.value(i).to_string(), start_ms.value(i)));
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::{
        DIM, TEXT_DIM, TextChunkRow, TextVectorStore, VectorRow, VectorStore, catch_corruption,
    };
    use crate::encoder::EMBED_DIM;
    use crate::error::IndexError;

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

    #[test]
    fn open_existing_returns_none_without_creating_anything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        assert!(
            VectorStore::open_existing(&lance_dir)
                .expect("open_existing on a missing dir")
                .is_none()
        );
        assert!(
            !lance_dir.exists(),
            "a read-only open must never materialize the dataset directory"
        );
    }

    #[test]
    fn open_existing_finds_a_dataset_created_by_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        VectorStore::open(&lance_dir)
            .expect("open")
            .add(vec![VectorRow {
                asset_hex: "aa11".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: unit(0),
            }])
            .expect("add");

        let store = VectorStore::open_existing(&lance_dir)
            .expect("open_existing")
            .expect("dataset exists");
        assert_eq!(store.existing_keys("m1").expect("keys").len(), 1);
    }

    #[test]
    fn catch_corruption_passes_through_ok_and_err() {
        assert_eq!(catch_corruption(|| Ok(1)), Ok(1));
        let err = catch_corruption(|| Err::<i32, _>(IndexError::VectorStore("boom".into())));
        assert_eq!(err, Err("vector store: boom".to_string()));
    }

    #[test]
    fn catch_corruption_turns_a_panic_into_an_err() {
        let result = catch_corruption(|| -> Result<i32, IndexError> {
            panic!("simulated lance manifest panic");
        });
        assert_eq!(result, Err("simulated lance manifest panic".to_string()));
    }

    /// Pins the actual failure mode a garbage `.manifest` produces (a panic
    /// deep inside lance's own dataset reader, not an `Err` — see
    /// `catch_corruption`'s doc comment; verified by hand against real
    /// `lance` 9.0.0: `attempt to subtract with overflow`) and confirms
    /// `catch_corruption` turns it into an ordinary `Err` instead of
    /// unwinding out of the caller.
    #[test]
    fn catch_corruption_recovers_from_a_garbage_manifest_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lance_dir = dir.path().join("lance");
        VectorStore::open(&lance_dir)
            .expect("open")
            .add(vec![VectorRow {
                asset_hex: "aa11".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: unit(0),
            }])
            .expect("add");
        corrupt_all_manifests(&lance_dir);

        let owned = lance_dir.clone();
        let result = catch_corruption(move || VectorStore::open(&owned));
        assert!(
            result.is_err(),
            "a garbage manifest must not panic past catch_corruption"
        );
    }

    /// Overwrites every manifest file with garbage bytes — the corruption
    /// recipe that makes lance's own manifest reader panic (see
    /// `catch_corruption`'s doc comment). Corrupting all of them (rather than
    /// guessing which one lance treats as "latest") keeps this independent
    /// of lance's internal version-file naming scheme.
    fn corrupt_all_manifests(lance_dir: &std::path::Path) {
        let versions_dir = lance_dir.join("vectors.lance/_versions");
        let entries = std::fs::read_dir(&versions_dir).expect("read _versions dir");
        for entry in entries.filter_map(Result::ok) {
            if entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "manifest")
            {
                std::fs::write(entry.path(), b"GARBAGE-NOT-A-REAL-MANIFEST")
                    .expect("corrupt manifest");
            }
        }
    }

    fn text_unit(i: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; TEXT_DIM];
        v[i] = 1.0;
        v
    }

    #[test]
    fn text_store_add_search_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TextVectorStore::open(dir.path()).expect("open");
        let mut a = vec![0.0_f32; TEXT_DIM];
        a[0] = 1.0;
        let mut b = vec![0.0_f32; TEXT_DIM];
        b[1] = 1.0;
        store
            .add(vec![
                TextChunkRow {
                    asset_hex: "aa11".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 45_000,
                    model_tag: "minilm-l6-v2-v1".into(),
                    text: "budget discussion".into(),
                    vector: a.clone(),
                },
                TextChunkRow {
                    asset_hex: "bb22".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 30_000,
                    model_tag: "minilm-l6-v2-v1".into(),
                    text: "cat video".into(),
                    vector: b,
                },
                TextChunkRow {
                    asset_hex: "cc33".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 10_000,
                    model_tag: "other-model".into(),
                    text: "unrelated model".into(),
                    vector: a.clone(),
                },
            ])
            .expect("add");
        let hits = store.search(&a, "minilm-l6-v2-v1", 10).expect("search");
        assert_eq!(hits[0].asset_hex, "aa11");
        assert_eq!(hits[0].text, "budget discussion");
        assert_eq!(hits[0].start_ms, 0);
        assert!(
            hits.iter().all(|h| h.asset_hex != "cc33"),
            "model_tag filter applies"
        );
    }

    #[test]
    fn text_store_search_ranks_closer_vector_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TextVectorStore::open(dir.path()).expect("open");
        store
            .add(vec![
                TextChunkRow {
                    asset_hex: "aa11".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 1_000,
                    model_tag: "m1".into(),
                    text: "near".into(),
                    vector: text_unit(0),
                },
                TextChunkRow {
                    asset_hex: "bb22".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 1_000,
                    model_tag: "m1".into(),
                    text: "far".into(),
                    vector: text_unit(1),
                },
            ])
            .expect("add");

        let hits = store.search(&text_unit(0), "m1", 10).expect("search");
        assert_eq!(hits[0].asset_hex, "aa11", "nearest by dot product");
        assert_eq!(hits[1].asset_hex, "bb22");
    }

    #[test]
    fn text_store_rejects_wrong_dim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TextVectorStore::open(dir.path()).expect("open");
        let row = TextChunkRow {
            asset_hex: "aa".into(),
            source: "transcript".into(),
            start_ms: 0,
            end_ms: 1,
            model_tag: "m".into(),
            text: "t".into(),
            vector: vec![0.0; 3],
        };
        assert!(store.add(vec![row]).is_err());
    }

    #[test]
    fn text_store_existing_keys_and_distinct_assets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TextVectorStore::open(dir.path()).expect("open");
        store
            .add(vec![TextChunkRow {
                asset_hex: "aa11".into(),
                source: "transcript".into(),
                start_ms: 5,
                end_ms: 6,
                model_tag: "m1".into(),
                text: "t".into(),
                vector: vec![0.0; TEXT_DIM],
            }])
            .expect("add");
        let keys = store.existing_keys("m1").expect("keys");
        assert!(keys.contains(&("aa11".to_string(), 5)));
        assert!(store.existing_keys("other").expect("keys").is_empty());
        assert_eq!(store.distinct_assets("m1").expect("assets").len(), 1);
    }

    #[test]
    fn text_store_coexists_with_image_store_in_same_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let image_store = VectorStore::open(dir.path()).expect("image open");
        let text_store = TextVectorStore::open(dir.path()).expect("text open");
        drop(image_store);
        drop(text_store);
        let reopened = TextVectorStore::open_existing(dir.path()).expect("reopen");
        assert!(reopened.is_some());
    }

    #[test]
    fn text_store_open_existing_returns_none_on_truly_empty_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            TextVectorStore::open_existing(dir.path())
                .expect("open_existing on an empty dir")
                .is_none()
        );
    }

    #[test]
    fn text_store_open_existing_returns_none_when_only_image_table_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        VectorStore::open(dir.path())
            .expect("image open")
            .add(vec![VectorRow {
                asset_hex: "aa11".into(),
                kind: "image".into(),
                ts_ms: -1,
                model_tag: "m1".into(),
                vector: unit(0),
            }])
            .expect("image add");

        assert!(
            TextVectorStore::open_existing(dir.path())
                .expect("open_existing")
                .is_none(),
            "a dir holding only the image vectors table has no text_chunks table"
        );
    }

    #[test]
    fn text_store_reopen_persists_and_empty_add_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let store = TextVectorStore::open(dir.path()).expect("open");
            store
                .add(vec![TextChunkRow {
                    asset_hex: "aa11".into(),
                    source: "transcript".into(),
                    start_ms: 0,
                    end_ms: 1_000,
                    model_tag: "m1".into(),
                    text: "t".into(),
                    vector: text_unit(0),
                }])
                .expect("add");

            // Lance versions every write that reaches the table, even an
            // empty one (see `Table::version`'s doc comment: "Every
            // operation that modifies the table increases version") — so a
            // real short-circuit and a deleted one are both silently
            // `Ok(())` by row count alone. Pinning the version number too
            // is what actually catches a mutant that deletes the
            // `is_empty` guard.
            let version_before = store.rt.block_on(store.table.version()).expect("version");
            store.add(vec![]).expect("empty add is a noop");
            let version_after = store.rt.block_on(store.table.version()).expect("version");
            assert_eq!(
                version_before, version_after,
                "an empty add must short-circuit before ever reaching the table, \
                 not commit a spurious empty write"
            );
        }

        let store = TextVectorStore::open(dir.path()).expect("reopen");
        let keys = store.existing_keys("m1").expect("keys");
        assert_eq!(keys.len(), 1);
        assert!(keys.contains(&("aa11".to_string(), 0)));
    }
}
