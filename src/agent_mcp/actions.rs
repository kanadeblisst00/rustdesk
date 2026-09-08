use super::*;
use crate::flutter::FlutterSession;

pub(super) fn call(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    args: &Map<String, Value>,
) -> ToolResult {
    if !s.is_default() {
        return Err("A desktop session is required".into());
    }
    let prepared = rustdesk_agent_mcp::actions::prepare(&id.to_string(), args)?;
    let generation = state.automation.generation();
    let deadline = Instant::now() + Duration::from_secs(35);
    let check = || -> Result<(), String> {
        writable()?;
        automation::check(id, s, &state.automation, generation)?;
        if Instant::now() >= deadline {
            return Err("Batch time budget exhausted; remaining actions were not queued".into());
        }
        Ok(())
    };
    let mut completed = Vec::new();
    let mut failure = None;
    for (index, action) in prepared.iter().enumerate() {
        let started = Instant::now();
        let result = (|| {
            check()?;
            let foreground = args
                .get("expected_window")
                .and_then(Value::as_str)
                .map(|token| automation::windows::assert_foreground(id, s, state, token))
                .transpose()?;
            check()?;
            let result = if action.name == "clipboard_set" {
                session::call(id, s, state, &action.name, &action.arguments)?
            } else {
                desktop::input(s, state, &action.name, &action.arguments)?
            };
            completed.push(json!({"index":index,"name":action.name,"status":"queued",
                "result":result["structuredContent"],"observed_foreground":foreground,
                "queue_elapsed_ms":started.elapsed().as_millis()}));
            let delay_end = Instant::now() + Duration::from_millis(action.delay_ms);
            while Instant::now() < delay_end {
                check()?;
                std::thread::sleep(
                    delay_end
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(20)),
                );
            }
            Ok::<_, String>(())
        })();
        if let Err(error) = result {
            failure = Some((index, error));
            break;
        }
    }
    let mut result = success(
        json!({"queued_actions":completed.len(),"completed":completed,
        "delivery":"queued_to_rustdesk_transport","task_success":null}),
    );
    if let Some((index, error)) = failure {
        result["isError"] = json!(true);
        result["structuredContent"]["failed_index"] = json!(index);
        result["structuredContent"]["error"] = json!(error);
    }
    if flag(args, "screenshot_after") {
        let shot = automation::check(id, s, &state.automation, generation).and_then(|_| {
            automation::fresh_shot(
                id,
                s,
                state,
                desktop::display(s, state, args),
                number(args, "timeout_ms", 3000).min(3000),
            )
        });
        match shot {
            Ok(shot) => {
                result["structuredContent"]["screen"] = shot["structuredContent"].clone();
                if let (Some(out), Some(items)) =
                    (result["content"].as_array_mut(), shot["content"].as_array())
                {
                    out.extend(items.iter().filter(|item| item["type"] == "image").cloned());
                }
            }
            Err(error) => result["structuredContent"]["screenshot_error"] = json!(error),
        }
    }
    // Keep the text representation consistent with structuredContent, including partial failures.
    result["content"][0]["text"] = json!(result["structuredContent"].to_string());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Data;
    use hbb_common::{config, message_proto::*, rendezvous_proto::ConnType};

    #[test]
    fn window_aware_batches_preserve_input_order_and_stop_on_error() {
        if std::env::var_os("RUSTDESK_MCP_ACTIONS_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "agent_mcp::actions::tests::window_aware_batches_preserve_input_order_and_stop_on_error", "--test-threads=1"])
                .env("RUSTDESK_MCP_ACTIONS_TEST_CHILD", "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        *config::APP_NAME.write().unwrap() = format!("RustDeskActionsTest-{}", SessionID::new_v4());
        config::OVERWRITE_LOCAL_SETTINGS.write().unwrap().extend([
            ("enable-agent-mcp".into(), "Y".into()),
            ("agent-mcp-read-only".into(), "N".into()),
            ("agent-mcp-devices".into(), String::new()),
        ]);
        let id = SessionID::new_v4();
        let s = FlutterSession::default();
        *s.server_keyboard_enabled.write().unwrap() = true;
        *s.server_clipboard_enabled.write().unwrap() = true;
        {
            let mut lc = s.lc.write().unwrap();
            lc.get_config().info.platform = "Windows".into();
            lc.peer_info = Some(PeerInfo {
                platform: "Windows".into(),
                displays: vec![DisplayInfo {
                    width: 2000,
                    height: 8,
                    ..Default::default()
                }],
                ..Default::default()
            });
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *s.sender.write().unwrap() = Some(tx.clone());
        crate::flutter::sessions::insert_session(id, ConnType::DEFAULT_CONN, s.clone());
        let state = track(id, false).unwrap();
        *state.requested_display.lock().unwrap() = Some(0);
        *state.frame.lock().unwrap() = Some(desktop::Frame {
            display: 0,
            width: 2000,
            height: 8,
            rgba: Arc::new(vec![255; 2000 * 8 * 4]),
            sequence: 1,
            captured: Instant::now(),
        });

        let window = json!({"handle":"1","process_id":"2","process_started":"3","class_name":"Chat","title":"Chat","process_name":"TestChat"});
        let active = Arc::new(Mutex::new(window.clone()));
        let (observed_tx, observed_rx) = std::sync::mpsc::channel();
        let worker_active = active.clone();
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            while let Some(data) = rx.blocking_recv() {
                match data {
                    Data::Message(message) => {
                        if let Some(request) = wire::decode(&message) {
                            let request = request.unwrap();
                            let result = match request["operation"].as_str().unwrap() {
                                "windows" => json!({"windows":[window],"truncated":false}),
                                "foreground" => {
                                    json!({"active_window":worker_active.lock().unwrap().clone()})
                                }
                                "focus_window" => json!({"focused":true,"active_window":window}),
                                _ => panic!("Unexpected request"),
                            };
                            let reply = wire::encode(&json!({"protocol":rustdesk_agent_mcp::automation::WIRE_VERSION,"id":request["id"],"result":result})).unwrap();
                            assert!(automation::response("", &reply));
                        } else if message.misc().refresh_video() {
                            let mut frame = worker_state.frame.lock().unwrap();
                            frame.as_mut().unwrap().sequence += 1;
                            frame.as_mut().unwrap().captured = Instant::now();
                        } else {
                            observed_tx.send(message).unwrap();
                        }
                    }
                    Data::Close => break,
                    _ => panic!("Unexpected transport data"),
                }
            }
        });
        let found = automation::call(
            id,
            &s,
            &state,
            "list_windows",
            json!({"process_name":"testchat"}).as_object().unwrap(),
        )
        .unwrap();
        let token = found["structuredContent"]["windows"][0]["window_id"]
            .as_str()
            .unwrap();
        let focused = automation::call(
            id,
            &s,
            &state,
            "focus_window",
            json!({"window_id":token}).as_object().unwrap(),
        )
        .unwrap();
        assert_eq!(focused["structuredContent"]["focused"], true);
        let batch = json!({"session":id.to_string(),"expected_window":token,"actions":[
            {"name":"clipboard_set","arguments":{"text":"hello 中文"},"delay_ms":30},
            {"name":"keyboard_hotkey","arguments":{"keys":["Ctrl","v"]}},
        ]});
        let start = Instant::now();
        let result = call(id, &s, &state, batch.as_object().unwrap()).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert_eq!(result["structuredContent"]["queued_actions"], 2);
        assert_eq!(
            result["structuredContent"]["completed"][1]["observed_foreground"]["title"],
            "Chat"
        );
        assert!(result["structuredContent"]["task_success"].is_null());
        assert_eq!(
            observed_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .clipboard()
                .content
                .as_ref(),
            "hello 中文".as_bytes()
        );
        for down in [true, true, false, false] {
            assert_eq!(
                observed_rx
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .key_event()
                    .down,
                down
            );
        }
        active.lock().unwrap()["process_started"] = json!("4");
        let stopped = call(id, &s, &state, batch.as_object().unwrap()).unwrap();
        assert_eq!(stopped["isError"], true);
        assert_eq!(stopped["structuredContent"]["queued_actions"], 0);
        assert!(observed_rx.try_recv().is_err());

        let partial = json!({"session":id.to_string(),"screenshot_after":true,"actions":[
            {"name":"keyboard_hotkey","arguments":{"keys":["Win"]}},
            {"name":"keyboard_hotkey","arguments":{"keys":["unknown-key"]}},
            {"name":"keyboard_input","arguments":{"text":"must not type"}}
        ]});
        let result = call(id, &s, &state, partial.as_object().unwrap()).unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["queued_actions"], 1);
        assert_eq!(result["structuredContent"]["failed_index"], 1);
        assert_eq!(result["structuredContent"]["screen"]["frame_id"], 2);
        assert!(result["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "image"));
        assert_eq!(
            observed_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .key_event()
                .control_key(),
            ControlKey::Meta
        );
        assert!(observed_rx.try_recv().is_err());

        for alias in ["Meta", "Win", "LWin", "Super", "Cmd"] {
            desktop::input(
                &s,
                &state,
                "keyboard_hotkey",
                json!({"keys":[alias]}).as_object().unwrap(),
            )
            .unwrap();
            assert_eq!(
                observed_rx
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap()
                    .key_event()
                    .control_key(),
                ControlKey::Meta
            );
        }

        let shot = desktop::screenshot(id, &s, &state, &Map::new()).unwrap();
        assert_eq!(shot["structuredContent"]["width"], 2000);
        assert_eq!(shot["structuredContent"]["scale_x"], 1.0);
        let shot = desktop::screenshot(
            id,
            &s,
            &state,
            json!({"region":{"x":100,"y":2,"width":128,"height":4},"max_width":64})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            shot["structuredContent"]["image_to_display"],
            json!({"offset_x":100,"offset_y":2,"scale_x":2.0,"scale_y":2.0})
        );
        let state_result =
            automation::call(id, &s, &state, "get_foreground_window", &Map::new()).unwrap();
        assert_eq!(
            state_result["structuredContent"]["active_window"]["process_started"],
            "4"
        );
        let observation = automation::call(
            id,
            &s,
            &state,
            "get_ui_state",
            json!({"include_uia":false,"include_ocr":false})
                .as_object()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(observation["structuredContent"]["uia"]["skipped"], true);
        assert_eq!(observation["structuredContent"]["ocr"]["skipped"], true);
        tx.send(Data::Close).unwrap();
        worker.join().unwrap();
        crate::flutter::sessions::remove_session_by_session_id(&id);
    }
}
