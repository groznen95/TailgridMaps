// SPDX-License-Identifier: GPL-3.0-only
use super::{
    Game, minimap_compat::Snapshot,
    reworked_maps::{LeafPalette, sample_chunk_tile},
};
use ::trailgrid_maps::runtime::{Dimension, maps::{self, Layer, SavedTiles}};
use horizon_client::chunk::LevelShape;
use horizon_core::{nbt::Limits, position::ChunkPos};
use horizon_world::region::RegionFile;
use std::{fs, io, path::{Component, Path}, sync::Arc};

pub(super) fn reader(game: &Game<'_>, leaves: Option<Arc<LeafPalette>>) -> Result<SavedTiles, String> {
    let root = if let Some(server) = game.integrated.as_ref() {
        let name = Path::new(&server.save_folder);
        let mut components = name.components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err("Invalid save folder".into());
        }
        std::env::current_dir().map_err(|error| error.to_string())?.join("saves").join(name)
    } else {
        game.native_data_dir(::trailgrid_maps::runtime::MOD_ID).join("world-import")
    };
    let resources = Arc::clone(&game.res);
    let biomes = game.world_biomes.as_ref().unwrap_or_else(|| game.res.biomes()).clone();
    let registry = game.pack.assets.registry;
    let air = registry.default_state("minecraft:air").ok_or("Missing air block")?;
    let mut dimension = Dimension::Player;
    let mut roots = [Dimension::Overworld, Dimension::Nether, Dimension::End].into_iter();
    let mut entries = None::<fs::ReadDir>;
    let mut region = None::<(RegionFile, std::vec::IntoIter<ChunkPos>)>;
    let mut checked = false;
    Ok(Box::new(std::iter::from_fn(move || {
        if !checked {
            checked = true;
            if !root.join("level.dat").is_file() {
                return Some(Err(io::Error::new(io::ErrorKind::NotFound,
                    "Open a local world or put a copy of this server's save in world-import")));
            }
        }
        if let Some((file, positions)) = &mut region {
            if let Some(pos) = positions.next() {
                let result = (|| {
                    if !(-1_875_000..1_875_000).contains(&pos.x) || !(-1_875_000..1_875_000).contains(&pos.z) {
                        return Ok(None);
                    }
                    let Some(record) = file.read(pos)? else { return Ok(None); };
                    let nbt = record.nbt(Limits::default())?;
                    let shape = match dimension {
                        Dimension::Overworld => LevelShape::OVERWORLD,
                        Dimension::Nether => LevelShape::NETHER,
                        Dimension::End => LevelShape::END,
                        Dimension::Player => unreachable!("regions have a saved dimension"),
                    };
                    let Some(chunk) = horizon_client::save::decode_with(&nbt, registry, &biomes, None, shape.column()) else {
                        return Ok(None);
                    };
                    if (chunk.x, chunk.z) != (pos.x, pos.z) {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "Saved chunk does not match the region slot"));
                    }
                    let snapshot = Snapshot::saved(chunk, shape.min_y..shape.max_y(), air);
                    let (tile, _) = sample_chunk_tile(&snapshot, &resources, &biomes, leaves.as_deref(),
                        (pos.x, pos.z), Layer::Surface, maps::Appearance::default());
                    Ok(Some((dimension, [pos.x, pos.z], tile)))
                })();
                return Some(result);
            }
            region = None;
        }
        if let Some(files) = entries.as_mut() {
            if let Some(entry) = files.next() {
                return Some((|| {
                    let entry = entry?;
                    if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|ext| ext == "mca") {
                        let file = RegionFile::open(entry.path())?;
                        let positions = file.chunks().map(|(pos, _)| pos).collect::<Vec<_>>();
                        region = Some((file, positions.into_iter()));
                    }
                    Ok(None)
                })());
            }
        }
        dimension = roots.next()?;
        let modern = horizon_client::save::dimension_dir(&root, dimension.id()).join("region");
        let legacy = match dimension {
            Dimension::Nether => root.join("DIM-1/region"),
            Dimension::End => root.join("DIM1/region"),
            _ => root.join("region"),
        };
        match fs::read_dir(modern).or_else(|error| {
            if error.kind() == io::ErrorKind::NotFound { fs::read_dir(legacy) } else { Err(error) }
        }) {
            Ok(files) => { entries = Some(files); Some(Ok(None)) }
            Err(error) if error.kind() == io::ErrorKind::NotFound => { entries = None; Some(Ok(None)) }
            Err(error) => { entries = None; Some(Err(error)) }
        }
    })))
}
