mod environment;
mod output;
mod platform;
mod store;
#[cfg(test)]
mod tests;
mod worker;
mod workspace;
mod workspace_files;
mod wait;
pub(super) mod recovery;
#[cfg(windows)]
mod setup;

use super::*;
use hbb_common::{message_proto::Message, protobuf::UnknownValueRef};
use rustdesk_agent_mcp::process as model;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
pub(super) struct State {
    pending: Mutex<HashMap<String, Option<Result<Value, String>>>>,
}

pub(crate) fn is_message(msg: &Message) -> bool {
    msg.union.is_none()
        && msg
            .special_fields
            .unknown_fields()
            .get(model::WIRE_FIELD)
            .is_some()
}

fn encode(value: &Value) -> Result<Message, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > model::MAX_WIRE_BYTES {
        return Err("Process message exceeds 1 MiB".into());
    }
    let mut msg = Message::new();
    msg.special_fields
        .mut_unknown_fields()
        .add_length_delimited(model::WIRE_FIELD, bytes);
    Ok(msg)
}

fn decode(msg: &Message) -> Option<Result<Value, String>> {
    let field = msg.special_fields.unknown_fields().get(model::WIRE_FIELD)?;
    Some((|| {
        if msg.union.is_some() {
            return Err("Mixed process message".into());
        }
        let UnknownValueRef::LengthDelimited(bytes) = field else {
            return Err("Invalid process wire type".into());
        };
        if bytes.len() > model::MAX_WIRE_BYTES {
            return Err("Process message exceeds 1 MiB".into());
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if value["protocol"] != model::WIRE_VERSION
            || value["id"]
                .as_str()
                .map_or(true, |id| id.len() > 64 || id.is_empty())
        {
            return Err("Invalid process protocol or request ID".into());
        }
        Ok(value)
    })())
}

pub(crate) fn response(peer: &str, terminal: bool, msg: &Message) -> bool {
    let Some(response) = decode(msg) else {
        return false;
    };
    if !terminal || !enabled() {
        return true;
    }
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Process reply: {e}");
            return true;
        }
    };
    let Some(s) = crate::flutter::sessions::get_session_by_peer_id(
        peer.into(),
        hbb_common::rendezvous_proto::ConnType::TERMINAL,
    ) else {
        return true;
    };
    for id in s.agent_session_ids() {
        if let Some(state) = states().lock().unwrap().get(&id).cloned() {
            if let Some(id) = response["id"].as_str() {
                if let Some(p) = state.process.pending.lock().unwrap().get_mut(id) {
                    *p = Some(match response["error"].as_str() {
                        Some(e) => Err(e.into()),
                        None => response
                            .get("result")
                            .cloned()
                            .ok_or_else(|| "Missing process result".into()),
                    });
                }
            }
        }
    }
    true
}

pub(super) fn call(
    id: SessionID,
    s: &crate::flutter::FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    if !s.is_terminal() {
        return Err("An authenticated terminal session is required".into());
    }
    session::ready(s)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    {
        let mut pending = state.process.pending.lock().unwrap();
        if pending.len() >= 8 {
            return Err("Too many pending process requests on this session".into());
        }
        pending.insert(request_id.clone(), None);
    }
    struct Guard<'a>(&'a State, String);
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.0.pending.lock().unwrap().remove(&self.1);
        }
    }
    let _guard = Guard(&state.process, request_id.clone());
    let generation = state.automation.generation();
    let msg = encode(
        &json!({"protocol":model::WIRE_VERSION,"id":request_id,"operation":name,"arguments":args}),
    )?;
    session::send(s, crate::client::Data::Message(msg))?;
    let started = Instant::now();
    loop {
        let current = session::get(id)?;
        if !Arc::ptr_eq(&current, s) || generation != state.automation.generation() {
            return Err("Connection changed; query the same job_id after reconnect".into());
        }
        session::ready(s)?;
        if let Some(result) = state
            .process
            .pending
            .lock()
            .unwrap()
            .get_mut(&request_id)
            .and_then(Option::take)
        {
            return result.map(success);
        }
        if started.elapsed() >= Duration::from_secs(15) {
            return Err("Remote process request timed out. Peer may need an mcp build. For run_process, query/reuse the SAME job_id; never retry with a new ID automatically.".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

static REQUESTS: AtomicUsize = AtomicUsize::new(0);
static STORE_LOCK: Mutex<()> = Mutex::new(());
struct Permit;
impl Permit {
    fn acquire() -> Result<Self, String> {
        REQUESTS
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 8).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| "Remote process service is busy; retry the same job_id".into())
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        REQUESTS.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(crate) fn dispatch(
    msg: &Message,
    authorized_terminal: bool,
    token: Option<crate::terminal_service::UserToken>,
    tx: &Option<crate::server::Sender>,
) -> bool {
    let Some(request) = decode(msg) else {
        return false;
    };
    let request = match request {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Process request: {e}");
            return true;
        }
    };
    let preparation = (|| {
        if !authorized_terminal {
            return Err(
                "Process operations require authorized terminal access and its OS identity".into(),
            );
        }
        model::validate(
            request["operation"].as_str().unwrap_or(""),
            &request["arguments"],
        )?;
        Ok((Permit::acquire()?, platform::Identity::new(token)?))
    })();
    let (permit, identity) = match preparation {
        Ok(prepared) => prepared,
        Err(error) => {
            reply(&request["id"], Err(error), tx);
            return true;
        }
    };
    let tx = tx.clone();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = (|| {
            let _identity_guard = identity.enter()?;
            if request["operation"] == "get_environment" {
                return environment::observe(&identity, &request["arguments"]);
            }
            let operation = request["operation"].as_str().ok_or("Missing operation")?;
            let _lock = if matches!(operation, "run_process" | "remove_process")
                || rustdesk_agent_mcp::workspace::is_tool(operation)
            {
                Some(STORE_LOCK.lock().unwrap())
            } else {
                None
            };
            let store = store::Store::new(identity.root()?)?;
            if rustdesk_agent_mcp::workspace::is_tool(operation) {
                return workspace::call(&store, operation, &request["arguments"], |dir| {
                    identity.launch(dir)
                });
            }
            if operation == "run_process" {
                store.create(&request["arguments"], |dir| identity.launch(dir))
            } else {
                store.call(operation, &request["arguments"])
            }
        })();
        reply(&request["id"], result, &tx);
    });
    true
}

fn reply(id: &Value, result: Result<Value, String>, tx: &Option<crate::server::Sender>) {
    let response = match result {
        Ok(result) => {
            json!({"protocol":model::WIRE_VERSION,"id":id,"result":result})
        }
        Err(error) => json!({"protocol":model::WIRE_VERSION,"id":id,"error":error}),
    };
    match encode(&response) {
        Ok(msg) => {
            if let Some(tx) = tx {
                if let Err(e) = tx.send((tokio::time::Instant::now(), Arc::new(msg))) {
                    log::debug!("Process reply connection closed: {e}");
                }
            }
        }
        Err(e) => log::error!("Encode process reply: {e}"),
    }
}

pub(crate) fn worker_requested() -> bool {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--mcp-process-worker")) {
        return false;
    }
    let result = match (args.next(), args.next()) {
        (Some(path), None) => worker::run(std::path::Path::new(&path)),
        _ => Err("Invalid process worker arguments".into()),
    };
    if let Err(e) = result {
        log::error!("Process worker: {e}");
    }
    true
}
