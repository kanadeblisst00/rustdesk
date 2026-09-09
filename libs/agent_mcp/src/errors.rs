use serde_json::{json, Map, Value};

pub(crate) fn tool_failure(message: String, arguments: &Map<String, Value>) -> Value {
    let kind = match message.as_str() {
        "Unknown or closed session"
        | "Session transport is closed"
        | "Session transport is not ready"
        | "Not connected" => Some("session_disconnected"),
        "Session is not authenticated yet" => Some("session_auth_required"),
        "MCP is disabled" => Some("service_unavailable"),
        text if text.starts_with("Connection changed") => Some("session_disconnected"),
        text if text.starts_with("Remote process request timed out") => {
            Some("remote_request_timeout")
        }
        _ => None,
    };
    let mut result = json!({"content":[{"type":"text","text":message}],"isError":true});
    if let Some(kind) = kind {
        result["structuredContent"] = json!({"error":{"kind":kind,"message":message,"session":arguments.get("session"),"job_id":arguments.get("job_id"),"remote_job_state":"unknown","request_replayed":false,"recovery":"Durable jobs and workspaces are retained across disconnects. Reconnect to the SAME device and OS identity, then query the SAME job_id or list_processes; do not start a replacement job automatically."}});
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_auth_and_controller_failures_are_distinct_without_claiming_job_success() {
        for (message, kind) in [
            ("Session transport is closed", "session_disconnected"),
            ("Session is not authenticated yet", "session_auth_required"),
            ("MCP is disabled", "service_unavailable"),
        ] {
            let args = json!({"session":"s","job_id":"j","password":"must-not-appear"});
            let result = tool_failure(message.into(), args.as_object().unwrap());
            assert_eq!(result["structuredContent"]["error"]["kind"], kind);
            assert_eq!(
                result["structuredContent"]["error"]["remote_job_state"],
                "unknown"
            );
            assert_eq!(result["content"][0]["text"], message);
            assert!(!result.to_string().contains("must-not-appear"));
        }
    }
}
