use super::*;
use rustdesk_agent_mcp::{
    context::CallContext,
    local::{ipc::Client, Provider, Registry},
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

const LOCAL_ENABLE: &str = "agent-mcp-local-enable";
const LOCAL_TOKEN: &str = "agent-mcp-local-token";
const LOCAL_INPUT: &str = "agent-mcp-local-input";
static REGISTRY: OnceLock<Registry<DesktopProvider>> = OnceLock::new();
static STOPPED: AtomicBool = AtomicBool::new(false);
static EPOCH: Mutex<u64> = Mutex::new(0);

pub(super) fn stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}

fn registry() -> &'static Registry<DesktopProvider> {
    REGISTRY.get_or_init(|| Registry::new(DesktopProvider(Mutex::new(None))))
}

pub(super) fn supported() -> bool {
    cfg!(all(
        any(windows, target_os = "linux", target_os = "macos"),
        not(feature = "mcp-isolated")
    )) && !daemon::active()
}

fn local_token() -> Option<String> {
    let token = LocalConfig::get_option(LOCAL_TOKEN);
    (supported()
        && enabled()
        && !lifecycle::stopping()
        && !STOPPED.load(Ordering::SeqCst)
        && LocalConfig::get_option(LOCAL_ENABLE) == "Y"
        && rustdesk_agent_mcp::http::valid_token(&token)
        && token != LocalConfig::get_option(TOKEN))
    .then_some(token)
}

fn credential(token: &str) -> String {
    format!(
        "{:x}:{}",
        Sha256::digest(token.as_bytes()),
        *EPOCH.lock().unwrap()
    )
}

pub(super) fn authenticate(supplied: &str) -> Option<CallContext> {
    let token = local_token()?;
    rustdesk_agent_mcp::http::token_equal(&token, supplied)
        .then(|| CallContext::new("local-desktop".into(), credential(&token), true))
}

fn check(context: &CallContext) -> Result<(), String> {
    ensure_enabled()?;
    let token =
        local_token().ok_or("Local control disabled or stopped; enable it in local settings")?;
    if !context.local
        || context.principal != "local-desktop"
        || context.credential != credential(&token)
    {
        return Err("Local credential or policy revoked".into());
    }
    context.check()
}

pub(super) fn call(context: &CallContext, name: &str, args: &Map<String, Value>) -> ToolResult {
    check(context)?;
    if name == "stop_local" {
        STOPPED.store(true, Ordering::SeqCst);
        LocalConfig::set_option(LOCAL_ENABLE.into(), "N".into());
        revoke();
        return Ok(success(
            json!({"state":"stopped","sessions_revoked":true,"resume":"Enable local control again in the host settings"}),
        ));
    }
    let input = LocalConfig::get_option(LOCAL_INPUT) == "Y"
        && LocalConfig::get_option("agent-mcp-read-only") != "Y";
    let scopes = if input {
        vec!["desktop.capture", "desktop.input"]
    } else {
        vec!["desktop.capture"]
    };
    registry().call(
        context,
        name,
        args,
        &scopes,
        || uuid::Uuid::new_v4().to_string(),
        || {
            check(context)?;
            if rustdesk_agent_mcp::local::input_tool(name)
                && (LocalConfig::get_option(LOCAL_INPUT) != "Y"
                    || LocalConfig::get_option("agent-mcp-read-only") == "Y")
            {
                return Err("Local input permission revoked".into());
            }
            Ok(())
        },
    )
}

pub(crate) fn option_changed(key: &str) {
    if matches!(
        key,
        LOCAL_ENABLE
            | LOCAL_TOKEN
            | LOCAL_INPUT
            | ENABLE
            | TOKEN
            | "agent-mcp-read-only"
            | BIND_ADDRESS
    ) {
        revoke();
        if key == LOCAL_ENABLE && LocalConfig::get_option(LOCAL_ENABLE) == "Y" {
            STOPPED.store(false, Ordering::SeqCst);
        }
    }
}

pub(super) fn revoke() {
    *EPOCH.lock().unwrap() += 1;
    if let Some(registry) = REGISTRY.get() {
        if let Err(e) = registry.revoke() {
            log::warn!("Local desktop stop: {e}");
        }
    }
}

struct DesktopProvider(Mutex<Option<Arc<Client>>>);
impl Provider for DesktopProvider {
    fn call(
        &self,
        name: &str,
        args: &Map<String, Value>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> ToolResult {
        if !supported() {
            return Err("Local desktop requires a non-isolated desktop mcp-local GUI build".into());
        }
        check()?;
        let client = {
            let mut current = self.0.lock().unwrap();
            if current.is_none() {
                let executable = std::env::current_exe().map_err(|e| e.to_string())?;
                let mut command = std::process::Command::new(executable);
                command.arg("--mcp-local-agent");
                *current = Some(Arc::new(Client::spawn(&mut command, check)?));
            }
            current.as_ref().cloned().ok_or("Local agent unavailable")?
        };
        let result = client.call(name, args, check);
        if result.is_err() {
            let mut current = self.0.lock().unwrap();
            if current
                .as_ref()
                .is_some_and(|value| Arc::ptr_eq(value, &client))
            {
                current.take();
            }
            if let Err(e) = client.stop() {
                log::warn!("Failed local agent stop: {e}");
            }
        }
        result
    }

    fn stop(&self) -> Result<(), String> {
        let previous = self.0.lock().unwrap().take();
        if let Some(client) = previous {
            client.stop()?;
        }
        Ok(())
    }
}

pub(crate) fn requested() -> bool {
    if std::env::args().nth(1).as_deref() != Some("--mcp-local-agent") {
        return false;
    }
    #[cfg(all(windows, not(feature = "mcp-isolated")))]
    let result = crate::platform::local_agent::run();
    #[cfg(all(
        any(target_os = "linux", target_os = "macos"),
        not(feature = "mcp-isolated")
    ))]
    let result = crate::platform::local_agent_unix::run();
    #[cfg(not(all(
        any(windows, target_os = "linux", target_os = "macos"),
        not(feature = "mcp-isolated")
    )))]
    let result: Result<(), String> = Err("Local desktop is unavailable in this build".into());
    if let Err(e) = result {
        eprintln!("Local desktop agent: {e}");
        std::process::exit(2);
    }
    true
}
