use serde_json::{json, Map, Value};

pub const MAX_NODES: usize = 512;
pub const WIRE_FIELD: u32 = 50001;
pub const WIRE_VERSION: &str = "rustdesk-uia/1";
pub const MAX_WIRE_BYTES: usize = 1024 * 1024;

pub fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "get_ui_tree"
            | "get_ui_state"
            | "find_ui_element"
            | "find_element"
            | "invoke_ui_element"
            | "set_ui_value"
            | "get_screen_text"
            | "find_text"
            | "click_text"
            | "list_windows"
            | "get_foreground_window"
            | "focus_window"
    )
}

pub fn matches_text(candidate: &str, text: &str, exact: bool, ignore_case: bool) -> bool {
    let (candidate, text) = if ignore_case {
        (candidate.to_lowercase(), text.to_lowercase())
    } else {
        (candidate.to_owned(), text.to_owned())
    };
    if exact {
        candidate == text
    } else {
        candidate.contains(&text)
    }
}

pub fn find(elements: &[Value], args: &Map<String, Value>) -> Vec<Value> {
    elements
        .iter()
        .filter(|e| {
            let text_matches = args
                .get("text")
                .and_then(Value::as_str)
                .map_or(true, |text| {
                    matches_text(
                        e.get("name")
                            .or_else(|| e.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        text,
                        args.get("exact").and_then(Value::as_bool).unwrap_or(false),
                        args.get("ignore_case")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    )
                });
            text_matches
                && ["automation_id", "control_type"]
                    .iter()
                    .all(|key| args.get(*key).map_or(true, |v| e.get(*key) == Some(v)))
        })
        .cloned()
        .collect()
}

pub fn choose(matches: &[Value], index: Option<u64>) -> Result<&Value, String> {
    if matches.is_empty() {
        return Err("Target not found; inspect the returned screenshot with a vision model".into());
    }
    if index.is_none() && matches.len() != 1 {
        return Err(format!("Ambiguous target: {} matches; specify match_index after inspecting find_ui_element/find_text", matches.len()));
    }
    matches
        .get(index.unwrap_or(0) as usize)
        .ok_or_else(|| "match_index is out of range".into())
}

pub fn validate_request(value: &Value) -> Result<(), String> {
    let request = value.as_object().ok_or("Invalid UIA request")?;
    if request.get("protocol").and_then(Value::as_str) != Some(WIRE_VERSION)
        || request
            .get("id")
            .and_then(Value::as_str)
            .map_or(true, |s| s.is_empty() || s.len() > 64)
        || request
            .keys()
            .any(|key| !["protocol", "id", "operation", "element", "value"].contains(&key.as_str()))
    {
        return Err("Invalid UIA envelope".into());
    }
    match request.get("operation").and_then(Value::as_str) {
        Some("tree" | "taskbar_tree" | "capabilities" | "windows" | "foreground") => Ok(()),
        Some("focus_window") => validate_window(&request["element"]),
        Some("invoke" | "toggle" | "set_value") => {
            let element = request.get("element").ok_or("Missing UIA element")?;
            for key in ["element_id", "name", "automation_id", "control_type"] {
                if element
                    .get(key)
                    .and_then(Value::as_str)
                    .map_or(true, |s| s.len() > 4096)
                {
                    return Err(format!("Invalid UIA element {key}"));
                }
            }
            if element["element_id"].as_str().map_or(true, str::is_empty) {
                return Err("Empty UIA element ID".into());
            }
            if element
                .get("scope")
                .is_some_and(|v| !matches!(v.as_str(), Some("foreground_window" | "taskbar")))
            {
                return Err("Invalid UIA scope".into());
            }
            if request["operation"] == "set_value"
                && request
                    .get("value")
                    .and_then(Value::as_str)
                    .map_or(true, |s| s.len() > 65536)
            {
                return Err("Invalid UIA value".into());
            }
            Ok(())
        }
        _ => Err("Unknown UIA operation".into()),
    }
}

pub fn validate_window(window: &Value) -> Result<(), String> {
    for key in [
        "handle",
        "process_id",
        "process_started",
        "title",
        "class_name",
    ] {
        let value = window
            .get(key)
            .and_then(Value::as_str)
            .ok_or("Invalid window identity")?;
        if value.len() > 4096 || (key != "title" && value.is_empty()) {
            return Err("Invalid window identity".into());
        }
    }
    if window["handle"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        == 0
        || window["process_id"]
            .as_str()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
            == 0
        || window["process_started"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            == 0
    {
        return Err("Invalid window identity".into());
    }
    Ok(())
}

pub fn same_window(expected: &Value, current: &Value) -> bool {
    validate_window(expected).is_ok()
        && validate_window(current).is_ok()
        && ["handle", "process_id", "process_started", "class_name"]
            .iter()
            .all(|key| expected[*key] == current[*key])
}

/// Clip physical desktop rectangles to the selected display, retaining original coordinates.
pub fn on_display(tree: &mut Value, origin: (i32, i32), size: (i32, i32)) {
    if let Some(elements) = tree.get_mut("elements").and_then(Value::as_array_mut) {
        elements.retain_mut(|e| {
            let Some(bounds) = e.get("bounds") else {
                return false;
            };
            let read = |key| {
                bounds
                    .get(key)
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite())
            };
            let (Some(x), Some(y), Some(w), Some(h)) =
                (read("x"), read("y"), read("width"), read("height"))
            else {
                return false;
            };
            let left = (x - origin.0 as f64).max(0.0);
            let top = (y - origin.1 as f64).max(0.0);
            let right = (x + w - origin.0 as f64).min(size.0 as f64);
            let bottom = (y + h - origin.1 as f64).min(size.1 as f64);
            if right <= left || bottom <= top {
                return false;
            }
            e["desktop_bounds"] = bounds.clone();
            e["bounds"] = json!({"x":left,"y":top,"width":right-left,"height":bottom-top});
            true
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_matching_and_ambiguity() {
        let elements = vec![
            json!({"name":"保存 Save","control_type":"Button"}),
            json!({"name":"另存为 Save","control_type":"Button"}),
        ];
        let args = json!({"text":"save","control_type":"Button"});
        let found = find(&elements, args.as_object().unwrap());
        assert_eq!(found.len(), 2);
        assert!(choose(&found, None).is_err());
        assert_eq!(choose(&found, Some(1)).unwrap()["name"], "另存为 Save");
        assert!(choose(&found, Some(2)).is_err());
        assert!(matches_text("保存", "保存", true, true));
        assert!(!matches_text("Save as", "Save", true, true));
        assert!(find(
            &elements,
            json!({"automation_id":"missing"}).as_object().unwrap()
        )
        .is_empty());
    }
    #[test]
    fn negative_monitor_origin_and_clipping() {
        let mut tree = json!({"elements":[{"bounds":{"x":-2000,"y":20,"width":200,"height":50}}, {"bounds":{"x":0,"y":0,"width":100,"height":100}}]});
        on_display(&mut tree, (-1920, 0), (1920, 1080));
        assert_eq!(tree["elements"].as_array().unwrap().len(), 1);
        assert_eq!(tree["elements"][0]["bounds"]["x"], 0.0);
        assert_eq!(tree["elements"][0]["bounds"]["width"], 120.0);
        assert_eq!(tree["elements"][0]["desktop_bounds"]["x"], -2000);
    }
    #[test]
    fn rejects_unknown_operations_and_missing_targets() {
        assert!(
            validate_request(&json!({"protocol":WIRE_VERSION,"id":"1","operation":"tree"})).is_ok()
        );
        for op in ["shell", "invoke", "set_value"] {
            assert!(
                validate_request(&json!({"protocol":WIRE_VERSION,"id":"1","operation":op}))
                    .is_err()
            );
        }
        assert!(validate_request(
            &json!({"protocol":WIRE_VERSION,"id":"1","operation":"tree","script":"bad"})
        )
        .is_err());
    }

    #[test]
    fn window_requests_require_bounded_identity_and_detect_handle_reuse() {
        let window = json!({"handle":"123","process_id":"5","process_started":"678","class_name":"Chat","title":"Chat"});
        assert!(validate_request(
            &json!({"protocol":WIRE_VERSION,"id":"1","operation":"focus_window","element":window})
        )
        .is_ok());
        assert!(validate_request(
            &json!({"protocol":WIRE_VERSION,"id":"1","operation":"taskbar_tree"})
        )
        .is_ok());
        for key in ["handle", "process_id", "process_started", "class_name"] {
            let mut changed = window.clone();
            changed[key] = json!("999");
            assert!(!same_window(&window, &changed));
        }
        let mut renamed = window.clone();
        renamed["title"] = json!("Chat - New message");
        assert!(same_window(&window, &renamed));
        for bad in [
            json!({"handle":"123"}),
            json!({"handle":"-1","process_id":"5","process_started":"678","class_name":"Chat","title":""}),
        ] {
            assert!(validate_window(&bad).is_err());
        }
    }
}
