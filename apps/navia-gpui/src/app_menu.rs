//! the desktop's global menu on Linux: the menus given to `cx.set_menus`, served on the
//! session bus as `com.canonical.dbusmenu`, with the main window pointed at them through KDE Plasma's app menu
//! protocol. desktops without a global menu never ask for it.

use gpui::{App, Menu, MenuItem};

use crate::i18n::tr;
use crate::shell::{CheckForUpdates, GetInfo, OpenSettings, Quit, ToggleMiniPlayer};

/// what the menus show that changes while Navia runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MenuState {
    pub mini_open: bool,
    pub has_song: bool,
}

/// the Mac menu bar's own items, laid out the way Linux apps have them.
pub fn menus(state: MenuState) -> Vec<Menu> {
    vec![
        Menu::new(tr("File")).items([
            MenuItem::action(tr("Get Info"), GetInfo),
            MenuItem::action(tr("Settings…"), OpenSettings),
            MenuItem::separator(),
            MenuItem::action(tr("Quit"), Quit),
        ]),
        Menu::new(tr("Window")).items([MenuItem::action(tr("Mini Player"), ToggleMiniPlayer).checked(state.mini_open).disabled(!state.mini_open && !state.has_song)]),
        Menu::new(tr("Help")).items([MenuItem::action(tr("Check for Updates…"), CheckForUpdates)]),
    ]
}

#[cfg(target_os = "linux")]
pub use imp::AppMenu;

#[cfg(target_os = "linux")]
mod imp {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use futures::channel::mpsc::UnboundedSender;
    use gpui::{Action, App, KeyBinding, OwnedMenu, OwnedMenuItem, Window};
    use zbus::blocking::Connection;
    use zbus::blocking::connection::Builder;
    use zbus::zvariant::{OwnedValue, StructureBuilder, Value};

    type Props = HashMap<Cow<'static, str>, OwnedValue>;
    /// dbusmenu's `(ia{sv}av)`: an item, its properties and its children, each child the same as a variant.
    type Layout = (i32, Props, Vec<OwnedValue>);

    const PATH: &str = "/MenuBar";
    const INTERFACE: &str = "com.canonical.dbusmenu";

    /// one entry; id 0 is the root, whose children are the menu bar's menus.
    #[derive(Default, Clone)]
    struct Item {
        label: String,
        separator: bool,
        enabled: bool,
        checked: Option<bool>,
        shortcut: Option<Vec<String>>,
        children: Vec<i32>,
    }

    #[derive(Default)]
    struct Tree {
        revision: u32,
        items: Vec<Item>,
    }

    impl Tree {
        fn properties(&self, id: i32, names: &[String]) -> Option<Props> {
            let item = self.items.get(usize::try_from(id).ok()?)?;
            let mut props = Props::new();
            let mut put = |name: &'static str, value: Value<'_>| {
                if names.is_empty() || names.iter().any(|n| n == name) {
                    if let Ok(value) = value.try_to_owned() {
                        props.insert(name.into(), value);
                    }
                }
            };
            if item.separator {
                put("type", Value::from("separator"));
                return Some(props);
            }
            if id != 0 {
                // an underscore marks the access key in dbusmenu labels.
                put("label", Value::from(item.label.replace('_', "__")));
            }
            if !item.enabled {
                put("enabled", Value::from(false));
            }
            if let Some(checked) = item.checked {
                put("toggle-type", Value::from("checkmark"));
                put("toggle-state", Value::from(i32::from(checked)));
            }
            if let Some(keys) = &item.shortcut {
                put("shortcut", Value::from(vec![keys.clone()]));
            }
            if !item.children.is_empty() {
                put("children-display", Value::from("submenu"));
            }
            Some(props)
        }

        fn layout(&self, id: i32, depth: i32, names: &[String]) -> Option<Layout> {
            let item = self.items.get(usize::try_from(id).ok()?)?;
            let children = if depth == 0 { Vec::new() } else { item.children.iter().filter_map(|c| self.layout(*c, depth - 1, names)).filter_map(as_value).collect() };
            Some((id, self.properties(id, names)?, children))
        }
    }

    struct DbusMenu {
        tree: Arc<Mutex<Tree>>,
        clicks: UnboundedSender<i32>,
    }

    #[zbus::interface(name = "com.canonical.dbusmenu")]
    impl DbusMenu {
        fn get_layout(&self, parent_id: i32, recursion_depth: i32, property_names: Vec<String>) -> zbus::fdo::Result<(u32, Layout)> {
            let tree = self.tree.lock().unwrap();
            let layout = tree.layout(parent_id, recursion_depth, &property_names).ok_or_else(|| zbus::fdo::Error::InvalidArgs("parentId not found".into()))?;
            Ok((tree.revision, layout))
        }

        fn get_group_properties(&self, ids: Vec<i32>, property_names: Vec<String>) -> Vec<(i32, Props)> {
            let tree = self.tree.lock().unwrap();
            let all: Vec<i32> = if ids.is_empty() { (0..tree.items.len() as i32).collect() } else { ids };
            all.into_iter().filter_map(|id| tree.properties(id, &property_names).map(|p| (id, p))).collect()
        }

        fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
            let tree = self.tree.lock().unwrap();
            tree.properties(id, std::slice::from_ref(&name)).and_then(|mut p| p.remove(name.as_str())).ok_or_else(|| zbus::fdo::Error::InvalidArgs("no such property".into()))
        }

        fn event(&self, id: i32, event_id: String, _data: OwnedValue, _timestamp: u32) {
            if event_id == "clicked" {
                self.clicks.unbounded_send(id).ok();
            }
        }

        fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
            for (id, event_id, ..) in events {
                if event_id == "clicked" {
                    self.clicks.unbounded_send(id).ok();
                }
            }
            Vec::new()
        }

        /// the layout is always current, so nothing needs updating before a menu opens.
        fn about_to_show(&self, _id: i32) -> bool {
            false
        }

        fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
            (Vec::new(), Vec::new())
        }

        #[zbus(property)]
        fn version(&self) -> u32 {
            3
        }

        #[zbus(property)]
        fn text_direction(&self) -> &str {
            "ltr"
        }

        #[zbus(property)]
        fn status(&self) -> &str {
            "normal"
        }

        #[zbus(property)]
        fn icon_theme_path(&self) -> Vec<String> {
            Vec::new()
        }
    }

    /// the served menus and the action behind each id.
    pub struct AppMenu {
        conn: Connection,
        tree: Arc<Mutex<Tree>>,
        actions: Vec<Option<Box<dyn Action>>>,
    }

    impl AppMenu {
        pub fn start(clicks: UnboundedSender<i32>) -> Option<Self> {
            let tree = Arc::new(Mutex::new(Tree::default()));
            let conn = Builder::session().ok()?.serve_at(PATH, DbusMenu { tree: tree.clone(), clicks }).ok()?.build().ok()?;
            Some(Self { conn, tree, actions: Vec::new() })
        }

        /// points the window's global menu here.
        pub fn attach(&self, window: &Window) {
            if let Some(name) = self.conn.unique_name() {
                window.set_app_menu_address(name.as_str(), PATH);
            }
        }

        /// serves what `cx.set_menus` was last given.
        pub fn publish(&mut self, cx: &App) {
            let Some(menus) = cx.get_menus() else { return };
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let mut items = vec![Item { enabled: true, ..Item::default() }];
            let mut actions: Vec<Option<Box<dyn Action>>> = vec![None];
            let mut add = |item: Item, action: Option<Box<dyn Action>>| {
                items.push(item);
                actions.push(action);
                (items.len() - 1) as i32
            };
            fn walk(menu: &OwnedMenu, keymap: &gpui::Keymap, add: &mut dyn FnMut(Item, Option<Box<dyn Action>>) -> i32) -> i32 {
                let mut children = Vec::new();
                for entry in &menu.items {
                    let id = match entry {
                        OwnedMenuItem::Separator => add(Item { separator: true, ..Item::default() }, None),
                        OwnedMenuItem::Submenu(sub) => walk(sub, keymap, add),
                        OwnedMenuItem::SystemMenu(_) => continue,
                        OwnedMenuItem::Action { name, action, checked, disabled, .. } => {
                            let shortcut = keymap.bindings_for_action(action.as_ref()).last().and_then(shortcut);
                            let toggle = action.name() == crate::shell::ToggleMiniPlayer.name();
                            add(Item { label: name.clone(), enabled: !disabled, checked: toggle.then_some(*checked), shortcut, ..Item::default() }, Some(action.boxed_clone()))
                        }
                    };
                    children.push(id);
                }
                add(Item { label: menu.name.to_string(), enabled: !menu.disabled, children, ..Item::default() }, None)
            }
            let roots: Vec<i32> = menus.iter().map(|m| walk(m, &keymap, &mut add)).collect();
            items[0].children = roots;
            let revision = {
                let mut tree = self.tree.lock().unwrap();
                tree.revision += 1;
                tree.items = items;
                tree.revision
            };
            self.actions = actions;
            self.conn.emit_signal(None::<&str>, PATH, INTERFACE, "LayoutUpdated", &(revision, 0i32)).ok();
        }

        pub fn action(&self, id: i32) -> Option<Box<dyn Action>> {
            self.actions.get(usize::try_from(id).ok()?)?.as_ref().map(|a| a.boxed_clone())
        }
    }

    fn as_value((id, props, children): Layout) -> Option<OwnedValue> {
        Value::from(StructureBuilder::new().add_field(id).add_field(props).add_field(children).build().ok()?).try_to_owned().ok()
    }

    /// Qt's key names, which Plasma's menu reads.
    fn shortcut(binding: &KeyBinding) -> Option<Vec<String>> {
        let [stroke] = binding.keystrokes() else { return None };
        let m = stroke.modifiers();
        let mut keys: Vec<String> = [(m.control, "Control"), (m.alt, "Alt"), (m.shift, "Shift"), (m.platform, "Super")].into_iter().filter(|(on, _)| *on).map(|(_, k)| k.to_string()).collect();
        let key = stroke.key();
        keys.push(if key.chars().count() == 1 { key.to_uppercase() } else { return None });
        Some(keys)
    }
}

impl crate::shell::Shell {
    /// refreshes the served menus when what they show changed.
    pub fn app_menu_sync(&mut self, cx: &mut App) {
        let state = MenuState { mini_open: self.mini.is_open(), has_song: self.current_song().is_some() };
        if self.app_menu_state == Some(state) {
            return;
        }
        self.app_menu_state = Some(state);
        cx.set_menus(menus(state));
        #[cfg(target_os = "linux")]
        if let Some(menu) = self.app_menu.as_mut() {
            menu.publish(cx);
        }
    }
}
