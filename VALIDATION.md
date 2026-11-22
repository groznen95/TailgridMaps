# Kadir's Map validation

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
