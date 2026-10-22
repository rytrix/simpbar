//! StatusNotifierItem tray, running the D-Bus client on its own tokio thread.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;

use calloop::channel::Sender;
use system_tray::client::{ActivateRequest, Client, Event};
use system_tray::item::{Status, StatusNotifierItem};
use system_tray::menu::TrayMenu;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::Msg;
use crate::icons::IconSource;

#[derive(Debug, Clone)]
pub struct TrayItem {
    /// The item's bus name.
    pub address: String,
    /// Candidates in order of preference.
    pub icon: Vec<IconSource>,
    pub item_is_menu: bool,
    pub menu_path: Option<String>,
    pub menu: Option<TrayMenu>,
}

#[derive(Debug)]
pub enum Request {
    Activate(ActivateRequest),
    /// Lets the app update a (sub)menu before it is shown.
    AboutToShow {
        address: String,
        menu_path: String,
        id: i32,
    },
    Scroll {
        address: String,
        delta: i32,
    },
}

pub fn spawn(tx: Sender<Msg>) -> UnboundedSender<Request> {
    let (req_tx, mut req_rx) = unbounded_channel::<Request>();
    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        rt.block_on(async move {
            let client = match Client::new().await {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    log::error!("system tray unavailable: {e}");
                    return;
                }
            };
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(e) => {
                    log::error!("system tray unavailable: {e}");
                    return;
                }
            };
            // Subscribe first so anything registering from here on produces an Add.
            let mut events = client.subscribe();
            let mut paths = item_paths(&conn).await;
            let mut order = Vec::new();
            resync(&client, &mut order);
            let _ = tx.send(Msg::Tray(snapshot(&client, &order)));

            loop {
                tokio::select! {
                    event = events.recv() => {
                        match event {
                            Ok(Event::Add(address, _)) => {
                                if !order.contains(&address) {
                                    order.push(address);
                                }
                                paths = item_paths(&conn).await;
                            }
                            Ok(Event::Remove(address)) => order.retain(|a| *a != address),
                            Ok(_) => {}
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                // Adds/Removes may have been dropped.
                                resync(&client, &mut order);
                                paths = item_paths(&conn).await;
                            }
                            Err(_) => break,
                        }
                        if tx.send(Msg::Tray(snapshot(&client, &order))).is_err() {
                            break;
                        }
                    }
                    Some(req) = req_rx.recv() => {
                        let client = client.clone();
                        let conn = conn.clone();
                        let req = with_paths(req, &paths);
                        tokio::spawn(async move {
                            if let Err(e) = handle(&client, &conn, req).await {
                                log::warn!("tray request failed: {e}");
                            }
                        });
                    }
                }
            }
        });
    });
    req_tx
}

/// Brings `order` in line with the client's items, keeping known items in
/// place and appending new ones (whose real order is unknown) sorted.
fn resync(client: &Client, order: &mut Vec<String>) {
    let items = client.items();
    let items = items.lock().unwrap();
    order.retain(|a| items.contains_key(a));
    let mut new: Vec<_> = items.keys().filter(|a| !order.contains(a)).cloned().collect();
    new.sort();
    order.extend(new);
}

/// Maps each registered item's bus name to its object path, which isn't always
/// /StatusNotifierItem (libappindicator uses /org/ayatana/NotificationItem/…).
async fn item_paths(conn: &zbus::Connection) -> HashMap<String, String> {
    let reply = conn
        .call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.kde.StatusNotifierWatcher", "RegisteredStatusNotifierItems"),
        )
        .await;
    let items = reply.ok().and_then(|r| {
        let value: zbus::zvariant::OwnedValue = r.body().deserialize().ok()?;
        Vec::<String>::try_from(value).ok()
    });
    items
        .unwrap_or_default()
        .iter()
        .filter_map(|a| a.find('/').map(|i| (a[..i].to_string(), a[i..].to_string())))
        .collect()
}

/// Rewrites requests that address the item object itself to "name/path".
fn with_paths(req: Request, paths: &HashMap<String, String>) -> Request {
    let full = |address: String| match paths.get(&address) {
        Some(path) => format!("{address}{path}"),
        None => address,
    };
    match req {
        Request::Activate(ActivateRequest::Default { address, x, y }) => {
            Request::Activate(ActivateRequest::Default { address: full(address), x, y })
        }
        Request::Activate(ActivateRequest::Secondary { address, x, y }) => {
            Request::Activate(ActivateRequest::Secondary { address: full(address), x, y })
        }
        Request::Scroll { address, delta } => Request::Scroll { address: full(address), delta },
        req => req,
    }
}

async fn handle(client: &Client, conn: &zbus::Connection, req: Request) -> anyhow::Result<()> {
    match req {
        Request::Activate(req) => client.activate(req).await?,
        Request::AboutToShow { address, menu_path, id } => {
            client.about_to_show_menuitem(address, menu_path, id).await?;
        }
        Request::Scroll { address, delta } => {
            let (dest, path) = address
                .split_once('/')
                .map_or_else(|| (address.as_str(), "/StatusNotifierItem".to_string()), |(d, p)| (d, format!("/{p}")));
            conn.call_method(
                Some(dest),
                path.as_str(),
                Some("org.kde.StatusNotifierItem"),
                "Scroll",
                &(delta, "vertical"),
            )
            .await?;
        }
    }
    Ok(())
}

fn snapshot(client: &Client, order: &[String]) -> Vec<TrayItem> {
    let items = client.items();
    let items = items.lock().unwrap();
    order
        .iter()
        .filter_map(|address| {
            let (item, menu) = items.get(address)?;
            Some(TrayItem {
                address: address.clone(),
                icon: icons(item),
                item_is_menu: item.item_is_menu,
                menu_path: item.menu.clone(),
                menu: menu.clone(),
            })
        })
        .collect()
}

fn icons(item: &StatusNotifierItem) -> Vec<IconSource> {
    let named = |name: &Option<String>| {
        name.as_ref()
            .filter(|n| !n.is_empty())
            .map(|name| IconSource::Named { name: name.clone(), theme_path: item.icon_theme_path.clone() })
    };
    let pixmaps = |p: &Option<Vec<_>>| p.as_ref().filter(|p| !p.is_empty()).map(|p| IconSource::pixmaps(p.clone()));
    let mut icons = Vec::new();
    if item.status == Status::NeedsAttention {
        icons.extend(named(&item.attention_icon_name));
        icons.extend(pixmaps(&item.attention_icon_pixmap));
    }
    icons.extend(named(&item.icon_name));
    icons.extend(pixmaps(&item.icon_pixmap));
    icons
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_use_registered_object_path() {
        let paths = HashMap::from([(":1.45".to_string(), "/org/ayatana/NotificationItem/app".to_string())]);
        let Request::Scroll { address, .. } =
            with_paths(Request::Scroll { address: ":1.45".into(), delta: 120 }, &paths)
        else {
            panic!()
        };
        assert_eq!(address, ":1.45/org/ayatana/NotificationItem/app");

        // Unknown items fall back to the default path inside the client.
        let req = Request::Activate(ActivateRequest::Default { address: ":1.9".into(), x: 0, y: 0 });
        let Request::Activate(ActivateRequest::Default { address, .. }) = with_paths(req, &paths) else { panic!() };
        assert_eq!(address, ":1.9");

        // Menu calls take the bare bus name.
        let req = Request::AboutToShow { address: ":1.45".into(), menu_path: "/Menu".into(), id: 0 };
        let Request::AboutToShow { address, .. } = with_paths(req, &paths) else { panic!() };
        assert_eq!(address, ":1.45");
    }
}
