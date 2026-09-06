use super::*;

struct OptionOverride {
    options: &'static RwLock<HashMap<String, String>>,
    key: &'static str,
    previous: Option<String>,
}

impl OptionOverride {
    fn new(
        options: &'static RwLock<HashMap<String, String>>,
        key: &'static str,
        value: &str,
    ) -> Self {
        let previous = options.write().unwrap().insert(key.into(), value.into());
        Self {
            options,
            key,
            previous,
        }
    }
}

impl Drop for OptionOverride {
    fn drop(&mut self) {
        let mut options = self.options.write().unwrap();
        if let Some(value) = self.previous.take() {
            options.insert(self.key.into(), value);
        } else {
            options.remove(self.key);
        }
    }
}

#[tokio::test]
async fn global_relay_policy_applies_to_all_session_types_without_saving_peer_preference() {
    // Configuration is process-wide; keep these overrides out of other concurrent tests.
    if std::env::var_os("RUSTDESK_RELAY_TEST_CHILD").is_none() {
        let output = tokio::task::spawn_blocking(|| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "client::relay_policy_tests::global_relay_policy_applies_to_all_session_types_without_saving_peer_preference",
                    "--test-threads=1",
                ])
                .env("RUSTDESK_RELAY_TEST_CHILD", "1")
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let id = Uuid::new_v4().to_string();
    *config::APP_NAME.write().unwrap() = format!("RustDeskRelayTest-{id}");
    assert!(
        !Config::is_proxy(),
        "This test requires direct loopback sockets"
    );
    if WebRTCStream::default_stun_servers().is_empty() {
        assert!(crate::test_ipv6().await.is_none());
    }
    let _webrtc = OptionOverride::new(
        &config::OVERWRITE_LOCAL_SETTINGS,
        keys::OPTION_ENABLE_WEBRTC,
        "Y",
    );
    for value in ["Y", "N", ""] {
        let _relay = OptionOverride::new(&config::OVERWRITE_SETTINGS, "force-always-relay", value);
        for conn_type in [
            ConnType::DEFAULT_CONN,
            ConnType::FILE_TRANSFER,
            ConnType::PORT_FORWARD,
            ConnType::VIEW_CAMERA,
            ConnType::TERMINAL,
        ] {
            for explicit_relay in [false, true] {
                let session = Session::<crate::flutter::FlutterHandler>::default();
                session.lc.write().unwrap().initialize(
                    id.clone(),
                    conn_type,
                    None,
                    explicit_relay,
                    None,
                    None,
                    None,
                );
                let lc = session.lc.read().unwrap();
                assert_eq!(lc.peer_relay, explicit_relay);
                assert_eq!(lc.get_option("force-always-relay"), "");
                let policy_relay = value == "Y" || explicit_relay || Config::is_proxy();
                assert_eq!(lc.policy_relay, policy_relay);
                assert_eq!(lc.force_relay, policy_relay || use_ws());
                drop(lc);
                if value == "Y" {
                    assert!(!Client::should_create_webrtc_offerer(&session));
                } else if !policy_relay {
                    assert!(Client::should_create_webrtc_offerer(&session));
                }
            }
        }
    }
    let _relay = OptionOverride::new(&config::OVERWRITE_SETTINGS, "force-always-relay", "Y");
    let _ws = OptionOverride::new(
        &config::OVERWRITE_SETTINGS,
        keys::OPTION_ALLOW_WEBSOCKET,
        "N",
    );
    let _tcp = OptionOverride::new(
        &config::OVERWRITE_LOCAL_SETTINGS,
        keys::OPTION_ENABLE_TCP_PUNCH,
        "Y",
    );
    let session = Session::<crate::flutter::FlutterHandler>::default();
    session.lc.write().unwrap().initialize(
        id.clone(),
        ConnType::DEFAULT_CONN,
        None,
        false,
        None,
        None,
        None,
    );
    assert_relay_refusal_does_not_attempt_direct(id, session).await;
}

async fn assert_relay_refusal_does_not_attempt_direct(
    id: String,
    session: Session<crate::flutter::FlutterHandler>,
) {
    use hbb_common::tcp::FramedStream;
    use tokio::net::TcpListener;

    let rendezvous = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rendezvous_addr = rendezvous.local_addr().unwrap().to_string();
    let direct_addr = direct.local_addr().unwrap();
    let server = async {
        let (socket, addr) = rendezvous.accept().await.unwrap();
        let mut socket = FramedStream::from(socket, addr);
        let bytes = socket.next().await.unwrap().unwrap();
        let request = RendezvousMessage::parse_from_bytes(&bytes).unwrap();
        assert!(request.has_punch_hole_request());
        assert!(request.punch_hole_request().force_relay);
        assert_eq!(request.punch_hole_request().udp_port, 0);
        assert!(request.punch_hole_request().socket_addr_v6.is_empty());
        assert!(request.punch_hole_request().webrtc_sdp_offer.is_empty());
        let mut response = RendezvousMessage::new();
        response.set_punch_hole_response(PunchHoleResponse {
            socket_addr: AddrMangle::encode(direct_addr).into(),
            relay_server: "127.0.0.1:9".into(),
            ..Default::default()
        });
        socket.send(&response).await.unwrap();
        let (socket, addr) = rendezvous.accept().await.unwrap();
        let mut socket = FramedStream::from(socket, addr);
        let bytes = socket.next().await.unwrap().unwrap();
        let request = RendezvousMessage::parse_from_bytes(&bytes).unwrap();
        assert!(request.has_request_relay());
        let mut response = RendezvousMessage::new();
        response.set_relay_response(RelayResponse {
            refuse_reason: "relay-policy-test-refusal".into(),
            ..Default::default()
        });
        socket.send(&response).await.unwrap();
    };
    let client = Client::_start_inner(
        id,
        String::new(),
        String::new(),
        ConnType::DEFAULT_CONN,
        session,
        (None, None),
        None,
        None,
        None,
        rendezvous_addr,
        Vec::new(),
        true,
    );
    tokio::select! {
        biased;
        _ = direct.accept() => panic!("Forced relay attempted a direct connection"),
        (_, result) = async { tokio::join!(server, client) } => {
            let error = result.err().expect("The test relay must refuse the connection");
            assert!(error.to_string().contains("relay-policy-test-refusal"), "{error}");
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("Relay test timed out"),
    }
}
