use hbb_common::config;
use std::sync::Once;

pub const APP_NAME: &str = "RustDeskMCP";
pub const BUNDLE_ID: &str = "com.carriez.RustDeskMCP";
pub const LAN_PORT: u16 = 59943;

pub fn initialize() -> bool {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        *config::APP_NAME.write().unwrap() = APP_NAME.into();
        config::DEFAULT_SETTINGS
            .write()
            .unwrap()
            .insert("direct-access-port".into(), "59942".into());
        config::OVERWRITE_LOCAL_SETTINGS
            .write()
            .unwrap()
            .insert("enable-check-update".into(), "N".into());
        config::OVERWRITE_SETTINGS
            .write()
            .unwrap()
            .insert("allow-auto-update".into(), "N".into());
    });
    true
}

pub fn initialize_device() -> bool {
    use hbb_common::rand::Rng;
    initialize();
    if std::env::args().nth(1).as_deref() != Some("--mcp-identity-info")
        && !config::Config::file().exists()
    {
        // The stock initial ID derives from the MAC address. Use a separate ID
        // before either copy can register the same device with different keys.
        let id = hbb_common::rand::thread_rng().gen_range(1_000_000_000..2_000_000_000u32);
        config::Config::set_id(&id.to_string());
    }
    true
}

pub fn report_requested() -> bool {
    if std::env::args().nth(1).as_deref() != Some("--mcp-identity-info") {
        return false;
    }
    println!(
        "{}",
        serde_json::json!({
            "app_name": APP_NAME,
            "bundle_id": BUNDLE_ID,
            "config_dir": config::Config::path(""),
            "log_dir": config::Config::log_path(),
            "ipc": config::Config::ipc_path(""),
            "service_ipc": config::Config::ipc_path("_service"),
            "mcp_endpoint": format!("http://{}/mcp", super::ADDRESS),
            "default_direct_access_port": config::DEFAULT_SETTINGS.read().unwrap().get("direct-access-port"),
            "lan_port": LAN_PORT,
        })
    );
    true
}

#[cfg(target_os = "macos")]
pub fn macos_template(template: &str) -> String {
    // Replace the bundle ID last: it also contains the original application name.
    template
        .replace("com.carriez.rustdesk", "__MCP_BUNDLE_ID__")
        .replace("rustdesk", "rustdeskmcp")
        .replace("RustDesk", APP_NAME)
        .replace("__MCP_BUNDLE_ID__", BUNDLE_ID)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_applied_before_config_and_survives_custom_branding() {
        // Config lazy statics are process-wide; never change another test's identity.
        const CHILD: &str = "RUSTDESK_MCP_IDENTITY_TEST";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "agent_mcp::identity::tests::identity_is_applied_before_config_and_survives_custom_branding"])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(result.success());
            return;
        }
        crate::common::global_init();
        initialize();
        crate::common::read_custom_client("must not override MCP identity");
        assert_eq!(crate::get_app_name(), APP_NAME);
        assert_eq!(crate::get_uri_prefix(), "rustdeskmcp://");
        assert!(config::Config::path("")
            .to_string_lossy()
            .to_lowercase()
            .contains("rustdeskmcp"));
        assert!(config::Config::log_path()
            .to_string_lossy()
            .to_lowercase()
            .contains("rustdeskmcp"));
        for suffix in ["", "_url", "_service", "_cm"] {
            assert!(config::Config::ipc_path(suffix).contains(APP_NAME));
        }
        assert_eq!(
            config::DEFAULT_SETTINGS.read().unwrap()["direct-access-port"],
            "59942"
        );
        assert!(!config::is_outgoing_only());
        assert!(!config::is_disable_installation());
        #[cfg(target_os = "macos")]
        for template in [
            include_str!("../platform/privileges_scripts/daemon.plist"),
            include_str!("../platform/privileges_scripts/agent.plist"),
        ] {
            let body = macos_template(template);
            assert!(body.contains(BUNDLE_ID));
            assert!(body.contains("/Applications/RustDeskMCP.app/"));
            assert!(!body.contains("MCPMCP"));
            assert!(!body.contains("com.carriez.RustDesk_"));
        }
    }
}
