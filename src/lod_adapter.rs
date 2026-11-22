// SPDX-License-Identifier: GPL-3.0-only
// Injected into Horizon's CPU LOD module. Read existing immutable samples only.
use horizon_client::lod::{Lod, Surface, TileKey};
use std::{collections::BTreeMap, sync::{Arc, Weak}};

pub struct Snapshot {
    pub key: TileKey,
    pub source: Weak<dyn Send + Sync>,
    pub surfaces: [Option<Surface>; 256],
}

pub fn priority(
    x: i32,
    z: i32,
    level: u8,
    focus: [f64; 2],
    span: [f64; 2],
) -> (bool, std::cmp::Reverse<u8>, u64, i32, i32) {
    let size = (16_i64 << level.min(22)) as f64;
    let dx = (focus[0] - (f64::from(x) + 0.5) * size).abs() - size / 2.;
    let dz = (focus[1] - (f64::from(z) + 0.5) * size).abs() - size / 2.;
    (
        dx > span[0] / 2. || dz > span[1] / 2.,
        std::cmp::Reverse(level),
        (dx.max(0.).powi(2) + dz.max(0.).powi(2)) as u64,
        x,
        z,
    )
}

pub fn snapshots(
    lod: &Lod,
    known: &BTreeMap<(i32, i32, u8), usize>,
    focus: [f64; 2],
    span: [f64; 2],
    limit: usize,
) -> Vec<Snapshot> {
    let limit = limit.min(8);
    if limit == 0 {
        return Vec::new();
    }
    let mut available: Vec<_> = lod
        .ready
        .iter()
        .filter(|(key, columns)| {
            known.get(&(key.x, key.z, key.level)).copied() != Some(Arc::as_ptr(columns) as usize)
        })
        .map(|(key, columns)| (priority(key.x, key.z, key.level, focus, span), key, columns))
        .collect();
    if available.len() > limit {
        available.select_nth_unstable_by_key(limit, |v| v.0);
        available.truncate(limit);
    }
    available.sort_unstable_by_key(|v| v.0);
    available
        .into_iter()
        .take(limit)
        .map(|(_, key, columns)| {
            let source: Arc<dyn Send + Sync> = columns.clone();
            let [x, _, z] = key.origin();
            let stride = key.size() / 16;
            // The engine packs columns privately. Sample its displayed surface at
            // the center of each cell, preferring resident finer samples.
            let surfaces = std::array::from_fn(|i| lod.surface(
                x + (i as i32 % 16) * stride + stride / 2,
                z + (i as i32 / 16) * stride + stride / 2,
            ));
            Snapshot { key: *key, source: Arc::downgrade(&source), surfaces }
        })
        .collect()
}
