mod desktop;
mod session;

use crate::flutter_ffi::SessionID;
use hbb_common::{config::LocalConfig, log, tokio};
use rustdesk_agent_mcp::{
    events::{ByteLog, Events},
    success, Backend, Server, ToolResult,
};
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

pub const ENABLE: &str = "enable-agent-mcp";
const TOKEN: &str = "agent-mcp-token";
const ADDRESS: &str = "127.0.0.1:59940";
static STARTED: Mutex<bool> = Mutex::new(false);
static STATE: OnceLock<Mutex<HashMap<SessionID, Arc<SessionState>>>> = OnceLock::new();
static STATUS: Mutex<String> = Mutex::new(String::new());

#[derive(Default)]
pub(super) struct SessionState {
    events: Events,
    operation: Mutex<()>,
    screenshot: Mutex<()>,
    pending_screenshot: Mutex<Option<desktop::PendingScreenshot>>,
    frame: Mutex<Option<desktop::Frame>>,
    requested_display: Mutex<Option<usize>>,
    clipboard: Mutex<Option<String>>,
    terminals: Mutex<HashMap<i32, (bool, ByteLog)>>,
    jobs: Mutex<HashSet<i32>>,
    overwrites: Mutex<HashMap<(i32, i32), bool>>,
    directory: Mutex<Option<Value>>,
    headless: bool,
}

fn states() -> &'static Mutex<HashMap<SessionID, Arc<SessionState>>> {
    STATE.get_or_init(Default::default)
}

fn enabled() -> bool {
    LocalConfig::get_option(ENABLE) == "Y"
}

pub fn local_option(key: &str) -> Option<String> {
    match key {
        "agent-mcp-supported" => Some("Y".into()),
        "agent-mcp-status" => Some(STATUS.lock().unwrap().clone()),
        _ => None,
    }
}

/// Application entry point, called only by the desktop's main event stream.
pub fn start() {
    let mut started = STARTED.lock().unwrap();
    if *started {
        return;
    }
    match std::thread::Builder::new()
        .name("rustdesk-agent-mcp".into())
        .spawn(|| {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    *STATUS.lock().unwrap() = format!("MCP runtime: {e}");
                    *STARTED.lock().unwrap() = false;
                    return;
                }
            };
            runtime.block_on(async {
                loop {
                    if !enabled() {
                        *STATUS.lock().unwrap() = "Stopped".into();
                        cleanup();
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        continue;
                    }
                    if LocalConfig::get_option(TOKEN).len() < 32 {
                        LocalConfig::set_option(
                            TOKEN.into(),
                            format!(
                                "{}{}",
                                uuid::Uuid::new_v4().simple(),
                                uuid::Uuid::new_v4().simple()
                            ),
                        );
                    }
                    match tokio::net::TcpListener::bind(ADDRESS).await {
                        Ok(listener) => {
                            *STATUS.lock().unwrap() = format!("Listening on http://{ADDRESS}/mcp");
                            let server = Arc::new(Server::new(Arc::new(DesktopBackend)));
                            if let Err(e) = rustdesk_agent_mcp::http::serve(listener, server).await
                            {
                                log::error!("Agent MCP server: {e}");
                                *STATUS.lock().unwrap() = format!("MCP server: {e}");
                            }
                        }
                        Err(e) => {
                            *STATUS.lock().unwrap() = format!("MCP listener: {e}");
                            log::error!("Agent MCP listener: {e}");
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            });
        }) {
        Ok(_) => *started = true,
        Err(e) => {
            *STATUS.lock().unwrap() = format!("MCP thread: {e}");
            log::error!("Agent MCP thread: {e}");
        }
    }
}

fn cleanup() {
    let previous = std::mem::take(&mut *states().lock().unwrap());
    for (id, state) in previous {
        if state.headless {
            crate::flutter_ffi::session_close(id);
        }
    }
}

struct DesktopBackend;

impl Backend for DesktopBackend {
    fn token(&self) -> Option<String> {
        enabled().then(|| LocalConfig::get_option(TOKEN))
    }

    fn call(&self, name: &str, args: &Map<String, Value>) -> ToolResult {
        ensure_enabled()?;
        if name == "get_capabilities" {
            return Ok(success(
                json!({"transport":["streamable-http","stdio-proxy"],
                "desktop":true,"terminal":true,"files":true,"headless":true,
                "clipboard":"remote received text","ocr":false,"accessibility_tree":false,
                "camera":false,"host_skills":false,"max_sessions":16,"max_events":256,
                "max_terminal_bytes":1048576,"max_frame_bytes":67108864,
                "device_allowlist":LocalConfig::get_option("agent-mcp-devices"),
                "read_only":LocalConfig::get_option("agent-mcp-read-only")=="Y"}),
            ));
        }
        if name == "list_connections" {
            return Ok(success(json!({"connections":session::list()})));
        }
        if name == "connect_device" {
            writable()?;
            return session::connect(args);
        }
        let id =
            SessionID::parse_str(string(args, "session")?).map_err(|_| "Invalid session UUID")?;
        let s = session::get(id)?;
        let state = track(id, false)?;
        let read_only = rustdesk_agent_mcp::catalog::tools()
            .iter()
            .find(|t| t["name"] == name)
            .is_some_and(|t| t["annotations"]["readOnlyHint"] == true);
        if !read_only {
            writable()?;
        }
        if matches!(name, "get_recent_events" | "wait_for_event") {
            let wait = if name == "wait_for_event" {
                number(args, "timeout_ms", 10000)
            } else {
                0
            };
            let result = state.events.read(
                number(args, "cursor", 0) as u64,
                args.get("event_type").and_then(Value::as_str),
                Duration::from_millis(wait as u64),
                || enabled() && session::get(id).is_ok(),
            )?;
            return Ok(success(result));
        }
        if name == "screenshot" {
            return desktop::screenshot(id, &s, &state, args);
        }
        if name == "terminal_output" {
            return session::terminal_output(&state, args);
        }
        if matches!(name, "get_connection_info" | "list_displays") {
            return Ok(success(session::info(id, &s)));
        }
        let _operation = state
            .operation
            .try_lock()
            .map_err(|_| "Another action is in progress on this session")?;
        ensure_enabled()?;
        if name == "execute_actions" {
            let actions = args
                .get("actions")
                .and_then(Value::as_array)
                .ok_or("Missing actions")?;
            let catalog = rustdesk_agent_mcp::catalog::tools();
            let mut validated = Vec::new();
            for action in actions {
                let name = action["name"].as_str().ok_or("Invalid action")?;
                let mut arguments = action["arguments"]
                    .as_object()
                    .ok_or("Invalid action arguments")?
                    .clone();
                if arguments.contains_key("session") {
                    return Err("Batch actions cannot override session".into());
                }
                arguments.insert("session".into(), json!(id.to_string()));
                let tool = catalog
                    .iter()
                    .find(|t| t["name"] == name)
                    .ok_or("Unknown action")?;
                rustdesk_agent_mcp::catalog::validate(
                    &tool["inputSchema"],
                    &json!(arguments),
                    "action",
                )?;
                validated.push((name, arguments));
            }
            let mut completed = Vec::new();
            for (index, (name, arguments)) in validated.iter().enumerate() {
                let result = writable()
                    .and_then(|_| session::get(id))
                    .and_then(|s| desktop::input(&s, &state, name, arguments));
                match result {
                    Ok(v) => completed.push(v),
                    Err(e) => {
                        return Ok(
                            json!({"content":[{"type":"text","text":format!("Action {index} failed: {e}")}],
                        "structuredContent":{"completed":completed,"failed_index":index},"isError":true}),
                        )
                    }
                }
            }
            return Ok(success(json!({"queued_actions":completed.len()})));
        }
        if name.starts_with("mouse_") || name.starts_with("keyboard_") {
            return desktop::input(&s, &state, name, args);
        }
        session::call(id, &s, &state, name, args)
    }
}

fn ensure_enabled() -> Result<(), String> {
    if enabled() {
        Ok(())
    } else {
        Err("MCP is disabled".into())
    }
}
fn writable() -> Result<(), String> {
    ensure_enabled()?;
    if LocalConfig::get_option("agent-mcp-read-only") == "Y" {
        Err("MCP is in read-only mode".into())
    } else {
        Ok(())
    }
}
fn string<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing {key}"))
}
fn number(args: &Map<String, Value>, key: &str, default: i64) -> i64 {
    args.get(key).and_then(Value::as_i64).unwrap_or(default)
}
fn flag(args: &Map<String, Value>, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn prune() {
    let live: HashSet<_> = crate::flutter::sessions::get_sessions()
        .iter()
        .flat_map(|s| s.agent_session_ids())
        .collect();
    states().lock().unwrap().retain(|id, _| live.contains(id));
}

fn track(id: SessionID, headless: bool) -> Result<Arc<SessionState>, String> {
    prune();
    let mut states = states().lock().unwrap();
    if let Some(s) = states.get(&id) {
        return Ok(s.clone());
    }
    if states.len() >= 16 {
        return Err("MCP session limit reached; disconnect a session".into());
    }
    let state = Arc::new(SessionState {
        headless,
        ..Default::default()
    });
    states.insert(id, state.clone());
    Ok(state)
}

pub fn event(id: SessionID, name: &str, data: &impl serde::Serialize) {
    if !enabled() {
        return;
    }
    let state = states().lock().unwrap().get(&id).cloned();
    let Some(state) = state else {
        return;
    };
    if name == "connection_ready" {
        // A reconnect keeps the UI UUID, but cached observations belong to the old transport.
        state.frame.lock().unwrap().take();
        state.requested_display.lock().unwrap().take();
        state.pending_screenshot.lock().unwrap().take();
        state.clipboard.lock().unwrap().take();
        state.directory.lock().unwrap().take();
        state.terminals.lock().unwrap().clear();
        state.jobs.lock().unwrap().clear();
        state.overwrites.lock().unwrap().clear();
    }
    let data = match serde_json::to_value(data) {
        Ok(data) => data,
        Err(e) => {
            log::warn!("MCP event: {e}");
            return;
        }
    };
    if name == "file_dir" && data.get("is_local").and_then(Value::as_str) == Some("false") {
        if let Some(raw) = data.get("value").and_then(Value::as_str) {
            let snapshot = if raw.len() > 4 * 1024 * 1024 {
                json!({"error":"Directory listing exceeds 4 MiB limit"})
            } else {
                serde_json::from_str(raw)
                    .unwrap_or_else(|_| json!({"error":"Invalid directory listing"}))
            };
            *state.directory.lock().unwrap() = Some(snapshot);
        }
    }
    if name == "override_file_confirm" {
        let parse = |key| {
            data.get(key)
                .and_then(Value::as_str)
                .and_then(|v| v.parse::<i32>().ok())
        };
        if let (Some(job), Some(file)) = (parse("id"), parse("file_num")) {
            if state.jobs.lock().unwrap().contains(&job) {
                let mut pending = state.overwrites.lock().unwrap();
                if pending.len() < 256 {
                    pending.insert((job, file), data["is_upload"] == "true");
                }
            }
        }
    }
    if name == "terminal_response" {
        use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
        let Some(id) = data
            .get("terminal_id")
            .and_then(Value::as_i64)
            .map(|v| v as i32)
        else {
            return;
        };
        let mut terminals = state.terminals.lock().unwrap();
        if terminals.len() >= 16 && !terminals.contains_key(&id) {
            return;
        }
        let entry = terminals.entry(id).or_default();
        match data.get("type").and_then(Value::as_str) {
            Some("opened") => entry.0 = data.get("success") == Some(&Value::Bool(true)),
            Some("closed" | "error") => entry.0 = false,
            Some("data") => {
                if let Some(encoded) = data.get("data").and_then(Value::as_str) {
                    match STANDARD.decode(encoded) {
                        Ok(bytes) => entry.1.append(&bytes),
                        Err(e) => log::warn!("MCP terminal frame: {e}"),
                    }
                }
                // Large terminal payloads live only in the byte log, not duplicated in the event ring.
                state
                    .events
                    .push(name, json!({"type":"data","terminal_id":id}));
                return;
            }
            _ => {}
        }
    }
    if data.to_string().len() <= 65536 {
        state.events.push(name, data.clone());
    } else {
        state.events.push(
            name,
            json!({"omitted":true,"reason":"event exceeds 64 KiB"}),
        );
    }
}

pub fn clipboard(peer_id: &str, clipboard: &hbb_common::message_proto::Clipboard) {
    use hbb_common::message_proto::ClipboardFormat;
    if !enabled() || clipboard.format.enum_value_or_default() != ClipboardFormat::Text {
        return;
    }
    let data = if clipboard.compress {
        hbb_common::compress::decompress(&clipboard.content)
    } else {
        clipboard.content.to_vec()
    };
    if data.len() > 262144 {
        return;
    }
    let Ok(text) = String::from_utf8(data) else {
        return;
    };
    let Some(s) = crate::flutter::sessions::get_session_by_peer_id(
        peer_id.into(),
        hbb_common::rendezvous_proto::ConnType::DEFAULT_CONN,
    ) else {
        return;
    };
    for id in s.agent_session_ids() {
        if let Some(state) = states().lock().unwrap().get(&id).cloned() {
            *state.clipboard.lock().unwrap() = Some(text.clone());
            state
                .events
                .push("clipboard_changed", json!({"bytes":text.len()}));
        }
    }
}

pub fn frame(ids: impl FnOnce() -> Vec<SessionID>, display: usize, rgba: &scrap::ImageRgb) {
    if !enabled() {
        return;
    }
    for id in ids() {
        let state = states().lock().unwrap().get(&id).cloned();
        let Some(state) = state else {
            continue;
        };
        if *state.requested_display.lock().unwrap() != Some(display) {
            continue;
        }
        let mut frame = state.frame.lock().unwrap();
        if frame
            .as_ref()
            .is_some_and(|f| f.captured.elapsed() < Duration::from_millis(150))
        {
            continue;
        }
        let sequence = frame.as_ref().map(|f| f.sequence + 1).unwrap_or(1);
        match desktop::Frame::from_rgb(display, sequence, rgba) {
            Ok(new) => *frame = Some(new),
            Err(e) => log::debug!("MCP frame unavailable: {e}"),
        }
    }
}

pub fn screenshot_response(
    peer_id: &str,
    response: &hbb_common::message_proto::ScreenshotResponse,
) -> bool {
    if !response.sid.starts_with("agent-mcp:") {
        return false;
    }
    if !enabled() {
        return true;
    }
    let Some(s) = crate::flutter::sessions::get_session_by_peer_id(
        peer_id.into(),
        hbb_common::rendezvous_proto::ConnType::DEFAULT_CONN,
    ) else {
        return true;
    };
    for id in s.agent_session_ids() {
        let state = states().lock().unwrap().get(&id).cloned();
        let Some(state) = state else {
            continue;
        };
        let mut pending = state.pending_screenshot.lock().unwrap();
        if let Some(pending) = pending.as_mut().filter(|p| p.id == response.sid) {
            pending.result = Some(if !response.msg.is_empty() {
                Err(response.msg.clone())
            } else if response.data.len() > rustdesk_agent_mcp::pixels::MAX_FRAME_BYTES {
                Err("Remote screenshot exceeds size limit".into())
            } else {
                Ok(response.data.to_vec())
            });
        }
    }
    true
}
