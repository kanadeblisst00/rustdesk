use crate::catalog;
use serde_json::{json, Map, Value};

pub struct Action {
    pub name: String,
    pub arguments: Map<String, Value>,
    pub delay_ms: u64,
}

pub fn prepare(session: &str, args: &Map<String, Value>) -> Result<Vec<Action>, String> {
    let tools = catalog::tools();
    let batch = tools
        .iter()
        .find(|t| t["name"] == "execute_actions")
        .ok_or("Missing batch schema")?;
    catalog::validate(&batch["inputSchema"], &json!(args), "batch")?;
    let mut prepared = Vec::new();
    let mut total_delay = 0;
    for action in args["actions"].as_array().ok_or("Missing actions")? {
        let name = action["name"].as_str().ok_or("Invalid action")?;
        let mut arguments = action["arguments"]
            .as_object()
            .ok_or("Invalid action arguments")?
            .clone();
        if arguments.contains_key("session") {
            return Err("Batch actions cannot override session".into());
        }
        arguments.insert("session".into(), json!(session));
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .ok_or("Unknown action")?;
        catalog::validate(&tool["inputSchema"], &json!(arguments), "action")?;
        let delay_ms = action["delay_ms"].as_u64().unwrap_or(0);
        total_delay += delay_ms;
        if total_delay > 10000 {
            return Err("Batch delays exceed 10000 ms total".into());
        }
        prepared.push(Action {
            name: name.into(),
            arguments,
            delay_ms,
        });
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preflights_the_entire_batch_and_preserves_clipboard_order() {
        let batch = json!({"session":"s","expected_window":"w","screenshot_after":true,"actions":[
            {"name":"clipboard_set","arguments":{"text":"hello 中文"},"delay_ms":200},
            {"name":"keyboard_hotkey","arguments":{"keys":["Ctrl","v"]},"delay_ms":100}
        ]});
        let steps = prepare("s", batch.as_object().unwrap()).unwrap();
        assert_eq!(steps[0].name, "clipboard_set");
        assert_eq!(steps[0].arguments["text"], "hello 中文");
        assert_eq!(steps[0].delay_ms, 200);
        assert_eq!(steps[1].arguments["session"], "s");
        for invalid in [
            json!({"name":"disconnect_device","arguments":{}}),
            json!({"name":"mouse_click","arguments":{"x":1}}),
            json!({"name":"keyboard_input","arguments":{"session":"other","text":"bad"}}),
            json!({"name":"keyboard_input","arguments":{"text":"bad"},"delay_ms":2001}),
        ] {
            let mut invalid_batch = batch.clone();
            invalid_batch["actions"]
                .as_array_mut()
                .unwrap()
                .push(invalid);
            assert!(prepare("s", invalid_batch.as_object().unwrap()).is_err());
        }
        let batch = json!({"session":"s","actions":vec![json!({"name":"keyboard_input","arguments":{"text":"x"},"delay_ms":2000});6]});
        assert!(prepare("s", batch.as_object().unwrap())
            .err()
            .unwrap()
            .contains("total"));
    }
}
