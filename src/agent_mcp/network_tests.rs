use super::*;

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
