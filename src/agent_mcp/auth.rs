use crate::client::{self, LoginConfigHandler};
use std::sync::{Arc, RwLock};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Default)]
pub(crate) enum State {
    #[default]
    Connecting,
    Authenticating,
    PasswordOrApproval,
    PasswordRequired,
    TwoFactorRequired,
    OsLoginRequired,
    OsLoginAndPasswordRequired,
    WaitingRemoteApproval,
    Failed,
}

impl State {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Authenticating => "authenticating",
            Self::PasswordOrApproval => "password_or_approval",
            Self::PasswordRequired => "password_required",
            Self::TwoFactorRequired => "two_factor_required",
            Self::OsLoginRequired => "os_login_required",
            Self::OsLoginAndPasswordRequired => "os_login_and_password_required",
            Self::WaitingRemoteApproval => "waiting_remote_approval",
            Self::Failed => "failed",
        }
    }

    pub(super) fn needs_password(self) -> bool {
        matches!(
            self,
            Self::PasswordOrApproval | Self::PasswordRequired | Self::OsLoginAndPasswordRequired
        )
    }
}

pub(crate) fn begin(lc: &Arc<RwLock<LoginConfigHandler>>) {
    lc.write().unwrap().agent_auth = State::Authenticating;
}

pub(crate) fn reset(lc: &Arc<RwLock<LoginConfigHandler>>) {
    lc.write().unwrap().agent_auth = State::Connecting;
}

pub(crate) fn password_prompt(lc: &Arc<RwLock<LoginConfigHandler>>) {
    // An empty login may still be accepted by the peer or its recent-session cache.
    lc.write().unwrap().agent_auth = State::PasswordOrApproval;
}

pub(crate) fn terminal_prompt(lc: &Arc<RwLock<LoginConfigHandler>>, password_missing: bool) {
    lc.write().unwrap().agent_auth = if password_missing {
        State::OsLoginAndPasswordRequired
    } else {
        State::OsLoginRequired
    };
}

pub(crate) fn login_error(lc: &Arc<RwLock<LoginConfigHandler>>, error: &str) {
    lc.write().unwrap().agent_auth = match error {
        client::LOGIN_MSG_PASSWORD_EMPTY | client::LOGIN_MSG_PASSWORD_WRONG => {
            State::PasswordRequired
        }
        client::REQUIRE_2FA | client::LOGIN_MSG_2FA_WRONG => State::TwoFactorRequired,
        client::LOGIN_MSG_NO_PASSWORD_ACCESS => State::WaitingRemoteApproval,
        _ => State::Failed,
    };
}

pub(crate) fn outgoing(
    lc: &Arc<RwLock<LoginConfigHandler>>,
    message: &hbb_common::message_proto::Message,
) {
    if matches!(
        message.union,
        Some(hbb_common::message_proto::message::Union::Auth2fa(_))
    ) {
        begin(lc);
    }
}

pub(super) fn settled(details: &serde_json::Value) -> bool {
    details["connected"] == true
        || matches!(
            details["authentication"].as_str(),
            Some(
                "password_required"
                    | "two_factor_required"
                    | "os_login_required"
                    | "os_login_and_password_required"
                    | "failed"
            )
        )
}

pub(super) fn wait_for_session(
    id: crate::flutter_ffi::SessionID,
    session: &crate::flutter::FlutterSession,
    args: &serde_json::Map<String, serde_json::Value>,
) -> super::ToolResult {
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(super::number(args, "timeout_ms", 12000) as u64);
    loop {
        super::writable()?;
        let current = super::session::get(id)?;
        if !Arc::ptr_eq(session, &current) {
            return Err("Session changed while waiting for authentication".into());
        }
        let details = super::session::info(id, &current);
        if settled(&details) || std::time::Instant::now() >= deadline {
            return Ok(super::success(details));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
