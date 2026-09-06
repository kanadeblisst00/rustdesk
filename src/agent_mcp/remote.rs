use super::wire;
use hbb_common::{message_proto::Message, tokio};
use rustdesk_agent_mcp::automation::{validate_request, WIRE_VERSION};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

#[derive(Default)]
pub(crate) struct Worker {
    epoch: Arc<AtomicU64>,
    busy: Arc<AtomicBool>,
}

impl Worker {
    pub fn cancel_if(&self, cancel: bool) {
        if cancel {
            self.epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn begin(&self) -> Result<Lease, String> {
        if self.busy.swap(true, Ordering::SeqCst) {
            return Err("Remote UIA is busy".into());
        }
        Ok(Lease {
            epoch: self.epoch.clone(),
            generation: self.epoch.load(Ordering::SeqCst),
            busy: self.busy.clone(),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel_if(true);
    }
}

struct Lease {
    epoch: Arc<AtomicU64>,
    generation: u64,
    busy: Arc<AtomicBool>,
}
impl Lease {
    fn active(&self) -> bool {
        self.epoch.load(Ordering::SeqCst) == self.generation
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::SeqCst);
    }
}

type Reply = Option<crate::server::Sender>;

fn reply(id: &Value, result: Result<Value, String>, tx: &Reply) {
    let response = match result {
        Ok(result) => json!({"protocol":WIRE_VERSION,"id":id,"result":result}),
        Err(error) => json!({"protocol":WIRE_VERSION,"id":id,"error":error}),
    };
    let msg = match wire::encode(&response) {
        Ok(msg) => Ok(msg),
        Err(e) => {
            hbb_common::log::warn!("UIA response: {e}");
            wire::encode(
                &json!({"protocol":WIRE_VERSION,"id":id,"error":"UIA response exceeds size limit"}),
            )
        }
    };
    match msg {
        Ok(msg) => {
            if let Some(tx) = tx {
                if let Err(e) = tx.send((tokio::time::Instant::now(), Arc::new(msg))) {
                    hbb_common::log::debug!("UIA connection closed before reply: {e}");
                }
            }
        }
        Err(e) => hbb_common::log::error!("Unable to encode UIA error: {e}"),
    }
}

/// Authentication and connection scope are checked by the caller. Work must not
/// block its event loop, which also processes permission revocation and close.
pub(crate) fn dispatch(
    msg: &Message,
    keyboard: bool,
    remote_desktop: bool,
    worker: &Worker,
    tx: &Reply,
) -> bool {
    let Some(request) = wire::decode(msg) else {
        return false;
    };
    let request = match request {
        Ok(request) => request,
        Err(e) => {
            hbb_common::log::warn!("Rejected UIA message: {e}");
            return true;
        }
    };
    let permit = validate_request(&request).and_then(|_| {
        if !remote_desktop || !keyboard {
            return Err(
                "Remote UIA requires an authorized desktop connection with keyboard permission"
                    .into(),
            );
        }
        worker.begin()
    });
    let lease = match permit {
        Ok(lease) => lease,
        Err(e) => {
            reply(&request["id"], Err(e), tx);
            return true;
        }
    };
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let result = if lease.active() {
            platform_request(&request, || lease.active())
        } else {
            Err("Remote UIA was cancelled before execution".into())
        };
        reply(&request["id"], result, &tx);
    });
    true
}

#[cfg(windows)]
fn platform_request(request: &Value, active: impl Fn() -> bool) -> Result<Value, String> {
    crate::platform::agent_uia::request(request, active)
}

#[cfg(not(windows))]
fn platform_request(_: &Value, _: impl Fn() -> bool) -> Result<Value, String> {
    Err("UIA is supported only on Windows peers built with mcp".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn rejects_permissions_and_scopes_before_starting_any_provider() {
        let worker = Worker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let request =
            wire::encode(&json!({"protocol":WIRE_VERSION,"id":"test","operation":"tree"})).unwrap();
        for (keyboard, desktop) in [(false, false), (false, true), (true, false)] {
            assert!(dispatch(
                &request,
                keyboard,
                desktop,
                &worker,
                &Some(tx.clone())
            ));
            let (_, response) = rx.recv().await.unwrap();
            let response = wire::decode(&response).unwrap().unwrap();
            assert_eq!(response["id"], "test");
            assert!(response["error"]
                .as_str()
                .unwrap()
                .contains("keyboard permission"));
            assert!(!worker.busy.load(Ordering::SeqCst));
        }
        let invalid =
            wire::encode(&json!({"protocol":WIRE_VERSION,"id":"bad","operation":"shell"})).unwrap();
        assert!(dispatch(&invalid, true, true, &worker, &Some(tx)));
        let (_, response) = rx.recv().await.unwrap();
        assert!(wire::decode(&response).unwrap().unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("Unknown UIA operation"));
        assert!(!dispatch(&Message::new(), true, true, &worker, &None));
    }

    #[test]
    fn revocation_and_connection_drop_cancel_leases_without_reactivation() {
        let worker = Worker::default();
        let lease = worker.begin().unwrap();
        assert!(lease.active());
        assert!(worker.begin().is_err());
        worker.cancel_if(true);
        worker.cancel_if(false);
        assert!(!lease.active());
        drop(lease);
        let next = worker.begin().unwrap();
        assert!(next.active());
        drop(worker);
        assert!(!next.active());
    }
}
