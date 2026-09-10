use serde_json::{json, Value};

pub const WIRE_FIELD: u32 = 50002;
pub const WIRE_VERSION: &str = "rustdesk-process/1";
pub const MAX_WIRE_BYTES: usize = 1024 * 1024;

pub fn dependency_policy() -> Value {
    json!({
        "project_libraries":"agent_may_install_for_authorized_task",
        "virtual_environments":"agent_may_create_using_existing_tools",
        "conda_environment_python":"allowed_using_existing_conda",
        "missing_software":"notify_user_to_install_or_explicitly_authorize_agent",
        "software_examples":["standalone Python","Conda","Git","compiler","SDK","browser"],
        "preferred_scope":"isolated_project_environment",
        "verify_before_installing":true,
        "verify_after_installing":true,
        "enforcement":"agent_guidance_not_command_sandbox"
    })
}

pub fn tools() -> Vec<Value> {
    let id = json!({"type":"string","minLength":1,"maxLength":64});
    let mut result = Vec::new();
    for (name, description, read, properties, required) in [
        ("run_process", "Start a durable command on an authenticated terminal session. Both peers need an mcp build. shell defaults to none: executable/args are passed directly using OS argv conventions. Select shell cmd/powershell explicitly for Windows script syntax. Default timeout is one hour; maximum 24 hours. Supply a unique job_id and reuse it after uncertain replies; conflicting reuse fails. Returns starting/running, not command success. Logs and environment overrides are stored on the remote machine. Jobs survive connection closure.", false,
            json!({"job_id":id,"executable":{"type":"string","minLength":1,"maxLength":32768},"args":{"type":"array","maxItems":256,"items":{"type":"string","maxLength":32768}},"cwd":{"type":"string","minLength":1,"maxLength":32768},"env":{"type":"array","maxItems":128,"items":{"type":"object","properties":{"name":{"type":"string","minLength":1,"maxLength":256},"value":{"type":"string","maxLength":32768}},"required":["name","value"],"additionalProperties":false}},"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000},"max_log_bytes":{"type":"integer","minimum":1024,"maximum":268435456}}), vec!["job_id","executable","cwd"]),
        ("get_process_status", "Read durable command state, exit code, timestamps and log sizes. A stale worker reports unknown, never success; no PID-based automatic rerun or kill.", true, json!({"job_id":id}), vec!["job_id"]),
        ("wait_for_process", "Wait up to 10 seconds for job completion, state/phase change or new log bytes. Returns the durable job and bounded incremental stdout/stderr/setup pages; reuse each next_offset. A wait timeout does not terminate the job. Completed jobs can still have unread log pages (check eof). Survives session replacement when queried with the same job_id.", true, json!({"job_id":id,"timeout_ms":{"type":"integer","minimum":0,"maximum":10000},"stdout_offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"stderr_offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"setup_offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"after_state":{"type":"string","maxLength":64},"after_phase":{"type":"string","maxLength":64},"max_bytes":{"type":"integer","minimum":1,"maximum":32768},"encoding":{"type":"string","enum":["auto","utf-8","oem","cp936","base64"]}}), vec!["job_id"]),
        ("extend_process_timeout", "Extend a starting/running durable job's total timeout from its start (maximum 24 hours). Does not rerun or resume a terminated job. Query timeout_ms until the worker applies the request; an uncertain/late reply is not proof of extension. Default jobs already allow one hour. Expiry terminates the process tree; partial artifacts do not imply command success.", false, json!({"job_id":id,"timeout_ms":{"type":"integer","minimum":100,"maximum":86400000}}), vec!["job_id","timeout_ms"]),
        ("list_processes", "List this remote OS user's retained command jobs after reconnect. Does not return command arguments or environment secrets.", true, json!({}), vec![]),
        ("read_process_output", "Read stdout or stderr from disk by byte offset. data_base64 is authoritative for split/non-UTF-8 bytes. Output may still grow until the job is terminal.", true,
            json!({"job_id":id,"stream":{"type":"string","enum":["stdout","stderr"]},"offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"max_bytes":{"type":"integer","minimum":1,"maximum":65536}}), vec!["job_id","stream"]),
        ("cancel_process", "Request cancellation of a command and its process group/job object. Query status until terminal; requesting cancellation is not confirmation. Does not use a possibly recycled PID.", false, json!({"job_id":id}), vec!["job_id"]),
        ("remove_process", "Remove one completed command's retained state and logs. Active or unknown jobs cannot be removed through this tool.", false, json!({"job_id":id}), vec!["job_id"]),
        ("get_environment", "Inspect the authenticated terminal user's remote OS/architecture, CPU count, selected environment variables, disk space and executable paths. Does not execute version/import checks or install dependencies. Windows uses the authenticated terminal identity: its logon-token environment when available, otherwise the authorized terminal process environment. Standard Windows system tool directories are appended to PATH before explicit command overrides. GUI environment variables do not prove a usable interactive desktop.", true, json!({"path":{"type":"string","minLength":1,"maxLength":32768},"executables":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1,"maxLength":64}}}), vec![]),
    ] {
        let mut properties = properties.as_object().cloned().unwrap_or_default();
        if name == "run_process" {
            properties.insert("environment_script".into(), json!({"type":"object","properties":{"path":{"type":"string","minLength":1,"maxLength":4096},"args":{"type":"array","maxItems":32,"items":{"type":"string","maxLength":4096}},"timeout_ms":{"type":"integer","minimum":100,"maximum":120000}},"required":["path"],"additionalProperties":false,"description":"Windows only: run an absolute .bat/.cmd setup script (e.g. vcvars64.bat), capture its Unicode environment in memory, then launch executable/args normally. Setup inherits the authorized user environment; env/unset_env apply after capture. Setup has a separate timeout (default 120 seconds) and the main command keeps timeout_ms. Setup failure/cancellation never launches the command. Quotes, %, !, ^ and control characters in setup path/args are rejected rather than reinterpreted by CALL. Read setup output with stream:setup."}));
            properties.insert("unset_env".into(), json!({"type":"array","maxItems":128,"items":{"type":"string","minLength":1,"maxLength":256},"description":"Remove inherited variables. env values may be empty strings; empty and absent are different."}));
            properties.insert("shell".into(), json!({"type":"string","enum":["none","cmd","powershell"],"description":"Default none preserves direct argv. Windows cmd/powershell require executable to name that shell and exactly one args entry containing the script, without /c or -Command wrappers. Script syntax is interpreted by the selected shell."}));
        }
        if name == "read_process_output" {
            properties.insert("stream".into(), json!({"type":"string","enum":["stdout","stderr","setup"]}));
            properties.insert("encoding".into(), json!({"type":"string","enum":["auto","utf-8","oem","cp936","base64"],"description":"Default auto compares strict UTF-8 and Windows OEM; ambiguous bytes return text:null instead of guessing. Explicit cp936 selects GBK. Failed decoding returns text:null; byte offsets and data_base64 remain authoritative. A chunk may split a multibyte character."}));
            properties.insert("tail_lines".into(), json!({"type":"integer","minimum":1,"maximum":1000,"description":"Read the last N LF-delimited lines within max_bytes (default 64 KiB). Mutually exclusive with offset. truncated_start means the byte limit cut the requested tail; continue using byte offsets for full logs."}));
        }
        properties.insert("session".into(), json!({"type":"string","minLength":1,"maxLength":36}));
        let mut required = required;
        required.push("session");
        result.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read,"destructiveHint":!read,"openWorldHint":true}}));
    }
    result.push(json!({"name":"recover_processes","description":"Find retained jobs by device_id, using its authenticated terminal OS identity. By default opens a headless terminal connection or reconnects a disconnected transport once; authentication/2FA/approval is never bypassed. Does not submit/replay/cancel commands. Optional job_id returns its status and recent bounded logs. reconnect:false only uses an existing session. Returns connection and observation separately; unavailable remote state is never reported as job failure.","inputSchema":{"type":"object","properties":{"device_id":{"type":"string","minLength":1,"maxLength":256},"job_id":id,"reconnect":{"type":"boolean"},"force_relay":{"type":"boolean"},"timeout_ms":{"type":"integer","minimum":100,"maximum":30000}},"required":["device_id"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}}));
    result
}

pub fn is_tool(name: &str) -> bool {
    crate::workspace::is_tool(name)
        || matches!(
            name,
            "run_process"
                | "get_process_status"
                | "wait_for_process"
                | "extend_process_timeout"
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
    if operation == "read_process_output"
        && arguments.get("tail_lines").is_some()
        && arguments.get("offset").is_some()
    {
        return Err("tail_lines and offset are mutually exclusive".into());
    }
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
        if let Some(setup) = arguments.get("environment_script") {
            let path = setup["path"]
                .as_str()
                .ok_or("Missing environment_script.path")?;
            let lower = path.to_ascii_lowercase();
            if !(lower.ends_with(".bat") || lower.ends_with(".cmd")) {
                return Err("environment_script.path must name a .bat or .cmd file".into());
            }
            for value in std::iter::once(path).chain(
                setup["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            ) {
                if value
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '"' | '%' | '!' | '^'))
                {
                    return Err("environment_script path/args cannot contain quotes, %, !, ^ or control characters; use direct argv for literal program arguments".into());
                }
            }
        }
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
        if let Some(unset) = arguments["unset_env"].as_array() {
            for name in unset {
                let name = name.as_str().ok_or("Invalid environment name")?;
                if name.contains(['=', '\0']) || !names.insert(name.to_ascii_uppercase()) {
                    return Err("Invalid, duplicate or conflicting unset_env name".into());
                }
            }
        }
        if matches!(arguments["shell"].as_str(), Some("cmd" | "powershell")) {
            let shell = arguments["shell"].as_str().unwrap_or("");
            let executable = arguments["executable"].as_str().unwrap_or("");
            let basename = executable
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if basename != shell && basename != format!("{shell}.exe") {
                return Err(
                    "executable must name the selected cmd.exe or powershell.exe shell".into(),
                );
            }
            if arguments["args"]
                .as_array()
                .map_or(true, |args| args.len() != 1)
            {
                return Err("shell requires exactly one args entry containing the script".into());
            }
        }
    }
    Ok(())
}
