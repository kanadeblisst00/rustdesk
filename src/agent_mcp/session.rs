use super::*;
use crate::{
    client::Data,
    flutter::{sessions, FlutterSession},
    flutter_ffi,
};
use hbb_common::{
    base64::{engine::general_purpose::STANDARD, Engine as _},
    message_proto::*,
    rendezvous_proto::ConnType,
};
use std::sync::atomic::{AtomicI32, Ordering};

static NEXT_JOB: AtomicI32 = AtomicI32::new(1_000_000_000);

pub(super) fn allowed(peer: &str) -> bool {
    let policy = LocalConfig::get_option("agent-mcp-devices");
    policy.trim().is_empty() || policy.split(',').any(|id| id.trim() == peer)
}

pub(super) fn get(id: SessionID) -> Result<FlutterSession, String> {
    ensure_enabled()?;
    let s = sessions::get_session_by_session_id(&id).ok_or("Unknown or closed session")?;
    if !allowed(s.lc.read().unwrap().get_id()) {
        return Err("Device is not in the MCP allowlist".into());
    }
    if !s.is_default() && !s.is_terminal() && !s.is_file_transfer() {
        return Err("Unsupported session type".into());
    }
    Ok(s)
}

pub(super) fn list() -> Vec<Value> {
    sessions::get_sessions()
        .iter()
        .filter(|s| {
            allowed(s.lc.read().unwrap().get_id())
                && (s.is_default() || s.is_terminal() || s.is_file_transfer())
        })
        .flat_map(|s| {
            s.agent_session_ids()
                .into_iter()
                .map(|id| info(id, s))
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(super) fn info(id: SessionID, s: &FlutterSession) -> Value {
    let lc = s.lc.read().unwrap();
    let connected = lc.peer_info.is_some()
        && s.sender
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| !tx.is_closed());
    let displays: Vec<_> = lc
        .peer_info
        .as_ref()
        .map(|p| {
            p.displays.iter().enumerate().map(|(index,d)|json!({
        "display":index,"x":d.x,"y":d.y,"width":d.width,"height":d.height,"scale":d.scale
    })).collect()
        })
        .unwrap_or_default();
    json!({"session":id.to_string(),"device_id":lc.get_id(),
        "kind":if lc.conn_type==ConnType::TERMINAL{"terminal"}else if lc.conn_type==ConnType::FILE_TRANSFER{"files"}else{"desktop"},
        "connected":connected,"needs_password":!connected && lc.agent_auth.needs_password(),
        "authentication":if connected {"authenticated"} else {lc.agent_auth.name()},
        "displays":displays,"current_display":lc.peer_info.as_ref().map(|p|p.current_display),
        "platform":lc.peer_info.as_ref().map(|p|p.platform.clone()),
        "permissions":{"keyboard":*s.server_keyboard_enabled.read().unwrap(),
        "clipboard":*s.server_clipboard_enabled.read().unwrap(),"files":*s.server_file_transfer_enabled.read().unwrap(),
        "view_only":lc.view_only.v}})
}

pub(super) fn connect(args: &Map<String, Value>) -> ToolResult {
    let queue = super::connection_queue::get(string(args, "device_id")?, args.get("kind").and_then(Value::as_str).unwrap_or("desktop"));
    let _connecting = queue.acquire(Duration::from_secs(30), writable)?;
    let peer = string(args, "device_id")?;
    if peer.starts_with('-')
        || peer
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "?#=&/\\".contains(c))
    {
        return Err("Invalid device ID".into());
    }
    if !allowed(peer) {
        return Err("Device is not in the MCP allowlist".into());
    }
    let kind = args
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("desktop");
    let conn = match kind {
        "terminal" => ConnType::TERMINAL,
        "files" => ConnType::FILE_TRANSFER,
        _ => ConnType::DEFAULT_CONN,
    };
    if let Some(s) = sessions::get_session_by_peer_id(peer.into(), conn) {
        if let Some(id) = s.agent_session_ids().first().copied() {
            track(id, false)?;
            return super::auth::wait_for_session(id, &s, args);
        }
    }
    prune();
    if states().lock().unwrap().len() >= 16 {
        return Err("MCP session limit reached".into());
    }
    let headless = flag(args, "headless");
    if headless {
        let id = SessionID::new_v4();
        let s = crate::flutter::session_add(
            &id,
            peer,
            kind == "files",
            false,
            false,
            false,
            kind == "terminal",
            "",
            flag(args, "force_relay"),
            String::new(),
            false,
            None,
        )
        .map_err(|e| e.to_string())?;
        if let Err(e) = track(id, true) {
            flutter_ffi::session_close(id);
            return Err(e);
        }
        let registered =
            sessions::get_session_by_session_id(&id).ok_or("Session was closed while opening")?;
        if !Arc::ptr_eq(&s, &registered) {
            // A user window may have connected to this peer concurrently with session_add.
            return super::auth::wait_for_session(id, &registered, args);
        }
        let handler = (*s).clone();
        // RustDesk's existing session loop owns its runtime on this dedicated OS thread.
        if let Err(e) = std::thread::Builder::new()
            .name("rustdesk-agent-session".into())
            .spawn(move || {
                let round = handler.connection_round_state.lock().unwrap().new_round();
                crate::ui_session_interface::io_loop(handler, round);
            })
        {
            flutter_ffi::session_close(id);
            states().lock().unwrap().remove(&id);
            return Err(format!("Unable to start session: {e}"));
        }
    } else {
        open_window(peer, kind, flag(args, "force_relay"))?;
    }
    let deadline = Instant::now() + Duration::from_millis(number(args, "timeout_ms", 12000) as u64);
    let mut found = None;
    while Instant::now() < deadline {
        writable()?;
        if !allowed(peer) {
            return Err("Device access revoked".into());
        }
        if let Some(s) = sessions::get_session_by_peer_id(peer.into(), conn) {
            if let Some(id) = s.agent_session_ids().first().copied() {
                track(id, headless)?;
                let details = info(id, &s);
                if super::auth::settled(&details) {
                    return Ok(success(details));
                }
                found = Some(details);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    found
        .map(success)
        .ok_or_else(|| "RustDesk did not create a session before timeout".into())
}

fn window_event(prefix: &str, peer: &str, kind: &str, force_relay: bool) -> String {
    let authority = match kind {
        "terminal" => "terminal",
        "files" => "file-transfer",
        _ => "connect",
    };
    let query = if force_relay { "?relay=true" } else { "" };
    json!({"name":"on_url_scheme_received",
        "url":format!("{prefix}{authority}/{peer}{query}")})
    .to_string()
}

fn open_window(peer: &str, kind: &str, force_relay: bool) -> Result<(), String> {
    // The MCP listener must create the window in its own process, not an IPC-selected instance.
    match crate::flutter::push_global_event(
        crate::flutter::APP_TYPE_MAIN,
        window_event(&crate::get_uri_prefix(), peer, kind, force_relay),
    ) {
        Some(true) => Ok(()),
        _ => Err("RustDesk main window is unavailable; reopen it or use headless:true".into()),
    }
}

pub(super) fn send(s: &FlutterSession, data: Data) -> Result<(), String> {
    ensure_enabled()?;
    let sender = s
        .sender
        .read()
        .unwrap()
        .clone()
        .ok_or("Session transport is not ready")?;
    sender
        .send(data)
        .map_err(|_| "Session transport is closed".into())
}

pub(super) fn ready(s: &FlutterSession) -> Result<(), String> {
    if s.lc.read().unwrap().peer_info.is_none() {
        return Err("Session is not authenticated yet".into());
    }
    if s.sender
        .read()
        .unwrap()
        .as_ref()
        .map_or(true, |s| s.is_closed())
    {
        return Err("Session transport is closed".into());
    }
    Ok(())
}

pub(super) fn call(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    let cursor = state.events.cursor();
    match name {
        "disconnect_device" => {
            // Notify the owning UI before session_close removes its event sink.
            s.push_event_to(
                "agent_close_session",
                &[("session", id.to_string())],
                &[&id],
            );
            flutter_ffi::session_close(id);
            states().lock().unwrap().remove(&id);
            return Ok(success(json!({"disconnected":true})));
        }
        "input_password" => {
            if !s.lc.read().unwrap().agent_has_login_challenge() {
                return Err("No login challenge received; wait for connection events".into());
            }
            send(
                s,
                Data::Login((
                    args.get("os_username")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    args.get("os_password")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    string(args, "password")?.into(),
                    false,
                )),
            )?;
        }
        "submit_2fa" => {
            let mut msg = Message::new();
            msg.set_auth_2fa(Auth2FA {
                code: string(args, "code")?.into(),
                ..Default::default()
            });
            send(s, Data::Message(msg))?;
        }
        _ => {
            ready(s)?;
            if name == "select_display" {
                desktop::select(s, state, number(args, "display", 0) as usize)?;
            } else if name.starts_with("clipboard_") {
                if !s.is_default() || !s.is_text_clipboard_required() {
                    return Err("Remote clipboard permission is unavailable".into());
                }
                if name == "clipboard_get" {
                    return state
                        .clipboard
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|text| success(json!({"text":text})))
                        .ok_or_else(|| {
                            "No remote clipboard text received for this session".into()
                        });
                }
                let mut msg = Message::new();
                msg.set_clipboard(Clipboard {
                    content: bytes::Bytes::from(string(args, "text")?.to_owned()),
                    format: ClipboardFormat::Text.into(),
                    ..Default::default()
                });
                send(s, Data::Message(msg))?;
            } else if name.starts_with("terminal_") {
                terminal(s, state, name, args)?;
            } else if name.starts_with("file_") {
                return file(s, state, name, args);
            } else {
                return Err("Unsupported tool".into());
            }
        }
    }
    state.events.push("tool_queued", json!({"tool":name}));
    Ok(success(json!({"queued":true,"after_cursor":cursor})))
}

fn terminal(
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> Result<(), String> {
    if !s.is_terminal() {
        return Err("A terminal session is required".into());
    }
    let id = number(args, "terminal_id", 1) as i32;
    if name == "terminal_open" {
        let mut terminals = state.terminals.lock().unwrap();
        if terminals.contains_key(&id) {
            return Err("Terminal ID already used; choose a new ID".into());
        }
        if terminals.len() >= 16 {
            return Err("Maximum 16 terminals per session".into());
        }
        terminals.insert(id, Default::default());
    } else if !state
        .terminals
        .lock()
        .unwrap()
        .get(&id)
        .is_some_and(|(ready, _)| *ready)
    {
        return Err("Terminal is not ready; wait for a successful opened event".into());
    }
    let mut action = TerminalAction::new();
    let rows = number(args, "rows", 24) as u32;
    let cols = number(args, "cols", 80) as u32;
    match name {
        "terminal_open" => action.set_open(OpenTerminal {
            terminal_id: id,
            rows,
            cols,
            ..Default::default()
        }),
        "terminal_input" => action.set_data(TerminalData {
            terminal_id: id,
            data: bytes::Bytes::from(string(args, "text")?.to_owned()),
            ..Default::default()
        }),
        "terminal_resize" => action.set_resize(ResizeTerminal {
            terminal_id: id,
            rows,
            cols,
            ..Default::default()
        }),
        "terminal_close" => action.set_close(CloseTerminal {
            terminal_id: id,
            ..Default::default()
        }),
        _ => return Err("Unknown terminal operation".into()),
    }
    let mut msg = Message::new();
    msg.set_terminal_action(action);
    send(s, Data::Message(msg))
}

pub(super) fn terminal_output(state: &SessionState, args: &Map<String, Value>) -> ToolResult {
    let terminals = state.terminals.lock().unwrap();
    let (ready, output) = terminals
        .get(&(number(args, "terminal_id", 1) as i32))
        .ok_or("Unknown terminal")?;
    let (bytes, next, truncated) = output.read(
        number(args, "cursor", 0) as u64,
        number(args, "limit", 65536) as usize,
    )?;
    Ok(success(
        json!({"ready":ready,"text":String::from_utf8_lossy(&bytes),
        "data_base64":STANDARD.encode(&bytes),"next_cursor":next,"truncated":truncated}),
    ))
}

fn file(
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    if !s.is_file_transfer() || !*s.server_file_transfer_enabled.read().unwrap() {
        return Err(
            "An authenticated file-transfer session with file permission is required".into(),
        );
    }
    let cursor = state.events.cursor();
    if name == "file_directory" {
        let directory = state.directory.lock().unwrap();
        let directory = directory
            .as_ref()
            .ok_or("No remote directory listing received; call file_list first")?;
        if let Some(error) = directory.get("error").and_then(Value::as_str) {
            return Err(error.into());
        }
        let entries = directory
            .get("entries")
            .and_then(Value::as_array)
            .ok_or("Invalid directory listing")?;
        let offset = number(args, "offset", 0) as usize;
        if offset > entries.len() {
            return Err("Directory offset is out of range".into());
        }
        let end = (offset + number(args, "limit", 100) as usize).min(entries.len());
        return Ok(success(
            json!({"path":directory["path"],"id":directory["id"],
            "entries":entries[offset..end],"total":entries.len(),"next_offset":if end < entries.len(){Some(end)}else{None}}),
        ));
    }
    if name == "file_list" {
        *state.directory.lock().unwrap() = None;
        let mut action = FileAction::new();
        action.set_read_dir(ReadDir {
            path: string(args, "path")?.into(),
            include_hidden: flag(args, "include_hidden"),
            ..Default::default()
        });
        let mut msg = Message::new();
        msg.set_file_action(action);
        send(s, Data::Message(msg))?;
        return Ok(success(
            json!({"queued":true,"after_cursor":cursor,"event_type":"file_dir"}),
        ));
    }
    let existing = matches!(name, "file_cancel_job" | "file_confirm_override");
    let job = if existing {
        let job = number(args, "job_id", 0) as i32;
        if !state.jobs.lock().unwrap().contains(&job) {
            return Err("Unknown MCP file job".into());
        }
        job
    } else {
        if state.jobs.lock().unwrap().len() >= 256 {
            return Err(
                "Maximum 256 retained MCP file jobs; reconnect to start a new batch".into(),
            );
        }
        NEXT_JOB
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| "File job IDs exhausted")?
    };
    let path = || string(args, "path").map(str::to_owned);
    let data = match name {
        "file_transfer" => Data::SendFiles((
            job,
            hbb_common::fs::JobType::Generic,
            string(args, "source")?.into(),
            string(args, "destination")?.into(),
            0,
            flag(args, "include_hidden"),
            string(args, "direction")? == "download",
        )),
        "file_create_directory" => Data::CreateDir((job, path()?, true)),
        "file_rename" => {
            let new_name = string(args, "new_name")?;
            if new_name.is_empty()
                || new_name == "."
                || new_name == ".."
                || new_name.contains(['/', '\\', '\0'])
            {
                return Err("new_name must be a basename".into());
            }
            Data::RenameFile((job, path()?, new_name.into(), true))
        }
        "file_remove" => {
            if !flag(args, "confirm") {
                return Err("Explicit confirmation required".into());
            }
            Data::RemoveFile((job, path()?, 0, true))
        }
        "file_cancel_job" => Data::CancelJob(job),
        "file_confirm_override" => {
            let file = number(args, "file_num", 0) as i32;
            if state.overwrites.lock().unwrap().get(&(job, file)) != Some(&flag(args, "is_upload"))
            {
                return Err("No matching pending overwrite prompt for this MCP job".into());
            }
            Data::SetConfirmOverrideFile((
                job,
                file,
                flag(args, "overwrite"),
                false,
                flag(args, "is_upload"),
            ))
        }
        _ => return Err("Unknown file operation".into()),
    };
    state.jobs.lock().unwrap().insert(job);
    if let Err(e) = send(s, data) {
        if !existing {
            state.jobs.lock().unwrap().remove(&job);
        }
        return Err(e);
    }
    if name == "file_confirm_override" {
        state
            .overwrites
            .lock()
            .unwrap()
            .remove(&(job, number(args, "file_num", 0) as i32));
    } else if name == "file_cancel_job" {
        state
            .overwrites
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != job);
    }
    state
        .events
        .push("tool_queued", json!({"tool":name,"job_id":job}));
    Ok(success(
        json!({"queued":true,"job_id":job,"after_cursor":cursor}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_notifies_only_the_target_before_removing_it() {
        if std::env::var_os("RUSTDESK_MCP_DISCONNECT_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_mcp::session::tests::disconnect_notifies_only_the_target_before_removing_it",
                    "--test-threads=1",
                ])
                .env("RUSTDESK_MCP_DISCONNECT_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use hbb_common::config;
        *config::APP_NAME.write().unwrap() =
            format!("RustDeskDisconnectTest-{}", SessionID::new_v4());
        config::OVERWRITE_LOCAL_SETTINGS.write().unwrap().extend([
            ("enable-agent-mcp".into(), "Y".into()),
            ("agent-mcp-devices".into(), String::new()),
        ]);
        let read_events = |state: &SessionState| {
            state.events.read(0, None, Duration::ZERO, || true).unwrap()["events"]
                .as_array()
                .unwrap()
                .clone()
        };
        let desktop_id = SessionID::new_v4();
        let desktop = FlutterSession::default();
        let (desktop_tx, mut desktop_rx) = tokio::sync::mpsc::unbounded_channel();
        *desktop.sender.write().unwrap() = Some(desktop_tx);
        sessions::insert_session(desktop_id, ConnType::DEFAULT_CONN, desktop.clone());
        let desktop_state = track(desktop_id, false).unwrap();

        for kind in [ConnType::FILE_TRANSFER, ConnType::TERMINAL] {
            let s = FlutterSession::default();
            s.lc.write().unwrap().conn_type = kind;
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            *s.sender.write().unwrap() = Some(tx);
            let first = SessionID::new_v4();
            let second = SessionID::new_v4();
            sessions::insert_session(first, kind, s.clone());
            sessions::insert_session(second, kind, s.clone());
            let first_state = track(first, false).unwrap();
            let second_state = track(second, false).unwrap();

            let result = call(first, &s, &first_state, "disconnect_device", &Map::new()).unwrap();
            assert_eq!(result["structuredContent"]["disconnected"], true);
            let events = read_events(&first_state);
            assert_eq!(events.len(), 1);
            assert_eq!(events[0]["type"], "agent_close_session");
            assert_eq!(events[0]["data"]["session"], first.to_string());
            assert!(read_events(&second_state).is_empty());
            assert!(sessions::get_session_by_session_id(&first).is_none());
            assert!(sessions::get_session_by_session_id(&second).is_some());
            assert!(!states().lock().unwrap().contains_key(&first));
            assert!(rx.try_recv().is_err());

            call(second, &s, &second_state, "disconnect_device", &Map::new()).unwrap();
            assert_eq!(read_events(&second_state)[0]["type"], "agent_close_session");
            assert!(matches!(rx.try_recv().unwrap(), Data::Close));
            assert!(sessions::get_session_by_session_id(&second).is_none());
            assert_eq!(list().len(), 1);
            assert_eq!(list()[0]["session"], desktop_id.to_string());
            assert!(read_events(&desktop_state).is_empty());
            assert!(desktop_rx.try_recv().is_err());
        }
        call(
            desktop_id,
            &desktop,
            &desktop_state,
            "disconnect_device",
            &Map::new(),
        )
        .unwrap();
        assert_eq!(
            read_events(&desktop_state)[0]["type"],
            "agent_close_session"
        );
        assert!(matches!(desktop_rx.try_recv().unwrap(), Data::Close));
        assert!(list().is_empty());
    }

    #[test]
    fn visible_connection_preserves_kind_peer_and_relay() {
        for (kind, authority) in [
            ("desktop", "connect"),
            ("files", "file-transfer"),
            ("terminal", "terminal"),
        ] {
            for (prefix, relay) in [
                ("rustdesk://", false),
                ("rustdesk://", true),
                ("rustdeskmcptest://", false),
                ("rustdeskmcptest://", true),
            ] {
                let event: Value =
                    serde_json::from_str(&window_event(prefix, "123456789", kind, relay)).unwrap();
                assert_eq!(event["name"], "on_url_scheme_received");
                let url = url::Url::parse(event["url"].as_str().unwrap()).unwrap();
                assert_eq!(url.scheme(), prefix.trim_end_matches("://"));
                assert_eq!(url.host_str(), Some(authority));
                assert_eq!(url.path(), "/123456789");
                assert_eq!(url.query(), relay.then_some("relay=true"));
            }
        }
    }

    #[test]
    fn missing_main_window_fails_without_launching_another_process() {
        assert!(crate::flutter::get_global_event_channels().is_empty());
        assert!(open_window("123456789", "desktop", false)
            .unwrap_err()
            .contains("main window is unavailable"));
    }
}
