# Kadir's Map validation

## 0.1.13

The source `.hmod` passed the native package checker and built on macOS Apple Silicon for Horizon Nightly 2026-11-22 (`2b77287`), Minecraft 26.3 and Mixins `e59da90`.

In a local world, Ctrl+F found a setting from a different tab by its English and Russian names. The setting could be changed from the results. At GUI scale 2, the waypoint Share menu opened a link in the chat input, and Escape closed the menu before closing the waypoint list. The first Delete click kept the waypoint and asked for confirmation.

On Horizon SMP, the world map and waypoint list opened with a copy of the existing profile. The copy retained the original waypoint records and cache paths. All map files in the original profile kept their SHA-256 hashes during the check.

## 0.1.12

The native package passed the registry checker and built as `native-2b77287-29b6adc81ee6` on Minecraft 26.3 and Horizon Nightly 2026-11-22. The test instance uses the same package as the prepared download.

The redesigned screens were exercised at 960 × 640 GUI units in English and 480 × 320 in Russian. The in-game checks covered waypoint search and selection, the map sidebar and context menu, PNG export, changing and dragging settings, radar categories, minimap placement, and returning from the cache screen. The first click on a waypoint's delete button left the saved waypoint in place and exposed confirmation.

On Horizon SMP, the final build opened the map and waypoint list on a copy of the existing profile. All prior waypoint records and cache paths were present. The main profile's map files kept their pre-test hashes.

## 0.1.11

The package passed the registry checker and built as `native-2b77287-6bc316a5b65f` on macOS Apple Silicon against Horizon Nightly 2026-11-22 (`2b77287`), Minecraft 26.3 and Mixins `e59da90`. On the exact SDK, 140 tests passed and three were ignored.

In a disposable save, typing replaced the default waypoint name, Tab switched between the name and group fields, and F2 changed the selected waypoint without losing its group. The client command `/mapdeaths` printed local history.

A new Overworld with far terrain enabled wrote 563 far-terrain records into the map cache using the Nov 22 surface API. A separate save scan added a distant region to the export and left its source region file byte for byte unchanged.

The final package ran on Horizon SMP with a copy of the existing map. The live server opened the map and waypoint list and displayed textured radar icons. The original cache paths and waypoint records were present. The main map data matched its pre-test hashes.

## 0.1.10

The package passed the Horizon registry checker and built with Freecam+ 2.0.5 on Horizon Nightly `8f83e5c`, Minecraft 26.3. The final native build is `native-8f83e5c-6995a5ecf9ad`. On the pinned SDK, 140 tests passed and three were ignored.

In disposable worlds, the UI was exercised in English at 1920 × 1280 GUI units and in Russian at 480 × 320. The tests created a waypoint, searched for it with the keyboard, opened the map and sidebar context menu, centered on a waypoint, exported PNG terrain, changed a setting in both directions, dragged a settings slider, opened radar categories and minimap placement, and returned to the game from the waypoint list. A separate local regression check confirmed that the save scan still exports distant terrain and leaves the original region file unchanged.

The final build opened the world map and waypoint list on live Horizon SMP using an isolated copy of the existing profile. All prior cache paths and waypoint records were present. The main launcher profile now selects the tested native build. All five original map data files and the Freecam+ package matched their pre-install hashes.

## 0.1.9

The package passed the Horizon registry checker. Horizon Nightly `8f83e5c` built it with Freecam+ 2.0.5 on Minecraft 26.3 as `native-8f83e5c-aad034f744cf`. On the pinned SDK, 140 tests passed and three were ignored.

In a local save, the scan added a region at block coordinates (8192, -4096) that the client had never visited. The region appeared in a 512 × 512 PNG export after the scan and its source file retained its original SHA-256 hash. The merge test verifies that known colors and heights survive a scan while gaps and a separate dimension can be added. The local radar displayed textured 3D sheep, villager, and iron golem icons.

On Horizon SMP, the radar rendered textured 3D mobs and both the world map and waypoint list opened. The isolated test profile found every original waypoint record and cache path. The server save was outside this check. The main launcher profile now selects the tested native build. The five existing map data files and the Freecam+ package matched their pre-install SHA-256 hashes.

## 0.1.8

The package passed the Horizon registry checker and ran with Freecam+ 2.0.5 on Minecraft 26.3 and Horizon Nightly `8f83e5c`. The final native build was `native-8f83e5c-262722ca4d25`. On the pinned SDK, 139 tests passed and three were ignored.

The LAN run exported one PNG from the world map, then imported a copied Xaero `world-map/client.cfg` and the client's selected profile. The game applied the PNG preferences, disabled waypoint visibility and separate world map cave height. The second export wrote four 512 × 512 region PNGs. The export regression test covers night tint, claim colors, negative region coordinates and cleanup after a failed split export.

The final build opened the world map and waypoint list on Horizon SMP with a copy of the existing data. The existing map cache paths and all original waypoint records remained present. The update to the main profile preserved all five map data files byte-for-byte and verified the Freecam+ package hash.

## 0.1.7

Horizon Nightly `8f83e5c` built Kadir's Map 0.1.7 and Freecam+ 2.0.5 together as `native-8f83e5c-11b3350a9ce5`. The native package checker passed. On the pinned SDK, 137 tests passed and three were ignored.

On Horizon SMP, the new version received the Overworld signal and opened the map and waypoint list on a copy of the main profile. The copied cache paths and all original waypoint records were present. In a second live login, the server's signal switched from a different map selected in the copy to the existing Default map. The server reported no public warps during the check.

The LAN test used two Overworlds with distinct hashed seeds at the same address, kept normal Nether travel in the first world map, automatically chose a separate map for the second Overworld, and restored the first world on a later connection. It verified separate terrain files and waypoints and retained the fixture from the previous version.

The main launcher profile selected the tested build. All five map data files matched their backed-up copies by SHA-256. The Freecam+ package also matched its pre-install hash.

## 0.1.6

Horizon Nightly `8f83e5c` built Kadir's Map 0.1.6 and Freecam+ 2.0.5 together as `native-8f83e5c-525cab6e55b6`. The native package checker passed. On the pinned SDK, 136 tests passed and three were ignored.

The two-client LAN test created a marker in the default server cache, created and selected a named world from the map, and saved a waypoint to the named world. The client reopened in the same named world after a restart. Returning to Default restored the original waypoint scope, and both terrain caches remained present. The regression test also covers separate waypoint groups and death markers.

The main profile selected the checked build. All five existing map data files and the installed Freecam+ package matched their pre-install hashes. Host verification used a local server. Live Horizon SMP and automatic server subworld discovery were outside these checks.

## 0.1.5

The map and Freecam+ 2.0.5 built together on macOS Apple Silicon against Minecraft 26.3 and Horizon Nightly `8f83e5c`. The launcher selected `native-8f83e5c-97a8213514ed` without omitting either mod.

Both release archives passed the native package checker. The test profile opened the existing world and loaded the map. The camera changed its view while the player stood still, and the second U press restored the normal view. All four existing camera runtime tests passed.

The main profile reached the title screen after installation. Options → Mods displayed **Kadir's Map** with the new pixel-art icon and **Freecam+ 2.0.5** with its Russian display name and icon. All files in the original map data directory matched their pre-install hashes. The mod ID remains `trailgrid_maps`.

Multiplayer was outside these checks.
