//! Exercises real child-process pipes without accessing a desktop or injecting input.
use rustdesk_agent_mcp::local::ipc::{self, Client};
use serde_json::{json, Map};
use std::{
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

fn main() -> Result<(), String> {
    if std::env::args().nth(1).as_deref() == Some("--agent") {
        return ipc::run(|name, args, check| {
            if name == "keyboard_input" {
                loop {
                    check()?;
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            if args.get("oversized").is_some() {
                return Ok(json!("x".repeat(25 * 1024 * 1024)));
            }
            Ok(json!({"state":"ready","version":ipc::VERSION}))
        });
    }
    let mut command = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    command.arg("--agent");
    let client = Client::spawn(&mut command, &|| Ok(()))?;
    assert_eq!(
        client.call("status", &Map::new(), &|| Ok(()))?["state"],
        "ready"
    );
    assert!(client
        .call("arbitrary_script", &Map::new(), &|| Ok(()))
        .is_err());
    let args = json!({"oversized":true});
    let oversized = client.call(
        "screenshot",
        args.as_object().ok_or("Invalid fixture arguments")?,
        &|| Ok(()),
    );
    assert!(matches!(oversized, Err(ref e) if e.contains("24 MiB")));
    assert_eq!(
        client.call("status", &Map::new(), &|| Ok(()))?["state"],
        "ready"
    );
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = cancelled.clone();
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.store(true, Ordering::SeqCst);
    });
    let result = client.call("keyboard_input", &Map::new(), &|| {
        if cancelled.load(Ordering::SeqCst) {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(ref e) if e.contains("outcome may be unknown")));
    assert!(client.call("status", &Map::new(), &|| Ok(())).is_err());
    timer
        .join()
        .map_err(|_| "Fixture cancellation thread failed")?;
    drop(client);
    println!("Local IPC: real child round-trip, operation allowlist, output bound, cancellation and stop passed");
    Ok(())
}
