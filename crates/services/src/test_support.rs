//! Shared fixtures for this crate's unit tests: a catalog with N scanned
//! assets, addressable by index. Extracted when a third copy of the same
//! `seeded_app` appeared — the two shapes it had grown (one returning the
//! single asset id alongside the app, one taking a count) collapse into the
//! count form plus [`asset_id`], so a test that needs one asset and a test
//! that needs three read the same way.
use crate::app::FsApp;
use majestical_core::event::{AssetId, Op};
use std::path::Path;

/// The `n`th fixture asset's id: a well-formed `xxh3:` id, distinct per `n`.
pub(crate) fn asset_id(n: u8) -> AssetId {
    AssetId(format!("xxh3:{n:032x}"))
}

/// A catalog at `<dir>/cat` holding `count` scanned assets — each one a
/// real `AssetSeen`, so the "unknown asset" guards see them as known.
pub(crate) fn seeded_app(dir: &Path, count: u8) -> FsApp {
    let mut app = FsApp::init(&dir.join("cat"), "m1", "m1").expect("init");
    let ops = (0..count)
        .map(|i| Op::AssetSeen {
            asset: asset_id(i),
            volume: "vol1".into(),
            path: format!("clip{i}.txt"),
            size: 5,
            mtime_ms: 1000,
        })
        .collect();
    app.emit(ops).expect("emit");
    app
}
