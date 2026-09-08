use super::*;
use crate::{client::Data, flutter::FlutterSession};
use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use rustdesk_agent_mcp::automation as model;
use std::{
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

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
    ocr: Mutex<Option<(Vec<u8>, Value)>>,
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
        self.ocr.lock().unwrap().take();
        self.windows.clear();
    }
}

struct PendingGuard<'a>(&'a State);
impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.pending.lock().unwrap().take();
    }
}

pub(super) fn ocr_capability() -> Value {
    json!({"engine":"PP-OCRv4","configured":std::env::var_os("RUSTDESK_MCP_OCR_PYTHON").is_some(),
        "runtime":"Python with tools/mcp/ocr-requirements.txt","location":"controller"})
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

fn ocr(shot: &Value, state: &State) -> Value {
    let started = Instant::now();
    let result = (|| {
        let python = std::env::var_os("RUSTDESK_MCP_OCR_PYTHON").ok_or("PP-OCRv4 is not configured. Install tools/mcp/ocr-requirements.txt and set RUSTDESK_MCP_OCR_PYTHON before starting RustDesk. OCR recognizes rendered text, not unlabeled icons; use list_windows, taskbar UIA or screenshot vision for icons")?;
        let data = shot["content"]
            .as_array()
            .and_then(|items| items.iter().find(|v| v["type"] == "image"))
            .and_then(|v| v["data"].as_str())
            .ok_or("Screenshot has no image")?;
        let png = STANDARD.decode(data).map_err(|e| e.to_string())?;
        let previous = state.ocr.lock().unwrap();
        if let Some((previous, result)) = previous.as_ref() {
            if *previous == png {
                return Ok(result.clone());
            }
        }
        drop(previous);
        let mut command = Command::new(python);
        command.args(["-I", "-c", include_str!("../../tools/mcp/ocr.py")]);
        let output = rustdesk_agent_mcp::helper::run(&mut command, &png, Duration::from_secs(15))?;
        let result: Value =
            serde_json::from_slice(&output).map_err(|_| "Invalid PP-OCRv4 response")?;
        if result["available"] == true {
            *state.ocr.lock().unwrap() = Some((png, result.clone()));
        }
        Ok::<_, String>(result)
    })();
    let mut result =
        result.unwrap_or_else(|error| json!({"available":false,"engine":"PP-OCRv4","error":error}));
    result["frame_id"] = shot["structuredContent"]["frame_id"].clone();
    result["display"] = shot["structuredContent"]["display"].clone();
    result["processing_ms"] = json!(started.elapsed().as_millis());
    result
}

fn target_unchanged(first: &Value, second: &Value, bounds: &Value) -> Result<bool, String> {
    use image::GenericImageView;
    let decode = |shot: &Value| {
        let data = shot["content"]
            .as_array()
            .and_then(|items| items.iter().find(|v| v["type"] == "image"))
            .and_then(|v| v["data"].as_str())
            .ok_or("Screenshot has no image")?;
        image::load_from_memory(&STANDARD.decode(data).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    let (first, second) = (decode(first)?, decode(second)?);
    if first.dimensions() != second.dimensions() {
        return Ok(false);
    }
    let coordinate = |key| {
        bounds
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("Invalid OCR bounds")
    };
    let (x, y, w, h) = (
        coordinate("x")?,
        coordinate("y")?,
        coordinate("width")?,
        coordinate("height")?,
    );
    if w == 0
        || h == 0
        || x.checked_add(w).map_or(true, |r| r > first.width())
        || y.checked_add(h).map_or(true, |b| b > first.height())
    {
        return Err("OCR bounds outside frame".into());
    }
    // Check the target, not unrelated clocks or animations elsewhere on the screen.
    Ok(first
        .view(x, y, w, h)
        .pixels()
        .zip(second.view(x, y, w, h).pixels())
        .all(|(a, b)| a.2 == b.2))
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
    let uia = if matches!(name, "get_screen_text" | "find_text")
        || (name == "get_ui_state" && args.get("include_uia") == Some(&Value::Bool(false)))
    {
        json!({"available":false,"skipped":true})
    } else {
        tree(id, s, state, display, scope)
    };
    check(id, s, &state.automation, generation)?;
    if name == "get_ui_tree" {
        return Ok(success(uia));
    }
    let mut matches = uia["elements"]
        .as_array()
        .map(|elements| model::find(elements, args))
        .unwrap_or_default();
    let shot = fresh_shot(id, s, state, display, timeout)?;
    let text = if name == "get_ui_state" && args.get("include_ocr") == Some(&Value::Bool(false)) {
        json!({"available":false,"skipped":true})
    } else if name == "get_ui_state"
        || matches!(name, "get_screen_text" | "find_text")
        || matches.is_empty()
    {
        ocr(&shot, &state.automation)
    } else {
        json!({"available":false,"skipped":true})
    };
    check(id, s, &state.automation, generation)?;
    if name == "get_ui_state" {
        return Ok(with_image(
            json!({"screen":shot["structuredContent"],"uia":uia,"ocr":text}),
            &shot,
        ));
    }
    if name == "get_screen_text" {
        return Ok(with_image(
            json!({"screen":shot["structuredContent"],"ocr":text}),
            &shot,
        ));
    }
    let mut source = "uia";
    if matches.is_empty() {
        source = "ocr";
        matches = text["blocks"]
            .as_array()
            .map(|elements| model::find(elements, args))
            .unwrap_or_default();
    }
    if name != "click_text" {
        return Ok(with_image(
            json!({"found":!matches.is_empty(),"source":source,"matches":matches,
            "screen":shot["structuredContent"],"uia":uia,"ocr":text,
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
                "screen":shot["structuredContent"],"uia":uia,"ocr":text}),
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
    let action = if source == "uia"
        && selected["patterns"]
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
        // Re-observe before coordinate input. OCR may be slow, and UIA rectangles may move.
        let newer = fresh_shot(id, s, state, display, timeout.min(3000))?;
        let current = if source == "uia" {
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
            selected.clone()
        } else {
            // Do not click coordinates from an image that changed while OCR was running.
            if !target_unchanged(&shot, &newer, &selected["bounds"])? {
                return Err(
                    "Remote screen changed during OCR; find_text again before clicking".into(),
                );
            }
            selected.clone()
        };
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

    #[test]
    fn embedded_ocr_helper_runs_on_a_lossless_remote_frame() {
        let Some(python) = std::env::var_os("RUSTDESK_TEST_OCR_PYTHON") else {
            return;
        };
        use image::{ImageBuffer, ImageOutputFormat, Rgba};
        use std::io::Cursor;
        let image = ImageBuffer::from_pixel(200, 80, Rgba([255u8, 255, 255, 255]));
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, ImageOutputFormat::Png).unwrap();
        let mut command = Command::new(python);
        command.args(["-I", "-c", include_str!("../../tools/mcp/ocr.py")]);
        let output =
            rustdesk_agent_mcp::helper::run(&mut command, bytes.get_ref(), Duration::from_secs(15))
                .unwrap();
        let result: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(result["available"], true, "{result}");
        assert_eq!(result["engine"], "PP-OCRv4");
        assert!(result["blocks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn ocr_click_rejects_changed_target_but_ignores_unrelated_pixels() {
        use image::{ImageBuffer, ImageOutputFormat, Rgba};
        use std::io::Cursor;
        let shot = |changed: Option<(u32, u32)>| {
            let mut image = ImageBuffer::from_pixel(100, 100, Rgba([255u8, 255, 255, 255]));
            if let Some((x, y)) = changed {
                image.put_pixel(x, y, Rgba([0, 0, 0, 255]));
            }
            let mut bytes = Cursor::new(Vec::new());
            image.write_to(&mut bytes, ImageOutputFormat::Png).unwrap();
            json!({"content":[{"type":"image","data":STANDARD.encode(bytes.into_inner())}]})
        };
        let bounds = json!({"x":20,"y":20,"width":30,"height":30});
        let first = shot(None);
        assert!(target_unchanged(&first, &shot(Some((90, 90))), &bounds).unwrap());
        assert!(!target_unchanged(&first, &shot(Some((30, 30))), &bounds).unwrap());
        assert!(
            target_unchanged(&first, &first, &json!({"x":99,"y":0,"width":10,"height":2})).is_err()
        );
    }
}
