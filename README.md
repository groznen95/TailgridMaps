# Kadir's Map

Kadir's Map is a client-side Horizon mod with a configurable minimap, a world map that remembers explored terrain, a searchable waypoint list, and entity radar. Map data stays in your local Horizon profile, with separate caches for each world and server.

The map, minimap, waypoint list, settings, radar editor and cache manager share the same interface. Active controls have a turquoise outline. A filled turquoise button marks the main action, and destructive confirmations are amber.

| Default key | Action |
| --- | --- |
| `M` | Open or close the world map. Drag to pan and scroll to zoom. |
| `N` | Open the waypoint list. Filter by group or dimension, search by name, and sort the results. Press Enter to show the selected waypoint or F2 to edit it. |
| `K` | Add a waypoint at your current position. |
| `Y` | Open or close minimap settings. The map toolbar opens world-map settings directly. |
| `Z` | Enlarge the minimap. |
| `I` / `O` | Zoom the minimap in or out. Both keys can be rebound. |
| `G` | Select the next waypoint group. |

In **Y → Minimap → Change position**, drag the minimap or use the arrow keys to move it one GUI pixel at a time. **Save** keeps the new position, and **Use corner** returns to the selected corner. At wide GUI sizes, settings are arranged in two columns. Scroll to see more, right-click a choice to step backward, or drag a slider to adjust it. Use **Ctrl+F** or **Command+F** to search across every settings tab.

In **Y → Radar → Radar categories**, open a category to edit its filters and styling. The first matching sibling takes priority. Right-click a field to clear its override. The Include and Exclude fields take Xaero selector lists, for example `["minecraft:zombie","player;Steve"]`. **Save and apply** stores a separate copy per settings profile. The imported Xaero file stays available.

Enable radar icons to see textured 3D renderings of mob models, including their appearance variants and overlay layers. If you enable a resource pack with Xaero icon definitions in Horizon, Kadir's Map can also display `sprite`, `normal_sprite`, `outlined_sprite`, fixed `item`, and `dot` icons. Model definitions can select parts of Horizon's mob models and adjust the view. Custom Java variant methods are unavailable in Horizon.

In **N → Select multiple**, click waypoints or select the current search results. You can move the selection to another group, change visibility and 3D markers, or confirm deletion. Use **Local / Global** on a row to control whether that waypoint follows the maximum 3D draw distance. **Open in chat** fills in a Xaero-compatible link for you to review and send. At large GUI scales, open **Share** to access the copy, paste, and chat actions.

**Delete** asks for confirmation when deleting one or more waypoints. Press **Esc** to dismiss the confirmation.

When you create or edit a waypoint, the name is selected so you can replace it by typing. Use **Tab** to move between the name and group fields. The active field supports select all, copy, cut and paste. Press **Enter** to save.

Open the **Waypoints** panel with the button in the bottom-right corner of the world map. Click a marker to center the map, or right-click it for editing and sharing. The panel has controls for the active group, all groups, and disabled markers. The search text is shared with the waypoint list. Use **Ctrl+F** or **Command+F** to focus search. Use **M** and **N** to switch between map and list.

At 4× zoom and above, the world map samples each loaded surface block's top texture at 4 × 4 pixels. Existing cache data supplies the flat map colors for terrain outside the client’s loaded chunks. At close zoom, the surface map tilts using cached heights. Drag with the right mouse button to rotate or change the angle. Left-drag still pans, and a short right click opens the map menu. The mountain button turns tilt off or back on. Both features also have switches in **Y → World map**.

The world map remembers whether to show disabled markers. When using a manual cave layer, its height can be set separately from the minimap under **Y → World map**.

Under **Y → World map**, choose a maximum PNG size, export a separate 512 × 512 image for each explored region, apply a night tint to surface maps, or include the current claim overlay. **M → Export PNG** saves the explored terrain of the selected layer to the mod's `cache/exports` directory. Region filenames contain their X and Z coordinates. Single-image exports are bounded to 4096 pixels per side. The night tint darkens the saved surface colors.

Open the world map with **M**, right-click, and choose **Build map from world save** to fill gaps in the surface map from saved regions in the Overworld, Nether, and End. The scan runs in the background and preserves existing map pixels and waypoints. It reads the active single-player save. For a multiplayer map, place a copy of the entire server save, including `level.dat`, in `world-import` inside the mod data directory, then run the scan from that server's map. Use a save that matches the selected map. The scan leaves the source files untouched and uses chunks already generated in the save.

On multiplayer servers, the map follows the level ID and hashed seed sent at login and respawn. The first visit after updating binds to your existing selection. Normal visits to the Overworld, Nether and End keep those layers together. If the server reuses a level ID with a different seed hash, or introduces a distinct custom level ID, a separate map is available with its own terrain, waypoints, groups and death history.

In **M → Server worlds**, choose an existing map or create a named one to change the binding for your current level. **Default** opens the original map for the server address. If the server sends identical level IDs and seed hashes for different worlds, switch through this menu.

The defaults are **K** to create a waypoint and **N** to open the list. Horizon uses **B** for its emote wheel, and Freecam+ uses **U**. You can change the map shortcuts in Controls.

Automatic death skulls appear at the death block after you respawn. Kadir's Map uses the server position when available and remembers the position from the death packet otherwise. In **M → Last death**, the world map jumps to the most recent death in that world. **/mapdeaths** shows the last ten saved entries from that world. New profiles retain points until you delete them. An optional waypoint setting can remove a point after you leave its area and return.

The interface follows your game language and defaults to English. To use the release in English, choose **English (US)** in Minecraft's language settings. Saved waypoint names stay as entered.

## Install

Version 0.1.17 targets **Horizon Nightly 2026-11-22 (`f1bb66f`)** and **Minecraft Java Edition 26.3**. Horizon Launcher builds the native source `.hmod` with the signed SDK for that Nightly. To install the package manually, put it at `horizon-native-mods/trailgrid_maps.hmod` inside the instance directory. Enable Kadir's Map and build native mods for that instance.

Use [v0.1.10](https://github.com/groznen95/trailgrid/releases/tag/v0.1.10) for Nightly 2026-11-21 (`8f83e5c`) or [v0.1.0](https://github.com/groznen95/trailgrid/releases/tag/v0.1.0) for Nightly 2026-11-20 (`0a875da`). Updates use the same mod ID and data directory.

Radar shows entities the client receives. Map teleport requires server permissions. On Horizon SMP the claim overlay can query `/claims` and `/claim info`; Waystones and Open Parties and Claims overlays depend on compatible data from the server. Version 0.1.7 was checked against the live Horizon SMP map and against separate test worlds on the same LAN address. See [VALIDATION](VALIDATION.md).

On multiplayer maps, click the grid icon beside **Server worlds** to show or hide other players’ claims. On Horizon SMP, the overlay shows nearby chunks reported by the server. Your claims appear in green after the server confirms `/claim` and disappear after it confirms `/unclaim`. Enable **Show other players’ claims** in World map settings to show foreign claims in amber.

## Copy maps and waypoints from a previous build

Close the game and use a profile with only one mapping mod enabled. Kadir's Map has its own mod ID and data directory. Before opening a world with it, copy the contents of `horizon-mods/.horizon-mod-data/minimap_worldmap_waypoints_reworked/` to `horizon-mods/.horizon-mod-data/trailgrid_maps/` in the target profile. Keep the original directory as a backup. For an existing Trailgrid profile, back up both directories and copy selected files explicitly to avoid replacing newer data. Use the same save folder name or server address so the copied map and waypoints resolve to the same world. If Horizon reports the old mod ID as missing in a single-player save, choose **Back Up and Open** after checking that the list only names the old map mod. Rebind custom keys in the new profile as needed.

The world map's import tools can also read supported waypoint, settings and cache formats from existing Xaero's Minimap and Xaero's World Map data. The Java mods themselves are separate products.

To import modern settings, copy Xaero's `minimap` and `world-map` folders from `config/xaero` into the mod data folder's `xaero-config` directory. Keep `client.cfg` and `profiles` in each folder. Use **Import Xaero settings** from the world map's right-click menu. The importer reads the selected profile and supported client options, including the world-map PNG settings and disabled marker visibility. It also accepts the flat `minimap.cfg` and `worldmap.cfg` files used by earlier Kadir's Map versions.

## Source and license

Kadir's Map is maintained by Kadir (groznen95) and licensed under [GPL-3.0-only](LICENSE). See [NOTICE](NOTICE) and [LICENSE-MIT](LICENSE-MIT) for upstream attribution and license terms. The icon is original to Kadir's Map. The release package contains the Rust source and assets used by the launcher.

Compatibility with existing data refers to [Xaero's Minimap](https://www.curseforge.com/minecraft/mc-mods/xaeros-minimap) and [Xaero's World Map](https://www.curseforge.com/minecraft/mc-mods/xaeros-world-map) by Xaero. Kadir's Map is an independent project.
