//! Test-only transport fixture. It never connects to or controls a real desktop.
use rustdesk_agent_mcp::{success, Backend, Server, ToolResult};
use serde_json::{json, Map, Value};
use std::sync::Arc;

struct Fixture;
impl Backend for Fixture {
    fn token(&self) -> Option<String> {
        Some("fixture-only-0123456789abcdef0123456789".into())
    }

    fn call(&self, name: &str, _: &Map<String, Value>) -> ToolResult {
        match name {
            "get_capabilities" => Ok(success(json!({"fixture":true}))),
            "list_connections" => Ok(success(json!({"connections":[]}))),
            _ => Err("Test fixture has no remote devices".into()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}/mcp", listener.local_addr()?);
    rustdesk_agent_mcp::http::serve(listener, Arc::new(Server::new(Arc::new(Fixture)))).await?;
    Ok(())
}
