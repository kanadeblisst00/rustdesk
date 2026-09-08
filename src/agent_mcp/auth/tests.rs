use super::*;
use crate::{
    agent_mcp::{session, states, track},
    client::Interface,
    flutter::{sessions, FlutterSession},
    flutter_ffi::SessionID,
};
use hbb_common::{
    config, message_proto::*, protobuf::Message as _, rendezvous_proto::ConnType,
    tcp::FramedStream, tokio,
};
use serde_json::{json, Value};
use std::time::Duration;

fn isolated(name: &str) -> bool {
    const CHILD: &str = "RUSTDESK_MCP_AUTH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("agent_mcp::auth::tests::{name}"),
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
        return false;
    }
    *config::APP_NAME.write().unwrap() = format!("RustDeskAuthTest-{}", SessionID::new_v4());
    config::OVERWRITE_LOCAL_SETTINGS.write().unwrap().extend([
        ("enable-agent-mcp".into(), "Y".into()),
        ("agent-mcp-devices".into(), String::new()),
        ("agent-mcp-read-only".into(), "N".into()),
    ]);
    true
}

fn terminal() -> (
    SessionID,
    FlutterSession,
    tokio::sync::mpsc::UnboundedReceiver<client::Data>,
) {
    let s = FlutterSession::default();
    s.lc.write().unwrap().initialize(
        "auth-test-peer".into(),
        ConnType::TERMINAL,
        None,
        false,
        None,
        None,
        None,
    );
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *s.sender.write().unwrap() = Some(tx);
    let id = SessionID::new_v4();
    sessions::insert_session(id, ConnType::TERMINAL, s.clone());
    (id, s, rx)
}

fn transport() -> (hbb_common::Stream, hbb_common::Stream) {
    let (client, server) = tokio::io::duplex(65536);
    let address = "127.0.0.1:1".parse().unwrap();
    (
        hbb_common::Stream::Tcp(FramedStream::from(client, address)),
        hbb_common::Stream::Tcp(FramedStream::from(server, address)),
    )
}

async fn challenge(s: &FlutterSession, preset: &str) -> LoginRequest {
    let (mut client, mut server) = transport();
    assert!(
        client::handle_hash(
            s.lc.clone(),
            preset,
            Hash {
                salt: "fixture-salt".into(),
                challenge: "fixture-challenge".into(),
                ..Default::default()
            },
            &**s,
            &mut client
        )
        .await
    );
    let bytes = server.next_timeout(1000).await.unwrap().unwrap();
    let message = Message::parse_from_bytes(&bytes).unwrap();
    match message.union.unwrap() {
        message::Union::LoginRequest(request) => request,
        _ => panic!("Expected an actual login request"),
    }
}

fn connect(timeout: u64) -> Value {
    session::connect(
        json!({"device_id":"auth-test-peer", "kind":"terminal", "timeout_ms":timeout})
            .as_object()
            .unwrap(),
    )
    .unwrap()["structuredContent"]
        .clone()
}

#[tokio::test]
async fn cached_password_challenge_waits_for_authentication_in_reused_session() {
    if !isolated("cached_password_challenge_waits_for_authentication_in_reused_session") {
        return;
    }
    let (id, s, _rx) = tokio::task::spawn_blocking(terminal).await.unwrap();
    assert!(!challenge(&s, "fixture-password").await.password.is_empty());
    // The second handshake uses the cached password, with no preset supplied.
    assert!(!challenge(&s, "").await.password.is_empty());
    assert!(s.lc.read().unwrap().agent_has_login_challenge());
    let pending = session::info(id, &s);
    assert_eq!(pending["connected"], false);
    assert_eq!(pending["needs_password"], false);
    assert_eq!(pending["authentication"], "authenticating");
    let waiting = tokio::task::spawn_blocking(|| connect(2000));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !waiting.is_finished(),
        "connect_device returned before automatic authentication finished"
    );
    s.lc.write().unwrap().peer_info = Some(PeerInfo::default());
    let details = waiting.await.unwrap();
    assert_eq!(details["session"], id.to_string());
    assert_eq!(details["connected"], true);
    assert_eq!(details["needs_password"], false);
    assert_eq!(details["authentication"], "authenticated");
    assert_eq!(session::list().len(), 1);
}

#[tokio::test]
async fn local_password_prompt_waits_for_recent_session_or_remote_approval() {
    if !isolated("local_password_prompt_waits_for_recent_session_or_remote_approval") {
        return;
    }
    let (id, s, _rx) = tokio::task::spawn_blocking(terminal).await.unwrap();
    assert!(challenge(&s, "").await.password.is_empty());
    assert_eq!(
        session::info(id, &s)["authentication"],
        "password_or_approval"
    );
    let waiting = tokio::task::spawn_blocking(|| connect(2000));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !waiting.is_finished(),
        "A local prompt is not a rejection from the peer"
    );
    s.lc.write().unwrap().peer_info = Some(PeerInfo::default());
    assert_eq!(waiting.await.unwrap()["connected"], true);
}

#[tokio::test]
async fn explicit_authentication_errors_survive_late_mcp_tracking() {
    if !isolated("explicit_authentication_errors_survive_late_mcp_tracking") {
        return;
    }
    let (id, s, _rx) = tokio::task::spawn_blocking(terminal).await.unwrap();
    challenge(&s, "fixture-password").await;
    assert!(!states().lock().unwrap().contains_key(&id));
    assert!(s.handle_login_error(client::LOGIN_MSG_PASSWORD_WRONG));
    track(id, false).unwrap();
    let details = connect(2000);
    assert_eq!(details["connected"], false);
    assert_eq!(details["needs_password"], true);
    assert_eq!(details["authentication"], "password_required");
    let (mut client, mut server) = transport();
    client::handle_login_from_ui(
        s.lc.clone(),
        String::new(),
        String::new(),
        "replacement-fixture".into(),
        false,
        &mut client,
    )
    .await;
    assert!(server.next_timeout(1000).await.is_some());
    assert_eq!(session::info(id, &s)["needs_password"], false);
    for (error, state, password) in [
        (client::REQUIRE_2FA, "two_factor_required", false),
        (client::LOGIN_MSG_2FA_WRONG, "two_factor_required", false),
        (
            client::LOGIN_MSG_NO_PASSWORD_ACCESS,
            "waiting_remote_approval",
            false,
        ),
        (client::LOGIN_MSG_PASSWORD_EMPTY, "password_required", true),
    ] {
        assert!(s.handle_login_error(error));
        let info = session::info(id, &s);
        assert_eq!(info["authentication"], state);
        assert_eq!(info["needs_password"], password);
    }
    assert!(!s.handle_login_error("fixture login failure"));
    assert_eq!(connect(2000)["authentication"], "failed");
    reset(&s.lc);
    assert_eq!(session::info(id, &s)["authentication"], "connecting");
    s.handle_login_error(client::REQUIRE_2FA);
    let mut message = Message::new();
    message.set_auth_2fa(Auth2FA::default());
    outgoing(&s.lc, &message);
    assert_eq!(session::info(id, &s)["authentication"], "authenticating");
}

#[tokio::test]
async fn authentication_timeout_does_not_invent_a_password_requirement() {
    if !isolated("authentication_timeout_does_not_invent_a_password_requirement") {
        return;
    }
    let (id, s, _rx) = tokio::task::spawn_blocking(terminal).await.unwrap();
    challenge(&s, "fixture-password").await;
    let details = tokio::task::spawn_blocking(|| connect(100)).await.unwrap();
    assert_eq!(details["session"], id.to_string());
    assert_eq!(details["connected"], false);
    assert_eq!(details["needs_password"], false);
    assert_eq!(details["authentication"], "authenticating");
}

#[tokio::test]
async fn terminal_os_login_is_distinct_from_connection_password() {
    if !isolated("terminal_os_login_is_distinct_from_connection_password") {
        return;
    }
    let (id, s, _rx) = tokio::task::spawn_blocking(terminal).await.unwrap();
    s.lc.write().unwrap().is_terminal_admin = true;
    for (preset, state, password) in [
        ("", "os_login_and_password_required", true),
        ("fixture-password", "os_login_required", false),
    ] {
        let (mut client, _server) = transport();
        assert!(
            client::handle_hash(
                s.lc.clone(),
                preset,
                Hash {
                    salt: "fixture-salt".into(),
                    challenge: "fixture-challenge".into(),
                    ..Default::default()
                },
                &*s,
                &mut client
            )
            .await
        );
        let info = connect(2000);
        assert_eq!(info["session"], id.to_string());
        assert_eq!(info["authentication"], state);
        assert_eq!(info["needs_password"], password);
    }
}
