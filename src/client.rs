// SPDX-License-Identifier: GPL-3.0-only
// Engine adapter: this module is injected under game, never into the standalone library.
use super::Game;
use super::minimap_compat::MapAccess;
use runtime::{Language, language, tr};

macro_rules! localized_format {
    ($en:literal, $ru:literal, $fr:literal, $es:literal, $de:literal $(, $arg:expr)* $(,)?) => {
        match language() {
            Language::Russian => format!($ru $(, $arg)*),
            Language::French => format!($fr $(, $arg)*),
            Language::Spanish => format!($es $(, $arg)*),
            Language::German => format!($de $(, $arg)*),
            Language::English => format!($en $(, $arg)*),
        }
    };
}

use ::trailgrid_maps::runtime::{
    self as runtime, Dimension, Waypoint,
    maps::{self, Layer, Portal, View},
};
use horizon_client::{
    chunk::Chunk, client_resources::ClientResources, extras::Corner, extras_hud::Corners,
    hud::HudBatch, mesher::TintSource,
};
use horizon_core::geometry::Direction;
use horizon_protocol::packets::play::{TrackedWaypoint, WaypointId, WaypointTarget, waypoint_operation};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

static WAYSTONES: Mutex<BTreeMap<u128, super::minimap_providers::Waystone>> = Mutex::new(BTreeMap::new());

static LOCATOR_WAYPOINTS: Mutex<Vec<TrackedWaypoint>> = Mutex::new(Vec::new());

fn apply_locator_waypoint(waypoints: &mut Vec<TrackedWaypoint>, packet: &TrackedWaypoint) {
    let slot = waypoints.iter().position(|point| point.id == packet.id);
    match (packet.operation, slot) {
        (waypoint_operation::TRACK, Some(i)) => waypoints[i] = packet.clone(),
        (waypoint_operation::TRACK, None) if waypoints.len() < 256 => waypoints.push(packet.clone()),
        (waypoint_operation::UNTRACK, Some(i)) => { waypoints.swap_remove(i); }
        (waypoint_operation::UPDATE, Some(i))
            if std::mem::discriminant(&waypoints[i].target) == std::mem::discriminant(&packet.target) =>
        {
            waypoints[i].target = packet.target;
        }
        _ => {}
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Map,
    Waypoints,
    Settings,
    Name,
    Caches,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MapPanel {
    None,
    Waypoints,
    ServerWorlds,
}

impl MapPanel {
    fn toggle(&mut self, panel: Self) {
        *self = if *self == panel { Self::None } else { panel };
    }
}

#[derive(Default)]
struct CacheUi {
    attention: bool,
    catalog_revision: u64,
    selected: Option<String>,
    scroll: usize,
    revision: u64,
    icons: BTreeMap<String, Arc<horizon_client::hud::pictures::Picture>>,
    operation: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    message: Option<String>,
    confirm: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Spot {
    dimension: Dimension,
    x: i32,
    z: i32,
    key: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    dimension: Dimension,
    x: i32,
    y: i32,
    z: i32,
}

struct NameForm {
    text: String,
    color: u32,
    target: Option<Target>,
    rename: Option<Spot>,
    group: String,
    group_focused: bool,
    kind: MarkerMode,
    group_edit: Option<GroupEdit>,
    background: Mode,
}

#[derive(Clone,Copy,Default)]
enum MarkerMode {
    #[default]
    Saved,
    Temporary,
    Destination,
}

enum GroupEdit { Create, Rename(String) }

#[derive(Clone, Copy, PartialEq, Eq)]
enum FluidKind {
    Water,
    Lava,
}

#[derive(Clone, Copy)]
struct FluidColumn {
    kind: FluidKind,
    bottom: i32,
}

#[derive(Clone, Copy)]
struct ColumnInfo {
    height: i32,
    fluid: Option<FluidColumn>,
}

#[derive(Clone, Copy)]
struct Menu {
    height_approximate: bool,
    block: [i32; 2],
    height: Option<i32>,
    fluid: Option<FluidColumn>,
    dimension: Dimension,
    at: [f32; 2],
    portal: Option<Portal>,
    waypoint: Option<Spot>,
    radar_player: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    Copy,
    Waypoint,
    ExportPng,
    ImportXaero,
    ImportXaeroCache,
    ImportXaeroSettings,
    ReloadRadarRules,
    NextXaeroProfile,
    TemporaryWaypoint,
    DestinationWaypoint,
    CopyDestination,
    Teleport,
    TeleportDestination,
    EditWaypoint,
    DeleteWaypoint,
    Toggle3D,
    Information,
    RefreshClaims,
    TeleportPlayer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeafTint {
    None,
    Fixed(u32),
    Grass,
    Foliage,
}

#[derive(Clone, Copy)]
struct LeafColor {
    /// Mean colour of the leaf texture, without its tint.
    rgb: u32,
    tint: LeafTint,
}

impl LeafColor {
    fn map_color(self, grass: Option<u32>, foliage: Option<u32>) -> Option<u32> {
        let tint = match self.tint {
            LeafTint::None => 0x00ff_ffff,
            LeafTint::Fixed(color) => color,
            LeafTint::Grass => grass?,
            LeafTint::Foliage => foliage?,
        };
        Some(multiply_color(self.rgb, tint))
    }
}

fn multiply_color(base: u32, tint: u32) -> u32 {
    let channel = |shift: u32| ((((base >> shift) & 255) * ((tint >> shift) & 255)) / 255) & 255;
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

struct LeafPalette {
    /// Block state id → the texture colour the game draws with, before tint.
    states: BTreeMap<u32, LeafColor>,
    snow: u32,
    ice: u32,
}
impl Default for LeafPalette {
    fn default() -> Self {
        Self {
            states: BTreeMap::new(),
            snow: 0xffffff,
            ice: 0xa0a0ff,
        }
    }
}

fn average_texture(resources: &ClientResources, name: &str) -> Option<u32> {
    let pixels = resources.texture(name)?;
    let mut sum = [0u64; 3];
    let mut weight = 0u64;
    for pixel in pixels.rgba.chunks_exact(4) {
        let alpha = u64::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        for (total, value) in sum.iter_mut().zip(&pixel[..3]) {
            *total += u64::from(*value) * alpha;
        }
        weight += alpha;
    }
    if weight == 0 {
        return None;
    }
    let red = (sum[0] / weight) as u32;
    let green = (sum[1] / weight) as u32;
    let blue = (sum[2] / weight) as u32;
    Some((red << 16) | (green << 8) | blue)
}

/// Mean texture colour and the game's tint source for every leaf block,
/// including vines and lily pads, whose tint the map takes from the biome.
/// Resource packs move the textures, so the result follows
/// `block_sprites_version`.
fn build_leaf_palette(game: &Game<'_>) -> LeafPalette {
    let names =
        horizon_client::block_sprites::texture_names(&game.pack.context.render.sprite_layers);
    let states = horizon_client::block_sprites::Lookup::new(
        &game.pack.context,
        game.renderer
            .as_ref()
            .map_or(u32::MAX, |r| r.sprite_split()),
        &names,
    );
    let resources = ClientResources::new(game.options.resources.clone());
    let book = game.res.facts();
    let mut textures = BTreeMap::<String, Option<u32>>::new();
    let mut palette = LeafPalette::default();
    for (index, facts) in book.states.iter().enumerate() {
        let id = horizon_world::registry::BlockStateId(index as u32);
        let name = facts.name.as_ref();
        if let Some(rgb) = book
            .states
            .get(id.0 as usize)
            .and_then(|state| horizon_client::blocks::MAP_COLORS.get(state.map_color as usize))
        {
            if name == "minecraft:snow_block" {
                palette.snow = *rgb;
            }
            if name == "minecraft:ice" {
                palette.ice = *rgb;
            }
        }
        let tinted_block = name.contains("leaves")
            || matches!(name, "minecraft:vine" | "minecraft:lily_pad")
            || book.states.get(id.0 as usize).is_some_and(|state| {
                state
                    .classes
                    .iter()
                    .any(|class| class.as_ref() == "LeavesBlock")
            });
        if !tinted_block {
            continue;
        }
        let sprites = states.state(id);
        let face = sprites.sides[Direction::Up as usize]
            .first()
            .or_else(|| sprites.sides.iter().find_map(|side| side.first()));
        let Some(face) = face else {
            continue;
        };
        let texture = face.texture.as_str();
        let rgb = *textures
            .entry(texture.to_owned())
            .or_insert_with(|| average_texture(&resources, texture));
        let Some(rgb) = rgb else {
            continue;
        };
        let tint = if name == "minecraft:lily_pad" {
            // Vanilla tints a lily pad with the biome's grass colour; the
            // mesher's constant is only a fallback for its own drawing pass.
            LeafTint::Grass
        } else {
            match face.tint {
                None => LeafTint::None,
                Some(TintSource::Fixed(color)) => LeafTint::Fixed(color),
                Some(TintSource::Grass) => LeafTint::Grass,
                Some(TintSource::Foliage) => LeafTint::Foliage,
                // Dry foliage and water belong to non-leaf blocks; keep their
                // map colour instead of pretending a wrong tint.
                Some(_) => continue,
            }
        };
        palette.states.insert(id.0, LeafColor { rgb, tint });
    }
    palette
}

#[derive(Clone, Copy, Default)]
struct BiomeTints {
    grass: Option<u32>,
    foliage: Option<u32>,
    water: Option<u32>,
}

// Minecraft blends the biome colours over a radius instead of switching at a
// line. Sample a small grid around the block and average each requested tint.
const BIOME_BLEND_RADIUS: i32 = 4;
const BIOME_BLEND_STEP: i32 = 2;

fn blend_biome_tints(
    palette: &horizon_client::biome::Biomes,
    at: [i32; 2],
    radius: i32,
    step: i32,
    grass: bool,
    foliage: bool,
    water: bool,
    mut biome: impl FnMut(i32, i32) -> Option<u16>,
) -> BiomeTints {
    if !(grass || foliage || water) {
        return BiomeTints::default();
    }
    let step = step.max(1);
    let mut sums = [[0u64; 3]; 3];
    let mut counts = [0u64; 3];
    let mut dz = -radius;
    while dz <= radius {
        let mut dx = -radius;
        while dx <= radius {
            let x = at[0].saturating_add(dx);
            let z = at[1].saturating_add(dz);
            if let Some(id) = biome(x, z)
                && let Some(info) = palette.list.get(usize::from(id))
            {
                if grass {
                    add_color(
                        &mut sums[0],
                        &mut counts[0],
                        palette.grass(id, f64::from(x), f64::from(z)),
                    );
                }
                if foliage {
                    add_color(&mut sums[1], &mut counts[1], info.foliage);
                }
                if water {
                    add_color(&mut sums[2], &mut counts[2], info.water);
                }
            }
            dx += step;
        }
        dz += step;
    }
    BiomeTints {
        grass: average_color(sums[0], counts[0]),
        foliage: average_color(sums[1], counts[1]),
        water: average_color(sums[2], counts[2]),
    }
}

fn add_color(sum: &mut [u64; 3], count: &mut u64, color: u32) {
    for (channel, shift) in sum.iter_mut().zip([16, 8, 0]) {
        *channel += u64::from((color >> shift) & 255);
    }
    *count += 1;
}

fn average_color(sum: [u64; 3], count: u64) -> Option<u32> {
    if count == 0 {
        return None;
    }
    let channel = |index: usize| (sum[index] / count) as u32;
    Some((channel(0) << 16) | (channel(1) << 8) | channel(2))
}

struct State {
    caches: CacheUi,
    world: String,
    base_world: String,
    server_world_signal: Option<runtime::ServerWorldSignal>,
    server_world_signal_pending: bool,
    session: u64,
    dimension: Dimension,
    seen: BTreeMap<(i32, i32), (Weak<Chunk>, Layer, Instant)>,
    appearance: maps::Appearance,
    chunk_keys: BTreeSet<(i32, i32)>,
    chunk_revision: u64,
    chunk_cursor: Option<(i32, i32)>,
    layers: BTreeMap<Dimension, Layer>,
    cave_pending: [Option<(bool,Instant)>;2],
    cave_open: [bool;2],
    selected: Dimension,
    large: bool,
    map_opened: Option<Instant>,
    camera: maps::Camera,
    marker: [f64; 2],
    footsteps: VecDeque<(Dimension, [i32; 2])>,
    elapsed: f64,
    frame: Option<Arc<maps::Frame>>,
    page_images: Vec<(
        Arc<maps::AtlasPage>,
        Arc<horizon_client::hud::pictures::Picture>,
    )>,
    dragging: bool,
    close: bool,
    mode: Mode,
    menu: Option<Menu>,
    name_form: Option<NameForm>,
    waypoints: Vec<Waypoint>,
    waypoint_label_source: Vec<Waypoint>,
    waypoint_labels: Vec<String>,
    waypoint_letter_mode: runtime::minimap::WaypointLetters,
    portals: Vec<(Dimension, [i32; 3], Portal)>,
    waypoint_scroll: usize,
    waypoint_search: String,
    waypoint_search_focus: bool,
    waypoint_sort: usize,
    waypoint_dimension: Dimension,
    waypoint_groups_open: bool,
    waypoint_group_move: bool,
    waypoint_bulk: Option<BTreeSet<u64>>,
    waypoint_confirm_delete: bool,
    waypoint_groups_scroll: usize,
    waypoint_selected: Option<u64>,
    waypoint_group: Option<String>,
    destinations: BTreeMap<u64,Instant>,
    deaths_outside_reach: BTreeSet<u64>,
    slime_seed: Option<i64>,
    slime_preview: bool,
    slime_overlay: bool,
    claims: BTreeMap<::trailgrid_maps::map_claims::Chunk, Option<String>>,
    opac: super::minimap_providers::OpenPac,
    info_displays: Option<Vec<::trailgrid_maps::xaero_settings::InfoDisplay>>,
    notice: Option<(String, Instant)>,
    export: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    waypoint_import: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    cache_import: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    settings_import: Option<std::sync::mpsc::Receiver<std::io::Result<::trailgrid_maps::xaero_settings::Import>>>,
    xaero_radar_enabled: Option<bool>,
    xaero_profile: Option<String>,
    map_items: [Option<String>;2],
    pending_copy: Option<String>,
    pending_chat: Option<(String, String)>,
    pending_command: Option<String>,
    map_focus: Option<Spot>,
    map_panel: MapPanel,
    map_panel_scroll: usize,
    map_panel_hidden: bool,
    server_world_name: String,
    server_world_focus: bool,
    map_settings_change: Option<(String, String)>,
    settings_return: Option<Mode>,
    settings_tab: usize,
    settings_scroll: usize,
    placement: Option<PlacementEdit>,
    radar_editor: Option<super::minimap_radar_editor::Editor>,
    placement_change: Option<Option<[f32; 2]>>,
    /// Chunks the server sent since the last sampling pass. A waypoint's biome
    /// is sampled when its chunk arrives, rather than polling every point.
    biome_chunks: BTreeSet<(i32, i32)>,
    leaves: Option<(u64, Arc<LeafPalette>)>,
    lod_elapsed: f64,
    lod_sources:
        BTreeMap<(Dimension, maps::LodKey), (Weak<Vec<horizon_client::lod::Column>>, Instant)>,
    info: Option<Menu>,
    info_scroll: usize,
}
fn known_lod_sources(state: &State, dimension: Dimension) -> BTreeMap<(i32, i32, u8), usize> {
    state
        .lod_sources
        .iter()
        .filter(|((d, _), (source, _))| *d == dimension && source.strong_count() != 0)
        .map(|((_, key), (source, _))| ((key.x, key.z, key.level), source.as_ptr() as usize))
        .collect()
}
impl Default for State {
    fn default() -> Self {
        Self {
            caches: CacheUi::default(),
            world: String::new(),
            base_world: String::new(),
            server_world_signal: None,
            server_world_signal_pending: false,
            session: 0,
            dimension: Dimension::Player,
            seen: BTreeMap::new(),
            appearance: maps::Appearance::default(),
            chunk_keys: BTreeSet::new(),
            chunk_revision: 0,
            chunk_cursor: None,
            layers: BTreeMap::new(),
            cave_pending: [None;2],
            cave_open: [false;2],
            selected: Dimension::Player,
            large: false,
            map_opened: None,
            camera: maps::Camera::new([0.0; 2], 2.0),
            marker: [0.0; 2],
            footsteps: VecDeque::new(),
            elapsed: 1.0,
            frame: None,
            page_images: Vec::new(),
            dragging: false,
            close: false,
            mode: Mode::Map,
            menu: None,
            name_form: None,
            waypoints: Vec::new(),
            waypoint_label_source: Vec::new(),
            waypoint_labels: Vec::new(),
            waypoint_letter_mode: runtime::minimap::WaypointLetters::default(),
            portals: Vec::new(),
            waypoint_scroll: 0,
            waypoint_search: String::new(),
            waypoint_search_focus: false,
            waypoint_sort: 0,
            waypoint_dimension: Dimension::Player,
            waypoint_groups_open: false,
            waypoint_group_move: false,
            waypoint_bulk: None,
            waypoint_confirm_delete: false,
            waypoint_groups_scroll: 0,
            waypoint_selected: None,
            waypoint_group: None,
            destinations: BTreeMap::new(),
            deaths_outside_reach: BTreeSet::new(),
            slime_seed: None,
            slime_preview: false,
            slime_overlay: false,
            claims: BTreeMap::new(),
            opac: super::minimap_providers::OpenPac::default(),
            info_displays: None,
            notice: None,
            export: None,
            waypoint_import: None,
            cache_import: None,
            settings_import: None,
            xaero_radar_enabled: None,
            xaero_profile: None,
            map_items: [None,None],
            pending_copy: None,
            pending_chat: None,
            pending_command: None,
            map_focus: None,
            map_panel: MapPanel::None,
            map_panel_scroll: 0,
            map_panel_hidden: false,
            server_world_name: String::new(),
            server_world_focus: false,
            map_settings_change: None,
            settings_return: None,
            settings_tab: 0,
            settings_scroll: 0,
            placement: None,
            radar_editor: None,
            placement_change: None,
            biome_chunks: BTreeSet::new(),
            leaves: None,
            lod_elapsed: 1.0,
            lod_sources: BTreeMap::new(),
            info: None,
            info_scroll: 0,
        }
    }
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .get_or_init(|| Mutex::new(State::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
fn rectangle(width: f32, height: f32) -> [f32; 4] {
    [0.0, 0.0, width.max(1.0), height.max(1.0)]
}
pub(super) fn inside(mouse: [f32; 2], rect: [f32; 4]) -> bool {
    mouse[0] >= rect[0]
        && mouse[1] >= rect[1]
        && mouse[0] < rect[0] + rect[2]
        && mouse[1] < rect[1] + rect[3]
}

// Ground cover the map leaves out: grass, ferns, bushes and dry grass.
fn hidden_cover(classes: &[Arc<str>], name: &str) -> bool {
    classes.iter().any(|class| {
        matches!(
            class.as_ref(),
            "TallGrassBlock" | "DryVegetationBlock" | "BushBlock" | "FireflyBushBlock"
        )
    }) || matches!(
        name,
        "minecraft:tall_grass" | "minecraft:large_fern" | "minecraft:grass"
    )
}

// Curated petal, crop and plant colours: the vanilla map colour of most plants
// is one generic green, which hides what actually grows there.
fn plant_color(name: &str) -> Option<u32> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    if let Some(inner) = short.strip_prefix("potted_")
        && let Some(color) = plant_color(&format!("minecraft:{inner}"))
    {
        return Some(color);
    }
    Some(match short {
        "poppy" => 0xc0392b,
        "dandelion" => 0xf1c40f,
        "blue_orchid" => 0x2e86c1,
        "allium" => 0xb57edc,
        "azure_bluet" => 0xf0f0f0,
        "oxeye_daisy" => 0xf5f5dc,
        "cornflower" => 0x4a69bd,
        "lily_of_the_valley" => 0xf8f8f8,
        "wither_rose" => 0x2c2c2c,
        "orange_tulip" => 0xe67e22,
        "red_tulip" => 0xc0392b,
        "white_tulip" => 0xecf0f1,
        "pink_tulip" => 0xe88bb0,
        "sunflower" => 0xf4d03f,
        "lilac" => 0xc39bd3,
        "rose_bush" => 0xc0392b,
        "peony" => 0xf5b7d7,
        "torchflower" => 0xe67e22,
        "cactus_flower" => 0xe88bb0,
        "open_eyeblossom" => 0xee8a26,
        "closed_eyeblossom" => 0x7f8c8d,
        "golden_dandelion" => 0xd4a017,
        "wildflowers" | "pink_petals" => 0xf08ac0,
        "spore_blossom" => 0xe88bb0,
        "chorus_flower" => 0xa569bd,
        "wheat" => 0xd9b44a,
        "carrots" => 0xe67e22,
        "potatoes" => 0xb8a06a,
        "beetroots" => 0xa03040,
        "nether_wart" => 0xa03040,
        "melon_stem" | "attached_melon_stem" | "pumpkin_stem" | "attached_pumpkin_stem" => 0x5b8a3c,
        "pitcher_crop" => 0x9dc183,
        "torchflower_crop" => 0xe67e22,
        "oak_sapling" | "spruce_sapling" | "birch_sapling" | "jungle_sapling"
        | "acacia_sapling" | "dark_oak_sapling" | "cherry_sapling" | "pale_oak_sapling"
        | "poplar_sapling" => 0x4f7f2f,
        "mangrove_propagule" | "bamboo_sapling" => 0x6a9f3f,
        "red_mushroom" => 0xd94b44,
        "brown_mushroom" => 0x9a755c,
        "crimson_fungus" => 0xa03030,
        "warped_fungus" => 0x4a8f7f,
        "crimson_roots" => 0x8a3030,
        "warped_roots" => 0x3f7f7f,
        "nether_sprouts" => 0x3f8f5f,
        "cave_vines" | "cave_vines_plant" => 0x3f6f2f,
        "twisting_vines" | "twisting_vines_plant" => 0x3fa0a0,
        "weeping_vines" | "weeping_vines_plant" => 0x8a3030,
        "sugar_cane" => 0x8fbf5a,
        "bamboo" => 0xb7c65a,
        "cactus" => 0x3f7f3f,
        "sea_pickle" => 0x6a8f3f,
        "seagrass" | "tall_seagrass" | "kelp" | "kelp_plant" => 0x3f9f3f,
        "moss_carpet" | "pale_moss_carpet" | "moss_block" => 0x5f8f3f,
        "fern" | "short_grass" | "tall_grass" | "large_fern" => 0x4f8f3f,
        "dead_bush" | "bush" | "red_shrub" | "firefly_bush" => 0x8a6a3a,
        "leaf_litter" => 0x8a6a3a,
        "pale_hanging_moss" => 0x8a9f8a,
        "hanging_roots" => 0x8a6a4a,
        "mangrove_roots" => 0x6a5a3a,
        "big_dripleaf" | "big_dripleaf_stem" | "small_dripleaf" => 0x4f8f3f,
        "azalea" | "flowering_azalea" | "azalea_bush" | "flowering_azalea_bush" => 0x4f8f3f,
        "sweet_berry_bush" => 0x5f8f3f,
        "nether_wart_block" => 0xa03040,
        "warped_wart_block" => 0x2f8f8f,
        _ => return None,
    })
}

// The dye of a stained pane, whose vanilla map colour is unset.
fn dye_color(name: &str) -> Option<u32> {
    let short = name.strip_prefix("minecraft:").unwrap_or(name);
    let dye = short.strip_suffix("_stained_glass_pane")?;
    Some(match dye {
        "white" => 0xecf0f1,
        "orange" => 0xe67e22,
        "magenta" => 0xc0398b,
        "light_blue" => 0x5dade2,
        "yellow" => 0xf4d03f,
        "lime" => 0x7dce3a,
        "pink" => 0xf5b7d7,
        "gray" => 0x7f8c8d,
        "light_gray" => 0xbdc3c7,
        "cyan" => 0x2e9a9a,
        "purple" => 0x8e44ad,
        "blue" => 0x2e4fa3,
        "brown" => 0x8a5a2b,
        "green" => 0x2e7d32,
        "red" => 0xc0392b,
        "black" => 0x1c1c1c,
        _ => return None,
    })
}

fn portal_tint(kind: Portal) -> (u32, u8) {
    match kind {
        Portal::Nether => (0x9b4dff, 150),
        Portal::End => (0x4a1a6a, 190),
        Portal::Gateway => (0x1a0a2a, 190),
    }
}

fn draw_icon(
    game: &Game<'_>,
    batch: &mut HudBatch,
    at: [f32; 2],
    rows: [&str; 16],
    colors: [u32; 3],
) {
    for (y, row) in rows.iter().enumerate() {
        let bytes = row.as_bytes();
        let mut x = 0;
        while x < bytes.len() {
            let color = bytes[x];
            let start = x;
            while x < bytes.len() && bytes[x] == color {
                x += 1;
            }
            if let Some(index) = color.checked_sub(b'1').filter(|i| *i < 3) {
                game.native_fill(
                    batch,
                    [
                        at[0] + start as f32,
                        at[1] + y as f32,
                        (x - start) as f32,
                        1.0,
                    ],
                    colors[index as usize],
                );
            }
        }
    }
}

fn dimension_icon(game: &Game<'_>, batch: &mut HudBatch, dimension: Dimension, at: [f32; 2]) {
    let (rows, colors) = match dimension {
        Dimension::Player => (
            [
                "................",
                ".......11.......",
                "......1111......",
                ".....111111.....",
                "....11211211....",
                "...1122112211...",
                "..112221122211..",
                ".11222211222211.",
                ".......11.......",
                ".......11.......",
                ".......11.......",
                ".......11.......",
                "......2112......",
                ".....222222.....",
                "................",
                "................",
            ],
            [0xffff6262, 0xffffffff, 0xffffffff],
        ),
        Dimension::Overworld => (
            [
                "................",
                "................",
                "..111111111111..",
                ".11111111111111.",
                ".11111111111111.",
                ".22111221112212.",
                ".22212222122222.",
                ".22222222222222.",
                ".22332222233222.",
                ".22222232222222.",
                ".22222222222222.",
                ".23222222222322.",
                "..222233222222..",
                "....22222222....",
                "......2222......",
                "................",
            ],
            [0xff68b34a, 0xff93643d, 0xffbc8a54],
        ),
        Dimension::Nether => (
            [
                "................",
                "..111111111111..",
                "..111111111111..",
                "..112222222211..",
                "..112233222211..",
                "..112333322211..",
                "..112233222211..",
                "..112222233211..",
                "..112222333211..",
                "..112222233211..",
                "..112332222211..",
                "..112233222211..",
                "..112222222211..",
                "..111111111111..",
                "..111111111111..",
                "................",
            ],
            [0xff44263f, 0xffb649cf, 0xffef9aff],
        ),
        Dimension::End => (
            [
                "................",
                "................",
                "......2222......",
                "....22111122....",
                "..221111111122..",
                ".22111333311122.",
                ".21113333331112.",
                ".21133333333112.",
                ".22113333331122.",
                "..221133331122..",
                "...2211111122...",
                "....22211222....",
                "......2222......",
                ".......22.......",
                "................",
                "................",
            ],
            [0xffc6c78c, 0xff598f88, 0xff191d35],
        ),
    };
    draw_icon(game, batch, at, rows, colors);
}

fn known_biome(game: &Game<'_>, dimension: Dimension, at: [i32; 3]) -> Option<String> {
    let player = game.native_player()?;
    if player.dimension != dimension.id()
        || !game
            .world
            .chunks
            .contains_key(&(at[0].div_euclid(16), at[2].div_euclid(16)))
    {
        return None;
    }
    game.map_biome(at)
}

fn localized_biome(id: &str) -> String {
    horizon_client::lang::get_or(&format!("biome.{}", id.replace(':', ".")), id)
}

fn biome_icon(game: &Game<'_>, batch: &mut HudBatch, biome: Option<&str>, at: [f32; 2]) {
    let icon = runtime::biome_pictogram(biome);
    for (y, row) in icon.rows.iter().enumerate() {
        let mut x = 0;
        while x < runtime::BIOME_ICON_SIZE {
            let index = (row >> (x * 2)) & 3;
            let start = x;
            while x < runtime::BIOME_ICON_SIZE && (row >> (x * 2)) & 3 == index {
                x += 1;
            }
            if index != 0 {
                game.native_fill(
                    batch,
                    [
                        at[0] + start as f32,
                        at[1] + y as f32,
                        (x - start) as f32,
                        1.0,
                    ],
                    icon.colors[index as usize - 1],
                );
            }
        }
    }
}

// A small skull for automatic death waypoints, drawn from relative blocks so
// it stays legible on the map, in the world and in the waypoint list.
fn draw_skull(game: &Game<'_>, batch: &mut HudBatch, at: [f32; 2], size: f32) {
    let u = size / 8.0;
    let bone = 0xffe9e6dc;
    let socket = 0xff14171c;
    game.native_fill(
        batch,
        [at[0] - 3.0 * u, at[1] - 3.0 * u, 6.0 * u, 5.0 * u],
        bone,
    );
    game.native_fill(
        batch,
        [at[0] - 2.0 * u, at[1] + 2.0 * u, 4.0 * u, 1.5 * u],
        bone,
    );
    for x in [-2.0, 0.75] {
        game.native_fill(
            batch,
            [at[0] + x * u, at[1] - 1.5 * u, 1.25 * u, 1.75 * u],
            socket,
        );
    }
    game.native_fill(
        batch,
        [at[0] - 0.5 * u, at[1] + 0.5 * u, 1.0 * u, 1.25 * u],
        socket,
    );
    for x in [-1.0, 0.5] {
        game.native_fill(
            batch,
            [at[0] + x * u, at[1] + 2.0 * u, 0.5 * u, 1.5 * u],
            socket,
        );
    }
}

fn portal_label(kind: Portal) -> &'static str {
    match kind {
        Portal::Nether => tr(
            "Nether portal",
            "Портал Незера",
            "Portail du Nether",
            "Portal del Nether",
            "Netherportal",
        ),
        Portal::End => tr(
            "End portal",
            "Портал Энда",
            "Portail de l’End",
            "Portal del End",
            "Endportal",
        ),
        Portal::Gateway => tr(
            "End gateway",
            "Врата Энда",
            "Passerelle de l’End",
            "Puerta de enlace del End",
            "Endtransitportal",
        ),
    }
}

// The world block under the pointer, when the pointer is over the map.
fn hovered_block(view: &View, rect: [f32; 4], mouse: [f32; 2]) -> Option<[i32; 2]> {
    if !inside(mouse, rect) {
        return None;
    }
    let x = f64::from((mouse[0] - rect[0]) / rect[2]) * view.width as f64;
    let y = f64::from((mouse[1] - rect[1]) / rect[3]) * view.height as f64;
    Some(view.block_at(x, y))
}

// A block's rectangle in GUI pixels, from the frame's own pixel mapping.
fn block_rect(view: &View, rect: [f32; 4], block: [i32; 2]) -> [f32; 4] {
    let first = view.screen_at([f64::from(block[0]), f64::from(block[1])]);
    let second = view.screen_at([f64::from(block[0]) + 1.0, f64::from(block[1]) + 1.0]);
    let scale_x = rect[2] / view.width as f32;
    let scale_y = rect[3] / view.height as f32;
    [
        rect[0] + first[0].min(second[0]) as f32 * scale_x,
        rect[1] + first[1].min(second[1]) as f32 * scale_y,
        (first[0] - second[0]).abs() as f32 * scale_x,
        (first[1] - second[1]).abs() as f32 * scale_y,
    ]
}

fn highlight(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4], block: [i32; 2]) {
    let cell = block_rect(view, rect, block);
    if cell[2] < 1.0 || cell[3] < 1.0 {
        return;
    }
    let left = cell[0].max(rect[0]);
    let top = cell[1].max(rect[1]);
    let right = (cell[0] + cell[2]).min(rect[0] + rect[2]);
    let bottom = (cell[1] + cell[3]).min(rect[1] + rect[3]);
    let (width, height) = (right - left, bottom - top);
    if width < 1.0 || height < 1.0 {
        return;
    }
    game.native_fill(batch, [left, top, width, height], 0x40ffffff);
    let line = 1.0_f32.min(width).min(height);
    game.native_fill(batch, [left, top, width, line], 0xff000000);
    game.native_fill(batch, [left, top + height - line, width, line], 0xff000000);
    game.native_fill(batch, [left, top, line, height], 0xff000000);
    game.native_fill(batch, [left + width - line, top, line, height], 0xff000000);
}

fn draw_marker(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    at: [f64; 2],
    player: &crate::game::native_hooks::NativePlayer,
) {
    let pixel = view.screen_at(at);
    let x = rect[0] + pixel[0] as f32 * rect[2] / view.width as f32;
    let y = rect[1] + pixel[1] as f32 * rect[3] / view.height as f32;
    if !inside([x, y], rect) {
        return;
    }
    let same_dimension = Dimension::from_id(player.dimension) == view.dimension;
    let alpha = if view.large {255} else {u32::from(game.map_option("minimap_arrow_opacity").unwrap_or(100))*255/100};
    let alpha = if same_dimension {alpha} else {alpha*3/4};
    let border = (alpha<<24) | 0xf0f4fa;
    let color_key = if view.large {"world_marker_color"} else {"minimap_marker_color"};
    let fill = (alpha<<24) | if same_dimension {game.map_color_option(color_key).unwrap_or(0xff5050)} else {0x60a0ff};
    let mode = if view.large {if game.map_option("world_marker_arrow")==Some(0) {1} else {2}}
        else {game.map_option("minimap_marker_mode").unwrap_or(2)};
    if mode!=2 {
        let size=f32::from(game.map_option("minimap_marker_size").unwrap_or(2));
        game.native_fill(batch,[x-size,y-size,size*2.,size*2.],border);
        if mode==1 {game.native_fill(batch,[x-size+1.,y-size+1.,size*2.-2.,size*2.-2.],fill);}
        else {
            game.native_fill(batch,[x-size,y-0.5,size*2.,1.],fill);
            game.native_fill(batch,[x-0.5,y-size,1.,size*2.],fill);
        }
        return;
    }
    let arrow_scale = if view.large {1.} else {game.map_number("minimap_arrow_scale").unwrap_or(1.5) as f32 / 1.5};
    let forward = view.player_heading(f64::from(player.yaw), [rect[2], rect[3]]);
    let right = [-forward[1], forward[0]];
    for (size, color) in [(7.0*arrow_scale, border), (5.0*arrow_scale, fill)] {
        let point = |along: f32, across: f32| {
            [
                x + size * (forward[0] * along + right[0] * across),
                y + size * (forward[1] * along + right[1] * across),
            ]
        };
        let tip = point(1.0, 0.0);
        game.native_quad(
            batch,
            [tip, point(-0.65, 0.55), point(-0.65, -0.55), tip],
            color,
        );
    }
}

fn first_letter(name: &str) -> char {
    name.chars()
        .find(|c| !c.is_whitespace())
        .unwrap_or('?')
        .to_uppercase()
        .next()
        .unwrap_or('?')
}
fn refresh_waypoint_labels(state: &mut State, mode: runtime::minimap::WaypointLetters) {
    if state.waypoint_label_source != state.waypoints || state.waypoint_letter_mode != mode {
        state.waypoint_labels = runtime::waypoint_labels(&state.waypoints, mode);
        state.waypoint_label_source.clone_from(&state.waypoints);
        state.waypoint_letter_mode = mode;
    }
}
fn waypoint_text<'a>(
    state: &'a State,
    index: usize,
    waypoint: &'a Waypoint,
) -> std::borrow::Cow<'a, str> {
    if let Some(initials) = &waypoint.initials {
        return std::borrow::Cow::Borrowed(initials);
    }
    state.waypoint_labels.get(index).map_or_else(
        || std::borrow::Cow::Owned(first_letter(&waypoint.name).to_string()),
        |s| std::borrow::Cow::Borrowed(s.as_str()),
    )
}

fn waypoint_argb(color: u32) -> u32 {
    0xff00_0000 | (color & 0x00ff_ffff)
}

fn waypoint_scale(game: &Game<'_>, key: &str) -> f32 {
    1. + f32::from(game.map_option(key).unwrap_or_default().min(17)) * 0.125
}

pub(super) fn set_opacity(batch: &mut HudBatch, start: usize, percent: u8) {
    if percent >= 100 { return; }
    for vertex in &mut batch.vertices[start..] {
        let alpha = (vertex.color >> 24) * u32::from(percent) / 100;
        vertex.color = (vertex.color & 0x00ff_ffff) | (alpha << 24);
    }
}

fn default_waypoint_color(state: &State) -> u32 {
    runtime::WAYPOINT_COLORS[state.waypoints.len() % runtime::WAYPOINT_COLORS.len()]
}

pub(super) fn fit_text(game: &Game<'_>, text: &str, width: f32) -> String {
    if game.map_text_width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    for c in text.chars() {
        let mut candidate = out.clone();
        candidate.push(c);
        candidate.push('…');
        if game.map_text_width(&candidate) > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn draw_waypoint(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    waypoint: &Waypoint,
    letter: &str,
    size: f32,
) {
    let pixel = view.screen_at([f64::from(waypoint.x) + 0.5, f64::from(waypoint.z) + 0.5]);
    let x = rect[0] + pixel[0] as f32 * rect[2] / view.width as f32;
    let y = rect[1] + pixel[1] as f32 * rect[3] / view.height as f32;
    if !inside([x, y], rect) {
        return;
    }
    let half = size / 2.0;
    let width = if waypoint.icon == runtime::WaypointIcon::Letter {
        size.max(game.map_text_width(letter) + 4.)
    } else {
        size
    };
    if !view.large || game.map_option("world_waypoint_backgrounds") != Some(0) {
    game.native_fill(
        batch,
        [
            x - width / 2. - 1.0,
            y - half - 1.0,
            width + 2.0,
            size + 2.0,
        ],
        0xff101010,
    );
    game.native_fill(
        batch,
        [x - width / 2., y - half, width, size],
        waypoint_argb(waypoint.color),
    );
    }
    if waypoint.icon == runtime::WaypointIcon::Skull {
        draw_skull(game, batch, [x, y], size * 0.95);
        return;
    }
    game.map_text(
        batch,
        &letter,
        [x - game.map_text_width(&letter) / 2.0, y - 4.0],
        if view.large && game.map_option("world_waypoint_backgrounds")==Some(0) {
            waypoint_argb(if waypoint.color==0 {0xffffff}else{waypoint.color})
        } else if waypoint.color == 0 { 0xffffffff } else { 0xff201800 },
        false,
    );
}

fn draw_portal_marker(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    at: [i32; 3],
    kind: Portal,
) {
    let pixel = view.screen_at([f64::from(at[0]) + 0.5, f64::from(at[2]) + 0.5]);
    let x = rect[0] + pixel[0] as f32 * rect[2] / view.width as f32;
    let y = rect[1] + pixel[1] as f32 * rect[3] / view.height as f32;
    if !inside([x, y], rect) {
        return;
    }
    let (color, _) = portal_tint(kind);
    game.native_fill(batch, [x - 3.5, y - 3.5, 7.0, 7.0], 0xff101010);
    game.native_fill(batch, [x - 2.5, y - 2.5, 5.0, 5.0], color);
    match kind {
        Portal::Nether => {
            game.native_fill(batch, [x - 0.5, y - 2.0, 1.0, 4.0], 0xfff0e0ff);
            game.native_fill(batch, [x - 2.0, y - 0.5, 4.0, 1.0], 0xfff0e0ff);
        }
        Portal::End => {
            game.native_fill(batch, [x - 1.0, y - 1.0, 2.0, 2.0], 0xffe0d0ff);
        }
        Portal::Gateway => {
            game.native_fill(batch, [x - 1.5, y - 1.5, 3.0, 3.0], 0xffb090ff);
        }
    }
}

// The visible height at a hovered column, in the player's current dimension
// (the only one whose loaded chunks the client holds). Water and lava also
// report the lowest block of their column.
fn hovered_column(game: &Game<'_>, layer: Layer, block: [i32; 2]) -> Option<ColumnInfo> {
    if layer == Layer::Below(maps::FULL_CAVE) {
        return None;
    }
    let chunk = game
        .world
        .chunks
        .get(&(block[0].div_euclid(16), block[1].div_euclid(16)))?;
    let (x, z) = (block[0].rem_euclid(16), block[1].rem_euclid(16));
    let facts = game.res.facts();
    let ceiling = match layer {
        Layer::Surface => i32::MAX,
        Layer::Below(y) => y,
    };
    let height = chunk.highest(game.world.air.0, |y| {
        if y > ceiling {
            return false;
        }
        let id = chunk.block(x, y, z);
        let Some(entry) = facts.states.get(id.0 as usize) else {
            return false;
        };
        if hidden_cover(entry.classes.as_ref(), entry.name.as_ref()) {
            return false;
        }
        !facts.is_air(id)
            && (matches!(
                entry.fluid,
                horizon_client::blocks::Fluid::Water | horizon_client::blocks::Fluid::Lava
            ) || horizon_client::blocks::MAP_COLORS
                .get(entry.map_color as usize)
                .copied()
                .unwrap_or(0)
                != 0)
    })?;
    let fluid = facts
        .states
        .get(chunk.block(x, height, z).0 as usize)
        .and_then(|entry| match entry.fluid {
            horizon_client::blocks::Fluid::Water => Some(FluidKind::Water),
            horizon_client::blocks::Fluid::Lava => Some(FluidKind::Lava),
            horizon_client::blocks::Fluid::None | horizon_client::blocks::Fluid::Modded => None,
        })
        .map(|kind| {
            let mut bottom = height;
            while bottom > chunk.min_y() {
                let below = chunk.block(x, bottom - 1, z);
                let Some(entry) = facts.states.get(below.0 as usize) else {
                    break;
                };
                let same = match kind {
                    FluidKind::Water => entry.fluid == horizon_client::blocks::Fluid::Water,
                    FluidKind::Lava => entry.fluid == horizon_client::blocks::Fluid::Lava,
                };
                if !same {
                    break;
                }
                bottom -= 1;
            }
            FluidColumn { kind, bottom }
        });
    Some(ColumnInfo { height, fluid })
}

const MENU_HEADER: f32 = 19.0;
const MENU_SUBLINE: f32 = 14.0;
const MENU_ROW: f32 = 15.0;
const MENU_PAD: f32 = 4.0;

fn teleport_allowed(game: &Game<'_>) -> bool {
    game.ui.registries.permission >= 2 && game.map_option("map_teleport_allowed") != Some(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PortalTarget {
    Exact { dimension: Dimension, at: [i32; 3] },
    Spawn,
    Outer,
    Unknown,
}

fn portal_target(state: &State, menu: Menu, player_y: i32) -> Option<PortalTarget> {
    let kind = menu.portal?;
    Some(match kind {
        Portal::Nether => {
            let (dimension, nominal) = match menu.dimension {
                Dimension::Overworld => (
                    Dimension::Nether,
                    [
                        menu.block[0] / 8,
                        menu.height.unwrap_or(player_y),
                        menu.block[1] / 8,
                    ],
                ),
                Dimension::Nether => (
                    Dimension::Overworld,
                    [
                        menu.block[0] * 8,
                        menu.height.unwrap_or(player_y),
                        menu.block[1] * 8,
                    ],
                ),
                _ => return Some(PortalTarget::Unknown),
            };
            let nearest = state
                .portals
                .iter()
                .filter(|(at_dimension, _, at_kind)| {
                    *at_dimension == dimension && *at_kind == Portal::Nether
                })
                .map(|(_, at, _)| *at)
                .filter(|at| {
                    let dx = f64::from(at[0] - nominal[0]);
                    let dz = f64::from(at[2] - nominal[2]);
                    dx * dx + dz * dz <= 128.0 * 128.0
                })
                .min_by_key(|at| {
                    let dx = i64::from(at[0] - nominal[0]);
                    let dz = i64::from(at[2] - nominal[2]);
                    dx * dx + dz * dz
                });
            PortalTarget::Exact {
                dimension,
                at: nearest.unwrap_or(nominal),
            }
        }
        Portal::End => {
            if menu.dimension == Dimension::End {
                PortalTarget::Spawn
            } else {
                PortalTarget::Exact {
                    dimension: Dimension::End,
                    at: [100, 49, 0],
                }
            }
        }
        Portal::Gateway => PortalTarget::Outer,
    })
}

fn portal_target_text(target: PortalTarget) -> String {
    match target {
        PortalTarget::Exact { dimension, at } => {
            format!("{}: {} {} {}", dimension.label(), at[0], at[1], at[2])
        }
        PortalTarget::Spawn => tr(
            "Overworld: spawn point",
            "Верхний мир: точка спавна",
            "Surface : point d’apparition",
            "Mundo normal: punto de aparición",
            "Oberwelt: Spawnpunkt",
        )
        .into(),
        PortalTarget::Outer => tr(
            "End: outer islands",
            "Энд: внешние острова",
            "End : îles extérieures",
            "End: islas exteriores",
            "Ende: äußere Inseln",
        )
        .into(),
        PortalTarget::Unknown => tr(
            "unknown destination",
            "назначение неизвестно",
            "destination inconnue",
            "destino desconocido",
            "Unbekanntes Ziel",
        )
        .into(),
    }
}

fn menu_title(menu: Menu) -> String {
    match menu.height {
        Some(y) => {
            let bottom = menu.fluid.map_or_else(String::new, |fluid| {
                localized_format!(
                    " (bottom {})",
                    " (дно {})",
                    " (fond {})",
                    " (fondo {})",
                    " (Grund {})",
                    fluid.bottom
                )
            });
            format!(
                "X: {}  Y: {}{}{}  Z: {}",
                menu.block[0],
                if menu.height_approximate { "≈" } else { "" },
                y,
                bottom,
                menu.block[1]
            )
        }
        None => format!("X: {}  Z: {}", menu.block[0], menu.block[1]),
    }
}

fn menu_subtitle(
    game:&Game<'_>,
    state: &State,
    menu: Menu,
    player: &crate::game::native_hooks::NativePlayer,
) -> Option<String> {
    let portal=menu.portal.map(|kind| {
        let target = portal_target(state, menu, player.y.floor() as i32);
        format!(
            "{} → {}",
            portal_label(kind),
            target.map_or_else(
                || tr(
                    "unknown destination",
                    "назначение неизвестно",
                    "destination inconnue",
                    "destino desconocido",
                    "Unbekanntes Ziel"
                )
                .to_owned(),
                portal_target_text
            )
        )
    });
    let local=Dimension::from_id(player.dimension);
    if game.map_option("world_map_distances")!=Some(1) || local!=menu.dimension {
        return portal;
    }
    let dx=f64::from(menu.block[0])+0.5-player.x;
    let dz=f64::from(menu.block[1])+0.5-player.z;
    let y=menu.waypoint.and_then(|spot|state.waypoints.iter().find(|point|point.key()==spot.key))
        .filter(|point|!point.options.unspecified_height).map(|point|f64::from(point.y)-player.y);
    let meters=dx.hypot(dz).hypot(y.unwrap_or(0.));
    let limit=game.map_number("world_map_km_at").unwrap_or(-1.);
    let precision=usize::from(game.map_option("world_map_dist_precision").unwrap_or_default());
    let distance=if limit>=0. && meters>=limit {
        format!("{km:.precision$} km",km=meters/1000.)
    } else {
        format!("{meters:.precision$} m")
    };
    Some(portal.map_or_else(||distance.clone(),|portal|format!("{portal} · {distance}")))
}

fn menu_items(
    game: &Game<'_>,
    menu: Menu,
    target: Option<PortalTarget>,
    show_3d: bool,
) -> Vec<(&'static str, MenuAction)> {
    let mut items = vec![
        (
            tr(
                "Chunk and region information",
                "Информация о чанке и регионе",
                "Informations sur le chunk et la région",
                "Información del chunk y la región",
                "Chunk- und Regionsinformationen",
            ),
            MenuAction::Information,
        ),
        (
            tr(
                "Copy coordinates",
                "Скопировать координаты",
                "Copier les coordonnées",
                "Copiar coordenadas",
                "Koordinaten kopieren",
            ),
            MenuAction::Copy,
        ),
        (
            tr(
                "Create waypoint",
                "Поставить вейпоинт",
                "Créer un repère",
                "Crear marcador",
                "Wegpunkt erstellen",
            ),
            MenuAction::Waypoint,
        ),
        (
            tr(
                "Export explored map as PNG",
                "Экспортировать карту в PNG",
                "Exporter la carte en PNG",
                "Exportar el mapa a PNG",
                "Karte als PNG exportieren",
            ),
            MenuAction::ExportPng,
        ),
    ];
    if menu.portal.is_some() && matches!(target, Some(PortalTarget::Exact { .. })) {
        items.push((
            tr(
                "Copy destination coordinates",
                "Скопировать координаты назначения",
                "Copier les coordonnées de destination",
                "Copiar coordenadas del destino",
                "Zielkoordinaten kopieren",
            ),
            MenuAction::CopyDestination,
        ));
    }
    if menu.waypoint.is_some() {
        items.push((
            tr("Edit waypoint","Изменить метку","Modifier le repère","Editar marcador","Wegpunkt bearbeiten"),
            MenuAction::EditWaypoint,
        ));
        items.push((
            tr(
                "Delete waypoint",
                "Удалить вейпоинт",
                "Supprimer le repère",
                "Eliminar marcador",
                "Wegpunkt löschen",
            ),
            MenuAction::DeleteWaypoint,
        ));
        items.push((
            if show_3d {
                tr(
                    "Hide in 3D world",
                    "Скрыть в 3D-мире",
                    "Masquer dans le monde 3D",
                    "Ocultar en el mundo 3D",
                    "In 3D-Welt ausblenden",
                )
            } else {
                tr(
                    "Show in 3D world",
                    "Показать в 3D-мире",
                    "Afficher dans le monde 3D",
                    "Mostrar en el mundo 3D",
                    "In 3D-Welt zeigen",
                )
            },
            MenuAction::Toggle3D,
        ));
    }
    if official_smp(game) {
        items.push((tr("Refresh my claims", "Обновить свои приваты", "Actualiser mes claims",
            "Actualizar mis parcelas", "Eigene Claims aktualisieren"), MenuAction::RefreshClaims));
    }
    if teleport_allowed(game) {
        if menu.radar_player.is_some() {
            items.push((tr("Teleport to player", "Телепортироваться к игроку", "Se téléporter au joueur",
                "Teletransportar al jugador", "Zum Spieler teleportieren"), MenuAction::TeleportPlayer));
        }
        if menu.portal.is_some() {
            if matches!(target, Some(PortalTarget::Exact { .. })) {
                items.push((
                    tr(
                        "Teleport to portal",
                        "Телепортироваться к порталу",
                        "Se téléporter au portail",
                        "Teletransportarse al portal",
                        "Zum Portal teleportieren",
                    ),
                    MenuAction::TeleportDestination,
                ));
            }
        } else {
            items.push((
                tr(
                    "Teleport here",
                    "Телепортироваться туда",
                    "Se téléporter ici",
                    "Teletransportarse aquí",
                    "Hierher teleportieren",
                ),
                MenuAction::Teleport,
            ));
        }
    }
    items.push((
        tr(
            "Import Xaero waypoints",
            "Импорт меток Xaero",
            "Importer les repères Xaero",
            "Importar marcadores de Xaero",
            "Xaero-Wegpunkte importieren",
        ),
        MenuAction::ImportXaero,
    ));
    items.push((
        tr(
            "Import Xaero map cache",
            "Импорт кеша карты Xaero",
            "Importer le cache cartographique Xaero",
            "Importar la caché de Xaero",
            "Xaero-Kartencache importieren",
        ),
        MenuAction::ImportXaeroCache,
    ));
    items.push((
        tr("Import Xaero settings", "Импорт настроек Xaero", "Importer les réglages Xaero",
            "Importar ajustes de Xaero", "Xaero-Einstellungen importieren"),
        MenuAction::ImportXaeroSettings,
    ));
    items.push((
        tr("Temporary waypoint", "Временная метка", "Repère temporaire", "Marcador temporal", "Temporärer Wegpunkt"),
        MenuAction::TemporaryWaypoint,
    ));
    items.push((
        tr("Destination", "Точка назначения", "Destination", "Destino", "Zielpunkt"),
        MenuAction::DestinationWaypoint,
    ));
    items.push((
        tr("Reload radar rules", "Загрузить правила радара", "Recharger les règles du radar",
            "Recargar reglas del radar", "Radarregeln neu laden"),
        MenuAction::ReloadRadarRules,
    ));
    items.push((
        tr("Next Xaero profile","Следующий профиль Xaero","Profil Xaero suivant","Siguiente perfil Xaero","Nächstes Xaero-Profil"),
        MenuAction::NextXaeroProfile,
    ));
    items
}

/// Whether the waypoint under the pointer is shown in the 3D world; defaults to
/// true for a point no longer in the current snapshot.
fn waypoint_show_3d(state: &State, spot: Spot) -> bool {
    state
        .waypoints
        .iter()
        .find(|waypoint| {
            waypoint.key() == spot.key
        })
        .map_or(true, |waypoint| waypoint.show_3d)
}

fn menu_layout(
    game: &Game<'_>,
    size: [f32; 2],
    menu: Menu,
    subtitle: Option<&str>,
    items: &[(&'static str, MenuAction)],
) -> ([f32; 4], Vec<[f32; 4]>) {
    let mut width = game.map_text_width(&menu_title(menu));
    if let Some(subtitle) = subtitle {
        width = width.max(game.map_text_width(subtitle));
    }
    for (label, _) in items {
        width = width.max(game.map_text_width(label));
    }
    let width = (width + 18.0).clamp(160.0, 340.0);
    let header = MENU_HEADER
        + if subtitle.is_some() {
            MENU_SUBLINE
        } else {
            0.0
        };
    let height = header + items.len() as f32 * MENU_ROW + MENU_PAD;
    let x = if menu.at[0] + width > size[0] - 6.0 {
        size[0] - width - 6.0
    } else {
        menu.at[0]
    }
    .max(6.0);
    let y = if menu.at[1] + height > size[1] - 6.0 {
        size[1] - height - 6.0
    } else {
        menu.at[1]
    }
    .max(6.0);
    let rect = [x, y, width, height];
    let rows = (0..items.len())
        .map(|index| {
            [
                rect[0] + 4.0,
                rect[1] + header + index as f32 * MENU_ROW,
                rect[2] - 8.0,
                MENU_ROW,
            ]
        })
        .collect();
    (rect, rows)
}

fn teleport_command(dimension: Dimension, at: [i32; 3], from: Dimension) -> String {
    let coords = format!(
        "{:.3} {:.3} {:.3}",
        f64::from(at[0]) + 0.5,
        f64::from(at[1]),
        f64::from(at[2]) + 0.5
    );
    if dimension == from {
        format!("tp {coords}")
    } else {
        format!("execute in {} run tp {coords}", dimension.id())
    }
}

fn configured_teleport(
    game: &Game<'_>, dimension: Dimension, from: Dimension,
    point: Option<&Waypoint>, at: [i32;3], unknown_height: bool,
) -> Option<String> {
    let cross_dimension = dimension != from;
    if cross_dimension && point.is_some() && game.map_option("waypoint_cross_dimension")==Some(0) {
        return None;
    }
    let partial = game.map_option(if point.is_some() {"waypoint_partial_y"} else {"map_partial_y"}) != Some(0);
    let y = if partial && unknown_height && !cross_dimension {"~".to_owned()} else {at[1].to_string()};
    let yaw = point.and_then(|point| point.options.rotation.then_some(point.options.yaw));
    let (key, default) = if yaw.is_some() {
        ("tp_rotation","tp @s {x} {y} {z} {yaw} ~")
    } else if point.is_some() {
        ("tp_waypoint","tp @s {x} {y} {z}")
    } else if cross_dimension {
        ("tp_map_dimension","execute as @s in {d} run tp {x} {y} {z}")
    } else {
        ("tp_map","tp @s {x} {y} {z}")
    };
    use ::trailgrid_maps::xaero_settings;
    let directory=game.native_data_dir(runtime::MOD_ID);
    let profile=xaero_settings::active_profile(&directory);
    let templates=xaero_settings::teleport_commands(&xaero_settings::commands_directory(&directory,profile.as_deref())).unwrap_or_default();
    let template = templates.get(key).map(String::as_str).unwrap_or(default);
    let command = template.replace("{x}",&format!("{:.3}",f64::from(at[0])+0.5))
        .replace("{z}",&format!("{:.3}",f64::from(at[2])+0.5))
        .replace("{y}",&y)
        .replace("{d}",dimension.id())
        .replace("{yaw}",&yaw.map_or_else(||"~".to_owned(),|yaw|yaw.to_string()));
    if cross_dimension && point.is_some() && !template.contains("{d}") {
        Some(format!("execute in {} run {}",dimension.id(),command.trim_start_matches('/')))
    } else {Some(command)}
}

fn default_waypoint_name(state: &State) -> String {
    localized_format!(
        "Waypoint {}",
        "Вейпоинт {}",
        "Repère {}",
        "Marcador {}",
        "Wegpunkt {}",
        state.waypoints.len() + 1
    )
}

fn perform_menu(game: &Game<'_>, state: &mut State, menu: Menu, action: MenuAction) {
    let Some(player) = game.native_player() else {
        state.menu = None;
        return;
    };
    let fallback = player.y.floor() as i32;
    let y = menu.height.unwrap_or(fallback);
    let target = portal_target(state, menu, fallback);
    state.menu = None;
    match action {
        MenuAction::ReloadRadarRules => {
            let directory = game.native_data_dir(runtime::MOD_ID);
            let message = match ::trailgrid_maps::radar::load_rules(&directory, true) {
                Ok((loaded, skipped)) => localized_format!(
                    "Radar rules {} · skipped {} · radar-rules.txt", "Правил радара {} · пропущено {} · radar-rules.txt",
                    "Règles radar {} · ignorées {} · radar-rules.txt", "Reglas {} · omitidas {} · radar-rules.txt",
                    "Radarregeln {} · übersprungen {} · radar-rules.txt", loaded, skipped
                ),
                Err(error) => error.to_string(),
            };
            state.notice = Some((message, Instant::now()));
        }
        MenuAction::ImportXaeroSettings | MenuAction::NextXaeroProfile => {
            if state.settings_import.is_some() { return; }
            let source = game.native_data_dir(runtime::MOD_ID);
            let next=action==MenuAction::NextXaeroProfile;
            let (send, reply) = std::sync::mpsc::channel();
            let worker = std::thread::Builder::new().name("map-settings-import".into()).spawn(move || {
                let _ = send.send(::trailgrid_maps::xaero_settings::read_active(&source,next));
            });
            match worker {
                Ok(_) => state.settings_import = Some(reply),
                Err(error) => state.notice = Some((error.to_string(), Instant::now())),
            }
        }
        MenuAction::ImportXaeroCache => {
            if state.cache_import.is_some() {
                return;
            }
            let directory = game.native_data_dir(runtime::MOD_ID);
            let source = directory.join("xaero-cache").join(menu.dimension.name());
            let read = xaero_cache_reader(game);
            match runtime::initialize(directory).maps.import_xaero(
                state.session, menu.dimension, source, read,
            ) {
                Ok(reply) => state.cache_import = Some(reply),
                Err(error) => state.notice = Some((error, Instant::now())),
            }
        }
        MenuAction::ImportXaero => {
            if state.waypoint_import.is_some() {
                return;
            }
            let directory = game.native_data_dir(runtime::MOD_ID);
            let path = directory.join(format!("xaero-{}.txt", menu.dimension.name()));
            let controller = runtime::initialize(directory);
            let world = state.world.clone();
            let (send, reply) = std::sync::mpsc::channel();
            let worker = std::thread::Builder::new().name("map-waypoint-import".into())
                .spawn(move || {
                    let result = controller.import_xaero_waypoints(&path, &world, menu.dimension);
                    let _ = send.send(result);
                });
            match worker {
                Ok(_) => state.waypoint_import = Some(reply),
                Err(error) => state.notice = Some((error.to_string(), Instant::now())),
            }
        }
        MenuAction::ExportPng => export_map_png(game,state,menu.dimension),
        MenuAction::Information => {
            state.info = Some(menu);
            state.info_scroll = 0;
        }
        MenuAction::Copy => {
            state.pending_copy = Some(format!("{} {} {}", menu.block[0], y, menu.block[1]));
            state.notice = Some((
                tr(
                    "Coordinates copied",
                    "Координаты скопированы",
                    "Coordonnées copiées",
                    "Coordenadas copiadas",
                    "Koordinaten kopiert",
                )
                .into(),
                Instant::now(),
            ));
        }
        MenuAction::Waypoint | MenuAction::TemporaryWaypoint | MenuAction::DestinationWaypoint => {
            let target = Target {
                dimension: menu.dimension,
                x: menu.block[0],
                y,
                z: menu.block[1],
            };
            state.name_form = Some(NameForm {
                text: default_waypoint_name(state),
                color: default_waypoint_color(state),
                target: Some(target),
                rename: None,
                group: state.waypoint_group.clone().unwrap_or_default(),
                group_focused: false,
                kind: match action {
                    MenuAction::TemporaryWaypoint => MarkerMode::Temporary,
                    MenuAction::DestinationWaypoint => MarkerMode::Destination,
                    _ => MarkerMode::Saved,
                },
                group_edit: None,
                background: Mode::Map,
            });
            state.mode = Mode::Name;
            runtime::set_text_input(true);
        }
        MenuAction::CopyDestination => {
            if let Some(PortalTarget::Exact { at, .. }) = target {
                state.pending_copy = Some(format!("{} {} {}", at[0], at[1], at[2]));
                state.notice = Some((
                    tr(
                        "Destination coordinates copied",
                        "Координаты назначения скопированы",
                        "Coordonnées de destination copiées",
                        "Coordenadas del destino copiadas",
                        "Zielkoordinaten kopiert",
                    )
                    .into(),
                    Instant::now(),
                ));
            }
        }
        MenuAction::RefreshClaims => {
            if official_smp(game) {
                state.pending_command = Some("claims".into());
            }
        }
        MenuAction::TeleportPlayer => {
            if teleport_allowed(game) && let Some(name) = menu.radar_player.and_then(|id| game.remote_player_name(id))
                && (1..=16).contains(&name.len())
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                use ::trailgrid_maps::xaero_settings;
                let directory = game.native_data_dir(runtime::MOD_ID);
                let profile = xaero_settings::active_profile(&directory);
                let commands = xaero_settings::teleport_commands(&xaero_settings::commands_directory(&directory, profile.as_deref())).unwrap_or_default();
                let template = commands.get("tp_player").map(String::as_str).unwrap_or("tp @s {name}");
                state.pending_command = Some(template.replace("{name}", name));
                state.close = true;
            }
        }
        MenuAction::Teleport => {
            if !teleport_allowed(game) {
                return;
            }
            let point = menu.waypoint.and_then(|spot| state.waypoints.iter().find(|point|point.key()==spot.key));
            let unknown = point.is_some_and(|point|point.options.unspecified_height)
                || (point.is_none() && menu.height.is_none());
            let at = point.map_or([menu.block[0], y.saturating_add(1), menu.block[1]], |point| {
                [point.x, if point.options.unspecified_height {y} else {point.y}, point.z]
            });
            state.pending_command = configured_teleport(game,menu.dimension,Dimension::from_id(player.dimension),point,at,unknown);
            state.close = state.pending_command.is_some();
        }
        MenuAction::TeleportDestination => {
            if !teleport_allowed(game) {
                return;
            }
            if let Some(PortalTarget::Exact { dimension, at }) = target {
                state.pending_command = Some(teleport_command(
                    dimension,
                    at,
                    Dimension::from_id(player.dimension),
                ));
                state.close = true;
            }
        }
        MenuAction::EditWaypoint => {
            if let Some(spot) = menu.waypoint {
                edit_waypoint(state,spot.key);
            }
        }
        MenuAction::DeleteWaypoint => {
            if let Some(spot) = menu.waypoint {
                runtime::initialize(game.native_data_dir(runtime::MOD_ID)).delete_waypoint(&state.world, spot.key);
                state.notice = Some((
                    tr(
                        "Waypoint deleted",
                        "Вейпоинт удалён",
                        "Repère supprimé",
                        "Marcador eliminado",
                        "Wegpunkt gelöscht",
                    )
                    .into(),
                    Instant::now(),
                ));
            }
        }
        MenuAction::Toggle3D => {
            if let Some(spot) = menu.waypoint {
                let show_3d = waypoint_show_3d(state, spot);
                runtime::initialize(game.native_data_dir(runtime::MOD_ID))
                    .change_waypoint(&state.world, spot.key, |point| point.show_3d = !show_3d);
            }
        }
    }
}

fn xaero_cache_reader(game: &Game<'_>) -> maps::ReadXaeroRegion {
    use ::trailgrid_maps::xaero_cache as xaero;
    use horizon_core::nbt::{self, Tag};
    use std::io::{self, Read};
    let resources = Arc::clone(&game.res);
    let registry = game.pack.assets.registry;
    let biome_palette = game.world_biomes.as_ref().unwrap_or_else(|| game.res.biomes()).clone();
    Box::new(move |path, coordinate, layer| {
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        let textures = matches!(ext, "xwmc" | "outdated");
        let limit = if textures { xaero::MAX_REGION_BYTES } else { xaero::MAX_BLOCK_REGION_BYTES };
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > limit {
            return Err(io::Error::other("Xaero archive is too large"));
        }
        let mut data = Vec::new();
        if ext == "xaero" {
            file.take(limit + 1).read_to_end(&mut data)?;
        } else {
            let mut archive = zip::ZipArchive::new(file)?;
            if archive.len() != 1 {
                return Err(io::Error::other("Expected one Xaero region entry"));
            }
            let entry = if textures { "cache.xaero" } else { "region.xaero" };
            archive.by_name(entry)?.take(limit + 1).read_to_end(&mut data)?;
        }
        if data.len() as u64 > limit {
            return Err(io::Error::other("Xaero region exceeds the decompressed size limit"));
        }
        if textures {
            return xaero::region(&data, coordinate, layer);
        }
        let decode = |bytes: &[u8]| -> io::Result<_> {
            let limits = nbt::Limits { max_bytes: 65_536, max_nodes: 512, max_depth: 8 };
            let bytes = &bytes[..bytes.len().min(limits.max_bytes)];
            let (root, consumed) = nbt::read_named(bytes, limits).map_err(|error| io::Error::other(error.to_string()))?;
            let mut tag = root.tag;
            if tag.get("id").is_none() {
                let Some(name) = tag.get("Name").cloned() else {
                    return Err(io::Error::other("Xaero block state has no name"));
                };
                let mut fields = vec![("id", name)];
                if let Some(properties) = tag.get("Properties") {
                    fields.push(("properties", properties.clone()));
                }
                tag = Tag::compound(fields);
            }
            let state = horizon_world::chunk::BlockState::from_nbt(&tag)?;
            let id = registry.resolve(&state).ok();
            let book = resources.facts();
            let rgb = id.and_then(|id| book.states.get(id.0 as usize))
                .and_then(|facts| horizon_client::blocks::MAP_COLORS.get(facts.map_color as usize))
                .copied()
                .unwrap_or(0x525968);
            let air = id.is_some_and(|id| book.is_air(id));
            Ok((xaero::SavedBlock { name: Arc::from(state.name), color: rgb, air }, consumed))
        };
        xaero::block_region(&data, coordinate, layer, decode, |block, biome, [x,z]| {
            let id = biome_palette.id(biome.unwrap_or("minecraft:plains"));
            let bi = biome_palette.get(id);
            if block.name.as_ref() == "minecraft:water" {
                return bi.water;
            }
            plant_color(&block.name).unwrap_or_else(|| {
                maps::block_map_color(&block.name, block.color,
                    Some(biome_palette.grass(id, f64::from(x), f64::from(z))), Some(bi.foliage))
            })
        })
    })
}

fn write_map_png(path: &std::path::Path, width: u32, height: u32, pixels: &[u8]) -> std::io::Result<()> {
    use std::{fs, io};
    let temporary = path.with_extension("png.partial");
    let file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary)?;
    let result = (|| -> io::Result<()> {
        let mut encoder = png::Encoder::new(io::BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().map_err(io::Error::other)?;
        writer.write_image_data(pixels).map_err(io::Error::other)?;
        writer.finish().map_err(io::Error::other)?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn poll_map_export(state: &mut State) {
    use std::sync::mpsc::TryRecvError;
    let Some(export) = &state.export else { return };
    let message = match export.try_recv() {
        Ok(Ok(file)) => localized_format!(
            "Saved to exports/{}", "Сохранено в exports/{}", "Enregistré dans exports/{}",
            "Guardado en exports/{}", "Gespeichert in exports/{}", file
        ),
        Ok(Err(error)) => error,
        Err(TryRecvError::Disconnected) => tr(
            "Export was interrupted", "Экспорт прерван", "Export interrompu",
            "Exportación interrumpida", "Export unterbrochen"
        ).to_owned(),
        Err(TryRecvError::Empty) => return,
    };
    state.export = None;
    state.notice = Some((message, Instant::now()));
}

fn poll_waypoint_import(state: &mut State) {
    use std::sync::mpsc::TryRecvError;
    let Some(import) = &state.waypoint_import else { return };
    let message = match import.try_recv() {
        Ok(Ok(count)) => localized_format!(
            "Imported waypoints: {}", "Импортировано меток: {}", "Repères importés : {}",
            "Marcadores importados: {}", "Importierte Wegpunkte: {}", count
        ),
        Ok(Err(error)) => error,
        Err(TryRecvError::Disconnected) => tr(
            "Import was interrupted", "Импорт прерван", "Importation interrompue",
            "Importación interrumpida", "Import unterbrochen"
        ).to_owned(),
        Err(TryRecvError::Empty) => return,
    };
    state.waypoint_import = None;
    state.notice = Some((message, Instant::now()));
}

fn poll_cache_import(state: &mut State) {
    use std::sync::mpsc::TryRecvError;
    let Some(import) = &state.cache_import else { return };
    let message = match import.try_recv() {
        Ok(Ok(count)) => localized_format!(
            "Imported chunks: {}", "Импортировано чанков: {}", "Chunks importés : {}",
            "Chunks importados: {}", "Importierte Chunks: {}", count
        ),
        Ok(Err(error)) => error,
        Err(TryRecvError::Disconnected) => tr(
            "Import was interrupted", "Импорт прерван", "Import interrompu",
            "Importación interrumpida", "Import unterbrochen"
        ).to_owned(),
        Err(TryRecvError::Empty) => return,
    };
    state.cache_import = None;
    state.notice = Some((message, Instant::now()));
}

fn draw_menu(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    let Some(menu) = state.menu else {
        return;
    };
    let Some(player) = game.native_player() else {
        return;
    };
    let subtitle = menu_subtitle(game,state,menu,&player);
    let has_subtitle = subtitle.is_some();
    let target = portal_target(state, menu, player.y.floor() as i32);
    let show_3d = menu
        .waypoint
        .map_or(true, |spot| waypoint_show_3d(state, spot));
    let items = menu_items(game, menu, target, show_3d);
    let (rect, rows) = menu_layout(
        game,
        [batch.width, batch.height],
        menu,
        subtitle.as_deref(),
        &items,
    );
    let mouse = game.cursor();
    let border = 0xff3a4553;
    game.native_fill(batch, rect, 0xf01b2028);
    game.native_fill(batch, [rect[0], rect[1], rect[2], 1.0], border);
    game.native_fill(
        batch,
        [rect[0], rect[1] + rect[3] - 1.0, rect[2], 1.0],
        border,
    );
    game.native_fill(batch, [rect[0], rect[1], 1.0, rect[3]], border);
    game.native_fill(
        batch,
        [rect[0] + rect[2] - 1.0, rect[1], 1.0, rect[3]],
        border,
    );
    game.map_text(
        batch,
        &menu_title(menu),
        [rect[0] + 8.0, rect[1] + 5.0],
        0xffffff,
        true,
    );
    if let Some(subtitle) = subtitle.as_deref() {
        game.map_text(
            batch,
            subtitle,
            [rect[0] + 8.0, rect[1] + MENU_HEADER - 3.0],
            0xd8b0ff,
            false,
        );
    }
    let header = MENU_HEADER + if has_subtitle { MENU_SUBLINE } else { 0.0 };
    game.native_fill(
        batch,
        [rect[0] + 4.0, rect[1] + header - 1.0, rect[2] - 8.0, 1.0],
        border,
    );
    for ((label, _), row) in items.iter().zip(rows) {
        if inside(mouse, row) {
            game.native_fill(batch, row, 0xff2e3a48);
        }
        game.map_text(batch, label, [row[0] + 7.0, row[1] + 4.0], 0xdddddd, false);
    }
}

fn draw_notice(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    if let Some(error) = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
        .maps
        .status()
    {
        let text = localized_format!(
            "Map cache: {}",
            "Кеш карты: {}",
            "Cache carte : {}",
            "Caché: {}",
            "Karten-Cache: {}",
            error
        );
        game.native_fill(
            batch,
            [0.0, batch.height - 20.0, batch.width, 20.0],
            0xe0402020,
        );
        game.map_text(batch, &text, [6.0, batch.height - 15.0], 0xffffd24a, false);
    }
    let Some((text, since)) = state.notice.as_ref() else {
        return;
    };
    if since.elapsed() > Duration::from_secs(2) {
        return;
    }
    let width = game.map_text_width(text);
    let x = (batch.width - width) / 2.0;
    let y = batch.height - 38.0;
    game.native_fill(batch, [x - 7.0, y - 4.0, width + 14.0, 14.0], 0xd01b2028);
    game.map_text(batch, text, [x, y], 0xffffd24a, true);
}

pub(super) fn button(
    game: &Game<'_>,
    batch: &mut HudBatch,
    rect: [f32; 4],
    label: &str,
    mouse: [f32; 2],
    enabled: bool,
) {
    let color = if !enabled {
        0xff232a33
    } else if inside(mouse, rect) {
        0xff3a4a5c
    } else {
        0xff2a3542
    };
    game.native_fill(batch, rect, color);
    let text_color = if enabled { 0xffffff } else { 0x707880 };
    game.map_text(
        batch,
        label,
        [
            rect[0] + (rect[2] - game.map_text_width(label)) / 2.0,
            rect[1] + 4.0,
        ],
        text_color,
        false,
    );
}

fn waypoint_visible_rows(height: f32) -> usize {
    (((height - 151.0) / 36.0).floor() as usize).clamp(1, 18)
}

fn shown_waypoints(state: &State) -> impl DoubleEndedIterator<Item = (usize, &Waypoint)> {
    state.waypoints.iter().enumerate().filter(|(_, point)| {
        state.waypoint_group.as_ref().is_none_or(|group| *group == point.options.group)
    })
}

fn waypoint_on_map(game: &Game<'_>, state: &State, point: &Waypoint, include_hidden: bool) -> bool {
    (!point.options.disabled || include_hidden)
        && !(point.options.purpose == 2 && game.map_option("old_deathpoints") == Some(0))
        && (game.map_option("all_waypoint_groups") == Some(1) || point.options.visibility % 2 == 1
            || state.waypoint_group.as_ref().is_none_or(|group| *group == point.options.group))
}

fn show_hidden_on_map(state: &State, large: bool) -> bool {
    large && state.map_panel == MapPanel::Waypoints && state.map_panel_hidden
}

fn waypoint_group_buttons(width: f32) -> [[f32;4];4] {
    let cell = ((width-36.)/4.).clamp(1.,150.);
    [
        [width-cell-12.,29.,cell,17.],
        [12.,29.,cell,17.],
        [16.+cell,29.,cell,17.],
        [20.+2.*cell,29.,cell,17.],
    ]
}

fn waypoint_header(width:f32)->[[f32;4];3] {
    let w=((width-34.)/3.).clamp(1.,120.);
    [[width-3.*w-20.,6.,w,18.],[width-2.*w-16.,6.,w,18.],[width-w-12.,6.,w,18.]]
}

fn waypoint_filters(width:f32)->[[f32;4];3] {
    let w=((width-32.)/4.).clamp(1.,151.);
    [[12.,51.,(width-32.-2.*w).max(1.),18.],[width-2.*w-16.,51.,w,18.],[width-w-12.,51.,w,18.]]
}

fn group_label(group:Option<&str>)->&str {
    match group {
        None=>tr("All groups","Все группы","Tous les groupes","Todos los grupos","Alle Gruppen"),
        Some("")=>tr("Default group","Основная группа","Groupe par défaut","Grupo predeterminado","Standardgruppe"),
        Some(name)=>name,
    }
}

fn waypoint_groups(game:&Game<'_>,state:&State)->Vec<Option<String>> {
    let groups=runtime::initialize(game.native_data_dir(runtime::MOD_ID)).waypoint_group_names(&state.world);
    std::iter::once(None).chain(groups.into_iter().map(Some)).collect()
}

fn visible_group_rows(height:f32)->usize {
    (((height-98.)/20.).floor() as usize).clamp(1,9)
}

fn group_row(width:f32,index:usize)->[f32;4] {
    let selector=waypoint_group_buttons(width)[0];
    [selector[0],selector[1]+selector[3]+2.+20.*index as f32,selector[2],19.]
}

fn listed_waypoints<'a>(game:&Game<'_>,state:&'a State) -> Vec<(usize,&'a Waypoint)> {
    let query=state.waypoint_search.trim().to_lowercase();
    let mut points:Vec<_>=shown_waypoints(state).filter(|(_,point)| {
        (state.waypoint_dimension==Dimension::Player||point.dimension==state.waypoint_dimension)
            && (query.is_empty()||point.name.to_lowercase().contains(&query))
    }).collect();
    match state.waypoint_sort {
        1=>points.sort_by_cached_key(|(_,point)| point.name.to_lowercase()),
        2=>if let Some(player)=game.native_player() {
            let dimension=Dimension::from_id(player.dimension);
            let distance=|point:&Waypoint| if point.dimension==dimension {
                (f64::from(point.x)+0.5-player.x).powi(2)+(f64::from(point.z)+0.5-player.z).powi(2)
            } else {f64::INFINITY};
            points.sort_by(|(_,a),(_,b)| distance(a).total_cmp(&distance(b)));
        },
        _=>if game.map_option("waypoints_new_bottom")==Some(0){points.reverse();},
    }
    points
}

fn visible_waypoint_rows<'a>(game:&Game<'_>,state:&'a State,width:f32,height:f32) -> impl Iterator<Item=(usize,&'a Waypoint,[f32;4])> {
    let points=listed_waypoints(game,state);
    let count=waypoint_visible_rows(height);
    let start=state.waypoint_scroll.min(points.len().saturating_sub(count));
    points.into_iter().skip(start).take(count).enumerate()
        .map(move |(row,(index,point))| (index,point,[12.,74.+row as f32*36.,(width-24.).max(1.),35.]))
}

/// The clickable 3D-visibility toggle at the right end of a waypoint row.
fn waypoint_3d_rect(row: [f32; 4]) -> [f32; 4] {
    [row[0] + row[2] - 24.0, row[1] + 10.0, 20.0, 18.0]
}

fn waypoint_scope_rect(row: [f32; 4]) -> [f32; 4] {
    let toggle = waypoint_3d_rect(row);
    [toggle[0] - 86., toggle[1], 58., 18.]
}

// Shared by drawing and hit testing, including narrow GUI viewports.
fn waypoint_actions(width: f32, height: f32) -> [[f32; 4]; 6] {
    let button_width = ((width - 54.0) / 6.0).max(1.0);
    std::array::from_fn(|i| {
        [
            12.0 + i as f32 * (button_width + 6.0),
            height - 42.0,
            button_width,
            18.0,
        ]
    })
}

fn waypoint_bulk_actions(width: f32, height: f32) -> [[f32; 4]; 6] {
    waypoint_actions(width, height - 24.)
}

fn change_waypoint_group(game: &Game<'_>, state: &mut State, group: Option<String>) {
    runtime::initialize(game.native_data_dir(runtime::MOD_ID))
        .select_waypoint_group(&state.world, group.as_deref());
    state.waypoint_group = group;
    state.waypoint_scroll = 0;
    state.map_panel_scroll = 0;
    state.waypoint_selected = None;
    state.waypoint_bulk = None;
    state.waypoint_confirm_delete = false;
}

fn next_waypoint_group(game: &Game<'_>, state: &mut State) {
    let groups = waypoint_groups(game, state);
    let current = groups.iter().position(|group| *group == state.waypoint_group);
    let next = current.map_or(0, |index| (index + 1) % groups.len());
    change_waypoint_group(game, state, groups[next].clone());
}

pub(super) fn cycle_waypoint_group(game: &mut Game<'_>) {
    let label = {
        let mut state = state();
        if state.world.is_empty() || game.map_client_state().as_ref()
            .and_then(waypoint_identity).as_deref() != Some(state.world.as_str()) {
            return;
        }
        next_waypoint_group(game, &mut state);
        group_label(state.waypoint_group.as_deref()).to_owned()
    };
    game.native_chat(&label);
}

fn waypoint_group_choices(game: &Game<'_>, state: &State) -> Vec<Option<String>> {
    let mut groups = waypoint_groups(game, state);
    if state.waypoint_group_move {
        groups.remove(0);
        if groups.first().is_none_or(|group| group.as_deref() != Some("")) {
            groups.insert(0, Some(String::new()));
        }
    }
    groups
}

fn edit_waypoint_batch(game: &Game<'_>, state: &mut State, action: runtime::WaypointBatch<'_>) {
    let Some(keys) = &state.waypoint_bulk else { return };
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    let notice = match controller.edit_waypoints(&state.world, keys, action) {
        Ok(changed) => {
            if matches!(action, runtime::WaypointBatch::Move(_) | runtime::WaypointBatch::Delete) {
                state.waypoint_bulk = None;
                state.waypoint_selected = None;
            }
            state.waypoints = controller.waypoints(&state.world);
            localized_format!("Waypoints updated: {}", "Меток обновлено: {}",
                "Repères modifiés : {}", "Marcadores actualizados: {}", "Wegpunkte geändert: {}", changed)
        }
        Err(error) => error.to_owned(),
    };
    state.waypoint_confirm_delete = false;
    state.notice = Some((notice, Instant::now()));
}

fn waypoint_search_changed(state: &mut State) {
    state.waypoint_scroll = 0;
    state.map_panel_scroll = 0;
    state.waypoint_selected = None;
    if let Some(keys) = &mut state.waypoint_bulk { keys.clear(); }
    state.waypoint_confirm_delete = false;
}

fn selected_waypoint(state: &State) -> Option<&Waypoint> {
    let selected = if let Some(keys) = &state.waypoint_bulk {
        if keys.len() != 1 { return None; }
        *keys.first()?
    } else {
        state.waypoint_selected?
    };
    shown_waypoints(state).find_map(|(_, point)| (point.key() == selected).then_some(point))
}

fn show_selected_waypoint(state: &mut State) -> Option<Dimension> {
    let key = selected_waypoint(state)?.key();
    focus_waypoint(state, key)
}

fn focus_waypoint(state: &mut State, key: u64) -> Option<Dimension> {
    let waypoint = state.waypoints.iter().find(|point| point.key() == key)?;
    if waypoint.world != state.world {
        return None;
    }
    let spot = Spot {
        dimension: waypoint.dimension,
        x: waypoint.x,
        z: waypoint.z,
        key: waypoint.key(),
    };
    state.map_focus = Some(spot);
    state.mode = Mode::Map;
    state.menu = None;
    state.dragging = false;
    state.waypoint_bulk = None;
    state.waypoint_confirm_delete = false;
    state.elapsed = 1.0;
    Some(spot.dimension)
}

// Apply after the normal dimension/mode transition so it cannot recenter on
// the player and overwrite the requested waypoint on the following frame.
fn apply_map_focus(state: &mut State, large: bool, selected: Dimension) {
    if large
        && let Some(spot) = state.map_focus.take()
        && spot.dimension == selected
    {
        state
            .camera
            .snap([f64::from(spot.x) + 0.5, f64::from(spot.z) + 0.5]);
        state.frame = None;
    }
}

fn draw_waypoints_screen(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    game.native_fill(batch, [0.0, 0.0, batch.width, batch.height], 0xa611151b);
    game.map_text(
        batch,
        tr(
            "Waypoints",
            "Вейпоинты",
            "Repères",
            "Marcadores",
            "Wegpunkte",
        ),
        [12.0, 8.0],
        0xffffff,
        true,
    );
    let mouse = game.cursor();
    let [options,create,back]=waypoint_header(batch.width);
    button(game,batch,options,tr("Settings","Настройки","Options","Opciones","Optionen"),mouse,true);
    button(
        game,
        batch,
        create,
        tr(
            "Create here",
            "Создать здесь",
            "Créer ici",
            "Crear aquí",
            "Hier erstellen",
        ),
        mouse,
        true,
    );
    button(
        game,
        batch,
        back,
        tr("Back", "Назад", "Retour", "Volver", "Zurück"),
        mouse,
        true,
    );
    game.native_fill(batch, [12.0, 28.0, batch.width - 24.0, 1.0], 0xff3a4553);
    let [group_rect,create_group,rename_group,remove_group] = waypoint_group_buttons(batch.width);
    let group = group_label(state.waypoint_group.as_deref());
    button(game,batch,group_rect,&fit_text(game,&format!("{group} ▾"),group_rect[2]-8.),mouse,true);
    let named_group = state.waypoint_group.as_ref().is_some_and(|name|!name.is_empty());
    let empty = named_group && shown_waypoints(state).next().is_none();
    for (rect,label,enabled) in [
        (create_group,tr("New group","Новая группа","Nouveau groupe","Nuevo grupo","Neue Gruppe"),true),
        (rename_group,tr("Rename group","Переименовать","Renommer le groupe","Renombrar grupo","Umbenennen"),named_group),
        (remove_group,tr("Delete empty group","Удалить пустую","Supprimer vide","Borrar vacío","Leere löschen"),empty),
    ] {
        button(game,batch,rect,&fit_text(game,label,rect[2]-8.),mouse,enabled);
    }
    let [search,dimension,sort]=waypoint_filters(batch.width);
    game.native_fill(batch,search,if state.waypoint_search_focus {0xff355464} else {0xff253744});
    let hint=tr("Search waypoints…","Поиск метки…","Rechercher un repère…","Buscar marcador…","Wegpunkt suchen…");
    let search_text=if state.waypoint_search.is_empty(){hint}else{&state.waypoint_search};
    game.map_text(batch,&fit_text(game,search_text,search[2]-12.),[search[0]+6.,search[1]+5.],
        if state.waypoint_search.is_empty(){0xffa8b8c5}else{0xffeef5f8},false);
    let dimension_label=if state.waypoint_dimension==Dimension::Player {
        tr("All dimensions","Все измерения","Toutes dimensions","Todas dimensiones","Alle Dimensionen")
    } else {state.waypoint_dimension.label()};
    button(game,batch,dimension,&fit_text(game,dimension_label,dimension[2]-8.),mouse,true);
    let sort_label=match state.waypoint_sort {
        1=>tr("Name","По имени","Par nom","Por nombre","Nach Name"),
        2=>tr("Distance","По расстоянию","Par distance","Por distancia","Nach Entfernung"),
        _=>tr("Added order","По созданию","Par création","Por creación","Nach Erstellung"),
    };
    button(game,batch,sort,&fit_text(game,sort_label,sort[2]-8.),mouse,true);
    if listed_waypoints(game,state).is_empty() {
        game.map_text(batch,tr("No matching waypoints","Нет подходящих меток","Aucun repère trouvé","No hay marcadores coincidentes","Keine passenden Wegpunkte"),
            [16.,83.],0xffadbcc7,true);
    }
    let mut biome_tooltip = None;
    for (index, waypoint,row) in visible_waypoint_rows(game,state,batch.width,batch.height) {
        let selected = state.waypoint_bulk.as_ref().map_or(
            state.waypoint_selected == Some(waypoint.key()), |keys| keys.contains(&waypoint.key()));

        if selected {
            game.native_fill(batch, row, 0xff2e4a3a);
        } else if inside(mouse, row) {
            game.native_fill(batch, row, 0xff222b35);
        }
        let letter_x = row[0] + 6.0;
        if waypoint.icon == runtime::WaypointIcon::Skull {
            draw_skull(game, batch, [letter_x + 4.0, row[1] + 8.0], 10.0);
        }
        let letter = waypoint_text(state, index, waypoint);
        if waypoint.icon == runtime::WaypointIcon::Letter {
            game.map_text(
                batch,
                &letter,
                [letter_x, row[1] + 3.0],
                if waypoint.color == 0 { 0xffdddddd } else { waypoint_argb(waypoint.color) },
                false,
            );
        }
        let name_x = letter_x + game.map_text_width(&letter).max(6.0) + 6.0;
        let right = waypoint_scope_rect(row)[0] - 6.;
        let name = fit_text(game, &waypoint.name, right - name_x);
        game.map_text(
            batch,
            &name,
            [name_x, row[1] + 3.0],
            0xffffff,
            false,
        );
        let biome = waypoint.biome.as_deref();
        let biome_x = name_x + game.map_text_width(&name) + 7.0;
        if biome_x + runtime::BIOME_ICON_SIZE as f32 <= right {
            biome_icon(game, batch, biome, [biome_x, row[1] + 3.0]);
            if let Some(biome) = biome {
                let label_x = biome_x + 20.0;
                if label_x < right {
                    let label = fit_text(game, &localized_biome(biome), right - label_x);
                    game.map_text(batch, &label, [label_x, row[1] + 3.0], 0x8fcf86, false);
                }
            }
            if inside(mouse, [biome_x, row[1] + 2.0, right - biome_x, 17.0]) {
                let label = biome.map_or_else(
                    || tr("Biome: —", "биом: —", "Biome : —", "Bioma: —", "Biom: —").to_owned(),
                    localized_biome,
                );
                biome_tooltip = Some(label);
            }
        }
        dimension_icon(
            game,
            batch,
            waypoint.dimension,
            [row[0] + 6.0, row[1] + 22.0],
        );
        let mut detail=waypoint.dimension.label().to_owned();
        if game.map_option("waypoint_hide_coords")!=Some(1) {
            let y=if waypoint.options.unspecified_height {"~".to_owned()} else {waypoint.y.to_string()};
            detail.push_str(&format!("  {} {y} {}",waypoint.x,waypoint.z));
        }
        if game.map_option("waypoint_menu_distance")==Some(1)
            && let Some(player)=game.native_player()
            && waypoint.dimension==Dimension::from_id(player.dimension) {
            let distance=(f64::from(waypoint.x)+0.5-player.x).hypot(f64::from(waypoint.z)+0.5-player.z);
            detail.push_str(&format!("  {}",waypoint_distance_text(game,distance)));
        }
        game.map_text(
            batch,
            &fit_text(game, &detail, right - row[0] - 26.),
            [row[0] + 26.0, row[1] + 25.0],
            0xaaaaaa,
            false,
        );
        let toggle = waypoint_3d_rect(row);
        let on = waypoint.show_3d && !waypoint.options.disabled && !waypoint.map_only();
        let disabled = [toggle[0] - 24.0, toggle[1], 20.0, 18.0];
        let scope = waypoint_scope_rect(row);
        let scope_label = if waypoint.options.visibility % 2 == 1 {
            tr("Global", "Глобальная", "Global", "Global", "Global")
        } else {
            tr("Local", "Локальная", "Local", "Local", "Lokal")
        };
        button(game, batch, scope, &fit_text(game, scope_label, scope[2] - 6.), mouse, true);
        button(game, batch, disabled, if waypoint.options.disabled { "○" } else { "●" }, mouse, true);
        game.native_fill(batch, toggle, if on { 0xff2e7d52 } else { 0xff2a3542 });
        if inside(mouse, toggle) {
            game.native_fill(batch, toggle, 0xff3a4a5c);
        }
        let label = tr("3D", "3D", "3D", "3D", "3D");
        game.map_text(
            batch,
            label,
            [
                toggle[0] + (toggle[2] - game.map_text_width(label)) / 2.0,
                toggle[1] + 3.0,
            ],
            if on { 0xffffff } else { 0x707880 },
            false,
        );
    }
    let selected = selected_waypoint(state);
    let [rename, delete, show, share, paste, chat] = waypoint_actions(batch.width, batch.height);
    let count = state.waypoint_bulk.as_ref().map_or(0, BTreeSet::len);
    let selecting = state.waypoint_bulk.is_some();
    let selection_label = if selecting {
        localized_format!("Finish selecting ({})", "Завершить выбор ({})", "Terminer la sélection ({})",
            "Terminar selección ({})", "Auswahl beenden ({})", count)
    } else {
        tr("Select multiple", "Выбрать несколько", "Sélection multiple", "Seleccionar varios", "Mehrfachauswahl").to_owned()
    };
    let three_d = bulk_three_d_value(state);
    for (rect, (label, enabled)) in waypoint_bulk_actions(batch.width, batch.height).into_iter().zip([
        (selection_label.as_str(), true),
        (tr("Select results", "Выбрать результаты", "Sélectionner les résultats", "Seleccionar resultados", "Ergebnisse auswählen"), selecting),
        (tr("Move to group…", "Перенести в группу…", "Déplacer vers un groupe…", "Mover a un grupo…", "In Gruppe verschieben…"), count != 0),
        (tr("Enable markers", "Показать метки", "Afficher les repères", "Mostrar marcadores", "Wegpunkte anzeigen"), count != 0),
        (tr("Hide markers", "Скрыть метки", "Masquer les repères", "Ocultar marcadores", "Wegpunkte ausblenden"), count != 0),
        (if three_d {tr("Enable 3D", "Включить 3D", "Activer la 3D", "Activar 3D", "3D einschalten")}
             else {tr("Hide 3D", "Скрыть 3D", "Masquer la 3D", "Ocultar 3D", "3D ausblenden")}, count != 0),
    ]).filter(|(_, (_, enabled))| selecting || *enabled) {
        button(game, batch, rect, &fit_text(game, label, rect[2] - 6.), mouse, enabled);
    }
    button(
        game,
        batch,
        rename,
        tr("Edit", "Изменить", "Modifier", "Editar", "Bearbeiten"),
        mouse,
        selected.is_some(),
    );
    button(
        game,
        batch,
        delete,
        if state.waypoint_confirm_delete {
            tr("Confirm delete", "Подтвердить удаление", "Confirmer", "Confirmar", "Löschen bestätigen")
        } else {
            tr("Delete", "Удалить", "Supprimer", "Eliminar", "Löschen")
        },
        mouse,
        selected.is_some() || count != 0,
    );
    button(
        game,
        batch,
        show,
        tr(
            "Show on map",
            "Показать на карте",
            "Voir sur la carte",
            "Ver en el mapa",
            "Auf Karte zeigen",
        ),
        mouse,
        selected.is_some(),
    );
    button(game, batch, share, tr("Copy link", "Копировать", "Copier le lien", "Copiar enlace", "Link kopieren"), mouse, selected.is_some());
    button(game, batch, paste, tr("Paste link", "Вставить", "Coller le lien", "Pegar enlace", "Link einfügen"), mouse, true);
    button(game, batch, chat, tr("Open in chat", "Открыть в чате", "Ouvrir dans le chat",
        "Abrir en el chat", "Im Chat öffnen"), mouse, selected.is_some());
    draw_notice(game, batch, state);
    if let Some(label) = biome_tooltip {
        draw_tooltip(game, batch, &label, mouse);
    }
    if state.waypoint_groups_open {
        let groups=waypoint_group_choices(game,state);
        let count=visible_group_rows(batch.height);
        let start=state.waypoint_groups_scroll.min(groups.len().saturating_sub(count));
        for (i,group) in groups.into_iter().skip(start).take(count).enumerate() {
            let row=group_row(batch.width,i);
            let selected=!state.waypoint_group_move && group==state.waypoint_group;
            let label=if selected {format!("✓ {}",group_label(group.as_deref()))}
                else {group_label(group.as_deref()).to_owned()};
            button(game,batch,row,&fit_text(game,&label,row[2]-8.),mouse,true);
        }
    }

}

const NAME_FORM_HEIGHT: f32 = 156.0;

fn name_form_rect(viewport_width: f32, height: f32) -> [f32; 4] {
    let width = 340.0_f32.min(viewport_width - 24.0);
    [
        (viewport_width - width) / 2.0,
        height / 2.0 - NAME_FORM_HEIGHT / 2.0,
        width,
        NAME_FORM_HEIGHT,
    ]
}

fn group_field(rect: [f32; 4]) -> [f32; 4] {
    [rect[0] + 10.0, rect[1] + 56.0, rect[2] - 20.0, 18.0]
}

fn color_swatch_at(rect: [f32; 4], index: usize) -> [f32; 4] {
    [
        rect[0] + 10.0 + index as f32 * 22.0,
        rect[1] + 91.0,
        18.0,
        18.0,
    ]
}

fn draw_name_form(game: &Game<'_>, batch: &mut HudBatch, state: &State, form: &NameForm) {
    match form.background {
        Mode::Map => draw_map_screen(game, batch, state),
        Mode::Waypoints => draw_waypoints_screen(game, batch, state),
        Mode::Name | Mode::Settings => {}
        Mode::Caches => draw_cache_screen(game, batch, state),
    }
    game.native_fill(batch, [0.0, 0.0, batch.width, batch.height], 0x70000000);
    let rect = name_form_rect(batch.width, batch.height);
    game.native_fill(batch, rect, 0xf01b2028);
    game.native_fill(batch, [rect[0], rect[1], rect[2], 1.0], 0xff3a4553);
    game.native_fill(
        batch,
        [rect[0], rect[1] + rect[3] - 1.0, rect[2], 1.0],
        0xff3a4553,
    );
    game.map_text(
        batch,
        if form.group_edit.is_some() {
            tr("Group name","Название группы","Nom du groupe","Nombre del grupo","Gruppenname")
        } else if form.rename.is_some() {
            tr(
                "Edit waypoint",
                "Изменить вейпоинт",
                "Modifier le repère",
                "Editar marcador",
                "Wegpunkt bearbeiten",
            )
        } else {
            tr(
                "Waypoint name",
                "Название вейпоинта",
                "Nom du repère",
                "Nombre del marcador",
                "Wegpunktname",
            )
        },
        [rect[0] + 10.0, rect[1] + 8.0],
        0xffffff,
        true,
    );
    let field = [rect[0] + 10.0, rect[1] + 26.0, rect[2] - 20.0, 18.0];
    game.native_fill(batch, field, 0xff0d1117);
    game.map_text(
        batch,
        &format!("{}{}", form.text, if form.group_focused { "" } else { "_" }),
        [field[0] + 4.0, field[1] + 5.0],
        0xffffff,
        false,
    );
    if form.group_edit.is_none() {
    let group = group_field(rect);
    game.map_text(batch, tr("Group", "Группа", "Groupe", "Grupo", "Gruppe"), [group[0], group[1]-10.0], 0xaaaaaa, false);
    game.native_fill(batch, group, 0xff0d1117);
    let value = fit_text(game, &form.group, group[2]-12.0);
    game.map_text(batch, &format!("{value}{}", if form.group_focused { "_" } else { "" }), [group[0]+4.0, group[1]+5.0], 0xffffff, false);
    game.map_text(
        batch,
        tr("Colour", "Цвет", "Couleur", "Color", "Farbe"),
        [rect[0] + 10.0, rect[1] + 79.0],
        0xaaaaaa,
        false,
    );
    for (index, &color) in runtime::WAYPOINT_COLORS.iter().enumerate() {
        let swatch = color_swatch_at(rect, index);
        if form.color == color {
            game.native_fill(
                batch,
                [
                    swatch[0] - 2.0,
                    swatch[1] - 2.0,
                    swatch[2] + 4.0,
                    swatch[3] + 4.0,
                ],
                0xffffffff,
            );
        }
        game.native_fill(batch, swatch, waypoint_argb(color));
    }
    }
    let mouse = game.cursor();
    let save = [
        rect[0] + rect[2] - 184.0,
        rect[1] + rect[3] - 24.0,
        80.0,
        18.0,
    ];
    let cancel = [
        rect[0] + rect[2] - 96.0,
        rect[1] + rect[3] - 24.0,
        80.0,
        18.0,
    ];
    button(
        game,
        batch,
        save,
        tr("Save", "Сохранить", "Enregistrer", "Guardar", "Speichern"),
        mouse,
        true,
    );
    button(
        game,
        batch,
        cancel,
        tr("Cancel", "Отмена", "Annuler", "Cancelar", "Abbrechen"),
        mouse,
        true,
    );
    game.map_text(
        batch,
        tr(
            "Enter — save, Esc — cancel",
            "Enter — сохранить, Esc — отмена",
            "Entrée — enregistrer, Échap — annuler",
            "Intro — guardar, Esc — cancelar",
            "Eingabe — speichern, Esc — abbrechen",
        ),
        [rect[0] + 10.0, rect[1] + 116.0],
        0x888f98,
        false,
    );
}

fn draw_tooltip(game: &Game<'_>, batch: &mut HudBatch, text: &str, at: [f32; 2]) {
    let width = game.map_text_width(text);
    let x = (at[0] + 10.0).min(batch.width - width - 10.0).max(4.0);
    let y = (at[1] - 18.0).max(4.0);
    game.native_fill(batch, [x - 4.0, y - 3.0, width + 8.0, 13.0], 0xf01b2028);
    game.map_text(batch, text, [x, y], 0xffffd24a, true);
}

// Hit-test the full short label of the waypoint under the map pointer.
fn waypoint_at_pointer<'a>(
    game: &Game<'_>,
    state: &'a State,
    view: &View,
    rect: [f32; 4],
    mouse: [f32; 2],
) -> Option<&'a Waypoint> {
    state
        .waypoints
        .iter()
        .enumerate()
        .find(|(index, waypoint)| {
            if waypoint.dimension != view.dimension || !waypoint_on_map(game, state, waypoint, show_hidden_on_map(state, view.large)) {
                return false;
            }
            let pixel = view.screen_at([f64::from(waypoint.x) + 0.5, f64::from(waypoint.z) + 0.5]);
            let x = rect[0] + pixel[0] as f32 * rect[2] / view.width as f32;
            let y = rect[1] + pixel[1] as f32 * rect[3] / view.height as f32;
            let letter = waypoint_text(state, *index, waypoint);
            let radius = if waypoint.icon == runtime::WaypointIcon::Letter {
                (game.map_text_width(&letter) + 4.).max(14.) / 2.
            } else {
                7.
            };
            (mouse[0] - x).abs() <= radius && (mouse[1] - y).abs() <= 7.0
        })
        .map(|(_, point)| point)
}

fn portal_near(state: &State, dimension: Dimension, block: [i32; 2]) -> Option<(Portal, [i32; 3])> {
    state
        .portals
        .iter()
        .filter(|(at_dimension, at, _)| {
            *at_dimension == dimension
                && (at[0] - block[0]).abs() <= 2
                && (at[2] - block[1]).abs() <= 2
        })
        .min_by_key(|(_, at, _)| {
            let dx = at[0] - block[0];
            let dz = at[2] - block[1];
            dx * dx + dz * dz
        })
        .map(|(_, at, kind)| (*kind, *at))
}

fn world_screen(view: &View, rect: [f32; 4], at: [f64; 2]) -> [f32; 2] {
    let pixel = view.screen_at(at);
    [
        rect[0] + pixel[0] as f32 * rect[2] / view.width as f32,
        rect[1] + pixel[1] as f32 * rect[3] / view.height as f32,
    ]
}

type ScreenSegment = ([f32; 2], [f32; 2]);

fn draw_segments(
    game: &Game<'_>,
    batch: &mut HudBatch,
    rect: [f32; 4],
    segments: &[ScreenSegment],
    color: u32,
) {
    for &(from, to) in segments {
        draw_line(game, batch, from, to, rect, color);
    }
}

fn world_box_segments(view: &View, rect: [f32; 4], bounds: [f64; 4]) -> Vec<ScreenSegment> {
    let [x0, z0, x1, z1] = bounds;
    let corners = [[x0, z0], [x1, z0], [x1, z1], [x0, z1]];
    let points = corners.map(|corner| world_screen(view, rect, corner));
    (0..4)
        .map(|index| (points[index], points[(index + 1) % 4]))
        .collect()
}

fn draw_world_box(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    bounds: [f64; 4],
    color: u32,
) {
    draw_segments(
        game,
        batch,
        rect,
        &world_box_segments(view, rect, bounds),
        color,
    );
}

fn draw_claims(game: &Game<'_>, batch: &mut HudBatch, state: &State, view: &View, rect: [f32; 4]) {
    if game.map_option(if view.large { "world_claims" } else { "minimap_claims" }) == Some(0) {
        return;
    }
    let fill = game.map_option(if view.large { "world_claim_fill" } else { "minimap_claim_fill" }).unwrap_or(46);
    let border = game.map_option(if view.large { "world_claim_border" } else { "minimap_claim_border" }).unwrap_or(80);
    let opacity = |percent| (u32::from(percent) * 255 / 100) << 24;
    let region = view.atlas_region();
    let chunk_at = |x: i64| x.div_euclid(16) as i32;
    let min = [chunk_at(region[0]), chunk_at(region[1])];
    let max = [chunk_at(region[0] + region[2]), chunk_at(region[1] + region[3])];
    let native = state.claims.iter().filter_map(|(&chunk, owner)| {
        (chunk.0 == view.dimension).then_some((chunk, owner.is_none()))
    });
    let opac = state.opac.chunks(view.dimension, min, max).filter_map(|chunk| {
        let (owner, _) = state.opac.owner(chunk)?;
        Some((chunk, owner == game.own_uuid()))
    });
    for ((_, x, z), own) in native.chain(opac) {
        let [x, z] = [i64::from(x) * 16, i64::from(z) * 16];
        if x + 16 < region[0] || z + 16 < region[1] || x > region[0] + region[2] || z > region[1] + region[3] {
            continue;
        }
        let points = maps::terrain_polygon(view, [x, z, 16, 16], [0.; 4]);
        let points: Vec<_> = points.into_iter().map(|p| [rect[0]+(p[0] as f32+0.5)*rect[2], rect[1]+(p[1] as f32+0.5)*rect[3]]).collect();
        if points.len() < 3 { continue; }
        let color = if own { 0x5ddd91 } else { 0xffac69 };
        for i in 1..points.len()-1 {
            game.native_quad(batch, [points[0], points[i], points[i+1], points[i+1]], opacity(fill) | color);
        }
        for i in 0..points.len() {
            draw_line(game, batch, points[i], points[(i+1)%points.len()], rect, opacity(border) | color);
        }
    }
}

fn draw_waystones(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4]) {
    if game.map_option(if view.large { "world_map_waypoints" } else { "minimap_waypoints" }) == Some(0) {
        return;
    }
    let discovered = game.map_option("waystones_discovered") != Some(0);
    let other = game.map_option("waystones_other") != Some(0);
    for stone in WAYSTONES.lock().unwrap_or_else(|error| error.into_inner()).values() {
        if stone.dimension != view.dimension || stone.hidden || !(if stone.discovered { discovered } else { other }) {
            continue;
        }
        let [x,y] = world_screen(view, rect, [f64::from(stone.at[0]) + 0.5, f64::from(stone.at[2]) + 0.5]);
        if !map_overlay_inside([x,y], rect, view.round) { continue; }
        game.native_quad(batch, [[x,y-5.],[x+4.,y],[x,y+5.],[x-4.,y]], 0xffc9a7ed);
        if view.large && !stone.name.is_empty() && map_overlay_inside([x+5.+game.map_text_width(&stone.name),y], rect, false) {
            game.map_text(batch, &stone.name, [x+5.,y-4.], 0xffead9ff, true);
        }
    }
}

fn circle_segments(
    view: &View,
    rect: [f32; 4],
    center: [f64; 2],
    radius: f64,
) -> Vec<ScreenSegment> {
    if radius <= 0.0 {
        return Vec::new();
    }
    let count = 96;
    let mut segments = Vec::with_capacity(count);
    let mut previous = world_screen(view, rect, [center[0] + radius, center[1]]);
    for index in 1..=count {
        let angle = std::f64::consts::TAU * index as f64 / count as f64;
        let point = world_screen(
            view,
            rect,
            [
                center[0] + angle.cos() * radius,
                center[1] + angle.sin() * radius,
            ],
        );
        segments.push((previous, point));
        previous = point;
    }
    segments
}

// The outer edges of the chunks the game actually draws at this radius,
// including the slack `in_view_distance` allows on each axis.
fn chunk_border_segments(
    view: &View,
    rect: [f32; 4],
    center: (i32, i32),
    radius: i32,
) -> Vec<ScreenSegment> {
    if radius < 1 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for dz in -radius..=radius {
        let mut min_dx = None;
        let mut max_dx = None;
        for dx in -radius..=radius {
            if horizon_client::streaming::in_view_distance((0, 0), radius, dx, dz) {
                min_dx = Some(min_dx.map_or(dx, |value: i32| value.min(dx)));
                max_dx = Some(max_dx.map_or(dx, |value: i32| value.max(dx)));
            }
        }
        if let (Some(min_dx), Some(max_dx)) = (min_dx, max_dx) {
            rows.push((dz, min_dx, max_dx));
        }
    }
    let Some(first) = rows.first().copied() else {
        return Vec::new();
    };
    let last = *rows.last().unwrap();
    let world = |dx: i32, dz: i32| {
        [
            f64::from(center.0 + dx) * 16.0,
            f64::from(center.1 + dz) * 16.0,
        ]
    };
    let mut segments = Vec::new();
    segments.push((
        world_screen(view, rect, world(first.1, first.0)),
        world_screen(view, rect, world(first.2 + 1, first.0)),
    ));
    segments.push((
        world_screen(view, rect, world(last.2 + 1, last.0 + 1)),
        world_screen(view, rect, world(last.1, last.0 + 1)),
    ));
    for (index, &(dz, min_dx, max_dx)) in rows.iter().enumerate() {
        let left = world(min_dx, dz);
        let right = world(max_dx + 1, dz);
        segments.push((
            world_screen(view, rect, left),
            world_screen(view, rect, [left[0], left[1] + 16.0]),
        ));
        segments.push((
            world_screen(view, rect, right),
            world_screen(view, rect, [right[0], right[1] + 16.0]),
        ));
        if let Some(&(_, next_min, next_max)) = rows.get(index + 1) {
            if next_min != min_dx {
                segments.push((
                    world_screen(view, rect, [left[0], left[1] + 16.0]),
                    world_screen(view, rect, [world(next_min, dz + 1)[0], left[1] + 16.0]),
                ));
            }
            if next_max != max_dx {
                segments.push((
                    world_screen(view, rect, [right[0], right[1] + 16.0]),
                    world_screen(
                        view,
                        rect,
                        [world(next_max + 1, dz + 1)[0], right[1] + 16.0],
                    ),
                ));
            }
        }
    }
    segments
}

fn segment_distance(point: [f32; 2], from: [f32; 2], to: [f32; 2]) -> f32 {
    let vx = to[0] - from[0];
    let vy = to[1] - from[1];
    let wx = point[0] - from[0];
    let wy = point[1] - from[1];
    let length = vx * vx + vy * vy;
    let t = if length > 0.0 {
        ((wx * vx + wy * vy) / length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = point[0] - (from[0] + vx * t);
    let dy = point[1] - (from[1] + vy * t);
    (dx * dx + dy * dy).sqrt()
}

fn segments_distance(point: [f32; 2], segments: &[ScreenSegment]) -> f32 {
    segments
        .iter()
        .map(|&(from, to)| segment_distance(point, from, to))
        .fold(f32::INFINITY, f32::min)
}

fn draw_border_hint(
    game: &Game<'_>,
    batch: &mut HudBatch,
    text: &str,
    mouse: [f32; 2],
    rect: [f32; 4],
) {
    if !inside(mouse, rect) {
        return;
    }
    let width = game.map_text_width(text);
    let x = (mouse[0] + 12.0)
        .min(rect[0] + rect[2] - width - 6.0)
        .max(rect[0] + 4.0);
    let y = (mouse[1] - 16.0).max(rect[1] + 4.0);
    game.native_fill(batch, [x - 4.0, y - 3.0, width + 8.0, 13.0], 0xe010151d);
    game.map_text(batch, text, [x, y], 0xffff9a9a, true);
}

fn draw_border(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    mouse: Option<[f32; 2]>,
) {
    let border_color = 0xffe04040;
    let mut labels = Vec::<(String, Vec<ScreenSegment>)>::new();
    if matches!(view.dimension, Dimension::Overworld | Dimension::Nether) {
        let [mut min_x, mut min_z, mut max_x, mut max_z] = game.border.bounds();
        if view.dimension == Dimension::Nether {
            min_x /= 8.0;
            min_z /= 8.0;
            max_x /= 8.0;
            max_z /= 8.0;
        }
        let segments = world_box_segments(view, rect, [min_x, min_z, max_x, max_z]);
        draw_segments(game, batch, rect, &segments, border_color);
        if view.large {
            labels.push((
                tr(
                    "World border",
                    "Граница мира",
                    "Bordure du monde",
                    "Borde del mundo",
                    "Weltgrenze",
                )
                .to_owned(),
                segments,
            ));
        }
    }

    let Some(player) = game.native_player() else {
        return;
    };
    if view.dimension != Dimension::from_id(player.dimension) {
        return;
    }
    let center = game.streaming.center;
    let center_blocks = [
        f64::from(center.0) * 16.0 + 8.0,
        f64::from(center.1) * 16.0 + 8.0,
    ];
    // The game's view still shows the first ring outside the nominal radius
    // while chunks arrive, so draw one extra chunk to contain it.
    let chunk_radius = match &game.remote {
        Some(remote) => game.streaming.radius.min(remote.view_distance + 1),
        None => game.streaming.radius,
    }
    .max(0)
        + 1;
    let full_radius = if game.lod.is_some() {
        chunk_radius.max(game.lod_distance() as i32)
    } else {
        chunk_radius
    };
    let chunk_segments = chunk_border_segments(view, rect, center, chunk_radius);
    draw_segments(game, batch, rect, &chunk_segments, border_color);
    if view.large {
        labels.push((
            localized_format!(
                "Chunk render: {} chunks",
                "Прорисовка чанков: {} чанков",
                "Rendu des chunks : {} chunks",
                "Render de chunks: {} chunks",
                "Chunk-Sichtweite: {} Chunks",
                chunk_radius
            ),
            chunk_segments,
        ));
    }
    if full_radius > chunk_radius {
        let lod_segments =
            circle_segments(view, rect, center_blocks, f64::from(full_radius) * 16.0);
        draw_segments(game, batch, rect, &lod_segments, border_color);
        if view.large {
            labels.push((
                localized_format!(
                    "Actual render (LOD): {} chunks",
                    "Фактическая прорисовка (LOD): {} чанков",
                    "Rendu réel (LOD) : {} chunks",
                    "Render real (LOD): {} chunks",
                    "Tatsächliche Sichtweite (LOD): {} Chunks",
                    full_radius
                ),
                lod_segments,
            ));
        }
    }
    if view.large
        && let Some(mouse) = mouse
    {
        let mut nearest: Option<(f32, &String)> = None;
        for (text, segments) in &labels {
            let distance = segments_distance(mouse, segments);
            let closer = nearest.is_none_or(|(best, _)| distance < best);
            if distance <= 8.0 && closer {
                nearest = Some((distance, text));
            }
        }
        if let Some((_, text)) = nearest {
            draw_border_hint(game, batch, text, mouse, rect);
        }
    }
}

fn draw_line(
    game: &Game<'_>,
    batch: &mut HudBatch,
    mut from: [f32; 2],
    mut to: [f32; 2],
    rect: [f32; 4],
    color: u32,
) {
    let [x0, y0, x1, y1] = [rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]];
    let dx = to[0] - from[0];
    let dy = to[1] - from[1];
    let mut t0 = 0.0_f32;
    let mut t1 = 1.0_f32;
    for (p, q) in [
        (-dx, from[0] - x0),
        (dx, x1 - from[0]),
        (-dy, from[1] - y0),
        (dy, y1 - from[1]),
    ] {
        if p.abs() < f32::EPSILON {
            if q < 0.0 {
                return;
            }
            continue;
        }
        let r = q / p;
        if p < 0.0 {
            if r > t1 {
                return;
            }
            if r > t0 {
                t0 = r;
            }
        } else {
            if r < t0 {
                return;
            }
            if r < t1 {
                t1 = r;
            }
        }
    }
    from = [from[0] + t0 * dx, from[1] + t0 * dy];
    to = [from[0] + (t1 - t0) * dx, from[1] + (t1 - t0) * dy];
    let steps = ((to[0] - from[0]).abs().max((to[1] - from[1]).abs()) / 2.0).ceil() as i32;
    for step in 0..=steps {
        let t = step as f32 / steps.max(1) as f32;
        game.native_fill(
            batch,
            [
                from[0] + (to[0] - from[0]) * t,
                from[1] + (to[1] - from[1]) * t,
                2.0,
                2.0,
            ],
            color,
        );
    }
}

fn project(camera: &mod_api::Camera, world: [f64; 3], size: [f32; 2]) -> Option<[f32; 2]> {
    let relative = [
        (world[0] - camera.position[0]) as f32,
        (world[1] - camera.position[1]) as f32,
        (world[2] - camera.position[2]) as f32,
        1.0,
    ];
    let transform = |matrix: [f32; 16], vector: [f32; 4]| {
        [
            matrix[0] * vector[0]
                + matrix[4] * vector[1]
                + matrix[8] * vector[2]
                + matrix[12] * vector[3],
            matrix[1] * vector[0]
                + matrix[5] * vector[1]
                + matrix[9] * vector[2]
                + matrix[13] * vector[3],
            matrix[2] * vector[0]
                + matrix[6] * vector[1]
                + matrix[10] * vector[2]
                + matrix[14] * vector[3],
            matrix[3] * vector[0]
                + matrix[7] * vector[1]
                + matrix[11] * vector[2]
                + matrix[15] * vector[3],
        ]
    };
    let view = transform(camera.view, relative);
    let clip = transform(camera.projection, view);
    if clip[3] <= camera.near {
        return None;
    }
    let x = clip[0] / clip[3];
    let y = clip[1] / clip[3];
    Some([(x * 0.5 + 0.5) * size[0], (1.0 - (y * 0.5 + 0.5)) * size[1]])
}

/// The exact camera the game uses this frame, rebuilt from its own view state.
/// `native_camera` only returns a frame when a trait mod (`mod_api::mods`) is
/// registered; this injection-based mod has none, so it reads the same camera
/// position, rotation and field of view (including FOV/zoom changes) directly.
fn world_camera(
    game: &Game<'_>,
    player: &crate::game::native_hooks::NativePlayer,
    size: [f32; 2],
) -> mod_api::Camera {
    let partial = game.clock.camera_fraction() as f32;
    let position = game.view.position;
    let yaw = game.view.y_rot;
    let pitch = game.view.x_rot;
    let fov = game.fov_degrees(partial, position);
    mod_api::Camera {
        position,
        eye_height: game.view.eye_height,
        partial_tick: partial,
        yaw,
        pitch,
        fov_degrees: fov,
        near: 0.05,
        far: 2048.0,
        // Match `Game::render` exactly: the bob and spin matrices go into the
        // view, so markers stay glued to the world instead of drifting as the
        // camera sways.
        view: (game.bob_matrix(partial)
            * game.spinning_matrix(partial)
            * horizon_client::camera::view_matrix(yaw, pitch))
        .to_cols_array(),
        projection: horizon_client::camera::perspective(fov, size[0], size[1], 2048.0, 0.05, true),
        dimension: player.dimension.to_owned(),
    }
}

fn waypoint_distance_text(game:&Game<'_>,meters:f64) -> String {
    let limit=game.map_number("waypoint_km_at").unwrap_or(-1.);
    let precision=usize::from(game.map_option("waypoint_dist_precision").unwrap_or(0));
    if limit>=0. && meters>=limit {format!("{value:.precision$} km",value=meters/1000.)}
    else {format!("{meters:.precision$} m")}
}

fn waypoint_label_left(x: f32, marker_width: f32, text_width: f32, viewport_width: f32) -> f32 {
    let gap = marker_width / 2. + 4.;
    if x + text_width + gap + 3. > viewport_width {
        x - text_width - gap
    } else {
        x + gap
    }
}

fn draw_world_waypoints(
    game: &Game<'_>,
    batch: &mut HudBatch,
    state: &State,
    settings: &runtime::minimap::Settings,
) {
    if !settings.world_waypoints {
        return;
    }
    let Some(player) = game.native_player() else {
        return;
    };
    let camera = world_camera(game, &player, [batch.width, batch.height]);
    let dimension = Dimension::from_id(player.dimension);
    let style = settings.world_waypoint_style;
    // In the 3D view the pointer is the crosshair at the screen centre: a label
    // is "hovered" when its marker sits under it. The radius is forgiving so a
    // marker just off the crosshair still counts.
    let crosshair = [batch.width / 2.0, batch.height / 2.0];
    let min_distance = game.map_number("waypoint_min_distance").unwrap_or(0.);
    let mut max_distance = game.map_number("waypoint_max_distance").unwrap_or(0.);
    if dimension == Dimension::Nether && game.map_option("world_waypoint_distance_scale")==Some(1) {
        max_distance/=8.;
    }
    let base_scale = waypoint_scale(game,"waypoint_world_scale");
    let close_scale = game.map_number("waypoint_close_scale").unwrap_or(1.) as f32;
    let opacity = game.map_option("waypoint_world_opacity").unwrap_or(100);
    const HOVER_RADIUS: f32 = 24.0;
    let show_multiple = match game.map_option("waypoint_multiple_info").unwrap_or(1) {
        1 => game.player.as_ref().is_some_and(|player| player.local_crouching),
        2 => true,
        _ => false,
    };
    let mut hover_shown = false;
    for (index, waypoint) in state.waypoints.iter().enumerate() {
        if !waypoint.show_3d || waypoint.map_only() || waypoint.dimension != dimension
            || !waypoint_on_map(game, state, waypoint, false)
        {
            continue;
        }
        let world = [
            f64::from(waypoint.x) + 0.5,
            if waypoint.options.unspecified_height { player.y + 1.5 } else { f64::from(waypoint.y) + 1.5 },
            f64::from(waypoint.z) + 0.5,
        ];
        let distance2 = (world[0]-player.x).powi(2) + (world[1]-player.y).powi(2) + (world[2]-player.z).powi(2);
        if distance2 < min_distance.powi(2)
            || (waypoint.options.visibility % 2 == 0 && max_distance > 0. && distance2 > max_distance.powi(2)) {
            continue;
        }
        let scale=base_scale*(1.+(close_scale-1.)*(1.-distance2.sqrt() as f32/12.).clamp(0.,1.));
        let Some([x, y]) = project(&camera, world, [batch.width, batch.height]) else {
            continue;
        };
        if x < -8.0 || y < -8.0 || x > batch.width + 8.0 || y > batch.height + 8.0 {
            continue;
        }
        let letter = waypoint_text(state, index, waypoint);
        let width = if waypoint.icon == runtime::WaypointIcon::Letter {
            (game.map_text_width(&letter) + 4.).max(10.)
        } else {
            10.
        };
        let start = batch.vertices.len();
        game.native_fill(
            batch,
            [x - width / 2. - 1., y - 6.0, width + 2., 12.0],
            0xff101010,
        );
        game.native_fill(
            batch,
            [x - width / 2., y - 5.0, width, 10.0],
            waypoint_argb(waypoint.color),
        );
        if waypoint.icon == runtime::WaypointIcon::Skull {
            draw_skull(game, batch, [x, y], 9.5);
        } else {
            game.map_text(
                batch,
                &letter,
                [x - game.map_text_width(&letter) / 2.0, y - 4.0],
                if waypoint.color == 0 { 0xffffffff } else { 0xff201800 },
                false,
            );
        }
        batch.scale_since(start,x,y,scale,scale);
        // The coloured marker is always drawn; the label below it depends on the
        // selected style. Distance changes each frame, so it is only composed
        // when the style asks for it and, with "labels only on hover", when the
        // crosshair is over the marker.
        let hovered =
            (x - crosshair[0]).abs() <= HOVER_RADIUS && (y - crosshair[1]).abs() <= HOVER_RADIUS;
        use runtime::minimap::WorldMarkerStyle;
        let dx=world[0]-player.x;
        let dz=world[2]-player.z;
        let dy=world[1]-player.y-1.62;
        let yaw=(-dx).atan2(dz).to_degrees();
        let pitch=(-dy).atan2(dx.hypot(dz)).to_degrees();
        let within_angle = ((yaw-f64::from(player.yaw)+180.).rem_euclid(360.)-180.).abs()
            <= game.map_number("waypoint_horizontal_angle").unwrap_or(10.)
            && (pitch-f64::from(player.pitch)).abs()<=game.map_number("waypoint_vertical_angle").unwrap_or(180.);
        let distance_mode=game.map_option("waypoint_distance_mode").unwrap_or(3);
        let show_distance=match distance_mode {
            0=>false,1=>within_angle,2=>true,_=>style!=WorldMarkerStyle::Letter,
        };
        let show_name=game.map_option("waypoint_name_enabled")!=Some(0) &&
            (style==WorldMarkerStyle::NameDistance || distance_mode!=3);
        let distance=if game.map_option("waypoint_short_distance")==Some(1) {dx.hypot(dz)} else {distance2.sqrt()};
        let dist=show_distance.then(|| waypoint_distance_text(game,distance));
        let name=show_name.then_some(waypoint.name.as_str());
        if (!settings.labels_on_hover || hovered) && (dist.is_some() || name.is_some())
            && (!hovered || show_multiple || !hover_shown)
        {
            hover_shown |= hovered;
            let name_scale=waypoint_scale(game,"waypoint_name_scale");
            let dist_scale=waypoint_scale(game,"waypoint_dist_scale");
            let name_width=name.map_or(0.,|name|game.map_text_width(name)*name_scale);
            let dist_width=dist.as_ref().map_or(0.,|distance|game.map_text_width(distance)*dist_scale);
            let gap=if name.is_some() && dist.is_some(){5.} else {0.};
            let label_x=waypoint_label_left(x,width*scale,name_width+dist_width+gap,batch.width);
            let label_y=y-4.;
            let label_color=if waypoint.icon==runtime::WaypointIcon::Skull {0xffe9e6dc}
                else {waypoint_argb(waypoint.color)};
            game.native_fill(batch,[label_x-3.,label_y-2.,name_width+dist_width+gap+6.,11.*name_scale.max(dist_scale)],0xd010151d);
            for (text,left,scale) in [(name,label_x,name_scale),(dist.as_deref(),label_x+name_width+gap,dist_scale)] {
                if let Some(text)=text {
                    let first=batch.vertices.len();
                    game.map_text(batch,text,[left,label_y],label_color,true);
                    batch.scale_since(first,left,label_y,scale,scale);
                }
            }
        }
        set_opacity(batch,start,opacity);
    }
}

// Camera motion never waits for terrain preparation. Newly exposed areas may
// be empty until the next atlas arrives; existing tiles retain world positions.
fn complete_map_view(desired: View, prepared: &View) -> View {
    if desired.session != prepared.session
        || desired.dimension != prepared.dimension
        || desired.layer != prepared.layer
        || desired.large != prepared.large
    {
        return prepared.clone();
    }
    desired
}

fn official_smp(game: &Game<'_>) -> bool {
    game.remote.as_ref().is_some_and(|remote| {
        let address = remote.session.address.as_str();
        address.eq_ignore_ascii_case("play.playhorizon.dev:24454")
            || address.eq_ignore_ascii_case("play.playhorizon.dev")
    })
}

fn base_world(identity: &super::minimap_compat::ClientState) -> Option<String> {
    use super::minimap_compat::WorldId as NativeWorldId;
    match identity.world.as_ref()? {
        NativeWorldId::Singleplayer(folder) => runtime::waypoint_world(true, folder),
        NativeWorldId::Server(address) => runtime::waypoint_world(false, address),
    }
}

fn waypoint_identity(identity: &super::minimap_compat::ClientState) -> Option<String> {
    base_world(identity).map(runtime::active_world)
}

static DEATH_CAPTURE: Mutex<runtime::DeathCapture> = Mutex::new(runtime::DeathCapture::new());

fn protocol_death(position: Option<&horizon_protocol::packets::play::GlobalPos>) -> Option<runtime::DeathLocation> {
    let position = position?;
    let dimension = Dimension::from_id(&position.dimension);
    (dimension != Dimension::Player).then_some((dimension, position.pos))
}

fn record_death(game: &Game<'_>, world: &str, (dimension, [x, y, z]): runtime::DeathLocation) {
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    if !controller.settings().death_waypoints { return; }
    let biome = known_biome(game, dimension, [x, y, z]);
    if controller.add_death_waypoint(world, dimension, x, y, z, biome.as_deref()) {
        let mut state = state();
        if state.world == world {
            state.deaths_outside_reach.clear();
            if game.map_option("auto_subworld_on_death") != Some(0) {
                change_waypoint_group(game, &mut state, None);
            }
        }
    }
}

pub(super) fn client_packet(
    game: &mut Game<'_>,
    packet: &horizon_protocol::packets::play::Clientbound,
) {
    use horizon_protocol::packets::play::Clientbound;
    if !game.native_window() || !game.map_on(runtime::MOD_ID) {
        return;
    }
    match packet {
        Clientbound::CustomPayload(payload) if payload.channel == "openpartiesandclaims:main" => {
            state().opac.update(&payload.data);
        }
        Clientbound::CustomPayload(payload) if payload.channel.starts_with("waystones:") => {
            super::minimap_providers::update_waystones(
                &mut WAYSTONES.lock().unwrap_or_else(|error| error.into_inner()),
                &payload.channel, &payload.data,
            );
        }
        Clientbound::Login(login) => {
            observe_world_packet(&login.spawn);
            WAYSTONES.lock().unwrap_or_else(|error| error.into_inner()).clear();
            LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner()).clear();
            DEATH_CAPTURE.lock().unwrap_or_else(|error| error.into_inner())
                .start(protocol_death(login.spawn.last_death.as_ref()));
        }
        Clientbound::Respawn(respawn) => {
            observe_world_packet(&respawn.spawn);
            LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner()).clear();
            let Some(world) = game.map_client_state().as_ref().and_then(waypoint_identity) else { return; };
            // A dimension change retains entity data. An actual respawn can
            // include the server's block of death, even in a different dimension.
            let location = DEATH_CAPTURE.lock().unwrap_or_else(|error| error.into_inner())
                .respawn(&world, protocol_death(respawn.spawn.last_death.as_ref()),
                    respawn.keep & 2 == 0, game.screen == super::Screen::Dead);
            if let Some(location) = location { record_death(game, &world, location); }
        }
        Clientbound::TrackedWaypoint(waypoint) => {
            apply_locator_waypoint(&mut LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner()),waypoint);
        }
        // The chunk is handled by the game after this hook, so sampling waits
        // for the next frame instead of reading a chunk that is still absent.
        Clientbound::LevelChunkWithLight(chunk) => {
            state().biome_chunks.insert((chunk.x, chunk.z));
        }
        Clientbound::PlayerCombatKill(death)
            if game.remote.as_ref().is_some_and(|remote| remote.entity_id == death.id) =>
        {
            let Some(player) = game.native_player() else { return; };
            let Some(world) = game.map_client_state().as_ref().and_then(waypoint_identity) else { return; };
            let dimension = Dimension::from_id(player.dimension);
            if dimension != Dimension::Player {
                let fallback = [player.x, player.y, player.z].map(|value| value.floor() as i32);
                DEATH_CAPTURE.lock().unwrap_or_else(|error| error.into_inner())
                    .combat(world, (dimension, fallback));
            }
        }
        _ => {}
    }
}

fn observe_world_packet(spawn: &horizon_protocol::packets::play::SpawnInfo) {
    let signal = runtime::ServerWorldSignal::new(&spawn.dimension, spawn.seed);
    let mut state = state();
    if state.server_world_signal != signal {
        state.server_world_signal = signal;
        state.server_world_signal_pending = true;
    }
}

fn cave_allowed(game:&Game<'_>,large:bool,dimension:Dimension)->bool {
    if large && (game.map_option("world_cave_enabled") == Some(0) || game.map_option("world_cave_type") == Some(0)) {
        return false;
    }
    let key=match (large,dimension) {
        (false,Dimension::Overworld)=>"minimap_cave_surface",
        (false,Dimension::Nether)=>"minimap_cave_nether",
        (false,Dimension::End)=>"minimap_cave_end",
        (true,Dimension::Overworld)=>"world_cave_surface",
        (true,Dimension::Nether)=>"world_cave_nether",
        (true,Dimension::End)=>"world_cave_end",
        (_,Dimension::Player)=>return true,
    };
    game.map_option(key)!=Some(0)
}

fn terrain_appearance(game: &Game<'_>, large: bool) -> maps::Appearance {
    use maps::Appearance as A;
    let mut appearance = 0;
    let option = |mini, world| game.map_option(if large { world } else { mini });
    if option("minimap_block_colors", "world_block_colors") == Some(1) {
        appearance |= A::VANILLA;
        if option("minimap_vanilla_biomes", "world_vanilla_biomes") == Some(1) {
            appearance |= A::VANILLA_BIOMES;
        }
    }
    if (!A(appearance).has(A::VANILLA) || A(appearance).has(A::VANILLA_BIOMES))
        && option("minimap_biome_blending", "world_biome_blending") == Some(0)
    {
        appearance |= A::HARD_BIOME_EDGES;
    }
    if !large && game.map_option("minimap_block_transparency") == Some(0) {
        appearance |= A::OPAQUE;
    }
    if option("minimap_flowers", "world_flowers") == Some(0) {
        appearance |= A::HIDE_FLOWERS;
    }
    if option("minimap_stained_glass", "world_stained_glass") == Some(0) {
        appearance |= A::HIDE_STAINED_GLASS;
    }
    if !large && game.map_option("minimap_redstone") == Some(0) {
        appearance |= A::HIDE_REDSTONE;
    }
    if option("minimap_short_block_height", "world_short_block_height") == Some(1) {
        appearance |= A::SHORT_HEIGHT;
    }
    if option("minimap_lighting", "world_lighting") != Some(0) {
        appearance |= A::LIGHTING;
    }
    A(appearance)
}

pub(super) fn frame(game: &mut Game<'_>, elapsed: Duration) {
    let _timing = runtime::profiling::measure("client-frame");
    if !game.native_window() {
        return;
    }
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    if !game.map_on(runtime::MOD_ID) {
        if game.mod_screen() == Some(runtime::MOD_ID) {
            game.mod_screen_close(runtime::MOD_ID);
        }
        return;
    }
    let Some(player) = game.native_player() else {
        return;
    };
    let Some(identity) = game.map_client_state() else {
        return;
    };
    if !controller.settings_loaded() {
        return;
    }
    let base_world = base_world(&identity).unwrap_or_default();
    let (copy, chat, command, change, placement) = {
        let mut state = state();
        if base_world.starts_with("server:")
            && std::mem::take(&mut state.server_world_signal_pending)
            && let Some(signal) = &state.server_world_signal
            && controller.observe_server_world(&base_world, signal)
        {
            log::info!("[map-auto-world] Applied the server's world identity");
        }
        (state.pending_copy.take(), state.pending_chat.take(), state.pending_command.take(),
            state.map_settings_change.take(), state.placement_change.take())
    };
    if let Some((key, value)) = change {
        super::minimap_compat::write_map_setting(game, &key, &value);
    }
    if let Some(placement) = placement {
        use super::minimap_compat::write_map_setting;
        if let Some([x, y]) = placement {
            write_map_setting(game, "minimap_position_x", &(x * 10000.).round().to_string());
            write_map_setting(game, "minimap_position_y", &(y * 10000.).round().to_string());
        }
        write_map_setting(game, "minimap_position_custom", &placement.is_some().to_string());
    }
    if let Some(text) = copy {
        use horizon_client::clipboard::Clipboard;
        game.clipboard.set(&text);
    }
    if let Some((world, link)) = chat
        && waypoint_identity(&identity).as_deref() == Some(world.as_str())
    {
        game.mod_screen_close(runtime::MOD_ID);
        runtime::set_text_input(false);
        if let Err(error) = game.open_chat(&link) { game.native_chat(&error); }
    }
    if let Some(command) = command {
        game.click_command(&command);
    }
    let mut state = state();
    if let Some(import) = state.settings_import.as_ref().and_then(|rx| match rx.try_recv() {
        Ok(result) => Some(result),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err(std::io::Error::other("Settings import was interrupted"))),
        Err(std::sync::mpsc::TryRecvError::Empty) => None,
    }) {
        state.settings_import = None;
        let message = match import.and_then(|data| super::minimap_compat::apply_xaero_settings(game,data)) {
            Ok((applied,skipped,profile)) => {
                state.xaero_radar_enabled=None;
                state.xaero_profile=profile;

                if !skipped.is_empty() {
                    eprintln!("[{}] Unapplied Xaero settings: {}", runtime::MOD_ID, skipped.join(", "));
                }
                let mut summary=localized_format!(
                    "Settings imported {} · skipped {}", "Настроек перенесено {} · пропущено {}",
                    "{} réglages importés · {} ignorés", "{} ajustes importados · {} omitidos",
                    "{} Einstellungen übernommen · {} übersprungen", applied, skipped.len()
                );
                if let Some(name)=&state.xaero_profile {summary.push_str(&format!(" · {name}"));}
                summary
            }
            Err(error) => error.to_string(),
        };
        state.notice = Some((message, Instant::now()));
    }
    let sprites_version = game.block_sprites_version();
    if state.leaves.as_ref().map(|(version, _)| *version) != Some(sprites_version) {
        state.leaves = Some((sprites_version, Arc::new(build_leaf_palette(game))));
    }
    if state.close {
        state.close = false;
        runtime::set_text_input(false);
        game.mod_screen_close(runtime::MOD_ID);
    }
    if game.mod_screen() != Some(runtime::MOD_ID) {
        runtime::set_text_input(false);
        if state.mode == Mode::Name {
            state.mode = Mode::Map;
            state.name_form = None;
        }
    }
    if let Some(request) = runtime::take_screen_request() {
        let desired = match request {
            runtime::ScreenRequest::Map | runtime::ScreenRequest::Slime => Mode::Map,
            runtime::ScreenRequest::Waypoints => Mode::Waypoints,
            runtime::ScreenRequest::Create if state.large => Mode::Map,
            runtime::ScreenRequest::Create => Mode::Waypoints,
            runtime::ScreenRequest::Settings | runtime::ScreenRequest::SettingsOverlay => Mode::Settings,
        };
        // The key helper opens the screen; this side only chooses its mode.
        if game.mod_screen() == Some(runtime::MOD_ID) {
            if matches!(request,runtime::ScreenRequest::Settings|runtime::ScreenRequest::SettingsOverlay) {
                let back = if request == runtime::ScreenRequest::Settings {
                    None
                } else {
                    match state.mode {
                        Mode::Map => Some(Mode::Map),
                        Mode::Waypoints => Some(Mode::Waypoints),
                        Mode::Settings => state.settings_return,
                        _ => None,
                    }
                };
                let tab = back.map_or(0, |mode| if mode == Mode::Map {1} else {2});
                open_settings(&mut state,back,tab);
            } else {
                state.mode = desired;
            }
            state.map_panel = MapPanel::None;
            state.slime_preview = request == runtime::ScreenRequest::Slime;
            if state.slime_preview && state.slime_seed.is_none() {
                state.notice = Some((tr("Slime chunks require a known seed", "Для карты слизи нужен сид мира",
                    "Les chunks à slimes nécessitent la graine", "Los chunks de slime necesitan la semilla",
                    "Schleim-Chunks benötigen den Welt-Seed").into(), Instant::now()));
            }
            state.info = None;
            state.name_form = None;
            state.menu = None;
            state.waypoint_search_focus = false;
            state.server_world_focus = false;
            state.waypoint_groups_open = false;
            state.waypoint_group_move = false;
            state.waypoint_bulk = None;
            state.waypoint_confirm_delete = false;
            state.waypoint_selected = None;
            runtime::set_text_input(false);
            if request == runtime::ScreenRequest::Create {
                create_waypoint_here(game, &mut state);
            }
        }
    }
    state.portals = controller.maps.portals();
    let world = runtime::active_world(base_world.clone());
    if state.world != world {
        controller.clear_temporary_waypoints(&state.world);
        let same_server = state.base_world == base_world && base_world.starts_with("server:");
        let keep_panel = same_server && state.map_panel == MapPanel::ServerWorlds
            && game.mod_screen() == Some(runtime::MOD_ID);
        runtime::set_text_input(false);
        let next = state.session + 1;
        if let Err(error) = ::trailgrid_maps::radar::load_rules(&game.native_data_dir(runtime::MOD_ID), false) {
            eprintln!("[{}] Radar rules: {error}", runtime::MOD_ID);
        }
        let slime_seed = if game.integrated.is_some() {
            Some(game.level.seed)
        } else {
            use std::io::Read;
            let mut text = String::new();
            let path = game.native_data_dir(runtime::MOD_ID).join("slime-seeds.ini");
            std::fs::File::open(path).ok().and_then(|file| {
                file.take(64 * 1024).read_to_string(&mut text).ok()?;
                runtime::slime_seed(&text, &world)
            })
        };
        *state = State {
            waypoint_group: controller.active_waypoint_group(&world),
            world,
            base_world,
            server_world_signal: state.server_world_signal.take(),
            server_world_signal_pending: state.server_world_signal_pending,
            session: next,
            map_panel: if keep_panel { MapPanel::ServerWorlds } else { MapPanel::None },
            map_panel_scroll: if keep_panel { state.map_panel_scroll } else { 0 },
            opac: if same_server { std::mem::take(&mut state.opac) } else { Default::default() },
            slime_seed,
            xaero_profile: ::trailgrid_maps::xaero_settings::active_profile(&game.native_data_dir(runtime::MOD_ID)),
            pending_command: (official_smp(game)
                && (game.map_option("minimap_claims") != Some(0)
                    || game.map_option("world_claims") != Some(0)
                    || game.map_option("minimap_current_claim") != Some(0))).then(|| "claims".to_owned()),
            ..State::default()
        };
        controller.maps.reset(next);
    }
    use ::trailgrid_maps::map_claims;
    let claim_replies = map_claims::take_replies();
    if official_smp(game) {
        for reply in claim_replies {
            match reply {
                map_claims::Reply::Own(own) => {
                    log::info!("[{}] Synced {} owned chunks from Horizon SMP", runtime::MOD_ID, own.len());
                    state.claims.retain(|_, owner| owner.is_some());
                    for chunk in own { state.claims.insert(chunk, None); }
                }
                map_claims::Reply::Here(chunk, Some(owner)) => {
                    if state.claims.len() < 8192 { state.claims.entry(chunk).or_insert(Some(owner)); }
                }
                map_claims::Reply::Here(chunk, None) => { state.claims.remove(&chunk); }
            }
        }
    }
    let enabled = game.map_option("use_xaero_radar") == Some(1);
    if state.xaero_radar_enabled != Some(enabled) {
        let categories = enabled.then(|| {
            let directory = game.native_data_dir(runtime::MOD_ID);
            let source = super::minimap_radar_editor::source(&directory, state.xaero_profile.as_deref())
                .ok().flatten()?;
            super::minimap_compat::xaero_radar_categories(&source, &mut Vec::new())
        }).flatten();
        use ::trailgrid_maps::xaero_settings;
        let directory=game.native_data_dir(runtime::MOD_ID);
        let item_config=xaero_settings::commands_directory(&directory,state.xaero_profile.as_deref());
        let values=xaero_settings::teleport_commands(&item_config).unwrap_or_default();
        state.map_items=["minimap_item","map_item"].map(|key|values.get(key)
            .filter(|item|item.as_str()!="-").cloned());
        ::trailgrid_maps::radar::use_imported(categories);
        let info_directory = xaero_settings::profile_directory(&directory, state.xaero_profile.as_deref());
        state.info_displays = xaero_settings::read_info_display(&info_directory).ok().flatten();
        state.xaero_radar_enabled = Some(enabled);
    }
    if let Some(point) = runtime::take_shared_waypoint()
        && point.world == state.world
    {
        let message = match controller.merge_waypoints(&state.world,vec![point]) {
            Ok(_) => tr("Waypoint imported", "Метка импортирована", "Repère importé", "Marcador importado", "Wegpunkt importiert").to_owned(),
            Err(error) => error,
        };
        state.notice = Some((message,Instant::now()));
    }
    update_cache_ui(game, &mut state);
    poll_map_export(&mut state);
    poll_waypoint_import(&mut state);
    poll_cache_import(&mut state);
    state.waypoints = controller.waypoints(&state.world);
    let dimension = Dimension::from_id(player.dimension);
    controller.maps.configure(
        state.session,
        &state.world,
        dimension,
        controller.settings().cache_ram_mib,
    );
    if state.dimension != dimension {
        state.lod_sources.clear();
        state.frame = None;
        state.portals.clear();

        state.seen.clear();
        state.chunk_keys.clear();
        state.chunk_revision = 0;
        state.chunk_cursor = None;
        state.cave_pending=[None;2];
        state.cave_open=[false;2];
        state.dimension = dimension;
    }
    let head = (player.y + 1.62).floor() as i32;
    let settings = controller.settings();
    use runtime::minimap::CaveView;
    let screen_open=game.mod_screen()==Some(runtime::MOD_ID);
    let large=screen_open && (state.mode==Mode::Map ||
        state.mode==Mode::Settings && state.placement.is_none() && state.settings_return==Some(Mode::Map));
    let appearance = terrain_appearance(game, large);
    if state.appearance != appearance {
        state.seen.clear();
        state.appearance = appearance;
    }
    let index=usize::from(large);
    let override_mode=if large {game.map_number("world_auto_cave").unwrap_or(-1.) as i32}else{-1};
    let cave_view=if override_mode==0 {CaveView::Surface}else if override_mode>0 {CaveView::Auto}else{settings.cave_view};
    let roof=if override_mode>0 {override_mode} else {i32::from(game.map_option("minimap_cave_roof").unwrap_or(1))};
    let roof_radius=roof.clamp(1,3)-1;
    let covered=cave_view==CaveView::Auto && (dimension==Dimension::Nether ||
        (-roof_radius..=roof_radius).all(|dx|(-roof_radius..=roof_radius).all(|dz| {
            game.map_light_if_loaded([player.x.floor() as i32+dx,head,player.z.floor() as i32+dz])
                .is_some_and(|(_,sky)|sky==0)
        })));
    if cave_view==CaveView::Auto && state.cave_open[index]!=covered {
        if state.cave_pending[index].is_none_or(|(expected,_)|expected!=covered) {
            state.cave_pending[index]=Some((covered,Instant::now()));
        }
        let ms=game.map_number(if large {"world_cave_timer"}else{"minimap_cave_timer"}).unwrap_or(0.) as u64;
        if state.cave_pending[index].is_some_and(|(_,since)|since.elapsed()>=Duration::from_millis(ms)) {
            state.cave_open[index]=covered;
            state.cave_pending[index]=None;
        }
    } else {
        state.cave_pending[index]=None;
    }
    let layer=if cave_allowed(game,large,dimension) {
        match cave_view {
            CaveView::Underground=>Layer::Below(head),
            CaveView::Manual=>Layer::Below(settings.cave_y),
            CaveView::Auto if state.cave_open[index]=>Layer::Below(head),
            _=>Layer::Surface,
        }
    } else {Layer::Surface};
    let layer = if large && game.map_option("world_cave_type") == Some(2) && matches!(layer, Layer::Below(_)) {
        Layer::Below(maps::FULL_CAVE)
    } else {
        layer
    };
    state.layers.insert(dimension,layer);
    if settings.footsteps {
        let point = (dimension, [player.x.floor() as i32, player.z.floor() as i32]);
        if state.footsteps.back() != Some(&point) {
            state.footsteps.push_back(point);
            if state.footsteps.len() > 512 {
                state.footsteps.pop_front();
            }
        }
    } else {
        state.footsteps.clear();
    }
    if dimension != Dimension::Player && game.screen != super::Screen::Dead {
        let at = [player.x,player.y,player.z];
        let mut reached = Vec::new();
        let mut previous = std::mem::take(&mut state.destinations);
        let mut destinations = BTreeMap::new();
        let mut was_outside = std::mem::take(&mut state.deaths_outside_reach);
        let mut outside_reach = BTreeSet::new();
        for point in state.waypoints.iter().filter(|point| {
            point.options.destination || (settings.death_waypoint_auto_remove && point.icon==runtime::WaypointIcon::Skull)
        }) {
            let key = point.key();
            let mut arrived = false;
            if point.options.destination {
                let start = previous.remove(&key).unwrap_or_else(Instant::now);
                arrived = point.dimension==dimension && start.elapsed()>=Duration::from_secs(5) && point.within_reach(at);
                if !arrived {destinations.insert(key,start);}
            }
            let mut returned = false;
            if settings.death_waypoint_auto_remove && point.icon == runtime::WaypointIcon::Skull {
                if point.dimension != dimension || !runtime::death_waypoint_reached(at, point) {
                    outside_reach.insert(key);
                } else {
                    returned = was_outside.remove(&key);
                }
            }
            if arrived || returned { reached.push(key); }
        }
        state.destinations = destinations;
        state.deaths_outside_reach = outside_reach;
        if !reached.is_empty() {
            for key in reached {
                controller.delete_waypoint(&state.world, key);
            }
            state.waypoints = controller.waypoints(&state.world);
        }
    }
    let selected = if large {
        settings.world_dimension
    } else {
        settings.mini_dimension
    }
    .resolve(dimension);
    let position = dimension.convert(selected, [player.x, player.z]);
    if selected != state.selected || large != state.large {
        if large!=state.large {state.map_opened=large.then(Instant::now);}
        state.camera.snap(position);
        state.marker = position;
        state.selected = selected;
        state.large = large;
        state.dragging = false;
        state.menu = None;
    }
    if !large {
        state.camera.follow(position);
    }
    apply_map_focus(&mut state, large, selected);
    let dt = elapsed.as_secs_f64();
    state.camera.advance(dt);
    state.marker = maps::follow_point(state.marker, position, dt);
    state.lod_elapsed += dt;
    if state.lod_elapsed >= 0.05 {
        state.lod_elapsed = 0.0;
        if let Some(lod) = game.lod.as_ref() {
            let _timing = runtime::profiling::measure("lod-snapshot-pick");
            state
                .lod_sources
                .retain(|_, (source, _)| source.strong_count() != 0);
            let known = known_lod_sources(&state, dimension);
            let span = controller
                .maps
                .requested()
                .filter(|v| v.dimension == dimension)
                .map_or([256.; 2], |v| v.span);
            let focus = if selected == dimension {
                state.camera.target
            } else {
                [player.x, player.z]
            };
            for (key, columns) in super::minimap_lod_adapter::snapshots(lod, &known, focus, span, 8)
            {
                let source = Arc::downgrade(&columns);
                let biome_palette = game
                    .world_biomes
                    .as_ref()
                    .unwrap_or_else(|| game.res.biomes())
                    .clone();
                let leaves = state.leaves.as_ref().map(|(_, leaves)| Arc::clone(leaves));
                let resources = Arc::clone(&game.res);
                let key = maps::LodKey {
                    x: key.x,
                    z: key.z,
                    level: key.level,
                };
                let accepted =
                    controller
                        .maps
                        .submit_lod(state.session, dimension, key, move || {
                            let mut biome_names = BTreeMap::<u16, Arc<str>>::new();
                            let book = resources.facts();
                            let [origin_x, origin_z, size, _] = key.bounds();
                            let stride = size / 16;
                            let biome_at = |wx: i32, wz: i32| -> Option<u16> {
                                let dx = i64::from(wx) - origin_x;
                                let dz = i64::from(wz) - origin_z;
                                if dx < 0 || dz < 0 || dx >= size || dz >= size {
                                    return None;
                                }
                                let cell = ((dz / stride) * 16 + (dx / stride)) as usize;
                                columns.get(cell)?.surface.map(|surface| surface.biome)
                            };
                            let color = |id: horizon_world::registry::BlockStateId,
                                         at: [i32; 2]| {
                                let state = book.states.get(id.0 as usize)?;
                                if book.is_air(id) {
                                    return None;
                                }
                                let base = plant_color(state.name.as_ref()).unwrap_or_else(|| {
                                    horizon_client::blocks::MAP_COLORS
                                        .get(state.map_color as usize)
                                        .copied()
                                        .unwrap_or(0)
                                });
                                let leaf = leaves
                                    .as_ref()
                                    .and_then(|leaves| leaves.states.get(&id.0))
                                    .copied();
                                let water =
                                    matches!(state.fluid, horizon_client::blocks::Fluid::Water);
                                let needs_grass = leaf
                                    .is_some_and(|leaf| leaf.tint == LeafTint::Grass)
                                    || maps::needs_grass_tint(state.name.as_ref());
                                let needs_foliage = leaf
                                    .is_some_and(|leaf| leaf.tint == LeafTint::Foliage)
                                    || maps::needs_foliage_tint(state.name.as_ref());
                                let blend = blend_biome_tints(
                                    &biome_palette,
                                    at,
                                    BIOME_BLEND_RADIUS,
                                    BIOME_BLEND_STEP,
                                    needs_grass,
                                    needs_foliage,
                                    water,
                                    |wx, wz| biome_at(wx, wz),
                                );
                                let rgb = if water {
                                    blend
                                        .water
                                        .unwrap_or_else(|| if base == 0 { 0x3f76e4 } else { base })
                                } else {
                                    leaf.and_then(|leaf| leaf.map_color(blend.grass, blend.foliage))
                                        .unwrap_or_else(|| {
                                            plant_color(state.name.as_ref()).unwrap_or_else(|| {
                                                maps::block_map_color(
                                                    state.name.as_ref(),
                                                    base,
                                                    blend.grass,
                                                    blend.foliage,
                                                )
                                            })
                                        })
                                };
                                Some(rgb)
                            };
                            let mut tile = maps::Tile {
                                pixels: vec![0; 1024],
                                biomes: vec![None; 256],
                                ..maps::Tile::default()
                            };
                            for (i, column) in columns.iter().enumerate() {
                                let Some(surface) = column.surface else {
                                    continue;
                                };
                                let at = [
                                    (origin_x + (i as i64 % 16) * stride + stride / 2) as i32,
                                    (origin_z + (i as i64 / 16) * stride + stride / 2) as i32,
                                ];
                                let height = surface
                                    .water
                                    .map_or(surface.height, |(h, _)| h)
                                    .saturating_sub(1);
                                let snows = dimension == Dimension::Overworld
                                    && biome_palette.list.get(surface.biome as usize).is_some_and(
                                        |b| b.snows_at(at[0], height, at[1]) == Some(true),
                                    );
                                let snow = leaves.as_ref().map_or(0xffffff, |p| p.snow);
                                let ice = leaves.as_ref().map_or(0xa0a0ff, |p| p.ice);
                                let base = color(surface.state, at);
                                let rgb = if let Some((height, fluid)) = surface.water {
                                    let Some(fluid_color) = color(fluid, at) else {
                                        continue;
                                    };
                                    if book.states.get(fluid.0 as usize).is_some_and(|s| {
                                        matches!(s.fluid, horizon_client::blocks::Fluid::Water)
                                    }) {
                                        maps::lod_water_color(
                                            base,
                                            fluid_color,
                                            height.saturating_sub(surface.height).max(0) as u32,
                                            snows,
                                            ice,
                                        )
                                    } else {
                                        fluid_color
                                    }
                                } else {
                                    let Some(base) = base else {
                                        continue;
                                    };
                                    let name = book
                                        .states
                                        .get(surface.state.0 as usize)
                                        .map_or("", |s| s.name.as_ref());
                                    maps::lod_snow_color(name, base, snow, snows)
                                };
                                if let Some(info) = biome_palette.list.get(surface.biome as usize) {
                                    let name = biome_names
                                        .entry(surface.biome)
                                        .or_insert_with(|| Arc::from(info.name.as_str()));
                                    tile.biomes[i] = Some(name.clone());
                                }
                                tile.heights[i] = surface
                                    .water
                                    .map(|(height, _)| height)
                                    .unwrap_or(surface.height)
                                    .saturating_sub(1);
                                tile.pixels[i * 4..i * 4 + 4].copy_from_slice(&[
                                    (rgb >> 16) as u8,
                                    (rgb >> 8) as u8,
                                    rgb as u8,
                                    255,
                                ]);
                            }
                            tile
                        });
                if accepted {
                    state
                        .lod_sources
                        .insert((dimension, key), (source, Instant::now()));
                }
            }
        }
    }
    state.elapsed += dt;
    if state.elapsed >= 0.05 {
        state.elapsed = 0.0;
        // Persist each newly sampled biome with its waypoint. A known biome is
        // only refreshed when its chunk arrives; an unknown one is retried
        // until the chunk is loaded. Only a change reaches the settings
        // worker, so an unchanged sample writes nothing.
        let arrived = std::mem::take(&mut state.biome_chunks);
        for index in 0..state.waypoints.len() {
            let (world, dimension, x, y, z, current) = {
                let waypoint = &state.waypoints[index];
                (
                    waypoint.world.clone(),
                    waypoint.dimension,
                    waypoint.x,
                    waypoint.y,
                    waypoint.z,
                    waypoint.biome.clone(),
                )
            };
            if current.is_some() && !arrived.contains(&(x.div_euclid(16), z.div_euclid(16))) {
                continue;
            }
            let Some(biome) = known_biome(game, dimension, [x, y, z]) else {
                continue;
            };
            if current.as_deref() == Some(biome.as_str()) {
                continue;
            }
            state.waypoints[index].biome = Some(biome.clone());
            controller.change_waypoint(&world, state.waypoints[index].key(), |point| point.biome=Some(biome));
        }
        let cx = (player.x.floor() as i32).div_euclid(16);
        let cz = (player.z.floor() as i32).div_euclid(16);
        let _timing = runtime::profiling::measure("chunk-collection");
        let revision = game.world.chunks.membership_revision();
        if state.chunk_revision != revision {
            if let Some(deltas) = game.world.chunks.deltas_since(state.chunk_revision) {
                use horizon_client::chunk_map::ChunkMembershipDelta;
                for (_, delta) in deltas {
                    match delta {
                        ChunkMembershipDelta::Added(pos) => {
                            state.chunk_keys.insert(pos);
                        }
                        ChunkMembershipDelta::Removed(pos) => {
                            state.chunk_keys.remove(&pos);
                            state.seen.remove(&pos);
                        }
                        ChunkMembershipDelta::Reset => {
                            unreachable!("reset requires membership rebuild")
                        }
                    }
                }
            } else {
                let keys: BTreeSet<_> = game.world.chunks.keys().copied().collect();
                state.seen.retain(|pos, _| keys.contains(pos));
                state.chunk_keys = keys;
            }
            state.chunk_revision = revision;
        }
        let requested = controller.maps.requested();
        let anchor = requested
            .as_ref()
            .filter(|v| v.dimension == dimension)
            .map_or([cx, cz], |v| {
                v.center.map(|v| (v.floor() as i32).div_euclid(16))
            });
        let mut positions: BTreeSet<_> = arrived.iter().copied().take(64).collect();
        for z in -4..=4 {
            for x in -4..=4 {
                positions.insert((anchor[0].saturating_add(x), anchor[1].saturating_add(z)));
            }
        }
        use std::ops::Bound::{Excluded, Unbounded};
        let scan: Vec<_> = state
            .chunk_keys
            .range((state.chunk_cursor.map_or(Unbounded, Excluded), Unbounded))
            .take(64)
            .copied()
            .collect();
        state.chunk_cursor = if scan.len() == 64 {
            scan.last().copied()
        } else {
            None
        };
        positions.extend(scan);
        let mut chunks: Vec<_> = positions
            .into_iter()
            .filter_map(|pos| {
                game.world
                    .chunks
                    .get(&pos)
                    .map(|chunk| (pos, Arc::downgrade(chunk)))
            })
            .collect();
        chunks.sort_by_key(|(pos, _)| {
            (
                !requested
                    .as_ref()
                    .is_some_and(|v| v.demands(dimension, layer, [pos.0, pos.1])),
                i64::from(pos.0 - cx).pow(2) + i64::from(pos.1 - cz).pow(2),
            )
        });
        let mut sent = 0;
        let mut attempted = 0;
        let collection_start = Instant::now();
        let server=game.remote.is_some() && game.integrated.is_none();
        let write_radius=game.map_number("map_write_radius").unwrap_or(-1.) as i32;
        let load=game.map_option("map_load_chunks")!=Some(0);
        let update=game.map_option("map_update_chunks")!=Some(0);
        let limited=server && write_radius>=0;
        let coverage=(!load || !update).then(||controller.maps.coverage());
        let budget=if game.map_option("map_fast_writes")==Some(1) {1400}else{800};
        let chunk_budget=if budget>800 {8}else{4};
        for (pos, weak) in chunks {
            if limited && ((pos.0-cx).abs()>write_radius || (pos.1-cz).abs()>write_radius) {
                continue;
            }
            if let Some(coverage)=coverage.as_ref() {
                let known=coverage.detailed_at(dimension,layer,[pos.0*16,pos.1*16]);
                if (known && !update) || (!known && !load) {continue;}
            }
            if attempted > 0 && collection_start.elapsed() >= Duration::from_micros(budget) {
                break;
            }
            let visible = requested
                .as_ref()
                .is_some_and(|v| v.demands(dimension, layer, [pos.0, pos.1]));
            if state.seen.get(&pos).is_some_and(|(old, old_layer, sent)| {
                Weak::ptr_eq(old, &weak)
                    && *old_layer == layer
                    && (!visible
                        || sent.elapsed() < Duration::from_millis(500)
                        || controller.maps.contains(dimension, layer, [pos.0, pos.1]))
            }) {
                continue;
            }
            attempted += 1;
            let snapshot = {
                let _sampling = runtime::profiling::measure("snapshot-collection");
                game.map_chunks([pos.0 - 1, pos.1 - 1], [pos.0 + 1, pos.1 + 1])
            };
            let Ok(snapshot) = snapshot else {
                continue;
            };
            let resources = Arc::clone(&game.res);
            let biome_palette = game
                .world_biomes
                .as_ref()
                .unwrap_or_else(|| game.res.biomes())
                .clone();
            let leaves = state.leaves.as_ref().map(|(_, leaves)| Arc::clone(leaves));
            let accepted = controller.maps.submit_with_appearance(
                state.session,
                dimension,
                layer,
                [pos.0, pos.1],
                move || {
                    let book = resources.facts();
                    let minimum = snapshot.height().start;
                    use maps::Appearance as A;
                    let sample = |style: A| {
                        let mut names = BTreeMap::<u16, Arc<str>>::new();
                        maps::sample_with_biomes(
                            |x, y, z| {
                                let id = snapshot.block([x, y, z]);
                                let air = book.is_air(id);
                                let Some(state) = book.states.get(id.0 as usize) else {
                                    return maps::Block {
                                        rgb: 0,
                                        air,
                                        alpha: 0,
                                        hidden: false,
                                        water: false,
                                        portal: None,
                                    };
                                };
                                let name = state.name.as_ref();
                                let class = |name| state.classes.iter().any(|class| class.as_ref() == name);
                                let stained_glass = name.contains("stained_glass");
                                let redstone = matches!(name, "minecraft:redstone_wire" | "minecraft:redstone_torch" | "minecraft:redstone_wall_torch" | "minecraft:repeater" | "minecraft:comparator");
                                let flower = class("FlowerBlock") || class("TallFlowerBlock")
                                    || matches!(name, "minecraft:torchflower" | "minecraft:pitcher_plant" | "minecraft:pink_petals" | "minecraft:wildflowers" | "minecraft:flowerbed");
                                let hidden = (hidden_cover(state.classes.as_ref(), name)
                                    && !flower)
                                    || (style.has(A::HIDE_FLOWERS) && flower)
                                    || (style.has(A::HIDE_REDSTONE) && redstone)
                                    || (style.has(A::HIDE_STAINED_GLASS) && stained_glass);
                                let portal = match state.name.as_ref() {
                                    "minecraft:nether_portal" => Some(Portal::Nether),
                                    "minecraft:end_portal" => Some(Portal::End),
                                    "minecraft:end_gateway" => Some(Portal::Gateway),
                                    _ => None,
                                };
                                if air || hidden {
                                    return maps::Block {
                                        rgb: 0,
                                        air,
                                        alpha: 0,
                                        hidden,
                                        water: false,
                                        portal: None,
                                    };
                                }
                                if let Some(kind) = portal {
                                    let (rgb, alpha) = portal_tint(kind);
                                    return maps::Block {
                                        rgb,
                                        air,
                                        alpha,
                                        hidden: false,
                                        water: false,
                                        portal: Some(kind),
                                    };
                                }
                                let water = matches!(state.fluid, horizon_client::blocks::Fluid::Water);
                                let fluid = !matches!(state.fluid, horizon_client::blocks::Fluid::None);
                                let invisible = matches!(
                                    state.render_shape,
                                    horizon_client::blocks::RenderShape::Invisible
                                );
                                if invisible && !fluid {
                                    // Barriers, light blocks, moving pistons and
                                    // other render-invisible blocks leave no pixel.
                                    return maps::Block {
                                        rgb: 0,
                                        air,
                                        alpha: 0,
                                        hidden: true,
                                        water: false,
                                        portal: None,
                                    };
                                }
                                let base = horizon_client::blocks::MAP_COLORS
                                    .get(state.map_color as usize)
                                    .copied()
                                    .unwrap_or(0);
                                let leaf = leaves
                                    .as_ref()
                                    .and_then(|leaves| leaves.states.get(&id.0))
                                    .copied();
                                let needs_grass = leaf.is_some_and(|leaf| leaf.tint == LeafTint::Grass)
                                    || maps::needs_grass_tint(state.name.as_ref());
                                let needs_foliage = leaf
                                    .is_some_and(|leaf| leaf.tint == LeafTint::Foliage)
                                    || maps::needs_foliage_tint(state.name.as_ref());
                                let tint_biomes = !style.has(A::VANILLA) || style.has(A::VANILLA_BIOMES);
                                let blend = blend_biome_tints(
                                    &biome_palette,
                                    [x, z],
                                    if style.has(A::HARD_BIOME_EDGES) { 0 } else { BIOME_BLEND_RADIUS },
                                    BIOME_BLEND_STEP,
                                    tint_biomes && needs_grass,
                                    tint_biomes && needs_foliage,
                                    tint_biomes && water,
                                    |wx, wz| snapshot.biome([wx, y, wz]).map(|biome| biome.0),
                                );
                                let rgb = if redstone && base == 0 && !style.has(A::VANILLA) {
                                    0xad160a
                                } else if water {
                                    blend
                                        .water
                                        .unwrap_or_else(|| if base == 0 { 0x3f76e4 } else { base })
                                } else if style.has(A::VANILLA) {
                                    blend.grass.or(blend.foliage).unwrap_or(base)
                                } else {
                                    leaf.and_then(|leaf| leaf.map_color(blend.grass, blend.foliage))
                                        .unwrap_or_else(|| {
                                            plant_color(state.name.as_ref()).unwrap_or_else(|| {
                                                maps::block_map_color(
                                                    state.name.as_ref(),
                                                    base,
                                                    blend.grass,
                                                    blend.foliage,
                                                )
                                            })
                                        })
                                };
                                let glass = state.classes.iter().any(|class| {
                                    class.as_ref() == "TransparentBlock"
                                        || class.as_ref() == "HalfTransparentBlock"
                                }) || state.name.contains("glass");
                                let soft = matches!(
                                    state.name.as_ref(),
                                    "minecraft:scaffolding" | "minecraft:cobweb"
                                );
                                let (rgb, mut alpha) = if water {
                                    (if rgb == 0 { 0x3f76e4 } else { rgb }, 120)
                                } else if glass {
                                    let tint = if rgb == 0 {
                                        dye_color(state.name.as_ref()).unwrap_or(0xbfd8e6)
                                    } else {
                                        rgb
                                    };
                                    let alpha = match state.name.as_ref() {
                                        "minecraft:tinted_glass" => 190,
                                        "minecraft:ice"
                                        | "minecraft:blue_ice"
                                        | "minecraft:frosted_ice" => 130,
                                        _ if state.name.contains("glass_pane") => 80,
                                        _ => 96,
                                    };
                                    (tint, alpha)
                                } else if soft {
                                    (if rgb == 0 { 0xbfb8a8 } else { rgb }, 90)
                                } else {
                                    (rgb, 0)
                                };
                                if style.has(A::OPAQUE) {
                                    alpha = 0;
                                }
                                maps::Block {
                                    rgb,
                                    air,
                                    alpha,
                                    hidden: false,
                                    water,
                                    portal: None,
                                }
                            },
                            |x, z| snapshot.section_top([x, z]),
                            [pos.0, pos.1],
                            minimum,
                            layer,
                            |at| {
                                let id = snapshot.biome(at)?.0;
                                let info = biome_palette.list.get(id as usize)?;
                                Some(
                                    names
                                        .entry(id)
                                        .or_insert_with(|| Arc::from(info.name.as_str()))
                                        .clone(),
                                )
                            },
                        )
                    };
                    let base = sample(A::default());
                    let alternate = (appearance != A::default()).then(|| {
                        let adjusted = appearance.0 & !(A::SHORT_HEIGHT | A::LIGHTING);
                        let mut tile = if adjusted == 0 { base.clone() } else { sample(A(adjusted)) };
                        for (i, height) in tile.heights.iter_mut().enumerate() {
                            if tile.pixels[i * 4 + 3] == 0 {
                                continue;
                            }
                            let x = pos.0 * 16 + i as i32 % 16;
                            let z = pos.1 * 16 + i as i32 / 16;
                            if appearance.has(A::LIGHTING) {
                                let light = snapshot.light([x, *height + 1, z]);
                                let level = (light & 15).max(light >> 4);
                                if level < 15 {
                                    let shade = 96 + u16::from(level) * 159 / 15;
                                    for channel in &mut tile.pixels[i * 4..i * 4 + 3] {
                                        *channel = (u16::from(*channel) * shade / 255) as u8;
                                    }
                                }
                            }
                            if appearance.has(A::SHORT_HEIGHT)
                                && let Some(block) = book.states.get(snapshot.block([x, *height, z]).0 as usize)
                                && !block.outline.is_empty()
                                && block.outline.iter().all(|bounds| bounds[4] <= 0.25)
                            {
                                *height -= 1;
                            }
                        }
                        (appearance, tile)
                    });
                    (base, alternate)
                },
            );
            if !accepted {
                break;
            }
            state.seen.insert(pos, (weak, layer, Instant::now()));
            sent += 1;
            if sent == chunk_budget {
                break;
            }
        }
    }
    let mut current_view = None;
    if let Some(renderer) = game.renderer.as_ref() {
        let viewport = HudBatch::new(
            [renderer.config.width, renderer.config.height],
            game.settings.gui_scale_inputs(),
        );
        let rect = rectangle(viewport.width, viewport.height);
        let (width, height, span) = if large {
            // The HUD draws in GUI pixels, but the texture is sampled at the
            // window's physical resolution. Keep this coordinate space for
            // overlays; terrain tiles are now rasterized directly by the GPU.
            let scale = viewport.scale.max(1.0);
            (
                ((rect[2] * scale).ceil() as usize).max(1),
                ((rect[3] * scale).ceil() as usize).max(1),
                [
                    f64::from(rect[2]) * state.camera.zoom,
                    f64::from(rect[3]) * state.camera.zoom,
                ],
            )
        } else {
            let size = fitted_minimap_size(game,settings,viewport.width,viewport.height)
                .unwrap_or(128.0);
            let sampling = if game.map_option("minimap_safe_mode") == Some(1) { 1. } else { 4. };
            let texture = (size * sampling).round().clamp(64.0, 1024.0) as usize;
            let cave = matches!(state.layers.get(&selected),Some(Layer::Below(_))) || settings.cave_view == CaveView::Manual;
            let mut zoom = f64::from(settings.blocks_per_pixel);
            if zoom >= 4. && game.map_option("minimap_further_zoomout") == Some(1) {
                zoom *= 2.;
            }
            if cave {
                zoom /= game.map_number("minimap_cave_zoom").unwrap_or(1.);
            }
            if quick_minimap() {
                let setting = game.map_number("quick_minimap_zoom").unwrap_or_default();
                if setting > 0. { zoom = 1. / setting; }
            }
            (
                texture,
                texture,
                [zoom * 128.0; 2],
            )
        };
        let selected_layer = if !cave_allowed(game,large,selected) || override_mode==0 {Layer::Surface}
            else if cave_view == CaveView::Manual {
                Layer::Below(if large && game.map_option("world_cave_type") == Some(2) { maps::FULL_CAVE } else { settings.cave_y })
            }
            else {state.layers.get(&selected).copied().unwrap_or(Layer::Surface)};
        let view = View {
            terrain_style: maps::TerrainStyle {
                appearance,
                live_chunks_only: !large && game.map_option("minimap_world_chunks") == Some(0),
                cpu_raster: !large && game.map_option("minimap_safe_mode") == Some(1),
                slopes:game.map_option(if large {"world_terrain_slopes"}else{"minimap_terrain_slopes"}).unwrap_or(2),
                depth:game.map_option(if large {"world_terrain_depth"}else{"minimap_terrain_depth"})!=Some(0),
                cave_depth:game.map_option(if large {"world_cave_depth"}else{"minimap_cave_depth"}).unwrap_or(0),
                legible_caves:game.map_option(if large {"world_legible_caves"}else{"minimap_legible_caves"})==Some(1),
            },
            session: state.session,
            dimension: selected,
            layer: selected_layer,
            center: state.camera.center,
            span,
            width,
            height,
            angle: if !large && (settings.rotate_with_player || quick_minimap() && game.map_option("minimap_unlocked_enlarged")==Some(1)) && selected == dimension {
                (f64::from(player.yaw) + 180.0).to_radians()
            } else {
                0.0
            },
            round: !large && settings.round,
            large,
        };
        let mut target = view.clone();
        target.center = state.camera.target;
        if large {
            let ratio = state.camera.zoom_target / state.camera.zoom;
            target.span = span.map(|size| size * ratio);
        }
        controller.maps.request_ahead(view.clone(), target);
        current_view = Some(view);
    }
    if let Some(image) = controller.maps.frame()
        && image.view.session == state.session
        && image.view.dimension == selected
        && image.view.large == large
    {
        if current_view
            .as_ref()
            .is_some_and(|view| view.layer != image.view.layer)
        {
            return;
        }
        // Camera movement is independent of worker sampling/upload latency.
        // Pointer queries and overlays use this exact same displayed view.
        let next = if let (Some(atlas), Some(view)) = (&image.atlas, current_view.clone()) {
            let view = complete_map_view(view, &image.view);
            Arc::new(maps::Frame {
                raster_source: None,
                serial: image.serial,
                view,
                pixels: Vec::new(),
                known: image.known,
                atlas: Some(atlas.clone()),
            })
        } else if let Some(view) = current_view.filter(|view| view.large) {
            Arc::new(maps::Frame {
                raster_source: Some(image.clone()),
                serial: image.serial,
                view: complete_map_view(view, &image.view),
                pixels: Vec::new(),
                known: image.known,
                atlas: None,
            })
        } else {
            image
        };
        let paged = next.atlas.as_ref().is_some_and(|a| !a.pages.is_empty());
        if paged {
            let blur = !large && game.map_option("minimap_antialias") == Some(1);
            let _timing = runtime::profiling::measure("page-upload");
            state.page_images = next
                .atlas
                .as_ref()
                .unwrap()
                .pages
                .iter()
                .map(|page| {
                    let image = state
                        .page_images
                        .iter()
                        .find(|(old, image)| Arc::ptr_eq(old, page) && image.flags.blur == blur)
                        .map(|(_, image)| image.clone())
                        .unwrap_or_else(|| {
                            runtime::profiling::record(
                                "texture-transfer",
                                Duration::ZERO,
                                1,
                                page.pixels.len() as u64,
                            );
                            super::minimap_pages::with_filter(
                                page.width as u32,
                                page.height as u32,
                                page.pixels.clone(),
                                blur,
                            )
                        });
                    (page.clone(), image)
                })
                .collect();
        } else {
            state.page_images.clear();
        }
        let previous = if next.atlas.is_some() {
            state.frame.as_ref().and_then(|f| f.atlas.clone())
        } else {
            None
        };
        maps::present_frame(&mut state.frame, next, |width, height, pixels| {
            if paged {
                true
            } else if let Some(previous) = &previous {
                upload_terrain_patches(game, previous, width, height, pixels)
            } else {
                game.mod_image(width, height, pixels)
            }
        });
    }
    refresh_waypoint_labels(&mut state, controller.settings().waypoint_letters);
}

static QUICK_MINIMAP: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
fn quick_minimap() -> bool {
    QUICK_MINIMAP.load(std::sync::atomic::Ordering::Relaxed) != 0
}

pub(super) fn quick_minimap_key(action: mod_api::KeyAction, playing: bool, toggle: bool) {
    use std::sync::atomic::Ordering::Relaxed;
    match action {
        mod_api::KeyAction::Press if playing => {
            let current = QUICK_MINIMAP.load(Relaxed);
            QUICK_MINIMAP.store(if toggle { if current == 2 {0} else {2} } else {1},Relaxed);
        }
        mod_api::KeyAction::Release => { let _ = QUICK_MINIMAP.compare_exchange(1,0,Relaxed,Relaxed); }
        _ => {}
    }
}

fn minimap_ui_scale(game: &Game<'_>) -> f32 {
    let scale = game.map_option("minimap_ui_scale").unwrap_or(1);
    // Xaero's first choice follows the current GUI scale.
    if scale <= 1 { 1. } else { (f32::from(scale) - 1.) * 0.25 + 0.5 }
}

fn fitted_minimap_size(game: &Game<'_>, mut settings: runtime::minimap::Settings, width: f32, height: f32) -> Option<f32> {
    if let Some(size) = game.map_option("minimap_exact_size").filter(|n| *n!=0) {
        settings.size_px = u16::from(size.max(16));
    }
    let regular = settings.fitted_size(width,height)?;
    if quick_minimap() { Some(regular.max((width.min(height) * 0.52).min(440.))) }
    else { Some(regular) }
}

fn missing_item<'a>(game:&Game<'_>,state:&'a State,large:bool)->Option<&'a str> {
    let required=state.map_items[usize::from(large)].as_deref()?;
    let slots=game.inventory.iter().take(9).chain(std::iter::once(&game.offhand)).chain(game.equipment.iter());
    (!slots.flatten().any(|stack| stack.item.as_ref()==required && stack.count>0)).then_some(required)
}

pub(super) fn toggle_slime_overlay() {
    let mut state = state();
    state.slime_overlay = !state.slime_overlay;
}

pub(super) fn check_map_item(game:&mut Game<'_>,large:bool)->bool {
    let state=state();
    if let Some(item)=missing_item(game,&state,large) {
        game.native_chat(&localized_format!(
            "Map needs {} in the hotbar","Для карты нужен {} на панели быстрого доступа",
            "La carte requiert {} dans la barre rapide","El mapa requiere {} en la barra rápida",
            "Für die Karte wird {} in der Schnellleiste benötigt",item
        ));
        return false;
    }
    true
}

type InfoLine = (String, Option<u32>, Option<u32>);
fn claim_text(game: &Game<'_>, state: &State, key: ::trailgrid_maps::map_claims::Chunk) -> Option<String> {
    let prefix = tr("Claim", "Приват", "Claim", "Parcela", "Claim");
    let own = tr("yours", "твой", "à toi", "tuya", "dein");
    if let Some(owner) = state.claims.get(&key) {
        return Some(format!("{prefix} · {}",owner.as_deref().unwrap_or(own)));
    }
    let (owner, name) = state.opac.owner(key)?;
    Some(format!("{prefix} · {}", if owner == game.own_uuid() { own }
        else { name.unwrap_or(tr("claimed", "занят", "protégé", "ocupada", "geschützt")) }))
}

fn minimap_lines(game: &Game<'_>, state: &State, settings: runtime::minimap::Settings) -> Vec<InfoLine> {
    let Some(player) = game.native_player() else { return Vec::new(); };
    let dimension = Dimension::from_id(player.dimension);
    let position = [player.x, player.y, player.z];
    let block = position.map(|value| value.floor() as i32);
    let chunk = (dimension, block[0].div_euclid(16), block[2].div_euclid(16));
    let claim = (game.map_option("minimap_current_claim") != Some(0)).then(|| claim_text(game,state,chunk)).flatten();
    let mut lines = Vec::new();
    let Some(config) = state.info_displays.as_ref() else {
        lines.extend(runtime::coordinate_lines(settings, dimension, position).into_iter().map(|text| (text,None,None)));
        lines.extend(claim.into_iter().map(|text| (text,None,None)));
        return lines;
    };
    for entry in config {
        let text = match entry.id.as_str() {
            "coords" => format!("X {}  Y {}  Z {}", block[0], block[1], block[2]),
            "overworld_coords" if dimension == Dimension::Nether => {
                format!("{} · X {}  Z {}", Dimension::Overworld.label(), block[0] * 8, block[2] * 8)
            }
            "chunk_coords" => format!("{} · X {}  Z {}",tr("Chunk","Чанк","Chunk","Chunk","Chunk"),chunk.1,chunk.2),
            "angles" => format!("{:.1}° / {:.1}°",player.yaw,player.pitch),
            "dimension" => dimension.label().to_owned(),
            "biome" => known_biome(game,dimension,block).unwrap_or_default(),
            "weather" if game.world.weather.thunder_level.1 > 0. => tr("Thunder", "Гроза", "Orage", "Tormenta", "Gewitter").into(),
            "weather" if game.world.weather.rain_level.1 > 0. => tr("Rain", "Дождь", "Pluie", "Lluvia", "Regen").into(),
            "light_level" => game.map_light_if_loaded(block).map_or_else(String::new,|(block,sky)| {
                format!("{} · {block}/{sky}",tr("Light","Свет","Lumière","Luz","Licht"))
            }),
            "time" => {
                let minute = ((game.world.day_time.rem_euclid(24_000) + 6000) * 60 / 1000) % 1440;
                format!("{:02}:{:02}", minute / 60, minute % 60)
            }
            "real_time" => {
                let minute = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default().as_secs() / 60 % 1440;
                format!("{:02}:{:02} UTC", minute / 60, minute % 60)
            }
            "light_overlay_indicator" if game.map_option("light_overlay") == Some(1) => tr("Light overlay", "Освещение", "Lumière", "Luz", "Licht").into(),
            "manual_cave_mode_indicator" if settings.cave_view == runtime::minimap::CaveView::Manual => format!("Y {}",settings.cave_y),
            "custom_sub_world" => state.waypoint_group.clone().unwrap_or_default(),
            "highlights" => claim.clone().unwrap_or_default(),
            _ => continue,
        };
        if !text.is_empty() {
            lines.push((text, entry.foreground, entry.background));
        }
    }
    if !config.iter().any(|entry| entry.id == "highlights") {
        lines.extend(claim.into_iter().map(|text| (text,None,None)));
    }
    lines
}

pub(super) fn hud(game: &Game<'_>, batch: &mut HudBatch, corners: &mut Corners) {
    if game.map_option("minimap_hud_layer").unwrap_or(1) == 1 {
        draw_hud(game, batch, corners);
    }
}

fn effect_height(game: &Game<'_>) -> f32 {
    let effects = &game.potions.effects.list;
    if effects.is_empty() || game.screen != super::Screen::Playing || game.pvp_effects_shown() {
        return 0.;
    }
    if effects.iter().any(|e| horizon_client::brewing::effect(e.id)
        .is_some_and(|d| d.category != horizon_client::brewing::Category::Beneficial))
    { 50. } else { 25. }
}

pub(super) fn hud_under(game: &Game<'_>, batch: &mut HudBatch) {
    if game.map_option("minimap_hud_layer") == Some(0) {
        draw_hud(game, batch, &mut Corners::new(batch.width,batch.height,effect_height(game)));
    }
}

fn draw_hud(game: &Game<'_>, batch: &mut HudBatch, corners: &mut Corners) {
    if (game.debug && game.map_option("minimap_hide_in_debug")==Some(1))
        || (game.screen!=super::Screen::Playing && game.map_option("minimap_hide_screens")==Some(1))
        || !game.native_window()
        || !game.map_on(runtime::MOD_ID)
        || game.native_player().is_none()
        || game.mod_screen().is_some()
    {
        return;
    }
    let settings = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).settings();
    if missing_item(game,&state(),false).is_some() {return;}
    if game.native_minimap_now().is_none() && settings.enabled {
        if let Some(size) = fitted_minimap_size(game,settings,batch.width,batch.height) {
            let corner = match settings.corner {
                runtime::minimap::Corner::TopLeft => Corner::TopLeft,
                runtime::minimap::Corner::TopRight => Corner::TopRight,
                runtime::minimap::Corner::BottomLeft => Corner::BottomLeft,
                runtime::minimap::Corner::BottomRight => Corner::BottomRight,
            };
            let custom = custom_minimap_position(game);
            if custom.is_some() || game.native_corner_shown(corner) {
                let state = state();
                let lines = minimap_lines(game, &state, settings);
                let scale = minimap_ui_scale(game);
                let line_height = 12. * scale;
                let footer = if lines.is_empty() { 0. } else { lines.len() as f32 * line_height + 2. };
                let width = lines.iter()
                    .map(|(line, _, _)| game.map_text_width(line) * scale + 4.)
                    .fold(size, f32::max);
                let (x, mut y) = if custom.is_none() {
                    corners.place(corner, width, size + footer)
                } else { (0., 0.) };
                let effects = game.map_option("minimap_effect_push").unwrap_or(1);
                if corner == Corner::TopRight && effects == 0 {
                    y -= effect_height(game);
                } else if corner == Corner::BottomRight && effects == 2 {
                    y -= effect_height(game);
                }
                let boss = game.map_option("minimap_boss_push").unwrap_or(1);
                if boss != 0 {
                    let bars = game.boss_overlay.bars(horizon_client::boss_overlay::now_millis()).len();
                    let bottom = horizon_client::hud_overlays::boss_bars_bottom(batch.height as i32,bars) as f32;
                    let overlaps = x < batch.width*0.5+92. && x+width > batch.width*0.5-92.;
                    if overlaps && bottom > 0. {
                        if boss == 2 { y += bottom; }
                        else if y < bottom { y = bottom + 2.; }
                    }
                }
                let map_x = if matches!(corner, Corner::TopRight | Corner::BottomRight) {
                    x + width - size
                } else {
                    x
                };
                let centered = quick_minimap() && game.map_option("quick_minimap_centered") == Some(1);
                let (map_x,y) = if centered {
                    ((batch.width-size)*0.5,(batch.height-size-footer)*0.5)
                } else if let Some(position) = custom {
                    let span = placement_span([batch.width, batch.height], size, footer);
                    (position[0] * span[0], position[1] * span[1])
                } else { (map_x,y) };
                let rect = [map_x, y, size, size];
                let first_vertex = batch.vertices.len();
                draw(game, batch, &state, rect, false, None);
                draw_minimap_frame(game, batch, rect, settings.round);
                if let Some(frame)=state.frame.as_ref().filter(|frame| !frame.view.large &&
                    frame.view.dimension==state.selected && frame.view.session==state.session) {
                    draw_radar(game,batch,&frame.view,rect,settings.round,true);
                }
                set_opacity(batch, first_vertex, game.map_option("minimap_opacity").unwrap_or(100));
                if !lines.is_empty() {
                    let left = if centered || custom.is_some() {
                        (map_x + (size - width) * 0.5).clamp(0., (batch.width - width).max(0.))
                    } else { x };
                    let top = y + size + 2.;
                    let opacity = u32::from(game.map_option("minimap_info_background").unwrap_or(40).min(100));
                    game.native_fill(batch, [left, top, width, footer - 2.], (opacity * 255 / 100) << 24);
                    let alignment = game.map_option("minimap_info_alignment").unwrap_or(0);
                    for (index, (line, fg, bg)) in lines.iter().enumerate() {
                        if let Some(color) = bg {
                            game.native_fill(batch, [left, top + index as f32 * line_height, width, line_height],
                                (opacity * 255 / 100) << 24 | color);
                        }
                        let spare = width - game.map_text_width(line) * scale;
                        let start = left + match alignment {
                            1 => 2.,
                            2 => spare - 2.,
                            _ => spare * 0.5,
                        };
                        let baseline = top + (index as f32 * 12. + 1.) * scale;
                        let first = batch.vertices.len();
                        game.map_text(batch, line, [start, baseline], fg.unwrap_or(0xdddddd), true);
                        batch.scale_since(first, start, baseline, scale, scale);
                    }
                }
            }
        }
    }
}

pub(super) fn gui_render_post(
    game: &Game<'_>,
    batch: &mut HudBatch,
    gui_hidden: bool,
    focused: bool,
) {
    if gui_hidden
        || !game.native_window()
        || !game.map_on(runtime::MOD_ID)
        || game.mod_screen().is_some()
        || game.screen != crate::game::Screen::Playing
        || game.native_player().is_none()
    {
        return;
    }
    if game.map_option("minimap_hud_layer").unwrap_or(1) > 1 {
        draw_hud(game, batch, &mut Corners::new(batch.width,batch.height,effect_height(game)));
    }
    if !focused { return; }
    let state = state();
    if missing_item(game,&state,false).is_some() {return;}
    let settings = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).settings();
    draw_world_waypoints(game, batch, &state, &settings);
    draw_tracked_in_world(game,batch);
}

/// The eight compass points as world-space unit steps (`dx`, `dz`): north is
/// `-Z`, east is `+X`. `draw_directions` rotates them by the map's angle.
const DIRECTION_STEPS: [(f64, f64); 8] = [
    (0.0, -1.0),  // N
    (1.0, -1.0),  // NE
    (1.0, 0.0),   // E
    (1.0, 1.0),   // SE
    (0.0, 1.0),   // S
    (-1.0, 1.0),  // SW
    (-1.0, 0.0),  // W
    (-1.0, -1.0), // NW
];

fn direction_label(style: runtime::minimap::DirectionStyle, index: usize) -> &'static str {
    match style {
        runtime::minimap::DirectionStyle::Compass => match index {
            0 => tr("N", "С", "N", "N", "N"),
            1 => tr("NE", "СВ", "NE", "NE", "NO"),
            2 => tr("E", "В", "E", "E", "O"),
            3 => tr("SE", "ЮВ", "SE", "SE", "SO"),
            4 => tr("S", "Ю", "S", "S", "S"),
            5 => tr("SW", "ЮЗ", "SO", "SO", "SW"),
            6 => tr("W", "З", "O", "O", "W"),
            7 => tr("NW", "СЗ", "NO", "NO", "NW"),
            _ => "",
        },
        runtime::minimap::DirectionStyle::Axes => match index {
            0 => "Z-",
            1 => "X+Z-",
            2 => "X+",
            3 => "X+Z+",
            4 => "Z+",
            5 => "X-Z+",
            6 => "X-",
            7 => "X-Z-",
            _ => "",
        },
    }
}

/// Cardinal letters at the inner edge of the map. They rotate with the map so
/// the north marker always points at real north.
fn draw_directions(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    round: bool,
    large: bool,
    settings: &runtime::minimap::Settings,
) {
    let compass=game.map_option("minimap_compass_location").unwrap_or(1);
    if !settings.directions || !large && compass==0 { return; }
    let (s, c) = view.angle.sin_cos();
    let cx = f64::from(rect[0]) + f64::from(rect[2]) / 2.0;
    let cy = f64::from(rect[1]) + f64::from(rect[3]) / 2.0;
    // The large map's dimension tabs and coordinate footer float at the top and
    // bottom; keep the letters clear of them there.
    let (inset, top, bottom) = if large {
        (12.0, 36.0, 52.0)
    } else {
        let inset=if compass==2 {5.} else {8.};
        (inset,inset,inset)
    };
    let radius = f64::from(rect[2].min(rect[3])) / 2.0 - f64::from(inset);
    let half_width = f64::from(rect[2]) / 2.0 - f64::from(inset);
    let half_top = f64::from(rect[3]) / 2.0 - f64::from(top);
    let half_bottom = f64::from(rect[3]) / 2.0 - f64::from(bottom);
    for (index, (dx, dz)) in DIRECTION_STEPS.iter().copied().enumerate() {
        // Screen direction of a world step, rotating with the map's angle.
        let sx = dx * c + dz * s;
        let sy = -dx * s + dz * c;
        let (x, y) = if round {
            let length = sx.hypot(sy);
            if length <= f64::EPSILON {
                continue;
            }
            (cx + sx / length * radius, cy + sy / length * radius)
        } else {
            let mut t = f64::INFINITY;
            if sx.abs() > 1e-6 {
                t = t.min(half_width / sx.abs());
            }
            if sy < 0.0 {
                t = t.min(half_top / sy.abs());
            } else if sy > 0.0 {
                t = t.min(half_bottom / sy.abs());
            }
            if !t.is_finite() {
                continue;
            }
            (cx + sx * t, cy + sy * t)
        };
        let label = direction_label(settings.direction_style, index);
        let width = game.map_text_width(label);
        let lx = x as f32 - width / 2.0;
        let ly = y as f32 - 4.0;
        let color=if large {0} else {game.map_color_option(if index==0 {"minimap_compass_north"} else {"minimap_compass_color"}).unwrap_or(0)};
        let scale = if large { 1. } else { waypoint_scale(game, "minimap_compass_scale") * minimap_ui_scale(game) };
        let first=batch.vertices.len();
        game.native_fill(batch,[lx-2.,ly-2.,width+4.,12.],0x90000000|color);
        game.map_text(batch,label,[lx,ly],0xffffffff,true);
        batch.scale_since(first,x as f32,y as f32,scale,scale);
    }
}

fn upload_terrain_patches(
    game: &mut Game<'_>,
    previous: &maps::TerrainAtlas,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> bool {
    let _timing = runtime::profiling::measure("upload");
    if previous.width != width as usize || previous.height != height as usize {
        return game.mod_image(width, height, pixels);
    }
    let Some(sprite) = game
        .gui
        .atlas
        .sprites
        .get(horizon_client::hud::MOD_IMAGE_SPRITE)
        .copied()
    else {
        return false;
    };
    if width > sprite.w || height > sprite.h || pixels.len() != width as usize * height as usize * 4
    {
        return false;
    }
    let origin = [
        (sprite.u0 * game.gui.atlas.width as f32).round() as u32,
        (sprite.v0 * game.gui.atlas.height as f32).round() as u32,
    ];
    for [x, y, w, h] in
        maps::image_patch_regions(&previous.pixels, pixels, width as usize, height as usize)
    {
        runtime::profiling::record("texture-transfer", Duration::ZERO, 1, (w * h * 4) as u64);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in y..y + h {
            let start = (row * width as usize + x) * 4;
            rgba.extend_from_slice(&pixels[start..start + w * 4]);
        }
        game.map_patches.push(horizon_client::font::Patch {
            x: origin[0] + x as u32,
            y: origin[1] + y as u32,
            width: w as u32,
            height: h as u32,
            rgba,
        });
    }
    true
}

fn draw_terrain_polygon(
    game: &Game<'_>,
    batch: &mut HudBatch,
    view: &View,
    rect: [f32; 4],
    bounds: [i64; 4],
    uv: [f32; 4],
) -> bool {
    let points = maps::terrain_polygon(view, bounds, uv);
    let Some(sprite) = game
        .gui
        .atlas
        .sprites
        .get(horizon_client::hud::MOD_IMAGE_SPRITE)
    else {
        return false;
    };
    let pos = |p: [f64; 4]| {
        [
            rect[0] + (p[0] as f32 + 0.5) * rect[2],
            rect[1] + (p[1] as f32 + 0.5) * rect[3],
        ]
    };
    let texture = |p: [f64; 4]| {
        [
            sprite.u0 + p[2] as f32 / sprite.w as f32 * (sprite.u1 - sprite.u0),
            sprite.v0 + p[3] as f32 / sprite.h as f32 * (sprite.v1 - sprite.v0),
        ]
    };
    for i in 1..points.len().saturating_sub(1) {
        let p = [points[0], points[i], points[i + 1], points[i + 1]];
        batch.quad(p.map(pos), p.map(texture), u32::MAX);
    }
    points.len() >= 3
}

fn draw_paged_terrain(
    batch: &mut HudBatch,
    state: &State,
    frame: &maps::Frame,
    atlas: &maps::TerrainAtlas,
    rect: [f32; 4],
) -> bool {
    let mut visible = false;
    // Keep the padded/predicted page set resident on the GPU, including pages
    // just behind the camera. Horizon drops picture textures absent from a batch.
    for (_, image) in &state.page_images {
        if !batch.pictures.iter().any(|old| Arc::ptr_eq(old, image)) {
            batch.pictures.push(image.clone());
        }
    }
    let mut paint = |bounds: [i64; 4], mut uv: [f32; 4]| {
        if frame.view.terrain_style.live_chunks_only
            && (state.dimension != frame.view.dimension
                || bounds[2] != 16 || bounds[3] != 16
                || !state.chunk_keys.contains(&((bounds[0] / 16) as i32, (bounds[1] / 16) as i32)))
        {
            return;
        }
        let page = uv[1] as usize / 64;
        let Some((_, image)) = state.page_images.get(page) else {
            return;
        };
        uv[1] -= (page * 64) as f32;
        if image.flags.blur {
            // Keep linear filtering within this atlas tile.
            for axis in 0..2 {
                let inset = (uv[axis + 2] * 0.5).min(0.5);
                uv[axis] += inset;
                uv[axis + 2] -= inset * 2.;
            }
        }
        let mut points = maps::terrain_polygon(&frame.view, bounds, uv);
        for p in &mut points {
            p[0] = f64::from(rect[0]) + (p[0] + 0.5) * f64::from(rect[2]);
            p[1] = f64::from(rect[1]) + (p[1] + 0.5) * f64::from(rect[3]);
        }
        visible |= points.len() >= 3;
        super::minimap_pages::paint(batch, image, &points);
    };
    for coarse in &atlas.coarse {
        if frame.view.terrain_style.live_chunks_only && coarse.key.level > 0 {
            continue;
        }
        let bounds = coarse.key.bounds();
        for part in &coarse.fragments {
            paint(
                *part,
                [
                    coarse.uv_origin[0] as f32
                        + ((part[0] - bounds[0]) as f64 * 16. / bounds[2] as f64) as f32,
                    coarse.uv_origin[1] as f32
                        + ((part[1] - bounds[1]) as f64 * 16. / bounds[3] as f64) as f32,
                    (part[2] as f64 * 16. / bounds[2] as f64) as f32,
                    (part[3] as f64 * 16. / bounds[3] as f64) as f32,
                ],
            );
        }
    }
    for &i in &atlas.draw_order {
        let chunk = &atlas.chunks[i];
        let slot = atlas.slots[i];
        paint(
            [i64::from(chunk[0]) * 16, i64::from(chunk[1]) * 16, 16, 16],
            [(slot % 64 * 16) as f32, (slot / 64 * 16) as f32, 16., 16.],
        );
    }
    visible
}

fn draw_chunk_grid(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4]) {
    let scale = (f64::from(rect[2]) / view.span[0]).min(f64::from(rect[3]) / view.span[1]);
    let width=if view.large {1.} else {f32::from(game.map_option("minimap_grid_width").unwrap_or(1))};
    let chunk_color=0x70000000|if view.large {0x525f70} else {game.map_color_option("minimap_grid_color").unwrap_or(0x525f70)};
    for (spacing, color) in [(16,chunk_color),(512,0xd0ffc871)] {
        if scale * f64::from(spacing) < 10.0 {
            continue;
        }
        for line in view.grid_lines(spacing) {
            let [mut from, mut to] = line.map(|point| world_screen(view, rect, point));
            if view.round {
                let center = [rect[0] + rect[2] * 0.5, rect[1] + rect[3] * 0.5];
                let dx = to[0] - from[0];
                let dy = to[1] - from[1];
                let length = dx.hypot(dy);
                if length < f32::EPSILON {
                    continue;
                }
                let [nx, ny] = [dx / length, dy / length];
                let [ox, oy] = [center[0] - from[0], center[1] - from[1]];
                let offset = ox * ny - oy * nx;
                let radius = rect[2].min(rect[3]) * 0.5 - 1.0;
                let inside = radius * radius - offset * offset;
                if inside <= 0.0 {
                    continue;
                }
                let half = inside.sqrt();
                let middle = ox * nx + oy * ny;
                let left = (middle - half).max(0.0);
                let right = (middle + half).min(length);
                if left >= right {
                    continue;
                }
                to = [from[0] + right * nx, from[1] + right * ny];
                from = [from[0] + left * nx, from[1] + left * ny];
            }
            let (x0, x1) = (from[0].min(to[0]), from[0].max(to[0]));
            let (y0, y1) = (from[1].min(to[1]), from[1].max(to[1]));
            if (x1 - x0) < 0.01 && x0 >= rect[0] && x0 <= rect[0] + rect[2] - 1.0 {
                let start = y0.max(rect[1]);
                let end = y1.min(rect[1] + rect[3]);
                if end > start {
                    game.native_fill(batch, [x0.floor(),start,width.min(rect[0]+rect[2]-x0),end-start], color);
                }
            } else if (y1 - y0) < 0.01 && y0 >= rect[1] && y0 <= rect[1] + rect[3] - 1.0 {
                let start = x0.max(rect[0]);
                let end = x1.min(rect[0] + rect[2]);
                if end > start {
                    game.native_fill(batch, [start,y0.floor(),end-start,width.min(rect[1]+rect[3]-y0)], color);
                }
            } else {
                draw_line(game, batch, from, to, rect, color);
            }
        }
    }
}

pub(super) fn tick_radar(game: &Game<'_>) {
    use ::trailgrid_maps::radar::{self, Facts};
    if !game.native_window() || game.is_logical_server() || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let setting = |key: &str| game.map_number(key).or_else(|| game.map_option(key).map(f64::from))
        .unwrap_or_else(|| match key {
            "radar_smooth" | "radar_hide_invisible" | "radar_players" | "radar_mobs" | "radar_items" | "radar_other" | "radar_name_scale" => 1.,
            "radar_range" => 64.,
            "radar_height" => 32.,
            "radar_dot_size" => 2.,
            "radar_icon_size" => 9.,
            _ => 0.,
        });
    if !radar::sample_due(setting("radar")>0.) {return;}
    if !radar::uses_categories() {
        radar::update(game, setting, |_,_,_| Facts::default());
        return;
    }
    let mobs: horizon_client::fast_hash::FxHashMap<_,_> = game.mobs.iter().map(|mob| (mob.id,mob)).collect();
    let items: horizon_client::fast_hash::FxHashMap<_,_> = game.dropped.iter().map(|item| (item.id,item.stack.item.as_ref())).collect();
    let own_team = game.player_team().map(|team| team.name.as_str());
    let own_uuid = game.own_uuid();
    let cave = game.native_player().is_some_and(|player| {
        matches!(state().layers.get(&Dimension::from_id(player.dimension)),Some(Layer::Below(_)))
    });
    let tracked = LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner());
    radar::update(game,setting,|id,kind,at| {
        let mob = mobs.get(&id).copied();
        let team = mob.and_then(|mob| game.mob_team_of(mob)).or_else(|| game.mob_team(id));
        let name = team.map(|team| team.name.as_str());
        let baby = mob.is_some_and(|mob| mob.is_baby());
        let is_tracked = kind=="minecraft:player" && game.remote.as_ref()
            .and_then(|remote| remote.player_uuid(id))
            .is_some_and(|uuid| tracked.iter().any(|point| point.id == WaypointId::Uuid(uuid)));
        Facts {
            item: items.get(&id).copied(),
            custom_name: mob.and_then(|mob| mob.custom_name.as_deref()).map(std::borrow::Cow::Borrowed)
                .or_else(|| kind.eq("minecraft:armor_stand").then(|| {
                    game.armor_stands.iter().find(|stand| stand.id==id)?.custom_name.as_ref()
                        .map(|name| std::borrow::Cow::Owned(name.plain()))
                }).flatten()),
            baby,
            hostile: game.difficulty_id()>0 && !(baby && kind=="minecraft:piglin")
                && (kind=="minecraft:ender_dragon" || mob.is_some_and(|mob| mob.category()=="monster")),
            tamed: mob.is_some_and(|mob| {
                let owned=|owner:[i32;4]| owner.into_iter().fold(0u128,|id,word|(id<<32)|u128::from(word as u32))==own_uuid;
                if horizon_client::mobs::equine::is_equine(mob.species()) {
                    mob.extra.tame
                } else if kind=="minecraft:fox" {
                    horizon_client::mobs::fox::trusted_entities(mob).any(owned)
                } else {
                    mob.extra.owner.is_some_and(owned)
                }
            }),
            same_team: name==own_team,
            team: name,
            team_color: team.and_then(|team| team.color.as_deref()).and_then(horizon_client::text::named_color),
            light: game.map_light_if_loaded(at.map(|axis| axis.floor() as i32)),
            cave,
            tracked: is_tracked,
        }
    });
}

fn radar_icon(game: &Game<'_>, batch: &mut HudBatch, mob: Option<&horizon_client::mobs::Mob>, dot: &::trailgrid_maps::radar::Dot, dst: [f32;4], opacity: u32) -> bool {
    if let Some(drawn) = super::minimap_radar_icons::render(game, batch, mob, &dot.kind, dst, opacity) {
        return drawn;
    }
    if dot.kind == "minecraft:player" {
        let skin = mob.map(|mob| mob.skin.as_deref().unwrap_or(&mob.kind.texture)).unwrap_or("entity/player/wide/steve");
        if game.native_sprite(batch, skin, [8.,8.,8.,8.], dst, Some(opacity | 0xffffff)) {
            game.native_sprite(batch, skin, [40.,8.,8.,8.], dst, Some(opacity | 0xffffff));
            return true;
        }
        return game.native_sprite(batch, "entity/player/wide/steve", [8.,8.,8.,8.], dst, Some(opacity | 0xffffff));
    }
    if dot.kind == "minecraft:item" {
        let Some(item) = game.dropped.iter().find(|item| item.id == dot.entity_id) else { return false };
        let start = batch.vertices.len();
        let drawn = game.native_item_stack(batch,&item.stack,[dst[0],dst[1]],dst[2]);
        set_opacity(batch,start,((opacity >> 24)*100/255) as u8);
        return drawn;
    }

    let Some(mob) = mob else { return false };
    let (model_key, texture) = horizon_client::mobs::render::body(mob);
    let model = mob.kind.layer(&model_key).unwrap_or(&mob.kind.model);
    let Some(sprite) = game.gui.atlas.sprites.get(texture.as_ref()) else { return false };
    let head = radar_head(model).unwrap_or(model);
    let Some(face) = head.polygons.iter().filter(|face| face.normal[2] < -0.5)
        .max_by(|a,b| radar_uv_area(a).total_cmp(&radar_uv_area(b))) else { return false };
    let [u0,v0,u1,v1] = radar_uv(face);
    if u0 >= u1 || v0 >= v1 || u0 < 0. || v0 < 0. || u1 > 1. || v1 > 1. { return false; }
    let (width,height) = (sprite.w as f32,sprite.h as f32);
    let src = [u0*width,v0*height,(u1-u0)*width,(v1-v0)*height];
    let [x,y,w,h] = dst;
    let ratio = src[2] / src[3];
    let (w2,h2) = if ratio > 1. { (w,h/ratio) } else { (w*ratio,h) };
    game.native_sprite(batch,texture.as_ref(),src,[x+(w-w2)*0.5,y+(h-h2)*0.5,w2,h2],Some(opacity | 0xffffff))
}

fn radar_head(part: &horizon_client::mobs::Part) -> Option<&horizon_client::mobs::Part> {
    if part.name == "head" || part.name.ends_with("_head") { return Some(part); }
    part.children.iter().find_map(radar_head)
}

fn radar_uv(face: &horizon_client::mobs::Polygon) -> [f32;4] {
    let mut bounds = [f32::INFINITY,f32::INFINITY,f32::NEG_INFINITY,f32::NEG_INFINITY];
    for [_,_,_,u,v] in face.vertices {
        bounds = [bounds[0].min(u),bounds[1].min(v),bounds[2].max(u),bounds[3].max(v)];
    }
    bounds
}

fn radar_uv_area(face: &horizon_client::mobs::Polygon) -> f32 {
    let [u0,v0,u1,v1] = radar_uv(face);
    (u1-u0)*(v1-v0)
}

fn draw_radar(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4], round: bool, after_frame: bool) {
    let Some(radar) = ::trailgrid_maps::radar::snapshot() else { return };
    if radar.dimension != view.dimension.id() || (view.large && !radar.world_map) { return; }
    let dots = radar.dots.iter().filter(|dot| !view.large && dot.over_frame == after_frame || view.large && !after_frame);
    let mobs: horizon_client::fast_hash::FxHashMap<_,_> = if radar.smooth || dots.clone().any(|dot| dot.icon) {
        game.mobs.iter().map(|mob| (mob.id,mob)).collect()
    } else { Default::default() };
    let mut labels = Vec::new();
    for dot in dots.rev() {
        let mut position = dot.at;
        if radar.smooth && let Some(mob) = mobs.get(&dot.entity_id) {
            let current = mob.position;
            let previous = mob.previous;
            let partial = game.clock.camera_fraction();
            position = [
                previous.x+(current.x-previous.x)*partial,
                previous.y+(current.y-previous.y)*partial,
                previous.z+(current.z-previous.z)*partial,
            ];
        }
        let [x,_,z] = position;
        let [x,y] = world_screen(view,rect,[x,z]);
        let width = if dot.icon { radar.icon_size*dot.icon_scale } else { dot.size };
        let half = width*0.5;
        let within = |at| map_overlay_inside(at,rect,round) ||
            after_frame && x >= rect[0] && y >= rect[1] && x <= rect[0]+rect[2] && y <= rect[1]+rect[3] &&
                (!round || ((x-rect[0])/rect[2]-0.5).hypot((y-rect[1])/rect[3]-0.5) <= 0.5);
        if !within([x-half-1.,y-half-1.]) || !within([x+half+1.,y+half+1.]) { continue; }
        let alpha = u32::from(dot.alpha)<<24;
        let icon = dot.icon && radar_icon(game,batch,mobs.get(&dot.entity_id).copied(),dot,[x-half,y-half,width,width],alpha);
        if !icon {
            let half = dot.size*0.5;
            if radar.legacy || dot.size<4. {
                game.native_fill(batch,[x-half-1.,y-half-1.,dot.size+2.,dot.size+2.],0xaf0a1318);
                game.native_fill(batch,[x-half,y-half,dot.size,dot.size],alpha|dot.color);
            } else {
                for (radius,color) in [(half+1.,0xaf0a1318),(half,alpha|dot.color)] {
                    game.native_quad(batch,[[x,y-radius],[x+radius,y],[x,y+radius],[x-radius,y]],color);
                }
            }
        }
        let label = dot.label.as_ref().or_else(|| if icon {None} else {dot.fallback_name.as_ref()});
        if let Some(label) = label {
            labels.push((label,alpha|dot.color,[x,y-width*0.5]));
        }
    }
    for (label,color,[x,y]) in labels.into_iter().rev().take(if view.large {48} else {24}).collect::<Vec<_>>().into_iter().rev() {
        let scale=radar.name_scale;
        let width=game.map_text_width(label)*scale;
        let start=[x-width*0.5,y-10.*scale];
        if [start,[start[0]+width,start[1]],[start[0],y],[start[0]+width,y]]
            .into_iter().all(|corner| map_overlay_inside(corner,rect,round))
        {
            let vertex=batch.vertices.len();
            game.map_text(batch,label,start,color,true);
            batch.scale_since(vertex,start[0],start[1],scale,scale);
        }
    }
}

fn draw_tracked_in_world(game:&Game<'_>,batch:&mut HudBatch) {
    if game.map_option("locator_world")!=Some(1) {return;}
    let Some(player)=game.native_player() else {return;};
    let size=[batch.width,batch.height];
    let camera=world_camera(game,&player,size);
    let points=LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error|error.into_inner());
    for point in points.iter() {
        let WaypointTarget::Block(at)=point.target else {continue;};
        let (id,name)=match &point.id {
            WaypointId::Uuid(uuid) if *uuid==game.own_uuid()=>continue,
            WaypointId::Uuid(uuid)=>{
                let id=game.remote.as_ref().and_then(|remote|remote.player_entity(*uuid));
                (id,id.and_then(|id|game.remote_player_name(id)))
            }
            WaypointId::Name(name)=>(None,Some(name.as_str())),
        };
        let position=[f64::from(at[0])+0.5,f64::from(at[1])+1.8,f64::from(at[2])+0.5];
        let Some([x,y])=project(&camera,position,size) else {continue;};
        if x<0. || y<0. || x>size[0] || y>size[1] {continue;}
        let w=8.*waypoint_scale(game,"locator_world_scale");
        let color=0xff000000|point.color.unwrap_or(0x85e9f8) as u32&0xffffff;
        let skin=id.and_then(|id|game.mobs.iter().find(|mob|mob.id==id))
            .map(|mob|mob.skin.as_deref().unwrap_or(&mob.kind.texture));
        if !skin.is_some_and(|skin|game.native_sprite(batch,skin,[8.,8.,8.,8.],[x-w*0.5,y-w*0.5,w,w],None)) {
            game.native_fill(batch,[x-w*0.5,y-w*0.5,w,w],color);
        }
        if let Some(name)=name {
            let name=name.chars().filter(|c|!c.is_control()).take(40).collect::<String>();
            let scale=waypoint_scale(game,"locator_name_scale");
            let left=x-game.map_text_width(&name)*scale*0.5;
            let top=y-w*0.5-10.*scale;
            let first=batch.vertices.len();
            game.map_text(batch,&name,[left,top],color,true);
            batch.scale_since(first,left,top,scale,scale);
        }
    }
}

fn draw_locator_waypoints(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4], round: bool) {
    let mut enabled=game.map_option("radar_locator")==Some(1);
    if view.large {
        enabled=match game.map_option("world_locator_mode") {Some(1)=>false,Some(2)=>true,_=>enabled};
    }
    if !enabled || !game.native_player().is_some_and(|player| player.dimension == view.dimension.id()) {
        return;
    }
    let visible_radar = ::trailgrid_maps::radar::snapshot()
        .filter(|radar| !view.large || radar.world_map);
    for point in LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner()).iter() {
        if let WaypointId::Uuid(uuid) = point.id
            && (uuid == game.own_uuid() || game.remote.as_ref().and_then(|remote| remote.player_entity(uuid))
                .is_some_and(|id| visible_radar.as_ref().is_some_and(|radar| radar.dots.iter().any(|dot| dot.entity_id == id))))
        {
            continue;
        }
        let (at, exact) = match point.target {
            WaypointTarget::Block([x,_,z]) => ([f64::from(x)+0.5,f64::from(z)+0.5],true),
            WaypointTarget::Chunk([x,z]) => ([f64::from(x)*16.+8.,f64::from(z)*16.+8.],false),
            _ => continue,
        };
        let [x,y] = world_screen(view,rect,at);
        let scale=if view.large {1.}else{waypoint_scale(game,"locator_mini_scale")};
        let margin=5.*scale;
        if !map_overlay_inside([x-margin,y-margin],rect,round) || !map_overlay_inside([x+margin,y+margin],rect,round) {
            continue;
        }
        let color = 0xff00_0000 | (point.color.unwrap_or(0x85e9f8) as u32 & 0xffffff);
        if exact {
            game.native_fill(batch,[x-2.*scale,y-2.*scale,4.*scale,4.*scale],color);
        } else {
            for [dx,dy,w,h] in [[-4.,-4.,8.,1.],[-4.,3.,8.,1.],[-4.,-3.,1.,6.],[3.,-3.,1.,6.]] {
                game.native_fill(batch,[x+dx*scale,y+dy*scale,w*scale,h*scale],color);
            }
        }
    }
}

fn map_overlay_inside(at: [f32; 2], rect: [f32; 4], round: bool) -> bool {
    if !inside(at, rect) { return false; }
    let x = (at[0] - rect[0]) / rect[2] - 0.5;
    let z = (at[1] - rect[1]) / rect[3] - 0.5;
    !round || x.hypot(z) <= 0.48
}

fn draw_minimap_frame(game: &Game<'_>, batch: &mut HudBatch, rect: [f32; 4], round: bool) {
    let mode = game.map_option("minimap_frame").unwrap_or(0);
    if mode == 3 { return; }
    let color = 0xff00_0000 | if mode == 0 { 0x303038 } else {
        game.map_color_option("minimap_frame_color").unwrap_or(0x44444a)
    };
    let [x,y,w,h] = rect;
    let thickness = if mode == 1 { 3. } else { 1. };
    if round {
        let center = [x+w/2.,y+h/2.];
        let radius = w.min(h)/2. - thickness;
        let ring = |step| {
            let angle = std::f32::consts::TAU * step as f32 / 96.;
            [center[0]+angle.cos()*radius, center[1]+angle.sin()*radius]
        };
        for step in 0..96 {
            draw_line(game,batch,ring(step),ring(step+1),rect,color);
        }
    } else {
        for edge in [[x,y,w,thickness],[x,y+h-thickness,w,thickness],
            [x,y+thickness,thickness,h-2.*thickness],[x+w-thickness,y+thickness,thickness,h-2.*thickness]] {
            game.native_fill(batch,edge,color);
        }
    }
}

fn draw_slime_chunks(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4], seed: i64) {
    use horizon_core::random::{RandomSource, slime_chunk_random};
    if view.dimension != Dimension::Overworld || view.span[0].max(view.span[1]) > 1024.0 {
        return;
    }
    let [cx, cz] = view.center.map(|value| (value.floor() as i32).div_euclid(16));
    let radius = (view.span[0].hypot(view.span[1]) / 32.0).ceil() as i32 + 1;
    for z in (cz - radius)..=(cz + radius) {
        for x in (cx - radius)..=(cx + radius) {
            let at = world_screen(view, rect, [f64::from(x * 16 + 8), f64::from(z * 16 + 8)]);
            if map_overlay_inside([at[0] - 3., at[1] - 3.], rect, view.round)
                && map_overlay_inside([at[0] + 3., at[1] + 3.], rect, view.round)
                && slime_chunk_random(x, z, seed, 987_234_911).next_int_bound(10) == Ok(0)
            {
                game.native_fill(batch, [at[0] - 3., at[1] - 3., 6., 6.], 0xca33c168);
            }
        }
    }
}

fn draw_light_overlay(game: &Game<'_>, batch: &mut HudBatch, view: &View, rect: [f32; 4]) {
    let Some(player) = game.native_player() else { return; };
    if Dimension::from_id(player.dimension) != view.dimension
        || (f64::from(rect[2]) / view.span[0]).min(f64::from(rect[3]) / view.span[1]) < 2.0
    {
        return;
    }
    let source = game.map_option("light_source").unwrap_or(0);
    let min = game.map_option("light_min").unwrap_or(0);
    let max = game.map_option("light_max").unwrap_or(7);
    let color=game.stored_setting(runtime::MOD_ID,"light_overlay_color")
        .and_then(|value|value.parse::<u32>().ok()).filter(|rgb|*rgb<=0xffffff);
    let y = match view.layer { Layer::Surface | Layer::Below(maps::FULL_CAVE) => player.y.floor() as i32, Layer::Below(y) => y };
    let [cx, cz] = view.center.map(|value| value.floor() as i32);
    for z in (cz - 24)..=(cz + 24) {
        for x in (cx - 24)..=(cx + 24) {
            let at = world_screen(view, rect, [f64::from(x) + 0.5, f64::from(z) + 0.5]);
            if !map_overlay_inside(at, rect, view.round) { continue; }
            if let Some((block, sky)) = game.map_light_if_loaded([x, y, z]) {
                let light = if source == 0 { block } else { sky };
                if (min..=max).contains(&light) {
                    let alpha=if light==0 {0xef000000}else{0xdf000000};
                    game.native_fill(batch,[at[0],at[1],1.5,1.5],alpha|color.unwrap_or(if light==0 {0xfd645f}else{0xf7c15d}));
                }
            }
        }
    }
}

fn draw(
    game: &Game<'_>,
    batch: &mut HudBatch,
    state: &State,
    rect: [f32; 4],
    large: bool,
    mouse: Option<[f32; 2]>,
) {
    let _timing = runtime::profiling::measure("hud-map");
    let settings = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).settings();
    let round = !large && settings.round;
    if large {
        let background=if game.map_option("world_skip_render")==Some(1) {0xff181b20} else {0xef181b20};
        game.native_fill(batch,rect,background);
    } else if let Some(opacity)=game.map_option("minimap_undiscovered").filter(|v|*v>0) {
        let color=(u32::from(opacity)*255/100)<<24 | 0x181b20;
        if round {
            let center=[rect[0]+rect[2]*0.5,rect[1]+rect[3]*0.5];
            let point=|step| {
                let (s,c)=(step as f32*std::f32::consts::TAU/48.).sin_cos();
                [center[0]+c*rect[2]*0.5,center[1]+s*rect[3]*0.5]
            };
            for i in 0..48 {game.native_quad(batch,[center,point(i),point(i+1),center],color);}
        } else {game.native_fill(batch,rect,color);}
    } else if !round {
        game.native_fill(batch,rect,0xef181b20);
    }
    let valid = state.frame.as_ref().filter(|f| {
        f.view.dimension == state.selected
            && f.view.large == large
            && f.view.session == state.session
    });
    if let Some(frame) = valid {
        if let Some(mouse) = mouse {
            let at = frame.view.block_at(
                f64::from((mouse[0] - rect[0]) / rect[2]) * frame.view.width as f64,
                f64::from((mouse[1] - rect[1]) / rect[3]) * frame.view.height as f64,
            );
            runtime::initialize(game.native_data_dir(runtime::MOD_ID))
                .maps
                .inspect(
                    frame.view.session,
                    frame.view.dimension,
                    frame.view.layer,
                    at,
                );
        }
        let mut terrain_visible = frame.known > 0;
        if let Some(atlas) = &frame.atlas {
            terrain_visible = false;
            if !atlas.pages.is_empty() {
                terrain_visible = draw_paged_terrain(batch, state, frame, atlas, rect);
            } else {
                for coarse in &atlas.coarse {
                    for part in &coarse.fragments {
                        if large {
                            if let Some((src, dst)) =
                                atlas.coarse_quad(&frame.view, rect, coarse, *part)
                            {
                                game.map_sprite(
                                    batch,
                                    horizon_client::hud::MOD_IMAGE_SPRITE,
                                    src,
                                    dst,
                                );
                                terrain_visible = true;
                            }
                        } else {
                            let bounds = coarse.key.bounds();
                            let uv = [
                                coarse.uv_origin[0] as f32
                                    + ((part[0] - bounds[0]) as f64 * 16.0 / bounds[2] as f64)
                                        as f32,
                                coarse.uv_origin[1] as f32
                                    + ((part[1] - bounds[1]) as f64 * 16.0 / bounds[3] as f64)
                                        as f32,
                                (part[2] as f64 * 16.0 / bounds[2] as f64) as f32,
                                (part[3] as f64 * 16.0 / bounds[3] as f64) as f32,
                            ];
                            terrain_visible |=
                                draw_terrain_polygon(game, batch, &frame.view, rect, *part, uv);
                        }
                    }
                }
                for (slot, chunk) in atlas.chunks.iter().enumerate() {
                    if large {
                        if let Some((src, dst)) = atlas.quad(&frame.view, rect, *chunk, slot) {
                            game.map_sprite(batch, horizon_client::hud::MOD_IMAGE_SPRITE, src, dst);
                            terrain_visible = true;
                        }
                    } else {
                        let columns = atlas.width / 16;
                        let slot = atlas.slots[slot];
                        terrain_visible |= draw_terrain_polygon(
                            game,
                            batch,
                            &frame.view,
                            rect,
                            [i64::from(chunk[0]) * 16, i64::from(chunk[1]) * 16, 16, 16],
                            [
                                (slot % columns * 16) as f32,
                                (slot / columns * 16) as f32,
                                16.,
                                16.,
                            ],
                        );
                    }
                }
            }
        } else if let Some(source) = &frame.raster_source {
            if let Some((src, dst)) = maps::raster_quad(&source.view, &frame.view, rect) {
                game.map_sprite(batch, horizon_client::hud::MOD_IMAGE_SPRITE, src, dst);
            } else {
                terrain_visible = false;
            }
        } else {
            game.map_sprite(
                batch,
                horizon_client::hud::MOD_IMAGE_SPRITE,
                [0.0, 0.0, frame.view.width as f32, frame.view.height as f32],
                rect,
            );
        }
        draw_claims(game, batch, state, &frame.view, rect);
        if settings.chunk_grid {
            draw_chunk_grid(game, batch, &frame.view, rect);
        }
        if large && settings.footsteps {
            for &(dimension, [x, z]) in &state.footsteps {
                if dimension == frame.view.dimension {
                    let px = world_screen(&frame.view, rect, [f64::from(x) + 0.5, f64::from(z) + 0.5]);
                    if inside(px, [rect[0] + 2.0, rect[1] + 2.0, rect[2] - 4.0, rect[3] - 4.0]) {
                        game.native_fill(batch, [px[0] - 1.5, px[1] - 1.5, 3.0, 3.0], 0xd42a_1f11);
                        game.native_fill(batch, [px[0] - 0.5, px[1] - 0.5, 1.0, 1.0], 0xffff_d679);
                    }
                }
            }
        }
        draw_border(game, batch, &frame.view, rect, mouse);
        if (game.map_option("slime_chunks") == Some(1) || state.slime_overlay || large && state.slime_preview)
            && let Some(seed) = state.slime_seed {
            draw_slime_chunks(game, batch, &frame.view, rect, seed);
        }
        if game.map_option("light_overlay") == Some(1) {
            draw_light_overlay(game, batch, &frame.view, rect);
        }
        let compass_front=large || game.map_option("minimap_compass_front")!=Some(0);
        if !compass_front {draw_directions(game,batch,&frame.view,rect,round,large,&settings);}
        if large && game.map_option("map_server_radius")==Some(1)
            && let Some(remote)=&game.remote
            && let Some(player)=game.native_player()
            && frame.view.dimension==Dimension::from_id(player.dimension) {
            let radius=remote.sent_radius().saturating_sub(1);
            let x=(player.x.floor() as i32).div_euclid(16);
            let z=(player.z.floor() as i32).div_euclid(16);
            draw_world_box(game,batch,&frame.view,rect,[
                f64::from((x-radius)*16),f64::from((z-radius)*16),
                f64::from((x+radius+1)*16),f64::from((z+radius+1)*16),
            ],0x9f62d0ff);
        }
        draw_radar(game, batch, &frame.view, rect, round, false);
        draw_locator_waypoints(game,batch,&frame.view,rect,round);
        draw_waystones(game, batch, &frame.view, rect);
        let mut drawn: Vec<[i32; 2]> = Vec::new();
        for (dimension, at, kind) in &state.portals {
            if *dimension != frame.view.dimension {
                continue;
            }
            if drawn
                .iter()
                .any(|point| (point[0] - at[0]).abs() <= 2 && (point[1] - at[2]).abs() <= 2)
            {
                continue;
            }
            drawn.push([at[0], at[2]]);
            draw_portal_marker(game, batch, &frame.view, rect, *at, *kind);
        }
        if large {
            let hover = state
                .menu
                .map(|menu| menu.block)
                .or_else(|| mouse.and_then(|mouse| hovered_block(&frame.view, rect, mouse)));
            if let Some(block) = hover {
                highlight(game, batch, &frame.view, rect, block);
            }
            if let Some(menu) = state.menu {
                let (chunk, region) = maps::chunk_region(menu.block);
                draw_world_box(
                    game,
                    batch,
                    &frame.view,
                    rect,
                    [
                        f64::from(chunk[0] * 16),
                        f64::from(chunk[1] * 16),
                        f64::from(chunk[0] * 16 + 16),
                        f64::from(chunk[1] * 16 + 16),
                    ],
                    0xffffd45c,
                );
                draw_world_box(
                    game,
                    batch,
                    &frame.view,
                    rect,
                    [
                        f64::from(region[0] * 512),
                        f64::from(region[1] * 512),
                        f64::from(region[0] * 512 + 512),
                        f64::from(region[1] * 512 + 512),
                    ],
                    0xff58c7ff,
                );
            }
        }
        for (index, waypoint) in state
            .waypoints
            .iter()
            .enumerate()
            .filter(|(_, waypoint)| waypoint.dimension == frame.view.dimension && waypoint_on_map(game, state, waypoint, show_hidden_on_map(state, large)))
            .filter(|_| game.map_option(if large {"world_map_waypoints"} else {"minimap_waypoints"}) != Some(0)
                && (!large || f64::from(rect[2])/frame.view.span[0]>=game.map_number("world_waypoint_zoom_min").unwrap_or(0.)))
        {
            let start = batch.vertices.len();
            draw_waypoint(
                game,
                batch,
                &frame.view,
                rect,
                waypoint,
                &waypoint_text(state, index, waypoint),
                if large {11.0*game.map_number("world_waypoint_symbol_scale").unwrap_or(1.) as f32}else{8.0*waypoint_scale(game,"waypoint_map_scale")},
            );
            if !large { set_opacity(batch,start,game.map_option("waypoint_map_opacity").unwrap_or(100)); }
        }
        if let Some(player) = game.native_player() {
            draw_marker(game, batch, &frame.view, rect, state.marker, &player);
        }
        if compass_front {draw_directions(game,batch,&frame.view,rect,round,large,&settings);}
        if terrain_visible {
            return;
        }
    }
    let text = if valid.is_some() {
        tr(
            "No data",
            "Нет данных",
            "Aucune donnée",
            "Sin datos",
            "Keine Daten",
        )
    } else {
        tr(
            "Preparing map…",
            "Подготовка карты…",
            "Préparation de la carte…",
            "Preparando mapa…",
            "Karte wird vorbereitet…",
        )
    };
    game.map_text(
        batch,
        text,
        [
            rect[0] + (rect[2] - game.map_text_width(text)) / 2.0,
            rect[1] + rect[3] / 2.0 - 4.0,
        ],
        0xcccccc,
        true,
    );
}

fn tab(width: f32, index: usize) -> [f32; 4] {
    let w = ((width - 16.0) / 4.0).clamp(1.0, 112.0);
    [8.0 + w * index as f32, 8.0, (w - 2.0).max(1.0), 24.0]
}

fn information_rect(width: f32, height: f32) -> [f32; 4] {
    let w = (width - 24.0).clamp(1.0, 560.0);
    let h = (height - 24.0).clamp(1.0, 260.0);
    [(width - w) / 2.0, (height - h) / 2.0, w, h]
}
fn information_lines(game: &Game<'_>, state: &State, menu: Menu, width: f32) -> Vec<(String, u32)> {
    let layer = state
        .frame
        .as_ref()
        .filter(|frame| frame.view.dimension == menu.dimension)
        .map(|f| f.view.layer)
        .or_else(|| state.layers.get(&menu.dimension).copied())
        .unwrap_or(Layer::Surface);
    let coverage = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
        .maps
        .coverage();
    let (chunk, region) = maps::chunk_region(menu.block);
    let chunk_rect = [i64::from(chunk[0]) * 16, i64::from(chunk[1]) * 16, 16, 16];
    let region_rect = [
        i64::from(region[0]) * 512,
        i64::from(region[1]) * 512,
        512,
        512,
    ];
    let detail = coverage.detailed_at(menu.dimension, layer, menu.block);
    let lod_sample = coverage.lod_sample(menu.dimension, layer, menu.block);
    let lod = lod_sample.as_ref().map(|sample| sample.key);
    let source = if detail {
        tr(
            "Detailed client chunk (cached)",
            "Подробный клиентский чанк (кэш)",
            "Chunk client détaillé (en cache)",
            "Chunk detallado del cliente (caché)",
            "Detaillierter Client-Chunk (Cache)",
        )
    } else if lod.is_some() {
        tr(
            "LOD — approximate data",
            "LOD — приблизительные данные",
            "LOD — données approximatives",
            "LOD — datos aproximados",
            "LOD — ungefähre Daten",
        )
    } else {
        tr(
            "No data for this layer",
            "Нет данных для этого слоя",
            "Aucune donnée pour cette couche",
            "Sin datos para esta capa",
            "Keine Daten für diese Ebene",
        )
    };
    let layer_text = match layer {
        Layer::Surface => tr(
            "Surface",
            "Поверхность",
            "Surface",
            "Superficie",
            "Oberfläche",
        )
        .to_owned(),
        Layer::Below(maps::FULL_CAVE) => tr("Full caves", "Общий слой пещер", "Toutes les grottes", "Todas las cuevas", "Alle Höhlen").to_owned(),
        Layer::Below(y) => localized_format!(
            "Below Y {y}",
            "Ниже Y {y}",
            "Sous Y {y}",
            "Por debajo de Y {y}",
            "Unter Y {y}"
        ),
    };
    let normal = 0xffe1e7ed;
    let yellow = 0xffffd45c;
    let mut lines = vec![
        (
            format!("{} — {}", menu.dimension.label(), layer_text),
            normal,
        ),
        (
            format!("X: {}  Z: {}", menu.block[0], menu.block[1]),
            normal,
        ),
    ];
    if let (Some(y), Some(fluid)) = (menu.height, menu.fluid) {
        let kind = match fluid.kind {
            FluidKind::Water => tr("Water", "Вода", "Eau", "Agua", "Wasser"),
            FluidKind::Lava => tr("Lava", "Лава", "Lave", "Lava", "Lava"),
        };
        lines.push((
            localized_format!(
                "{}: surface Y {y}, bottom Y {}",
                "{}: поверхность Y {y}, дно Y {}",
                "{} : surface Y {y}, fond Y {}",
                "{}: superficie Y {y}, fondo Y {}",
                "{}: Oberfläche Y {y}, Grund Y {}",
                kind,
                fluid.bottom
            ),
            normal,
        ));
    }
    lines.extend([
        (
            localized_format!(
                "Chunk: {} / {}",
                "Чанк: {} / {}",
                "Chunk : {} / {}",
                "Chunk: {} / {}",
                "Chunk: {} / {}",
                chunk[0],
                chunk[1]
            ),
            normal,
        ),
        (
            format!(
                "X {}..{}   Z {}..{}",
                chunk_rect[0],
                chunk_rect[0] + 15,
                chunk_rect[1],
                chunk_rect[1] + 15
            ),
            normal,
        ),
        (
            source.to_owned(),
            if !detail && lod.is_some() {
                yellow
            } else {
                normal
            },
        ),
    ]);
    if let Some(key) = lod {
        let stride = 1_i64 << key.level;
        let level = key.level;
        lines.push((
            localized_format!(
                "LOD quality: level {level} (1 sample per {stride}×{stride} blocks)",
                "Качество LOD: уровень {level} (1 отсчёт на {stride}×{stride} блоков)",
                "Qualité LOD : niveau {level} (1 échantillon pour {stride}×{stride} blocs)",
                "Calidad LOD: nivel {level} (1 muestra por {stride}×{stride} bloques)",
                "LOD-Qualität: Stufe {level} (1 Abtastwert je {stride}×{stride} Blöcke)",
            ),
            yellow,
        ));
        let b = key.bounds();
        lines.push((
            format!(
                "LOD: X {}..{}  Z {}..{}",
                b[0],
                b[0] + b[2] - 1,
                b[1],
                b[1] + b[3] - 1
            ),
            normal,
        ));
    }
    if let Some(sample) = lod_sample {
        let biome = sample
            .biome
            .as_ref()
            .map(|id| localized_biome(id))
            .unwrap_or_else(|| {
                tr("Biome: —", "биом: —", "Biome : —", "Bioma: —", "Biom: —").into()
            });
        lines.push((format!("LOD: Y ≈{} | {biome}", sample.height), yellow));
    }
    for (is_region, bounds) in [(false, chunk_rect), (true, region_rect)] {
        let (d, _) = coverage.counts(menu.dimension, layer, bounds);
        let (_, l) = coverage.counts(menu.dimension, Layer::Surface, bounds);
        if is_region {
            lines.push((
                localized_format!(
                    "Region: {} / {} (32×32 chunks)",
                    "Регион: {} / {} (32×32 чанка)",
                    "Région : {} / {} (32×32 chunks)",
                    "Región: {} / {} (32×32 chunks)",
                    "Region: {} / {} (32×32 Chunks)",
                    region[0],
                    region[1]
                ),
                normal,
            ));
            lines.push((
                format!(
                    "X {}..{}   Z {}..{}",
                    bounds[0],
                    bounds[0] + 511,
                    bounds[1],
                    bounds[1] + 511
                ),
                normal,
            ));
        }
        if l > 0 {
            let area = if is_region {
                tr("Region", "Регион", "Région", "Región", "Region")
            } else {
                tr("Chunk", "Чанк", "Chunk", "Chunk", "Chunk")
            };
            let status = if d > 0 {
                tr(
                    "mixed coverage: detailed + LOD",
                    "смешанное покрытие: подробное + LOD",
                    "couverture mixte : détaillée + LOD",
                    "cobertura mixta: detallada + LOD",
                    "gemischte Abdeckung: detailliert + LOD",
                )
            } else {
                tr(
                    "LOD coverage — approximate",
                    "LOD-покрытие — приблизительно",
                    "couverture LOD — approximative",
                    "cobertura LOD — aproximada",
                    "LOD-Abdeckung — ungefähr",
                )
            };
            lines.push((format!("{area}: {status}"), yellow));
            if layer != Layer::Surface {
                lines.push((
                    tr(
                        "Surface LOD cannot describe this underground layer",
                        "Наружный LOD не описывает этот подземный слой",
                        "Le LOD de surface ne décrit pas cette couche souterraine",
                        "El LOD de superficie no describe esta capa subterránea",
                        "Oberflächen-LOD beschreibt diese unterirdische Ebene nicht",
                    )
                    .to_owned(),
                    yellow,
                ));
            }
        }
        if is_region {
            lines.push((
                localized_format!(
                    "Cached detailed chunks: {d}; LOD footprints: {l}",
                    "Подробных чанков в кэше: {d}; LOD-тайлов: {l}",
                    "Chunks détaillés en cache : {d} ; tuiles LOD : {l}",
                    "Chunks detallados en caché: {d}; teselas LOD: {l}",
                    "Detaillierte Chunks im Cache: {d}; LOD-Kacheln: {l}"
                ),
                normal,
            ));
        }
    }
    lines.push((
        tr(
            "Visit history is not recorded yet",
            "История посещений пока не записывается",
            "L’historique des visites n’est pas encore enregistré",
            "El historial de visitas aún no se registra",
            "Der Besuchsverlauf wird noch nicht aufgezeichnet",
        )
        .to_owned(),
        0xff9faebd,
    ));
    let mut wrapped = Vec::new();
    for (text, color) in lines {
        let mut line = String::new();
        for word in text.split_whitespace() {
            let next = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if !line.is_empty() && game.map_text_width(&next) > width {
                wrapped.push((std::mem::take(&mut line), color));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            wrapped.push((line, color));
        }
    }
    wrapped
}
fn draw_information(game: &Game<'_>, batch: &mut HudBatch, state: &State, menu: Menu) {
    game.native_fill(batch, [0.0, 0.0, batch.width, batch.height], 0x98000000);
    let rect = information_rect(batch.width, batch.height);
    game.native_fill(batch, rect, 0xff151d27);
    game.map_text(
        batch,
        tr(
            "Chunk / region",
            "Чанк / регион",
            "Chunk / région",
            "Chunk / región",
            "Chunk / Region",
        ),
        [rect[0] + 10.0, rect[1] + 8.0],
        0xffffff,
        true,
    );
    let lines = information_lines(game, state, menu, rect[2] - 24.0);
    let visible = ((rect[3] - 56.0) / 12.0).max(1.0) as usize;
    let start = state.info_scroll.min(lines.len().saturating_sub(visible));
    for (i, (text, color)) in lines.iter().skip(start).take(visible).enumerate() {
        game.map_text(
            batch,
            text,
            [rect[0] + 10.0, rect[1] + 25.0 + i as f32 * 12.0],
            *color,
            true,
        );
    }
    if lines.len() > visible {
        let area = rect[3] - 56.0;
        let thumb = area * visible as f32 / lines.len() as f32;
        let y = rect[1]
            + 25.0
            + (area - thumb) * start as f32 / lines.len().saturating_sub(visible).max(1) as f32;
        game.native_fill(batch, [rect[0] + rect[2] - 7.0, y, 3.0, thumb], 0xffa2b2bf);
    }
    button(
        game,
        batch,
        [
            rect[0] + rect[2] - 82.0,
            rect[1] + rect[3] - 24.0,
            72.0,
            18.0,
        ],
        tr("Close", "Закрыть", "Fermer", "Cerrar", "Schließen"),
        game.cursor(),
        true,
    );
}

fn export_map_png(game: &Game<'_>, state: &mut State, dimension: Dimension) {
    if state.export.is_some() {
        return;
    }
    let layer = state.frame.as_ref()
        .filter(|frame| frame.view.session == state.session && frame.view.dimension == dimension)
        .map_or(Layer::Surface, |frame| frame.view.layer);
    let maps = &runtime::initialize(game.native_data_dir(runtime::MOD_ID)).maps;
    let text = match maps.export_png(state.session, dimension, layer, write_map_png) {
        Ok(reply) => {
            state.export = Some(reply);
            tr(
                "Exporting explored terrain…",
                "Экспортирую исследованную местность…",
                "Export de la carte…",
                "Exportando el mapa…",
                "Karte wird exportiert…",
            ).to_owned()
        }
        Err(error) => error,
    };
    state.notice = Some((text, Instant::now()));
}

fn map_tools(width: f32, state: &State) -> impl Iterator<Item = [f32; 4]> + use<> {
    let count = if state.base_world.starts_with("server:") { 6 } else { 5 };
    let w = ((width - 14. - 2. * count as f32) / count as f32).clamp(1., 100.);
    (0..count).map(move |i| {
        let remaining = (count - i) as f32;
        [width - 6. - remaining * (w + 2.), 60., w, 20.]
    })
}

fn latest_death(state: &State) -> Option<&Waypoint> {
    let mut deaths = state.waypoints.iter().rev().filter(|point| point.icon == runtime::WaypointIcon::Skull);
    deaths.clone().find(|point| point.options.purpose == 1).or_else(|| deaths.next())
}

fn map_panel_rows(height: f32) -> usize {
    (((height - 238.) / 20.).floor() as usize).clamp(1, 12)
}

fn map_panel_rect(width: f32, height: f32) -> [f32; 4] {
    let w = (width - 16.).clamp(1., 228.);
    let body = 20. * map_panel_rows(height) as f32 + 76.;
    [width - w - 8., 84., w, body + 26.]
}

fn map_panel_row(panel: [f32; 4], index: usize) -> [f32; 4] {
    [panel[0] + 5., panel[1] + 78. + 20. * index as f32, (panel[2] - 10.).max(1.), 18.]
}

fn map_panel_fields(panel: [f32; 4]) -> [[f32; 4]; 4] {
    let [x, y, w, _] = panel;
    let half = (w - 14.) * 0.5;
    [
        [x + 5., y + 20., w - 10., 17.],
        [x + 5., y + 39., w - 10., 17.],
        [x + 5., y + 58., half, 17.],
        [x + 9. + half, y + 58., half, 17.],
    ]
}

fn map_panel_waypoints<'a>(game: &Game<'_>, state: &'a State) -> Vec<&'a Waypoint> {
    let query = state.waypoint_search.trim().to_lowercase();
    state.waypoints.iter().filter(|point| {
        waypoint_on_map(game, state, point, state.map_panel_hidden)
            && (query.is_empty() || point.name.to_lowercase().contains(&query))
    }).collect()
}

fn visible_map_panel_waypoints<'a>(game: &Game<'_>, state: &'a State, height: f32) -> Vec<&'a Waypoint> {
    let points = map_panel_waypoints(game, state);
    let count = map_panel_rows(height);
    let start = state.map_panel_scroll.min(points.len().saturating_sub(count));
    points.into_iter().skip(start).take(count).collect()
}

const SETTING_CATEGORIES: usize = 5;

fn setting_category(key: &str) -> usize {
    if key.contains("radar") || key.starts_with("locator_") || key == "world_locator_mode" {
        3
    } else if key.contains("waypoint") || key.contains("waystone") {
        2
    } else if key.starts_with("world_") || key.starts_with("map_") || key == "footsteps" {
        1
    } else if key.starts_with("minimap_") || key.starts_with("quick_minimap") ||
        matches!(key,"enabled"|"corner"|"size"|"shape"|"rotate"|"zoom"|"mini_dimension"|"directions"|"direction_style")
    {
        0
    } else {
        4
    }
}

fn setting_tabs() -> [&'static str; SETTING_CATEGORIES] {
    [
        tr("Minimap","Мини-карта","Mini-carte","Minimapa","Minikarte"),
        tr("World map","Карта мира","Carte du monde","Mapa mundial","Weltkarte"),
        tr("Waypoints","Метки","Repères","Marcadores","Wegpunkte"),
        tr("Radar","Радар","Radar","Radar","Radar"),
        tr("Other","Прочее","Autres","Otros","Sonstiges"),
    ]
}

fn settings_catalog<'a>(game: &'a Game<'_>, category: usize) -> impl Iterator<Item=&'a horizon_client::mod_list::NativeSetting> {
    game.settings.mods.installed.iter().find(|m|m.id==runtime::MOD_ID)
        .and_then(|m|m.native.as_ref()).into_iter()
        .flat_map(|page|page.settings.iter()).filter(move |row| setting_category(&row.key)==category)
}

pub(super) fn settings_panel(w: f32, h: f32) -> [f32;4] {
    let width = (w-20.).clamp(1.,670.);
    let height = (h-24.).clamp(1.,600.);
    [(w-width)*0.5,(h-height)*0.5,width,height]
}

fn settings_tab(panel:[f32;4],i:usize)->[f32;4] {
    let width=(panel[2]-26.)/SETTING_CATEGORIES as f32;
    [panel[0]+10.+(width+1.)*i as f32,panel[1]+27.,width,22.]
}

fn settings_rows(panel:[f32;4])->usize {
    (((panel[3]-94.)/25.).floor() as usize).max(1)
}

fn settings_row(panel:[f32;4],i:usize)->([f32;4],[f32;4]) {
    let row=[panel[0]+10.,panel[1]+56.+25.*i as f32,panel[2]-20.,22.];
    let control=(row[2]*0.37).clamp(1.,210.);
    (row,[row[0]+row[2]-control,row[1],control,row[3]])
}

fn settings_back(panel:[f32;4])->[f32;4] {
    [panel[0]+panel[2]-100.,panel[1]+panel[3]-28.,90.,20.]
}

fn setting_value(game: &Game<'_>, value:&horizon_client::mod_list::NativeSettingValue)->String {
    use horizon_client::mod_list::NativeSettingValue as Value;
    match value {
        Value::Toggle(on) => if *on {tr("On","Вкл.","Oui","Sí","An")} else {tr("Off","Выкл.","Non","No","Aus")}.into(),
        Value::Choice { options, index } => options.get(*index)
            .map(|option| game.translate(option).unwrap_or_else(||option.clone())).unwrap_or_default(),
        Value::Range { value, .. } => format!("{value:.3}").trim_end_matches('0').trim_end_matches('.').to_owned(),
        Value::Color(rgb) => format!("#{rgb:06X}"),
    }
}

#[derive(Clone, Copy)]
struct PlacementEdit {
    position: [f32; 2],
    span: [f32; 2],
}

impl PlacementEdit {
    fn move_by(&mut self, pixels: [f32; 2]) {
        for (axis, delta) in pixels.into_iter().enumerate() {
            if self.span[axis] > 0. {
                self.position[axis] = (self.position[axis] + delta / self.span[axis]).clamp(0., 1.);
            }
        }
    }
}

fn custom_minimap_position(game: &Game<'_>) -> Option<[f32; 2]> {
    (game.map_option("minimap_position_custom") == Some(1)).then(|| {
        ["minimap_position_x", "minimap_position_y"].map(|key| {
            (game.map_number(key).unwrap_or_default() as f32 / 10000.).clamp(0., 1.)
        })
    })
}

fn placement_span(viewport: [f32; 2], size: f32, footer: f32) -> [f32; 2] {
    [(viewport[0] - size).max(0.), (viewport[1] - size - footer).max(0.)]
}

fn placement_size(game: &Game<'_>, state: &State, viewport: [f32; 2])
    -> (runtime::minimap::Settings, f32, [f32; 2])
{
    let settings = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).settings();
    let size = fitted_minimap_size(game, settings, viewport[0], viewport[1]).unwrap_or(64.);
    let lines = minimap_lines(game, state, settings).len();
    let footer = if lines == 0 { 0. } else { lines as f32 * 12. * minimap_ui_scale(game) + 2. };
    (settings, size, placement_span(viewport, size, footer))
}

fn placement_buttons(viewport: [f32; 2]) -> [[f32; 4]; 3] {
    let width = ((viewport[0] - 20.) / 3.).clamp(1., 110.);
    std::array::from_fn(|i| {
        [(viewport[0] - 3. * width) * 0.5 + i as f32 * width + 2.,
            viewport[1] - 29., (width - 4.).max(1.), 22.]
    })
}

fn draw_placement(game: &Game<'_>, batch: &mut HudBatch, state: &State, edit: PlacementEdit) {
    let viewport = [batch.width, batch.height];
    let (settings, size, span) = placement_size(game, state, viewport);
    game.native_fill(batch, [0., 0., batch.width, batch.height], 0xb710171e);
    let rect = [edit.position[0] * span[0], edit.position[1] * span[1], size, size];
    draw(game, batch, state, rect, false, None);
    draw_minimap_frame(game, batch, rect, settings.round);
    let hint = tr("Drag the minimap or use the arrow keys", "Перетащи мини-карту или используй стрелки",
        "Déplacez la minicarte ou utilisez les flèches", "Arrastra el minimapa o usa las flechas",
        "Minikarte ziehen oder Pfeiltasten verwenden");
    game.map_text(batch, &fit_text(game, hint, batch.width - 16.), [8., 8.], 0xfff0f5f8, true);
    for (rect, label) in placement_buttons(viewport).into_iter().zip([
        tr("Save", "Сохранить", "Enregistrer", "Guardar", "Speichern"),
        tr("Use corner", "В угол", "Utiliser le coin", "Usar esquina", "Ecke verwenden"),
        tr("Cancel", "Отмена", "Annuler", "Cancelar", "Abbrechen"),
    ]) {
        button(game, batch, rect, &fit_text(game, label, rect[2] - 6.), game.cursor(), true);
    }
}

fn placement_press(game: &Game<'_>, state: &mut State, screen: &mod_api::ScreenView<'_>) {
    let viewport = [screen.rect[2], screen.rect[3]];
    let buttons = placement_buttons(viewport);
    if let Some(index) = buttons.iter().position(|rect| inside(screen.mouse, *rect)) {
        let edit = state.placement.take().unwrap();
        match index {
            0 => state.placement_change = Some(Some(edit.position)),
            1 => state.placement_change = Some(None),
            _ => {}
        }
        return;
    }
    let (_, size, span) = placement_size(game, state, viewport);
    let edit = state.placement.as_mut().unwrap();
    edit.span = span;
    let rect = [edit.position[0] * span[0], edit.position[1] * span[1], size, size];
    if !inside(screen.mouse, rect) {
        edit.move_by([screen.mouse[0] - rect[0] - size / 2., screen.mouse[1] - rect[1] - size / 2.]);
    }
}

fn settings_action(panel: [f32; 4]) -> [f32; 4] {
    let right = settings_back(panel)[0] - 5.;
    let left = (right - 157.).max(panel[0] + 75.);
    [left, panel[1] + panel[3] - 28., (right - left).max(1.), 20.]
}

fn draw_settings(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    if let Some(edit) = state.placement {
        draw_placement(game, batch, state, edit);
        return;
    }
    if let Some(editor) = &state.radar_editor {
        super::minimap_radar_editor::draw(game, batch, editor);
        return;
    }
    if state.settings_return==Some(Mode::Map) {
        draw_map_screen(game,batch,state);
    }
    let panel=settings_panel(batch.width,batch.height);
    let mouse=game.cursor();
    game.native_fill(batch,[0.,0.,batch.width,batch.height],0xd710171e);
    game.native_fill(batch,panel,0xff23303b);
    game.native_fill(batch,[panel[0],panel[1],panel[2],2.],0xff62d0c2);
    game.map_text(batch,tr("Map settings","Настройки карт","Paramètres des cartes","Ajustes del mapa","Karteneinstellungen"),
        [panel[0]+10.,panel[1]+10.],0xffeaf1f6,true);
    for (i,name) in setting_tabs().into_iter().enumerate() {
        let rect=settings_tab(panel,i);
        button(game,batch,rect,&fit_text(game,name,rect[2]-6.),mouse,true);
        if state.settings_tab==i {
            game.native_fill(batch,[rect[0],rect[1]+rect[3]-2.,rect[2],2.],0xff62d0c2);
        }
    }
    let total=settings_catalog(game,state.settings_tab).count();
    let count=settings_rows(panel);
    let start=state.settings_scroll.min(total.saturating_sub(count));
    for (i,setting) in settings_catalog(game,state.settings_tab).skip(start).take(count).enumerate() {
        use horizon_client::mod_list::NativeSettingValue as Value;
        let (row,control)=settings_row(panel,i);
        game.native_fill(batch,row,0xff293745);
        let label=game.translate(&setting.label).unwrap_or_else(||setting.key.replace('_'," "));
        game.map_text(batch,&fit_text(game,&label,control[0]-row[0]-12.),[row[0]+6.,row[1]+7.],0xffe2edf5,false);
        let value=setting_value(game,&setting.value);
        button(game,batch,control,&fit_text(game,&value,control[2]-10.),mouse,true);
        if matches!(setting.value,Value::Range {..}) {
            let filled=(control[2]-8.)*setting.value.fraction();
            game.native_fill(batch,[control[0]+4.,control[1]+18.,control[2]-8.,2.],0xff14202a);
            game.native_fill(batch,[control[0]+4.,control[1]+18.,filled,2.],0xff62d0c2);
        }
        if let Value::Color(rgb)=setting.value {
            game.native_fill(batch,[control[0]+5.,control[1]+6.,10.,10.],waypoint_argb(rgb));
        }
    }
    let shown = if total == 0 {0} else {start+1};
    game.map_text(batch,&format!("{shown}–{} / {total}",(start+count).min(total)),[panel[0]+10.,panel[1]+panel[3]-22.],0xffb9cbd8,false);
    if matches!(state.settings_tab, 0 | 3) {
        let rect = settings_action(panel);
        let label = if state.settings_tab == 0 {
            tr("Change position", "Положение на экране", "Changer la position",
                "Cambiar posición", "Position ändern")
        } else {
            tr("Radar categories", "Категории радара", "Catégories du radar",
                "Categorías de radar", "Radarkategorien")
        };
        button(game, batch, rect, &fit_text(game, label, rect[2] - 6.), mouse, true);
    }
    button(game,batch,settings_back(panel),tr("Back","Назад","Retour","Volver","Zurück"),mouse,true);
    draw_notice(game, batch, state);
}

fn open_settings(state:&mut State, back:Option<Mode>, tab:usize) {
    state.settings_return=back;
    state.placement=None;
    state.radar_editor=None;
    state.settings_tab=tab;
    state.settings_scroll=0;
    state.map_panel=MapPanel::None;
    state.waypoint_search_focus=false;
    state.server_world_focus=false;
    state.dragging=false;
    state.menu=None;
    state.mode=Mode::Settings;
    runtime::set_text_input(false);
}

fn close_settings(state:&mut State) {
    if let Some(mode)=state.settings_return.take() {state.mode=mode;} else {state.close=true;}
}

fn settings_press(game:&Game<'_>, state:&mut State, screen:&mod_api::ScreenView<'_>, button:i32) {
    if state.placement.is_some() {
        if button == 0 { placement_press(game, state, screen); }
        return;
    }
    if let Some(editor) = &mut state.radar_editor {
        use super::minimap_radar_editor::{press, ResultAction};
        match press(editor, screen, button) {
            ResultAction::Close => state.radar_editor = None,
            ResultAction::Saved => {
                state.map_settings_change = Some(("use_xaero_radar".to_owned(), "true".to_owned()));
                state.xaero_radar_enabled = None;
            }
            ResultAction::Stay => {}
        }
        return;
    }
    if !matches!(button,0|1) {return;}
    let panel=settings_panel(screen.rect[2],screen.rect[3]);
    let mouse=screen.mouse;
    if button==0 && inside(mouse,settings_back(panel)) {close_settings(state);return;}
    if button == 0 && state.settings_tab == 3 && inside(mouse, settings_action(panel)) {
        match super::minimap_radar_editor::Editor::open(
            &game.native_data_dir(runtime::MOD_ID), state.xaero_profile.as_deref(),
        ) {
            Ok(editor) => state.radar_editor = Some(editor),
            Err(error) => state.notice = Some((error.to_string(), Instant::now())),
        }
        return;
    }
    if button == 0 && state.settings_tab == 0 && inside(mouse, settings_action(panel)) {
        let viewport = [screen.rect[2], screen.rect[3]];
        let (settings, _, span) = placement_size(game, state, viewport);
        use runtime::minimap::Corner::*;
        let position = custom_minimap_position(game).unwrap_or(match settings.corner {
            TopLeft => [0., 0.], TopRight => [1., 0.],
            BottomLeft => [0., 1.], BottomRight => [1., 1.],
        });
        state.placement = Some(PlacementEdit { position, span });
        return;
    }
    if button==0 && let Some(i)=(0..SETTING_CATEGORIES).find(|i|inside(mouse,settings_tab(panel,*i))) {
        state.settings_tab=i;
        state.settings_scroll=0;
        return;
    }
    let total=settings_catalog(game,state.settings_tab).count();
    let count=settings_rows(panel);
    let start=state.settings_scroll.min(total.saturating_sub(count));
    let Some((i,row))=settings_catalog(game,state.settings_tab).skip(start).take(count)
        .enumerate().find(|(i,_)|inside(mouse,settings_row(panel,*i).0))
    else {return;};
    use horizon_client::mod_list::NativeSettingValue as Value;
    let (_,control)=settings_row(panel,i);
    let mut next=row.value.clone();
    match &mut next {
        Value::Range {..} => {
            if !inside(mouse,control) {return;}
            next.slide((mouse[0]-control[0]-4.)/(control[2]-8.).max(1.));
        }
        Value::Choice { options,index } if button==1 && !options.is_empty() => {
            *index=(*index+options.len()-1)%options.len();
        }
        _=>next.click(),
    }
    let value = match next {
        Value::Toggle(value)=>value.to_string(),
        Value::Choice {index,..}=>index.to_string(),
        Value::Range {value,..}=>value.to_string(),
        Value::Color(rgb)=>rgb.to_string(),
    };
    state.map_settings_change=Some((row.key.clone(),value));
}

fn draw_map_tools(game: &Game<'_>, batch: &mut HudBatch, state: &State, mouse: [f32;2]) {
    let labels = [
        tr("Waypoints","Метки","Repères","Marcadores","Wegpunkte"),
        tr("Center on me","К игроку","Recentrer","Centrar","Zu mir"),
        tr("Settings","Настройки","Paramètres","Ajustes","Einstellungen"),
        tr("Export PNG","Экспорт PNG","Exporter PNG","Exportar PNG","PNG-Export"),
        tr("Last death","Последняя смерть","Dernière mort","Última muerte","Letzter Tod"),
        tr("Server worlds", "Миры сервера", "Mondes du serveur", "Mundos del servidor", "Serverwelten"),
    ];
    for ((i, rect), label) in map_tools(batch.width, state).enumerate().zip(labels) {
        let active = i == 0 && state.map_panel == MapPanel::Waypoints
            || i == 5 && (state.map_panel == MapPanel::ServerWorlds || state.world.starts_with("subworld:"));
        if active {
            game.native_fill(batch, [rect[0], rect[1] - 2., rect[2], 2.], 0xff62d0c2);
        }
        button(game,batch,rect,&fit_text(game,label,rect[2]-6.),mouse,i != 4 || latest_death(state).is_some());
    }
    let panel = map_panel_rect(batch.width,batch.height);
    let title = match state.map_panel {
        MapPanel::None => return,
        MapPanel::Waypoints => labels[0],
        MapPanel::ServerWorlds => labels[5],
    };
    game.native_fill(batch,panel,0xf5212e39);
    game.native_fill(batch,[panel[0],panel[1],2.,panel[3]],0xff62d0c2);
    game.map_text(batch,title,[panel[0]+7.,panel[1]+7.],0xffe6f0f4,true);
    if state.map_panel == MapPanel::ServerWorlds {
        draw_server_worlds(game, batch, state, panel, mouse);
        return;
    }
    let [search, group, all, hidden] = map_panel_fields(panel);
    game.native_fill(batch, search, if state.waypoint_search_focus {0xff355464} else {0xff253744});
    let value = if state.waypoint_search.is_empty() {
        tr("Search waypoints…","Поиск метки…","Rechercher un repère…","Buscar marcador…","Wegpunkt suchen…")
    } else { &state.waypoint_search };
    game.map_text(batch, &fit_text(game, value, search[2]-10.), [search[0]+5., search[1]+5.], 0xffeaf1f6, false);
    let group_name = format!("{} ›", group_label(state.waypoint_group.as_deref()));
    for (rect, label, active) in [
        (group, group_name.as_str(), false),
        (all, tr("All groups","Все группы","Tous les groupes","Todos los grupos","Alle Gruppen"),
            game.map_option("all_waypoint_groups") == Some(1)),
        (hidden, tr("Show disabled","Показать скрытые","Voir les repères inactifs","Ver desactivados","Inaktive anzeigen"),
            state.map_panel_hidden),
    ] {
        button(game,batch,rect,&fit_text(game,label,rect[2]-6.),mouse,true);
        if active { game.native_fill(batch,[rect[0],rect[1]+rect[3]-2.,rect[2],2.],0xff62d0c2); }
    }
    let count = map_panel_rows(batch.height);
    let visible = visible_map_panel_waypoints(game, state, batch.height);
    for (i, waypoint) in visible.iter().enumerate() {
        let row=map_panel_row(panel,i);
        game.native_fill(batch,row,if inside(mouse,row) {0xff395469} else {0xff293c4e});
        game.native_fill(batch,[row[0]+4.,row[1]+5.,8.,8.],waypoint_argb(waypoint.color));
        let name=fit_text(game,&waypoint.name,row[2]-20.);
        game.map_text(batch,&name,[row[0]+17.,row[1]+5.],
            if waypoint.options.disabled {0xff9daab4} else {0xffeef5f8},true);
        if inside(mouse,row)
            && let Some(frame) = state.frame.as_ref().filter(|f| f.view.large && f.view.dimension == waypoint.dimension)
        {
            let rect = rectangle(batch.width,batch.height);
            let at = world_screen(&frame.view,rect,[f64::from(waypoint.x)+0.5,f64::from(waypoint.z)+0.5]);
            if inside(at,rect) && !inside(at,panel) {
                game.native_fill(batch,[at[0]-14.,at[1]-1.,6.,2.],0xff6ce1d5);
                game.native_fill(batch,[at[0]+8.,at[1]-1.,6.,2.],0xff6ce1d5);
            }
        }
    }
    if visible.is_empty() {
        game.map_text(batch,tr("No matching waypoints","Нет подходящих меток","Aucun repère trouvé",
            "No hay marcadores coincidentes","Keine passenden Wegpunkte"),
            [panel[0]+8.,panel[1]+83.],0xffb4c6d2,true);
    }
    let row = map_panel_row(panel,count);
    button(game,batch,row,tr("Manage waypoints…","Управление метками…","Gérer les repères…","Gestionar marcadores…","Wegpunkte verwalten…"),mouse,true);
}

fn server_world_rows(names: &BTreeSet<String>, skip: usize, take: usize) -> impl Iterator<Item = Option<&str>> {
    std::iter::once(None).chain(names.iter().map(|name| Some(name.as_str()))).skip(skip).take(take)
}

fn draw_server_worlds(game: &Game<'_>, batch: &mut HudBatch, state: &State, panel: [f32; 4], mouse: [f32; 2]) {
    let (names, active) = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).server_worlds(&state.base_world);
    let [name_field, create, _, _] = map_panel_fields(panel);
    let prompt = tr("New world name…", "Имя нового мира…", "Nom du nouveau monde…", "Nombre del nuevo mundo…", "Neuer Weltname…");
    let name = if state.server_world_name.is_empty() { prompt } else { &state.server_world_name };
    game.native_fill(batch, name_field, if state.server_world_focus { 0xff355464 } else { 0xff253744 });
    game.map_text(batch, &fit_text(game, name, name_field[2] - 10.), [name_field[0] + 5., name_field[1] + 5.], 0xffeaf1f6, false);
    button(game, batch, create, tr("Create", "Создать", "Créer", "Crear", "Anlegen"), mouse, !state.server_world_name.trim().is_empty());
    let default_label = tr("Default", "По умолчанию", "Par défaut", "Predeterminado", "Standard");
    for (i, name) in server_world_rows(&names, state.map_panel_scroll, map_panel_rows(batch.height)).enumerate() {
        let row = map_panel_row(panel, i);
        let active = name == active.as_deref();
        let text = fit_text(game, name.unwrap_or(default_label), row[2] - 14.);
        button(game, batch, row, &text, mouse, true);
        if active { game.native_fill(batch, [row[0], row[1], 3., row[3]], 0xff62d0c2); }
    }
}

fn select_map_server_world(game: &Game<'_>, state: &mut State, name: Option<&str>) {
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    if controller.select_server_world(&state.base_world, name, state.server_world_signal.as_ref()) {
        state.server_world_focus = false;
        state.server_world_name.clear();
        runtime::set_text_input(false);
    } else {
        state.notice = Some((tr("Could not select server world", "Не удалось выбрать мир сервера",
            "Impossible de choisir le monde du serveur", "No se pudo seleccionar el mundo del servidor",
            "Serverwelt konnte nicht gewählt werden").into(), Instant::now()));
    }
}

fn create_map_server_world(game: &Game<'_>, state: &mut State) {
    let name = state.server_world_name.trim().to_owned();
    if !name.is_empty() {
        select_map_server_world(game, state, Some(&name));
    }
}

fn map_tools_press(game: &Game<'_>, state: &mut State, screen: &mod_api::ScreenView<'_>, button: i32) -> bool {
    let [w,h] = [screen.rect[2],screen.rect[3]];
    let mouse = screen.mouse;
    if let Some(i) = map_tools(w, state).position(|rect| inside(mouse,rect)) {
        state.dragging = false;
        if button != 0 { return true; }
        state.server_world_focus = false;
        runtime::set_text_input(false);
        match i {
            0 => {
                state.map_panel.toggle(MapPanel::Waypoints);
                state.map_panel_scroll = 0;
                state.waypoint_search_focus = false;
                runtime::set_text_input(false);
            }
            1 => {
                if let Some(player) = game.native_player() {
                    let dimension = Dimension::from_id(player.dimension);
                    state.camera.snap(dimension.convert(state.selected,[player.x,player.z]));
                    state.elapsed=1.;
                }
            }
            2 => open_settings(state,Some(Mode::Map),1),
            3 => export_map_png(game,state,state.selected),
            4 => {
                if let Some(key) = latest_death(state).map(Waypoint::key)
                    && let Some(dimension) = focus_waypoint(state, key)
                {
                    runtime::initialize(game.native_data_dir(runtime::MOD_ID)).choose_dimension(true, dimension);
                }
            }
            5 => {
                state.map_panel.toggle(MapPanel::ServerWorlds);
                state.waypoint_search_focus = false;
                state.map_panel_scroll = 0;
            }
            _ => {}
        }
        return true;
    }
    if state.map_panel == MapPanel::None { return false; }
    let panel = map_panel_rect(w,h);
    if state.map_panel == MapPanel::ServerWorlds {
        let [name_field, create, _, _] = map_panel_fields(panel);
        state.server_world_focus = button == 0 && inside(mouse, name_field);
        runtime::set_text_input(state.server_world_focus);
        if !inside(mouse, panel) { return false; }
        state.dragging = false;
        if button == 0 {
            if inside(mouse, create) {
                create_map_server_world(game, state);
            } else {
                let (names, _) = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).server_worlds(&state.base_world);
                for (i, name) in server_world_rows(&names, state.map_panel_scroll, map_panel_rows(h)).enumerate() {
                    if inside(mouse, map_panel_row(panel, i)) {
                        select_map_server_world(game, state, name);
                        break;
                    }
                }
            }
        }
        return true;
    }
    let [search, group, all, hidden] = map_panel_fields(panel);
    state.waypoint_search_focus = button == 0 && inside(mouse, search);
    runtime::set_text_input(state.waypoint_search_focus);
    if !inside(mouse,panel) { return false; }
    state.dragging = false;
    if button != 0 || state.waypoint_search_focus { return true; }
    if inside(mouse, group) {
        next_waypoint_group(game, state);
    } else if inside(mouse, all) {
        let enabled = game.map_option("all_waypoint_groups") != Some(1);
        state.map_settings_change = Some(("all_waypoint_groups".to_owned(), enabled.to_string()));
        state.map_panel_scroll = 0;
    } else if inside(mouse, hidden) {
        state.map_panel_hidden = !state.map_panel_hidden;
        state.map_panel_scroll = 0;
    } else if inside(mouse, map_panel_row(panel, map_panel_rows(h))) {
        state.map_panel = MapPanel::None;
        state.mode = Mode::Waypoints;
        state.waypoint_scroll = 0;
        state.waypoint_bulk = None;
    } else {
        let key = visible_map_panel_waypoints(game, state, h).iter().enumerate()
            .find_map(|(i, point)| inside(mouse, map_panel_row(panel, i)).then(|| point.key()));
        if let Some(key) = key && let Some(dimension) = focus_waypoint(state, key) {
            runtime::initialize(game.native_data_dir(runtime::MOD_ID)).choose_dimension(true, dimension);
        }
    }
    true
}

fn map_footer(width: f32, height: f32) -> [f32; 4] {
    [
        8.0,
        (height - 47.0).max(0.0),
        (width - 16.0).max(1.0),
        39.0_f32.min(height),
    ]
}
fn world_zoom_buttons(width:f32) -> [[f32;4];2] {
    [[width-152.,36.,19.,18.],[width-130.,36.,19.,18.]]
}

fn map_zoom(game:&Game<'_>,state:&mut State,step:f64,offset:[f64;2]) {
    let min=if game.map_option("world_unlimited_zoom_in")==Some(1) {0.025}else{0.1};
    let max=if game.map_option("world_unlimited_zoom_out")==Some(1) {512.}else{64.};
    state.camera.zoom_at_limit(step,offset,min,max);
    state.elapsed=1.;
}

fn over_map_controls(game:&Game<'_>,state:&State,mouse: [f32; 2], width: f32, height: f32) -> bool {
    inside(mouse, cache_map_button(width))
        || map_tools(width, state).any(|rect| inside(mouse, rect))
        || state.map_panel != MapPanel::None && inside(mouse, map_panel_rect(width, height))
        || inside(mouse, map_footer(width, height))
        || (0..4).any(|i| inside(mouse, tab(width, i)))
        || game.map_option("world_zoom_buttons")==Some(1) && world_zoom_buttons(width).into_iter().any(|rect| inside(mouse,rect))
}
fn draw_map_screen(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    let mouse = game.cursor();
    let rect = rectangle(batch.width, batch.height);
    let first_vertex=batch.vertices.len();
    draw(game,batch,state,rect,true,Some(mouse));
    if game.map_option("map_open_animation")==Some(1)
        && let Some(since)=state.map_opened {
        let percent=(since.elapsed().as_millis()*100/180).min(100) as u8;
        set_opacity(batch,first_vertex,percent);
    }
    let mut coordinates = tr(
        "Cursor: —",
        "Курсор: —",
        "Curseur : —",
        "Cursor: —",
        "Mauszeiger: —",
    )
    .to_owned();
    let mut nether = tr(
        "Nether: —",
        "Незер: —",
        "Nether : —",
        "Nether: —",
        "Nether: —",
    )
    .to_owned();
    let mut biome = None;
    let mut lod_warning = false;
    let mut tooltip = None;
    if !over_map_controls(game,state,mouse, batch.width, batch.height)
        && let Some(frame) = state.frame.as_ref().filter(|f| {
            f.view.dimension == state.selected && f.view.large && f.view.session == state.session
        })
        && let Some(block) = hovered_block(&frame.view, rect, mouse)
    {
        let column = if state.dimension == frame.view.dimension {
            hovered_column(game, frame.view.layer, block)
        } else {
            match frame.view.layer {
                Layer::Below(maps::FULL_CAVE) => None,
                Layer::Below(y) => Some(ColumnInfo {
                    height: y,
                    fluid: None,
                }),
                Layer::Surface => None,
            }
        };
        let coverage = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
            .maps
            .coverage();
        let rough = if coverage.detailed_at(frame.view.dimension, frame.view.layer, block) {
            None
        } else {
            coverage.lod_sample(frame.view.dimension, frame.view.layer, block)
        };
        let detailed = coverage.detailed_biome(frame.view.dimension, frame.view.layer, block);
        let height_approximate = column.is_none() && rough.is_some();
        let height = column
            .map(|column| column.height)
            .or_else(|| {
                (game.map_option("world_detect_ambiguous_y") == Some(0))
                    .then(|| coverage.detailed_height(frame.view.dimension, frame.view.layer, block))
                    .flatten()
            })
            .or_else(|| rough.as_ref().map(|p| p.height));
        let bottom = column.and_then(|column| column.fluid.map(|fluid| fluid.bottom));
        coordinates = match height {
            Some(y) => {
                let bottom = bottom.map_or_else(String::new, |bottom| {
                    localized_format!(
                        " (bottom {})",
                        " (дно {})",
                        " (fond {})",
                        " (fondo {})",
                        " (Grund {})",
                        bottom
                    )
                });
                format!(
                    "{}: X {}  Y {}{}{}  Z {}",
                    frame.view.dimension.label(),
                    block[0],
                    if height_approximate { "≈" } else { "" },
                    y,
                    bottom,
                    block[1]
                )
            }
            None => format!(
                "{}: X {}  Z {}",
                frame.view.dimension.label(),
                block[0],
                block[1]
            ),
        };
        if let Some([x, _, z]) = runtime::portal_coordinates(
            frame.view.dimension,
            Dimension::Nether,
            [f64::from(block[0]), 0.0, f64::from(block[1])],
        ) {
            nether = localized_format!(
                "Nether: X {x}  Z {z}",
                "Незер: X {x}  Z {z}",
                "Nether : X {x}  Z {z}",
                "Nether: X {x}  Z {z}",
                "Nether: X {x}  Z {z}"
            );
        }
        if state.dimension == frame.view.dimension {
            if let Some(y) = height.or_else(|| game.native_player().map(|p| p.y.floor() as i32)) {
                biome = known_biome(game, state.dimension, [block[0], y, block[1]]);
            }
        }
        if biome.is_none() {
            biome = detailed
                .as_ref()
                .or_else(|| rough.as_ref().and_then(|sample| sample.biome.as_ref()))
                .map(|name| name.to_string());
        }
        lod_warning = !coverage.detailed_at(frame.view.dimension, frame.view.layer, block)
            && coverage
                .lod_at(frame.view.dimension, frame.view.layer, block)
                .is_some();
        if let Some(waypoint)=waypoint_at_pointer(game,state,&frame.view,rect,mouse) {
            tooltip=Some(if game.map_option("waypoint_hide_coords")==Some(1) {waypoint.name.clone()}
                else {format!("{}  ({} {} {})",waypoint.name,waypoint.x,waypoint.y,waypoint.z)});
        }
    }
    let settings = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).settings();
    game.native_fill(batch, [6.,6.,tab(batch.width,3)[0]+tab(batch.width,3)[2]-4.,28.], 0xec101922);
    for (i, dimension) in Dimension::ALL.into_iter().enumerate() {
        let tab = tab(batch.width, i);
        button(game, batch, tab, "", mouse, true);
        if dimension == settings.world_dimension {
            game.native_fill(batch, tab, 0xe032624b);
        }
        dimension_icon(game, batch, dimension, [tab[0] + 4.0, tab[1] + 4.0]);
        game.map_text(
            batch,
            &fit_text(game, dimension.label(), tab[2] - 28.),
            [tab[0] + 24.0, tab[1] + 8.0],
            0xffffff,
            true,
        );
    }
    let footer = map_footer(batch.width, batch.height);
    game.native_fill(batch, footer, 0xdf10151d);
    if game.map_option("map_show_coordinates")!=Some(0) {
        let coordinates=fit_text(game,&format!("{coordinates}   {nether}"),footer[2]-12.);
        game.map_text(batch,&coordinates,[footer[0]+6.,footer[1]+5.],0xffffff,true);
    }
    let show_biome=game.map_option("world_hover_biome")!=Some(0);
    if show_biome {biome_icon(game,batch,biome.as_deref(),[footer[0]+6.,footer[1]+18.]);}
    let mut label = if show_biome {biome.as_deref().map_or_else(
        || tr("Biome: —", "биом: —", "Biome : —", "Bioma: —", "Biom: —").to_owned(),
        localized_biome,
    )} else {String::new()};
    if game.map_option("world_zoom_readout")==Some(1) {
        if !label.is_empty(){label.push_str(" · ");}
        label.push_str(&format!("{:.2}×",1./state.camera.zoom));
    }
    if game.map_option("world_cave_ceiling")==Some(1)
        && let Some(Layer::Below(y))=state.frame.as_ref().map(|frame|frame.view.layer) {
        if !label.is_empty() {label.push_str(" · ");}
        if y == maps::FULL_CAVE {
            label.push_str(tr("Full caves", "Общий слой пещер", "Toutes les grottes", "Todas las cuevas", "Alle Höhlen"));
        } else {
            label.push_str(&format!("Y ≤ {y}"));
        }
    }
    if lod_warning {
        label.push_str(tr(
            " | LOD — approximate",
            " | LOD — приблизительно",
            " | LOD — approximatif",
            " | LOD — aproximado",
            " | LOD — ungefähr",
        ));
    }
    game.map_text(
        batch,
        &label,
        [footer[0] + 26.0, footer[1] + 21.0],
        if lod_warning { 0xffffd45c } else { 0xffffff },
        true,
    );
    if let Some(tooltip) = tooltip
        && state.info.is_none()
    {
        draw_tooltip(game, batch, &tooltip, mouse);
    }
    if game.map_option("world_zoom_buttons")==Some(1) && state.menu.is_none() && state.info.is_none() {
        for (rect,label) in world_zoom_buttons(batch.width).into_iter().zip(["−","+"]) {
            button(game,batch,rect,label,mouse,true);
        }
    }
    if state.info.is_none() && state.menu.is_none() {
        draw_map_tools(game,batch,state,mouse);
    }
    draw_menu(game, batch, state);
    draw_notice(game, batch, state);
    if state.info.is_none() && state.menu.is_none() {
        let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
        let attention = controller.maps.needs_upgrade(&state.world) || state.caches.attention;
        button(
            game,
            batch,
            cache_map_button(batch.width),
            if attention {
                tr(
                    "Update cache…",
                    "Обновить кеш…",
                    "Actualiser cache…",
                    "Actualizar caché…",
                    "Cache aktualisieren…",
                )
            } else {
                tr(
                    "Map caches…",
                    "Кеши карт…",
                    "Caches…",
                    "Cachés…",
                    "Karten-Caches…",
                )
            },
            mouse,
            true,
        );
    }
    if let Some(info) = state.info {
        draw_information(game, batch, state, info);
    }
}

fn cache_map_button(width: f32) -> [f32; 4] {
    [(width - 106.).max(6.), 36., 100., 18.]
}
fn cache_rows(height: f32) -> usize {
    (((height - 122.).max(0.) / 48.) as usize).clamp(1, 16)
}
fn cache_row(width: f32, index: usize) -> [f32; 4] {
    [12., 42. + index as f32 * 48., (width - 24.).max(1.), 46.]
}
fn cache_footer(width: f32, height: f32, index: usize) -> [f32; 4] {
    let w = ((width - 32.) / 3.).max(1.);
    [12. + index as f32 * (w + 4.), height - 55., w, 18.]
}
fn cache_status(status: &runtime::container::Compatibility) -> &'static str {
    use runtime::container::Compatibility::*;
    match status {
        Current => tr(
            "Compatible",
            "Совместим",
            "Compatible",
            "Compatible",
            "Kompatibel",
        ),
        RebuildImages => tr(
            "Update available: rebuild map images",
            "Доступно обновление: перестройте изображения карты",
            "Mise à jour : reconstruire les images",
            "Actualización: reconstruir imágenes",
            "Update: Kartenbilder neu aufbauen",
        ),
        Older(_) => tr(
            "Old format: conversion is not available yet",
            "Старый формат: конвертация пока недоступна",
            "Ancien format : conversion indisponible",
            "Formato antiguo: conversión no disponible",
            "Altes Format: Konvertierung noch nicht verfügbar",
        ),
        Newer(_) => tr(
            "Requires a newer mod version",
            "Нужна более новая версия мода",
            "Version plus récente du mod requise",
            "Requiere una versión más reciente del mod",
            "Neuere Mod-Version erforderlich",
        ),
        Damaged => tr(
            "Header unreadable; file preserved",
            "Заголовок не читается; файл сохранён",
            "En-tête illisible ; fichier conservé",
            "Cabecera ilegible; archivo conservado",
            "Header unlesbar; Datei erhalten",
        ),
    }
}
fn cache_text(game: &Game<'_>, value: &str, width: f32) -> String {
    let mut text: String = value
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    if game.map_text_width(&text) <= width {
        return text;
    }
    while !text.is_empty() && game.map_text_width(&format!("{text}…")) > width {
        text.pop();
    }
    format!("{text}…")
}
fn cache_icon(bytes: &[u8]) -> Option<Arc<Vec<u8>>> {
    // Validate size before the PNG decoder allocates its output.
    if bytes.len() > 1024 * 1024
        || bytes.len() < 24
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || &bytes[12..16] != b"IHDR"
        || u32::from_be_bytes(bytes[16..20].try_into().ok()?) != 64
        || u32::from_be_bytes(bytes[20..24].try_into().ok()?) != 64
    {
        return None;
    }
    horizon_client::hud::decode_bytes(bytes)
        .ok()
        .filter(|(w, h, p)| *w == 64 && *h == 64 && p.len() == 64 * 64 * 4)
        .map(|(_, _, p)| Arc::new(p))
}
fn refresh_cache_catalog(game: &Game<'_>) {
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    controller.caches.refresh(|rows| {
        use std::io::Read;
        let saves = std::env::current_dir()?.join("saves");
        let (servers, hidden) =
            horizon_protocol::servers::load_all(&horizon_client::multiplayer::servers_file(&saves));
        let servers: Vec<_> = servers.into_iter().chain(hidden).collect();
        for row in rows {
            let Some(world) = row.world.as_deref() else {
                continue;
            };
            if let Some(folder) = world.strip_prefix("save:") {
                let mut parts = std::path::Path::new(folder).components();
                if !matches!(parts.next(), Some(std::path::Component::Normal(_)))
                    || parts.next().is_some()
                    || folder.contains('\\')
                {
                    continue;
                }
                let root = saves.join(folder);
                if let Some(level) = horizon_client::save::Level::read_summary(&root) {
                    row.name = level.name;
                }
                if let Ok(file) = std::fs::File::open(root.join("icon.png")) {
                    let mut bytes = Vec::new();
                    if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_ok() {
                        row.icon = cache_icon(&bytes);
                    }
                }
            } else {
                let scoped = runtime::server_subworld_parts(world);
                let address = scoped.as_ref().and_then(|(base, _)| base.strip_prefix("server:"))
                    .or_else(|| world.strip_prefix("server:"));
                if let Some(address) = address {
                    let server = servers.iter().find(|s| s.ip == address);
                    let title = server.map(|s| s.name.as_str()).filter(|s| !s.is_empty()).unwrap_or(address);
                    row.name = scoped.as_ref().map_or_else(|| title.to_owned(), |(_, name)| format!("{title} / {name}"));
                    row.icon = server.and_then(|s| s.icon.as_deref())
                        .filter(|s| s.len() <= 1400000)
                        .and_then(horizon_client::skins::base64_decode)
                        .and_then(|png| cache_icon(&png));
                }
            }
        }
        Ok(())
    });
}
fn show_cache_manager(game: &Game<'_>, state: &mut State) {
    state.mode = Mode::Caches;
    state.menu = None;
    state.info = None;
    state.name_form = None;
    runtime::set_text_input(false);
    refresh_cache_catalog(game);
}
pub(super) fn open_cache_manager(game: &mut Game<'_>) {
    if game.native_player().is_none() || game.title.is_some() {
        return;
    }
    if game.screen == super::Screen::Options {
        game.settings.save(&Game::options_path());
        game.screen = super::Screen::Playing;
    }
    if game.mod_screen_open(runtime::MOD_ID) {
        show_cache_manager(game, &mut state());
    }
}
fn update_cache_ui(game: &Game<'_>, state: &mut State) {
    let snapshot = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
        .caches
        .snapshot();
    if state.caches.catalog_revision != snapshot.revision {
        state.caches.catalog_revision = snapshot.revision;
        let file = runtime::container::world_path(std::path::Path::new(""), &state.world);
        state.caches.attention = snapshot.entries.iter().any(|e| {
            (e.world.as_ref() == Some(&state.world)
                || file.file_name().is_some_and(|f| f == e.file.as_str()))
                && e.compatibility != runtime::container::Compatibility::Current
        });
    }
    if state.mode != Mode::Caches {
        return;
    }
    if let Some(rx) = &state.caches.operation {
        match rx.try_recv() {
            Ok(result) => {
                state.caches.operation = None;
                state.caches.message = Some(match result {
                    Ok(()) => tr(
                        "Rebuild scheduled; images update when this cache is viewed",
                        "Перестроение назначено; изображения обновятся при просмотре кеша",
                        "Reconstruction planifiée à la prochaine consultation",
                        "Reconstrucción programada al consultar la caché",
                        "Neuaufbau beim Anzeigen geplant",
                    )
                    .into(),
                    Err(error) => error,
                });
                refresh_cache_catalog(game);
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                state.caches.operation = None;
                state.caches.message = Some(
                    tr(
                        "Cache worker stopped",
                        "Поток кеша остановлен",
                        "Gestion du cache arrêtée",
                        "Gestor detenido",
                        "Cache-Verarbeitung beendet",
                    )
                    .into(),
                );
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    if state.caches.revision != snapshot.revision {
        state.caches.revision = snapshot.revision;
        state.caches.confirm = None;
        state.caches.icons.clear();
        if state
            .caches
            .selected
            .as_ref()
            .is_some_and(|name| !snapshot.entries.iter().any(|e| e.file == *name))
        {
            state.caches.selected = None;
        }
    }
    let height = game.map_client_state().map_or(480., |s| s.gui_size[1]);
    let start = state
        .caches
        .scroll
        .min(snapshot.entries.len().saturating_sub(cache_rows(height)));
    let visible: BTreeSet<_> = snapshot
        .entries
        .iter()
        .skip(start)
        .take(cache_rows(height))
        .map(|e| e.file.as_str())
        .collect();
    state
        .caches
        .icons
        .retain(|file, _| visible.contains(file.as_str()));
    for entry in snapshot.entries.iter().skip(start).take(cache_rows(height)) {
        if let Some(pixels) = &entry.icon {
            state
                .caches
                .icons
                .entry(entry.file.clone())
                .or_insert_with(|| super::minimap_pages::create(64, 64, (**pixels).clone()));
        }
    }
}
fn draw_cache_screen(game: &Game<'_>, batch: &mut HudBatch, state: &State) {
    let snapshot = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
        .caches
        .snapshot();
    game.native_fill(batch, [0., 0., batch.width, batch.height], 0xf0181b20);
    game.map_text(
        batch,
        tr(
            "Map caches",
            "Кеши карт",
            "Caches des cartes",
            "Cachés de mapas",
            "Karten-Caches",
        ),
        [12., 14.],
        0xffffff,
        true,
    );
    button(
        game,
        batch,
        [batch.width - 92., 10., 80., 18.],
        tr(
            "Refresh",
            "Обновить",
            "Actualiser",
            "Actualizar",
            "Aktualisieren",
        ),
        game.cursor(),
        !snapshot.loading && state.caches.operation.is_none(),
    );
    let start = state.caches.scroll.min(
        snapshot
            .entries
            .len()
            .saturating_sub(cache_rows(batch.height)),
    );
    for (i, entry) in snapshot
        .entries
        .iter()
        .skip(start)
        .take(cache_rows(batch.height))
        .enumerate()
    {
        let row = cache_row(batch.width, i);
        game.native_fill(
            batch,
            row,
            if state.caches.selected.as_ref() == Some(&entry.file) {
                0xff334658
            } else {
                0xff232c35
            },
        );
        let server = entry.world.as_deref().is_some_and(|world| world.starts_with("server:") || world.starts_with("subworld:"));
        if let Some(icon) = state.caches.icons.get(&entry.file) {
            let x = f64::from(row[0] + 6.);
            let y = f64::from(row[1] + 6.);
            super::minimap_pages::paint(
                batch,
                icon,
                &[
                    [x, y, 0., 0.],
                    [x + 32., y, 64., 0.],
                    [x + 32., y + 32., 64., 64.],
                    [x, y + 32., 0., 64.],
                ],
            );
        } else {
            game.native_fill(batch, [row[0] + 6., row[1] + 6., 32., 32.], 0xff506846);
            game.map_text(
                batch,
                if server { "S" } else { "W" },
                [row[0] + 18., row[1] + 18.],
                0xffffff,
                true,
            );
        }
        let mask = game.map_option("hide_world_names").unwrap_or(0);
        let hidden = mask >= 2 || (mask == 1 && server);
        let label = if hidden {
            if server { tr("Server", "Сервер", "Serveur", "Servidor", "Server") }
            else { tr("World", "Мир", "Monde", "Mundo", "Welt") }
        } else { &entry.name };
        let title = cache_text(game, label, row[2] - 52.);
        game.map_text(batch, &title, [row[0] + 44., row[1] + 4.], 0xffffff, false);
        let size = entry.bytes as f64 / 1048576.;
        let description = if hidden { format!("{size:.2} MiB") } else { format!("{size:.2} MiB · {}", entry.file) };
        let detail = cache_text(game, &description, row[2] - 52.);
        game.map_text(
            batch,
            &detail,
            [row[0] + 44., row[1] + 17.],
            0xbac4d0,
            false,
        );
        let label = cache_text(game, cache_status(&entry.compatibility), row[2] - 52.);
        game.map_text(
            batch,
            &label,
            [row[0] + 44., row[1] + 30.],
            if entry.compatibility == runtime::container::Compatibility::Current {
                0x9fc792
            } else {
                0xffd24a
            },
            false,
        );
    }
    if snapshot.entries.is_empty() {
        game.map_text(
            batch,
            tr(
                "No map cache files",
                "Файлов кеша карт нет",
                "Aucun cache",
                "No hay cachés",
                "Keine Karten-Caches",
            ),
            [12., 46.],
            0xbac4d0,
            false,
        );
    }
    let busy = snapshot.loading || state.caches.operation.is_some();
    let selected = snapshot
        .entries
        .iter()
        .find(|e| Some(&e.file) == state.caches.selected.as_ref());
    if let Some(entry) = selected {
        let identity = cache_text(
            game,
            &format!(
                "{} · {}",
                entry.name,
                entry.world.as_deref().unwrap_or(&entry.file)
            ),
            batch.width - 24.,
        );
        game.map_text(batch, &identity, [12., batch.height - 73.], 0xbac4d0, false);
    }
    let can_rebuild = selected.is_some_and(|e| {
        e.world.is_some()
            && matches!(
                e.compatibility,
                runtime::container::Compatibility::Current
                    | runtime::container::Compatibility::RebuildImages
            )
    });
    button(
        game,
        batch,
        cache_footer(batch.width, batch.height, 0),
        if state.caches.confirm.is_some() {
            tr(
                "Confirm rebuild",
                "Подтвердить",
                "Confirmer",
                "Confirmar",
                "Bestätigen",
            )
        } else {
            tr(
                "Rebuild images",
                "Перестроить",
                "Reconstruire",
                "Reconstruir",
                "Neu aufbauen",
            )
        },
        game.cursor(),
        can_rebuild && !busy,
    );
    button(
        game,
        batch,
        cache_footer(batch.width, batch.height, 1),
        tr(
            "Compress: later",
            "Сжать: позже",
            "Compresser : plus tard",
            "Comprimir: después",
            "Komprimieren: später",
        ),
        game.cursor(),
        false,
    );
    button(
        game,
        batch,
        cache_footer(batch.width, batch.height, 2),
        tr("Back to map", "К карте", "Carte", "Mapa", "Zur Karte"),
        game.cursor(),
        true,
    );
    let message = state
        .caches
        .message
        .as_deref()
        .or(snapshot.error.as_deref())
        .unwrap_or(if busy {
            tr(
                "Working…",
                "Обработка…",
                "En cours…",
                "Procesando…",
                "Verarbeitung…",
            )
        } else if state.caches.confirm.is_some() {
            tr(
                "Only images are rebuilt; saved terrain is retained",
                "Перестроятся только изображения; исходная карта сохранится",
                "Seules les images changent, le terrain est conservé",
                "Solo se reconstruyen imágenes; se conserva el terreno",
                "Nur Bilder werden neu aufgebaut; Gelände bleibt erhalten",
            )
        } else {
            tr(
                "Select a cache. Compression and file conversion are not available yet",
                "Выберите кеш. Сжатие и конвертация файлов пока недоступны",
                "Sélectionnez un cache. Compression et conversion à venir",
                "Selecciona una caché. Compresión y conversión pendientes",
                "Cache auswählen. Komprimierung und Konvertierung folgen",
            )
        });
    let message = cache_text(game, message, batch.width - 24.);
    game.map_text(batch, &message, [12., batch.height - 29.], 0xffd24a, false);
    if snapshot.truncated {
        game.map_text(batch, "4096+", [12., batch.height - 15.], 0xffd24a, false);
    }
}
fn cache_press(
    game: &Game<'_>,
    state: &mut State,
    screen: &mod_api::ScreenView<'_>,
    button_id: i32,
    canceled: &mut bool,
) {
    *canceled = true;
    if button_id != 0 {
        return;
    }
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    let snapshot = controller.caches.snapshot();
    if inside(
        screen.mouse,
        cache_footer(screen.rect[2], screen.rect[3], 2),
    ) {
        state.mode = Mode::Map;
        state.caches.confirm = None;
        return;
    }
    if snapshot.loading || state.caches.operation.is_some() {
        return;
    }
    if inside(screen.mouse, [screen.rect[2] - 92., 10., 80., 18.]) {
        state.caches.confirm = None;
        refresh_cache_catalog(game);
        return;
    }
    if inside(
        screen.mouse,
        cache_footer(screen.rect[2], screen.rect[3], 0),
    ) {
        if let Some(entry) = snapshot
            .entries
            .iter()
            .find(|e| Some(&e.file) == state.caches.selected.as_ref())
            && matches!(
                entry.compatibility,
                runtime::container::Compatibility::Current
                    | runtime::container::Compatibility::RebuildImages
            )
            && let Some(world) = entry.world.as_deref()
        {
            if state.caches.confirm.as_deref() == Some(&entry.file) {
                state.caches.confirm = None;
                match controller.maps.rebuild_images(world, &entry.file) {
                    Ok(rx) => {
                        state.caches.operation = Some(rx);
                        state.caches.message = None;
                    }
                    Err(error) => state.caches.message = Some(error),
                }
            } else {
                state.caches.confirm = Some(entry.file.clone());
                state.caches.message = None;
            }
        }
        return;
    }
    let start = state.caches.scroll.min(
        snapshot
            .entries
            .len()
            .saturating_sub(cache_rows(screen.rect[3])),
    );
    for (i, entry) in snapshot
        .entries
        .iter()
        .skip(start)
        .take(cache_rows(screen.rect[3]))
        .enumerate()
    {
        if inside(screen.mouse, cache_row(screen.rect[2], i)) {
            state.caches.selected = Some(entry.file.clone());
            state.caches.confirm = None;
            state.caches.message = None;
            break;
        }
    }
}

// Only the map and its name prompt cover the world. Other screens keep
// Horizon's normal rendering and overlay priorities.
fn map_covers_world(state: &State) -> bool {
    state.mode == Mode::Map
        || (state.mode == Mode::Name
            && state
                .name_form
                .as_ref()
                .is_some_and(|form| form.background == Mode::Map))
}

pub(super) fn transparent_waypoints(game: &Game<'_>) -> bool {
    if !game.map_on(runtime::MOD_ID) || game.mod_screen() != Some(runtime::MOD_ID) {
        return false;
    }
    let state = state();
    state.mode == Mode::Waypoints
        || (state.mode == Mode::Name
            && state
                .name_form
                .as_ref()
                .is_some_and(|form| form.background == Mode::Waypoints))
}

pub(super) fn covers_world(game: &Game<'_>) -> bool {
    game.map_on(runtime::MOD_ID)
        && game.mod_screen() == Some(runtime::MOD_ID)
        && game.native_player().is_some()
        && map_covers_world(&state())
}

pub(super) fn screen(game: &Game<'_>, batch: &mut HudBatch) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let state = state();
    match state.mode {
        Mode::Map => draw_map_screen(game, batch, &state),
        Mode::Waypoints => draw_waypoints_screen(game, batch, &state),
        Mode::Settings => draw_settings(game,batch,&state),
        Mode::Name => {
            if let Some(form) = state.name_form.as_ref() {
                draw_name_form(game, batch, &state, form);
            }
        }
        Mode::Caches => draw_cache_screen(game, batch, &state),
    }
}

pub(super) fn press(
    game: &Game<'_>,
    screen: &mod_api::ScreenView<'_>,
    button: i32,
    canceled: &mut bool,
) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let chosen = game.map_key(
        runtime::MOD_ID,
        runtime::WORLD_MAP_KEY,
        runtime::WORLD_MAP_DEFAULT,
    );
    let mouse = match button {
        0 => "key.mouse.left",
        1 => "key.mouse.right",
        2 => "key.mouse.middle",
        _ => "",
    };
    if chosen == mouse {
        state().close = true;
        *canceled = true;
        return;
    }
    let mut state = state();
    if state.info.is_some() {
        if button == 0 {
            let rect = information_rect(screen.rect[2], screen.rect[3]);
            let close = [
                rect[0] + rect[2] - 82.0,
                rect[1] + rect[3] - 24.0,
                72.0,
                18.0,
            ];
            if !inside(screen.mouse, rect) || inside(screen.mouse, close) {
                state.info = None;
            }
        }
        *canceled = true;
        return;
    }
    match state.mode {
        Mode::Map => map_press(game, &mut state, screen, button, canceled),
        Mode::Waypoints => waypoint_press(game, &mut state, screen, button, canceled),
        Mode::Settings => { settings_press(game,&mut state,screen,button); *canceled=true; }
        Mode::Name => name_press(game, &mut state, screen, button, canceled),
        Mode::Caches => cache_press(game, &mut state, screen, button, canceled),
    }
}

fn map_press(
    game: &Game<'_>,
    state: &mut State,
    screen: &mod_api::ScreenView<'_>,
    button: i32,
    canceled: &mut bool,
) {
    if inside(screen.mouse, cache_map_button(screen.rect[2])) {
        if button == 0 {
            show_cache_manager(game, state);
        }
        *canceled = true;
        return;
    }
    if state.menu.is_none() && button==0 && game.map_option("world_zoom_buttons")==Some(1) {
        for (rect,step) in world_zoom_buttons(screen.rect[2]).into_iter().zip([-1.,1.]) {
            if inside(screen.mouse,rect) {
                map_zoom(game,state,step,[0.,0.]);
                *canceled=true;
                return;
            }
        }
    }
    if state.menu.is_none() && map_tools_press(game,state,screen,button) {
        *canceled = true;
        return;
    }
    if state.menu.is_none() && over_map_controls(game,state,screen.mouse, screen.rect[2], screen.rect[3]) {
        if button == 0 {
            for (i, dimension) in Dimension::ALL.into_iter().enumerate() {
                if inside(screen.mouse, tab(screen.rect[2], i)) {
                    runtime::initialize(game.native_data_dir(runtime::MOD_ID))
                        .choose_dimension(true, dimension);
                    break;
                }
            }
        }
        state.menu = None;
        *canceled = true;
        return;
    }
    if button == 1 {
        let menu = menu_target_at(game, state, screen, screen.mouse);
        state.menu = menu;
        *canceled = true;
        return;
    }
    if button != 0 {
        return;
    }
    let width = screen.rect[2];
    state.dragging = false;
    if let Some(menu) = state.menu {
        let Some(player) = game.native_player() else {
            state.menu = None;
            return;
        };
        let subtitle = menu_subtitle(game,state,menu,&player);
        let target = portal_target(state, menu, player.y.floor() as i32);
        let show_3d = menu
            .waypoint
            .map_or(true, |spot| waypoint_show_3d(state, spot));
        let items = menu_items(game, menu, target, show_3d);
        let (rect, rows) = menu_layout(
            game,
            [screen.rect[2], screen.rect[3]],
            menu,
            subtitle.as_deref(),
            &items,
        );
        if inside(screen.mouse, rect) {
            for ((_, action), row) in items.iter().zip(rows) {
                if inside(screen.mouse, row) {
                    perform_menu(game, state, menu, *action);
                    *canceled = true;
                    return;
                }
            }
            *canceled = true;
            return;
        }
        state.menu = None;
    }
    let portal_click = state
        .frame
        .as_ref()
        .filter(|frame| {
            frame.view.dimension == state.selected
                && frame.view.large
                && frame.view.session == state.session
        })
        .and_then(|frame| {
            hovered_block(&frame.view, rectangle(width, screen.rect[3]), screen.mouse)
        })
        .is_some_and(|block| portal_near(state, state.selected, block).is_some());
    if portal_click {
        let menu = menu_target_at(game, state, screen, screen.mouse);
        state.menu = menu;
        *canceled = true;
        return;
    }
    for (i, dimension) in Dimension::ALL.into_iter().enumerate() {
        if inside(screen.mouse, tab(width, i)) {
            runtime::initialize(game.native_data_dir(runtime::MOD_ID))
                .choose_dimension(true, dimension);
            *canceled = true;
            return;
        }
    }
    if inside(screen.mouse, rectangle(width, screen.rect[3])) {
        state.dragging = true;
        *canceled = true;
    }
}

fn menu_target_at(
    game: &Game<'_>,
    state: &State,
    screen: &mod_api::ScreenView<'_>,
    at: [f32; 2],
) -> Option<Menu> {
    let rect = rectangle(screen.rect[2], screen.rect[3]);
    let frame = state.frame.as_ref().filter(|frame| {
        frame.view.dimension == state.selected
            && frame.view.large
            && frame.view.session == state.session
    })?;
    let block = hovered_block(&frame.view, rect, at)?;
    let portal = portal_near(state, state.selected, block);
    let column = if state.dimension == frame.view.dimension {
        hovered_column(game, frame.view.layer, block)
    } else {
        match frame.view.layer {
            Layer::Below(maps::FULL_CAVE) => None,
            Layer::Below(y) => Some(ColumnInfo {
                height: y,
                fluid: None,
            }),
            Layer::Surface => None,
        }
    };
    let height = if let Some((_, at)) = portal {
        Some(at[1])
    } else {
        column.map(|column| column.height)
    };
    let fluid = if portal.is_some() {
        None
    } else {
        column.and_then(|column| column.fluid)
    };
    let coverage = runtime::initialize(game.native_data_dir(runtime::MOD_ID)).maps.coverage();
    let height = height.or_else(|| {
        (game.map_option("world_detect_ambiguous_y") == Some(0))
            .then(|| coverage.detailed_height(state.selected, frame.view.layer, block))
            .flatten()
    });
    let rough = coverage.lod_sample(state.selected, frame.view.layer, block);
    let height_approximate = height.is_none() && rough.is_some();
    let height = height.or_else(|| rough.map(|p| p.height));
    let radar_player = ::trailgrid_maps::radar::snapshot()
        .filter(|radar| radar.world_map && radar.dimension == frame.view.dimension.id())
        .and_then(|radar| radar.dots.iter().filter_map(|dot| {
            game.remote_player_name(dot.entity_id)?;
            let point = world_screen(&frame.view, rect, [dot.at[0], dot.at[2]]);
            let distance = ((point[0] - at[0]).powi(2) + (point[1] - at[1]).powi(2)).sqrt();
            (distance <= 9.).then_some((distance, dot.entity_id))
        }).min_by(|a,b| a.0.total_cmp(&b.0)).map(|(_,id)| id));
    Some(Menu {
        height_approximate,
        block,
        height,
        fluid,
        dimension: state.selected,
        at,
        portal: portal.map(|(kind, _)| kind),
        radar_player,
        waypoint: waypoint_at_pointer(game,state,&frame.view,rect,at)
            .map(|waypoint| Spot {
                dimension: waypoint.dimension,
                x: waypoint.x,
                z: waypoint.z,
                key: waypoint.key(),
            }),
    })
}

fn edit_waypoint(state: &mut State, key: u64) {
    let Some(waypoint) = state.waypoints.iter().find(|waypoint| waypoint.key() == key) else {
        return;
    };
    state.name_form = Some(NameForm {
        text: waypoint.name.clone(),
        color: waypoint.color,
        target: None,
        rename: Some(Spot {
            dimension: waypoint.dimension,
            x: waypoint.x,
            z: waypoint.z,
            key,
        }),
        group: waypoint.options.group.clone(),
        group_focused: false,
        kind: MarkerMode::Saved,
        group_edit: None,
        background: state.mode,
    });
    state.mode = Mode::Name;
    runtime::set_text_input(true);
}

fn create_waypoint_here(game: &Game<'_>, state: &mut State) {
    let Some(player) = game.native_player() else { return; };
    state.name_form = Some(NameForm {
        text: default_waypoint_name(state),
        color: default_waypoint_color(state),
        target: Some(Target {
            dimension: Dimension::from_id(player.dimension),
            x: player.x.floor() as i32,
            y: player.y.floor() as i32,
            z: player.z.floor() as i32,
        }),
        rename: None,
        group: state.waypoint_group.clone().unwrap_or_default(),
        group_focused: false,
        kind: MarkerMode::Saved,
        group_edit: None,
        background: state.mode,
    });
    state.mode = Mode::Name;
    runtime::set_text_input(true);
}

fn bulk_three_d_value(state: &State) -> bool {
    state.waypoint_bulk.as_ref().is_some_and(|keys| state.waypoints.iter()
        .any(|point| keys.contains(&point.key()) && (!point.show_3d || point.map_only())))
}

fn toggle_waypoint_bulk(state: &mut State) {
    state.waypoint_confirm_delete = false;
    state.waypoint_bulk = if state.waypoint_bulk.is_some() {
        None
    } else {
        Some(state.waypoint_selected.into_iter().collect())
    };
}

fn select_waypoint_results(game: &Game<'_>, state: &mut State) {
    state.waypoint_bulk = Some(listed_waypoints(game, state).into_iter().map(|(_, point)| point.key()).collect());
    state.waypoint_confirm_delete = false;
}

fn waypoint_press(
    game: &Game<'_>,
    state: &mut State,
    screen: &mod_api::ScreenView<'_>,
    button: i32,
    canceled: &mut bool,
) {
    if button != 0 {
        return;
    }
    let w=screen.rect[2];
    let h=screen.rect[3];
    let [options,create,back]=waypoint_header(w);
    let controller=runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    let [group_rect, create_group, rename_group, remove_group]=waypoint_group_buttons(w);
    if state.waypoint_groups_open {
        state.waypoint_groups_open=false;
        let groups=waypoint_group_choices(game,state);
        let move_group = std::mem::take(&mut state.waypoint_group_move);
        if inside(screen.mouse,group_rect) && !move_group { *canceled=true;return; }
        let count=visible_group_rows(h);
        let start=state.waypoint_groups_scroll.min(groups.len().saturating_sub(count));
        if let Some((_,group))=groups.into_iter().skip(start).take(count).enumerate()
            .find(|(i,_)|inside(screen.mouse,group_row(w,*i)))
        {
            if move_group {
                if let Some(group) = group { edit_waypoint_batch(game, state, runtime::WaypointBatch::Move(&group)); }
            } else {
                change_waypoint_group(game, state, group);
            }
            *canceled=true;
            return;
        }
    }
    let [rename, delete, show, share, paste, chat] = waypoint_actions(w, h);
    if !inside(screen.mouse, delete) { state.waypoint_confirm_delete = false; }
    if let Some(index) = waypoint_bulk_actions(w,h).iter().position(|rect| inside(screen.mouse,*rect))
        && (index == 0 || state.waypoint_bulk.is_some())
    {
        let count = state.waypoint_bulk.as_ref().map_or(0, BTreeSet::len);
        match index {
            0 => toggle_waypoint_bulk(state),
            1 => select_waypoint_results(game, state),
            2 if count > 0 => {
                state.waypoint_group_move = true;
                state.waypoint_groups_open = true;
                state.waypoint_groups_scroll = 0;
            }
            3 if count > 0 => edit_waypoint_batch(game, state, runtime::WaypointBatch::Visible(true)),
            4 if count > 0 => edit_waypoint_batch(game, state, runtime::WaypointBatch::Visible(false)),
            5 if count > 0 => {
                let enabled = bulk_three_d_value(state);
                edit_waypoint_batch(game, state, runtime::WaypointBatch::ThreeD(enabled));
            }
            _ => {}
        }
        *canceled = true;
        return;
    }
    let [search,dimension,sort]=waypoint_filters(w);
    state.waypoint_search_focus=inside(screen.mouse,search);
    runtime::set_text_input(state.waypoint_search_focus);
    if state.waypoint_search_focus {
        *canceled=true;
        return;
    }
    if inside(screen.mouse,dimension) {
        let i=Dimension::ALL.iter().position(|d|*d==state.waypoint_dimension).unwrap_or(0);
        state.waypoint_dimension=Dimension::ALL[(i+1)%Dimension::ALL.len()];
        waypoint_search_changed(state);
        *canceled=true;
        return;
    }
    if inside(screen.mouse,sort) {
        state.waypoint_sort=(state.waypoint_sort+1)%3;
        state.waypoint_scroll=0;
        *canceled=true;
        return;
    }
    if inside(screen.mouse,options) {
        open_settings(state,Some(Mode::Waypoints),2);
        *canceled=true;
        return;
    }
    if inside(screen.mouse,create) {
        create_waypoint_here(game,state);
        *canceled=true;
        return;
    }
    if inside(screen.mouse,back) {
        if missing_item(game,state,true).is_none() {state.mode=Mode::Map;}
        *canceled=true;
        return;
    }
    if inside(screen.mouse,group_rect) {
        state.waypoint_groups_open=true;
        state.waypoint_groups_scroll=0;
        *canceled=true;
        return;
    }
    let named = state.waypoint_group.as_ref().filter(|name|!name.is_empty());
    let group_edit = if inside(screen.mouse,create_group) {
        Some(GroupEdit::Create)
    } else if inside(screen.mouse,rename_group) {
        named.cloned().map(GroupEdit::Rename)
    } else {None};
    if let Some(group_edit) = group_edit {
        let text = match &group_edit {GroupEdit::Create => String::new(),GroupEdit::Rename(name)=>name.clone()};
        state.name_form = Some(NameForm {
            text, color: 0, target: None, rename: None, group: String::new(), group_focused:false,
            kind: MarkerMode::Saved, group_edit: Some(group_edit), background:Mode::Waypoints,
        });
        state.mode = Mode::Name;
        runtime::set_text_input(true);
        *canceled=true;
        return;
    }
    if inside(screen.mouse,remove_group) && let Some(name)=named
        && controller.remove_empty_waypoint_group(&state.world,name)
    {
        change_waypoint_group(game, state, None);
        *canceled=true;
        return;
    }
    let hit = visible_waypoint_rows(game,state,screen.rect[2],screen.rect[3]).find_map(|(_,point,row)| {
        inside(screen.mouse, row).then(|| (row, point.key()))
    });
    if let Some((row, key)) = hit {
        let toggle = waypoint_3d_rect(row);
        let add = game.modifiers.control_key() || game.modifiers.super_key();
        let range = game.modifiers.shift_key();
        if state.waypoint_bulk.is_some() || add || range {
            let mut keys = state.waypoint_bulk.take().unwrap_or_else(||
                state.waypoint_selected.into_iter().collect());
            if range {
                let rows = listed_waypoints(game, state);
                if let Some(end) = rows.iter().position(|(_, point)| point.key() == key) {
                    let start = rows.iter().position(|(_, point)| Some(point.key()) == state.waypoint_selected).unwrap_or(end);
                    keys.extend(rows[start.min(end)..=start.max(end)].iter().map(|(_, point)| point.key()));
                }
            } else if !keys.remove(&key) {
                keys.insert(key);
            }
            state.waypoint_bulk = Some(keys);
            state.waypoint_selected = Some(key);
        } else if inside(screen.mouse, toggle) {
            controller.change_waypoint(&state.world, key, |point| {
                if point.map_only() {
                    point.options.visibility %= 2;
                    point.show_3d = true;
                } else {
                    point.show_3d = !point.show_3d;
                }
            });
        } else if inside(screen.mouse, waypoint_scope_rect(row)) {
            controller.change_waypoint(&state.world, key, |point| point.options.visibility ^= 1);
        } else if inside(screen.mouse, [toggle[0]-24.0, toggle[1], 20.0, 18.0]) {
            controller.change_waypoint(&state.world, key, |point| point.options.disabled = !point.options.disabled);
        } else {
            state.waypoint_selected = Some(key);
        }
        *canceled = true;
        return;
    }
    if inside(screen.mouse, chat) && let Some(link) = selected_waypoint(state).map(Waypoint::share_link) {
        if link.encode_utf16().count() <= 256 {
            state.pending_chat = Some((state.world.clone(), link));
        } else {
            state.notice = Some((tr("This link is too long for the chat. Use Copy link.",
                "Ссылка слишком длинная для чата. Используй «Копировать».",
                "Ce lien est trop long pour le chat. Utilisez Copier le lien.",
                "El enlace es demasiado largo para el chat. Usa Copiar enlace.",
                "Der Link ist zu lang für den Chat. Verwende Link kopieren.").to_owned(), Instant::now()));
        }
        *canceled = true;
        return;
    }
    if inside(screen.mouse, share) && let Some(point) = selected_waypoint(state) {
        state.pending_copy = Some(point.share_link());
        state.notice = Some((tr(
            "Waypoint link copied", "Ссылка на метку скопирована", "Lien du repère copié",
            "Enlace del marcador copiado", "Wegpunktlink kopiert",
        ).to_owned(), Instant::now()));
        *canceled = true;
        return;
    }
    if inside(screen.mouse, paste) {
        let clipboard = game.clipboard.text();
        let dimension = game.native_player().map_or(Dimension::Overworld, |player| Dimension::from_id(player.dimension));
        let message = match runtime::receive_shared_waypoint(&clipboard, &state.world, dimension) {
            Some(point) => match controller.merge_waypoints(&state.world, vec![point]) {
                Ok(_) => tr("Waypoint imported", "Метка импортирована", "Repère importé", "Marcador importado", "Wegpunkt importiert").to_owned(),
                Err(error) => error,
            },
            None => tr("No waypoint link in clipboard", "В буфере нет ссылки на метку", "Aucun lien de repère dans le presse-papiers", "No hay enlace en el portapapeles", "Kein Wegpunktlink in der Zwischenablage").to_owned(),
        };
        state.notice = Some((message, Instant::now()));
        *canceled = true;
        return;
    }
    if inside(screen.mouse, show) {
        if let Some(dimension) = show_selected_waypoint(state) {
            runtime::initialize(game.native_data_dir(runtime::MOD_ID))
                .choose_dimension(true, dimension);
        }
        *canceled = true;
        return;
    }
    if inside(screen.mouse, rename)
        && let Some(key) = selected_waypoint(state).map(Waypoint::key)
    {
        state.waypoint_bulk = None;
        edit_waypoint(state,key);
        *canceled = true;
        return;
    }
    if inside(screen.mouse, delete) && let Some(keys) = &state.waypoint_bulk {
        if !keys.is_empty() {
            if state.waypoint_confirm_delete {
                edit_waypoint_batch(game, state, runtime::WaypointBatch::Delete);
            } else {
                state.waypoint_confirm_delete = true;
            }
        }
        *canceled = true;
        return;
    }
    if inside(screen.mouse, delete)
        && let Some(waypoint) = selected_waypoint(state)
    {
        controller.delete_waypoint(&state.world, waypoint.key());
        state.notice = Some((
            tr(
                "Waypoint deleted",
                "Вейпоинт удалён",
                "Repère supprimé",
                "Marcador eliminado",
                "Wegpunkt gelöscht",
            )
            .into(),
            Instant::now(),
        ));
        state.waypoint_selected = None;
        *canceled = true;
    }
}

fn name_press(
    game: &Game<'_>,
    state: &mut State,
    screen: &mod_api::ScreenView<'_>,
    button: i32,
    canceled: &mut bool,
) {
    if button != 0 {
        return;
    }
    let rect = name_form_rect(screen.rect[2], screen.rect[3]);
    if let Some(form) = state.name_form.as_mut() {
        if form.group_edit.is_none() && inside(screen.mouse, group_field(rect)) {
            form.group_focused = true;
            *canceled = true;
            return;
        }
        if inside(screen.mouse, [rect[0] + 10.0, rect[1] + 26.0, rect[2] - 20.0, 18.0]) {
            form.group_focused = false;
            *canceled = true;
            return;
        }
    }
    for (index, &color) in runtime::WAYPOINT_COLORS.iter().enumerate() {
        if inside(screen.mouse, color_swatch_at(rect, index)) {
            if let Some(form) = state.name_form.as_mut() {
                form.color = color;
            }
            *canceled = true;
            return;
        }
    }
    let save = [
        rect[0] + rect[2] - 184.0,
        rect[1] + rect[3] - 24.0,
        80.0,
        18.0,
    ];
    let cancel = [
        rect[0] + rect[2] - 96.0,
        rect[1] + rect[3] - 24.0,
        80.0,
        18.0,
    ];
    if inside(screen.mouse, save) {
        save_name(game, state);
        *canceled = true;
    } else if inside(screen.mouse, cancel) {
        cancel_name(state);
        *canceled = true;
    }
}

fn save_name(game: &Game<'_>, state: &mut State) {
    let Some(form) = state.name_form.take() else {
        return;
    };
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    if let Some(edit) = form.group_edit {
        let name = form.text.trim();
        let saved = match edit {
            GroupEdit::Create => controller.add_waypoint_groups(&state.world,[name.to_owned()]),
            GroupEdit::Rename(old) => controller.rename_waypoint_group(&state.world,&old,name),
        };
        if saved {
            change_waypoint_group(game, state, Some(name.to_owned()));
        }
        state.notice = Some((if saved {
            tr("Group saved","Группа сохранена","Groupe enregistré","Grupo guardado","Gruppe gespeichert")
        } else {
            tr("Group unchanged","Группа не изменена","Groupe inchangé","Grupo sin cambios","Gruppe unverändert")
        }.to_owned(),Instant::now()));
    } else if let Some(rename) = form.rename {
        controller.change_waypoint(&state.world, rename.key, |point| {
            point.name = Waypoint::clean_name(&form.text);
            point.options.group = form.group.trim().to_owned();
            point.color = if point.initials.is_some() || point.icon == runtime::WaypointIcon::Skull {
                form.color & 0x00ff_ffff
            } else {
                Waypoint::clean_color(form.color)
            };
        });
        state.notice = Some((
            tr(
                "Waypoint updated",
                "Вейпоинт обновлён",
                "Repère modifié",
                "Marcador actualizado",
                "Wegpunkt aktualisiert",
            )
            .into(),
            Instant::now(),
        ));
    } else if let Some(target) = form.target {
        let biome = known_biome(game, target.dimension, [target.x, target.y, target.z]);
        let saved = controller.add_waypoint(Waypoint {
            world: state.world.clone(),
            dimension: target.dimension,
            x: target.x,
            y: target.y,
            z: target.z,
            name: Waypoint::clean_name(&form.text),
            initials: None,
            options: runtime::WaypointOptions {
                group: form.group.trim().to_owned(),
                temporary: !matches!(form.kind, MarkerMode::Saved),
                destination: matches!(form.kind, MarkerMode::Destination),
                visibility: if !matches!(form.kind,MarkerMode::Saved) && game.map_option("temporary_waypoints_global")==Some(1) {1} else {0},
                ..Default::default()
            },
            color: form.color,
            icon: runtime::WaypointIcon::Letter,
            biome,
            show_3d: true,
        });
        state.notice = Some((
            if saved {
                tr(
                    "Waypoint saved",
                    "Вейпоинт сохранён",
                    "Repère enregistré",
                    "Marcador guardado",
                    "Wegpunkt gespeichert",
                )
            } else {
                tr(
                    "Waypoint unchanged: duplicate, limit reached or world unavailable",
                    "Вейпоинт не изменён: дубликат, лимит или мир недоступен",
                    "Repère inchangé : doublon, limite atteinte ou monde indisponible",
                    "Marcador sin cambios: duplicado, límite alcanzado o mundo no disponible",
                    "Wegpunkt unverändert: Duplikat, Grenze erreicht oder Welt nicht verfügbar",
                )
            }
            .into(),
            Instant::now(),
        ));
    }
    state.mode = form.background;
    runtime::set_text_input(false);
}

fn cancel_name(state: &mut State) {
    if let Some(form) = state.name_form.take() {
        state.mode = form.background;
    }
    runtime::set_text_input(false);
}

pub(super) fn drag(game: &Game<'_>, delta: [f32; 2], button: i32, canceled: &mut bool) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) || button != 0 {
        return;
    }
    let mut state = state();
    if state.info.is_some() {
        *canceled = true;
        return;
    }
    if let Some(edit) = &mut state.placement {
        edit.move_by(delta);
        *canceled = true;
        return;
    }
    if state.mode != Mode::Map || !state.dragging {
        return;
    }
    state.camera.pan([f64::from(delta[0]), f64::from(delta[1])]);
    state.elapsed = 1.0;
    *canceled = true;
}

pub(super) fn scroll(
    game: &Game<'_>,
    screen: &mod_api::ScreenView<'_>,
    scroll: [f64; 2],
    canceled: &mut bool,
) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let mut state = state();
    if state.mode == Mode::Settings {
        if let Some(editor) = &mut state.radar_editor {
            editor.scroll([screen.rect[2], screen.rect[3]], scroll[1]);
            *canceled = true;
            return;
        }
        let panel=settings_panel(screen.rect[2],screen.rect[3]);
        let max=settings_catalog(game,state.settings_tab).count().saturating_sub(settings_rows(panel));
        state.settings_scroll=(state.settings_scroll as i64-scroll[1] as i64).clamp(0,max as i64) as usize;
        *canceled=true;
        return;
    }
    if state.mode == Mode::Caches {
        let snapshot = runtime::initialize(game.native_data_dir(runtime::MOD_ID))
            .caches
            .snapshot();
        let max = snapshot
            .entries
            .len()
            .saturating_sub(cache_rows(screen.rect[3]));
        state.caches.scroll =
            (state.caches.scroll.min(max) as i64 - scroll[1] as i64).clamp(0, max as i64) as usize;
        *canceled = true;
        return;
    }
    if state.info.is_some() {
        let rect = information_rect(screen.rect[2], screen.rect[3]);
        let visible = ((rect[3] - 56.0) / 12.0).max(1.0) as usize;
        let lines = information_lines(game, &state, state.info.unwrap(), rect[2] - 24.0);
        let max = lines.len().saturating_sub(visible);
        state.info_scroll =
            (state.info_scroll.min(max) as i32 - scroll[1] as i32).clamp(0, max as i32) as usize;
        *canceled = true;
        return;
    }
    if state.mode == Mode::Waypoints {
        if state.waypoint_groups_open {
            let groups=waypoint_group_choices(game,&state).len().saturating_sub(visible_group_rows(screen.rect[3]));
            state.waypoint_groups_scroll=(state.waypoint_groups_scroll as i64-scroll[1] as i64).clamp(0,groups as i64) as usize;
            *canceled=true;
            return;
        }
        let rows = listed_waypoints(game,&state).len().saturating_sub(waypoint_visible_rows(screen.rect[3]));
        let next = (state.waypoint_scroll as i32 - scroll[1] as i32).clamp(0, rows as i32);
        state.waypoint_scroll = next as usize;
        *canceled = true;
        return;
    }
    if state.mode != Mode::Map {
        return;
    }
    if state.map_panel != MapPanel::None
        && inside(screen.mouse,map_panel_rect(screen.rect[2],screen.rect[3])) {
        let count = if state.map_panel == MapPanel::ServerWorlds {
            let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
            1 + controller.server_worlds(&state.base_world).0.len()
        } else {
            map_panel_waypoints(game, &state).len()
        };
        let max = count.saturating_sub(map_panel_rows(screen.rect[3]));
        state.map_panel_scroll = (state.map_panel_scroll as i64 - scroll[1] as i64).clamp(0,max as i64) as usize;
        *canceled = true;
        return;
    }
    if over_map_controls(game,&state,screen.mouse, screen.rect[2], screen.rect[3]) {
        *canceled = true;
        return;
    }
    let rect = rectangle(screen.rect[2], screen.rect[3]);
    if !inside(screen.mouse, rect) {
        return;
    }
    let offset = [
        f64::from(screen.mouse[0] - rect[0] - rect[2] / 2.0),
        f64::from(screen.mouse[1] - rect[1] - rect[3] / 2.0),
    ];
    map_zoom(game,&mut state,scroll[1],offset);
    *canceled = true;
}

pub(super) fn screen_key(game: &Game<'_>, key: &str, modifiers: u32, canceled: &mut bool) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let mut state = state();
    if state.mode == Mode::Map && state.map_panel == MapPanel::ServerWorlds && state.server_world_focus {
        match key {
            "key.keyboard.escape" => {
                state.server_world_focus = false;
                runtime::set_text_input(false);
            }
            "key.keyboard.backspace" => { state.server_world_name.pop(); }
            "key.keyboard.enter" | "key.keyboard.keypad.enter" => create_map_server_world(game, &mut state),
            "key.keyboard.a" if modifiers & 10 != 0 => state.server_world_name.clear(),
            "key.keyboard.v" if modifiers & 10 != 0 => {
                let text = game.clipboard.text();
                for c in text.chars().filter(|c| !c.is_control()) {
                    if state.server_world_name.len() + c.len_utf8() > runtime::SERVER_WORLD_NAME_LIMIT { break; }
                    state.server_world_name.push(c);
                }
            }
            _ => {}
        }
        *canceled = true;
        return;
    }
    if state.info.is_some() {
        if key == "key.keyboard.escape" {
            state.info = None;
        }
        *canceled = true;
        return;
    }
    if state.mode == Mode::Settings {
        if let Some(editor) = &mut state.radar_editor {
            if editor.key(game, key, modifiers) { state.radar_editor = None; }
        } else if let Some(edit) = &mut state.placement {
            match key {
                "key.keyboard.left" => edit.move_by([-1., 0.]),
                "key.keyboard.right" => edit.move_by([1., 0.]),
                "key.keyboard.up" => edit.move_by([0., -1.]),
                "key.keyboard.down" => edit.move_by([0., 1.]),
                "key.keyboard.enter" | "key.keyboard.keypad.enter" => {
                    state.placement_change = Some(Some(edit.position));
                    state.placement = None;
                }
                "key.keyboard.escape" => state.placement = None,
                _ => {}
            }
        } else if key == "key.keyboard.escape" { close_settings(&mut state); }
        *canceled=true;
        return;
    }
    if matches!(state.mode, Mode::Waypoints | Mode::Map) && state.waypoint_search_focus {
        match key {
            "key.keyboard.escape" | "key.keyboard.enter" | "key.keyboard.keypad.enter" => {
                state.waypoint_search_focus=false;
                runtime::set_text_input(false);
            }
            "key.keyboard.backspace" => {
                state.waypoint_search.pop();
                waypoint_search_changed(&mut state);
            }
            "key.keyboard.a" if modifiers & 10 != 0 => {
                state.waypoint_search.clear();
                waypoint_search_changed(&mut state);
            }
            "key.keyboard.v" if modifiers & 10 != 0 => {
                let text = game.clipboard.text();
                for ch in text.chars().filter(|c| !c.is_control()) {
                    if state.waypoint_search.len() + ch.len_utf8() > 128 { break; }
                    state.waypoint_search.push(ch);
                }
                waypoint_search_changed(&mut state);
            }
            _=>{}
        }
        *canceled=true;
        return;
    }
    if state.mode == Mode::Waypoints && state.waypoint_groups_open && key=="key.keyboard.escape" {
        state.waypoint_groups_open=false;
        state.waypoint_group_move=false;
        *canceled=true;
        return;
    }
    if state.mode == Mode::Waypoints && key == "key.keyboard.a" && modifiers & 10 != 0 {
        select_waypoint_results(game, &mut state);
        *canceled = true;
        return;
    }
    if state.mode == Mode::Waypoints && key == "key.keyboard.escape" && state.waypoint_bulk.is_some() {
        state.waypoint_bulk = None;
        state.waypoint_confirm_delete = false;
        *canceled = true;
        return;
    }
    if state.mode==Mode::Waypoints && matches!(key,"key.keyboard.up"|"key.keyboard.down") {
        let step=if key=="key.keyboard.up" {-1} else {1};
        let (_,h)=game.gui_size();
        let count=waypoint_visible_rows(h);
        let points=listed_waypoints(game,&state);
        let next=points.iter().position(|(_,p)|Some(p.key())==state.waypoint_selected)
            .map_or(0,|i|(i as i64+step).clamp(0,points.len().saturating_sub(1) as i64) as usize);
        let key=points.get(next).map(|(_,p)|p.key());
        if key.is_some() {
            state.waypoint_selected=key;
            state.waypoint_scroll=state.waypoint_scroll.clamp(next.saturating_sub(count-1),next);
        }
        *canceled=true;
        return;
    }
    if state.mode==Mode::Waypoints && matches!(key,"key.keyboard.enter"|"key.keyboard.keypad.enter") {
        if let Some(dimension)=show_selected_waypoint(&mut state) {
            runtime::initialize(game.native_data_dir(runtime::MOD_ID)).choose_dimension(true,dimension);
        }
        *canceled=true;
        return;
    }
    if state.mode == Mode::Name {
        match key {
            "key.keyboard.escape" => {
                cancel_name(&mut state);
                *canceled = true;
            }
            "key.keyboard.backspace" => {
                if let Some(form) = state.name_form.as_mut() {
                    if form.group_focused { form.group.pop(); } else { form.text.pop(); }
                }
                *canceled = true;
            }
            "key.keyboard.enter" | "key.keyboard.keypad.enter" => {
                save_name(game, &mut state);
                *canceled = true;
            }
            _ => {
                *canceled = true;
            }
        }
        return;
    }
    if key == "key.keyboard.escape" {
        if state.map_panel != MapPanel::None && state.mode == Mode::Map {
            state.map_panel = MapPanel::None;
            state.waypoint_search_focus = false;
            state.server_world_focus = false;
            runtime::set_text_input(false);
            *canceled = true;
            return;
        }
        runtime::set_text_input(false);
        state.close = true;
        *canceled = true;
    }
}

pub(super) fn screen_char(game: &Game<'_>, c: char, canceled: &mut bool) {
    if game.mod_screen() != Some(runtime::MOD_ID) || !game.map_on(runtime::MOD_ID) {
        return;
    }
    let mut state = state();
    if state.mode == Mode::Map && state.map_panel == MapPanel::ServerWorlds && state.server_world_focus {
        if !c.is_control() && state.server_world_name.len() + c.len_utf8() <= runtime::SERVER_WORLD_NAME_LIMIT {
            state.server_world_name.push(c);
        }
        *canceled = true;
        return;
    }
    if let Some(editor) = &mut state.radar_editor {
        editor.character(c);
        *canceled = true;
        return;
    }
    if matches!(state.mode, Mode::Waypoints | Mode::Map) && state.waypoint_search_focus {
        if !c.is_control() && state.waypoint_search.len()+c.len_utf8()<=128 {
            state.waypoint_search.push(c);
            waypoint_search_changed(&mut state);
        }
        *canceled=true;
        return;
    }
    if state.mode != Mode::Name {
        return;
    }
    if c.is_control() {
        return;
    }
    if let Some(form) = state.name_form.as_mut() {
        if form.group_focused {
            if form.group.len() + c.len_utf8() <= 128 { form.group.push(c); }
        } else if if form.group_edit.is_some() {form.text.len()+c.len_utf8()<=128} else {form.text.chars().count()<32} {
            form.text.push(c);
        }
    }
    *canceled = true;
}

pub(super) fn disconnect(game: &Game<'_>) {
    if !game.native_window() {
        return;
    }
    let pending = DEATH_CAPTURE.lock().unwrap_or_else(|error| error.into_inner()).disconnect();
    if let Some((world, location)) = pending && game.map_on(runtime::MOD_ID) {
        record_death(game, &world, location);
    }
    runtime::set_text_input(false);
    QUICK_MINIMAP.store(0,std::sync::atomic::Ordering::Relaxed);
    LOCATOR_WAYPOINTS.lock().unwrap_or_else(|error| error.into_inner()).clear();
    WAYSTONES.lock().unwrap_or_else(|error| error.into_inner()).clear();
    let mut s = state();
    let controller = runtime::initialize(game.native_data_dir(runtime::MOD_ID));
    controller.clear_temporary_waypoints(&s.world);
    let next = s.session + 1;
    *s = State {
        session: next,
        ..State::default()
    };
    controller.maps.reset(next);
}

pub(super) fn atlas_invalidated() {
    state().frame = None;
    state().page_images.clear();
    super::minimap_radar_icons::invalidate();
}
