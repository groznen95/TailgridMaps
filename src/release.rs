// SPDX-License-Identifier: GPL-3.0-only
fn map_release(
    game: &crate::game::Game<'_>,
    screen: &mod_api::ScreenView<'_>,
    button: i32,
    canceled: &mut bool,
) {
    crate::game::reworked_maps::release(game, screen, button, canceled);
}
