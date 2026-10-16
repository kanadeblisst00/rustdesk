//! Exercise the real durable worker without RustDesk's audio/video/UI dependencies.
#![allow(dead_code)]
extern crate self as hbb_common;
pub use {base64, libc, log};
#[cfg(test)]
use serde_json::{json, Value};
#[cfg(all(test, unix))]
use std::time::{Duration, Instant};
#[cfg(test)]
use rustdesk_agent_mcp::process as model;

mod config {
    pub struct Config;
    impl Config {
        pub fn get_home() -> std::path::PathBuf { std::env::temp_dir() }
    }
}
mod terminal_service {
    pub struct UserToken(usize);
    impl UserToken { pub fn as_raw(&self) -> usize { self.0 } }
}
#[path = "../../../src/agent_mcp/process/diagnostics.rs"]
mod diagnostics;
#[path = "../../../src/agent_mcp/process/capabilities.rs"]
mod capabilities;
#[path = "../../../src/agent_mcp/process/reconcile.rs"]
mod reconcile;
#[path = "../../../src/agent_mcp/process/retention.rs"]
mod retention;
#[path = "../../../src/agent_mcp/process/store.rs"]
mod store;
#[path = "../../../src/agent_mcp/process/platform.rs"]
mod platform;
#[path = "../../../src/agent_mcp/process/output.rs"]
mod output;
#[path = "../../../src/agent_mcp/process/head_tail.rs"]
mod head_tail;
#[path = "../../../src/agent_mcp/process/download.rs"]
mod download;
#[path = "../../../src/agent_mcp/process/preflight.rs"]
mod preflight;
#[path = "../../../src/agent_mcp/process/read_file.rs"]
mod read_file;
#[path = "../../../src/agent_mcp/process/presentation.rs"]
mod presentation;
#[path = "../../../src/agent_mcp/process/worker.rs"]
mod worker;
#[path = "../../../src/agent_mcp/process/wait.rs"]
mod wait;
#[path = "../../../src/agent_mcp/process/workspace.rs"]
mod workspace;
#[path = "../../../src/agent_mcp/process/workspace_location.rs"]
mod workspace_location;
#[path = "../../../src/agent_mcp/process/workspace_files.rs"]
mod workspace_files;
#[path = "../../../src/agent_mcp/process/artifact.rs"]
mod artifact;
#[path = "../../../src/agent_mcp/process/environment.rs"]
mod environment;
#[cfg(windows)]
#[path = "../../../src/agent_mcp/process/setup.rs"]
mod setup;
#[cfg(test)]
#[path = "../../../src/agent_mcp/process/tests.rs"]
mod tests;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let operation = args.next();
    if operation.as_deref() == Some(std::ffi::OsStr::new("--mcp-reconcile-job")) {
        let result = (|| {
            let path = std::path::PathBuf::from(args.next().ok_or("Missing job directory")?);
            if args.next().is_some() { return Err("Unexpected arguments".into()); }
            let id = path.file_name().and_then(|s| s.to_str()).ok_or("Invalid job directory")?;
            let store = store::Store::new(path.parent().ok_or("Missing store")?.to_owned())?;
            store.call("reconcile_process", &serde_json::json!({"job_id":id}))
        })();
        match result {
            Ok(value) => println!("{value}"),
            Err(error) => { eprintln!("{error}"); std::process::exit(1); }
        }
        return;
    }
    if operation.as_deref() != Some(std::ffi::OsStr::new("--mcp-process-worker")) {
        eprintln!("Expected --mcp-process-worker DIRECTORY");
        std::process::exit(2);
    }
    let result = match (args.next(), args.next()) {
        (Some(path), None) => worker::run(std::path::Path::new(&path)),
        _ => Err("Expected one job directory".into()),
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
