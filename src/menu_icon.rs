// SPDX-License-Identifier: GPL-3.0-only
fn menu_icon(game: &mut crate::game::Game<'_>) {
    if let Some(info) = game.settings.mods.info.get_mut(::trailgrid_maps::runtime::MOD_ID) {
        info.icon = Some(::trailgrid_maps::mod_icon().into());
    }
}
