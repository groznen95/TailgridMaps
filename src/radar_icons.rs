// SPDX-License-Identifier: GPL-3.0-only
use super::{Game, minimap_pages};
use ::trailgrid_maps::radar::resource_id;
use horizon_client::{
    hud::{self, HudBatch, pictures::Picture},
    mobs::{Mob, Part, render as models},
    resource_source::ResourceSource,
};
use glam::{EulerRot, Mat4, Quat, Vec3};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

const ROOT: &str = "assets/xaerominimap/entity/icon";
const MAX_DEFINITIONS: usize = 1024;
const MAX_PICTURES: usize = 256;
const MAX_IMAGE_SIDE: u32 = 128;
const MODEL_SIZE: usize = 48;
const MAX_MODEL_FACES: usize = 4096;

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
    Model(Option<usize>),
}

impl Form {
    fn parse(source: &str) -> Option<Self> {
        match source {
            "dot" => return Some(Self::Dot),
            "model" | "item" => return Some(Self::Model(None)),
            _ => {}
        }
        let (kind, path) = source.split_once(':')?;
        match kind {
            "model" => path.parse::<usize>().ok().filter(|index| *index < 128).map(|index| Self::Model(Some(index))),
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
struct Definition {
    variants: BTreeMap<String, Form>,
    models: Vec<ModelConfig>,
}

#[derive(Clone, Default)]
struct ModelConfig {
    scale: Option<f32>,
    rotation: [f32; 3],
    offset: [f32; 2],
    parts: Vec<String>,
    main: Vec<String>,
    root: Vec<Vec<String>>,
    full: bool,
    reset: bool,
    hide_layers: bool,
}

fn aliases(value: &serde_json::Value) -> Vec<String> {
    value.as_array().into_iter().flatten().take(64)
        .filter_map(serde_json::Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 256)
        .map(|name| name.rsplit(';').next().unwrap_or(name).to_owned()).collect()
}

impl ModelConfig {
    fn parse(value: &serde_json::Value) -> Self {
        let number = |key: &str, min: f32, max: f32| value[key].as_f64()
            .map(|n| n as f32).filter(|n| n.is_finite()).map(|n| n.clamp(min, max));
        let angle = |key| number(key, -360., 360.).unwrap_or(0.).to_radians();
        Self {
            scale: number("baseScale", 0.1, 8.),
            rotation: [angle("rotationX"), angle("rotationY"), angle("rotationZ")],
            offset: [number("offsetX", -24., 24.).unwrap_or(0.), number("offsetY", -24., 24.).unwrap_or(0.)],
            parts: aliases(&value["modelPartsFields"]),
            main: aliases(&value["modelMainPartFieldAliases"]),
            root: value["modelRootPath"].as_array().into_iter().flatten().take(16).map(aliases).collect(),
            full: value["renderingFullModel"].as_bool().unwrap_or(false),
            reset: value["modelPartsRotationReset"].as_bool().unwrap_or(false),
            hide_layers: value["layersAllowed"].as_bool() == Some(false),
        }
    }
}

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
        let models = value["modelConfigs"].as_array().into_iter().flatten().take(128).map(ModelConfig::parse).collect();
        Some(Self { variants, models })
    }

    fn choose(&self, texture: Option<&str>) -> Option<&Form> {
        let variant = if self.variants.len() == 1 && self.variants.contains_key("default") {
            None
        } else {
            texture.and_then(|texture| {
                let (namespace, path) = texture.split_once(':').unwrap_or(("minecraft", texture));
                self.variants.get(&format!("{namespace}:textures/{path}.png"))
            })
        };
        variant.or_else(|| self.variants.get("default"))
    }
}

#[derive(Default)]
struct Cache {
    definitions: BTreeMap<String, Option<Definition>>,
    pictures: BTreeMap<Sprite, Option<Arc<Picture>>>,
    models: BTreeMap<ModelKey, Option<Arc<Picture>>>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ModelKey {
    kind: String,
    config: Option<usize>,
    layers: Vec<(String, String, u32, bool)>,
}

static CACHE: Mutex<Cache> = Mutex::new(Cache {
    definitions: BTreeMap::new(),
    pictures: BTreeMap::new(),
    models: BTreeMap::new(),
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

fn named_parts<'a>(part: &'a Part, name: &str, out: &mut Vec<&'a Part>) {
    let name = if name == "headModel" { "head" } else { name };
    if part.name == name {
        out.push(part);
    } else {
        for child in &part.children { named_parts(child, name, out); }
    }
}

fn selected_parts<'a>(root: &'a Part, aliases: &[String]) -> Vec<&'a Part> {
    let mut out = Vec::new();
    for alias in aliases {
        let Some((field, selector)) = alias.split_once('[') else {
            if alias == "root" { out.push(root); } else { named_parts(root, alias, &mut out); }
            continue;
        };
        let Some(selector) = selector.strip_suffix(']') else { continue };
        if field == "children" {
            for name in selector.split(',').map(|name| name.trim().trim_matches(['\'', '"'])) {
                for part in &root.children {
                    if part.name == name { out.push(part); }
                }
            }
        } else {
            let mut group = Vec::new();
            named_parts(root, field, &mut group);
            if group.len() == 1 && !group[0].children.is_empty() {
                group = group[0].children.iter().collect();
            }
            out.extend(selector.split(',').filter_map(|i| group.get(i.trim().parse::<usize>().ok()?).copied()));
        }
    }
    out
}

fn default_head(part: &Part) -> Option<&Part> {
    if matches!(part.name.as_str(), "head" | "head1") || part.name.ends_with("_head") {
        return Some(part);
    }
    part.children.iter().find_map(default_head)
}

struct Face {
    points: [Vec3; 4],
    uv: [[f32; 2]; 4],
    shade: f32,
    layer: usize,
}

fn collect_faces(part: &Part, parent: Mat4, selected: &[&Part], drawing: bool, reset: bool,
                 layer: usize, faces: &mut Vec<Face>) {
    if faces.len() >= MAX_MODEL_FACES { return; }
    let picked = selected.iter().any(|selected| std::ptr::eq(*selected, part));
    let [x, y, z, rx, ry, rz, sx, sy, sz] = part.pose;
    let rotation = if picked && reset { Quat::IDENTITY } else { Quat::from_euler(EulerRot::ZYX, rz, ry, rx) };
    let transform = parent * Mat4::from_scale_rotation_translation(Vec3::new(sx, sy, sz), rotation, Vec3::new(x,y,z));
    let drawing = drawing || picked;
    if drawing {
        let light = Vec3::new(-0.35, -0.6, -1.).normalize();
        for polygon in part.polygons.iter().take(MAX_MODEL_FACES - faces.len()) {
            let points = polygon.vertices.map(|v| transform.transform_point3(Vec3::new(v[0],v[1],v[2])));
            if points.iter().any(|point| !point.is_finite()) { continue; }
            let normal = transform.transform_vector3(Vec3::from_array(polygon.normal)).normalize_or_zero();
            if polygon.cull_back && normal.z > 0. { continue; }
            faces.push(Face { points, uv: polygon.vertices.map(|v| [v[3],v[4]]),
                shade: 0.59 + 0.41 * normal.dot(light).max(0.), layer });
        }
    }
    for child in &part.children {
        collect_faces(child, transform, selected, drawing, reset, layer, faces);
    }
}

fn mesh(model: &Part, config: &ModelConfig, layer: usize, faces: &mut Vec<Face>) {
    let mut root = model;
    for aliases in &config.root {
        if let Some(candidate) = selected_parts(root, aliases).first() { root = candidate; }
    }
    let mut selected = selected_parts(root, &config.parts);
    if let Some(main) = selected_parts(root, &config.main).first() {
        selected.push(main);
    }
    if selected.is_empty() {
        selected.push(if config.full { root } else { default_head(root).unwrap_or(root) });
    }
    let [x, y, z] = config.rotation;
    let rotation = Mat4::from_quat(Quat::from_euler(EulerRot::YXZ, y + 0.48, x + 0.28, z));
    collect_faces(model, rotation, &selected, false, config.reset, layer, faces);
}

struct Texture {
    size: [usize; 2],
    rgba: Vec<u8>,
    tint: [u8; 4],
    bright: bool,
}

fn read_model_texture(resources: &ResourceSource, texture: &str, tint: u32, bright: bool) -> Option<Texture> {
    let id = resource_id(texture)?;
    let (namespace, path) = id.split_once(':')?;
    let path = path.strip_prefix("textures/").unwrap_or(path);
    let path = path.strip_suffix(".png").unwrap_or(path);
    let bytes = resources.read_limited(&format!("assets/{namespace}/textures/{path}.png"), 4 * 1024 * 1024).ok()?;
    if bytes.get(0..8)? != b"\x89PNG\r\n\x1a\n" || bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let edge = |at| u32::from_be_bytes(bytes.get(at..at+4)?.try_into().ok()?).checked_sub(1);
    if edge(16)? >= 1024 || edge(20)? >= 1024 { return None; }
    let (w, h, rgba) = hud::decode_bytes(&bytes).ok()?;
    Some(Texture { size: [w as usize,h as usize], rgba, tint: tint.to_le_bytes(), bright })
}

fn raster(faces: &[Face], textures: &[Texture], config: &ModelConfig) -> Option<Vec<u8>> {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = -lo;
    for face in faces {
        for point in face.points {
            lo = lo.min(point);
            hi = hi.max(point);
        }
    }
    let span = (hi.x - lo.x).max(hi.y - lo.y);
    if !span.is_finite() || span <= 0.0 { return None; }
    let scale = 40. * config.scale.unwrap_or(1.) / span;
    let center = (lo + hi) * 0.5;
    let offset = Vec3::new(24. + config.offset[0], 24. + config.offset[1], 0.);
    let mut depth = vec![f32::INFINITY; MODEL_SIZE * MODEL_SIZE];
    let mut rgba = vec![0; MODEL_SIZE * MODEL_SIZE * 4];
    for face in faces {
        let points = face.points.map(|p| (p - center) * scale + offset);
        let texture = &textures[face.layer];
        for indices in [[0,1,2], [0,2,3]] {
            let [a,b,c] = indices.map(|i| points[i]);
            let edge = |a: Vec3, b: Vec3, p: Vec3| (b.x-a.x)*(p.y-a.y)-(b.y-a.y)*(p.x-a.x);
            let area = edge(a,b,c);
            if area.abs() < 0.001 { continue; }
            let begin = |a: f32,b: f32,c: f32| a.min(b).min(c).floor().clamp(0., MODEL_SIZE as f32) as usize;
            let end = |a: f32,b: f32,c: f32| a.max(b).max(c).ceil().clamp(0., MODEL_SIZE as f32) as usize;
            for y in begin(a.y,b.y,c.y)..end(a.y,b.y,c.y) {
                for x in begin(a.x,b.x,c.x)..end(a.x,b.x,c.x) {
                    let p = Vec3::new(x as f32 + 0.5, y as f32 + 0.5, 0.);
                    let weights = [edge(b,c,p)/area, edge(c,a,p)/area, edge(a,b,p)/area];
                    if weights.iter().any(|w| *w < -0.0001) { continue; }
                    let z = a.z*weights[0]+b.z*weights[1]+c.z*weights[2];
                    let i = y * MODEL_SIZE + x;
                    if z > depth[i] + 0.001 { continue; }
                    let tex = |axis: usize| {
                        let uv = indices.iter().zip(weights).map(|(&j, w)| face.uv[j][axis] * w).sum::<f32>();
                        ((uv.clamp(0., 1.) * texture.size[axis] as f32) as usize).min(texture.size[axis]-1)
                    };
                    let src = (tex(1) * texture.size[0] + tex(0)) * 4;
                    let alpha = u32::from(texture.rgba[src+3]) * u32::from(texture.tint[3]) / 255;
                    if alpha == 0 { continue; }
                    depth[i] = z;
                    let dst = &mut rgba[i*4..i*4+4];
                    let behind = u32::from(dst[3]) * (255-alpha) / 255;
                    let shade = if texture.bright { 1. } else { face.shade };
                    for k in 0..3 {
                        let channel = (f32::from(texture.rgba[src+k]) * f32::from(texture.tint[k]) * shade / 255.) as u32;
                        dst[k] = ((channel*alpha+u32::from(dst[k])*behind)/(alpha+behind)) as u8;
                    }
                    dst[3] = (alpha+behind) as u8;
                }
            }
        }
    }
    rgba.chunks_exact(4).any(|p| p[3] > 0).then_some(rgba)
}

fn model_picture(game: &Game<'_>, mob: &Mob, key: &ModelKey, config: &ModelConfig) -> Option<Arc<Picture>> {
    let mut faces = Vec::new();
    let mut textures = Vec::new();
    for (index, (model, texture, color, bright)) in key.layers.iter().enumerate() {
        let part = mob.kind.layer(model).map(Arc::as_ref)
            .or_else(|| (index == 0).then_some(&mob.kind.model).map(|part| &**part));
        if let Some(part) = part
            && let Some(texture) = read_model_texture(&game.pack.source, texture, *color, *bright)
        {
            mesh(part, config, textures.len(), &mut faces);
            textures.push(texture);
        }
    }
    let pixels = raster(&faces, &textures, config)?;
    Some(minimap_pages::create(MODEL_SIZE as u32, MODEL_SIZE as u32, pixels))
}

fn paint(batch: &mut HudBatch, picture: &Arc<Picture>, dst: [f32; 4], opacity: u32) {
    let [x, y, w, h] = dst.map(f64::from);
    let [pw, ph] = [f64::from(picture.width), f64::from(picture.height)];
    let scale = (w / pw).min(h / ph);
    let left = x + (w - scale * pw) / 2.;
    let top = y + (h - scale * ph) / 2.;
    let right = left + scale * pw;
    let bottom = top + scale * ph;
    let first = batch.vertices.len();
    minimap_pages::paint(batch, picture,
        &[[left, top, 0., 0.], [right, top, pw, 0.], [right, bottom, pw, ph], [left, bottom, 0., ph]]);
    super::reworked_maps::set_opacity(batch, first, ((opacity >> 24) * 100 / 255) as u8);
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
    let definition = cache.definitions.get(kind).and_then(Option::as_ref);
    let body = mob.map(models::body);
    let form = definition.and_then(|d| d.choose(body.as_ref().map(|(_, texture)| texture.as_ref())))
        .cloned().unwrap_or(Form::Model(None));
    match form {
        Form::Dot => Some(false),
        Form::Model(index) => {
            if kind == "minecraft:player" || kind == "minecraft:item" { return None; }
            let mob = mob?;
            let (model, texture) = body?;
            let config = definition.and_then(|d| index.and_then(|i| d.models.get(i))).cloned().unwrap_or_default();
            let mut layers = vec![(model.into_owned(), texture.into_owned(), models::body_color(mob, 0.), false)];
            if !config.hide_layers && models::layers_shown(mob) {
                let armor = game.mob_armor_layers(mob);
                layers.extend(models::layers(mob, 0., &armor).into_iter().take(12)
                    .map(|layer| (layer.model.into_owned(), layer.texture.into_owned(), layer.color, layer.full_bright)));
            }
            let key = ModelKey { kind: kind.to_owned(), config: index, layers };
            if !cache.models.contains_key(&key) {
                if cache.models.len() >= MAX_PICTURES { cache.models.pop_first(); }
                let picture = model_picture(game, mob, &key, &config);
                cache.models.insert(key.clone(), picture);
            }
            let picture = cache.models.get(&key)?.as_ref()?.clone();
            drop(cache);
            paint(batch, &picture, dst, opacity);
            Some(true)
        }
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
            paint(batch, &picture, dst, opacity);
            Some(true)
        }
    }
}
