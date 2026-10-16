use serde_json::{json, Map, Value};
use std::time::Duration;

const PREFIX: &str = "RustDesk process RPC: ";

pub fn timeout(operation: &str, args: &Map<String, Value>) -> Duration {
    match operation {
        "wait_for_process" => Duration::from_millis(
            args.get("timeout_ms").and_then(Value::as_u64).unwrap_or(1000).min(60000) + 3000,
        ).max(Duration::from_secs(15)),
        "run_process" | "run_workspace_process" | "write_workspace_file"
        | "seal_workspace" | "get_artifact_manifest" | "publish_artifact"
        | "reconcile_process" | "probe_environment" | "begin_workspace_download"
        | "finish_workspace_download" => Duration::from_secs(30),
        _ => Duration::from_secs(15),
    }
}

pub fn failure(operation: &str, request_id: &str, args: &Map<String, Value>, delivery: &str,
    kind: &str, message: &str) -> String {
    let recovery = match operation {
        "run_process" | "run_workspace_process" => "Reconnect to the SAME device and OS identity and query the SAME job_id. Never submit a replacement job automatically.",
        "write_workspace_file" => "Query get_workspace_upload with the SAME workspace_id, path, total_bytes and sha256. Confirm next_offset or the final hash before resending the identical chunk. Never change the destination or replacement hash automatically.",
        "probe_environment" => "Query the version jobs using the SAME probe_id and executable order. Repeating the identical probe request returns the original jobs; do not create replacement IDs automatically.",
        "begin_workspace_download" | "finish_workspace_download" => "Query get_workspace_download with the SAME workspace_id and transfer_id. A served final chunk is not proof of client receipt. Keep the original source and checksum.",
        "remove_workspace_download" => "Query the SAME transfer_id. A missing record confirms removal; only an already completed receipt can be removed. The artifact is not deleted.",
        "publish_artifact" => "Inspect the SAME destination and expected hash, or repeat the identical conditional publish. Never bypass the replacement hash check.",
        "cancel_process" | "extend_process_timeout" | "reconcile_process" => "Query the SAME job_id to confirm the requested change; do not infer it from a missing reply.",
        "get_process_status" | "list_processes" | "wait_for_process" | "read_process_output"
        | "read_remote_file" | "get_environment" | "get_process_capabilities" | "get_workspace"
        | "get_workspace_upload" | "read_workspace_file" | "get_artifact_manifest"
        | "get_workspace_download" | "read_workspace_download" => "Reconnect to the SAME device and OS identity and repeat the read-only observation. A missing reply does not cancel a durable job.",
        _ => "Inspect the SAME resource after reconnect. Do not infer that the operation failed or automatically repeat a mutation after a missing reply.",
    };
    format!("{PREFIX}{}", json!({"kind":kind,"message":message,"operation":operation,
        "request_id":request_id,"job_id":args.get("job_id"),"workspace_id":args.get("workspace_id"),
        "probe_id":args.get("probe_id"),"transfer_id":args.get("transfer_id"),
        "delivery_state":delivery,"request_dispatched":delivery != "not_sent",
        "remote_received":if delivery == "remote_received" {json!(true)} else {Value::Null},
        "outcome":if delivery == "not_sent" {"not_started"} else {"unknown"},
        "retryable":false,"request_replayed":false,"recovery":recovery}))
}

pub(crate) fn details(message: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(message.strip_prefix(PREFIX)?).ok()?;
    let object = value.as_object()?;
    let mut result = Map::new();
    for key in ["kind", "message", "operation", "request_id", "job_id", "workspace_id", "probe_id", "transfer_id",
        "delivery_state", "request_dispatched", "remote_received", "outcome", "retryable",
        "request_replayed", "recovery"] {
        result.insert(key.into(), object.get(key)?.clone());
    }
    if let Some(compatibility) = object.get("read_compatibility") { result.insert("read_compatibility".into(),compatibility.clone()); }
    Some(Value::Object(result))
}

pub struct LogReadCompatibility {
    pub arguments: Map<String,Value>,
    include_base64: bool,
    evidence: Value,
}

impl LogReadCompatibility {
    fn new(operation: &str, args: &Map<String,Value>, evidence: Value) -> Option<Self> {
        if operation != "read_process_output" { return None; }
        let include_base64 = args.get("include_base64")?.as_bool()?;
        let mut arguments = args.clone();
        arguments.remove("include_base64");
        Some(Self {arguments,include_base64,evidence})
    }

    pub fn negotiated(peer: &Value, operation: &str, args: &Map<String,Value>) -> Option<Self> {
        if peer["negotiated"] != true || peer["protocol"] != super::process::WIRE_VERSION { return None; }
        let tool = peer["operations"].as_array()?.iter().find(|t|t["name"] == operation)?;
        let schema = &tool["inputSchema"];
        if schema["additionalProperties"] != false || schema["properties"].as_object()?.contains_key("include_base64") { return None; }
        Self::new(operation,args,json!({"trigger":"negotiated_peer_schema","read_reissue_attempted":false,"max_wire_read_requests":1}))
    }

    pub fn rejected(error: &str, operation: &str, args: &Map<String,Value>) -> Option<Self> {
        let detail = details(error)?;
        if detail["kind"] != "remote_operation_error" || detail["operation"] != operation
            || detail["remote_received"] != true || detail["message"] != "Unknown arguments.include_base64" { return None; }
        Self::new(operation,args,json!({"trigger":"explicit_remote_argument_rejection","read_reissue_attempted":true,
            "max_wire_read_requests":2,"original_request_id":detail["request_id"]}))
    }

    pub fn finish(self, result: Result<Value,String>) -> Result<Value,String> {
        let evidence = json!({"operation":"read_process_output","removed_arguments":["include_base64"],
            "requested_include_base64":self.include_base64,"local_projection":true,"command_replayed":false,
            "job_id":self.arguments.get("job_id"),"session":self.arguments.get("session"),"evidence":self.evidence});
        let result = result.and_then(|result| {
            let mut data = result.get("structuredContent").filter(|d|d.is_object()).cloned()
                .ok_or("Invalid remote log response after compatible read")?;
            if !self.include_base64 && data["text"].is_string() && data["encoding"] != "base64" && data["decoding_error"].is_null() {
                if let Some(object) = data.as_object_mut() { object.remove("data_base64"); }
            } else if !data["data_base64"].is_string() {
                return Err("Compatible log read did not preserve required raw bytes".into());
            }
            data["read_compatibility"] = evidence.clone();
            Ok(crate::success(data))
        });
        result.map_err(|error| {
            if let Some(mut detail) = details(&error) {
                detail["read_compatibility"] = evidence.clone();
                format!("{PREFIX}{detail}")
            } else { format!("{error}; read_compatibility: {evidence}") }
        })
    }
}

pub fn validate_peer(peer: &Value, operation: &str, args: &Map<String, Value>) -> Result<(), String> {
    if peer["protocol"] != super::process::WIRE_VERSION || peer["negotiated"] != true {
        return Err("Remote process capability negotiation is unavailable".into());
    }
    let tool = peer["operations"].as_array().and_then(|tools| tools.iter()
        .find(|tool| tool["name"] == operation))
        .ok_or_else(|| format!("Remote peer does not support {operation}; no operation was sent"))?;
    super::catalog::validate(&tool["inputSchema"], &Value::Object(args.clone()), "remote arguments")
        .map_err(|e| format!("{e} for {operation}; inspect the negotiated remote schema; no operation was sent"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_distinguish_queue_from_remote_acceptance_without_leaking_arguments() {
        let args = json!({"job_id":"same-job","env":[{"name":"KEY","value":"private"}]});
        for delivery in ["not_sent", "queued", "remote_received"] {
            let error = failure("run_process", "request", args.as_object().unwrap(), delivery,
                "remote_request_timeout", "Deadline expired");
            assert!(!error.contains("private"));
            let detail = details(&error).unwrap();
            assert_eq!(detail["request_dispatched"], delivery != "not_sent");
            assert_eq!(detail["remote_received"], if delivery == "remote_received" {json!(true)} else {Value::Null});
            assert_eq!(detail["job_id"], "same-job");
            assert_eq!(detail["request_id"], "request");
            assert_eq!(detail["retryable"], false);
        }
    }

    #[test]
    fn remote_schema_rejects_unsupported_parameters_and_limits_before_dispatch() {
        let peer = json!({"negotiated":true,"protocol":super::super::process::WIRE_VERSION,
            "operations":[{"name":"read_remote_file","inputSchema":{"type":"object",
                "properties":{"max_bytes":{"type":"integer","maximum":16}},"additionalProperties":false}}]});
        assert!(validate_peer(&peer, "read_remote_file", json!({"max_bytes":16}).as_object().unwrap()).is_ok());
        for args in [json!({"max_bytes":17}), json!({"include_base64":false})] {
            assert!(validate_peer(&peer, "read_remote_file", args.as_object().unwrap()).is_err());
        }
        assert!(validate_peer(&peer, "run_process", &Map::new()).is_err());
        assert!(validate_peer(&json!({}), "run_process", &Map::new()).is_err());
        assert_eq!(timeout("run_process", &Map::new()).as_secs(), 30);
        assert_eq!(timeout("wait_for_process", json!({"timeout_ms":60000}).as_object().unwrap()).as_secs(), 63);
    }

    #[test]
    fn compatible_log_reads_only_remove_the_rejected_presentation_option() {
        let args = json!({"session":"original-session","job_id":"original-job","stream":"stdout","offset":123,"max_bytes":8,"encoding":"utf-8","include_base64":false});
        let args = args.as_object().unwrap();
        let error = failure("read_process_output","first-request",args,"remote_received","remote_operation_error","Unknown arguments.include_base64");
        let fallback = LogReadCompatibility::rejected(&error,"read_process_output",args).unwrap();
        let mut expected = args.clone(); expected.remove("include_base64");
        assert_eq!(fallback.arguments,expected);
        let read = crate::success(json!({"job_id":"original-job","text":"ok","encoding":"utf-8","decoding_error":null,
            "data_base64":"b2s=","offset":123,"next_offset":125}));
        let result = fallback.finish(Ok(read)).unwrap();
        assert_eq!(result["structuredContent"]["next_offset"],125);
        assert!(result["structuredContent"].get("data_base64").is_none());
        assert_eq!(result["structuredContent"]["read_compatibility"]["evidence"]["original_request_id"],"first-request");
        let content: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(content,result["structuredContent"]);
        for (operation,kind,delivery,message) in [
            ("run_process","remote_operation_error","remote_received","Unknown arguments.include_base64"),
            ("read_process_output","remote_request_timeout","queued","Unknown arguments.include_base64"),
            ("read_process_output","remote_operation_error","remote_received","Unknown arguments.encoding"),
            ("read_process_output","remote_operation_error","remote_received","Unknown arguments.include_base64; retry"),
        ] {
            let error = failure(operation,"request",args,delivery,kind,message);
            assert!(LogReadCompatibility::rejected(&error,operation,args).is_none());
        }
        assert!(LogReadCompatibility::rejected("Unknown arguments.include_base64","read_process_output",args).is_none());
        assert!(LogReadCompatibility::rejected(&error,"read_process_output",&expected).is_none());
    }

    #[test]
    fn negotiated_log_projection_preserves_undecodable_bytes_and_failed_attempt_evidence() {
        let args = json!({"session":"s","job_id":"j","include_base64":false});
        let args = args.as_object().unwrap();
        let peer = json!({"negotiated":true,"protocol":super::super::process::WIRE_VERSION,"operations":[
            {"name":"read_process_output","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]});
        let fallback = LogReadCompatibility::negotiated(&peer,"read_process_output",args).unwrap();
        let read = crate::success(json!({"text":null,"encoding":"base64","data_base64":"/w==","next_offset":1}));
        let result = fallback.finish(Ok(read)).unwrap();
        assert_eq!(result["structuredContent"]["data_base64"],"/w==");
        assert_eq!(result["structuredContent"]["read_compatibility"]["evidence"]["read_reissue_attempted"],false);
        let fallback = LogReadCompatibility::negotiated(&peer,"read_process_output",args).unwrap();
        let error = failure("read_process_output","second-request",&fallback.arguments,"queued","remote_request_timeout","Timed out");
        let detail = details(&fallback.finish(Err(error)).unwrap_err()).unwrap();
        assert_eq!(detail["request_id"],"second-request");
        assert!(detail["read_compatibility"].is_object());
    }
}
