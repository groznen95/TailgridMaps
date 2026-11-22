// SPDX-License-Identifier: GPL-3.0-only
// Adapter to the published SDK. Chunk payloads remain shared immutable Arcs.
use super::Game;
use horizon_client::{chunk::Chunk, hud::HudBatch};
use horizon_world::registry::BlockStateId;
use std::{ops::Range, sync::Arc};

pub enum WorldId {
    Singleplayer(String),
    Server(String),
}
pub struct ClientState {
    pub world: Option<WorldId>,
    pub gui_size: [f32; 2],
}
pub struct BiomeId(pub u16);
pub struct Snapshot {
    origin: [i32; 2],
    width: usize,
    chunks: Vec<Option<Arc<Chunk>>>,
    height: Range<i32>,
    air: BlockStateId,
}
impl Snapshot {
    fn chunk(&self, x: i32, z: i32) -> Option<&Chunk> {
        let dx = i64::from(x >> 4) - i64::from(self.origin[0]);
        let dz = i64::from(z >> 4) - i64::from(self.origin[1]);
        if !(0..self.width as i64).contains(&dx) || dz < 0 {
            return None;
        }
        let index = usize::try_from(dz * self.width as i64 + dx).ok()?;
        self.chunks.get(index)?.as_deref()
    }
    pub fn height(&self) -> Range<i32> {
        self.height.clone()
    }
    pub fn block(&self, [x, y, z]: [i32; 3]) -> BlockStateId {
        if !self.height.contains(&y) {
            return self.air;
        }
        self.chunk(x, z)
            .map_or(self.air, |c| c.block(x & 15, y, z & 15))
    }
    pub fn biome(&self, [x, y, z]: [i32; 3]) -> Option<BiomeId> {
        self.chunk(x, z)
            .map(|c| BiomeId(c.biome(x, y.clamp(self.height.start, self.height.end - 1), z)))
    }
    pub fn light(&self, [x, y, z]: [i32; 3]) -> u8 {
        self.chunk(x, z).map_or(0xf0, |chunk| chunk.light(x & 15, y, z & 15))
    }
    pub fn section_top(&self, [x, z]: [i32; 2]) -> i32 {
        self.chunk(x, z).map_or(self.height.start, |c| {
            c.sections
                .iter()
                .rposition(|s| !s.is_uniform(self.air.0))
                .map_or(self.height.start, |s| c.min_y() + (s as i32 + 1) * 16)
        })
    }
}
pub trait MapAccess {
    fn map_client_state(&self) -> Option<ClientState>;
    fn map_chunks(&self, from: [i32; 2], to: [i32; 2]) -> Result<Snapshot, String>;
    fn map_biome(&self, pos: [i32; 3]) -> Option<String>;
    fn map_key(&self, mod_id: &str, name: &str, default: &str) -> String;
    fn map_text_width(&self, text: &str) -> f32;
    fn map_text(&self, batch: &mut HudBatch, text: &str, at: [f32; 2], color: u32, shadow: bool);
    fn map_sprite(&self, batch: &mut HudBatch, name: &str, src: [f32; 4], dst: [f32; 4]) -> bool;
    fn map_on(&self, id: &str) -> bool;
    fn map_light_if_loaded(&self, pos: [i32; 3]) -> Option<(u8, u8)>;
    fn map_option(&self, key: &str) -> Option<u8>;
    fn map_number(&self, key: &str) -> Option<f64>;
    fn map_color_option(&self, key: &str) -> Option<u32>;
}
impl MapAccess for Game<'_> {
    fn map_client_state(&self) -> Option<ClientState> {
        if !self.native_window() {
            return None;
        }
        let (w, h) = self.gui_size();
        let world = self.remote.as_ref().filter(|r| r.logged_in).and_then(|r| {
            match self.integrated.as_ref().map(|server| &server.save_folder) {
                Some(f) => Some(WorldId::Singleplayer(f.clone())),
                // A memory connection's "local" address does not identify a
                // world. Keep it transient rather than mixing guests' saves.
                None if r.session.address == "local" || r.session.address.is_empty() => None,
                None => Some(WorldId::Server(r.session.address.clone())),
            }
        });
        Some(ClientState {
            world,
            gui_size: [w, h],
        })
    }
    fn map_chunks(&self, from: [i32; 2], to: [i32; 2]) -> Result<Snapshot, String> {
        let lo = [from[0].min(to[0]), from[1].min(to[1])];
        let hi = [from[0].max(to[0]), from[1].max(to[1])];
        let count = (i64::from(hi[0]) - i64::from(lo[0]) + 1)
            .saturating_mul(i64::from(hi[1]) - i64::from(lo[1]) + 1);
        if count > 9 {
            return Err("map snapshot exceeds its 3x3 neighbourhood".into());
        }
        let mut chunks = Vec::with_capacity(count as usize);
        for z in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                chunks.push(self.world.chunks.get(&(x, z)).cloned());
            }
        }
        Ok(Snapshot {
            origin: lo,
            width: (i64::from(hi[0]) - i64::from(lo[0]) + 1) as usize,
            chunks,
            height: self.world.shape.min_y..self.world.shape.max_y(),
            air: self.world.air,
        })
    }
    fn map_biome(&self, pos: [i32; 3]) -> Option<String> {
        let p = horizon_core::position::BlockPos::new(pos[0], pos[1], pos[2]);
        let [x, y, z] = self.streaming.biome_zoom.quart_at_block(p);
        self.biome_name_at_block([x << 2, y << 2, z << 2])
    }
    fn map_key(&self, mod_id: &str, name: &str, default: &str) -> String {
        self.mod_key(mod_id, name)
            .map_or_else(|| default.to_owned(), |(key, _)| key)
    }
    fn map_text_width(&self, text: &str) -> f32 {
        HudBatch::text_width(&self.gui.atlas, text)
    }
    fn map_text(&self, batch: &mut HudBatch, text: &str, at: [f32; 2], color: u32, shadow: bool) {
        self.native_text(
            batch,
            text,
            at,
            if color >> 24 == 0 {
                color | 0xff000000
            } else {
                color
            },
            shadow,
        );
    }
    fn map_sprite(&self, batch: &mut HudBatch, name: &str, src: [f32; 4], dst: [f32; 4]) -> bool {
        self.native_sprite(batch, name, src, dst, None)
    }
    fn map_light_if_loaded(&self, [x,y,z]: [i32; 3]) -> Option<(u8,u8)> {
        if !self.world.loaded(x,z) || !(self.world.shape.min_y..self.world.shape.max_y()).contains(&y) {
            return None;
        }
        let light = self.world.light(x, y, z);
        Some((light & 15, light >> 4))
    }
    fn map_option(&self, key: &str) -> Option<u8> {
        use horizon_client::mod_list::NativeSettingValue;
        match setting(self, key)? {
            NativeSettingValue::Toggle(value) => Some(u8::from(*value)),
            NativeSettingValue::Choice { index, .. } => u8::try_from(*index).ok(),
            NativeSettingValue::Range { value, .. } => Some(value.round() as u8),
            NativeSettingValue::Color(_) => None,
        }
    }
    fn map_color_option(&self, key: &str) -> Option<u32> {
        if let horizon_client::mod_list::NativeSettingValue::Color(rgb) = setting(self,key)? {
            Some(*rgb)
        } else { None }
    }
    fn map_number(&self, key: &str) -> Option<f64> {
        match setting(self, key)? {
            horizon_client::mod_list::NativeSettingValue::Range {value,..} => Some(*value),
            _ => None,
        }
    }
    fn map_on(&self, id: &str) -> bool {
        !self.settings.mods.disabled.contains(id)
    }
}

fn setting<'a>(game: &'a Game<'_>, key: &str) -> Option<&'a horizon_client::mod_list::NativeSettingValue> {
    Some(&game.settings.mods.installed.iter()
        .find(|m| m.id == ::trailgrid_maps::runtime::MOD_ID)?
        .native.as_ref()?.settings.iter().find(|row| row.key == key)?.value)
}

pub fn xaero_radar_categories(source: &str, skipped: &mut Vec<String>) -> Option<::trailgrid_maps::radar::ImportedCategory> {
    use ::trailgrid_maps::radar::{ImportedCategory, Selector};
    use serde_json::Value;

    fn decode(node: &Value, skipped: &mut Vec<String>, depth: usize, next: &mut usize) -> Option<ImportedCategory> {
        let object = node.as_object()?;
        let name = node["name"].as_str().unwrap_or_default();
        if depth > 18 || name.len() > 160 || *next > 2048 {
            skipped.push("radar/category_limit".to_owned());
            return None;
        }
        *next += 1;
        let mut category = ImportedCategory {id:*next,..Default::default()};
        if let Some(rule)=node["hardInclude"].as_str() {
            category.base = Selector::hard_rule(rule);
            if category.base.is_none() {
                skipped.push(format!("radar/{name}/{rule}"));
                return None;
            }
        }
        for (field, selectors) in [("includeList",&mut category.includes),("excludeList",&mut category.excludes)] {
            if let Some(values) = object.get(field).and_then(Value::as_array) {
                for value in values {
                    let Some(selector) = value.as_str().and_then(Selector::from_xaero) else {
                        skipped.push(format!("radar/{name}/{field}"));
                        return None;
                    };
                    selectors.push(selector);
                }
            }
        }
        category.reversed = node["excludeMode"].as_str() == Some("ALL_BUT");
        category.inherit_includes = node["includeListInSuperCategory"].as_bool().unwrap_or(false);
        if let Some(overrides) = node["settingOverrides"].as_object() {
            let rule = &mut category.rule;
            for (key, value) in overrides {
                let number = || value.as_f64();
                let integer = || number().filter(|n| n.fract() == 0. && *n >= 0.).map(|n| n as usize);
                let real = |min:f64,max:f64| number().filter(|n| (min..=max).contains(n));
                let pick = |max:usize| integer().filter(|n| *n<=max).map(|n| n as u8);
                let valid = match key.as_str() {
                    "displayed" => { rule.visible=value.as_bool(); rule.visible.is_some() }
                    "color" => {
                        const CHAT: [u32;16] = [0,0x0000aa,0x00aa00,0x00aaaa,0xaa0000,0xaa00aa,0xffaa00,
                            0xaaaaaa,0x555555,0x5555ff,0x55ff55,0x55ffff,0xff5555,0xff55ff,0xffff55,0xffffff];
                        rule.rgb = integer().and_then(|index| CHAT.get(index)).copied();
                        rule.team_color = integer().filter(|n| *n<=16).map(|n|n==16);
                        rule.team_color.is_some()
                    }
                    "icons" => {rule.icon=pick(2);rule.icon.is_some()}
                    "names" => {rule.label=pick(2);rule.label.is_some()}
                    "dotSize" => {rule.size=real(1.,10.).map(|n|n as f32);rule.size.is_some()}
                    "heightLimit" => {rule.height=real(0.,2050.);rule.height.is_some()}
                    "iconScale" => {rule.icon_scale=real(0.25,2.).map(|n| n as f32);rule.icon_scale.is_some()}
                    "heightBasedFade" => {rule.height_fade=value.as_bool();rule.height_fade.is_some()}
                    "displayHeight" => {rule.height_label=pick(3);rule.height_label.is_some()}
                    "startFadingAt" => {rule.fade_from=real(0.,256.);rule.fade_from.is_some()}
                    "entityNumber" => {rule.limit=real(0.,1000.).map(|n| n as u16);rule.limit.is_some()}
                    "alwaysDisplayNametags" => {rule.nametag=value.as_bool();rule.nametag.is_some()}
                    "displayNameWhenIconFails" => {rule.icon_fallback=value.as_bool();rule.icon_fallback.is_some()}
                    "renderOverMinimapFrame" => {rule.over_frame=pick(2);rule.over_frame.is_some()}
                    "renderOrder" => {rule.order=real(0.,1000.).map(|n| n as u16);rule.order.is_some()}
                    _ => false,
                };
                if !valid { skipped.push(format!("radar/{name}/{key}")); }
            }
        }
        if let Some(children) = node["subCategories"].as_array() {
            category.children = children.iter().filter_map(|child| decode(child,skipped,depth+1,next)).collect();
        }
        Some(category)
    }

    let node = serde_json::from_str::<Value>(source).ok()?;
    decode(&node,skipped,0,&mut 0)
}

fn setting_text(game:&Game<'_>,key:&str) -> Option<String> {
    use horizon_client::mod_list::NativeSettingValue as Value;
    Some(match setting(game,key)? {
        Value::Toggle(flag)=>flag.to_string(),
        Value::Choice {index,..}=>index.to_string(),
        Value::Range {value,..}=>value.to_string(),
        Value::Color(rgb)=>rgb.to_string(),
    })
}

pub(super) fn write_map_setting(game:&mut Game<'_>,key:&str,value:&str) -> bool {
    use horizon_client::mod_list::NativeSettingValue as Value;
    use ::trailgrid_maps::runtime::MOD_ID;
    let Some(mut next)=setting(game,key).cloned() else {return false;};
    let valid=match &mut next {
        Value::Toggle(flag)=>value.parse::<bool>().map(|v|*flag=v).is_ok(),
        Value::Choice {options,index}=>value.parse::<usize>().ok()
            .filter(|n|*n<options.len()).map(|v|*index=v).is_some(),
        Value::Range {min,max,value:stored,..}=>value.parse::<f64>().ok()
            .filter(|n|(*min..=*max).contains(n)).map(|v|*stored=v).is_some(),
        Value::Color(rgb)=>value.parse::<u32>().ok().filter(|v|*v<=0xffffff).map(|v|*rgb=v).is_some(),
    };
    if !valid || !game.store_setting(MOD_ID,key,value) {return false;}
    if let Some(row)=game.settings.mods.installed.iter_mut()
        .find(|m|m.id==MOD_ID).and_then(|m|m.native.as_mut())
        .and_then(|page|page.settings.iter_mut().find(|row|row.key==key)) {
        row.value=next;
    }
    true
}

pub fn apply_xaero_settings(game: &mut Game<'_>, mut import: ::trailgrid_maps::xaero_settings::Import)
    -> std::io::Result<(usize,Vec<String>,Option<String>)>
{
    use ::trailgrid_maps::{runtime::{MOD_ID,write_preferences_file},xaero_settings};
    use std::{collections::BTreeMap,fs::File,io::{self,Read}};
    let directory=game.native_data_dir(MOD_ID);
    let mut applied=0;
    let source = super::minimap_radar_editor::source(&directory, import.profile.as_deref())
        .ok().flatten()
        .or_else(|| import.radar_categories.take());
    let activate_radar=match source {
        Some(source) if xaero_radar_categories(&source,&mut import.unsupported).is_some()=>Some(true),
        Some(_)=>{import.unsupported.push("entity_radar_categories".to_owned());Some(false)}
        None if import.profile.is_some()=>Some(false),
        None=>None,
    };
    if let Some(enabled)=activate_radar {
        import.settings.insert("use_xaero_radar",enabled.to_string());
    }
    let restore_file=directory.join("xaero-profile-restore.json");
    if import.switching || import.profile.is_some() {
        let mut restore=match File::open(&restore_file) {
            Ok(file)=>{
                let mut text=String::new();
                file.take(65537).read_to_string(&mut text)?;
                if text.len()>65536 {return Err(io::Error::other("Xaero profile restore exceeds 64 KiB"));}
                serde_json::from_str::<BTreeMap<String,String>>(&text)?
            }
            Err(error) if error.kind()==io::ErrorKind::NotFound=>BTreeMap::new(),
            Err(error)=>return Err(error),
        };
        if import.switching {
            for (key,value) in &restore {
                if !write_map_setting(game,key,value) {return Err(io::Error::other(format!("Cannot restore Xaero setting {key}")));}
            }
            restore.clear();
        }
        if import.profile.is_some() {
            for key in import.settings.keys() {
                if let Some(value)=setting_text(game,key) {restore.entry((*key).to_owned()).or_insert(value);}
            }
        }
        write_preferences_file(&restore_file,&serde_json::to_string(&restore)?,0)?;
        let active_path=directory.join("xaero-profile");
        write_preferences_file(&active_path,import.profile.as_deref().unwrap_or(""),0)?;
    }
    if !import.commands.is_empty() {
        let keys:Vec<_>=import.commands.keys().copied().collect();
        let target=xaero_settings::commands_directory(&directory,import.profile.as_deref());
        match xaero_settings::merge_teleport_commands(&target,import.commands) {
            Ok(())=>applied+=keys.len(),
            Err(error)=>{
                eprintln!("[{MOD_ID}] Teleport settings: {error}");
                import.unsupported.extend(keys.into_iter().map(str::to_owned));
            }
        }
    }
    for (key,value) in import.settings {
        if write_map_setting(game,key,&value) {applied+=1;} else {import.unsupported.push(key.to_owned());}
    }
    Ok((applied,import.unsupported,import.profile))
}

const SETTING_SLOTS: usize = ::trailgrid_maps::runtime::SDK_SETTING_SLOTS;

struct Synced {
    values: [Option<u64>; SETTING_SLOTS],
    preferences: ::trailgrid_maps::runtime::minimap::Settings,
}
fn row_button(key: &str) -> Option<usize> {
    use ::trailgrid_maps::runtime as rt;
    match key {
        "cache_ram_mib" => Some(18),
        "cache_manager" => Some(19),
        "cave_y" => Some(25),
        "reset" => Some(6),
        _ => rt::SDK_SETTING_ROWS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, button)| *button),
    }
}
// Only changed preferences reach the SDK's persistence API. Ordinary frames
// compare fixed-size snapshots without filesystem access or allocations.
pub fn sync_settings(game: &mut Game<'_>) {
    use ::trailgrid_maps::{self as entry, runtime as rt};
    use horizon_client::mod_list::NativeSettingValue as Value;
    static LAST: std::sync::OnceLock<std::sync::Mutex<Synced>> = std::sync::OnceLock::new();
    let controller = rt::initialize(game.native_data_dir(rt::MOD_ID));
    let mut sync = LAST
        .get_or_init(|| {
            let initial = entry::initial_settings();
            let mut values = [None; SETTING_SLOTS];
            for (key, button) in rt::SDK_SETTING_ROWS {
                let _ = key;
                let index = initial.option_index(button).unwrap_or(0);
                controller.set_option(button, index);
                values[button] = Some(index as u64);
            }
            controller.set_cache_ram_mib(initial.cache_ram_mib);
            controller.set_cave_y(initial.cave_y);
            values[18] = Some(f64::from(initial.cache_ram_mib).to_bits());
            values[25] = Some(f64::from(initial.cave_y).to_bits());
            std::sync::Mutex::new(Synced {
                values,
                preferences: initial,
            })
        })
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut values = sync.values;
    if let Some(page) = game
        .settings
        .mods
        .installed
        .iter()
        .find(|m| m.id == rt::MOD_ID)
        .and_then(|m| m.native.as_ref())
    {
        for row in &page.settings {
            if let Some(button) = row_button(&row.key) {
                values[button] = match &row.value {
                    Value::Choice { index, .. } => Some(*index as u64),
                    Value::Range { value, .. } => Some(value.to_bits()),
                    _ => None,
                };
            }
        }
    }
    if values == sync.values && controller.settings() == sync.preferences {
        return;
    }
    let mut from_sdk = [false; SETTING_SLOTS];
    let mut reset = false;
    let mut caches = false;
    for (button, value) in values.iter().enumerate() {
        if *value == sync.values[button] {
            continue;
        }
        let Some(value) = value else {
            continue;
        };
        from_sdk[button] = true;
        match button {
            6 => reset = sync.values[6].is_some(),
            19 => caches = sync.values[19].is_some(),
            18 => {
                controller.set_cache_ram_mib(f64::from_bits(*value).round().clamp(16., 512.) as u16)
            }
            25 => controller.set_cave_y(f64::from_bits(*value).round() as i32),
            _ => {
                controller.set_option(button, *value as usize);
            }
        }
    }
    if reset {
        controller.click(6);
    }
    let current = controller.settings();
    let mut writes = Vec::new();
    if current != sync.preferences {
        for (key, button) in rt::SDK_SETTING_ROWS {
            let selected = current.option_index(button).unwrap_or(0);
            if !from_sdk[button]
                && (reset || current.option_index(button) != sync.preferences.option_index(button))
            {
                writes.push((key, selected.to_string()));
            }
            values[button] = Some(selected as u64);
        }
        if !from_sdk[18] && (reset || current.cache_ram_mib != sync.preferences.cache_ram_mib) {
            writes.push(("cache_ram_mib", current.cache_ram_mib.to_string()));
        }
        values[18] = Some(f64::from(current.cache_ram_mib).to_bits());
        if !from_sdk[25] && (reset || current.cave_y != sync.preferences.cave_y) {
            writes.push(("cave_y", current.cave_y.to_string()));
        }
        values[25] = Some(f64::from(current.cave_y).to_bits());
    }
    if let Some(page) = game
        .settings
        .mods
        .installed
        .iter_mut()
        .find(|m| m.id == rt::MOD_ID)
        .and_then(|m| m.native.as_mut())
    {
        for row in &mut page.settings {
            if let Some(button) = row_button(&row.key) {
                match &mut row.value {
                    Value::Choice { index, .. } if button != 6 && button != 19 => {
                        *index = values[button].unwrap_or(0) as usize
                    }
                    Value::Range { value, .. } if button == 18 => {
                        *value = f64::from(current.cache_ram_mib)
                    }
                    Value::Range { value, .. } if button == 25 => *value = f64::from(current.cave_y),
                    _ => {}
                }
            }
        }
    }
    sync.values = values;
    sync.preferences = current;
    drop(sync);
    for (key, value) in writes {
        game.store_setting(rt::MOD_ID, key, &value);
    }
    if caches {
        super::reworked_maps::open_cache_manager(game);
    }
}
