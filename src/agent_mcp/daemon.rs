use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

fn overrides(values: &HashMap<String, String>) -> Result<HashMap<String, String>, String> {
    let mut options = HashMap::new();
    if let Some(token) = values.get("RUSTDESK_MCP_TOKEN") {
        if !rustdesk_agent_mcp::http::valid_token(token) {
            return Err(
                "RUSTDESK_MCP_TOKEN must contain 32–256 visible ASCII characters without spaces"
                    .into(),
            );
        }
        options.insert(TOKEN.into(), token.clone());
        options.insert(ENABLE.into(), "Y".into());
    }
    if let Some(address) = values.get("RUSTDESK_MCP_BIND_ADDRESS") {
        rustdesk_agent_mcp::http::parse_listen_address(
            address,
            if cfg!(feature = "mcp-isolated") {
                59941
            } else {
                59940
            },
        )?;
        options.insert(BIND_ADDRESS.into(), address.clone());
    }
    if let Some(devices) = values.get("RUSTDESK_MCP_DEVICES") {
        if devices.len() > 65536 || devices.chars().any(char::is_control) {
            return Err("Invalid RUSTDESK_MCP_DEVICES".into());
        }
        options.insert("agent-mcp-devices".into(), devices.clone());
    }
    if let Some(read_only) = values.get("RUSTDESK_MCP_READ_ONLY") {
        options.insert(
            "agent-mcp-read-only".into(),
            match read_only.as_str() {
                "1" | "true" | "Y" => "Y",
                "0" | "false" | "N" => "N",
                _ => return Err("RUSTDESK_MCP_READ_ONLY must be true or false".into()),
            }
            .into(),
        );
    }
    Ok(options)
}

pub(crate) fn requested() -> bool {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("--mcp-server") {
        return false;
    }
    let result = if args.len() == 1 || (args.len() == 2 && args[1] == "--check") {
        run(args.len() == 2)
    } else {
        Err("Usage: --mcp-server [--check]".into())
    };
    if let Err(e) = result {
        eprintln!("MCP background server: {e}");
        std::process::exit(2);
    }
    true
}

fn run(check: bool) -> Result<(), String> {
    let mut values = HashMap::new();
    for key in [
        "RUSTDESK_MCP_TOKEN",
        "RUSTDESK_MCP_BIND_ADDRESS",
        "RUSTDESK_MCP_DEVICES",
        "RUSTDESK_MCP_READ_ONLY",
    ] {
        match std::env::var(key) {
            Ok(value) => {
                values.insert(key.into(), value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(_) => return Err(format!("{key} must be UTF-8")),
        }
    }
    let options = overrides(&values)?;
    // Process-local policy overrides; never write the user's GUI configuration file.
    hbb_common::config::OVERWRITE_LOCAL_SETTINGS
        .write()
        .unwrap()
        .extend(options);
    if !enabled() || !rustdesk_agent_mcp::http::valid_token(&LocalConfig::get_option(TOKEN)) {
        return Err(
            "Provide RUSTDESK_MCP_TOKEN, or enable MCP with a valid saved token before starting"
                .into(),
        );
    }
    let address = listen_address()?;
    let endpoint = rustdesk_agent_mcp::http::client_endpoint(address);
    if check {
        println!(
            "{}",
            json!({"configuration_valid":true,"endpoint":endpoint,"listener_started":false,"headless_required":true,"environment_overrides_are_persistent":false})
        );
        return Ok(());
    }
    ACTIVE.store(true, Ordering::SeqCst);
    super::start();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = STATUS.lock().unwrap().clone();
        if status.starts_with("Listening on ") {
            break;
        }
        if status.starts_with("MCP ") {
            return Err(status);
        }
        if Instant::now() >= deadline {
            return Err("Listener did not start within 10 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    println!(
        "{}",
        json!({"state":"listening","endpoint":endpoint,"headless_required":true})
    );
    while enabled() && !super::lifecycle::stopping() {
        std::thread::sleep(Duration::from_millis(250));
    }
    super::lifecycle::shutdown();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_configuration_without_exposing_token_values() {
        let values = HashMap::from([("RUSTDESK_MCP_TOKEN".into(), "invalid-secret".into())]);
        let error = overrides(&values).unwrap_err();
        assert!(!error.contains("invalid-secret"));
        for address in ["localhost:59940", "[::1]:59940", "127.0.0.1:0"] {
            assert!(overrides(&HashMap::from([(
                "RUSTDESK_MCP_BIND_ADDRESS".into(),
                address.into()
            )]))
            .is_err());
        }
        let values = HashMap::from([
            ("RUSTDESK_MCP_TOKEN".into(), "a".repeat(64)),
            ("RUSTDESK_MCP_BIND_ADDRESS".into(), "127.0.0.1:59940".into()),
            ("RUSTDESK_MCP_DEVICES".into(), "one,two".into()),
            ("RUSTDESK_MCP_READ_ONLY".into(), "true".into()),
        ]);
        let options = overrides(&values).unwrap();
        assert_eq!(options[ENABLE], "Y");
        assert_eq!(options["agent-mcp-read-only"], "Y");
        assert_eq!(options["agent-mcp-devices"], "one,two");
        assert_eq!(options.len(), 5);
    }

    #[test]
    fn listener_runs_without_flutter_and_requires_headless_connections() {
        const CHILD: &str = "RUSTDESK_MCP_DAEMON_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap().to_string();
            drop(listener);
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "agent_mcp::daemon::tests::listener_runs_without_flutter_and_requires_headless_connections", "--test-threads=1"])
                .env(CHILD, "1")
                .env("RUSTDESK_MCP_TOKEN", "test-token-".repeat(8))
                .env("RUSTDESK_MCP_BIND_ADDRESS", address)
                .env("RUSTDESK_MCP_DEVICES", "unit-device")
                .env("RUSTDESK_MCP_READ_ONLY", "false")
                .output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        *hbb_common::config::APP_NAME.write().unwrap() =
            format!("RustDeskMCPDaemonTest-{}", uuid::Uuid::new_v4());
        let thread = std::thread::spawn(|| run(false));
        let address = std::env::var("RUSTDESK_MCP_BIND_ADDRESS").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if std::net::TcpStream::connect(&address).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "MCP listener did not bind");
            std::thread::sleep(Duration::from_millis(20));
        }
        let request = |tool: &str, args: Value, token: &str| {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(&address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}}).to_string();
            write!(stream, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            let mut output = String::new();
            stream.read_to_string(&mut output).unwrap();
            output
        };
        let token = std::env::var("RUSTDESK_MCP_TOKEN").unwrap();
        let capabilities = request("get_capabilities", json!({}), &token);
        assert!(capabilities.starts_with("HTTP/1.1 200"), "{capabilities}");
        assert!(capabilities.contains("\"background_controller\":true"));
        assert!(capabilities.contains("\"headless_required\":true"));
        let wrong = request("get_capabilities", json!({}), "wrong");
        assert!(wrong.starts_with("HTTP/1.1 401"));
        let visible = request("connect_device", json!({"device_id":"unit-device"}), &token);
        assert!(visible.contains("Background MCP requires"), "{visible}");
        hbb_common::config::OVERWRITE_LOCAL_SETTINGS
            .write()
            .unwrap()
            .insert(ENABLE.into(), "N".into());
        thread.join().unwrap().unwrap();
    }
}
