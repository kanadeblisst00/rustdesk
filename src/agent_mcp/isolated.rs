use hbb_common::{config, log};
use std::sync::Once;

pub const APP_NAME: &str = "RustDeskMCPTest";
pub const ORGANIZATION: &str = "cn.ikanade";

// Both native entry points must set the identity before any lazy config is loaded.
pub fn initialize() -> bool {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        *config::APP_NAME.write().unwrap() = APP_NAME.into();
        *config::ORG.write().unwrap() = ORGANIZATION.into();
        config::HARD_SETTINGS.write().unwrap().extend([
            ("conn-type".into(), "outgoing".into()),
            ("disable-installation".into(), "Y".into()),
            ("disable-tcp-listen".into(), "Y".into()),
        ]);
        config::OVERWRITE_LOCAL_SETTINGS
            .write()
            .unwrap()
            .insert("enable-check-update".into(), "N".into());
    });
    true
}

fn allowed_command(command: Option<&str>) -> bool {
    matches!(
        command,
        None | Some(
            "--version"
                | "--mcp-server"
                | "--build-date"
                | "--no-server"
                | "--connect"
                | "--play"
                | "--file-transfer"
                | "--view-camera"
                | "--port-forward"
                | "--terminal"
                | "--rdp"
        )
    )
}

fn default_server_options() -> serde_json::Value {
    let defaults = config::DEFAULT_SETTINGS.read().unwrap();
    serde_json::json!({
        "custom-rendezvous-server": defaults.get("custom-rendezvous-server"),
        "relay-server": defaults.get("relay-server"),
        "key": defaults.get("key"),
        "force-always-relay": defaults.get("force-always-relay"),
    })
}

pub fn allow_launch() -> bool {
    let command = std::env::args().skip(1).find(|arg| arg != "--no-server");
    if command.as_deref() == Some("--mcp-isolation-info") {
        println!(
            "{}",
            serde_json::json!({
                "app_name": APP_NAME,
                "bundle_id": format!("{ORGANIZATION}.{APP_NAME}"),
                "config_dir": config::Config::path(""),
                "log_dir": config::Config::log_path(),
                "ipc": config::Config::ipc_path(""),
                "service_ipc": config::Config::ipc_path("_service"),
                "mcp_endpoint": format!("http://{}/mcp", super::ADDRESS),
                "outgoing_only": config::is_outgoing_only(),
                "installation_disabled": config::is_disable_installation(),
                "default_server_options": default_server_options(),
                "built_in_server": {
                    "id_servers": config::RENDEZVOUS_SERVERS,
                    "public_key": config::RS_PUB_KEY,
                    "relay_server": config::DEFAULT_SETTINGS.read().unwrap()
                        .get("relay-server").cloned().unwrap_or_default(),
                    "always_relay": config::DEFAULT_SETTINGS.read().unwrap()
                        .get("force-always-relay").map_or(false, |value| value == "Y"),
                    "default_stun_servers": hbb_common::webrtc::WebRTCStream::default_stun_servers(),
                },
            })
        );
        return false;
    }
    if !allowed_command(command.as_deref()) {
        eprintln!("RustDeskMCPTest refuses service, installation and management commands");
        return false;
    }
    true
}

// Keep GUI option synchronization, without starting capture/input/host listeners.
pub fn start_control_ipc() {
    if let Err(err) = std::thread::Builder::new()
        .name("mcp-test-ipc".into())
        .spawn(|| {
            if let Err(err) = crate::ipc::start("") {
                log::error!("Isolated MCP configuration IPC: {err}");
            }
        })
    {
        log::error!("Failed to start isolated MCP configuration IPC: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_commands_are_not_allowed() {
        for command in [
            "--server",
            "--service",
            "--install",
            "--update",
            "--remove",
            "--install-service",
            "--uninstall-service",
            "--write-plists",
            "--config",
            "--import-config",
            "--password",
            "--tray",
            "--noinstall",
        ] {
            assert!(!allowed_command(Some(command)), "{command}");
        }
        assert!(allowed_command(None));
        assert!(allowed_command(Some("--connect")));
        assert!(allowed_command(Some("--version")));
    }

    #[test]
    fn system_installation_and_updates_are_rejected() {
        assert!(!crate::platform::macos::is_installed_daemon(true));
        assert!(crate::platform::macos::write_plists().is_err());
        assert!(crate::platform::macos::update_me().is_err());
        assert!(crate::platform::macos::update_from_dmg("/must-not-open.dmg").is_err());
    }

    #[test]
    fn configuration_and_ipc_use_a_separate_identity() {
        initialize();
        initialize();
        assert_eq!(*config::APP_NAME.read().unwrap(), APP_NAME);
        assert!(config::Config::path("")
            .to_string_lossy()
            .contains(APP_NAME));
        assert!(config::Config::log_path()
            .to_string_lossy()
            .contains(APP_NAME));
        for postfix in ["", "_url", "_service", "_cm"] {
            assert!(config::Config::ipc_path(postfix).contains(APP_NAME));
        }
        assert!(config::is_outgoing_only());
        assert!(config::is_disable_installation());
        assert!(config::is_disable_tcp_listen());
        assert_eq!(super::super::ADDRESS, "127.0.0.1:59941");
    }
}
