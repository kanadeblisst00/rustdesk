use super::*;

#[derive(Default)]
pub(super) struct State {
    entries: Mutex<HashMap<String, (Instant, Value)>>,
}

impl State {
    pub fn clear(&self) {
        self.entries.lock().unwrap().clear();
    }

    fn remember(&self, window: &Value) -> Value {
        let mut public = window.clone();
        if model::validate_window(window).is_err() {
            public["focusable"] = json!(false);
            return public;
        }
        let mut entries = self.entries.lock().unwrap();
        entries.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(30));
        let token = entries
            .iter()
            .find(|(_, (_, value))| {
                model::same_window(value, window) && value["title"] == window["title"]
            })
            .map(|(id, _)| id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if entries.len() >= 256 && !entries.contains_key(&token) {
            public["focusable"] = json!(false);
            return public;
        }
        entries.insert(token.clone(), (Instant::now(), window.clone()));
        public["window_id"] = json!(token);
        public["focusable"] = json!(true);
        public
    }

    fn resolve(&self, token: &str) -> Result<Value, String> {
        self.entries
            .lock()
            .unwrap()
            .get(token)
            .filter(|(time, _)| time.elapsed() < Duration::from_secs(30))
            .map(|(_, window)| window.clone())
            .ok_or_else(|| "Window ID is unknown or expired; call list_windows again".into())
    }
}

pub(super) fn foreground(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
) -> Result<Value, String> {
    request(
        id,
        s,
        &state.automation,
        state.automation.generation.load(Ordering::SeqCst),
        "foreground",
        None,
        None,
    )
}

pub(in crate::agent_mcp) fn assert_foreground(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    token: &str,
) -> Result<Value, String> {
    let expected = state.automation.windows.resolve(token)?;
    let current = foreground(id, s, state)?;
    if !model::same_window(&expected, &current["active_window"]) {
        return Err("Foreground window changed; no further batch actions were queued. Observe before retrying".into());
    }
    Ok(current["active_window"].clone())
}

pub(super) fn call(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    let generation = state.automation.generation.load(Ordering::SeqCst);
    let registry = &state.automation.windows;
    if name == "list_windows" {
        let result = request(id, s, &state.automation, generation, "windows", None, None)?;
        let windows = result["windows"]
            .as_array()
            .filter(|v| v.len() <= 128)
            .ok_or("Invalid remote window list")?;
        let matches: Vec<_> = windows
            .iter()
            .filter(|w| {
                ["title", "process_name"].iter().all(|key| {
                    args.get(*key).and_then(Value::as_str).map_or(true, |text| {
                        model::matches_text(
                            w[*key].as_str().unwrap_or(""),
                            text,
                            flag(args, "exact"),
                            true,
                        )
                    })
                })
            })
            .map(|w| registry.remember(w))
            .collect();
        return Ok(success(
            json!({"windows":matches,"truncated":result["truncated"],
            "coordinate_space":"desktop_physical_pixels","window_id_ttl_ms":30000}),
        ));
    }
    if name == "get_foreground_window" {
        let mut result = foreground(id, s, state)?;
        if result["active_window"].is_object() {
            result["active_window"] = registry.remember(&result["active_window"]);
        }
        result["coordinate_space"] = json!("desktop_physical_pixels");
        return Ok(success(result));
    }
    let target = registry.resolve(string(args, "window_id")?)?;
    let action = request(
        id,
        s,
        &state.automation,
        generation,
        "focus_window",
        Some(&target),
        None,
    )?;
    check(id, s, &state.automation, generation)?;
    let focused =
        action["focused"] == true && model::same_window(&target, &action["active_window"]);
    let mut result = success(
        json!({"focused":focused,"active_window":action["active_window"],
        "task_success":null,"guidance":if focused { "Window activation was observed. Inspect the target control before typing" }
        else { "Windows did not grant foreground activation. Use taskbar UIA or screenshot vision; do not type into the current window" }}),
    );
    if flag(args, "screenshot_after") {
        match fresh_shot(
            id,
            s,
            state,
            desktop::display(s, state, args),
            number(args, "timeout_ms", 3000),
        ) {
            Ok(shot) => {
                result = with_image(
                    {
                        let mut data = result["structuredContent"].clone();
                        data["screen"] = shot["structuredContent"].clone();
                        data
                    },
                    &shot,
                )
            }
            Err(e) => result["structuredContent"]["screenshot_error"] = json!(e),
        }
    }
    if !focused {
        result["isError"] = json!(true);
    }
    result["content"][0]["text"] = json!(result["structuredContent"].to_string());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn window_tokens_are_scoped_expiring_and_resettable() {
        let first = State::default();
        let second = State::default();
        let window = json!({"handle":"1","process_id":"2","process_started":"3","title":"Chat","class_name":"Window"});
        let public = first.remember(&window);
        let token = public["window_id"].as_str().unwrap();
        assert_eq!(first.resolve(token).unwrap(), window);
        assert!(second.resolve(token).is_err());
        assert_eq!(first.remember(&window)["window_id"], token);
        first.entries.lock().unwrap().get_mut(token).unwrap().0 =
            Instant::now() - Duration::from_secs(31);
        assert!(first.resolve(token).is_err());
        let token = first.remember(&window)["window_id"]
            .as_str()
            .unwrap()
            .to_string();
        first.clear();
        assert!(first.resolve(&token).is_err());
        assert_eq!(first.remember(&json!({"handle":"1"}))["focusable"], false);
    }
}
