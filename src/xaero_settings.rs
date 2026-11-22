// SPDX-License-Identifier: GPL-3.0-only
use crate::runtime::minimap::Settings;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    path::Path,
};

const COMMAND_KEYS: [&str; 7] = [
    "tp_waypoint",
    "tp_rotation",
    "tp_map",
    "tp_map_dimension",
    "tp_player",
    "minimap_item",
    "map_item",
];

pub fn teleport_commands(directory: &Path) -> io::Result<BTreeMap<&'static str, String>> {
    let file = match File::open(directory.join("teleport.ini")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 {
        return Err(io::Error::other("teleport.ini exceeds 4 KiB"));
    }
    Ok(text
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = *COMMAND_KEYS.iter().find(|candidate| **candidate == key)?;
            Some((key, valid_command(value)?.to_owned()))
        })
        .collect())
}

pub fn merge_teleport_commands(
    directory: &Path,
    incoming: BTreeMap<&'static str, String>,
) -> io::Result<()> {
    let mut commands = teleport_commands(directory)?;
    commands.extend(incoming);
    let text = commands
        .into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect::<String>();
    crate::runtime::write_preferences_file(&directory.join("teleport.ini"), &text, 0)
}

fn valid_command(value: &str) -> Option<&str> {
    let value = value.trim().trim_start_matches('/');
    (!value.is_empty() && value.len() <= 550 && !value.chars().any(char::is_control))
        .then_some(value)
}

#[derive(Debug, Default)]
pub struct Import {
    pub settings: BTreeMap<&'static str, String>,
    pub commands: BTreeMap<&'static str, String>,
    pub radar_categories: Option<String>,
    pub unsupported: Vec<String>,
    pub profile: Option<String>,
    pub switching: bool,
}

pub fn valid_profile(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

pub fn active_profile(directory: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(directory.join("xaero-profile"))
        .ok()?
        .take(129)
        .read_to_string(&mut text)
        .ok()?;
    valid_profile(text.trim()).then(|| text.trim().to_owned())
}

pub fn profile_directory(directory: &Path, profile: Option<&str>) -> std::path::PathBuf {
    let root = directory.join("xaero-config");
    profile
        .filter(|name| valid_profile(name))
        .map_or_else(|| root.clone(), |name| root.join("profiles").join(name))
}

pub fn commands_directory(directory: &Path, profile: Option<&str>) -> std::path::PathBuf {
    profile.filter(|name| valid_profile(name)).map_or_else(
        || directory.to_owned(),
        |name| directory.join("xaero-commands").join(name),
    )
}

pub fn read_active(directory: &Path, next: bool) -> io::Result<Import> {
    let current = active_profile(directory);
    let mut chosen = current.clone();
    if next {
        let root = directory.join("xaero-config/profiles");
        let mut profiles = std::fs::read_dir(&root)?
            .take(4096)
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                let name = entry.file_name().into_string().ok()?;
                valid_profile(&name).then_some(name)
            })
            .collect::<Vec<_>>();
        profiles.sort_unstable();
        chosen = profiles
            .into_iter()
            .find(|name| current.as_ref().is_none_or(|old| name > old));
        if chosen.is_none() && current.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "No named Xaero profile",
            ));
        }
    }
    let switching = chosen != current;
    let source = profile_directory(directory, chosen.as_deref());
    let mut data = match read(&source) {
        Err(error) if switching && chosen.is_none() && error.kind() == io::ErrorKind::NotFound => {
            Import::default()
        }
        result => result?,
    };
    data.profile = chosen;
    data.switching = switching;
    Ok(data)
}

pub fn read(directory: &Path) -> io::Result<Import> {
    let mut import = Import::default();
    let mut found = false;
    for (world_map, folder, files) in [
        (false, "minimap", ["xaerominimap.txt", "minimap.cfg"]),
        (true, "world-map", ["xaeroworldmap.txt", "worldmap.cfg"]),
    ] {
        let native = directory.join(folder);
        let client = read_config(&native.join("client.cfg"))?;
        let profile = client.as_deref().and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.trim_start_matches('\u{feff}').split_once('=')?;
                let value = value.trim().trim_matches('"');
                (key.trim() == "current_profile" && valid_profile(value)).then_some(value)
            })
        }).unwrap_or("default");
        if let Some(text) = read_config(&native.join("profiles").join(format!("{profile}.cfg")))? {
            parse(&text, world_map, &mut import);
            found = true;
        }
        if let Some(text) = client {
            parse(&text, world_map, &mut import);
            found = true;
        }
        for file in files {
            if let Some(text) = read_config(&directory.join(file))? {
                parse(&text, world_map, &mut import);
                found = true;
            }
        }
    }
    import.radar_categories = read_radar_categories(directory)?;
    found |= import.radar_categories.is_some();
    if import.radar_categories.is_some() {
        import
            .unsupported
            .retain(|key| key != "entity_radar_categories");
    }
    if read_info_display(directory)?.is_some() {
        found = true;
        import.unsupported.retain(|key| key != "info_display_config");
    }
    if !found {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Put Xaero profiles in xaero-config as minimap.cfg and/or worldmap.cfg",
        ));
    }
    import.unsupported.sort();
    import.unsupported.dedup();
    Ok(import)
}

fn read_config(path: &Path) -> io::Result<Option<String>> {
    const LIMIT: u64 = 256 * 1024;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut text = String::new();
    file.take(LIMIT + 1).read_to_string(&mut text)?;
    if text.len() as u64 > LIMIT {
        return Err(io::Error::other(format!("Xaero profile exceeds 256 KiB: {}", path.display())));
    }
    Ok(Some(text))
}

pub fn read_radar_categories(directory: &Path) -> io::Result<Option<String>> {
    const LIMIT: u64 = 256 * 1024;
    for relative in [
        "entity_radar_categories/minimap.cfg.json",
        "entity_radar_categories.json",
    ] {
        let file = match File::open(directory.join(relative)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut source = String::new();
        file.take(LIMIT + 1).read_to_string(&mut source)?;
        if source.len() as u64 > LIMIT {
            return Err(io::Error::other("Xaero radar categories exceed 256 KiB"));
        }
        return Ok(Some(source));
    }
    Ok(None)
}

fn toggle(value: &str) -> Option<bool> {
    match value {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

fn boolean_choice(value: &str, default: bool) -> Option<String> {
    Some(u8::from(toggle(value)? != default).to_string())
}

fn bounded(value: &str, min: i32, max: i32) -> Option<String> {
    let number = value.parse::<i32>().ok()?;
    (min..=max).contains(&number).then(|| number.to_string())
}

fn decimal(value: &str, min: f64, max: f64) -> Option<String> {
    let number = value.parse::<f64>().ok()?;
    (min..=max).contains(&number).then(|| number.to_string())
}

fn map_palette(value: &str) -> Option<String> {
    let index = value.parse::<usize>().ok()?;
    crate::runtime::XAERO_WAYPOINT_COLORS
        .get(index)
        .map(u32::to_string)
}

fn arrow_color(value: &str) -> Option<String> {
    let index = value.parse::<i8>().ok()?;
    if index == -1 || index == -2 {
        return Some(0xff5050u32.to_string());
    }
    [
        0xcc1a1a, 0x179100, 0x008cff, 0xffed00, 0xba54d4, 0xffffff, 0, 0x750000,
    ]
    .get(usize::try_from(index).ok()?)
    .map(u32::to_string)
}

fn parse(input: &str, world_map: bool, out: &mut Import) {
    let mut cave_allowed = None;
    let mut automatic = None;
    let mut height = None;
    for line in input.lines() {
        let line = line.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            out.unsupported.push(line.chars().take(60).collect());
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key == "current_profile" {
            continue;
        }
        if matches!(
            (world_map, key),
            (false, "minimap_item") | (true, "map_item")
        ) {
            let value = value.trim_matches('"');
            if value == "-" {
                out.commands.insert(
                    if world_map {
                        "map_item"
                    } else {
                        "minimap_item"
                    },
                    "-".to_owned(),
                );
            } else if let Some(item) = crate::radar::resource_id(value) {
                out.commands.insert(
                    if world_map {
                        "map_item"
                    } else {
                        "minimap_item"
                    },
                    item,
                );
            } else {
                out.unsupported.push(key.to_owned());
            }
            continue;
        }
        let command = match (world_map, key) {
            (false, "default_waypoint_teleport_format") => Some("tp_waypoint"),
            (false, "default_waypoint_teleport_rotation_format") => Some("tp_rotation"),
            (true, "default_player_teleport_command_format") => Some("tp_player"),
            (true, "default_map_teleport_command_format") => Some("tp_map"),
            (true, "default_map_teleport_command_dimension_format") => Some("tp_map_dimension"),
            _ => None,
        };
        if let Some(command) = command {
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            if let Some(value) = valid_command(value) {
                out.commands.insert(command, value.to_owned());
            } else {
                out.unsupported.push(key.to_owned());
            }
            continue;
        }
        if matches!(
            (world_map, key),
            (false, "minimap_cave_mode_allowed_dimensions")
                | (true, "cave_mode_allowed_dimensions")
        ) {
            let text = value.trim_matches('"');
            let names = text
                .split(',')
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .collect::<Vec<_>>();
            let dims = [
                ("minecraft:overworld", "overworld"),
                ("minecraft:the_nether", "the_nether"),
                ("minecraft:the_end", "the_end"),
            ];
            if names.iter().any(|value| {
                !dims
                    .iter()
                    .any(|(canonical, short)| value == canonical || value == short)
            }) {
                out.unsupported.push(key.to_owned());
            }
            for ((canonical, short), mini, world) in [
                (dims[0], "minimap_cave_surface", "world_cave_surface"),
                (dims[1], "minimap_cave_nether", "world_cave_nether"),
                (dims[2], "minimap_cave_end", "world_cave_end"),
            ] {
                let enabled =
                    names.is_empty() || names.iter().any(|n| *n == canonical || *n == short);
                out.settings
                    .insert(if world_map { world } else { mini }, enabled.to_string());
            }
            continue;
        }
        let bool_value = || toggle(value).map(|state| state.to_string());
        let setting = match (world_map, key) {
            (false, "display_minimap" | "minimap") => ("enabled", boolean_choice(value, true)),
            (false, "display_radar" | "entityRadar") => ("radar", bool_value()),
            (false, "radar_hide_invisible_entities") => ("radar_hide_invisible", bool_value()),
            (false, "radar_main_dot_size") => ("minimap_marker_size", bounded(value, 1, 4)),
            (false, "waypoints_in_world" | "ingameWaypoints") => {
                ("world_waypoints", boolean_choice(value, true))
            }
            (false, "waypoints_on_minimap" | "waypoints") => ("minimap_waypoints", bool_value()),
            (false, "waypoints_all_sets") => ("all_waypoint_groups", bool_value()),
            (false, "deathpoints") => ("death_waypoints", boolean_choice(value, true)),
            (false, "old_deathpoints") => ("old_deathpoints", bool_value()),
            (false, "temporary_waypoints_global") => ("temporary_waypoints_global", bool_value()),
            (false, "waypoint_teleport_cross_dimension") => {
                ("waypoint_cross_dimension", bool_value())
            }
            (false, "waypoint_partial_y_teleport") => ("waypoint_partial_y", bool_value()),
            (false, "waypoint_opacity_in_world") => {
                ("waypoint_world_opacity", bounded(value, 0, 100))
            }
            (false, "waypoint_opacity_on_minimap") => {
                ("waypoint_map_opacity", bounded(value, 10, 100))
            }
            (false, "waypoint_icon_scale_in_world") => {
                ("waypoint_world_scale", bounded(value, 0, 17))
            }
            (false, "waypoint_icon_scale_on_minimap") => {
                ("waypoint_map_scale", bounded(value, 0, 17))
            }
            (false, "waypoint_min_distance_in_world") => (
                "waypoint_min_distance",
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|n| (0.0..=4096.).contains(n))
                    .map(|n| n.to_string()),
            ),
            (false, "waypoint_max_distance") => {
                ("waypoint_max_distance", bounded(value, 0, 30_000_000))
            }
            (false, "minimap_centered_when_enlarged") => ("quick_minimap_centered", bool_value()),
            (false, "minimap_toggled_enlarged") => ("quick_minimap_toggle", bool_value()),
            (false, "minimap_enlarged_zoom") => ("quick_minimap_zoom", bounded(value, 0, 6)),
            (false, "minimap_cave_mode_extra_zoom") => ("minimap_cave_zoom", bounded(value, 1, 4)),
            (false, "delete_reached_deathpoints" | "deleteReachedDeathpoints") => {
                ("death_waypoint_auto_remove", boolean_choice(value, true))
            }
            (false, "minimap_shape" | "minimapShape") => ("shape", bounded(value, 0, 1)),
            (false, "minimap_north_locked" | "lockNorth") => {
                ("rotate", boolean_choice(value, true))
            }
            (false, "minimap_size" | "minimapSize") => (
                "minimap_exact_size",
                value
                    .parse::<u16>()
                    .ok()
                    .filter(|n| *n == 0 || (55..=250).contains(n))
                    .map(|n| n.to_string()),
            ),
            (false, "minimap_zoom") => (
                "zoom",
                value
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=6).contains(n))
                    .and_then(|zoom| {
                        Settings::options(4)
                            .iter()
                            .position(|settings| settings.blocks_per_pixel == 1.0 / f32::from(zoom))
                    })
                    .map(|index| index.to_string()),
            ),
            (false, "minimap_opacity") => ("minimap_opacity", bounded(value, 30, 100)),
            (false, "minimap_frame") => ("minimap_frame", bounded(value, 0, 3)),
            (false, "minimap_frame_color") => (
                "minimap_frame_color",
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|index| *index < 16)
                    .map(|index| crate::runtime::XAERO_WAYPOINT_COLORS[index].to_string()),
            ),
            (false, "minimap_slime_chunks" | "slimeChunks") => ("slime_chunks", bool_value()),
            (false, "minimap_chunk_grid" | "chunkGrid") => {
                if let Some(color) = map_palette(value) {
                    out.settings.insert("minimap_grid_color", color);
                }
                (
                    "chunk_grid",
                    value
                        .parse::<i32>()
                        .ok()
                        .filter(|n| (-1..=15).contains(n))
                        .map(|n| u8::from(n >= 0).to_string()),
                )
            }
            (false, "minimap_light_overlay_min_light") => ("light_min", bounded(value, 0, 15)),
            (false, "minimap_light_overlay_max_light") => ("light_max", bounded(value, 0, 15)),
            (false, "minimap_light_overlay_type") => {
                if let Some(mode) = value.parse::<i32>().ok().filter(|n| (0..=2).contains(n)) {
                    out.settings
                        .insert("light_overlay", (mode != 0).to_string());
                    if mode != 0 {
                        out.settings.insert("light_source", (mode - 1).to_string());
                    }
                    continue;
                }
                ("light_source", None)
            }
            (false, "minimap_cave_mode_allowed" | "caveMaps") => {
                cave_allowed = toggle(value);
                if cave_allowed.is_some() {
                    continue;
                }
                ("cave_view", None)
            }
            (false, "minimap_auto_cave_mode") => {
                automatic = value.parse::<i32>().ok().filter(|n| (0..=3).contains(n));
                if automatic.is_some() {
                    continue;
                }
                ("cave_view", None)
            }
            (false, "minimap_manual_cave_mode_start") => {
                height = bounded(value, -64, 319);
                if height.is_some() || value == "2147483647" {
                    continue;
                }
                ("cave_y", None)
            }
            (true, "display_hovered_biome") => ("world_hover_biome", bool_value()),
            (true, "display_zoom") => ("world_zoom_readout", bool_value()),
            (true, "zoom_buttons") => ("world_zoom_buttons", bool_value()),
            (true, "display_player_as_arrow") => ("world_marker_arrow", bool_value()),
            (true, "waypoint_backgrounds") => ("world_waypoint_backgrounds", bool_value()),
            (false, "waypoint_max_distance_dimension_scale") => {
                ("world_waypoint_distance_scale", bool_value())
            }
            (false, "waypoint_short_distance_in_world") => {
                ("waypoint_short_distance", bool_value())
            }
            (false, "hide_waypoint_coordinates") => ("waypoint_hide_coords", bool_value()),
            (false, "waypoint_name_in_world") => ("waypoint_name_enabled", bool_value()),
            (false, "waypoint_distance_in_menu") => ("waypoint_menu_distance", bool_value()),
            (false, "keep_enlarged_minimap_unlocked") => {
                ("minimap_unlocked_enlarged", bool_value())
            }
            (true, "skip_world_render") => ("world_skip_render", bool_value()),
            (false, "radar_smooth_dots") => ("radar_smooth", bool_value()),
            (true, "waypoint_scale") => ("world_waypoint_symbol_scale", decimal(value, 0.5, 5.0)),
            (true, "minimum_zoom_for_local_waypoints") => {
                ("world_waypoint_zoom_min", bounded(value, 0, 3))
            }
            (false, "radar_main_entity") => ("minimap_marker_mode", bounded(value, 0, 2)),
            (false, "minimap_arrow_scale") => ("minimap_arrow_scale", bounded(value, 1, 2)),
            (false, "minimap_arrow_opacity") => ("minimap_arrow_opacity", bounded(value, 1, 100)),
            (false, "minimap_compass_location") => {
                ("minimap_compass_location", bounded(value, 0, 2))
            }
            (false, "minimap_compass_scale") => ("minimap_compass_scale", bounded(value, 0, 17)),
            (false, "minimap_chunk_grid_line_width") => {
                ("minimap_grid_width", bounded(value, 1, 8))
            }
            (false, "waypoint_name_scale_in_world") => {
                ("waypoint_name_scale", bounded(value, 0, 17))
            }
            (false, "waypoint_distance_scale_in_world") => {
                ("waypoint_dist_scale", bounded(value, 0, 17))
            }
            (false, "waypoint_close_scale_in_world") => {
                ("waypoint_close_scale", decimal(value, 0.125, 8.0))
            }
            (false, "waypoint_distance_in_world") => {
                ("waypoint_distance_mode", bounded(value, 0, 3))
            }
            (false, "waypoint_vertical_pointing_angle") => {
                ("waypoint_vertical_angle", bounded(value, 1, 180))
            }
            (false, "waypoint_horizontal_pointing_angle") => {
                ("waypoint_horizontal_angle", bounded(value, 1, 180))
            }
            (false, "waypoint_convert_distance_to_km_at") => {
                ("waypoint_km_at", bounded(value, -1, 30000000))
            }
            (false, "waypoint_distance_precision") => {
                ("waypoint_dist_precision", bounded(value, 0, 10))
            }
            (true, "unlimited_zoom_in") => {
                ("world_unlimited_zoom_in", boolean_choice(value, false))
            }
            (true, "unlimited_zoom_out") => {
                ("world_unlimited_zoom_out", boolean_choice(value, false))
            }
            (false, "minimap_arrow_color") => ("minimap_marker_color", arrow_color(value)),
            (true, "arrow_color") => ("world_marker_color", arrow_color(value)),
            (false, "minimap_compass_shadow_color") => {
                ("minimap_compass_color", map_palette(value))
            }
            (false, "minimap_compass_north_shadow_color") => {
                ("minimap_compass_north", map_palette(value))
            }
            (false, "minimap_light_overlay_color") => ("light_overlay_color", map_palette(value)),
            (false, "radar_dots_style") => ("radar_style_legacy", bool_value()),
            (false, "radar_name_scale") => ("radar_name_scale", decimal(value, 1.0, 3.0)),
            (true, "display_coordinates") => ("map_show_coordinates", bool_value()),
            (true, "cave_mode_allowed") => ("world_cave_enabled", bool_value()),
            (true, "display_cave_mode_start") => ("world_cave_ceiling", bool_value()),
            (false, "minimap_terrain_depth") => ("minimap_terrain_depth", bool_value()),
            (true, "terrain_depth") => ("world_terrain_depth", bool_value()),
            (false, "minimap_legible_cave_maps") => ("minimap_legible_caves", bool_value()),
            (true, "legible_cave_maps") => ("world_legible_caves", bool_value()),
            (false, "hide_minimap_under_f3") => ("minimap_hide_in_debug", bool_value()),
            (false, "minimap_terrain_slopes") => ("minimap_terrain_slopes", bounded(value, 0, 3)),
            (true, "terrain_slopes") => ("world_terrain_slopes", bounded(value, 0, 3)),
            (false, "minimap_cave_mode_depth") => ("minimap_cave_depth", bounded(value, 0, 64)),
            (true, "cave_mode_depth") => ("world_cave_depth", bounded(value, 0, 64)),
            (false, "minimap_cave_mode_toggle_timer") => {
                ("minimap_cave_timer", decimal(value, 0.0, 10000.0))
            }
            (true, "cave_mode_toggle_timer") => (
                "world_cave_timer",
                value
                    .parse::<f64>()
                    .ok()
                    .filter(|n| (0.0..=10.0).contains(n))
                    .map(|n| (n * 1000.).to_string()),
            ),
            (true, "auto_cave_mode") => ("world_auto_cave", bounded(value, -1, 3)),
            (false, "minimap_undiscovered_opacity") => {
                ("minimap_undiscovered", bounded(value, 0, 100))
            }
            (false, "tracked_players_on_minimap") => ("radar_locator", bool_value()),
            (false, "tracked_players_in_world") => ("locator_world", bool_value()),
            (false, "hide_minimap_under_screen") => ("minimap_hide_screens", bool_value()),
            (true, "display_map_distances") => ("world_map_distances", bool_value()),
            (true, "display_tracked_players") => (
                "world_locator_mode",
                toggle(value).map(|show| if show { "2".to_owned() } else { "1".to_owned() }),
            ),
            (false, "tracked_player_minimap_icon_scale") => {
                ("locator_mini_scale", bounded(value, 0, 17))
            }
            (false, "tracked_player_world_icon_scale") => {
                ("locator_world_scale", bounded(value, 0, 17))
            }
            (false, "tracked_player_world_name_scale") => {
                ("locator_name_scale", bounded(value, 0, 17))
            }
            (true, "convert_map_distances_to_km_at") => {
                ("world_map_km_at", bounded(value, -1, 30000000))
            }
            (true, "map_distance_precision") => ("world_map_dist_precision", bounded(value, 0, 10)),
            (false, "minimap_biomes_in_vanilla_colors") => ("minimap_vanilla_biomes", bool_value()),
            (true, "biome_colors_in_vanilla") => ("world_vanilla_biomes", bool_value()),
            (false, "minimap_biome_blending") => ("minimap_biome_blending", bool_value()),
            (true, "biome_blending") => ("world_biome_blending", bool_value()),
            (false, "minimap_block_transparency") => ("minimap_block_transparency", bool_value()),
            (false, "minimap_display_flowers") => ("minimap_flowers", bool_value()),
            (true, "display_flowers") => ("world_flowers", bool_value()),
            (false, "minimap_display_stained_glass") => ("minimap_stained_glass", bool_value()),
            (true, "display_stained_glass") => ("world_stained_glass", bool_value()),
            (false, "minimap_display_redstone") => ("minimap_redstone", bool_value()),
            (false, "minimap_adjust_height_for_short_blocks") => ("minimap_short_block_height", bool_value()),
            (true, "adjust_height_for_short_blocks") => ("world_short_block_height", bool_value()),
            (false, "minimap_block_colors") => ("minimap_block_colors", bounded(value, 0, 1)),
            (true, "block_colors") => ("world_block_colors", bounded(value, 0, 1)),
            (false, "minimap_ui_scale") => ("minimap_ui_scale", bounded(value, 1, 11)),
            (false, "minimap_further_zoomout") => ("minimap_further_zoomout", bool_value()),
            (false, "minimap_info_display_background_opacity") => ("minimap_info_background", bounded(value, 1, 100)),
            (false, "minimap_info_display_alignment") => ("minimap_info_alignment", bounded(value, 0, 2)),
            (true, "detect_ambiguous_y") => ("world_detect_ambiguous_y", bool_value()),
            (false, "minimap_lighting") => ("minimap_lighting", bool_value()),
            (true, "lighting") => ("world_lighting", bool_value()),
            (false, "minimap_display_world_map_chunks") => ("minimap_world_chunks", bool_value()),
            (false, "minimap_anti_aliasing") => ("minimap_antialias", bool_value()),
            (false, "hide_world_names") => ("hide_world_names", bounded(value, 0, 2)),
            (false, "minimap_safe_mode") => ("minimap_safe_mode", bool_value()),
            (false, "minimap_open_slime_chunks_screen") => ("slime_screen_on_key", bool_value()),
            (false, "switch_auto_waypoints_on_death") => ("auto_subworld_on_death", bool_value()),
            (false, "minimap_display_opac_claims") => ("minimap_claims", bool_value()),
            (true, "display_opac_claims") => ("world_claims", bool_value()),
            (false, "minimap_display_opac_current_claim") => ("minimap_current_claim", bool_value()),
            (false, "discovered_waystone_waypoints") => ("waystones_discovered", bool_value()),
            (false, "other_waystone_waypoints") => ("waystones_other", bool_value()),
            (false, "minimap_opac_claim_fill_opacity") => ("minimap_claim_fill", bounded(value, 1, 100)),
            (false, "minimap_opac_claim_border_opacity") => ("minimap_claim_border", bounded(value, 1, 100)),
            (true, "opac_claims_fill_opacity") => ("world_claim_fill", bounded(value, 1, 100)),
            (true, "opac_claims_border_opacity") => ("world_claim_border", bounded(value, 1, 100)),
            (false, "hud_render_layer") => ("minimap_hud_layer", bounded(value, 0, 100)),
            (false, "multiple_waypoints_info") => ("waypoint_multiple_info", bounded(value, 0, 2)),
            (false, "boss_health_push_box") => ("minimap_boss_push", bounded(value, 0, 2)),
            (false, "potion_effect_push_box") => ("minimap_effect_push", bounded(value, 0, 2)),
            (true, "default_cave_mode_type") => ("world_cave_type", bounded(value, 0, 2)),
            (true, "load_new_chunks") => ("map_load_chunks", bool_value()),
            (true, "update_chunks") => ("map_update_chunks", bool_value()),
            (true, "force_fast_map_writing") => ("map_fast_writes", bool_value()),
            (true, "display_server_chunk_radius") => ("map_server_radius", bool_value()),
            (true, "opening_animation") => ("map_open_animation", bool_value()),
            (false, "minimap_compass_over_everything") => ("minimap_compass_front", bool_value()),
            (false, "new_waypoints_to_bottom") => ("waypoints_new_bottom", bool_value()),
            (true, "map_writing_distance") => ("map_write_radius", bounded(value, -1, 32)),
            (true, "footsteps") => ("footsteps", boolean_choice(value, false)),
            (true, "display_minimap_radar" | "displayMinimapRadar") => {
                ("radar_world_map", bool_value())
            }
            (true, "render_waypoints" | "renderWaypoints") => ("world_map_waypoints", bool_value()),
            (true, "display_disabled_waypoints") => ("world_show_disabled_waypoints", bool_value()),
            (true, "png_export_in_multiple_images") => ("world_png_regions", bool_value()),
            (true, "png_export_night") => ("world_png_night", bool_value()),
            (true, "png_export_highlights") => ("world_png_claims", bool_value()),
            (true, "png_export_scale_down_square") => ("world_png_max_edge", value.parse::<u16>().ok()
                .filter(|regions| *regions <= 90)
                .map(|regions| if regions == 0 { 4096 } else { regions.saturating_mul(512).min(4096) }.to_string())),
            (true, "cave_mode_start_y") => ("world_cave_y", if value == "2147483647" {
                Some("320".to_owned())
            } else {
                bounded(value, -64, 319)
            }),
            (true, "map_teleport_allowed") => ("map_teleport_allowed", bool_value()),
            (true, "partial_y_teleport") => ("map_partial_y", bool_value()),
            _ => ("", None),
        };
        match setting {
            (key, Some(value)) => {
                out.settings.insert(key, value);
            }
            _ => out.unsupported.push(key.chars().take(80).collect()),
        }
    }
    if let Some(height) = height.as_ref() {
        out.settings.insert("cave_y", height.clone());
    }
    let mode = if cave_allowed == Some(false) {
        Some(1)
    } else if automatic.is_some_and(|mode| mode > 0) {
        out.settings
            .insert("minimap_cave_roof", automatic.unwrap().to_string());
        Some(0)
    } else if automatic == Some(0) {
        Some(if height.is_some() { 3 } else { 1 })
    } else {
        None
    };
    if let Some(mode) = mode {
        out.settings.insert("cave_view", mode.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_modern_and_legacy_settings_and_reports_missing_options() {
        let mut import = Import::default();
        parse(
            "# modern\ndisplay_minimap = false\nminimap_north_locked=true\nminimap_shape=1\n\
            minimap_auto_cave_mode = 0\nminimap_manual_cave_mode_start = -17\nminimap_light_overlay_type=2\n\
            radar_hide_invisible_entities=false\nminimap_info_display_config=separate.cfg\nminimap_opacity=900",
            false,
            &mut import,
        );
        parse(
            "footsteps:true\ndisplayMinimapRadar:true",
            true,
            &mut import,
        );
        assert_eq!(&import.settings["enabled"], "1");
        assert_eq!(&import.settings["rotate"], "0");
        assert_eq!(&import.settings["shape"], "1");
        assert_eq!(&import.settings["cave_view"], "3");
        assert_eq!(&import.settings["cave_y"], "-17");
        assert_eq!(&import.settings["light_source"], "1");
        assert_eq!(&import.settings["radar_hide_invisible"], "false");
        assert_eq!(&import.settings["footsteps"], "1");
        assert_eq!(&import.settings["radar_world_map"], "true");
        assert_eq!(
            import.unsupported,
            ["minimap_info_display_config", "minimap_opacity"]
        );
    }

    #[test]
    fn imports_an_exact_size_zoom_and_cave_roof() {
        let mut import = Import::default();
        parse(
            "minimap_size=177\nminimap_zoom=3\nminimap_auto_cave_mode=3",
            false,
            &mut import,
        );
        assert_eq!(import.settings["minimap_exact_size"], "177");
        let zoom = import.settings["zoom"].parse::<usize>().unwrap();
        assert_eq!(Settings::options(4)[zoom].blocks_per_pixel, 1. / 3.);
        assert_eq!(import.settings["minimap_cave_roof"], "3");
        assert!(import.unsupported.is_empty());
    }

    #[test]
    fn reads_a_native_world_map_profile_and_its_export_preferences() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("trailgrid-xaero-layout-{}-{nonce}", std::process::id()));
        let world = directory.join("world-map");
        std::fs::create_dir_all(world.join("profiles")).unwrap();
        std::fs::write(world.join("client.cfg"), concat!(
            "\u{feff}current_profile = Building\n",
            "png_export_scale_down_square=3\npng_export_in_multiple_images=true\n",
            "png_export_night=true\npng_export_highlights=true\n",
            "cave_mode_start_y=2147483647\ndisplay_disabled_waypoints=true",
        )).unwrap();
        std::fs::write(world.join("profiles/Building.cfg"), "render_waypoints=false").unwrap();
        let import = read(&directory).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
        assert_eq!(import.settings["world_map_waypoints"], "false");
        assert_eq!(import.settings["world_show_disabled_waypoints"], "true");
        assert_eq!(import.settings["world_png_max_edge"], "1536");
        assert_eq!(import.settings["world_png_regions"], "true");
        assert_eq!(import.settings["world_png_night"], "true");
        assert_eq!(import.settings["world_png_claims"], "true");
        assert!(!import.settings.contains_key("cave_y"));
        assert_eq!(import.settings["world_cave_y"], "320");
        let mut import = import;
        parse("minimap_manual_cave_mode_start=-15", false, &mut import);
        parse("cave_mode_start_y=40", true, &mut import);
        assert_eq!(import.settings["cave_y"], "-15");
        assert_eq!(import.settings["world_cave_y"], "40");
        assert!(import.unsupported.is_empty());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoDisplay {
    pub id: String,
    pub foreground: Option<u32>,
    pub background: Option<u32>,
}

fn parse_info_display(text: &str) -> Option<Vec<InfoDisplay>> {
    let mut order = None;
    let mut entries = BTreeMap::new();
    for line in text.lines().take(256) {
        if let Some(ids) = line.strip_prefix("infoDisplayOrder") {
            order = Some(ids.trim_start_matches(':').split(':').filter(|id| !id.is_empty()).take(32).collect::<Vec<_>>());
        } else if let Some(line) = line.strip_prefix("infoDisplay:") {
            let mut fields = line.split(':');
            let (Some(id), Some(state), Some(fg), Some(bg)) =
                (fields.next(), fields.next(), fields.next(), fields.next()) else { continue; };
            if matches!(state, "false" | "0") { continue; }
            let color = |v: &str| v.parse::<i32>().ok().map(|v| v as u32 & 0xff_ffff);
            entries.insert(id, InfoDisplay { id: id.to_owned(), foreground: color(fg), background: color(bg) });
        }
    }
    Some(order?.into_iter().filter_map(|id| entries.remove(id)).collect())
}

pub fn read_info_display(directory: &Path) -> io::Result<Option<Vec<InfoDisplay>>> {
    for relative in ["info_display_config/minimap.cfg.txt", "info_display_config.txt", "xaerominimap.txt"] {
        let file = match File::open(directory.join(relative)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut source = String::new();
        file.take(262_145).read_to_string(&mut source)?;
        if source.len() > 262_144 {
            return Err(io::Error::other("Xaero info display exceeds 256 KiB"));
        }
        if let Some(displays) = parse_info_display(&source) {
            return Ok(Some(displays));
        }
    }
    Ok(None)
}
