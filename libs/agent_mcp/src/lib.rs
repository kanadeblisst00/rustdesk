pub mod actions;
pub mod automation;
pub mod catalog;
mod errors;
pub mod events;
pub mod helper;
pub mod http;
pub mod pixels;
pub mod process;
pub mod queue;
pub mod workspace;

use serde_json::{json, Map, Value};
use std::sync::Arc;

pub const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub type ToolResult = Result<Value, String>;

/// The desktop owns authorization and sessions; the protocol never invents a device backend.
pub trait Backend: Send + Sync + 'static {
    fn token(&self) -> Option<String>;
    fn call(&self, name: &str, arguments: &Map<String, Value>) -> ToolResult;
}

pub struct Server {
    pub backend: Arc<dyn Backend>,
}

impl Server {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }

    pub fn dispatch(&self, message: Value) -> Option<Value> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str);
        if !message.is_object()
            || message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || !(id.is_null() || id.is_string() || id.is_i64() || id.is_u64())
            || message.get("params").is_some_and(|v| !v.is_object())
        {
            return Some(error(Value::Null, -32600, "Invalid JSON-RPC request"));
        }
        let Some(method) = method else {
            if message.get("id").is_some()
                && (message.get("result").is_some() ^ message.get("error").is_some())
            {
                return None;
            }
            return Some(error(id, -32600, "Missing method"));
        };
        // Notifications must never execute tools, including a tools/call without an id.
        message.get("id")?;
        let empty = json!({});
        let params = message.get("params").unwrap_or(&empty);
        let result = match method {
            "initialize" => {
                let Some(requested) = params.get("protocolVersion").and_then(Value::as_str) else {
                    return Some(error(id, -32602, "protocolVersion is required"));
                };
                let version = if VERSIONS.contains(&requested) {
                    requested
                } else {
                    VERSIONS[0]
                };
                json!({"protocolVersion":version,
                    "capabilities":{"tools":{"listChanged":false},"resources":{},"prompts":{}},
                    "serverInfo":{"name":"rustdesk-agent","version":env!("CARGO_PKG_VERSION")},
                    "instructions":catalog::GUIDE})
            }
            "ping" => json!({}),
            "tools/list" => json!({"tools":catalog::tools()}),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error(id, -32602, "Tool name is required"));
                };
                let Some(tool) = catalog::tools().into_iter().find(|t| t["name"] == name) else {
                    return Some(error(id, -32602, "Unknown tool"));
                };
                let args = params.get("arguments").unwrap_or(&empty);
                if let Err(e) = catalog::validate(&tool["inputSchema"], args, "arguments") {
                    return Some(error(id, -32602, &e));
                }
                let Some(args) = args.as_object() else {
                    return Some(error(id, -32602, "arguments must be an object"));
                };
                match self.backend.call(name, args) {
                    Ok(v) => v,
                    Err(e) => errors::tool_failure(e, args),
                }
            }
            "resources/list" => json!({"resources":[
                {"uri":"rustdesk://sessions","name":"sessions","mimeType":"application/json"},
                {"uri":"rustdesk://capabilities","name":"capabilities","mimeType":"application/json"}
            ]}),
            "resources/templates/list" => json!({"resourceTemplates":[]}),
            "resources/read" => {
                let (uri, name) = match params.get("uri").and_then(Value::as_str) {
                    Some("rustdesk://sessions") => ("rustdesk://sessions", "list_connections"),
                    Some("rustdesk://capabilities") => {
                        ("rustdesk://capabilities", "get_capabilities")
                    }
                    _ => return Some(error(id, -32002, "Unknown resource")),
                };
                match self.backend.call(name, &Map::new()) {
                    Ok(v) => json!({"contents":[{"uri":uri,"mimeType":"application/json",
                        "text":v.get("structuredContent").unwrap_or(&v).to_string()}]}),
                    Err(e) => return Some(error(id, -32603, &e)),
                }
            }
            "prompts/list" => json!({"prompts":[{"name":"remote_operator",
                "description":"Observe, act, and verify a RustDesk remote session"}]}),
            "prompts/get" => {
                if params.get("name").and_then(Value::as_str) != Some("remote_operator") {
                    return Some(error(id, -32602, "Unknown prompt"));
                }
                json!({"messages":[{"role":"user","content":{"type":"text","text":catalog::GUIDE}}]})
            }
            _ => return Some(error(id, -32601, "Method not found")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}

pub fn success(data: Value) -> Value {
    json!({"content":[{"type":"text","text":data.to_string()}],"structuredContent":data,"isError":false})
}

pub fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
