# Kadir's Map

Kadir's Map is a client-side Horizon mod with a configurable minimap, a world map that remembers explored terrain, a searchable waypoint list, and entity radar. Map data stays in your local Horizon profile, with separate caches for each world and server.

| Default key | Action |
| --- | --- |
| `M` | Open the world map. Drag to pan and scroll to zoom. |
| `N` | Open the waypoint list. Filter by group or dimension, search by name, and sort the results. Press Enter to show the selected waypoint on the map. |
| `K` | Add a waypoint at your current position. |
| `Y` | Open map settings. The same settings are available from the map and waypoint list. |
| `Z` | Enlarge the minimap. |
| `I` / `O` | Zoom the minimap in or out. Both keys can be rebound. |
| `G` | Select the next waypoint group. |

In **Y → Minimap → Change position**, drag the minimap or use the arrow keys to move it one GUI pixel at a time. **Save** keeps the new position, and **Use corner** returns to the selected corner.

In **Y → Radar → Radar categories**, open a category to edit its filters and styling. The first matching sibling takes priority. Right-click a field to clear its override. The Include and Exclude fields take Xaero selector lists, for example `["minecraft:zombie","player;Steve"]`. **Save and apply** stores a separate copy per settings profile. The imported Xaero file stays available.

Enable a resource pack containing Xaero icon definitions in Horizon’s resource pack settings, then enable radar icons. Kadir's Map reads `sprite`, `normal_sprite`, `outlined_sprite`, fixed `item`, and `dot` forms. It selects sprites by the default definition or matching entity texture. Java model-part overrides and variant methods fall back to Horizon’s own radar icon renderer when required.

In **N → Select multiple**, click waypoints or select the current search results. You can move the selection to another group, change visibility and 3D markers, or confirm deletion. Use **Local / Global** on a row to control whether that waypoint follows the maximum 3D draw distance. **Open in chat** fills in a Xaero-compatible link for you to review and send.

The **Waypoints** panel on the world map has its own search field and controls for the active group, all groups, and disabled markers. The search text is shared with the waypoint list.

On multiplayer servers, the map follows the level ID and hashed seed sent at login and respawn. The first visit after updating binds to your existing selection. Normal visits to the Overworld, Nether and End keep those layers together. If the server reuses a level ID with a different seed hash, or introduces a distinct custom level ID, a separate map is available with its own terrain, waypoints, groups and death history.

In **M → Server worlds**, choose an existing map or create a named one to change the binding for your current level. **Default** opens the original map for the server address. If the server sends identical level IDs and seed hashes for different worlds, switch through this menu.

The default waypoint key changed from **B** to **K** because Horizon uses **B** for its emote wheel. If you saved **B** as a custom shortcut, change it in Controls.

Automatic death skulls appear at the death block after you respawn. Kadir's Map uses the server position when available and remembers the position from the death packet otherwise. In **M → Last death**, the world map jumps to the most recent death in that world. **/deaths** shows the last ten saved entries from that world. New profiles retain points until you delete them. An optional waypoint setting can remove a point after you leave its area and return.

The interface follows your game language and defaults to English. To use the release in English, choose **English (US)** in Minecraft's language settings. Saved waypoint names stay as entered.

## Install

Version 0.1.7 targets **Horizon Nightly 2026-11-21 (`8f83e5c`)** and **Minecraft Java Edition 26.3**. It is a native source `.hmod`; Horizon Launcher builds it with the signed SDK for that exact Nightly. To install the release manually, put the package at `horizon-native-mods/trailgrid_maps.hmod` inside the instance directory. Enable Kadir's Map and build native mods for that instance.

Use [v0.1.0](https://github.com/groznen95/trailgrid/releases/tag/v0.1.0) for Nightly 2026-11-20 (`0a875da`). Updates from v0.1.0 through v0.1.4 use the same mod ID and data directory.

Radar shows entities the client receives. Map teleport requires server permissions. On Horizon SMP the claim overlay can query `/claims` and `/claim info`; Waystones and Open Parties and Claims overlays depend on compatible data from the server. Version 0.1.7 was checked against the live Horizon SMP map and against separate test worlds on the same LAN address. See [VALIDATION](VALIDATION.md).

## Copy maps and waypoints from a previous build

Close the game and use a profile with only one mapping mod enabled. Kadir's Map has its own mod ID and data directory. Before opening a world with it, copy the contents of `horizon-mods/.horizon-mod-data/minimap_worldmap_waypoints_reworked/` to `horizon-mods/.horizon-mod-data/trailgrid_maps/` in the target profile. Keep the original directory as a backup. For an existing Trailgrid profile, back up both directories and copy selected files explicitly to avoid replacing newer data. Use the same save folder name or server address so the copied map and waypoints resolve to the same world. If Horizon reports the old mod ID as missing in a single-player save, choose **Back Up and Open** after checking that the list only names the old map mod. Rebind custom keys in the new profile as needed.

The world map's import tools can also read supported waypoint, settings and cache formats from existing Xaero's Minimap and Xaero's World Map data. The Java mods themselves are separate products.

## Source and license

Kadir's Map is maintained by Kadir (groznen95) and licensed under [GPL-3.0-only](LICENSE). See [NOTICE](NOTICE) and [LICENSE-MIT](LICENSE-MIT) for upstream attribution and license terms. The icon is original to Kadir's Map. The release package contains the Rust source and assets used by the launcher.

Compatibility with existing data refers to [Xaero's Minimap](https://www.curseforge.com/minecraft/mc-mods/xaeros-minimap) and [Xaero's World Map](https://www.curseforge.com/minecraft/mc-mods/xaeros-world-map) by Xaero. Kadir's Map is an independent project.
