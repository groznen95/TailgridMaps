// SPDX-License-Identifier: GPL-3.0-only
fn on_map_key(
    game: &mut crate::game::Game<'_>,
    key: &str,
    action: mod_api::KeyAction,
    modifiers: u32,
) {
    use crate::game::minimap_compat::MapAccess;
    use ::trailgrid_maps::runtime;
    if game.native_window()
        && game.map_on(runtime::MOD_ID)
        && game.mod_key(runtime::MOD_ID, "quick_minimap")
            .is_some_and(|(bound, mods)| bound == key && (action != mod_api::KeyAction::Press || mods.bits & 7 == modifiers & 7))
    {
        crate::game::reworked_maps::quick_minimap_key(
            action,
            game.playing() && !runtime::text_input_active(),
            game.map_option("quick_minimap_toggle") == Some(1),
        );
        return;
    }
    if action != mod_api::KeyAction::Press
        || !game.native_window()
        || !game.map_on(runtime::MOD_ID)
        || runtime::text_input_active()
    {
        return;
    }
    if key != "key.keyboard.unknown" && game.playing() && game.mod_screen().is_none() {
        for (id, nearer) in [("minimap_zoom_in", true), ("minimap_zoom_out", false)] {
            if game.mod_key(runtime::MOD_ID, id)
                .is_some_and(|(bound, mods)| bound == key && mods.bits & 7 == modifiers & 7)
            {
                let mut settings = runtime::minimap::Settings::default();
                if let Some(index) = game.map_option("zoom") {
                    settings.select_option(4, usize::from(index));
                }
                if let Some(index) = settings.zoom_step(nearer) {
                    crate::game::minimap_compat::write_map_setting(game, "zoom", &index.to_string());
                }
                return;
            }
        }
    }
    if key != "key.keyboard.unknown" && game.playing() && game.mod_screen().is_none()
        && game.mod_key(runtime::MOD_ID, "next_waypoint_group")
            .is_some_and(|(bound, mods)| bound == key && mods.bits & 7 == modifiers & 7)
    {
        crate::game::reworked_maps::cycle_waypoint_group(game);
        return;
    }
    if key != "key.keyboard.unknown" && game.mod_key(runtime::MOD_ID, "slime_map")
        .is_some_and(|(bound, mods)| bound == key && mods.bits & 7 == modifiers & 7)
        && game.playing() && game.mod_screen().is_none()
    {
        if game.map_option("slime_screen_on_key") == Some(0) {
            crate::game::reworked_maps::toggle_slime_overlay();
        } else if crate::game::reworked_maps::check_map_item(game, true) && game.mod_screen_open(runtime::MOD_ID) {
            runtime::request_screen(runtime::ScreenRequest::Slime);
        }
        return;
    }
    let chosen = [
        (runtime::WORLD_MAP_KEY, runtime::ScreenRequest::Map),
        (runtime::WAYPOINTS_KEY, runtime::ScreenRequest::Waypoints),
        ("create_waypoint", runtime::ScreenRequest::Create),
        ("map_settings", runtime::ScreenRequest::Settings),
    ]
    .into_iter()
    .find(|(id, _)| {
        game.mod_key(runtime::MOD_ID, id)
            .is_some_and(|(bound, mods)| bound == key && mods.bits & 7 == modifiers & 7)
    });
    let Some((_, request)) = chosen else {
        return;
    };
    if game.mod_screen() == Some(runtime::MOD_ID) {
        if request == runtime::ScreenRequest::Settings {
            runtime::request_screen(runtime::ScreenRequest::SettingsOverlay);
            return;
        }
        if request == runtime::ScreenRequest::Create {
            runtime::request_screen(request);
            return;
        }
        game.mod_screen_close(runtime::MOD_ID);
        return;
    }
    if !game.playing() || !crate::game::reworked_maps::check_map_item(game,matches!(request,runtime::ScreenRequest::Map)) {
        return;
    }
    if game.mod_screen_open(runtime::MOD_ID) {
        runtime::request_screen(request);
    }
}
