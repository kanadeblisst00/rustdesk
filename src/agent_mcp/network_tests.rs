use super::*;

#[test]
fn shutdown_releases_listener_and_incomplete_http_connections() {
    const CHILD: &str = "RUSTDESK_MCP_SHUTDOWN_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_mcp::network_tests::shutdown_releases_listener_and_incomplete_http_connections",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::io::{Read, Write};
    *hbb_common::config::APP_NAME.write().unwrap() =
        format!("RustDeskMcpShutdownTest-{}", uuid::Uuid::new_v4());
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = probe.local_addr().unwrap();
    drop(probe);
    hbb_common::config::OVERWRITE_LOCAL_SETTINGS
        .write()
        .unwrap()
        .extend([
            (ENABLE.into(), "Y".into()),
            (TOKEN.into(), "a".repeat(64)),
            (BIND_ADDRESS.into(), address.to_string()),
        ]);
    start();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut client = loop {
        if let Ok(client) = std::net::TcpStream::connect(address) {
            break client;
        }
        assert!(Instant::now() < deadline, "MCP listener did not start");
        std::thread::sleep(Duration::from_millis(20));
    };
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        client,
        "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: 1000\r\n\r\n{{"
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = Instant::now();
    lifecycle::shutdown();
    assert!(before.elapsed() < Duration::from_secs(3));
    assert_eq!(*STATUS.lock().unwrap(), "Stopped");
    assert!(DesktopBackend {
        address: address.to_string().parse().unwrap()
    }
    .token()
    .is_none());
    let mut bytes = [0; 1];
    match client.read(&mut bytes) {
        Ok(0) => {}
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) => {}
        result => panic!("MCP connection remained open: {result:?}"),
    }
    drop(client);
    let replacement = std::net::TcpListener::bind(address).unwrap();
    lifecycle::shutdown();
    start();
    assert_eq!(*STATUS.lock().unwrap(), "Stopped");
    drop(replacement);
}

#[test]
fn address_changes_retire_old_backend_and_tokens_rotate_without_rebinding() {
    if std::env::var_os("RUSTDESK_MCP_NETWORK_TEST_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_mcp::network_tests::address_changes_retire_old_backend_and_tokens_rotate_without_rebinding",
                "--test-threads=1",
            ])
            .env("RUSTDESK_MCP_NETWORK_TEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    // These process-wide overrides must not load or persist the user's MCP configuration.
    *hbb_common::config::APP_NAME.write().unwrap() =
        format!("RustDeskMcpNetworkTest-{}", uuid::Uuid::new_v4());
    let set = |key: &str, value: &str| {
        hbb_common::config::OVERWRITE_LOCAL_SETTINGS
            .write()
            .unwrap()
            .insert(key.into(), value.into());
    };
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    set(ENABLE, "Y");
    set(TOKEN, &first);
    set(BIND_ADDRESS, "");
    let default = listen_address().unwrap();
    assert_eq!(default.to_string(), ADDRESS);
    let backend = DesktopBackend { address: default };
    assert_eq!(backend.token().as_deref(), Some(first.as_str()));
    set(BIND_ADDRESS, "0.0.0.0:60000");
    assert_eq!(backend.token(), None);
    let lan = DesktopBackend {
        address: listen_address().unwrap(),
    };
    assert_eq!(lan.token().as_deref(), Some(first.as_str()));
    assert_eq!(
        local_option("agent-mcp-endpoint").unwrap(),
        "http://127.0.0.1:60000/mcp"
    );
    set(TOKEN, &second);
    assert_eq!(lan.token().as_deref(), Some(second.as_str()));
    set(TOKEN, "invalid");
    assert_eq!(lan.token(), None);
    set(TOKEN, &second);
    set(BIND_ADDRESS, "invalid-address");
    assert_eq!(lan.token(), None);
    assert_eq!(local_option("agent-mcp-endpoint").unwrap(), "");
    set(BIND_ADDRESS, "192.168.1.20:60001");
    let specific = DesktopBackend {
        address: listen_address().unwrap(),
    };
    assert_eq!(lan.token(), None);
    assert!(specific.token().is_some());
    assert_eq!(
        local_option("agent-mcp-endpoint").unwrap(),
        "http://192.168.1.20:60001/mcp"
    );
    set(ENABLE, "N");
    assert_eq!(specific.token(), None);
}
