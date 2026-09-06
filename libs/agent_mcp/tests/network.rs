use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rustdesk_agent_mcp::{http, success, Backend, Server, ToolResult};
use serde_json::{json, Map, Value};
use std::{
    net::SocketAddrV4,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
struct Fake {
    token: Mutex<Option<String>>,
    calls: AtomicUsize,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            token: Mutex::new(Some(TOKEN.into())),
            calls: AtomicUsize::new(0),
        })
    }
}
impl Backend for Fake {
    fn token(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }
    fn call(&self, _: &str, _: &Map<String, Value>) -> ToolResult {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(success(json!({})))
    }
}
fn address(value: &str) -> SocketAddrV4 {
    value.parse().unwrap()
}
fn request(host: &str, tokens: &[&str], origin: bool) -> Request<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("Host", host)
        .header("Content-Type", "application/json");
    for token in tokens {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    if origin {
        req = req.header("Origin", "http://192.168.1.20");
    }
    req.body(Body::from(
        json!({"jsonrpc":"2.0", "id":1, "method":"tools/call",
        "params":{"name":"list_connections","arguments":{}}})
        .to_string(),
    ))
    .unwrap()
}

#[test]
fn listen_configuration_and_copyable_endpoints() {
    for (value, port, expected) in [
        ("", 59940, "127.0.0.1:59940"),
        ("", 59941, "127.0.0.1:59941"),
        (" 0.0.0.0 ", 59940, "0.0.0.0:59940"),
        ("192.168.1.20:60000", 59940, "192.168.1.20:60000"),
    ] {
        assert_eq!(
            http::parse_listen_address(value, port).unwrap(),
            address(expected)
        );
    }
    for value in [
        "localhost",
        "evil.test:80",
        "127.1",
        "[::]:80",
        "::1",
        "1.2.3.4:0",
        "1.2.3.4:65536",
        "256.1.1.1",
        "224.0.0.1",
        "255.255.255.255",
        "0.1.2.3",
        "http://0.0.0.0:80",
    ] {
        assert!(http::parse_listen_address(value, 59940).is_err(), "{value}");
    }
    assert_eq!(
        http::client_endpoint(address("0.0.0.0:59941")),
        "http://127.0.0.1:59941/mcp"
    );
    assert_eq!(
        http::client_endpoint(address("192.168.1.20:60000")),
        "http://192.168.1.20:60000/mcp"
    );
}

#[tokio::test]
async fn lan_requires_token_and_rotation_revokes_old_credentials() {
    let backend = Fake::new();
    let app = http::router_on(
        Arc::new(Server::new(backend.clone())),
        address("0.0.0.0:59940"),
    );
    for tokens in [&[][..], &["wrong"][..], &[TOKEN, TOKEN][..]] {
        let result = app
            .clone()
            .oneshot(request("192.168.1.20:59940", tokens, false))
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(result.headers()["www-authenticate"], "Bearer");
    }
    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        app.clone()
            .oneshot(request("192.168.1.20:59940", &[TOKEN], false))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let replacement = "b".repeat(64);
    *backend.token.lock().unwrap() = Some(replacement.clone());
    assert_eq!(
        app.clone()
            .oneshot(request("192.168.1.20:59940", &[TOKEN], false))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(request("192.168.1.20:59940", &[&replacement], false))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    *backend.token.lock().unwrap() = None;
    assert_eq!(
        app.oneshot(request("192.168.1.20:59940", &[&replacement], false))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(backend.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn lan_host_policy_rejects_dns_origins_and_wrong_interfaces() {
    for bind in ["0.0.0.0:59940", "192.168.1.20:59940"] {
        let backend = Fake::new();
        let app = http::router_on(Arc::new(Server::new(backend.clone())), address(bind));
        for host in [
            "evil.test:59940",
            "192.168.1.20.evil.test:59940",
            "192.168.1.20:80",
            "192.168.1.20",
            "192.168.1.20:0",
            "192.168.1.20:59940@evil.test",
            "0.0.0.0:59940",
            "224.0.0.1:59940",
            "255.255.255.255:59940",
            "[::1]:59940",
            "",
        ] {
            assert_eq!(
                app.clone()
                    .oneshot(request(host, &[TOKEN], false))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN,
                "{bind} / {host}"
            );
        }
        assert_eq!(
            app.clone()
                .oneshot(request("192.168.1.20:59940", &[TOKEN], true))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let mut duplicate = request("192.168.1.20:59940", &[TOKEN], false);
        duplicate
            .headers_mut()
            .append("Host", "evil.test".parse().unwrap());
        assert_eq!(
            app.clone().oneshot(duplicate).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        if bind.starts_with("192") {
            for host in ["192.168.1.21:59940", "localhost:59940", "127.0.0.1:59940"] {
                assert_eq!(
                    app.clone()
                        .oneshot(request(host, &[TOKEN], false))
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::FORBIDDEN
                );
            }
        }
        assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            app.oneshot(request("192.168.1.20:59940", &[TOKEN], false))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn invalid_configured_tokens_fail_closed() {
    for token in [
        String::new(),
        "x".repeat(31),
        "x".repeat(257),
        " ".repeat(32),
        "x".repeat(32) + "\n",
        "令牌".repeat(32),
    ] {
        assert!(!http::valid_token(&token));
        let backend = Fake::new();
        *backend.token.lock().unwrap() = Some(token);
        let result = http::router_on(
            Arc::new(Server::new(backend.clone())),
            address("0.0.0.0:59940"),
        )
        .oneshot(request("192.168.1.20:59940", &[TOKEN], false))
        .await
        .unwrap();
        assert_eq!(result.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    }
    assert!(http::valid_token(TOKEN));
    assert!(http::valid_token(&"x".repeat(256)));
}

#[tokio::test]
async fn wildcard_listener_serves_authenticated_requests_and_releases_port() {
    use std::{
        io::{Read, Write},
        time::Duration,
    };
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let backend = Fake::new();
    let server = tokio::spawn(http::serve(
        listener,
        Arc::new(Server::new(backend.clone())),
    ));
    let response = tokio::task::spawn_blocking(move || {
        let body = r#"{"jsonrpc":"2.0","id":42,"method":"ping"}"#;
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(stream, "POST /mcp HTTP/1.1\r\nHost: 192.168.1.20:{port}\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"id\":42"));
    *backend.token.lock().unwrap() = None;
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let replacement = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    assert_eq!(replacement.local_addr().unwrap().port(), port);
}
