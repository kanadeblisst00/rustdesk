use super::super::*;

fn can_reconnect(details: &Value, transport_closed: bool) -> bool {
    transport_closed
        && details["connected"] != true
        && details["needs_password"] != true
        && !matches!(
            details["authentication"].as_str(),
            Some(
                "two_factor_required"
                    | "os_login_required"
                    | "os_login_and_password_required"
                    | "waiting_remote_approval"
            )
        )
}

pub(in super::super) fn call(args: &Map<String, Value>) -> ToolResult {
    let arguments = Value::Object(args.clone());
    rustdesk_agent_mcp::process::validate("recover_processes", &arguments)?;
    let device = string(args, "device_id")?;
    if !session::allowed(device) {
        return Err("Device is not in the MCP allowlist".into());
    }
    let reconnect = args
        .get("reconnect")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let existing = session::list()
        .into_iter()
        .find(|info| info["device_id"] == device && info["kind"] == "terminal");
    let connection = if let Some(info) = existing {
        let queue = connection_queue::get(device, "terminal");
        let _guard = queue.acquire(Duration::from_secs(30), writable)?;
        let id = SessionID::parse_str(info["session"].as_str().ok_or("Missing terminal session")?)
            .map_err(|e| e.to_string())?;
        let s = session::get(id)?;
        let info = session::info(id, &s);
        let closed = s
            .sender
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|tx| tx.is_closed());
        if reconnect && can_reconnect(&info, closed) {
            s.reconnect(flag(args, "force_relay"));
        }
        if reconnect && info["connected"] != true {
            auth::wait_for_session(id, &s, args)?["structuredContent"].clone()
        } else {
            info
        }
    } else if reconnect {
        let connect = json!({"device_id":device,"kind":"terminal","headless":true,"force_relay":flag(args,"force_relay"),"timeout_ms":number(args,"timeout_ms",12000)});
        session::connect(connect.as_object().ok_or("Invalid recovery connection")?)?
            ["structuredContent"]
            .clone()
    } else {
        json!({"device_id":device,"kind":"terminal","connected":false,"authentication":"connection_required"})
    };
    let job = args.get("job_id").and_then(Value::as_str);
    let result = observe(device, connection, job, |operation, query| {
        let id = SessionID::parse_str(
            query["session"]
                .as_str()
                .ok_or("Missing terminal session")?,
        )
        .map_err(|e| e.to_string())?;
        let s = session::get(id)?;
        let state = track(id, false)?;
        let response = super::call(
            id,
            &s,
            &state,
            operation,
            query.as_object().ok_or("Invalid recovery query")?,
        )?;
        Ok(response["structuredContent"].clone())
    });
    Ok(success(result))
}

fn observe(
    device: &str,
    connection: Value,
    job: Option<&str>,
    mut invoke: impl FnMut(&str, Value) -> Result<Value, String>,
) -> Value {
    let mut result = json!({"device_id":device,"connection":connection,"request_replayed":false,"identity_scope":"current authenticated terminal OS user; use the original job's OS credentials"});
    if connection["connected"] != true {
        result["recovery_state"] = json!("connection_required");
        result["jobs"] = Value::Null;
        return result;
    }
    let session = &connection["session"];
    if let Some(job) = job {
        match invoke(
            "get_process_status",
            json!({"session":session,"job_id":job}),
        ) {
            Ok(state) => {
                result["recovery_state"] = json!("observed");
                let mut query =
                    json!({"session":session,"job_id":job,"timeout_ms":0,"max_bytes":8192});
                for stream in ["stdout", "stderr", "setup"] {
                    query[format!("{stream}_offset")] = json!(state[format!("{stream}_bytes")]
                        .as_u64()
                        .unwrap_or(0)
                        .saturating_sub(8192));
                }
                result["job"] = state;
                match invoke("wait_for_process", query) {
                    Ok(recent) => {
                        result["job"] = recent["job"].clone();
                        result["recent_output"] = recent["output"].clone();
                    }
                    Err(error) => result["output_error"] = json!(error),
                }
            }
            Err(error) => {
                result["recovery_state"] = json!("unavailable");
                result["observation_error"] = json!(error);
                result["job"] = Value::Null;
            }
        }
    } else {
        match invoke("list_processes", json!({"session":session})) {
            Ok(jobs) => {
                result["recovery_state"] = json!("observed");
                result["jobs"] = jobs["jobs"].clone();
            }
            Err(error) => {
                result["recovery_state"] = json!("unavailable");
                result["observation_error"] = json!(error);
                result["jobs"] = Value::Null;
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_recovery_obeys_controller_policy_without_creating_connections() {
        const CHILD: &str = "RUSTDESK_MCP_RECOVERY_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "agent_mcp::process::recovery::tests::device_recovery_obeys_controller_policy_without_creating_connections", "--test-threads=1"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }
        *hbb_common::config::APP_NAME.write().unwrap() =
            format!("RustDeskMcpRecoveryTest-{}", uuid::Uuid::new_v4());
        let set = |key: &str, value: &str| {
            hbb_common::config::OVERWRITE_LOCAL_SETTINGS
                .write()
                .unwrap()
                .insert(key.into(), value.into());
        };
        set(ENABLE, "Y");
        set("agent-mcp-devices", "allowed");
        set("agent-mcp-read-only", "Y");
        let backend = DesktopBackend {
            address: ADDRESS.parse().unwrap(),
        };
        let args = json!({"device_id":"allowed","reconnect":false});
        assert!(backend
            .call("recover_processes", args.as_object().unwrap())
            .is_err());
        set("agent-mcp-read-only", "N");
        let denied = json!({"device_id":"denied","reconnect":false});
        assert!(backend
            .call("recover_processes", denied.as_object().unwrap())
            .unwrap_err()
            .contains("allowlist"));
        let result = backend
            .call("recover_processes", args.as_object().unwrap())
            .unwrap();
        assert_eq!(
            result["structuredContent"]["recovery_state"],
            "connection_required"
        );
        assert!(session::list().is_empty());
    }

    #[test]
    fn recovery_never_replays_jobs_and_keeps_status_when_log_read_disconnects() {
        let connection = json!({"session":"replacement","connected":true});
        let mut calls = Vec::new();
        let result = observe("device", connection, Some("original"), |operation, args| {
            calls.push(operation.to_owned());
            assert_eq!(args["job_id"], "original");
            assert_eq!(args["session"], "replacement");
            if operation == "get_process_status" {
                Ok(json!({"state":"running","stdout_bytes":20000,"stderr_bytes":12}))
            } else {
                assert_eq!(operation, "wait_for_process");
                assert_eq!(args["stdout_offset"], 11808);
                assert_eq!(args["stderr_offset"], 0);
                Err("Connection changed".into())
            }
        });
        assert_eq!(calls, ["get_process_status", "wait_for_process"]);
        assert_eq!(result["job"]["state"], "running");
        assert_eq!(result["output_error"], "Connection changed");
        assert_eq!(result["request_replayed"], false);
        let unavailable = observe("device", json!({"connected":true}), None, |op, _| {
            assert_eq!(op, "list_processes");
            Err("Not connected".into())
        });
        assert!(unavailable["jobs"].is_null());
        assert_eq!(unavailable["recovery_state"], "unavailable");
    }

    #[test]
    fn recovery_keeps_authentication_challenges_and_live_transports() {
        for details in [
            json!({"connected":true}),
            json!({"needs_password":true}),
            json!({"authentication":"two_factor_required"}),
            json!({"authentication":"os_login_required"}),
        ] {
            assert!(!can_reconnect(&details, true));
        }
        assert!(!can_reconnect(&json!({"connected":false}), false));
        assert!(can_reconnect(&json!({"connected":false}), true));
        let result = observe(
            "device",
            json!({"connected":false,"authentication":"two_factor_required"}),
            None,
            |_, _| panic!("must not query before authentication"),
        );
        assert_eq!(
            result["connection"]["authentication"],
            "two_factor_required"
        );
    }
}
