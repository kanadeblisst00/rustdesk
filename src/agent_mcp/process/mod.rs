mod diagnostics;
mod worker_record;
mod capabilities;
mod reconcile;
mod artifact;
mod environment;
mod output;
mod head_tail;
mod download;
mod preflight;
mod read_file;
mod presentation;
mod platform;
mod store;
mod retention;
#[cfg(test)]
mod tests;
mod worker;
mod workspace;
mod workspace_location;
mod workspace_files;
mod wait;
pub(super) mod recovery;
#[cfg(windows)]
mod setup;

use super::*;
use hbb_common::{message_proto::Message, protobuf::UnknownValueRef};
use rustdesk_agent_mcp::process as model;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) fn plain_terminal_output(text: &str) -> String {
    presentation::plain(text)
}

#[derive(Default)]
struct Pending {
    received: bool,
    result: Option<Result<Value, String>>,
}

impl Pending {
    fn accept(&mut self, response: &Value) {
        self.received = true;
        if response["received"] == true && response.get("result").is_none() && response.get("error").is_none() {
            return;
        }
        self.result = Some(match response["error"].as_str() {
            Some(error) => Err(error.into()),
            None => response.get("result").cloned().ok_or_else(|| "Missing process result".into()),
        });
    }
}

#[derive(Default)]
pub(super) struct State {
    pending: Mutex<HashMap<String, Pending>>,
    capabilities: Mutex<Option<(u64, Value)>>,
}

impl State {
    pub(super) fn negotiated(&self, generation: u64) -> bool {
        self.capabilities.lock().unwrap().as_ref().is_some_and(|(observed,_)| *observed == generation)
    }
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
                    p.accept(&response);
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
    if name != "get_process_capabilities" {
        let peer = state.process.capabilities.lock().unwrap().clone();
        if let Some((generation, peer)) = peer {
            let check = if generation != state.automation.generation() {
                Err("Connection changed; negotiate get_capabilities(session) again before sending operations".into())
            } else {
                rustdesk_agent_mcp::process_rpc::validate_peer(&peer, name, args)
            };
            check.map_err(|error: String| rustdesk_agent_mcp::process_rpc::failure(name,
                &uuid::Uuid::new_v4().to_string(), args, "not_sent", "remote_capability_mismatch", &error))?;
        }
    }
    request(id, s, state, name, args, None)
}

pub(super) fn negotiate(id: SessionID, s: &crate::flutter::FlutterSession,
    state: &SessionState, args: &Map<String, Value>) -> ToolResult {
    negotiate_with_timeout(id, s, state, args, None)
}

pub(super) fn negotiate_with_timeout(id: SessionID, s: &crate::flutter::FlutterSession,
    state: &SessionState, args: &Map<String, Value>, timeout: Option<Duration>) -> ToolResult {
    let generation = state.automation.generation();
    let query = json!({"session":args.get("session")});
    let response = request(id, s, state, "get_process_capabilities", query.as_object().ok_or("Invalid capability query")?, timeout);
    let peer = match response {
        Ok(response) => response["structuredContent"].clone(),
        Err(error) if error == "Unknown process operation" => {
            return Ok(success(json!({"session":id.to_string(),"device_id":s.lc.read().unwrap().get_id(),"negotiated":false,
                "reason":"legacy_peer","operations":[],"recovery":"Update the remote MCP build before relying on its operations or parameter limits"})));
        }
        Err(error) => return Err(error),
    };
    if peer["protocol"] != model::WIRE_VERSION || peer["negotiated"] != true || !peer["operations"].is_array() {
        return Err("Invalid remote capability response".into());
    }
    if generation != state.automation.generation() {
        return Err("Connection changed during capability negotiation; query again".into());
    }
    *state.process.capabilities.lock().unwrap() = Some((generation, peer.clone()));
    Ok(success(json!({"session":id.to_string(),"negotiated":true,
        "device_id":s.lc.read().unwrap().get_id(),"connection_generation":generation,"observed_at_ms":store::now(),
        "controller":capabilities::observe(),"peer":peer})))
}

fn request(id: SessionID, s: &crate::flutter::FlutterSession, state: &SessionState,
    name: &str, args: &Map<String, Value>, timeout: Option<Duration>) -> ToolResult {
    let request_id = uuid::Uuid::new_v4().to_string();
    let failure = |delivery: &str, kind: &str, message: &str| {
        rustdesk_agent_mcp::process_rpc::failure(name, &request_id, args, delivery, kind, message)
    };
    let not_ready = |delivery: &str, message: &str| failure(delivery,
        if message == "Session is not authenticated yet" { "session_auth_required" } else { "session_disconnected" }, message);
    if !s.is_terminal() {
        return Err(failure("not_sent", "session_auth_required", "An authenticated terminal session is required"));
    }
    session::ready(s).map_err(|e| not_ready("not_sent", &e))?;
    {
        let mut pending = state.process.pending.lock().unwrap();
        if pending.len() >= 8 {
            return Err(failure("not_sent", "local_request_capacity", "Too many pending process requests on this session"));
        }
        pending.insert(request_id.clone(), Pending::default());
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
        &json!({"protocol":model::WIRE_VERSION,"id":request_id,"operation":name,"arguments":args,"receipts":true}),
    ).map_err(|e| failure("not_sent", "invalid_request", &e))?;
    session::send(s, crate::client::Data::Message(msg)).map_err(|e| failure("not_sent", "session_disconnected", &e))?;
    let started = Instant::now();
    let request_timeout = timeout.unwrap_or_else(|| rustdesk_agent_mcp::process_rpc::timeout(name, args));
    loop {
        let received = state.process.pending.lock().unwrap().get(&request_id).is_some_and(|p| p.received);
        let delivery = if received { "remote_received" } else { "queued" };
        let current = session::get(id).map_err(|e| failure(delivery, "session_disconnected", &e))?;
        if !Arc::ptr_eq(&current, s) || generation != state.automation.generation() {
            return Err(failure(delivery, "session_disconnected", "Connection changed; query the same job_id after reconnect"));
        }
        session::ready(s).map_err(|e| not_ready(delivery, &e))?;
        if let Some(result) = state
            .process
            .pending
            .lock()
            .unwrap()
            .get_mut(&request_id)
            .and_then(|p| p.result.take())
        {
            return match result {
                Ok(result) => Ok(success(result)),
                Err(error) if name == "get_process_capabilities" && error == "Unknown process operation" => Err(error),
                Err(error) => Err(failure("remote_received", "remote_operation_error", &error)),
            };
        }
        if started.elapsed() >= request_timeout {
            return Err(failure(delivery, "remote_request_timeout", &format!("Remote process request timed out: {name} ({request_id}), deadline {} ms", request_timeout.as_millis())));
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
    if request["receipts"] == true {
        send_reply(&json!({"protocol":model::WIRE_VERSION,"id":request["id"],"received":true}), &tx);
    }
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = (|| {
            let _identity_guard = identity.enter()?;
            if request["operation"] == "get_process_capabilities" {
                return Ok(capabilities::observe());
            }
            if request["operation"] == "get_environment" {
                return environment::observe(&identity, &request["arguments"]);
            }
            if request["operation"] == "read_remote_file" {
                return read_file::call(&request["arguments"]);
            }
            let operation = request["operation"].as_str().ok_or("Missing operation")?;
            let _lock = if matches!(operation, "run_process" | "remove_process" | "remove_processes" | "reconcile_process" | "probe_environment")
                || rustdesk_agent_mcp::workspace::is_tool(operation)
            {
                Some(STORE_LOCK.lock().unwrap())
            } else {
                None
            };
            let store = store::Store::new(identity.root()?)?;
            if operation == "probe_environment" {
                return environment::probe(&identity,&store,&request["arguments"]);
            }
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
    send_reply(&response, tx);
}

fn send_reply(response: &Value, tx: &Option<crate::server::Sender>) {
    match encode(response) {
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
        std::process::exit(1);
    }
    true
}
