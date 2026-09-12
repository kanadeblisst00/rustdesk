use super::*;

pub(crate) fn mark_action(action: &mut hbb_common::message_proto::TerminalAction) {
    action
        .special_fields
        .mut_unknown_fields()
        .add_varint(50003, 1);
}

pub(crate) fn is_action(action: &hbb_common::message_proto::TerminalAction) -> bool {
    matches!(
        action.special_fields.unknown_fields().get(50003),
        Some(hbb_common::protobuf::UnknownValueRef::Varint(1))
    )
}

pub(super) struct Terminal {
    pub phase: &'static str,
    pub output: ByteLog,
    pub generation: u64,
    pub details: Value,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            phase: "starting",
            output: ByteLog::default(),
            generation: 1,
            details: json!({}),
        }
    }
}

impl Terminal {
    pub fn begin(&mut self) -> bool {
        if matches!(self.phase, "starting" | "ready") {
            return false;
        }
        self.phase = "starting";
        self.generation += 1;
        self.output = ByteLog::default();
        self.details = json!({});
        true
    }

    fn response(&mut self, data: &Value) {
        match data["type"].as_str() {
            Some("opened") => {
                self.phase = if data["success"] == true {
                    "ready"
                } else {
                    "failed"
                };
                self.details = data.clone();
            }
            Some("closed") => {
                self.phase = "exited";
                self.details = data.clone();
            }
            Some("error") => {
                self.phase = if data["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("Terminal service") && m.contains("not found"))
                {
                    "service_lost"
                } else {
                    "failed"
                };
                self.details = data.clone();
            }
            _ => {}
        }
    }
}

pub(super) fn reconnected(state: &SessionState) {
    for terminal in state.terminals.lock().unwrap().values_mut() {
        if terminal.phase != "exited" {
            terminal.phase = "disconnected";
        }
    }
}

pub(super) fn response(state: &SessionState, data: &Value) {
    use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
    let Some(id) = data["terminal_id"]
        .as_i64()
        .and_then(|id| i32::try_from(id).ok())
    else {
        return;
    };
    let mut terminals = state.terminals.lock().unwrap();
    // Older peers send service-wide failures with ID zero, even for an open request.
    if id == 0 && data["type"] == "error" {
        for terminal in terminals.values_mut().filter(|t| t.phase != "exited") {
            terminal.response(data);
        }
        return;
    }
    if id <= 0 || (terminals.len() >= 16 && !terminals.contains_key(&id)) {
        return;
    }
    let entry = terminals.entry(id).or_default();
    if data["type"] == "data" {
        if let Some(encoded) = data["data"].as_str() {
            match STANDARD.decode(encoded) {
                Ok(bytes) => entry.output.append(&bytes),
                Err(error) => log::warn!("MCP terminal frame: {error}"),
            }
        }
    } else {
        entry.response(data);
    }
}

pub(super) fn opened(state: &SessionState, id: i32, timeout_ms: u64) -> Value {
    let started = Instant::now();
    loop {
        let terminals = state.terminals.lock().unwrap();
        let Some(terminal) = terminals.get(&id) else {
            return json!({"terminal_id":id,"state":"unknown","ready":false});
        };
        if terminal.phase != "starting" || started.elapsed() >= Duration::from_millis(timeout_ms) {
            return json!({"terminal_id":id,"state":terminal.phase,"ready":terminal.phase == "ready",
                "generation":terminal.generation,"details":terminal.details,
                "waited_ms":started.elapsed().as_millis() as u64});
        }
        drop(terminals);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_marker_survives_wire_encoding_without_changing_ui_actions() {
        use hbb_common::{message_proto::TerminalAction, protobuf::Message as _};
        let mut action = TerminalAction::new();
        assert!(!is_action(&action));
        mark_action(&mut action);
        let decoded = TerminalAction::parse_from_bytes(&action.write_to_bytes().unwrap()).unwrap();
        assert!(is_action(&decoded));
    }

    #[test]
    fn service_errors_complete_all_pending_opens_without_creating_id_zero() {
        let state = SessionState::default();
        for id in [1, 2] {
            state
                .terminals
                .lock()
                .unwrap()
                .insert(id, Terminal::default());
        }
        response(
            &state,
            &json!({"terminal_id":0,"type":"error","message":"Terminal service ts_test not found"}),
        );
        for id in [1, 2] {
            let result = opened(&state, id, 1000);
            assert_eq!(result["state"], "service_lost");
            assert_eq!(result["ready"], false);
        }
        assert!(!state.terminals.lock().unwrap().contains_key(&0));
    }

    #[test]
    fn reopen_is_idempotent_and_reconnect_requires_a_fresh_ack() {
        let state = Arc::new(SessionState::default());
        state
            .terminals
            .lock()
            .unwrap()
            .insert(7, Terminal::default());
        assert!(!state.terminals.lock().unwrap().get_mut(&7).unwrap().begin());
        assert_eq!(opened(&state, 7, 0)["state"], "starting");
        let remote = state.clone();
        let reply = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            response(
                &remote,
                &json!({"terminal_id":7,"type":"opened","success":true,"service_id":"ts_test","pid":42}),
            );
        });
        assert_eq!(opened(&state, 7, 1000)["state"], "ready");
        reply.join().unwrap();
        reconnected(&state);
        assert_eq!(opened(&state, 7, 0)["state"], "disconnected");
        let mut terminals = state.terminals.lock().unwrap();
        let terminal = terminals.get_mut(&7).unwrap();
        assert!(terminal.begin());
        assert_eq!(terminal.generation, 2);
        assert_eq!(terminal.phase, "starting");
        drop(terminals);
        response(
            &state,
            &json!({"terminal_id":7,"type":"opened","success":false,"message":"permission denied"}),
        );
        assert_eq!(
            opened(&state, 7, 0)["details"]["message"],
            "permission denied"
        );
        response(
            &state,
            &json!({"terminal_id":7,"type":"closed","exit_code":7}),
        );
        assert_eq!(opened(&state, 7, 0)["state"], "exited");
        assert_eq!(opened(&state, 7, 0)["details"]["exit_code"], 7);
    }
}
