use super::*;
use crate::{client::Data, flutter::FlutterSession};
use rustdesk_agent_mcp::automation as model;
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) mod windows;

struct Pending {
    id: String,
    result: Option<Result<Value, String>>,
}

struct Snapshot {
    captured: Instant,
    display: usize,
    raw: Value,
    elements: HashMap<String, Value>,
}

#[derive(Default)]
pub(super) struct State {
    pending: Mutex<Option<Pending>>,
    snapshot: Mutex<Option<Snapshot>>,
    unavailable: Mutex<Option<(Instant, String, String)>>,
    generation: AtomicU64,
    windows: windows::State,
}

impl State {
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    pub fn reset(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.pending.lock().unwrap().take();
        self.snapshot.lock().unwrap().take();
        self.unavailable.lock().unwrap().take();
        self.windows.clear();
    }
}

struct PendingGuard<'a>(&'a State);
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().take();
    }
}

pub(crate) fn response(peer_id: &str, msg: &hbb_common::message_proto::Message) -> bool {
    let Some(response) = wire::decode(msg) else {
        return false;
    };
    let response = match response {
        Ok(response) => response,
        Err(e) => {
            log::warn!("MCP UIA response: {e}");
            return true;
        }
    };
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
        let mut pending = state.automation.pending.lock().unwrap();
        if let Some(p) = pending.as_mut().filter(|p| response["id"] == p.id) {
            p.result = Some(
                if let Some(error) = response.get("error").and_then(Value::as_str) {
                    Err(error.into())
                } else {
                    response
                        .get("result")
                        .cloned()
                        .ok_or_else(|| "Missing UIA result".into())
                },
            );
        }
    }
    true
}

pub(super) fn check(
    id: SessionID,
    s: &FlutterSession,
    state: &State,
    generation: u64,
) -> Result<(), String> {
    let current = session::get(id)?;
    if !Arc::ptr_eq(s, &current) || state.generation.load(Ordering::SeqCst) != generation {
        return Err("Connection changed; observe the remote desktop again".into());
    }
    session::ready(s)
}

fn request(
    id: SessionID,
    s: &FlutterSession,
    state: &State,
    generation: u64,
    operation: &str,
    element: Option<&Value>,
    value: Option<&str>,
) -> Result<Value, String> {
    check(id, s, state, generation)?;
    if !s.is_default() {
        return Err("A desktop session is required".into());
    }
    if s.lc
        .read()
        .unwrap()
        .peer_info
        .as_ref()
        .map_or(true, |p| p.platform != "Windows")
    {
        return Err("Remote UIA is available only on Windows".into());
    }
    let mutation = !matches!(
        operation,
        "tree" | "taskbar_tree" | "capabilities" | "windows" | "foreground"
    );
    if mutation {
        desktop::input_allowed(s)?;
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut payload = json!({"protocol":model::WIRE_VERSION,"id":request_id,"operation":operation});
    if let Some(element) = element {
        payload["element"] = element.clone();
    }
    if let Some(value) = value {
        payload["value"] = json!(value);
    }
    model::validate_request(&payload)?;
    let msg = wire::encode(&payload)?;
    *state.pending.lock().unwrap() = Some(Pending {
        id: request_id,
        result: None,
    });
    let _guard = PendingGuard(state);
    check(id, s, state, generation)?;
    session::send(s, Data::Message(msg))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        check(id, s, state, generation)?;
        if let Some(result) = state
            .pending
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|p| p.result.take())
        {
            return result;
        }
        if Instant::now() >= deadline {
            return Err(if mutation { "UIA action timed out; outcome unknown. Inspect get_ui_state before deciding whether to retry" }
                else { "Remote UIA unavailable or timed out; the peer may need an mcp build" }.into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn tree(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    display: usize,
    scope: &str,
) -> Value {
    state.automation.snapshot.lock().unwrap().take();
    if let Some((time, error, _)) = state
        .automation
        .unavailable
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(_, _, cached_scope)| cached_scope == scope)
    {
        if time.elapsed() < Duration::from_secs(30) {
            return json!({"available":false,"error":error,"retry_after_ms":30000-time.elapsed().as_millis().min(30000)});
        }
    }
    let result = (|| {
        let mut raw = request(
            id,
            s,
            &state.automation,
            state.automation.generation.load(Ordering::SeqCst),
            if scope == "taskbar" {
                "taskbar_tree"
            } else {
                "tree"
            },
            None,
            None,
        )?;
        let elements = raw
            .get("elements")
            .and_then(Value::as_array)
            .ok_or("Invalid remote UIA tree")?;
        if elements.len() > model::MAX_NODES {
            return Err("Remote UIA tree exceeds node limit".into());
        }
        let lc = s.lc.read().unwrap();
        let d = lc
            .peer_info
            .as_ref()
            .and_then(|p| p.displays.get(display))
            .ok_or("Unknown display")?;
        model::on_display(&mut raw, (d.x, d.y), (d.width, d.height));
        drop(lc);
        let mut public = raw.clone();
        let mut elements = HashMap::new();
        let mut ids = HashMap::new();
        for e in raw["elements"].as_array().ok_or("Missing UIA elements")? {
            let id = e["element_id"].as_str().ok_or("Missing UIA element ID")?;
            let token = uuid::Uuid::new_v4().to_string();
            ids.insert(id.to_owned(), token.clone());
            elements.insert(token, e.clone());
        }
        for e in public["elements"]
            .as_array_mut()
            .ok_or("Missing UIA elements")?
        {
            e["element_id"] = json!(e["element_id"].as_str().and_then(|id| ids.get(id)));
            e["parent_id"] = json!(e["parent_id"].as_str().and_then(|id| ids.get(id)));
        }
        public["display"] = json!(display);
        public["coordinate_space"] = json!("display_relative_original_pixels");
        *state.automation.snapshot.lock().unwrap() = Some(Snapshot {
            captured: Instant::now(),
            display,
            raw,
            elements,
        });
        Ok::<_, String>(public)
    })();
    match result {
        Ok(tree) => {
            state.automation.unavailable.lock().unwrap().take();
            tree
        }
        Err(error) => {
            *state.automation.unavailable.lock().unwrap() =
                Some((Instant::now(), error.clone(), scope.into()));
            json!({"available":false,"error":error})
        }
    }
}

pub(super) fn fresh_shot(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    display: usize,
    timeout: i64,
) -> ToolResult {
    let after = state
        .frame
        .lock()
        .unwrap()
        .as_ref()
        .map(|f| f.sequence)
        .unwrap_or(0);
    s.refresh_video(display as i32);
    desktop::screenshot(
        id,
        s,
        state,
        json!({"display":display,"after_frame":after,"max_width":32768,"timeout_ms":timeout})
            .as_object()
            .ok_or("Invalid screenshot arguments")?,
    )
}

fn with_image(data: Value, shot: &Value) -> Value {
    let mut result = success(data);
    if let (Some(out), Some(items)) = (result["content"].as_array_mut(), shot["content"].as_array())
    {
        out.extend(items.iter().filter(|v| v["type"] == "image").cloned());
    }
    result
}

fn raw_target(state: &State, id: &str, display: usize) -> Result<(Value, Value), String> {
    let snapshot = state.snapshot.lock().unwrap();
    let snapshot = snapshot
        .as_ref()
        .filter(|s| s.display == display && s.captured.elapsed() < Duration::from_secs(30))
        .ok_or("UIA snapshot expired; call get_ui_tree or find_ui_element again")?;
    let element = snapshot
        .elements
        .get(id)
        .ok_or("Unknown element_id in this session's latest snapshot")?;
    Ok((element.clone(), snapshot.raw.clone()))
}

fn pattern(element: &Value, requested: &str) -> Result<&'static str, String> {
    let has = |name: &str| {
        element["patterns"]
            .as_array()
            .is_some_and(|p| p.iter().any(|v| v == name))
    };
    if element["enabled"] != true || element["offscreen"] == true || element["password"] == true {
        return Err("UIA target is disabled, offscreen, or a password control".into());
    }
    match requested {
        "auto" if has("Invoke") => Ok("invoke"),
        "auto" if has("Toggle") => Ok("toggle"),
        "invoke" if has("Invoke") => Ok("invoke"),
        "toggle" if has("Toggle") => Ok("toggle"),
        "set_value" if has("Value") => Ok("set_value"),
        _ => Err("Requested UIA pattern is unavailable".into()),
    }
}

fn verify(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    display: usize,
    timeout: i64,
    baseline: (u64, Option<&Value>),
    action: Result<Value, String>,
) -> Value {
    let (generation, before) = baseline;
    let (action, error) = match action {
        Ok(result) => (result, None),
        Err(error) => (Value::Null, Some(error)),
    };
    if let Err(reason) = check(id, s, &state.automation, generation) {
        let mut result = success(json!({"action":action,"action_error":error,
            "verification":{"error":reason,"uia_changed":null,"new_frame":false,"task_success":null},
            "guidance":"Connection changed; do not automatically retry the action"}));
        result["isError"] = json!(true);
        return result;
    }
    // Snapshot and UIA are independent observations; neither is proof of task completion.
    let shot = fresh_shot(id, s, state, display, timeout.min(3000));
    let scope = before
        .and_then(|v| v["scope"].as_str())
        .unwrap_or("foreground_window");
    let uia = tree(id, s, state, display, scope);
    let after = state.automation.snapshot.lock().unwrap();
    let changed = before
        .zip(after.as_ref())
        .map(|(before, after)| before != &after.raw);
    drop(after);
    let mut data = json!({"action":action,"action_error":error,"uia":uia,
        "verification":{"uia_changed":changed,"new_frame":shot.is_ok(),"task_success":null},
        "guidance":"Inspect UIA changes and the screenshot; do not retry uncertain actions automatically"});
    let mut result = match shot {
        Ok(shot) => {
            data["screen"] = shot["structuredContent"].clone();
            with_image(data, &shot)
        }
        Err(error) => {
            data["verification"]["screenshot_error"] = json!(error);
            success(data)
        }
    };
    if error.is_some() {
        result["isError"] = json!(true);
    }
    result
}

pub(super) fn call(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    session::ready(s)?;
    if !s.is_default() {
        return Err("A desktop session is required".into());
    }
    let generation = state.automation.generation.load(Ordering::SeqCst);
    let display = desktop::display(s, state, args);
    let timeout = number(args, "timeout_ms", 3000);
    if matches!(
        name,
        "list_windows" | "get_foreground_window" | "focus_window"
    ) {
        return windows::call(id, s, state, name, args);
    }
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("foreground_window");
    if matches!(name, "invoke_ui_element" | "set_ui_value") {
        desktop::input_allowed(s)?;
        let (element, before) =
            raw_target(&state.automation, string(args, "element_id")?, display)?;
        let requested = if name == "set_ui_value" {
            "set_value"
        } else {
            args.get("action").and_then(Value::as_str).unwrap_or("auto")
        };
        let operation = pattern(&element, requested)?;
        check(id, s, &state.automation, generation)?;
        let action = request(
            id,
            s,
            &state.automation,
            generation,
            operation,
            Some(&element),
            args.get("value").and_then(Value::as_str),
        );
        return Ok(verify(
            id,
            s,
            state,
            display,
            timeout,
            (generation, Some(&before)),
            action,
        ));
    }
    let uia = if name == "get_ui_state" && args.get("include_uia") == Some(&Value::Bool(false)) {
        json!({"available":false,"skipped":true})
    } else {
        tree(id, s, state, display, scope)
    };
    check(id, s, &state.automation, generation)?;
    if name == "get_ui_tree" {
        return Ok(success(uia));
    }
    let matches = uia["elements"]
        .as_array()
        .map(|elements| model::find(elements, args))
        .unwrap_or_default();
    let shot = fresh_shot(id, s, state, display, timeout)?;
    check(id, s, &state.automation, generation)?;
    if name == "get_ui_state" {
        return Ok(with_image(
            json!({"screen":shot["structuredContent"],"uia":uia}),
            &shot,
        ));
    }
    if name != "click_text" {
        return Ok(with_image(
            json!({"found":!matches.is_empty(),"source":"uia","matches":matches,
            "screen":shot["structuredContent"],"uia":uia,
            "next_step":if matches.is_empty(){"Use the screenshot with a vision model"}else{"Choose an unambiguous target"}}),
            &shot,
        ));
    }
    desktop::input_allowed(s)?;
    let selected = match model::choose(&matches, args.get("match_index").and_then(Value::as_u64)) {
        Ok(selected) => selected,
        Err(error) => {
            let mut result = with_image(
                json!({"found":!matches.is_empty(),"matches":matches,"error":error,
                "screen":shot["structuredContent"],"uia":uia}),
                &shot,
            );
            result["isError"] = json!(true);
            return Ok(result);
        }
    };
    let before = state
        .automation
        .snapshot
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.raw.clone());
    let action = if selected["patterns"]
        .as_array()
        .is_some_and(|patterns| patterns.iter().any(|p| p == "Invoke" || p == "Toggle"))
    {
        let (element, _) = raw_target(
            &state.automation,
            selected["element_id"]
                .as_str()
                .ok_or("Missing element ID")?,
            display,
        )?;
        let operation = pattern(&element, "auto")?;
        request(
            id,
            s,
            &state.automation,
            generation,
            operation,
            Some(&element),
            None,
        )
    } else {
        // Re-observe before coordinate input because UIA rectangles may move.
        fresh_shot(id, s, state, display, timeout.min(3000))?;
        let raw_id = raw_target(
            &state.automation,
            selected["element_id"]
                .as_str()
                .ok_or("Missing element ID")?,
            display,
        )?
        .0["element_id"]
            .clone();
        let current = tree(id, s, state, display, scope);
        let found = current["elements"]
            .as_array()
            .map(|elements| model::find(elements, args))
            .unwrap_or_default();
        let selected = model::choose(&found, args.get("match_index").and_then(Value::as_u64))?;
        let element = raw_target(
            &state.automation,
            selected["element_id"]
                .as_str()
                .ok_or("Missing element ID")?,
            display,
        )?
        .0;
        if element["element_id"] != raw_id
            || element["enabled"] != true
            || element["offscreen"] == true
            || element["password"] == true
        {
            return Err("UIA target changed; find it again".into());
        }
        let current = selected.clone();
        let b = &current["bounds"];
        let x = b["x"].as_f64().ok_or("Invalid target bounds")?
            + b["width"].as_f64().ok_or("Invalid target bounds")? / 2.0;
        let y = b["y"].as_f64().ok_or("Invalid target bounds")?
            + b["height"].as_f64().ok_or("Invalid target bounds")? / 2.0;
        check(id, s, &state.automation, generation)?;
        desktop::input(s, state, "mouse_click", json!({"display":display,"x":x.floor() as i64,"y":y.floor() as i64,"button":"left","clicks":1}).as_object().ok_or("Invalid click")?)
    };
    Ok(verify(
        id,
        s,
        state,
        display,
        timeout,
        (generation, before.as_ref()),
        action,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_ids_are_session_display_and_generation_scoped() {
        let state = State::default();
        *state.snapshot.lock().unwrap() = Some(Snapshot {
            captured: Instant::now(),
            display: 1,
            raw: json!({"elements":[]}),
            elements: HashMap::from([("token".into(), json!({"element_id":"runtime"}))]),
        });
        assert!(raw_target(&state, "token", 1).is_ok());
        assert!(raw_target(&state, "token", 0).is_err());
        assert!(raw_target(&state, "another-session-token", 1).is_err());
        state.snapshot.lock().unwrap().as_mut().unwrap().captured =
            Instant::now() - Duration::from_secs(31);
        assert!(raw_target(&state, "token", 1).is_err());
        *state.pending.lock().unwrap() = Some(Pending {
            id: "old".into(),
            result: None,
        });
        state.reset();
        assert!(state.pending.lock().unwrap().is_none());
        assert!(state.snapshot.lock().unwrap().is_none());
        assert_eq!(state.generation.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn selects_patterns_and_rejects_unsafe_targets() {
        let mut element = json!({"enabled":true,"offscreen":false,"password":false,"patterns":["Invoke","Toggle","Value"]});
        assert_eq!(pattern(&element, "auto").unwrap(), "invoke");
        assert_eq!(pattern(&element, "toggle").unwrap(), "toggle");
        assert_eq!(pattern(&element, "set_value").unwrap(), "set_value");
        element["patterns"] = json!(["Toggle"]);
        assert_eq!(pattern(&element, "auto").unwrap(), "toggle");
        assert!(pattern(&element, "invoke").is_err());
        for key in ["offscreen", "password"] {
            element[key] = json!(true);
            assert!(pattern(&element, "auto").is_err());
            element[key] = json!(false);
        }
        element["enabled"] = json!(false);
        assert!(pattern(&element, "auto").is_err());
    }
}
