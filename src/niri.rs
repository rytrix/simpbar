//! Workspace state from niri's IPC event stream.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::Duration;

use calloop::channel::Sender;
use serde::Deserialize;
use serde_json::Value;

use crate::Msg;

#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub id: u64,
    pub idx: u8,
    pub output: Option<String>,
    pub is_active: bool,
}

fn connect() -> std::io::Result<UnixStream> {
    let path = std::env::var_os("NIRI_SOCKET").ok_or_else(|| std::io::Error::other("NIRI_SOCKET is not set"))?;
    UnixStream::connect(path)
}

/// Follows the event stream, reconnecting if niri goes away.
pub fn spawn(tx: Sender<Msg>) {
    if std::env::var_os("NIRI_SOCKET").is_none() {
        log::warn!("NIRI_SOCKET is not set; workspaces disabled");
        return;
    }
    thread::spawn(move || {
        loop {
            if let Err(e) = stream(&tx) {
                log::warn!("niri event stream: {e}");
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn stream(tx: &Sender<Msg>) -> std::io::Result<()> {
    let mut socket = connect()?;
    socket.write_all(b"\"EventStream\"\n")?;
    let mut workspaces: Vec<Workspace> = Vec::new();
    for line in BufReader::new(socket).lines() {
        let Ok(event) = serde_json::from_str::<Value>(&line?) else { continue };
        if let Some(changed) = event.get("WorkspacesChanged") {
            let Ok(mut list) = serde_json::from_value::<Vec<Workspace>>(changed["workspaces"].clone()) else {
                continue;
            };
            list.sort_by_key(|w| w.idx);
            workspaces = list;
        } else if let Some(id) = event.pointer("/WorkspaceActivated/id").and_then(Value::as_u64) {
            // Activation is per output, so only that output's workspaces change.
            let Some(output) = workspaces.iter().find(|w| w.id == id).map(|w| w.output.clone()) else {
                continue;
            };
            for w in workspaces.iter_mut().filter(|w| w.output == output) {
                w.is_active = w.id == id;
            }
        } else {
            continue;
        }
        if tx.send(Msg::Workspaces(workspaces.clone())).is_err() {
            break;
        }
    }
    Ok(())
}

pub fn focus_workspace(id: u64) {
    thread::spawn(move || {
        let request = serde_json::json!({ "Action": { "FocusWorkspace": { "reference": { "Id": id } } } });
        let result = connect().and_then(|mut s| {
            s.write_all(format!("{request}\n").as_bytes())?;
            let mut reply = String::new();
            BufReader::new(s).read_line(&mut reply)?;
            Ok(())
        });
        if let Err(e) = result {
            log::warn!("failed to focus workspace {id}: {e}");
        }
    });
}
