// SPDX-License-Identifier: GPL-3.0-only
use super::{Game, minimap_pages};
use ::trailgrid_maps::radar::resource_id;
use horizon_client::{
    hud::{self, HudBatch, pictures::Picture},
    resource_source::ResourceSource,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const ROOT: &str = "assets/xaerominimap/entity/icon";
const MAX_DEFINITIONS: usize = 1024;
const MAX_PICTURES: usize = 256;
const MAX_IMAGE_SIDE: u32 = 128;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Sprite {
    file: String,
    outline: bool,
    flipped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Form {
    Sprite(Sprite),
    Item(String),
    Dot,
    Model,
}

impl Form {
    fn parse(source: &str) -> Option<Self> {
        match source {
            "dot" => return Some(Self::Dot),
            "model" | "item" => return Some(Self::Model),
            _ => {}
        }
        let (kind, path) = source.split_once(':')?;
        match kind {
            "model" => Some(Self::Model),
            "item" => resource_id(path).map(Self::Item),
            "sprite" | "normal_sprite" | "outlined_sprite"
                if path.ends_with(".png")
                    && path.len() <= 180
                    && path.split('/').all(|part| {
                        !part.is_empty()
                            && part != "."
                            && part != ".."
                            && part.bytes().all(|c| {
                                c.is_ascii_lowercase() || c.is_ascii_digit() || b"_.-".contains(&c)
                            })
                    }) =>
            {
                Some(Self::Sprite(Sprite {
                    file: path.to_owned(),
                    outline: kind == "outlined_sprite",
                    flipped: kind == "sprite",
                }))
            }
            _ => None,
        }
    }
}

#[derive(Default)]
struct Definition(BTreeMap<String, Form>);

impl Definition {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 128 * 1024 {
            return None;
        }
        let value = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
        let forms = value.get("variants")?.as_object()?;
        let variants = forms
            .iter()
            .take(512)
            .filter_map(|(key, form)| {
                (key.len() <= 512).then_some(())?;
                Some((key.clone(), Form::parse(form.as_str()?)?))
            })
            .collect();
        Some(Self(variants))
    }

    fn choose(&self, texture: Option<&str>) -> Option<&Form> {
        let variant = if self.0.len() == 1 && self.0.contains_key("default") {
            None
        } else {
            texture.and_then(|texture| {
                let (namespace, path) = texture.split_once(':').unwrap_or(("minecraft", texture));
                self.0.get(&format!("{namespace}:textures/{path}.png"))
            })
        };
        variant.or_else(|| self.0.get("default"))
    }
}

#[derive(Default)]
struct Cache {
    definitions: BTreeMap<String, Option<Definition>>,
    pictures: BTreeMap<Sprite, Option<Arc<Picture>>>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    definitions: BTreeMap::new(),
    pictures: BTreeMap::new(),
});

pub(super) fn invalidate() {
    *CACHE.lock().unwrap_or_else(|error| error.into_inner()) = Cache::default();
}

fn read_definition(resources: &ResourceSource, kind: &str) -> Option<Definition> {
    let id = resource_id(kind)?;
    let (namespace, path) = id.split_once(':')?;
    let bytes = resources
        .read_limited(
            &format!("{ROOT}/definition/{namespace}/{path}.json"),
            128 * 1024,
        )
        .ok()?;
    Definition::parse(&bytes)
}

fn crop(width: u32, height: u32, pixels: &[u8], sprite: &Sprite) -> Option<(u32, u32, Vec<u8>)> {
    let mut bounds = [width, height, 0, 0];
    for y in 0..height {
        for x in 0..width {
            if pixels[((y * width + x) * 4 + 3) as usize] != 0 {
                bounds[0] = bounds[0].min(x);
                bounds[1] = bounds[1].min(y);
                bounds[2] = bounds[2].max(x + 1);
                bounds[3] = bounds[3].max(y + 1);
            }
        }
    }
    if bounds[0] >= bounds[2] {
        return None;
    }
    let padding = u32::from(sprite.outline);
    let w = bounds[2] - bounds[0] + 2 * padding;
    let h = bounds[3] - bounds[1] + 2 * padding;
    let mut rgba = vec![0; (w * h * 4) as usize];
    for y in bounds[1]..bounds[3] {
        for x in bounds[0]..bounds[2] {
            let src = ((y * width + x) * 4) as usize;
            let dst_x = x - bounds[0] + padding;
            let dst_y = y - bounds[1] + padding;
            let dst_y = if sprite.flipped { h - 1 - dst_y } else { dst_y };
            let dst = ((dst_y * w + dst_x) * 4) as usize;
            rgba[dst..dst + 4].copy_from_slice(&pixels[src..src + 4]);
        }
    }
    if sprite.outline {
        let original = rgba.clone();
        for y in 0..h {
            for x in 0..w {
                let dst = ((y * w + x) * 4) as usize;
                if original[dst + 3] != 0 {
                    continue;
                }
                let alpha = [
                    x.checked_sub(1).map(|x| (x, y)),
                    y.checked_sub(1).map(|y| (x, y)),
                    (x + 1 < w).then_some((x + 1, y)),
                    (y + 1 < h).then_some((x, y + 1)),
                ]
                .into_iter()
                .flatten()
                .map(|(x, y)| original[((y * w + x) * 4 + 3) as usize])
                .max()
                .unwrap_or(0);
                rgba[dst + 3] = alpha;
            }
        }
    }
    Some((w, h, rgba))
}

fn read_picture(resources: &ResourceSource, sprite: &Sprite) -> Option<Arc<Picture>> {
    let bytes = resources
        .read_limited(&format!("{ROOT}/sprite/{}", sprite.file), 256 * 1024)
        .ok()?;
    if bytes.len() > 256 * 1024
        || bytes.get(0..8)? != b"\x89PNG\r\n\x1a\n"
        || bytes.get(12..16)? != b"IHDR"
    {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    if !(1..=MAX_IMAGE_SIDE).contains(&width) || !(1..=MAX_IMAGE_SIDE).contains(&height) {
        return None;
    }
    let (width, height, pixels) = hud::decode_bytes(&bytes).ok()?;
    let (w, h, rgba) = crop(width, height, &pixels, sprite)?;
    Some(minimap_pages::create(w, h, rgba))
}

pub(super) fn render(
    game: &Game<'_>,
    batch: &mut HudBatch,
    mob: Option<&horizon_client::mobs::Mob>,
    kind: &str,
    dst: [f32; 4],
    opacity: u32,
) -> Option<bool> {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if !cache.definitions.contains_key(kind) && cache.definitions.len() < MAX_DEFINITIONS {
        cache
            .definitions
            .insert(kind.to_owned(), read_definition(&game.pack.source, kind));
    }
    let def = cache.definitions.get(kind)?.as_ref()?;
    let texture = if def.0.len() > 1 || !def.0.contains_key("default") {
        mob.map(|mob| horizon_client::mobs::render::body(mob).1)
    } else {
        None
    };
    match def.choose(texture.as_deref())?.clone() {
        Form::Dot => Some(false),
        Form::Model => None,
        Form::Item(id) => {
            drop(cache);
            let item = horizon_client::items::ItemStack::new(id.as_str(), 1);
            let start = batch.vertices.len();
            let drawn = game.native_item_stack(batch, &item, [dst[0], dst[1]], dst[2]);
            super::reworked_maps::set_opacity(batch, start, ((opacity >> 24) * 100 / 255) as u8);
            drawn.then_some(true)
        }
        Form::Sprite(sprite) => {
            if !cache.pictures.contains_key(&sprite) && cache.pictures.len() < MAX_PICTURES {
                let picture = read_picture(&game.pack.source, &sprite);
                cache.pictures.insert(sprite.clone(), picture);
            }
            let picture = cache.pictures.get(&sprite)?.as_ref()?.clone();
            drop(cache);
            let [x, y, w, h] = dst.map(f64::from);
            let [pw, ph] = [f64::from(picture.width), f64::from(picture.height)];
            let scale = (w / pw).min(h / ph);
            let left = x + (w - scale * pw) / 2.;
            let top = y + (h - scale * ph) / 2.;
            let right = left + scale * pw;
            let bottom = top + scale * ph;
            let first = batch.vertices.len();
            minimap_pages::paint(
                batch,
                &picture,
                &[
                    [left, top, 0., 0.],
                    [right, top, pw, 0.],
                    [right, bottom, pw, ph],
                    [left, bottom, 0., ph],
                ],
            );
            super::reworked_maps::set_opacity(batch, first, ((opacity >> 24) * 100 / 255) as u8);
            Some(true)
        }
    }
}
