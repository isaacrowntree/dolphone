//! The desktop's tray icon and launcher badge.
//!
//! The tray icon is a StatusNotifierItem (freedesktop's spec, which KDE, waybar,
//! sway and GNOME's AppIndicator extension host), registered with whichever
//! StatusNotifierWatcher is running (`org.kde.` or `org.freedesktop.`, at
//! `/StatusNotifierWatcher`, the item at `/StatusNotifierItem`, as sway's tray
//! expects). It asks for attention while calls are missed or texts unread; a click
//! brings the window up, a middle click opens Messages.
//!
//! The launcher badge is the Unity LauncherEntry signal (`Update`, `sa{sv}`, with
//! `count` and `count-visible`), which Dash to Dock, Plasma's task manager and
//! others read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::object_server::SignalEmitter;
use zbus::wire::{OwnedObjectPath, Variant};
use zbus::{interface, Connection};

/// StatusNotifierItem's ToolTip: icon name, icon pixmaps (width, height, ARGB),
/// title, text.
pub type ToolTip = (String, Vec<(i32, i32, Vec<u8>)>, String, String);

const ITEM_PATH: &str = "/StatusNotifierItem";
const DESKTOP_URI: &str = "application://io.github.isaacrowntree.Dolphone.desktop";
const ICON: &str = "io.github.isaacrowntree.Dolphone";

/// What the tray shows: missed calls and unread texts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub missed: u32,
    pub unread: u32,
}

impl Counts {
    fn waiting(self) -> u32 {
        self.missed + self.unread
    }

    pub fn tooltip(self) -> String {
        let part = |n: u32, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        match (self.missed, self.unread) {
            (0, 0) => "Nothing new".into(),
            (m, 0) => part(m, "missed call", "missed calls"),
            (0, u) => part(u, "new text", "new texts"),
            (m, u) => format!("{}, {}", part(m, "missed call", "missed calls"), part(u, "new text", "new texts")),
        }
    }
}

struct Item {
    counts: Arc<Mutex<Counts>>,
    on_activate: Arc<dyn Fn(bool) + Send + Sync>,
}

/// The item, under KDE's interface name, which every host reads (the spec's
/// freedesktop name is identical in methods, signals and properties).
#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> String {
        "Communications".into()
    }
    #[zbus(property)]
    fn id(&self) -> String {
        "dolphone".into()
    }
    #[zbus(property)]
    fn title(&self) -> String {
        "Dolphone".into()
    }
    #[zbus(property)]
    fn status(&self) -> String {
        let waiting = self.counts.lock().map(|c| c.waiting()).unwrap_or(0);
        if waiting > 0 { "NeedsAttention" } else { "Active" }.into()
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        ICON.into()
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> String {
        ICON.into()
    }
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let c = self.counts.lock().map(|c| *c).unwrap_or_default();
        (String::new(), Vec::new(), "Dolphone".into(), c.tooltip())
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/NO_DBUSMENU").expect("path")
    }

    fn activate(&self, _x: i32, _y: i32) {
        (self.on_activate)(false);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        (self.on_activate)(true);
    }
    fn context_menu(&self, _x: i32, _y: i32) {
        (self.on_activate)(false);
    }
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_status(em: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(em: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// The launcher badge's signal, sent from any path.
struct Launcher;

#[interface(name = "com.canonical.Unity.LauncherEntry")]
impl Launcher {
    #[zbus(signal)]
    async fn update(em: &SignalEmitter<'_>, app_uri: &str, props: HashMap<&str, Variant<'_>>) -> zbus::Result<()>;
}

/// The tray and the badge, running on their own thread. `set` updates both.
pub struct Tray {
    counts: Arc<Mutex<Counts>>,
    tx: tokio::sync::mpsc::UnboundedSender<Counts>,
}

impl Tray {
    /// Start it. `on_activate(messages)` is called (on the tray's thread) when the
    /// icon is clicked; `messages` is true for a middle click.
    pub fn start(on_activate: impl Fn(bool) + Send + Sync + 'static) -> Self {
        let counts = Arc::new(Mutex::new(Counts::default()));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Counts>();
        let shared = counts.clone();
        let on_activate: Arc<dyn Fn(bool) + Send + Sync> = Arc::new(on_activate);
        std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                let Ok(conn) = serve(shared, on_activate).await else { return };
                while let Some(c) = rx.recv().await {
                    announce(&conn, c).await;
                }
            });
        });
        Self { counts, tx }
    }

    pub fn set(&self, c: Counts) {
        let changed = self.counts.lock().map(|mut cur| std::mem::replace(&mut *cur, c) != c).unwrap_or(false);
        if changed {
            let _ = self.tx.send(c);
        }
    }
}

async fn serve(counts: Arc<Mutex<Counts>>, on_activate: Arc<dyn Fn(bool) + Send + Sync>) -> zbus::Result<Connection> {
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let conn = zbus::connection::Builder::session()
        .name(name.as_str())
        .serve_at(ITEM_PATH, Item { counts, on_activate })
        .serve_at("/io/github/isaacrowntree/Dolphone/launcher", Launcher)
        .build()
        .await?;
    // Whichever watcher there is; a desktop with none simply shows no icon.
    for watcher in ["org.kde.StatusNotifierWatcher", "org.freedesktop.StatusNotifierWatcher"] {
        let ok = conn
            .call_method(
                Some(watcher),
                "/StatusNotifierWatcher",
                Some(watcher),
                "RegisterStatusNotifierItem",
                &(name.as_str(),),
            )
            .await
            .is_ok();
        if ok {
            break;
        }
    }
    Ok(conn)
}

async fn announce(conn: &Connection, c: Counts) {
    if let Ok(em) = SignalEmitter::new(conn, ITEM_PATH) {
        let status = if c.waiting() > 0 { "NeedsAttention" } else { "Active" };
        let _ = Item::new_status(&em, status).await;
        let _ = Item::new_tool_tip(&em).await;
    }
    if let Ok(em) = SignalEmitter::new(conn, "/io/github/isaacrowntree/Dolphone/launcher") {
        let props = HashMap::from([
            ("count", Variant::from(i64::from(c.waiting()))),
            ("count-visible", Variant::from(c.waiting() > 0)),
        ]);
        let _ = Launcher::update(&em, DESKTOP_URI, props).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltips_say_what_is_waiting() {
        assert_eq!(Counts { missed: 0, unread: 0 }.tooltip(), "Nothing new");
        assert_eq!(Counts { missed: 1, unread: 0 }.tooltip(), "1 missed call");
        assert_eq!(Counts { missed: 2, unread: 1 }.tooltip(), "2 missed calls, 1 new text");
    }
}
