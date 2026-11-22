// SPDX-License-Identifier: GPL-3.0-only
use mod_api::{self, Hooks, KeyRegistration, Registrar, RegistrationContext, SettingSpec};
pub mod radar;
pub mod map_claims;
pub mod runtime;
pub mod xaero_cache;
pub mod xaero_settings;
use mod_api::*;

#[derive(Default)]
struct Mod;
static INITIAL: std::sync::OnceLock<runtime::minimap::Settings> = std::sync::OnceLock::new();
pub fn initial_settings() -> runtime::minimap::Settings {
    INITIAL.get().copied().unwrap_or_default()
}

fn legacy_preferences(cx: &RegistrationContext) -> runtime::minimap::Settings {
    use std::io::Read;
    let Some(root) = cx.instance_dir().ok().and_then(native_path) else {
        return runtime::minimap::Settings::default();
    };
    let path = root
        .join("horizon-mods/.horizon-mod-data")
        .join(runtime::MOD_ID)
        .join("settings.ini");
    let mut text = String::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(65536).read_to_string(&mut text);
    }
    runtime::minimap::Settings::decode(&text)
}
fn native_path(directory: &mod_api::InstanceDirectory) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    if let mod_api::InstanceDirectory::Unix(bytes) = directory {
        use std::os::unix::ffi::OsStringExt;
        return Some(std::ffi::OsString::from_vec(bytes.clone()).into());
    }
    #[cfg(windows)]
    if let mod_api::InstanceDirectory::Windows(units) = directory {
        use std::os::windows::ffi::OsStringExt;
        return Some(std::ffi::OsString::from_wide(units).into());
    }
    let _ = directory;
    None
}

#[mod_api::horizon_mod]
impl Hooks for Mod {
    fn register_gui_layers(
        &self,
        _: &RegistrationContext,
        event: &mut RegisterGuiLayersEvent<'_>,
    ) {
        event.layers.push(GuiLayer {
            id: "trailgrid_maps:background".into(),
            placement: GuiLayerPlacement::Below("minecraft:effects".into()),
        });
    }
    fn register(&self, r: &mut dyn Registrar, cx: &RegistrationContext) {
        let mut preferences = legacy_preferences(cx);
        for (key, button) in runtime::SDK_SETTING_ROWS {
            if let Ok(Some(raw)) = cx.setting(runtime::MOD_ID, key) {
                if let Ok(index) = raw.parse() {
                    preferences.select_option(button, index);
                }
            }
        }
        if let Ok(Some(raw)) = cx.setting(runtime::MOD_ID, "cache_ram_mib") {
            if let Ok(value) = raw.parse::<f64>() {
                if value.is_finite() && (16. ..=512.).contains(&value) {
                    preferences.cache_ram_mib = value.round() as u16;
                }
            }
        }
        if let Ok(Some(raw)) = cx.setting(runtime::MOD_ID, "cave_y")
            && let Ok(value) = raw.parse::<f64>()
            && value.is_finite()
        {
            preferences.cave_y = value.round().clamp(-64., 319.) as i32;
        }
        let _ = INITIAL.set(preferences);
        radar::register(r);
        for key in ["minimap_block_colors", "world_block_colors"] {
            r.register_setting(SettingSpec::choice(
                key,
                format!("mwwr.setting.{key}"),
                ["mwwr.value.block_colors.0", "mwwr.value.block_colors.1"],
                0,
            )).expect("valid block colour mode");
        }
        r.register_setting(SettingSpec::choice(
            "minimap_info_alignment",
            "mwwr.setting.minimap_info_alignment",
            ["mwwr.value.info_alignment.0", "mwwr.value.info_alignment.1", "mwwr.value.info_alignment.2"],
            0,
        )).expect("valid minimap text alignment");
        r.register_setting(SettingSpec::choice(
            "hide_world_names",
            "mwwr.setting.hide_world_names",
            ["mwwr.value.hide_world_names.0", "mwwr.value.hide_world_names.1", "mwwr.value.hide_world_names.2"],
            0,
        )).expect("valid world name visibility");
        for (key, values, default) in [
            ("waypoint_multiple_info", ["mwwr.value.multiple.0", "mwwr.value.multiple.1", "mwwr.value.multiple.2"], 1),
            ("minimap_boss_push", ["mwwr.value.push.0", "mwwr.value.push.1", "mwwr.value.push.2"], 1),
            ("minimap_effect_push", ["mwwr.value.push.0", "mwwr.value.push.1", "mwwr.value.push.2"], 1),
            ("world_cave_type", ["mwwr.value.cave.0", "mwwr.value.cave.1", "mwwr.value.cave.2"], 1),
        ] {
            r.register_setting(SettingSpec::choice(key, format!("mwwr.setting.{key}"), values, default))
                .expect("valid map choice");
        }
        for key in ["slime_chunks", "light_overlay"] {
            r.register_setting(SettingSpec::toggle(key, format!("mwwr.setting.{key}"), false))
                .expect("valid map overlay setting");
        }
        for (key, default) in [
            ("minimap_waypoints", true),
            ("world_map_waypoints", true),
            ("world_show_disabled_waypoints", false),
            ("world_png_regions", false),
            ("world_png_night", false),
            ("world_png_claims", false),
            ("all_waypoint_groups", false),
            ("old_deathpoints", true),
            ("temporary_waypoints_global", false),
            ("waypoint_cross_dimension",true),
            ("waypoint_partial_y",true),
            ("quick_minimap_centered",false),
            ("minimap_position_custom",false),
            ("quick_minimap_toggle",false),
            ("map_teleport_allowed",true),
            ("map_partial_y",true),
            ("minimap_vanilla_biomes", false),
            ("world_vanilla_biomes", false),
            ("minimap_biome_blending", true),
            ("world_biome_blending", true),
            ("minimap_block_transparency", true),
            ("minimap_flowers", true),
            ("world_flowers", true),
            ("minimap_stained_glass", true),
            ("world_stained_glass", true),
            ("minimap_redstone", true),
            ("minimap_safe_mode", false),
            ("slime_screen_on_key", true),
            ("auto_subworld_on_death", true),
            ("minimap_claims", true),
            ("world_claims", true),
            ("minimap_current_claim", true),
            ("waystones_discovered", true),
            ("waystones_other", true),
            ("minimap_lighting", true),
            ("minimap_world_chunks", true),
            ("minimap_antialias", false),
            ("world_lighting", true),
            ("minimap_further_zoomout", false),
            ("world_detect_ambiguous_y", true),
            ("minimap_short_block_height", true),
            ("world_short_block_height", true),
            ("map_load_chunks",true),
            ("map_update_chunks",true),
            ("map_fast_writes",false),
            ("map_server_radius",false),
            ("map_open_animation",false),
            ("minimap_compass_front",true),
            ("waypoints_new_bottom",true),

            ("world_hover_biome",true),
            ("world_zoom_readout",false),
            ("world_zoom_buttons",false),
            ("world_marker_arrow",true),
            ("world_waypoint_backgrounds",true),
            ("world_waypoint_distance_scale",false),
            ("waypoint_short_distance",false),
            ("waypoint_hide_coords",false),
            ("waypoint_name_enabled",true),
            ("waypoint_menu_distance",false),
            ("minimap_unlocked_enlarged",false),
            ("world_skip_render",false),
            ("map_show_coordinates",true),
            ("world_cave_enabled",true),
            ("world_cave_ceiling",false),
            ("minimap_terrain_depth",true),
            ("world_terrain_depth",true),
            ("minimap_legible_caves",false),
            ("world_legible_caves",false),
            ("minimap_hide_in_debug",false),
            ("minimap_cave_surface",true),
            ("minimap_cave_nether",true),
            ("minimap_cave_end",true),
            ("world_cave_surface",true),
            ("world_cave_nether",true),
            ("world_cave_end",true),

            ("use_xaero_radar",false),
            ("radar_locator",false),
            ("locator_world",false),
            ("minimap_hide_screens",false),
            ("world_map_distances",false),

        ] {
            r.register_setting(SettingSpec::toggle(key, format!("mwwr.setting.{key}"), default))
                .expect("valid waypoint display setting");
        }
        r.register_setting(SettingSpec::range("minimap_opacity", "mwwr.setting.minimap_opacity", 30.,100.,1.,100.))
            .expect("valid minimap opacity");
        r.register_setting(SettingSpec::choice(
            "minimap_frame", "mwwr.setting.minimap_frame",
            (0..4).map(|i| format!("mwwr.value.minimap_frame.{i}")), 0,
        )).expect("valid minimap frame");
        r.register_setting(SettingSpec::color("minimap_frame_color","mwwr.setting.minimap_frame_color",0x44444a))
            .expect("valid frame color");
        r.register_setting(SettingSpec::choice(
            "light_source",
            "mwwr.setting.light_source",
            ["mwwr.value.light_source.0", "mwwr.value.light_source.1"],
            0,
        )).expect("valid light source");
        for (key, color) in [
            ("minimap_marker_color",0xff5050),
            ("world_marker_color",0xff5050),
            ("minimap_grid_color",0x525f70),
            ("minimap_compass_color",0x000000),
            ("minimap_compass_north",0x000000),
            ("light_overlay_color",0xf7c15d),
        ] {
            r.register_setting(SettingSpec::color(key,format!("mwwr.setting.{key}"),color))
                .expect("valid map color");
        }
        for (key, value) in [("light_min", 0.), ("light_max", 7.)] {
            r.register_setting(SettingSpec::range(key, format!("mwwr.setting.{key}"), 0., 15., 1., value))
                .expect("valid light level");
        }
        for (key, min, max, step, default) in [
            ("waypoint_world_opacity", 0., 100., 1., 100.),
            ("waypoint_map_opacity", 10., 100., 1., 100.),
            ("waypoint_world_scale", 0., 17., 1., 0.),
            ("waypoint_map_scale", 0., 17., 1., 0.),
            ("waypoint_min_distance", 0., 4096., 1., 0.),
            ("waypoint_max_distance", 0., 30_000_000., 1., 0.),
            ("quick_minimap_zoom", 0., 6., 1., 0.),
            ("minimap_position_x", 0., 10000., 1., 10000.),
            ("minimap_position_y", 0., 10000., 1., 0.),
            ("minimap_cave_zoom", 1., 4., 1., 1.),
            ("minimap_exact_size", 0., 250., 1., 0.),
            ("minimap_claim_fill", 1., 100., 1., 46.),
            ("minimap_claim_border", 1., 100., 1., 80.),
            ("world_claim_fill", 1., 100., 1., 46.),
            ("world_claim_border", 1., 100., 1., 80.),
            ("world_png_max_edge", 512., 4096., 512., 4096.),
            ("minimap_hud_layer", 0., 100., 1., 1.),
            ("minimap_ui_scale", 1., 11., 1., 1.),
            ("minimap_info_background", 1., 100., 1., 40.),
            ("minimap_cave_roof", 1., 3., 1., 1.),
            ("map_write_radius",-1.0,32.0,1.0,-1.0),

            ("world_locator_mode",0.0,2.0,1.0,0.0),
            ("locator_mini_scale",0.0,17.0,1.0,0.0),
            ("locator_world_scale",0.0,17.0,1.0,0.0),
            ("locator_name_scale",0.0,17.0,1.0,0.0),
            ("world_map_km_at",-1.0,30000000.0,1.0,-1.0),
            ("world_map_dist_precision",0.0,10.0,1.0,0.0),

            ("minimap_terrain_slopes",0.0,3.0,1.0,2.0),
            ("world_terrain_slopes",0.0,3.0,1.0,2.0),
            ("minimap_cave_depth",0.0,64.0,1.0,0.0),
            ("world_cave_depth",0.0,64.0,1.0,0.0),
            ("world_cave_y",-64.0,320.0,1.0,320.0),
            ("minimap_cave_timer",0.0,10000.0,100.0,0.0),
            ("world_cave_timer",0.0,10000.0,100.0,0.0),
            ("world_auto_cave",-1.0,3.0,1.0,-1.0),
            ("minimap_undiscovered",0.0,100.0,1.0,0.0),

            ("world_waypoint_symbol_scale", 0.5, 5.0, 0.1, 1.0),
            ("world_waypoint_zoom_min", 0.0, 3.0, 0.1, 0.0),
            ("minimap_marker_mode", 0.0, 2.0, 1.0, 2.0),
            ("minimap_marker_size", 1.0, 4.0, 1.0, 2.0),
            ("minimap_arrow_scale", 1.0, 2.0, 0.05, 1.5),
            ("minimap_arrow_opacity", 1.0, 100.0, 1.0, 100.0),
            ("minimap_compass_location", 0.0, 2.0, 1.0, 1.0),
            ("minimap_compass_scale", 0.0, 17.0, 1.0, 0.0),
            ("minimap_grid_width", 1.0, 8.0, 1.0, 1.0),
            ("waypoint_name_scale", 0.0, 17.0, 1.0, 0.0),
            ("waypoint_dist_scale", 0.0, 17.0, 1.0, 0.0),
            ("waypoint_close_scale", 0.125, 8.0, 0.025, 1.0),
            ("waypoint_distance_mode", 0.0, 3.0, 1.0, 3.0),
            ("waypoint_vertical_angle", 1.0, 180.0, 1.0, 180.0),
            ("waypoint_horizontal_angle", 1.0, 180.0, 1.0, 10.0),
            ("waypoint_km_at", -1.0, 30000000.0, 1.0, -1.0),
            ("waypoint_dist_precision", 0.0, 10.0, 1.0, 0.0),
            ("world_unlimited_zoom_in", 0.0, 1.0, 1.0, 0.0),
            ("world_unlimited_zoom_out", 0.0, 1.0, 1.0, 0.0),
        ] {
            r.register_setting(SettingSpec::range(key, format!("mwwr.setting.{key}"), min, max, step, default))
                .expect("valid map display range");
        }
        for (id, default) in [
            (runtime::WORLD_MAP_KEY, runtime::WORLD_MAP_DEFAULT),
            (runtime::WAYPOINTS_KEY, runtime::WAYPOINTS_DEFAULT),
            ("quick_minimap", "key.keyboard.z"),
            ("minimap_zoom_in", "key.keyboard.i"),
            ("minimap_zoom_out", "key.keyboard.o"),
            ("slime_map", "key.keyboard.unknown"),
            ("create_waypoint", "key.keyboard.k"),
            ("next_waypoint_group", "key.keyboard.g"),
            ("map_settings", "key.keyboard.y"),
        ] {
            r.register_key_mapping(
                KeyRegistration::new(id, default).label(&format!("mwwr.key.{id}")),
            );
        }
        for (key, button) in runtime::SDK_SETTING_ROWS {
            let options = runtime::minimap::Settings::options(button)
                .iter()
                .enumerate()
                .map(|(index, _)| format!("mwwr.value.{key}.{index}"))
                .collect::<Vec<_>>();
            r.register_setting(SettingSpec::choice(
                key,
                format!("mwwr.setting.{key}"),
                options,
                preferences.option_index(button).unwrap_or(0),
            ))
            .expect("valid minimap setting");
        }
        r.register_setting(SettingSpec::range(
            "cache_ram_mib",
            "mwwr.setting.cache_ram_mib",
            16.,
            512.,
            1.,
            f64::from(preferences.cache_ram_mib),
        ))
        .expect("valid RAM setting");
        r.register_setting(SettingSpec::range(
            "cave_y",
            "mwwr.setting.cave_y",
            -64.,
            319.,
            1.,
            f64::from(preferences.cave_y),
        )).expect("valid cave height setting");
        for key in ["cache_manager", "reset"] {
            r.register_setting(SettingSpec::choice(
                key,
                format!("mwwr.setting.{key}"),
                [format!("mwwr.action.{key}"), format!("mwwr.action.{key}")],
                0,
            ))
            .expect("valid minimap action");
        }
    }

    fn client_player_logging_out(&self, _: &mut dyn ClientHost, _: &mut ClientPlayerLoggingOutEvent) {
        radar::clear();
        runtime::take_shared_waypoint();
    }

    fn register_client_commands(&self, _: &mut dyn ClientHost, event: &mut RegisterClientCommandsEvent<'_>) {
        event.commands.push(Command::new("horizon_map_accept <waypoint:text>"));
        event.commands.push(Command::new("mapdeaths"));
    }

    fn command(&self, host: &mut dyn ClientHost, event: &mut CommandEvent<'_>) -> Flow {
        if event.command == "mapdeaths" {
            let world = host.client_session().client_server().and_then(|server| {
                if server.integrated_local {
                    server.save_folder.and_then(|folder| runtime::waypoint_world(true, &folder))
                } else {
                    server.address.and_then(|address| runtime::waypoint_world(false, &address)).map(runtime::active_world)
                }
            });
            let points = world.as_deref().map_or_else(Vec::new, runtime::death_history);
            let heading = if points.is_empty() {
                runtime::tr("No death points in this world.", "В этом мире пока нет точек смерти.",
                    "Aucune mort dans ce monde.", "No hay muertes en este mundo.", "Keine Todespunkte in dieser Welt.")
            } else {
                runtime::tr("Death points · newest first", "Точки смерти · сначала последняя",
                    "Morts récentes", "Muertes recientes", "Letzte Todespunkte")
            };
            let _ = host.chat_line(Text::literal(heading));
            for (index, point) in points.iter().enumerate() {
                let _ = host.chat_line(Text::literal(format!(
                    "{}. {} · X {}  Y {}  Z {}", index + 1, point.dimension.label(), point.x, point.y, point.z,
                )));
            }
            return Flow::Cancel;
        }
        let Some(link) = event.command.strip_prefix("horizon_map_accept ") else { return Flow::Continue };
        if let Some(server) = host.client_session().client_server() {
            let world = if server.integrated_local {
                server.save_folder.and_then(|folder| runtime::waypoint_world(true,&folder))
            } else {
                server.address.and_then(|address| runtime::waypoint_world(false,&address)).map(runtime::active_world)
            };
            let dimension = runtime::Dimension::from_id(host.dimension());
            if let Some(world) = world
                && let Some(point) = runtime::receive_shared_waypoint(link,&world,dimension)
            {
                runtime::queue_shared_waypoint(point);
            }
        }
        Flow::Cancel
    }

    fn client_chat_received(
        &self,
        host: &mut dyn ClientHost,
        event: &mut ClientChatReceivedEvent<'_>,
    ) -> Flow {
        if event.overlay {
            return Flow::Continue;
        }
        let Ok(line) = host.text_string(event.message) else { return Flow::Continue };
        let dimension = runtime::Dimension::from_id(host.dimension());
        if event.sender == 0 {
            map_claims::receive(&line, dimension);
        }
        if let Some(point) = runtime::receive_shared_waypoint(&line, "server:incoming", dimension) {
            use mod_api::text::{TextClick,TextStyle};
            let link = point.share_link();
            let mut save_style = TextStyle::default();
            save_style.click = Some(TextClick::RunCommand(format!("/horizon_map_accept {link}")));
            save_style.underlined = Some(true);
            let mut copy_style = TextStyle::default();
            copy_style.click = Some(TextClick::CopyToClipboard(link));
            copy_style.underlined = Some(true);
            let add = runtime::tr(" [Add waypoint]", " [Добавить метку]", " [Ajouter le repère]",
                " [Añadir marcador]", " [Wegpunkt hinzufügen]");
            let copy = runtime::tr(" [Copy waypoint]", " [Копировать метку]", " [Copier le repère]",
                " [Copiar marcador]", " [Wegpunkt kopieren]");
            *event.message = std::mem::take(event.message)
                .append(Text::literal(add).with_style(save_style))
                .append(Text::literal(copy).with_style(copy_style));
        }
        Flow::Continue
    }

    // The engine skips these entry points unless an SDK listener subscribes.
    fn render_gui_post(&self, host: &dyn ClientHost, event: &mut RenderGuiPostEvent<'_>) {
        let _ = host;
        let _ = event;
    }
    fn gui_render_post(&self, host: &dyn ClientHost, event: &mut GuiRenderPostEvent<'_>) {
        let _ = host;
        let _ = event;
    }
    fn screen_render_post(&self, host: &dyn ClientHost, event: &mut ScreenRenderPostEvent<'_>) {
        let _ = host;
        let _ = event;
    }
    fn screen_mouse_button_pressed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseButtonPressedPreEvent<'_>,
    ) -> Flow {
        let _ = host;
        let _ = event;
        Flow::Continue
    }
    fn screen_mouse_dragged_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseDraggedPreEvent<'_>,
    ) -> Flow {
        let _ = host;
        let _ = event;
        Flow::Continue
    }
    fn screen_mouse_scrolled_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenMouseScrolledPreEvent<'_>,
    ) -> Flow {
        let _ = host;
        let _ = event;
        Flow::Continue
    }
    fn screen_key_pressed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenKeyPressedPreEvent<'_>,
    ) -> Flow {
        let _ = host;
        let _ = event;
        Flow::Continue
    }
    fn screen_char_typed_pre(
        &self,
        host: &dyn ClientHost,
        event: &mut ScreenCharTypedPreEvent<'_>,
    ) -> Flow {
        let _ = host;
        let _ = event;
        Flow::Continue
    }
    fn menu_background(&self, host: &dyn ClientHost, event: &mut MenuBackgroundEvent<'_>) {
        let _ = host;
        let _ = event;
    }
    fn client_disconnecting(&self, host: &dyn ClientHost, event: &mut ClientDisconnectingEvent) {
        let _ = host;
        let _ = event;
    }
    fn client_resources_reloaded(
        &self,
        host: &mut dyn ClientHost,
        event: &mut ClientResourcesReloadedEvent,
    ) {
        let _ = host;
        let _ = event;
    }
    fn client_stopping(
        &self,
        _host: &mut dyn mod_api::ClientHost,
        _event: &mut mod_api::ClientStoppingEvent,
    ) {
        runtime::shutdown();
    }
}

#[cfg(test)]
mod minimap_container_tests;
#[cfg(test)]
mod minimap_map_regression_tests;
#[cfg(test)]
mod minimap_maps_tests;
#[cfg(test)]
mod minimap_settings_tests;

pub fn mod_icon() -> &'static [u8] {
    include_bytes!("../icon.png")
}
