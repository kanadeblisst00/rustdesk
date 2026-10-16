use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;

fn executable_hash() -> Result<String, String> {
    let mut file = std::fs::File::open(std::env::current_exe().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if count == 0 { break; }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn observe() -> Value {
    let hash = executable_hash();
    let mut operations: Vec<_> = rustdesk_agent_mcp::process::tools().into_iter()
        .filter(|t| t["name"] != "recover_processes")
        .chain(rustdesk_agent_mcp::workspace::tools()).collect();
    if !cfg!(windows) {
        operations.retain(|tool| tool["name"] != "reconcile_process");
        for tool in &mut operations {
            if let Some(encoding) = tool["inputSchema"]["properties"].get_mut("encoding") {
                encoding["enum"] = json!(["auto","utf-8","base64"]);
            }
            if matches!(tool["name"].as_str(), Some("run_process" | "run_workspace_process")) {
                if let Some(properties) = tool["inputSchema"]["properties"].as_object_mut() {
                    properties.remove("environment_script");
                    if let Some(shell) = properties.get_mut("shell") {
                        shell["enum"] = json!(["none"]);
                    }
                }
            }
        }
    }
    json!({"negotiated":true,"protocol":rustdesk_agent_mcp::process::WIRE_VERSION,
        "capability_revision":1,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "package_version":env!("CARGO_PKG_VERSION"),"build_commit":option_env!("RUSTDESK_MCP_BUILD_COMMIT"),
        "executable_sha256":hash.as_ref().ok(),"executable_hash_error":hash.as_ref().err(),
        "executable_hash_scope":"on-disk current_exe file at this observation; loaded image is not attested",
        "build_commit_source":"optional compile-time RUSTDESK_MCP_BUILD_COMMIT; null means unavailable",
        "receipts":true,"max_wire_bytes":rustdesk_agent_mcp::process::MAX_WIRE_BYTES,
        "operations":operations,"unknown_reconciliation":if cfg!(windows) {"process_identity_and_named_job"} else {"unavailable"}})
}
