use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use rustdesk_agent_mcp::{context::CallContext, http, success, Backend, Server, ToolResult};
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const REMOTE: &str = "remote-token-012345678901234567890123456789";
const LOCAL: &str = "local-token-0123456789012345678901234567890";
#[derive(Default)]
struct Authenticated {
    calls: Mutex<Vec<(String, bool, String)>>,
}
impl Backend for Authenticated {
    fn token(&self) -> Option<String> {
        Some(REMOTE.into())
    }
    fn authenticate(&self, supplied: &str) -> Option<CallContext> {
        match supplied {
            REMOTE => Some(CallContext::new("remote-owner".into(), "r1".into(), false)),
            LOCAL => Some(CallContext::new("local-owner".into(), "l1".into(), true)),
            _ => None,
        }
    }
    fn call(&self, _: &str, _: &Map<String, Value>) -> ToolResult {
        panic!("HTTP must use authenticated dispatch")
    }
    fn call_authorized(&self, ctx: &CallContext, name: &str, _: &Map<String, Value>) -> ToolResult {
        self.calls
            .lock()
            .unwrap()
            .push((ctx.principal.clone(), ctx.local, name.into()));
        Ok(success(json!({"owner":ctx.principal})))
    }
}
#[tokio::test]
async fn identity_comes_from_authentication_for_tools_and_resources() {
    let backend = Arc::new(Authenticated::default());
    let app = http::router(Arc::new(Server::new(backend.clone())));
    for (token, method, params, owner) in [
        (
            LOCAL,
            "tools/call",
            json!({"name":"get_capabilities","arguments":{}}),
            "local-owner",
        ),
        (
            LOCAL,
            "resources/read",
            json!({"uri":"rustdesk://sessions"}),
            "local-owner",
        ),
        (
            REMOTE,
            "resources/read",
            json!({"uri":"rustdesk://capabilities"}),
            "remote-owner",
        ),
    ] {
        let req=Request::builder().method("POST").uri("/mcp").header("Host","127.0.0.1").header("Content-Type","application/json").header("Authorization",format!("Bearer {token}"))
            .body(Body::from(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params,"principal":"attacker"}).to_string())).unwrap();
        let response = app.clone().oneshot(req).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 10240).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains(owner));
    }
    assert_eq!(
        backend.calls.lock().unwrap().as_slice(),
        &[
            ("local-owner".into(), true, "get_capabilities".into()),
            ("local-owner".into(), true, "list_connections".into()),
            ("remote-owner".into(), false, "get_capabilities".into())
        ]
    );
}

#[test]
fn cancelled_authorized_dispatch_never_reaches_backend() {
    let backend = Arc::new(Authenticated::default());
    let server = Server::new(backend.clone());
    let ctx = CallContext::new("owner".into(), "credential".into(), true);
    ctx.cancel();
    let result=server.dispatch_authorized(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_capabilities","arguments":{}}}),&ctx).unwrap();
    assert_eq!(result["result"]["isError"], true);
    assert!(backend.calls.lock().unwrap().is_empty());
}

#[cfg(feature = "local")]
#[tokio::test]
async fn emergency_stop_has_capacity_when_all_control_slots_are_busy() {
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::{Duration, Instant},
    };
    #[derive(Default)]
    struct Busy {
        started: AtomicUsize,
        stopped: AtomicBool,
    }
    impl Backend for Busy {
        fn token(&self) -> Option<String> {
            Some(REMOTE.into())
        }
        fn call(&self, name: &str, _: &Map<String, Value>) -> ToolResult {
            if name == "stop_local" {
                self.stopped.store(true, Ordering::SeqCst);
                return Ok(success(json!({})));
            }
            self.started.fetch_add(1, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(3);
            while !self.stopped.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(success(json!({})))
        }
    }
    let backend = Arc::new(Busy::default());
    let app = http::router(Arc::new(Server::new(backend.clone())));
    let request = |name: &str| {
        Request::builder().method("POST").uri("/mcp")
        .header("Host","127.0.0.1").header("Content-Type","application/json").header("Authorization",format!("Bearer {REMOTE}"))
        .body(Body::from(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":{}}}).to_string())).unwrap()
    };
    let mut requests = Vec::new();
    for _ in 0..8 {
        requests.push(tokio::spawn(
            app.clone().oneshot(request("get_capabilities")),
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while backend.started.load(Ordering::SeqCst) < 8 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        app.clone()
            .oneshot(request("get_capabilities"))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        app.oneshot(request("stop_local")).await.unwrap().status(),
        StatusCode::OK
    );
    for request in requests {
        assert_eq!(request.await.unwrap().unwrap().status(), StatusCode::OK);
    }
}
