// SPDX-License-Identifier: GPL-3.0-only
use super::{Game, minimap_pages};
use ::trailgrid_maps::{radar::resource_id, runtime::maps::{AtlasPage, Coverage, Frame, RELIEF_HEIGHT_LIMIT, Tile}};
use horizon_client::{chunk::Chunk, hud::{self, HudBatch, pictures::Picture}};
use horizon_core::geometry::Direction;
use horizon_world::registry::BlockStateId;
use std::{collections::BTreeMap, sync::{Arc, Weak}};

const DETAIL: usize = 4;
const CHUNK: usize = 16;
const SIZE: usize = CHUNK * DETAIL;

struct DetailTile {
    page: Weak<AtlasPage>,
    terrain: Weak<Tile>,
    live: Weak<Chunk>,
    slot: usize,
    picture: Arc<Picture>,
}

#[derive(Default)]
pub struct DetailCache {
    resource_version: Option<u64>,
    names: Vec<Option<Arc<str>>>,
    textures: BTreeMap<Arc<str>, Option<[i16; DETAIL * DETAIL]>>,
    tiles: BTreeMap<[i32; 2], DetailTile>,
}

impl DetailCache {
    pub fn clear(&mut self) { self.tiles.clear(); }

    pub fn picture(&self, chunk: [i32; 2], page: &Arc<AtlasPage>, slot: usize) -> Option<&Arc<Picture>> {
        let entry = self.tiles.get(&chunk)?;
        (entry.slot == slot && entry.page.as_ptr() == Arc::as_ptr(page)).then_some(&entry.picture)
    }

    pub fn refresh(&mut self, game: &Game<'_>, frame: &Frame, coverage: &Coverage) {
        let Some(atlas) = frame.atlas.as_ref().filter(|atlas| !atlas.pages.is_empty()) else {
            self.clear();
            return;
        };
        let version = game.block_sprites_version();
        if self.resource_version != Some(version) {
            self.clear();
            self.textures.clear();
            self.names = top_textures(game);
            self.resource_version = Some(version);
        }
        let view = &frame.view;
        let extent = view.relief.as_ref().map_or(view.span.map(|size| size / 2.), |relief| {
            let (s,c) = view.angle.sin_cos();
            let width = view.span[0] / 2.;
            let height = (view.span[1] / 2. + RELIEF_HEIGHT_LIMIT * relief.pitch.sin()) / relief.pitch.cos();
            [width*c.abs()+height*s.abs(),width*s.abs()+height*c.abs()]
        });
        let visible = |[x, z]: [i32; 2]| {
            [x, z].into_iter().enumerate().all(|(axis, chunk)| {
                (f64::from(chunk) * 16. + 8. - view.center[axis]).abs() < extent[axis] + 9.
            })
        };
        self.tiles.retain(|&chunk, _| visible(chunk) && game.world.chunks.contains_key(&(chunk[0], chunk[1])));
        let mut remaining = 4;
        for (&chunk, &slot) in atlas.chunks.iter().zip(&atlas.slots) {
            if remaining == 0 { break; }
            if !visible(chunk) || self.tiles.len() >= 160 && !self.tiles.contains_key(&chunk) { continue; }
            let Some(live) = game.world.chunks.get(&(chunk[0], chunk[1])) else { continue; };
            let Some(terrain) = coverage.detailed_tile(view.dimension, view.layer, chunk) else { continue; };
            let Some(page) = atlas.pages.get(slot / (CHUNK * 16)) else { continue; };
            if self.tiles.get(&chunk).is_some_and(|entry| entry.slot == slot
                && entry.page.as_ptr() == Arc::as_ptr(page)
                && entry.terrain.as_ptr() == Arc::as_ptr(terrain)
                && entry.live.as_ptr() == Arc::as_ptr(live)) { continue; }
            let pixels = self.tile_pixels(game, live, terrain, page, slot);
            self.tiles.insert(chunk, DetailTile {
                page: Arc::downgrade(page), terrain: Arc::downgrade(terrain), live: Arc::downgrade(live), slot,
                picture: minimap_pages::create(SIZE as u32, SIZE as u32, pixels),
            });
            remaining -= 1;
        }
    }

    fn tile_pixels(&mut self, game: &Game<'_>, chunk: &Chunk, terrain: &Tile, page: &AtlasPage, slot: usize) -> Vec<u8> {
        let origin = [(slot % 64) * CHUNK, (slot / 64 * CHUNK) % 64];
        let mut out = vec![0; SIZE * SIZE * 4];
        for z in 0..CHUNK {
            for x in 0..CHUNK {
                let index = ((origin[1] + z) * page.width + origin[0] + x) * 4;
                let color = &page.pixels[index..index + 4];
                if color[3] == 0 { continue; }
                let y = terrain.heights[z * CHUNK + x];
                let id = chunk.block(x as i32, y, z as i32);
                let pattern = self.names.get(id.0 as usize).and_then(Option::as_ref).and_then(|name| {
                    self.textures.entry(name.clone()).or_insert_with(|| texture_pattern(game, name)).as_ref()
                });
                for dz in 0..DETAIL {
                    for dx in 0..DETAIL {
                        let pixel = ((z * DETAIL + dz) * SIZE + x * DETAIL + dx) * 4;
                        let shade = pattern.map_or(0, |pattern| pattern[dz * DETAIL + dx]);
                        for channel in 0..3 {
                            let value = i32::from(color[channel]);
                            out[pixel + channel] = (value + value * i32::from(shade) / 128).clamp(0, 255) as u8;
                        }
                        out[pixel + 3] = color[3];
                    }
                }
            }
        }
        out
    }
}

fn top_textures(game: &Game<'_>) -> Vec<Option<Arc<str>>> {
    let names = horizon_client::block_sprites::texture_names(&game.pack.context.render.sprite_layers);
    let lookup = horizon_client::block_sprites::Lookup::new(&game.pack.context,
        game.renderer.as_ref().map_or(u32::MAX, |renderer| renderer.sprite_split()), &names);
    let mut unique = BTreeMap::<String, Arc<str>>::new();
    game.res.facts().states.iter().enumerate().map(|(index, state)| {
        if !matches!(state.fluid, horizon_client::blocks::Fluid::None)
            || state.name.contains("glass") || state.name.contains("portal") { return None; }
        let sprites = lookup.state(BlockStateId(index as u32));
        let face = sprites.sides[Direction::Up as usize].first()?;
        let name = face.texture.as_str();
        Some(unique.entry(name.to_owned()).or_insert_with(|| Arc::from(name)).clone())
    }).collect()
}

fn texture_pattern(game: &Game<'_>, name: &str) -> Option<[i16; DETAIL * DETAIL]> {
    let id = resource_id(name)?;
    let (namespace, path) = id.split_once(':')?;
    let path = path.strip_prefix("textures/").unwrap_or(path);
    let path = path.strip_suffix(".png").unwrap_or(path);
    let bytes = game.pack.source.read_limited(&format!("assets/{namespace}/textures/{path}.png"), 512 * 1024).ok()?;
    if bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" || bytes.get(12..16)? != b"IHDR" { return None; }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?) as usize;
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?) as usize;
    if !(DETAIL..=1024).contains(&width) || !(DETAIL..=2048).contains(&height) { return None; }
    let (_, _, pixels) = hud::decode_bytes(&bytes).ok()?;
    let frame_height = width.min(height);
    let luminance: [i16; DETAIL * DETAIL] = std::array::from_fn(|i| {
        let (x, z) = (i % DETAIL, i / DETAIL);
        let mut total = 0;
        let mut alpha = 0;
        for row in z * frame_height / DETAIL..(z + 1) * frame_height / DETAIL {
            for col in x * width / DETAIL..(x + 1) * width / DETAIL {
                let index = (row * width + col) * 4;
                let pixel = &pixels[index..index + 4];
                let luma = (3 * u32::from(pixel[0]) + 6 * u32::from(pixel[1]) + u32::from(pixel[2])) / 10;
                total += luma * u32::from(pixel[3]);
                alpha += u32::from(pixel[3]);
            }
        }
        total.checked_div(alpha).map_or(-1, |value| value as i16)
    });
    let mut lit = luminance.into_iter().filter(|&value| value >= 0).peekable();
    lit.peek()?;
    let (sum, count) = lit.fold((0, 0), |(sum, count), value| (sum + value, count + 1));
    let mean = sum / count;
    Some(luminance.map(|value| if value < 0 { 0 } else { (value - mean).clamp(-36, 36) }))
}

/// Paint the visible height field back-to-front. The atlas still owns the
/// colors, so resource textures and biome shading match the flat map.
pub fn paint_surface(
    game: &Game<'_>, batch: &mut HudBatch, frame: &Frame, detail: &DetailCache,
    pages: &[(Arc<AtlasPage>, Arc<Picture>)], rect: [f32; 4],
    claim_color: impl Fn([i32; 2]) -> Option<(u32, u32)>,
) -> bool {
    let view = &frame.view;
    let (Some(relief), Some(atlas)) = (&view.relief, &frame.atlas) else { return false; };
    let stride = relief.stride;
    let (sin, cos) = view.angle.sin_cos();
    let face_margin = (2. * RELIEF_HEIGHT_LIMIT * relief.pitch.sin() * f64::from(rect[3]) / view.span[1]) as f32;
    let to_screen = |point, height| {
        let [x,y] = view.screen_at_height(point, height);
        [rect[0] + x as f32 * rect[2] / view.width as f32,
            rect[1] + y as f32 * rect[3] / view.height as f32]
    };
    let corners = |x: i32, z: i32, size: i32| [
        [f64::from(x), f64::from(z)], [f64::from(x+size), f64::from(z)],
        [f64::from(x+size), f64::from(z+size)], [f64::from(x), f64::from(z+size)],
    ];
    let visible = |quad: [[f32; 2]; 4]| {
        (0..2).all(|axis| quad.iter().any(|p| p[axis] >= rect[axis])
            && quad.iter().any(|p| p[axis] <= rect[axis] + rect[axis+2]))
    };
    struct Source<'a> {
        picture: &'a Arc<Picture>,
        page: &'a AtlasPage,
        origin: [usize; 2],
        detailed: bool,
        claim: Option<(u32,u32)>,
    }
    struct Cell {
        source: usize,
        at: [i32; 2],
        local: [usize; 2],
        height: i32,
        quad: [[f32; 2]; 4],
        depth: f64,
    }
    let mut sources = Vec::new();
    let mut cells = Vec::new();
    for (&chunk, &slot) in atlas.chunks.iter().zip(&atlas.slots) {
        let Some(tile) = relief.coverage.detailed_tile(view.dimension, view.layer, chunk) else { continue; };
        let Some((page, image)) = pages.get(slot / 256) else { continue; };
        let [x, z] = chunk.map(|axis| axis * 16);
        let low = (relief.reference - RELIEF_HEIGHT_LIMIT).floor() as i32;
        let high = (relief.reference + RELIEF_HEIGHT_LIMIT).ceil() as i32;
        let bounds = corners(x, z, 16);
        let mut min = [f32::INFINITY;2];
        let mut max = [f32::NEG_INFINITY;2];
        for point in bounds {
            for height in [low, high] {
                let screen = to_screen(point, Some(height));
                for axis in 0..2 { min[axis]=min[axis].min(screen[axis]); max[axis]=max[axis].max(screen[axis]); }
            }
        }
        if (0..2).any(|axis| max[axis] < rect[axis] || min[axis] > rect[axis] + rect[axis+2]) { continue; }
        let origin = [slot % 64 * 16, slot / 64 * 16 % 64];
        let detail = detail.picture(chunk, page, slot);
        let picture = detail.unwrap_or(image);
        if !batch.pictures.iter().any(|p| p.id == picture.id) { batch.pictures.push(picture.clone()); }
        let source = sources.len();
        sources.push(Source { picture, page, origin, detailed: detail.is_some(), claim: claim_color(chunk) });
        for dz in (0..16).step_by(stride as usize) {
            for dx in (0..16).step_by(stride as usize) {
                let index = dz * 16 + dx;
                if tile.pixels.get(index*4+3).copied().unwrap_or(0) == 0 { continue; }
                let height = tile.heights[index];
                let at = [x + dx as i32, z + dz as i32];
                let quad = corners(at[0],at[1],stride).map(|p| to_screen(p, Some(height)));
                if !visible(quad) && !quad.iter().any(|p| p[0] >= rect[0] && p[0] <= rect[0]+rect[2]
                    && p[1] < rect[1] && p[1] >= rect[1] - face_margin) { continue; }
                let depth = -(f64::from(at[0]) - view.center[0]) * sin + (f64::from(at[1]) - view.center[1]) * cos;
                cells.push(Cell { source, at, local: [dx,dz], height, quad, depth });
            }
        }
    }
    cells.sort_unstable_by(|a,b| a.depth.total_cmp(&b.depth));
    let shadow = |color: &[u8], amount: u16| {
        let shade = |value| (u16::from(value) * amount / 100) as u8;
        u32::from_be_bytes([255, shade(color[0]), shade(color[1]), shade(color[2])])
    };
    let x_face = if sin <= 0. { (1,2,[stride,0]) } else { (3,0,[-stride,0]) };
    let z_face = if cos >= 0. { (2,3,[0,stride]) } else { (0,1,[0,-stride]) };
    let screen = rect;
    for cell in &cells {
        let source = &sources[cell.source];
        let [x,z] = cell.local;
        let color = ((source.origin[1]+z)*source.page.width + source.origin[0]+x)*4;
        let bounds = corners(cell.at[0],cell.at[1],stride);
        for ((a,b,[dx,dz]),shade) in [(x_face,70),(z_face,83)] {
            let lower = view.terrain_height([cell.at[0]+dx,cell.at[1]+dz]).unwrap_or(cell.height.saturating_sub(1));
            if lower < cell.height {
                let side = [cell.quad[a],cell.quad[b],to_screen(bounds[b],Some(lower)),to_screen(bounds[a],Some(lower))];
                if visible(side) { game.native_quad(batch, side, shadow(&source.page.pixels[color..color+4],shade)); }
            }
        }
        if !visible(cell.quad) { continue; }
        let (uv_origin, factor) = if source.detailed { ([0;2],DETAIL) } else { (source.origin,1) };
        let u = (uv_origin[0] + x*factor) as f32;
        let v = (uv_origin[1] + z*factor) as f32;
        let step = (stride as usize * factor) as f32;
        let uv = [[u,v],[u+step,v],[u+step,v+step],[u,v+step]]
            .map(|[u,v]| [u/source.picture.width as f32, v/source.picture.height as f32]);
        let start = batch.vertices.len();
        batch.quad(cell.quad,uv,u32::MAX);
        batch.blend_since(start,hud::Blend::Picture(source.picture.id));
        if let Some((fill,border)) = source.claim {
            game.native_quad(batch,cell.quad,fill);
            let step = stride as usize;
            for (at_edge, a,b) in [(z==0,0,1),(x+step==16,1,2),(z+step==16,2,3),(x==0,3,0)] {
                if at_edge { super::reworked_maps::draw_line(game,batch,cell.quad[a],cell.quad[b],screen,border); }
            }
        }
    }
    !cells.is_empty()
}
