use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Mutex,
};

#[derive(Default)]
pub(crate) struct State(Mutex<Events>);

#[derive(Default)]
struct Events {
    connection_ready: Option<String>,
    permissions: BTreeMap<&'static str, String>,
    peer_info: Option<String>,
}

impl State {
    // Callers hold the UI handler lock so attaching a stream cannot race live delivery.
    pub(crate) fn remember(&self, name: &str, data: &HashMap<&str, Value>, event: &str) {
        if !matches!(name, "connection_ready" | "permission" | "peer_info") {
            return;
        }
        let mut events = self.0.lock().unwrap();
        match name {
            "connection_ready" => {
                *events = Events::default();
                events.connection_ready = Some(event.into());
            }
            "permission" => {
                for permission in [
                    "keyboard",
                    "clipboard",
                    "file",
                    "audio",
                    "restart",
                    "recording",
                    "block_input",
                    "privacy_mode",
                ] {
                    if data.contains_key(permission) {
                        events.permissions.insert(permission, event.into());
                    }
                }
            }
            "peer_info" => events.peer_info = Some(event.into()),
            _ => {}
        }
    }

    pub(crate) fn events(&self) -> Vec<String> {
        let events = self.0.lock().unwrap();
        let Some(peer_info) = &events.peer_info else {
            return Vec::new();
        };
        events
            .connection_ready
            .iter()
            .chain(events.permissions.values())
            .chain(std::iter::once(peer_info))
            .cloned()
            .collect()
    }
}
