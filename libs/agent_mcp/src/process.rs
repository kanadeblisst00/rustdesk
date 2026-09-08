use serde_json::{json, Value};

pub const WIRE_FIELD: u32 = 50002;
pub const WIRE_VERSION: &str = "rustdesk-process/1";
pub const MAX_WIRE_BYTES: usize = 1024 * 1024;

pub fn tools() -> Vec<Value> {
    let id = json!({"type":"string","minLength":1,"maxLength":64});
    let mut result = Vec::new();
    for (name, description, read, properties, required) in [
        ("run_process", "Start a durable command on an authenticated terminal session. Both peers need an mcp build. executable/args are passed directly, never interpreted by a shell. Supply a unique job_id and reuse it after uncertain replies; conflicting reuse fails. Returns starting/running, not command success. Logs and environment overrides are stored on the remote machine. Jobs survive connection closure.", false,
            json!({"job_id":id,"executable":{"type":"string","minLength":1,"maxLength":32768},"args":{"type":"array","maxItems":256,"items":{"type":"string","maxLength":32768}},"cwd":{"type":"string","minLength":1,"maxLength":32768},"env":{"type":"array","maxItems":128,"items":{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":256},"value":{"type":"string","maxLength":32768}},"required":["name","value"],"additionalProperties":false}},"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000},"max_log_bytes":{"type":"integer","minimum":1024,"maximum":268435456}}), vec!["job_id","executable","cwd"]),
        ("get_process_status", "Read durable command state, exit code, timestamps and log sizes. A stale worker reports unknown, never success; no PID-based automatic rerun or kill.", true, json!({"job_id":id}), vec!["job_id"]),
        ("list_processes", "List this remote OS user's retained command jobs after reconnect. Does not return command arguments or environment secrets.", true, json!({}), vec![]),
        ("read_process_output", "Read stdout or stderr from disk by byte offset. data_base64 is authoritative for split/non-UTF-8 bytes. Output may still grow until the job is terminal.", true,
            json!({"job_id":id,"stream":{"type":"string","enum":["stdout","stderr"]},"offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"max_bytes":{"type":"integer","minimum":1,"maximum":65536}}), vec!["job_id","stream"]),
        ("cancel_process", "Request cancellation of a command and its process group/job object. Query status until terminal; requesting cancellation is not confirmation. Does not use a possibly recycled PID.", false, json!({"job_id":id}), vec!["job_id"]),
        ("remove_process", "Remove one completed command's retained state and logs. Active or unknown jobs cannot be removed through this tool.", false, json!({"job_id":id}), vec!["job_id"]),
        ("get_environment", "Inspect the authenticated terminal user's remote OS/architecture, CPU count, selected environment variables, disk space and executable paths. Does not execute version/import checks or install dependencies. Windows uses the authorized user's environment, not the service's PATH. GUI environment variables do not prove a usable interactive desktop.", true, json!({"path":{"type":"string","minLength":1,"maxLength":32768},"executables":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1,"maxLength":64}}}), vec![]),
    ] {
        let mut properties = properties.as_object().cloned().unwrap_or_default();
        properties.insert("session".into(), json!({"type":"string","minLength":1,"maxLength":36}));
        let mut required = required;
        required.push("session");
        result.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read,"destructiveHint":!read,"openWorldHint":true}}));
    }
    result
}

pub fn is_tool(name: &str) -> bool {
    crate::workspace::is_tool(name)
        || matches!(
            name,
            "run_process"
                | "get_process_status"
                | "list_processes"
                | "read_process_output"
                | "cancel_process"
                | "remove_process"
                | "get_environment"
        )
}

pub fn validate(operation: &str, arguments: &Value) -> Result<(), String> {
    let tool = tools()
        .into_iter()
        .chain(crate::workspace::tools())
        .find(|t| t["name"] == operation)
        .ok_or("Unknown process operation")?;
    crate::catalog::validate(&tool["inputSchema"], arguments, "arguments")?;
    if let Some(id) = arguments.get("workspace_id").and_then(Value::as_str) {
        if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("Invalid workspace_id".into());
        }
    }
    if let Some(id) = arguments.get("job_id").and_then(Value::as_str) {
        if !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err("job_id must contain only ASCII letters, digits, _ and -".into());
        }
    }
    if operation == "run_process" {
        for key in ["executable", "cwd"] {
            if arguments[key].as_str().is_some_and(|v| v.contains('\0')) {
                return Err(format!("{key} contains NUL"));
            }
        }
        if !std::path::Path::new(arguments["cwd"].as_str().unwrap_or("")).is_absolute() {
            return Err("cwd must be an absolute remote path".into());
        }
        if let Some(args) = arguments["args"].as_array() {
            if args
                .iter()
                .any(|a| a.as_str().is_some_and(|s| s.contains('\0')))
            {
                return Err("Argument contains NUL".into());
            }
        }
        let mut names = std::collections::HashSet::new();
        if let Some(env) = arguments["env"].as_array() {
            for entry in env {
                let name = entry["name"].as_str().ok_or("Invalid environment name")?;
                if name.contains(['=', '\0'])
                    || !names.insert(name.to_ascii_uppercase())
                    || entry["value"].as_str().is_some_and(|v| v.contains('\0'))
                {
                    return Err("Invalid or duplicate environment override".into());
                }
            }
        }
    }
    Ok(())
}
