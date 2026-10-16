use super::*;

const FILE_PROBE_MARKER: u32 = 0x4d435052;

#[derive(Default)]
pub(super) struct State {
    file_probe: Mutex<Option<(u64,i64,bool)>>,
}

impl State {
    pub(super) fn reset(&self) { self.file_probe.lock().unwrap().take(); }
    fn acknowledge(&self, generation: u64, nonce: i64) {
        if let Some((observed,expected,ready)) = self.file_probe.lock().unwrap().as_mut() {
            if *observed == generation && *expected == nonce { *ready = true; }
        }
    }
}

pub(crate) fn response(peer: &str, files: bool, message: &hbb_common::message_proto::Message) -> bool {
    let Some(hbb_common::message_proto::message::Union::TestDelay(probe)) = message.union.as_ref() else { return false; };
    if !files || !probe.from_client || probe.time >= 0 || probe.last_delay != FILE_PROBE_MARKER { return false; }
    let Some(session) = crate::flutter::sessions::get_session_by_peer_id(peer.into(),hbb_common::rendezvous_proto::ConnType::FILE_TRANSFER) else { return true; };
    for id in session.agent_session_ids() {
        let state = states().lock().unwrap().get(&id).cloned();
        if let Some(state) = state {
            state.readiness.acknowledge(state.automation.generation(),probe.time);
        }
    }
    true
}

pub(super) fn annotate(s: &crate::flutter::FlutterSession, info: &mut Value) {
    let authenticated = info["connected"] == true;
    let state = info["session"].as_str().and_then(|id| SessionID::parse_str(id).ok())
        .and_then(|id| states().lock().unwrap().get(&id).cloned());
    let command = authenticated && s.is_terminal() && state.as_ref().is_some_and(|state|
        state.process.negotiated(state.automation.generation()));
    let files = authenticated && s.is_file_transfer() && info["permissions"]["files"] == true
        && state.as_ref().is_some_and(|state|state.readiness.file_probe.lock().unwrap().as_ref()
            .is_some_and(|(generation,_,ready)|*generation == state.automation.generation() && *ready));
    info["readiness"] = json!({"session_created":true,"authenticated":authenticated,
        "command_channel":if command {"ready"} else {"unverified"},
        "file_channel":if files {"ready"} else {"unverified"},
        "evidence":{"command":"current connection capability response",
            "files":"authenticated file session and matching transport nonce; path access is checked separately"}});
}

pub(super) fn wait(id: SessionID, original: &crate::flutter::FlutterSession,
    state: &SessionState, args: &Map<String, Value>) -> ToolResult {
    let deadline = Instant::now() + Duration::from_millis(number(args,"timeout_ms",12000) as u64);
    let channel = args.get("channel").and_then(Value::as_str).unwrap_or("authentication");
    let mut file_probe_sent = false;
    loop {
        let s = session::get(id)?;
        if !Arc::ptr_eq(&s, original) { return Err("Session changed while waiting for readiness".into()); }
        let mut info = session::info(id, &s);
        let authenticated = info["connected"] == true;
        let ready = authenticated && match channel {
            "processes" => info["readiness"]["command_channel"] == "ready",
            "files" => info["readiness"]["file_channel"] == "ready",
            _ => true,
        };
        if ready || (!authenticated && auth::settled(&info)) || Instant::now() >= deadline {
            info["ready"] = json!(ready);
            info["channel"] = json!(channel);
            info["wait_timed_out"] = json!(!ready && Instant::now() >= deadline);
            return Ok(success(info));
        }
        if authenticated && channel == "files" && info["permissions"]["files"] != true {
            info["ready"] = json!(false);
            info["channel"] = json!(channel);
            info["channel_error"] = json!("File transfer permission is unavailable on this session");
            info["wait_timed_out"] = json!(false);
            return Ok(success(info));
        }
        if authenticated && channel == "processes" {
            let query = json!({"session":id.to_string()});
            let result = process::negotiate_with_timeout(id, &s, state,
                query.as_object().ok_or("Invalid readiness query")?, Some(deadline.saturating_duration_since(Instant::now())));
            match result {
                Ok(result) if result["structuredContent"]["negotiated"] == true => continue,
                Ok(result) => info["channel_observation"] = result["structuredContent"].clone(),
                Err(error) => info["channel_error"] = json!(error),
            }
            info["ready"] = json!(false);
            info["channel"] = json!(channel);
            info["wait_timed_out"] = json!(Instant::now() >= deadline);
            return Ok(success(info));
        }
        if authenticated && channel == "files" && !file_probe_sent {
            if !s.is_file_transfer() { return Err("A file-transfer session is required for file readiness".into()); }
            let _guard = state.operation.try_lock().map_err(|_| "Another file operation is in progress; wait on the same session")?;
            let bytes = uuid::Uuid::new_v4();
            let mut nonce = [0;8]; nonce.copy_from_slice(&bytes.as_bytes()[..8]);
            let probe = -((u64::from_le_bytes(nonce) & i64::MAX as u64).max(1) as i64);
            *state.readiness.file_probe.lock().unwrap() = Some((state.automation.generation(),probe,false));
            let mut message = hbb_common::message_proto::Message::new();
            message.set_test_delay(hbb_common::message_proto::TestDelay {time:probe,from_client:true,last_delay:FILE_PROBE_MARKER,..Default::default()});
            session::send(&s,crate::client::Data::Message(message))?;
            file_probe_sent = true;
            continue;
        }
        std::thread::sleep(Duration::from_millis(50).min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_or_wrong_file_probe_receipts_cannot_mark_a_new_round_ready() {
        let state = State::default();
        *state.file_probe.lock().unwrap() = Some((2,-7,false));
        state.acknowledge(1,-7);
        state.acknowledge(2,-8);
        assert_eq!(state.file_probe.lock().unwrap().as_ref().unwrap().2,false);
        state.acknowledge(2,-7);
        assert_eq!(state.file_probe.lock().unwrap().as_ref().unwrap().2,true);
        state.reset();
        state.acknowledge(2,-7);
        assert!(state.file_probe.lock().unwrap().is_none());
    }
}
