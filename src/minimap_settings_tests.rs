// SPDX-License-Identifier: GPL-3.0-only
// Shared standalone/SDK tests, expanded in the runtime's private scope.

#[cfg(test)]
macro_rules! define_tests {
    () => {
        mod tests {
            use super::*;
            use std::sync::atomic::{AtomicU64, Ordering};

            fn read_profile(path: &Path) -> io::Result<(Settings, Vec<Waypoint>)> {
                read_profile_migration(path).map(|(settings, points, _, _)| (settings, points))
            }

            fn read_settings(path: &Path) -> io::Result<Settings> {
                read_profile(path).map(|profile| profile.0)
            }
            #[test]
            fn zoom_hotkeys_follow_scale_order_and_stop_at_bounds() {
                for nearer in [true, false] {
                    let mut current = Settings::default();
                    let mut steps = 0;
                    while let Some(index) = current.zoom_step(nearer) {
                        let prior = current.blocks_per_pixel;
                        assert!(current.select_option(4, index));
                        assert_eq!(current.blocks_per_pixel < prior, nearer);
                        steps += 1;
                    }
                    assert_eq!(steps, if nearer { 7 } else { 2 });
                }
            }

            #[test]
            fn sdk_choices_cover_saved_preferences_and_reject_actions_and_bad_indices() {
                let mut keys=std::collections::BTreeSet::new();
                for (key,button) in SDK_SETTING_ROWS {
                    assert!(keys.insert(key));
                    assert!(button < SDK_SETTING_SLOTS);
                    let options=Settings::options(button);assert!(!options.is_empty()&&options.len()<=32);
                    for (index,option) in options.iter().enumerate() {
                        assert_eq!(option.option_index(button),Some(index));
                        assert_eq!(Settings::decode(&option.encode()),*option);
                        assert!(!option.option_label(button).unwrap().is_empty());
                        let mut selected=Settings{cache_ram_mib:173,..Settings::default()};
                        assert!(selected.select_option(button,index));
                        assert_eq!(selected.option_index(button),Some(index));assert_eq!(selected.cache_ram_mib,173);
                        let before=selected;assert!(!selected.select_option(button,options.len()));assert_eq!(selected,before);
                    }
                }
                for action in [6,18,19,21,usize::MAX] {assert!(Settings::options(action).is_empty());assert_eq!(Settings::default().option_index(action),None);}
            }
            #[test]
            fn sdk_choice_during_initial_load_preserves_other_preferences() {
                let directory=scratch();fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join(SETTINGS_FILE),"size=256\ncache_ram_mib=173\nwaypoint_letters=one\n").unwrap();
                let controller=Controller::open(directory.clone());
                assert!(controller.set_option(20,1));assert!(!controller.set_option(20,3));assert!(!controller.set_option(19,0));
                controller.shutdown();
                let saved=read_settings(&directory.join(SETTINGS_FILE)).unwrap();
                assert_eq!(saved.size_px,256);assert_eq!(saved.cache_ram_mib,173);assert_eq!(saved.waypoint_letters,minimap::WaypointLetters::AnyColors);
                fs::remove_dir_all(directory).unwrap();
            }
            #[test]
            fn sdk_explicit_defaults_override_nondefault_preferences_during_initial_load() {
                let directory=scratch();fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join(SETTINGS_FILE),"size=256\ncache_ram_mib=256\nwaypoint_letters=one\n").unwrap();
                let controller=Controller::open(directory.clone());
                controller.set_cache_ram_mib(64);assert!(controller.set_option(20,0));controller.shutdown();
                let saved=read_settings(&directory.join(SETTINGS_FILE)).unwrap();
                assert_eq!(saved.size_px,256);assert_eq!(saved.cache_ram_mib,64);assert_eq!(saved.waypoint_letters,minimap::WaypointLetters::SimilarColors);
                fs::remove_dir_all(directory).unwrap();
            }
            #[test]
            fn waypoint_initials_expand_for_collisions_in_the_selected_colour_scope() {
                use minimap::WaypointLetters::*;
                let a=point("save:a","  Ёлка");let mut b=point("save:a","ёжик");b.color=a.color^0x000504;
                assert_eq!(waypoint_labels(&[a.clone(),b.clone()],SimilarColors),vec!["ЁЛ","ЁЖ"]);
                assert_eq!(waypoint_labels(&[a.clone(),b.clone()],One),vec!["Ё","Ё"]);
                b.color=0xff0000;assert_eq!(waypoint_labels(&[a.clone(),b.clone()],SimilarColors),vec!["Ё","Ё"]);
                assert_eq!(waypoint_labels(&[a.clone(),b.clone()],AnyColors),vec!["ЁЛ","ЁЖ"]);
                b.world="save:b".into();assert_eq!(waypoint_labels(&[a.clone(),b.clone()],AnyColors),vec!["Ё","Ё"]);
                b.world=a.world.clone();b.dimension=Dimension::Nether;assert_eq!(waypoint_labels(&[a.clone(),b.clone()],AnyColors),vec!["Ё","Ё"]);
                b.dimension=a.dimension;b.icon=WaypointIcon::Skull;assert_eq!(waypoint_labels(&[a.clone(),b.clone()],AnyColors),vec!["Ё","Ё"]);
                b.icon=WaypointIcon::Letter;b.name="Ё".into();assert_eq!(waypoint_labels(&[a,b],AnyColors),vec!["ЁЛ","Ё"]);
                assert!(similar_waypoint_colors(0xff101020,0x00101020));
                assert!(similar_waypoint_colors(0,48));assert!(!similar_waypoint_colors(0,49));
            }
            #[test]
            fn waypoint_initials_setting_roundtrips_merges_and_resets() {
                use minimap::WaypointLetters;
                for mode in WaypointLetters::ALL {
                    let settings=Settings{waypoint_letters:mode,..Settings::default()};assert_eq!(Settings::decode(&settings.encode()),settings);
                }
                assert_eq!(Settings::decode("waypoint_letters=invalid").waypoint_letters,WaypointLetters::SimilarColors);
                let mut early=Settings::default();let mask=minimap::apply(&mut early,20);
                assert_eq!(early.waypoint_letters,WaypointLetters::AnyColors);
                let mut loaded=Settings::decode("size=256\ncache_ram_mib=128\nwaypoint_letters=one");minimap::merge(&mut loaded,early,mask);
                assert_eq!(loaded.waypoint_letters,WaypointLetters::AnyColors);assert_eq!(loaded.size_px,256);assert_eq!(loaded.cache_ram_mib,128);
                let reset=minimap::apply(&mut early,6);minimap::merge(&mut loaded,early,reset);assert_eq!(loaded,Settings::default());
            }

            static NEXT: AtomicU64 = AtomicU64::new(0);
            fn scratch() -> PathBuf {
                std::env::temp_dir().join(format!(
                    "minimap-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ))
            }

            #[test]
            fn settings_reject_invalid_sizes_zooms_and_boolean_values() {
                let settings = Settings::decode(
                    "size=65535\nzoom=NaN\nrotate=invalid\nshape=triangle\ncorner=unknown\nenabled=false",
                );
                assert_eq!(
                    settings,
                    Settings {
                        enabled: false,
                        ..Settings::default()
                    }
                );
                assert_eq!(Settings::decode(&settings.encode()), settings);
            }

            #[test]
            fn early_edits_merge_with_loaded_preferences_instead_of_resetting_them() {
                let mut loaded = Settings::decode("size=128\nshape=round\ncorner=bottom_right");
                let mut early = Settings::default();
                let mask = minimap::apply(&mut early, 0);
                minimap::merge(&mut loaded, early, mask);
                assert!(!loaded.enabled);
                assert!(loaded.round);
                assert_eq!(loaded.size_px, 128);
                assert_eq!(loaded.corner, minimap::Corner::BottomRight);
            }

            #[test]
            fn controller_flushes_queued_edits_and_reloads_them() {
                let directory = scratch();
                let controller = Controller::open(directory.clone());
                controller.click(3);
                controller.click(4);
                controller.click(1);
                controller.shutdown();
                let saved = read_settings(&directory.join(SETTINGS_FILE)).unwrap();
                assert!(saved.round);
                assert_eq!(saved.blocks_per_pixel, 2.0);
                assert_eq!(saved.corner, minimap::Corner::BottomLeft);
                assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                assert_eq!(reopened.settings(), saved);
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn cache_ram_slider_roundtrips_and_merges_early_edits() {
                let value = Settings::decode("cache_ram_mib=128");
                assert_eq!(Settings::decode(&value.encode()).cache_ram_mib, 128);
                assert_eq!(Settings::decode("cache_ram_mib=0").cache_ram_mib, 64);
                let mut loaded = Settings::decode("cache_ram_mib=256");
                let edited = Settings {
                    cache_ram_mib: 32,
                    ..Settings::default()
                };
                minimap::merge(&mut loaded, edited, 1 << 18);
                assert_eq!(loaded.cache_ram_mib, 32);
            }

            fn point(world: &str, name: &str) -> Waypoint {
                Waypoint {
                    world: world.into(),
                    dimension: Dimension::Overworld,
                    x: 10,
                    y: 64,
                    z: -5,
                    name: name.into(),
                    initials: None,
                    options: WaypointOptions::default(),
                    color: DEFAULT_WAYPOINT_COLOR,
                    icon: WaypointIcon::Letter,
                    biome: None,
                    show_3d: true,
                }
            }

            #[test]
            fn waypoint_batch_move_rejects_conflicts_without_changing_any_marker() {
                let directory = scratch();
                let controller = Controller::open(directory.clone());
                let world = "save:bulk";
                let mut bridge = point(world, "Bridge");
                bridge.options.group = "Source".into();
                let mut duplicate = bridge.clone();
                duplicate.options.group = "Destination".into();
                let tower = point(world, "Tower");
                let other_world = point("save:another", "Tower");
                for point in [&bridge, &duplicate, &tower, &other_world] {
                    assert!(controller.add_waypoint(point.clone()));
                }
                let keys = BTreeSet::from([bridge.key(), tower.key()]);
                let before = controller.waypoints(world);
                assert!(controller.edit_waypoints(world, &keys, WaypointBatch::Move("Destination")).is_err());
                assert_eq!(controller.waypoints(world), before);
                assert_eq!(controller.edit_waypoints(world, &keys, WaypointBatch::Visible(false)), Ok(2));
                assert_eq!(controller.edit_waypoints(world, &keys, WaypointBatch::Move("Other")), Ok(2));
                assert!(controller.waypoint_group_names(world).contains("Source"));
                assert_eq!(controller.waypoints("save:another"), vec![other_world]);

                let moved: BTreeSet<_> = controller.waypoints(world).into_iter()
                    .filter(|point| {
                        if point.options.group == "Other" { assert!(point.options.disabled); }
                        point.options.group == "Other"
                    })
                    .map(|point| point.key()).collect();
                assert!(controller.edit_waypoints(world, &keys, WaypointBatch::Delete).is_err());
                assert_eq!(controller.edit_waypoints(world, &moved, WaypointBatch::Delete), Ok(2));
                assert_eq!(controller.waypoints(world), vec![duplicate]);
                controller.shutdown();
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn manual_cave_height_and_server_seed_are_scoped() {
                let settings = Settings::decode("cave_view=manual\ncave_y=-17\n");
                assert_eq!(settings.cave_view, minimap::CaveView::Manual);
                assert_eq!(settings.cave_y, -17);
                assert_eq!(Settings::decode(&settings.encode()), settings);
                let input = "# optional manual seeds\nserver:a.example:24454=-12\nserver:b.example=42\n";
                assert_eq!(slime_seed(input, "server:a.example:24454"), Some(-12));
                assert_eq!(slime_seed(input, "server:a.example"), None);
                assert_eq!(slime_seed(input, "server:b.example"), Some(42));
            }

            #[test]
            fn xaero_groups_survive_roundtrip_and_share_links_keep_coordinates() {
                let file = "sets:gui.xaero_default:Маяки\n\
                    waypoint:Север§§_мост-*:СМ:-24:~:37:12:true:0:Маяки:true:-45:3:false";
                let world = "server:test";
                let mut points = parse_xaero_waypoints(file, world, Dimension::Nether).unwrap();
                let p = points.pop().unwrap();
                assert_eq!(p.options.group, "Маяки");
                assert!(p.options.disabled && p.map_only());
                assert_eq!(Waypoint::decode(p.encode().trim_end()).unwrap(), p);
                let pasted = receive_shared_waypoint(&format!("<player> {}", p.share_link()), world, Dimension::End).unwrap();
                assert_eq!(pasted.name, p.name);
                assert_eq!(pasted.dimension, Dimension::Nether);
                assert_eq!((pasted.x, pasted.z), (-24, 37));
                assert_eq!(pasted.color, p.color);
                assert!(pasted.options.unspecified_height && pasted.options.rotation);
                assert_eq!(pasted.options.yaw, -45);
            }

            #[test]
            fn xaero_import_is_atomic_and_preserves_custom_initials_and_black() {
                let root = scratch();
                fs::create_dir_all(&root).unwrap();
                let file = root.join("xaero-nether.txt");
                let line = "waypoint:Дом§§река:ДР:-40:32:15:0:false:0:gui.xaero_default:false:0:0:false";
                let controller = Controller::open(root.clone());
                for _ in 0..100 {
                    if controller.state.lock().unwrap().loaded {
                        break;
                    }
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                let world = "server:test";
                let dimension = Dimension::Nether;
                fs::write(&file, format!("{line}\nwaypoint:malformed")).unwrap();
                assert!(controller.import_xaero_waypoints(&file, world, dimension).is_err());
                assert!(controller.waypoints(world).is_empty());
                fs::write(&file, format!("# fixture\n{line}\n{line}\n")).unwrap();
                assert_eq!(controller.import_xaero_waypoints(&file, world, dimension).unwrap(), 1);
                assert_eq!(controller.import_xaero_waypoints(&file, world, dimension).unwrap(), 0);
                let points = controller.waypoints(world);
                assert_eq!(points[0].name, "Дом:река");
                assert_eq!(points[0].initials.as_deref(), Some("ДР"));
                assert_eq!(points[0].color, 0);
                assert_eq!(points[0].dimension, dimension);
                fs::write(&file, line.replace(":0:false:0:", ":1:false:0:")).unwrap();
                assert!(controller.import_xaero_waypoints(&file, world, dimension).is_err());
                assert_eq!(controller.waypoints(world), points);
                controller.shutdown();
                assert_eq!(read_profile_migration(&root.join(SETTINGS_FILE)).unwrap().1, points);
                fs::remove_dir_all(root).unwrap();
            }

            #[test]
            fn world_and_server_waypoints_do_not_collide_and_survive_reconnect() {
                let directory = scratch();
                let a = waypoint_world(true, "World A").unwrap();
                let b = waypoint_world(true, "World B").unwrap();
                let server = waypoint_world(false, "World A").unwrap();
                let controller = Controller::open(directory.clone());
                for (world, name) in [(&a, "A"), (&b, "B"), (&server, "Server")] {
                    assert!(controller.add_waypoint(point(world, name)));
                }
                assert_eq!(controller.waypoints(&a), vec![point(&a, "A")]);
                assert_eq!(controller.waypoints(&b), vec![point(&b, "B")]);
                assert!(controller.update_waypoint(
                    &b,
                    Dimension::Overworld,
                    10,
                    -5,
                    "B renamed",
                    DEFAULT_WAYPOINT_COLOR
                ));
                assert!(controller.remove_waypoint(&server, Dimension::Overworld, 10, -5));
                assert_eq!(controller.waypoints(&a), vec![point(&a, "A")]);
                assert!(controller.waypoints("").is_empty());
                assert!(!controller.add_waypoint(point("", "unknown")));
                controller.shutdown();
                let reconnected = Controller::open(directory.clone());
                reconnected.shutdown();
                assert_eq!(reconnected.waypoints(&a), vec![point(&a, "A")]);
                assert_eq!(reconnected.waypoints(&b), vec![point(&b, "B renamed")]);
                assert!(reconnected.waypoints(&server).is_empty());
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn waypoint_limits_never_evict_other_worlds() {
                let mut points = vec![point("save:A", "A")];
                for x in 0..(MAX_WAYPOINTS as i32 + 5) {
                    push_waypoint(
                        &mut points,
                        Waypoint {
                            x,
                            ..point("save:B", "B")
                        },
                    );
                }
                assert_eq!(points[0], point("save:A", "A"));
                assert_eq!(points.len(), MAX_WAYPOINTS + 1);
                for i in 0..MAX_TOTAL_WAYPOINTS {
                    push_waypoint(&mut points, point(&format!("save:{i}"), "other"));
                }
                assert_eq!(points.len(), MAX_TOTAL_WAYPOINTS);
                assert!(!push_waypoint(&mut points, point("save:overflow", "no")));
                assert_eq!(points[0], point("save:A", "A"));
            }

            #[test]
            fn scoped_identity_roundtrips_delimiters_and_rejects_unknown_kinds() {
                let world = waypoint_world(false, "[::1]:25565% Мир").unwrap();
                let point = point(&world, "Home: 100%");
                assert_eq!(Waypoint::decode(point.encode().trim()), Some(point));
                assert_ne!(waypoint_world(true, "same"), waypoint_world(false, "same"));
                assert!(waypoint_world(false, "").is_none());
                assert!(waypoint_world(false, "bad\naddress").is_none());
                assert!(Waypoint::decode("waypoint_v2=unknown:overworld:1:2:3:name").is_none());
            }

            #[test]
            fn manual_server_worlds_keep_default_points_and_remember_selection() {
                fn wait_for_settings(controller: &Controller) {
                    for _ in 0..100 {
                        if controller.settings_loaded() { return; }
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    panic!("Settings did not load");
                }

                let root = scratch();
                let base = "server:[::1]:25565";
                let other = "server:example.org";
                let label = "Spawn: 100%";
                let scoped = server_subworld_id(base, label);
                assert_eq!(server_subworld_parts(&scoped), Some((base.into(), label.into())));
                assert!(!valid_waypoint_world("subworld:server%3Atest:bad:name"));
                assert_ne!(container::world_path(&root, base), container::world_path(&root, &scoped));

                {
                    let controller = Controller::open(root.clone());
                    wait_for_settings(&controller);
                    assert_eq!(controller.world_for_server(base.into()), base);
                    assert!(controller.add_waypoint(point(base, "Original")));
                    assert!(!controller.select_server_world("save:Local", Some(label), None));
                    assert!(!controller.select_server_world(base, Some("hidden\nline"), None));
                    assert!(controller.select_server_world(base, Some(label), None));
                    assert_eq!(controller.world_for_server(base.into()), scoped);
                    assert_eq!(controller.world_for_server(other.into()), other);
                    assert!(controller.add_waypoint(point(&scoped, "Subworld waypoint")));
                    assert!(controller.add_waypoint_groups(&scoped, ["Subworld group".to_owned()]));
                    assert!(controller.add_death_waypoint(&scoped, Dimension::Overworld, 2, 64, 4, None));
                }
                {
                    let controller = Controller::open(root.clone());
                    wait_for_settings(&controller);
                    assert_eq!(controller.world_for_server(base.into()), scoped);
                    assert_eq!(controller.waypoints(base), vec![point(base, "Original")]);
                    assert_eq!(controller.waypoints(&scoped).len(), 2);
                    assert!(controller.waypoint_group_names(&scoped).contains("Subworld group"));
                    assert!(controller.select_server_world(base, None, None));
                    assert_eq!(controller.world_for_server(base.into()), base);
                }
                {
                    let controller = Controller::open(root.clone());
                    wait_for_settings(&controller);
                    assert_eq!(controller.world_for_server(base.into()), base);
                    assert!(controller.server_worlds(base).0.contains(label));
                }
                fs::remove_dir_all(root).unwrap();
            }

            #[test]
            fn server_signals_automatically_restore_maps_without_moving_legacy_data() {
                fn open_loaded(root: &Path) -> Controller {
                    let controller = Controller::open(root.to_owned());
                    for _ in 0..100 {
                        if controller.settings_loaded() { return controller; }
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    panic!("Settings did not load");
                }

                let root = scratch();
                fs::create_dir_all(&root).unwrap();
                let base = "server:smp.example.org:25565";
                let other_server = "server:another.example.org";
                let legacy = point(base, "Already saved");
                fs::write(root.join(SETTINGS_FILE), legacy.encode()).unwrap();
                let main = ServerWorldSignal::new("minecraft:overworld", -17).unwrap();
                let nether = ServerWorldSignal::new("minecraft:the_nether", 5).unwrap();
                let alternate = ServerWorldSignal::new("minecraft:overworld", 99).unwrap();
                let custom = ServerWorldSignal::new("smp:build/plots", -17).unwrap();
                assert!(ServerWorldSignal::new("bad:\n", 1).is_none());
                let alternate_map;
                {
                    let controller = open_loaded(&root);
                    assert!(controller.observe_server_world(base, &main));
                    assert_eq!(controller.world_for_server(base.into()), base);
                    assert!(controller.observe_server_world(base, &nether));
                    assert_eq!(controller.world_for_server(base.into()), base);
                    assert!(controller.observe_server_world(base, &alternate));
                    alternate_map = controller.world_for_server(base.into());
                    assert_ne!(container::world_path(&root, base), container::world_path(&root, &alternate_map));
                    assert!(controller.add_waypoint(point(&alternate_map, "Alternate")));
                    assert!(controller.observe_server_world(base, &custom));
                    assert_ne!(controller.world_for_server(base.into()), alternate_map);
                    assert!(controller.observe_server_world(base, &main));
                    assert_eq!(controller.waypoints(base), [legacy.clone()]);
                    assert_eq!(controller.world_for_server(base.into()), base);
                }
                {
                    let controller = open_loaded(&root);
                    assert!(controller.observe_server_world(other_server, &alternate));
                    assert_eq!(controller.world_for_server(other_server.into()), other_server);
                    assert!(controller.observe_server_world(base, &alternate));
                    assert_eq!(controller.world_for_server(base.into()), alternate_map);
                    assert_eq!(controller.waypoints(&alternate_map), [point(&alternate_map, "Alternate")]);
                    assert!(controller.select_server_world(base, Some("My saved plots"), Some(&custom)));
                    assert!(controller.observe_server_world(base, &main));
                    assert!(controller.observe_server_world(base, &custom));
                    assert_eq!(controller.world_for_server(base.into()), server_subworld_id(base, "My saved plots"));
                }
                assert!(fs::read_to_string(root.join(SETTINGS_FILE)).unwrap().contains(&legacy.encode()));
                fs::remove_dir_all(root).unwrap();
            }

            #[test]
            fn legacy_waypoints_are_deleted_but_preferences_and_scoped_points_survive() {
                let directory = scratch();
                fs::create_dir_all(&directory).unwrap();
                let scoped = point("server:example.org:25565", "Scoped");
                fs::write(
                    directory.join(SETTINGS_FILE),
                    format!(
                        "size=192\nsmooth_worldmap=false\nwaypoint=overworld:10:64:-5:Old\n{}",
                        scoped.encode()
                    ),
                )
                .unwrap();
                let controller = Controller::open(directory.clone());
                // Migration must persist even without an edit or a world being joined.
                controller.shutdown();
                assert_eq!(controller.settings().size_px, 192);
                assert!(!controller.settings().encode().contains("smooth_worldmap"));
                assert_eq!(controller.waypoints(&scoped.world), vec![scoped.clone()]);
                assert!(controller.waypoints("save:A").is_empty());
                let file = fs::read_to_string(directory.join(SETTINGS_FILE)).unwrap();
                assert!(!file.lines().any(|l| l.starts_with("waypoint=")));
                assert!(file.contains("waypoint_v5="));
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                assert_eq!(reopened.waypoints(&scoped.world), vec![scoped]);
                assert_eq!(
                    fs::read_to_string(directory.join(SETTINGS_FILE)).unwrap(),
                    file
                );
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn scoped_v2_records_gain_the_default_colour_and_migrate_to_v5() {
                let directory = scratch();
                fs::create_dir_all(&directory).unwrap();
                fs::write(
                    directory.join(SETTINGS_FILE),
                    "waypoint_v2=save%3Atest:overworld:1:2:3:Старый\n",
                )
                .unwrap();
                let controller = Controller::open(directory.clone());
                controller.shutdown();
                let file = fs::read_to_string(directory.join(SETTINGS_FILE)).unwrap();
                assert!(file.contains("waypoint_v5="));
                assert!(!file.contains("waypoint_v2="));
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                let waypoint = reopened.waypoints("save:test")[0].clone();
                assert_eq!(waypoint.name, "Старый");
                assert_eq!(waypoint.color, DEFAULT_WAYPOINT_COLOR);
                assert_eq!(waypoint.icon, WaypointIcon::Letter);
                assert_eq!(waypoint.biome, None);
                assert!(waypoint.show_3d);
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn waypoints_persist_deduplicate_and_stay_bounded() {
                let directory = scratch();
                let controller = Controller::open(directory.clone());
                let point = Waypoint {
                    world: "save:test".into(),
                    dimension: Dimension::Nether,
                    x: 10,
                    y: 64,
                    z: -5,
                    name: "База".into(),
                    initials: None,
                    options: WaypointOptions::default(),
                    color: DEFAULT_WAYPOINT_COLOR,
                    icon: WaypointIcon::Letter,
                    biome: None,
                    show_3d: true,
                };
                assert!(controller.add_waypoint(point.clone()));
                assert!(!controller.add_waypoint(point.clone()));
                for x in 0..(MAX_WAYPOINTS as i32 - 1) {
                    assert!(controller.add_waypoint(Waypoint {
                        world: "save:test".into(),
                        dimension: Dimension::Overworld,
                        x,
                        y: 64,
                        z: 0,
                        name: format!("Точка {x}"),
                        initials: None,
                        options: WaypointOptions::default(),
                        color: WAYPOINT_COLORS[(x as usize) % WAYPOINT_COLORS.len()],
                        icon: WaypointIcon::Letter,
                        biome: None,
                        show_3d: true,
                    }));
                }
                controller.shutdown();
                let (_, saved) = read_profile(&directory.join(SETTINGS_FILE)).unwrap();
                assert_eq!(saved.len(), MAX_WAYPOINTS);
                assert!(saved.contains(&point));
                assert_eq!(Waypoint::decode("waypoint=player:1:2:3"), None);
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                assert_eq!(reopened.waypoints("save:test"), saved);
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn death_waypoint_settings_roundtrip_toggle_and_reset() {
                let mut settings = Settings::default();
                assert_eq!(minimap::apply(&mut settings, 11), 1 << 11);
                assert!(!settings.death_waypoints);
                assert_eq!(settings.option_index(12), Some(1));
                assert_eq!(minimap::apply(&mut settings, 12), 1 << 12);
                assert!(settings.death_waypoint_auto_remove);
                assert_eq!(settings.option_index(12), Some(0));
                assert_eq!(Settings::decode(&settings.encode()), settings);
                let mask = minimap::apply(&mut settings, 6);
                assert_eq!(mask & (1 << 11), 1 << 11);
                assert_eq!(mask & (1 << 12), 1 << 12);
                assert!(settings.death_waypoints && !settings.death_waypoint_auto_remove);
                assert!(
                    Settings::decode("death_waypoints=false\ndeath_waypoint_auto_remove=false")
                        .encode()
                        .contains("death_waypoints=false")
                );
            }

            #[test]
            fn world_waypoint_settings_roundtrip_toggle_and_reset() {
                let mut settings = Settings::default();
                assert!(settings.world_waypoints);
                assert_eq!(
                    settings.world_waypoint_style,
                    minimap::WorldMarkerStyle::LetterDistance
                );
                assert_eq!(minimap::apply(&mut settings, 13), 1 << 13);
                assert!(!settings.world_waypoints);
                assert_eq!(minimap::apply(&mut settings, 14), 1 << 14);
                assert_eq!(
                    settings.world_waypoint_style,
                    minimap::WorldMarkerStyle::Letter
                );
                assert_eq!(Settings::decode(&settings.encode()), settings);
                let mask = minimap::apply(&mut settings, 6);
                assert_eq!(mask & (1 << 13), 1 << 13);
                assert_eq!(mask & (1 << 14), 1 << 14);
                assert!(settings.world_waypoints);
                assert_eq!(
                    settings.world_waypoint_style,
                    minimap::WorldMarkerStyle::LetterDistance
                );
                assert!(
                    Settings::decode("world_waypoints=false\nworld_waypoint_style=name_distance")
                        .encode()
                        .contains("world_waypoints=false")
                );
            }

            #[test]
            fn direction_settings_roundtrip_toggle_and_reset() {
                let mut settings = Settings::default();
                assert!(!settings.directions);
                assert_eq!(settings.direction_style, minimap::DirectionStyle::Compass);
                assert_eq!(minimap::apply(&mut settings, 15), 1 << 15);
                assert!(settings.directions);
                assert_eq!(minimap::apply(&mut settings, 16), 1 << 16);
                assert_eq!(settings.direction_style, minimap::DirectionStyle::Axes);
                assert_eq!(Settings::decode(&settings.encode()), settings);
                let mask = minimap::apply(&mut settings, 6);
                assert_eq!(mask & (1 << 15), 1 << 15);
                assert_eq!(mask & (1 << 16), 1 << 16);
                assert!(!settings.directions);
                assert_eq!(settings.direction_style, minimap::DirectionStyle::Compass);
                assert!(
                    Settings::decode("directions=true\ndirection_style=axes")
                        .encode()
                        .contains("directions=true")
                );
            }

            #[test]
            fn labels_on_hover_setting_roundtrips_toggle_and_reset() {
                let mut settings = Settings::default();
                assert!(settings.labels_on_hover);
                assert_eq!(minimap::apply(&mut settings, 17), 1 << 17);
                assert!(!settings.labels_on_hover);
                assert_eq!(Settings::decode(&settings.encode()), settings);
                let mask = minimap::apply(&mut settings, 6);
                assert_eq!(mask & (1 << 17), 1 << 17);
                assert!(settings.labels_on_hover);
                assert!(
                    Settings::decode("labels_on_hover=false")
                        .encode()
                        .contains("labels_on_hover=false")
                );
            }

            #[test]
            fn waypoint_3d_flag_roundtrips_and_legacy_records_default_on() {
                let mut point = point("save:test", "Home");
                point.show_3d = false;
                let decoded = Waypoint::decode(point.encode().trim()).unwrap();
                assert!(!decoded.show_3d);
                assert_eq!(decoded, point);
                let legacy =
                    Waypoint::decode("waypoint_v4=save%3Atest:overworld:1:64:2:ff6b6b:letter::Old")
                        .unwrap();
                assert!(legacy.show_3d);
            }

            #[test]
            fn toggling_3d_visibility_persists() {
                let directory = scratch();
                let controller = Controller::open(directory.clone());
                assert!(controller.add_waypoint(point("save:test", "Home")));
                assert!(controller.set_waypoint_3d("save:test", Dimension::Overworld, 10, -5, false));
                assert!(!controller.set_waypoint_3d("save:test", Dimension::Overworld, 10, -5, false));
                assert!(!controller.waypoints("save:test")[0].show_3d);
                controller.shutdown();
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                assert!(!reopened.waypoints("save:test")[0].show_3d);
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn death_waypoints_are_numbered_black_skulls_that_persist_their_biome() {
                let directory = scratch();
                let world = "save:death";
                let controller = Controller::open(directory.clone());
                assert!(controller.add_death_waypoint(
                    world,
                    Dimension::Overworld,
                    4,
                    64,
                    8,
                    Some("minecraft:dappled_forest")
                ));
                assert!(controller.add_death_waypoint(world, Dimension::Overworld, 12, 70, 16, None));
                assert!(controller.add_death_waypoint(world, Dimension::Overworld, 4, 64, 8, None));
                assert!(!controller.add_death_waypoint(world, Dimension::Overworld, 4, 64, 8, None));
                let points = controller.waypoints(world);
                assert_eq!(points.len(), 2);
                assert_eq!(points[0].name, "Death 2");
                assert_eq!(points[0].options.purpose, 2);
                assert_eq!(points[1].name, "Death");
                assert_eq!(points[1].options.purpose, 1);
                assert!(points.iter().all(|point| {
                    point.icon == WaypointIcon::Skull && point.color == DEATH_WAYPOINT_COLOR
                }));
                assert_eq!(points[1].biome.as_deref(), Some("minecraft:dappled_forest"));
                assert!(controller.update_waypoint_biome(
                    world,
                    Dimension::Overworld,
                    12,
                    16,
                    "minecraft:plains"
                ));
                assert!(!controller.update_waypoint_biome(
                    world,
                    Dimension::Overworld,
                    12,
                    16,
                    "minecraft:plains"
                ));
                controller.shutdown();
                let reopened = Controller::open(directory.clone());
                reopened.shutdown();
                let saved = reopened.waypoints(world);
                assert_eq!(saved[0].biome.as_deref(), Some("minecraft:plains"));
                assert_eq!(saved[1].biome.as_deref(), Some("minecraft:dappled_forest"));
                assert_eq!(saved[0].icon, WaypointIcon::Skull);
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn respawn_prefers_the_server_death_block_and_ignores_portals_and_reconnects() {
                let world = "server:example";
                let mut capture = DeathCapture::new();
                let previous = (Dimension::Overworld, [21, 74, -7]);
                let fallback = (Dimension::Overworld, [9, 71, -14]);
                let authoritative = (Dimension::Nether, [-17, 42, 61]);
                capture.start(Some(previous));
                assert_eq!(capture.respawn(world, Some(previous), false, false), None);
                capture.combat(world.to_owned(), fallback);
                assert_eq!(capture.respawn(world, Some(authoritative), true, true), Some(authoritative));
                assert_eq!(capture.respawn(world, Some(authoritative), false, false), None);
                capture.combat(world.to_owned(), fallback);
                assert_eq!(capture.respawn(world, None, true, true), Some(fallback));
                capture.combat(world.to_owned(), fallback);
                assert_eq!(capture.disconnect(), Some((world.to_owned(), fallback)));
                assert_eq!(capture.disconnect(), None);
                capture.start(Some(authoritative));
                assert_eq!(capture.respawn(world, Some(authoritative), false, false), None);
                assert_eq!(capture.respawn(world, Some(previous), true, false), Some(previous));
                capture.combat(world.to_owned(), fallback);
                assert_eq!(capture.respawn("server:other", Some(authoritative), true, true), None);
            }

            #[test]
            fn reaching_death_waypoints_uses_three_dimensional_distance_and_skips_letters() {
                let mut death = point("save:death", "Death");
                death.icon = WaypointIcon::Skull;
                assert!(death_waypoint_reached([10.5, 64.0, -4.5], &death));
                assert!(!death_waypoint_reached([10.5, 64.0, 0.0], &death));
                assert!(!death_waypoint_reached([10.5, 72.0, -4.5], &death));
                assert!(!death_waypoint_reached(
                    [10.5, 64.0, -4.5],
                    &point("save:home", "Home")
                ));
            }

            #[test]
            fn v3_records_default_to_letters_without_a_biome() {
                let legacy =
                    Waypoint::decode("waypoint_v3=save%3Atest:overworld:1:2:3:ff6b6b:Old").unwrap();
                assert_eq!(legacy.icon, WaypointIcon::Letter);
                assert_eq!(legacy.biome, None);
                assert_eq!(legacy.color, 0xff6b6b);
                let v4 = Waypoint::decode(
                    "waypoint_v4=save%3Atest:overworld:1:64:2:000000:skull:minecraft%3Adappled_forest:Death",
                )
                .unwrap();
                assert_eq!(v4.icon, WaypointIcon::Skull);
                assert_eq!(v4.color, DEATH_WAYPOINT_COLOR);
                assert_eq!(v4.biome.as_deref(), Some("minecraft:dappled_forest"));
                assert_eq!(
                    Waypoint::decode(v4.encode().trim()).unwrap().biome,
                    v4.biome
                );
            }

            #[test]
            fn waypoint_names_roundtrip_and_waypoints_can_be_removed() {
                let directory = scratch();
                let waypoint = Waypoint {
                    world: "save:test".into(),
                    dimension: Dimension::Overworld,
                    x: 1,
                    y: 2,
                    z: 3,
                    name: "Дом: у % реки".into(),
                    initials: None,
                    options: WaypointOptions::default(),
                    color: 0x5aa9ff,
                    icon: WaypointIcon::Letter,
                    biome: Some("minecraft:plains".into()),
                    show_3d: true,
                };
                assert_eq!(
                    Waypoint::decode(waypoint.encode().trim()).unwrap(),
                    waypoint
                );
                // 0.5.0 wrote v2 lines without a colour; they keep the default marker.
                let legacy = Waypoint::decode("waypoint_v2=save%3Atest:overworld:1:2:3:Старое").unwrap();
                assert_eq!(legacy.name, "Старое");
                assert_eq!(legacy.color, DEFAULT_WAYPOINT_COLOR);
                let controller = Controller::open(directory.clone());
                assert!(controller.add_waypoint(waypoint));
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while controller.waypoints("save:test").is_empty() {
                    assert!(std::time::Instant::now() < until);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                assert!(controller.update_waypoint(
                    "save:test",
                    Dimension::Overworld,
                    1,
                    3,
                    "Новое имя",
                    0xff6b6b
                ));
                controller.shutdown();
                let reopened = Controller::open(directory.clone());
                let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while reopened.waypoints("save:test").is_empty() {
                    assert!(std::time::Instant::now() < until);
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                let saved_waypoint = reopened.waypoints("save:test")[0].clone();
                assert_eq!(saved_waypoint.name, "Новое имя");
                assert_eq!(saved_waypoint.color, 0xff6b6b);
                assert!(reopened.remove_waypoint("save:test", Dimension::Overworld, 1, 3));
                reopened.shutdown();
                let (_, saved) = read_profile(&directory.join(SETTINGS_FILE)).unwrap();
                assert!(saved.is_empty());
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn bounded_notification_queue_persists_latest_edit_after_a_burst() {
                let directory = scratch();
                let controller = Controller::open(directory.clone());
                for _ in 0..1001 {
                    controller.click(0);
                }
                controller.click(usize::MAX);
                controller.shutdown();
                assert!(
                    !read_settings(&directory.join(SETTINGS_FILE))
                        .unwrap()
                        .enabled
                );
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn failed_save_is_visible_and_keeps_the_previous_file_intact() {
                let directory = scratch();
                fs::write(&directory, b"not a directory").unwrap();
                let controller = Controller::open(directory.clone());
                controller.click(0);
                controller.shutdown();
                assert!(controller.labels().last().unwrap().contains(tr(
                    "Failed",
                    "Ошибка",
                    "Échec",
                    "Error",
                    "Fehler"
                )));
                assert_eq!(fs::read(&directory).unwrap(), b"not a directory");
                fs::remove_file(directory).unwrap();
            }

            #[test]
            fn shutdown_retries_unsaved_preferences_after_a_transient_error() {
                let directory = scratch();
                fs::write(&directory, b"temporarily blocked").unwrap();
                let controller = Controller::open(directory.clone());
                controller.click(0);
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let failed = controller
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .error
                        .as_deref()
                        .is_some_and(|e| e.starts_with("Save:"));
                    if failed {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "worker never reported the failed save"
                    );
                    thread::sleep(std::time::Duration::from_millis(1));
                }
                fs::remove_file(&directory).unwrap();
                fs::create_dir(&directory).unwrap();
                controller.shutdown();
                assert!(
                    !read_settings(&directory.join(SETTINGS_FILE))
                        .unwrap()
                        .enabled
                );
                assert!(controller.state.lock().unwrap().error.is_none());
                fs::remove_dir_all(directory).unwrap();
            }

            #[test]
            fn map_size_fits_gui_viewport_without_overwriting_preference() {
                let settings = Settings {
                    size_px: 128,
                    ..Settings::default()
                };
                assert_eq!(settings.fitted_size(320.0, 240.0), Some(128.0));
                assert_eq!(settings.fitted_size(100.0, 100.0), Some(36.0));
                assert_eq!(settings.fitted_size(100.0, 80.0), Some(16.0));
                assert_eq!(settings.fitted_size(1.0, 1.0), None);
                assert_eq!(settings.size_px, 128);
            }
        }
    };
}

#[cfg(test)]
pub(crate) use define_tests;
