use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use rustdesk_agent_mcp::{
    catalog,
    events::{ByteLog, Events},
    http, pixels, success, Backend, Server, ToolResult,
};
use serde_json::{json, Map, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
#[derive(Default)]
struct Fake {
    disabled: AtomicBool,
    calls: AtomicUsize,
}
impl Backend for Fake {
    fn token(&self) -> Option<String> {
        (!self.disabled.load(Ordering::Relaxed)).then(|| TOKEN.into())
    }
    fn call(&self, name: &str, _: &Map<String, Value>) -> ToolResult {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if name == "disconnect_device" {
            return Err("Remote permission denied".into());
        }
        Ok(success(json!({"test_backend":true,"tool":name})))
    }
}
fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":7,"method":method,"params":params})
}

#[test]
fn negotiates_versions_and_preserves_ids() {
    let server = Server::new(Arc::new(Fake::default()));
    for version in rustdesk_agent_mcp::VERSIONS {
        let result = server
            .dispatch(request("initialize", json!({"protocolVersion":version})))
            .unwrap();
        assert_eq!(result["result"]["protocolVersion"], *version);
        assert_eq!(result["id"], 7);
    }
    let response = server
        .dispatch(request("initialize", json!({"protocolVersion":"unknown"})))
        .unwrap();
    assert_eq!(
        response["result"]["protocolVersion"],
        rustdesk_agent_mcp::VERSIONS[0]
    );
    assert_eq!(
        server.dispatch(request("initialize", json!({}))).unwrap()["error"]["code"],
        -32602
    );
    assert_eq!(
        server
            .dispatch(json!({"jsonrpc":"2.0","id":"007","method":"ping"}))
            .unwrap()["id"],
        "007"
    );
}

#[test]
fn notifications_never_invoke_the_backend() {
    let backend = Arc::new(Fake::default());
    let server = Server::new(backend.clone());
    for method in [
        "notifications/initialized",
        "notifications/cancelled",
        "tools/call",
    ] {
        assert!(server
            .dispatch(json!({"jsonrpc":"2.0","method":method,"params":{"name":"list_connections"}}))
            .is_none());
    }
    assert!(server
        .dispatch(json!({"jsonrpc":"2.0","id":1,"result":{}}))
        .is_none());
    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn invalid_requests_and_arguments_cannot_act() {
    let backend = Arc::new(Fake::default());
    let server = Server::new(backend.clone());
    for message in [
        json!([]),
        json!(null),
        json!({"method":"ping"}),
        json!({"jsonrpc":"2.0","id":{},"method":"ping"}),
        json!({"jsonrpc":"2.0","method":"ping","params":[]}),
    ] {
        assert_eq!(server.dispatch(message).unwrap()["error"]["code"], -32600);
    }
    for arguments in [
        json!({"session":"s","x":-1,"y":0}),
        json!({"session":"s","x":0.5,"y":1}),
        json!({"session":"s","x":1,"y":1,"clicks":999}),
        json!({"session":"s","x":1}),
        json!({"session":"s","x":1,"y":1,"unrecognized":true}),
        json!({"session":"s","x":"1","y":1}),
    ] {
        let response = server
            .dispatch(request(
                "tools/call",
                json!({"name":"mouse_click","arguments":arguments}),
            ))
            .unwrap();
        assert_eq!(response["error"]["code"], -32602, "{response}");
    }
    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn automation_tools_validate_targets_and_read_only_annotations() {
    let backend = Arc::new(Fake::default());
    let server = Server::new(backend.clone());
    for (name, arguments) in [
        ("click_text", json!({"session":"s","text":""})),
        (
            "click_text",
            json!({"session":"s","text":"保存","match_index":512}),
        ),
        ("set_ui_value", json!({"session":"s","value":"text"})),
        (
            "invoke_ui_element",
            json!({"session":"s","element_id":"id","action":"shell"}),
        ),
        ("get_screen_text", json!({"session":"s"})),
        ("find_text", json!({"session":"s","text":"yes"})),
        ("get_ui_state", json!({"session":"s","include_ocr":true})),
        ("get_ui_tree", json!({"session":"s","timeout_ms":10001})),
        ("get_ui_state", json!({"session":"s","display":64})),
        (
            "get_ui_tree",
            json!({"session":"s","scope":"all_processes"}),
        ),
        ("focus_window", json!({"session":"s","handle":123})),
        ("list_windows", json!({"session":"s","launch":"app.exe"})),
    ] {
        let result = server
            .dispatch(request(
                "tools/call",
                json!({"name":name,"arguments":arguments}),
            ))
            .unwrap();
        assert_eq!(result["error"]["code"], -32602, "{result}");
    }
    assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    let tools = catalog::tools();
    assert_eq!(tools.len(), 57);
    for tool in tools
        .iter()
        .filter(|t| rustdesk_agent_mcp::automation::is_tool(t["name"].as_str().unwrap()))
    {
        let write = matches!(
            tool["name"].as_str().unwrap(),
            "click_text" | "invoke_ui_element" | "set_ui_value" | "focus_window"
        );
        assert_eq!(tool["annotations"]["readOnlyHint"], !write);
        assert_eq!(tool["annotations"]["destructiveHint"], write);
    }
}

#[test]
fn errors_resources_prompts_and_catalog_are_consistent() {
    let server = Server::new(Arc::new(Fake::default()));
    let result = server
        .dispatch(request(
            "tools/call",
            json!({"name":"disconnect_device","arguments":{"session":"s"}}),
        ))
        .unwrap();
    assert_eq!(result["result"]["isError"], true);
    let result = server
        .dispatch(request(
            "resources/read",
            json!({"uri":"rustdesk://sessions"}),
        ))
        .unwrap();
    assert!(result["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("test_backend"));
    assert!(server
        .dispatch(request("prompts/get", json!({"name":"remote_operator"})))
        .unwrap()["result"]["messages"]
        .is_array());
    let mut names = std::collections::HashSet::new();
    for tool in catalog::tools() {
        assert!(names.insert(tool["name"].as_str().unwrap().to_owned()));
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
}

#[test]
fn dangerous_and_nested_arguments_are_validated() {
    let catalog = catalog::tools();
    let schema =
        |name| catalog.iter().find(|tool| tool["name"] == name).unwrap()["inputSchema"].clone();
    assert!(catalog::validate(
        &schema("file_remove"),
        &json!({"session":"s","path":"a","confirm":false}),
        "args"
    )
    .is_err());
    assert!(catalog::validate(
        &schema("keyboard_hotkey"),
        &json!({"session":"s","keys":[]}),
        "args"
    )
    .is_err());
    assert!(catalog::validate(
        &schema("screenshot"),
        &json!({"session":"s","region":{"x":0,"y":0,"width":0,"height":1}}),
        "args"
    )
    .is_err());
    assert!(catalog::validate(
        &schema("execute_actions"),
        &json!({"session":"s","actions":[{"name":"file_remove","arguments":{}}]}),
        "args"
    )
    .is_err());
}

#[test]
fn event_authorization_does_not_hold_producer_locks_and_sessions_are_isolated() {
    let first = Events::default();
    let second = Events::default();
    let result = first
        .read(0, None, Duration::ZERO, || {
            first.push("ready", json!({}));
            true
        })
        .unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(second.cursor(), 0);
    assert!(
        second.read(0, None, Duration::ZERO, || true).unwrap()["events"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn http_rejects_non_loopback_and_malformed_hosts() {
    for host in [
        "evil.test",
        "127.0.0.1.evil.test",
        "localhost:evil",
        "localhost:0",
        "localhost:59940@evil.test",
    ] {
        let req = Request::builder()
            .uri("/mcp")
            .method("POST")
            .header("Host", host)
            .header("Authorization", format!("Bearer {TOKEN}"))
            .header("Content-Type", "application/json")
            .body(Body::from(request("ping", json!({})).to_string()))
            .unwrap();
        let b = Arc::new(Fake::default());
        let result = http::router(Arc::new(Server::new(b.clone())))
            .oneshot(req)
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::FORBIDDEN, "{host}");
        assert_eq!(b.calls.load(Ordering::Relaxed), 0);
    }
}

async fn post(
    backend: Arc<Fake>,
    body: String,
    headers: &[(&str, &str)],
) -> axum::response::Response {
    let mut req = Request::builder()
        .uri("/mcp")
        .method("POST")
        .header("Host", "127.0.0.1:59940")
        .header("Content-Type", "application/json");
    for (key, value) in headers {
        req = req.header(*key, *value);
    }
    http::router(Arc::new(Server::new(backend)))
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn http_enforces_auth_origin_version_size_and_parse_errors() {
    let b = Arc::new(Fake::default());
    let body = request("ping", json!({})).to_string();
    let auth = format!("Bearer {TOKEN}");
    assert_eq!(
        post(b.clone(), body.clone(), &[]).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            b.clone(),
            body.clone(),
            &[("Authorization", "Bearer wrong")]
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            b.clone(),
            body.clone(),
            &[("Authorization", &auth), ("Origin", "https://evil.test")]
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(
            b.clone(),
            body.clone(),
            &[("Authorization", &auth), ("MCP-Protocol-Version", "bad")]
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(
            b.clone(),
            "x".repeat(http::MAX_BODY + 1),
            &[("Authorization", &auth)]
        )
        .await
        .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let bad = post(b.clone(), "{".into(), &[("Authorization", &auth)]).await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let result: Value =
        serde_json::from_slice(&to_bytes(bad.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(result["error"]["code"], -32700);
    assert_eq!(
        post(b.clone(), body.clone(), &[("Authorization", &auth)])
            .await
            .status(),
        StatusCode::OK
    );
    b.disabled.store(true, Ordering::Relaxed);
    assert_eq!(
        post(b, body, &[("Authorization", &auth)]).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn http_notifications_have_no_body_and_get_is_optional() {
    let b = Arc::new(Fake::default());
    let auth = format!("Bearer {TOKEN}");
    let response = post(
        b.clone(),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
        &[("Authorization", &auth)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(to_bytes(response.into_body(), 100)
        .await
        .unwrap()
        .is_empty());
    let app = http::router(Arc::new(Server::new(b)));
    let request = Request::builder()
        .uri("/mcp")
        .header("Host", "localhost:59940")
        .header("Authorization", auth)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
}

#[tokio::test]
async fn long_polls_cannot_consume_control_request_capacity() {
    #[derive(Default)]
    struct Waiting {
        started: AtomicUsize,
        release: AtomicBool,
    }
    impl Backend for Waiting {
        fn token(&self) -> Option<String> {
            Some(TOKEN.into())
        }
        fn call(&self, name: &str, _: &Map<String, Value>) -> ToolResult {
            if name == "wait_for_event" {
                self.started.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(3);
                while !self.release.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(success(json!({"tool":name})))
        }
    }
    fn tool_request(name: &str, arguments: Value) -> Request<Body> {
        Request::builder()
            .uri("/mcp")
            .method("POST")
            .header("Host", "127.0.0.1:59940")
            .header("Authorization", format!("Bearer {TOKEN}"))
            .header("Content-Type", "application/json")
            .body(Body::from(
                request("tools/call", json!({"name":name,"arguments":arguments})).to_string(),
            ))
            .unwrap()
    }
    let backend = Arc::new(Waiting::default());
    let app = http::router(Arc::new(Server::new(backend.clone())));
    let mut waits = Vec::new();
    for _ in 0..4 {
        waits.push(tokio::spawn(app.clone().oneshot(tool_request(
            "wait_for_event",
            json!({"session":"test","cursor":0,"timeout_ms":1000}),
        ))));
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while backend.started.load(Ordering::SeqCst) != 4 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(backend.started.load(Ordering::SeqCst), 4);
    let overflow = app
        .clone()
        .oneshot(tool_request(
            "wait_for_event",
            json!({"session":"test","cursor":0}),
        ))
        .await
        .unwrap();
    assert_eq!(overflow.status(), StatusCode::TOO_MANY_REQUESTS);
    let control = tokio::time::timeout(
        Duration::from_millis(500),
        app.oneshot(tool_request(
            "cancel_process",
            json!({"session":"test","job_id":"build"}),
        )),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(control.status(), StatusCode::OK);
    backend.release.store(true, Ordering::SeqCst);
    for wait in waits {
        assert_eq!(wait.await.unwrap().unwrap().status(), StatusCode::OK);
    }
}

#[test]
fn events_resume_filter_and_report_eviction() {
    let events = Events::default();
    for n in 0..300 {
        events.push(if n % 2 == 0 { "a" } else { "b" }, json!({"n":n}));
    }
    let result = events.read(0, Some("a"), Duration::ZERO, || true).unwrap();
    assert_eq!(result["truncated"], true);
    assert_eq!(result["next_cursor"], 300);
    assert_eq!(result["events"].as_array().unwrap().len(), 128);
    assert_eq!(
        events.read(300, None, Duration::ZERO, || true).unwrap()["timed_out"],
        true
    );
    assert!(events.read(301, None, Duration::ZERO, || true).is_err());
}

#[test]
fn event_wait_wakes_and_revocation_ends_wait() {
    let events = Arc::new(Events::default());
    let sender = events.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        sender.push("ready", json!({}));
    });
    assert_eq!(
        events
            .read(0, Some("ready"), Duration::from_secs(1), || true)
            .unwrap()["events"][0]["type"],
        "ready"
    );
    worker.join().unwrap();
    let start = Instant::now();
    assert!(events
        .read(1, None, Duration::from_secs(1), || start.elapsed()
            < Duration::from_millis(15))
        .is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
}

#[test]
fn terminal_bytes_preserve_split_unicode_and_bound_memory() {
    let mut log = ByteLog::default();
    let bytes = "终端😀".as_bytes();
    log.append(&bytes[..4]);
    log.append(&bytes[4..]);
    let (first, next, truncated) = log.read(0, 4).unwrap();
    let (rest, end, _) = log.read(next, 100).unwrap();
    assert_eq!([first, rest].concat(), bytes);
    assert!(!truncated);
    assert_eq!(end, bytes.len() as u64);
    log.append(&vec![b'x'; 2 * 1024 * 1024]);
    let (data, _, truncated) = log.read(0, 2 * 1024 * 1024).unwrap();
    assert_eq!(data.len(), 1024 * 1024);
    assert!(truncated);
    assert!(log.read(u64::MAX, 1).is_err());
}

#[test]
fn pixels_handle_bgra_stride_and_negative_display_origins() {
    let rgba = pixels::pack_rgba(&[3, 2, 1, 255, 9, 9, 9, 9, 6, 5, 4, 255], 1, 2, 8, true).unwrap();
    assert_eq!(rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    assert!(pixels::pack_rgba(&[], usize::MAX, 1, 4, false).is_err());
    assert!(pixels::pack_rgba(&[1; 4], 1, 2, 4, false).is_err());
    assert!(pixels::pack_rgba(&[1; 4], 1, 1, 3, false).is_err());
    assert_eq!(
        pixels::remote_point((-1920, -200), (1920, 1080), (20, 30)).unwrap(),
        (-1900, -170)
    );
    assert!(pixels::remote_point((0, 0), (1920, 1080), (1920, 0)).is_err());
}
