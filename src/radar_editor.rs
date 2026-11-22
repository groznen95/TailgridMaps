// SPDX-License-Identifier: GPL-3.0-only
use super::{
    Game,
    minimap_compat::{MapAccess, xaero_radar_categories},
    reworked_maps::{button, fit_text, inside, settings_panel},
};
use ::trailgrid_maps::{radar::Selector, runtime, xaero_settings};
use horizon_client::hud::HudBatch;
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const LIMIT: usize = 256 * 1024;
const MAX_DEPTH: usize = 17;
const MAX_NODES: usize = 2047;
const CHILDREN: &str = "subCategories";
const DEFAULT: &str = "Default";

fn edited_path(data: &Path, profile: Option<&str>) -> PathBuf {
    xaero_settings::commands_directory(data, profile).join("radar-categories-edited.json")
}

pub(super) fn source(data: &Path, profile: Option<&str>) -> io::Result<Option<String>> {
    match File::open(edited_path(data, profile)) {
        Ok(file) => {
            let mut content = String::new();
            file.take(LIMIT as u64 + 1).read_to_string(&mut content)?;
            if content.len() > LIMIT {
                return Err(io::Error::other("Radar categories exceed 256 KiB"));
            }
            Ok(Some(content))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            xaero_settings::read_radar_categories(&xaero_settings::profile_directory(data, profile))
        }
        Err(err) => Err(err),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Name,
    Rule,
    List,
    Bool,
    ExcludeMode,
    Number,
}

#[derive(Clone, Copy)]
struct Field {
    key: &'static str,
    en: &'static str,
    ru: &'static str,
    kind: Kind,
    override_value: bool,
}

const fn member(key: &'static str, en: &'static str, ru: &'static str, kind: Kind) -> Field {
    Field {
        key,
        en,
        ru,
        kind,
        override_value: false,
    }
}

const fn setting(key: &'static str, en: &'static str, ru: &'static str, kind: Kind) -> Field {
    Field {
        key,
        en,
        ru,
        kind,
        override_value: true,
    }
}

const FIELDS: &[Field] = &[
    member("name", "Name", "Название", Kind::Name),
    member("hardInclude", "Base filter", "Основной фильтр", Kind::Rule),
    member("includeList", "Include", "Добавить в радар", Kind::List),
    member("excludeList", "Exclude", "Исключить из радара", Kind::List),
    member(
        "excludeMode",
        "Exclusion mode",
        "Режим исключения",
        Kind::ExcludeMode,
    ),
    member(
        "includeListInSuperCategory",
        "Share includes with parent",
        "Учитывать список в родительской категории",
        Kind::Bool,
    ),
    setting("displayed", "Visible", "Видимость", Kind::Bool),
    setting("color", "Color (0–16)", "Цвет (0–16)", Kind::Number),
    setting("icons", "Icons (0–2)", "Иконки (0–2)", Kind::Number),
    setting("names", "Names (0–2)", "Имена (0–2)", Kind::Number),
    setting(
        "dotSize",
        "Dot size (1–10)",
        "Размер точки (1–10)",
        Kind::Number,
    ),
    setting(
        "iconScale",
        "Icon scale (0.25–2)",
        "Масштаб иконок (0,25–2)",
        Kind::Number,
    ),
    setting(
        "heightLimit",
        "Height range (0–2050)",
        "Диапазон высоты (0–2050)",
        Kind::Number,
    ),
    setting(
        "heightBasedFade",
        "Fade by height",
        "Затухание по высоте",
        Kind::Bool,
    ),
    setting(
        "displayHeight",
        "Height label (0–3)",
        "Подпись высоты (0–3)",
        Kind::Number,
    ),
    setting(
        "startFadingAt",
        "Start fading at (0–256)",
        "Начало затухания (0–256)",
        Kind::Number,
    ),
    setting(
        "entityNumber",
        "Limit (0–1000)",
        "Максимум сущностей (0–1000)",
        Kind::Number,
    ),
    setting(
        "alwaysDisplayNametags",
        "Always show name tags",
        "Всегда показывать имена",
        Kind::Bool,
    ),
    setting(
        "displayNameWhenIconFails",
        "Name when icon fails",
        "Имя при отсутствии иконки",
        Kind::Bool,
    ),
    setting(
        "renderOverMinimapFrame",
        "Draw over frame (0–2)",
        "Поверх рамки (0–2)",
        Kind::Number,
    ),
    setting(
        "renderOrder",
        "Draw order (0–1000)",
        "Порядок отрисовки (0–1000)",
        Kind::Number,
    ),
];

fn new_category(name: &str) -> Value {
    json!({"name": name, "hardInclude": "nothing", "subCategories": []})
}

fn node<'a>(mut tree: &'a Value, path: &[usize]) -> Option<&'a Value> {
    for index in path {
        tree = tree.get(CHILDREN)?.get(*index)?;
    }
    Some(tree)
}

fn node_mut<'a>(mut tree: &'a mut Value, path: &[usize]) -> Option<&'a mut Value> {
    for index in path {
        tree = tree.get_mut(CHILDREN)?.get_mut(*index)?;
    }
    Some(tree)
}

fn child_count(node: &Value) -> usize {
    node.get(CHILDREN)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn nodes(node: &Value) -> usize {
    1 + node
        .get(CHILDREN)
        .and_then(Value::as_array)
        .map_or(0, |children| children.iter().map(nodes).sum())
}

fn name(value: &Value) -> &str {
    value["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .unwrap_or(DEFAULT)
}

impl Field {
    fn value(self, node: &Value) -> &Value {
        if self.override_value {
            &node["settingOverrides"][self.key]
        } else {
            &node[self.key]
        }
    }

    fn parse(self, text: &str) -> Option<Value> {
        let text = text.trim();
        if text.is_empty() {
            return (self.kind != Kind::Name).then_some(Value::Null);
        }
        match self.kind {
            Kind::Name => (text.len() <= 160).then(|| json!(text)),
            Kind::Rule => Selector::hard_rule(text).map(|_| json!(text)),
            Kind::List => {
                let items = serde_json::from_str::<Vec<String>>(text).ok()?;
                items
                    .iter()
                    .all(|item| Selector::from_xaero(item).is_some())
                    .then(|| json!(items))
            }
            Kind::Bool => text.parse::<bool>().ok().map(|value| json!(value)),
            Kind::ExcludeMode => matches!(text, "ALL_BUT" | "ONLY").then(|| json!(text)),
            Kind::Number => {
                let value = serde_json::from_str::<serde_json::Number>(text).ok()?;
                let json = json!({"name":"validation", "settingOverrides":{(self.key): value}});
                let mut skipped = Vec::new();
                xaero_radar_categories(&json.to_string(), &mut skipped)?;
                skipped.is_empty().then(|| json!(value))
            }
        }
    }

    fn set(self, node: &mut Value, value: Value) {
        let target = if self.override_value {
            &mut node["settingOverrides"]
        } else {
            node
        };
        if value.is_null() {
            if let Some(object) = target.as_object_mut() {
                object.remove(self.key);
            }
        } else {
            if !target.is_object() {
                *target = json!({});
            }
            target[self.key] = value;
        }
    }
}

pub(super) struct Editor {
    tree: Value,
    path: Vec<usize>,
    target: PathBuf,
    scroll: usize,
    input: Option<(usize, String)>,
    message: String,
    dirty: bool,
    confirm: bool,
    discard: bool,
}

impl Editor {
    pub(super) fn open(data: &Path, profile: Option<&str>) -> io::Result<Self> {
        let tree = match source(data, profile)? {
            Some(text) => serde_json::from_str::<Value>(&text)?,
            None => json!({"name": DEFAULT, "hardInclude": "anything", "subCategories": []}),
        };
        if !tree.is_object() {
            return Err(io::Error::other(
                "The radar category root must be an object",
            ));
        }
        Ok(Self {
            tree,
            path: Vec::new(),
            target: edited_path(data, profile),
            scroll: 0,
            input: None,
            message: String::new(),
            dirty: false,
            confirm: false,
            discard: false,
        })
    }

    fn current(&self) -> &Value {
        node(&self.tree, &self.path).expect("editor path points to a category")
    }

    fn current_mut(&mut self) -> &mut Value {
        node_mut(&mut self.tree, &self.path).expect("editor path points to a category")
    }

    fn path_label(&self) -> String {
        (0..=self.path.len())
            .filter_map(|depth| node(&self.tree, &self.path[..depth]).map(name))
            .collect::<Vec<_>>()
            .join(" / ")
    }

    fn navigate(&mut self) {
        self.input = None;
        self.scroll = 0;
        self.message.clear();
        self.confirm = false;
        self.discard = false;
    }

    fn finish_field(&mut self) -> bool {
        let Some((index, text)) = &self.input else {
            return true;
        };
        let field = FIELDS[*index];
        let Some(value) = field.parse(text) else {
            return false;
        };
        field.set(self.current_mut(), value);
        self.dirty = true;
        self.input = None;
        self.message.clear();
        runtime::set_text_input(false);
        true
    }

    pub(super) fn key(&mut self, game: &Game<'_>, key: &str, modifiers: u32) -> bool {
        if key == "key.keyboard.escape" {
            let had_input = self.input.take().is_some();
            runtime::set_text_input(false);
            return !had_input && self.leave();
        }
        if self.input.is_none() {
            return false;
        }
        match key {
            "key.keyboard.enter" | "key.keyboard.keypad.enter" => {
                if !self.finish_field() {
                    self.message = localized("Invalid value", "Недопустимое значение").to_owned();
                }
            }
            "key.keyboard.backspace" => {
                self.input.as_mut().unwrap().1.pop();
            }
            "key.keyboard.a" if modifiers & 0b1010 != 0 => self.input.as_mut().unwrap().1.clear(),
            "key.keyboard.v" if modifiers & 0b1010 != 0 => {
                use horizon_client::clipboard::Clipboard;
                let clipboard = game.clipboard.text();
                let text = &mut self.input.as_mut().unwrap().1;
                for ch in clipboard.chars().filter(|ch| !ch.is_control()) {
                    if text.len() + ch.len_utf8() > 8192 {
                        break;
                    }
                    text.push(ch);
                }
            }
            _ => {}
        }
        false
    }

    pub(super) fn character(&mut self, c: char) {
        if let Some((_, text)) = &mut self.input
            && !c.is_control()
            && text.len() + c.len_utf8() <= 8192
        {
            text.push(c);
        }
    }

    pub(super) fn scroll(&mut self, size: [f32; 2], y: f64) {
        let maximum = (child_count(self.current()) + FIELDS.len())
            .saturating_sub(rows(settings_panel(size[0], size[1])));
        self.scroll = (self.scroll as f64 - y).round().clamp(0., maximum as f64) as usize;
    }

    fn add(&mut self) {
        if self.path.len() >= MAX_DEPTH || nodes(&self.tree) >= MAX_NODES {
            self.message =
                localized("Category limit reached", "Достигнут предел категорий").to_owned();
            return;
        }
        let current = self.current_mut();
        if !current[CHILDREN].is_array() {
            current[CHILDREN] = json!([]);
        }
        let list = current[CHILDREN].as_array_mut().unwrap();
        let index = list.len();
        list.push(new_category(localized("New category", "Новая категория")));
        self.path.push(index);
        self.navigate();
        self.dirty = true;
        self.input = Some((0, String::new()));
        runtime::set_text_input(true);
    }

    fn order(&mut self, step: isize) {
        let Some((&index, parent)) = self.path.split_last() else {
            return;
        };
        let Some(next) = index.checked_add_signed(step) else {
            return;
        };
        let siblings = node_mut(&mut self.tree, parent).unwrap()[CHILDREN]
            .as_array_mut()
            .unwrap();
        if next < siblings.len() {
            siblings.swap(index, next);
            *self.path.last_mut().unwrap() = next;
            self.dirty = true;
        }
    }

    fn delete(&mut self) {
        if self.path.is_empty() {
            return;
        }
        if !self.confirm {
            self.confirm = true;
            return;
        }
        let index = self.path.pop().unwrap();
        self.current_mut()[CHILDREN]
            .as_array_mut()
            .unwrap()
            .remove(index);
        self.navigate();
        self.dirty = true;
    }

    fn leave(&mut self) -> bool {
        if self.dirty && !self.discard {
            self.discard = true;
            self.message = localized(
                "Unsaved changes. Cancel again to discard.",
                "Есть изменения. Нажми «Отмена» ещё раз, чтобы сбросить.",
            )
            .into();
            return false;
        }
        runtime::set_text_input(false);
        true
    }

    fn save(&mut self) -> io::Result<()> {
        let text = serde_json::to_string_pretty(&self.tree)?;
        if text.len() > LIMIT {
            return Err(io::Error::other("Radar categories exceed 256 KiB"));
        }
        let mut skipped = Vec::new();
        if xaero_radar_categories(&text, &mut skipped).is_none() {
            return Err(io::Error::other("Invalid root radar category"));
        }
        static REVISION: AtomicU64 = AtomicU64::new(0);
        runtime::write_preferences_file(
            &self.target,
            &text,
            REVISION.fetch_add(1, Ordering::Relaxed),
        )?;
        self.dirty = false;
        self.message = if skipped.is_empty() {
            localized("Radar categories saved", "Категории радара сохранены").to_owned()
        } else {
            format!(
                "{}: {}",
                localized(
                    "Saved, unsupported imported fields",
                    "Сохранено. Неподдерживаемые поля импорта"
                ),
                skipped.len()
            )
        };
        Ok(())
    }
}

pub(super) enum ResultAction {
    Stay,
    Close,
    Saved,
}

fn localized(en: &'static str, ru: &'static str) -> &'static str {
    runtime::tr(en, ru, en, en, en)
}

fn rows(panel: [f32; 4]) -> usize {
    (((panel[3] - 136.) / 22.).floor() as usize).max(1)
}

fn row(panel: [f32; 4], i: usize) -> [f32; 4] {
    [
        panel[0] + 10.,
        panel[1] + 44. + i as f32 * 22.,
        panel[2] - 20.,
        20.,
    ]
}

fn action(panel: [f32; 4], index: usize) -> [f32; 4] {
    let width = (panel[2] - 24.) / 5.;
    let (x, y) = if index < 5 {
        (index, 52.)
    } else {
        (index - 2, 28.)
    };
    [
        panel[0] + 10. + x as f32 * (width + 1.),
        panel[1] + panel[3] - y,
        width,
        21.,
    ]
}

pub(super) fn draw(game: &Game<'_>, batch: &mut HudBatch, editor: &Editor) {
    let panel = settings_panel(batch.width, batch.height);
    let mouse = game.cursor();
    game.native_fill(batch, [0., 0., batch.width, batch.height], 0xd710171e);
    game.native_fill(batch, panel, 0xff23303b);
    game.native_fill(batch, [panel[0], panel[1], panel[2], 2.], 0xff62d0c2);
    game.map_text(
        batch,
        localized("Radar categories", "Категории радара"),
        [panel[0] + 10., panel[1] + 8.],
        0xfff0f5f8,
        true,
    );
    game.map_text(
        batch,
        &fit_text(game, &editor.path_label(), panel[2] - 20.),
        [panel[0] + 10., panel[1] + 25.],
        0xffb9cbd8,
        false,
    );
    let current = editor.current();
    let children = child_count(current);
    let count = rows(panel);
    let start = editor
        .scroll
        .min((children + FIELDS.len()).saturating_sub(count));
    for (i, absolute) in (start..children + FIELDS.len()).take(count).enumerate() {
        let rect = row(panel, i);
        if absolute < children {
            let child = name(&current[CHILDREN][absolute]);
            let label = format!("{child}  ›");
            button(
                game,
                batch,
                rect,
                &fit_text(game, &label, rect[2] - 10.),
                mouse,
                true,
            );
            continue;
        }
        let index = absolute - children;
        let field = FIELDS[index];
        let right = rect[2] * 0.55;
        let control = [rect[0] + rect[2] - right, rect[1], right, rect[3]];
        let active = editor
            .input
            .as_ref()
            .filter(|(editing, _)| *editing == index);
        use std::borrow::Cow;
        let value = if let Some((_, input)) = active {
            format!("{input}_").into()
        } else {
            match field.value(current) {
                Value::Null => {
                    Cow::Borrowed(localized("Inherited / unset", "Унаследовано / не задано"))
                }
                Value::String(s) => Cow::Borrowed(s.as_str()),
                value => Cow::Owned(value.to_string()),
            }
        };
        game.map_text(
            batch,
            &fit_text(game, localized(field.en, field.ru), rect[2] - right - 6.),
            [rect[0] + 2., rect[1] + 5.],
            0xffeaf1f6,
            false,
        );
        button(
            game,
            batch,
            control,
            &fit_text(game, &value, right - 8.),
            mouse,
            true,
        );
        if active.is_some() {
            game.native_fill(batch, [control[0], control[1] + 19., right, 1.], 0xff62d0c2);
        }
    }
    let help = if !editor.message.is_empty() {
        &editor.message
    } else if let Some((index, _)) = editor.input.as_ref() {
        match FIELDS[*index].kind {
            Kind::List => localized(
                r#"Example: ["minecraft:zombie","player;Steve"]"#,
                r#"Пример: ["minecraft:zombie","player;Steve"]"#,
            ),
            Kind::Rule => localized(
                "Examples: hostile, friendly, players, living, items, nothing",
                "Примеры: hostile, friendly, players, living, items, nothing",
            ),
            Kind::ExcludeMode => localized(
                "ONLY excludes matches. ALL_BUT excludes everything else.",
                "ONLY исключает совпадения. ALL_BUT исключает остальные.",
            ),
            Kind::Bool => localized("Enter true or false", "Введи true или false"),
            _ => localized("Enter applies the value", "Enter применяет значение"),
        }
    } else {
        localized(
            "Click to edit. Right click clears an override.",
            "ЛКМ редактирует. ПКМ сбрасывает значение.",
        )
    };
    game.map_text(
        batch,
        &fit_text(game, help, panel[2] - 22.),
        [panel[0] + 10., panel[1] + panel[3] - 67.],
        0xffb9cbd8,
        false,
    );
    let child = !editor.path.is_empty();
    for (index, (text, enabled)) in [
        (localized("Parent", "На уровень выше"), child),
        (localized("Add child", "Добавить"), true),
        (localized("Move up", "Выше"), child),
        (localized("Move down", "Ниже"), child),
        (
            if editor.confirm {
                localized("Confirm delete", "Подтвердить")
            } else {
                localized("Delete", "Удалить")
            },
            child,
        ),
        (localized("Save and apply", "Сохранить"), true),
        (localized("Cancel", "Отмена"), true),
    ]
    .into_iter()
    .enumerate()
    {
        let rect = action(panel, index);
        button(
            game,
            batch,
            rect,
            &fit_text(game, text, rect[2] - 5.),
            mouse,
            enabled,
        );
    }
}

pub(super) fn press(
    editor: &mut Editor,
    screen: &mod_api::ScreenView<'_>,
    mouse_button: i32,
) -> ResultAction {
    if !matches!(mouse_button, 0 | 1) {
        return ResultAction::Stay;
    }
    let panel = settings_panel(screen.rect[2], screen.rect[3]);
    if mouse_button == 0 && inside(screen.mouse, action(panel, 6)) {
        return if editor.leave() {
            ResultAction::Close
        } else {
            ResultAction::Stay
        };
    }
    if !editor.finish_field() {
        editor.message = localized(
            "Invalid value. Press Escape to cancel editing the field.",
            "Недопустимое значение. Esc отменяет редактирование поля.",
        )
        .into();
        return ResultAction::Stay;
    }
    if mouse_button == 0
        && let Some(index) = (0..6).find(|&index| inside(screen.mouse, action(panel, index)))
    {
        match index {
            0 => {
                if editor.path.pop().is_some() {
                    editor.navigate();
                }
            }
            1 => editor.add(),
            2 => editor.order(-1),
            3 => editor.order(1),
            4 => editor.delete(),
            _ => match editor.save() {
                Ok(()) => return ResultAction::Saved,
                Err(error) => editor.message = error.to_string(),
            },
        }
        return ResultAction::Stay;
    }
    editor.confirm = false;
    editor.discard = false;
    let children = child_count(editor.current());
    let start = editor
        .scroll
        .min((children + FIELDS.len()).saturating_sub(rows(panel)));
    if let Some(i) = (0..rows(panel)).find(|&i| inside(screen.mouse, row(panel, i))) {
        let absolute = start + i;
        if absolute < children {
            if mouse_button == 0 && editor.current()[CHILDREN][absolute].is_object() {
                editor.path.push(absolute);
                editor.navigate();
            }
        } else if let Some(field) = FIELDS.get(absolute - children).copied() {
            if mouse_button == 1 {
                if field.kind != Kind::Name {
                    field.set(editor.current_mut(), Value::Null);
                    editor.dirty = true;
                }
            } else {
                let value = field.value(editor.current());
                if field.kind == Kind::Bool {
                    let next = json!(!value.as_bool().unwrap_or(false));
                    field.set(editor.current_mut(), next);
                    editor.dirty = true;
                } else if field.kind == Kind::ExcludeMode {
                    let next = if value.as_str() == Some("ALL_BUT") {
                        "ONLY"
                    } else {
                        "ALL_BUT"
                    };
                    field.set(editor.current_mut(), json!(next));
                    editor.dirty = true;
                } else {
                    editor.input = Some((
                        absolute - children,
                        match value {
                            Value::Null => String::new(),
                            Value::String(s) => s.clone(),
                            _ => value.to_string(),
                        },
                    ));
                    runtime::set_text_input(true);
                }
            }
        }
    }
    ResultAction::Stay
}
