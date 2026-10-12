use serde_json::{json, Map, Value};

pub(crate) fn tool_failure(name: &str, message: String, arguments: &Map<String, Value>) -> Value {
    let kind = match message.as_str() {
        "Unknown or closed session"
        | "Session transport is closed"
        | "Session transport is not ready"
        | "Not connected" => "session_disconnected",
        "Session is not authenticated yet" => "session_auth_required",
        "MCP is disabled" => "service_unavailable",
        text if text.starts_with("Connection changed") => "session_disconnected",
        text if text.starts_with("Remote process request timed out") => "remote_request_timeout",
        _ => "tool_error",
    };
    let stage = match kind {
        "session_disconnected" => "connection",
        "session_auth_required" => "authentication",
        "service_unavailable" => "service",
        "remote_request_timeout" => "remote_response",
        _ => "unknown",
    };
    // String backend errors do not prove whether a request reached the peer.
    json!({"content":[{"type":"text","text":message}],"isError":true,
    "structuredContent":{"error":{"kind":kind,"message":message,"tool":name,"stage":stage,
        "session":arguments.get("session"),"job_id":arguments.get("job_id"),
        "remote_job_state":"unknown","request_dispatched":null,"retryable":null,"request_replayed":false,
        "recovery":if arguments.contains_key("job_id") {
            "Query the SAME job_id before retrying. If disconnected, reconnect to the SAME device and OS identity. Do not start a replacement job automatically."
        } else {
            "Inspect the error and current state before retrying; this response does not prove that the operation was not executed."
        }}}})
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
            let result = tool_failure("run_process", message.into(), args.as_object().unwrap());
            assert_eq!(result["structuredContent"]["error"]["kind"], kind);
            assert_eq!(
                result["structuredContent"]["error"]["remote_job_state"],
                "unknown"
            );
            assert_eq!(result["content"][0]["text"], message);
            assert!(!result.to_string().contains("must-not-appear"));
        }
    }

    #[test]
    fn all_backend_errors_have_a_conservative_structured_result() {
        for message in [
            "Access denied (os error 5)",
            "Invalid job_id",
            "Remote process request timed out",
        ] {
            let args = json!({"session":"s","job_id":"j","env":[{"name":"SECRET","value":"hidden-value"}]});
            let result = tool_failure("run_process", message.into(), args.as_object().unwrap());
            let error = &result["structuredContent"]["error"];
            assert_eq!(result["isError"], true);
            assert_eq!(error["message"], result["content"][0]["text"]);
            assert_eq!(error["tool"], "run_process");
            assert!(error["kind"].is_string());
            assert!(error["stage"].is_string());
            assert!(error["request_dispatched"].is_null());
            assert!(error["retryable"].is_null());
            assert_eq!(error["request_replayed"], false);
            assert!(!result.to_string().contains("hidden-value"));
        }
    }
}
