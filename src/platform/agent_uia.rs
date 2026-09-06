use serde_json::Value;
use std::{process::Command, sync::Mutex, time::Duration};

static WORKER: Mutex<()> = Mutex::new(());

pub(crate) fn request(request: &Value, active: impl Fn() -> bool) -> Result<Value, String> {
    let _worker = WORKER.try_lock().map_err(|_| "Remote UIA is busy")?;
    let system_root = std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable")?;
    let powershell =
        std::path::Path::new(&system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut command = Command::new(powershell);
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Mta",
        "-Command",
        include_str!("agent_uia.ps1"),
    ]);
    let bytes = rustdesk_agent_mcp::helper::run_checked(
        &mut command,
        &serde_json::to_vec(request).map_err(|e| e.to_string())?,
        Duration::from_secs(8),
        active,
    )?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid Windows UIA helper response")?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.into());
    }
    Ok(value)
}
