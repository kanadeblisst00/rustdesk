use crate::{error, Server, VERSIONS};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Clone)]
struct HttpState {
    server: Arc<Server>,
    slots: Arc<Semaphore>,
    wait_slots: Arc<Semaphore>,
    listen_address: Option<SocketAddrV4>,
}

pub fn router(server: Arc<Server>) -> Router {
    Router::new()
        .route("/mcp", any(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(HttpState {
            server,
            slots: Arc::new(Semaphore::new(8)),
            wait_slots: Arc::new(Semaphore::new(4)),
            listen_address: None,
        })
}

pub fn router_on(server: Arc<Server>, address: SocketAddrV4) -> Router {
    if *address.ip() == Ipv4Addr::LOCALHOST {
        return router(server);
    }
    Router::new()
        .route("/mcp", any(handle))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(HttpState {
            server,
            slots: Arc::new(Semaphore::new(8)),
            wait_slots: Arc::new(Semaphore::new(4)),
            listen_address: Some(address),
        })
}

pub async fn serve(listener: tokio::net::TcpListener, server: Arc<Server>) -> std::io::Result<()> {
    let guard = server.clone();
    let app = match listener.local_addr()? {
        SocketAddr::V4(address) => router_on(server, address),
        SocketAddr::V6(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "MCP requires an IPv4 listen address",
            ))
        }
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            while guard.backend.token().is_some() {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
}

pub fn valid_token(token: &str) -> bool {
    (32..=256).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}

pub fn parse_listen_address(value: &str, default_port: u16) -> Result<SocketAddrV4, &'static str> {
    let value = value.trim();
    let address = if value.is_empty() {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, default_port)
    } else if let Ok(ip) = value.parse::<Ipv4Addr>() {
        SocketAddrV4::new(ip, default_port)
    } else {
        value
            .parse::<SocketAddrV4>()
            .map_err(|_| "Use an IPv4 address, optionally followed by :port")?
    };
    if address.port() == 0
        || address.ip().is_multicast()
        || address.ip().is_broadcast()
        || (address.ip().octets()[0] == 0 && !address.ip().is_unspecified())
    {
        return Err("Invalid MCP listen address or port");
    }
    Ok(address)
}

pub fn client_endpoint(address: SocketAddrV4) -> String {
    let ip = if address.ip().is_unspecified() {
        Ipv4Addr::LOCALHOST
    } else {
        *address.ip()
    };
    format!("http://{ip}:{}/mcp", address.port())
}

fn allowed_host(host: &str, address: Option<SocketAddrV4>) -> bool {
    let (hostname, port) = match host.split_once(':') {
        Some((name, port)) => match port.parse::<u16>() {
            Ok(port) if port != 0 => (name, Some(port)),
            _ => return false,
        },
        None => (host, None),
    };
    let Some(address) = address else {
        return matches!(hostname, "localhost" | "127.0.0.1");
    };
    if port.unwrap_or(80) != address.port() {
        return false;
    }
    if hostname == "localhost" {
        return address.ip().is_unspecified() || address.ip().is_loopback();
    }
    let Ok(ip) = hostname.parse::<Ipv4Addr>() else {
        return false;
    };
    if ip.octets()[0] == 0 || ip.is_multicast() || ip.is_broadcast() {
        return false;
    }
    address.ip().is_unspecified() || *address.ip() == ip
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
    if headers.get_all(header::HOST).iter().count() != 1
        || !allowed_host(host, state.listen_address)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(token) = state.server.backend.token().filter(|t| valid_token(t)) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if headers.get_all(header::AUTHORIZATION).iter().count() != 1 || !token_equal(&token, supplied)
    {
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
    let message: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error(serde_json::Value::Null, -32700, "Parse error")),
            )
                .into_response()
        }
    };
    let waiting = message.get("method").and_then(serde_json::Value::as_str) == Some("tools/call")
        && message
            .pointer("/params/name")
            .and_then(serde_json::Value::as_str)
            == Some("wait_for_event");
    let slots = if waiting {
        state.wait_slots
    } else {
        state.slots
    };
    let Ok(permit) = slots.try_acquire_owned() else {
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
