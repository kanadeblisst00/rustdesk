use crate::{error, Server, VERSIONS};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Clone)]
struct HttpState {
    server: Arc<Server>,
    slots: Arc<Semaphore>,
}

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/mcp", any(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(HttpState {
            server,
            slots: Arc::new(Semaphore::new(8)),
        })
}

pub async fn serve(listener: tokio::net::TcpListener, server: Arc<Server>) -> std::io::Result<()> {
    let guard = server.clone();
    axum::serve(listener, router(server))
        .with_graceful_shutdown(async move {
            while guard.backend.token().is_some() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
}

fn token_equal(expected: &str, actual: &str) -> bool {
    let mut diff = expected.len() ^ actual.len();
    for (i, byte) in expected.bytes().enumerate() {
        diff |= (byte ^ actual.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    diff == 0
}

async fn handle(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Browser origins are intentionally unsupported. Native MCP clients do not send Origin.
    if headers.contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (hostname, valid_port) = match host.split_once(':') {
        Some((name, port)) => (name, port.parse::<u16>().is_ok_and(|port| port != 0)),
        None => (host, true),
    };
    if !valid_port || !matches!(hostname, "localhost" | "127.0.0.1") {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(token) = state.server.backend.token().filter(|t| t.len() >= 32) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !token_equal(&token, supplied) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
        )
            .into_response();
    }
    if method != Method::POST {
        return (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST")]).into_response();
    }
    if let Some(v) = headers.get("MCP-Protocol-Version") {
        if !v.to_str().is_ok_and(|v| VERSIONS.contains(&v)) {
            return StatusCode::BAD_REQUEST.into_response();
        }
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/json"))
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let message = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error(serde_json::Value::Null, -32700, "Parse error")),
            )
                .into_response()
        }
    };
    let Ok(permit) = state.slots.clone().try_acquire_owned() else {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    };
    let server = state.server;
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if server.backend.token().as_deref() != Some(&token) {
            return None;
        }
        server.dispatch(message)
    });
    match tokio::time::timeout(Duration::from_secs(65), work).await {
        Ok(Ok(Some(value))) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Ok(Ok(None)) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}
