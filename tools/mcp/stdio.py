#!/usr/bin/env python3
"""Forward MCP stdio to a RustDesk desktop's authenticated IPv4 endpoint."""

import argparse
import http.client
import ipaddress
import json
import os
import sys
import threading
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from urllib.parse import urlsplit

MAX_REQUEST = 1024 * 1024
MAX_RESPONSE = 96 * 1024 * 1024
EMIT_LOCK = threading.Lock()


class ForwardError(ValueError):
    def __init__(self, kind, message, **details):
        super().__init__(message)
        self.kind = kind
        self.details = details


class Proxy:
    def __init__(self, url, token):
        target = urlsplit(url)
        hostname = target.hostname
        if hostname == "localhost":
            hostname = "127.0.0.1"
        try:
            address = ipaddress.IPv4Address(hostname)
        except (ValueError, ipaddress.AddressValueError):
            raise ValueError("MCP URL must use localhost or an IPv4 address") from None
        if (
            target.scheme != "http"
            or address.is_unspecified or address.is_multicast
            or address == ipaddress.IPv4Address("255.255.255.255")
            or address.packed[0] == 0
            or target.path != "/mcp"
            or target.query
            or target.fragment
            or target.username
            or target.password
        ):
            raise ValueError("MCP URL must be http://IPv4:PORT/mcp with a reachable host address")
        if not 32 <= len(token) <= 256 or any(ord(c) < 33 or ord(c) > 126 for c in token):
            raise ValueError("Set RUSTDESK_MCP_TOKEN to the token from RustDesk MCP settings")
        self.port = target.port if target.port is not None else 80
        if self.port == 0:
            raise ValueError("MCP URL port must be between 1 and 65535")
        self.host = str(address)
        self.token = token
        self.version = "2025-03-26"

    def forward(self, message):
        payload = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if len(payload) > MAX_REQUEST:
            raise ForwardError("payload_too_large", "MCP request exceeds 1 MiB; use write_workspace_file chunks", max_bytes=MAX_REQUEST)
        timeout = 70
        if isinstance(message, dict) and message.get("method") == "tools/call":
            params = message.get("params", {})
            if isinstance(params, dict) and params.get("name") == "connect_device":
                arguments = params.get("arguments", {})
                requested = arguments.get("timeout_ms", 12000) if isinstance(arguments, dict) else 12000
                if isinstance(requested, (int, float)) and not isinstance(requested, bool):
                    timeout = max(timeout, min(120000, max(0, requested)) / 1000 + 35)
        connection = http.client.HTTPConnection(self.host, self.port, timeout=timeout)
        try:
            connection.request("POST", "/mcp", body=payload, headers={
                "Authorization": "Bearer " + self.token,
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
                "MCP-Protocol-Version": self.version,
            })
            response = connection.getresponse()
            if response.status == 202:
                return None
            if response.status != 200:
                kind, detail = {
                    401: ("need_reauth", "RustDesk MCP rejected the controller token; refresh credentials and reconnect the plugin"),
                    413: ("payload_too_large", "MCP request exceeds the HTTP body limit; use write_workspace_file chunks"),
                    429: ("service_busy", "RustDesk MCP request capacity is full; wait before another request"),
                    503: ("service_unavailable", "RustDesk MCP service is disabled, stopping or unavailable"),
                }.get(response.status, ("http_error", "RustDesk MCP HTTP request failed"))
                raise ForwardError(kind, detail, http_status=response.status)
            data = response.read(MAX_RESPONSE + 1)
            if len(data) > MAX_RESPONSE:
                raise ValueError("MCP response exceeds 96 MiB")
            result = json.loads(data)
            if not isinstance(result, dict) or result.get("jsonrpc") != "2.0":
                raise ValueError("Invalid MCP response")
            if isinstance(message, dict) and result.get("id") != message.get("id"):
                raise ValueError("MCP response ID mismatch")
            if isinstance(message, dict) and message.get("method") == "initialize":
                self.version = result.get("result", {}).get("protocolVersion", self.version)
            return result
        finally:
            connection.close()


def emit(message):
    payload = json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n"
    with EMIT_LOCK:
        sys.stdout.buffer.write(payload.encode("utf-8"))
        sys.stdout.buffer.flush()


def forward_and_emit(proxy, message):
    try:
        response = proxy.forward(message)
        if response is not None:
            emit(response)
    except (ValueError, OSError, http.client.HTTPException) as exc:
        # A failed reply does not prove that the action failed. Never replay it here.
        if isinstance(message, dict) and "id" not in message:
            print("MCP notification could not be forwarded", file=sys.stderr)
            return
        if isinstance(exc, ForwardError):
            emit_forward_error(message, kind=exc.kind, detail=str(exc), details=exc.details)
        elif isinstance(exc, (OSError, http.client.HTTPException)):
            emit_forward_error(message, kind="controller_unreachable", detail="Cannot reach the RustDesk MCP controller or read its response; check the service and plugin transport")
        else:
            emit_forward_error(message, kind="invalid_response", detail="Invalid response from RustDesk MCP controller")


def emit_forward_error(message, code=-32000, *, kind="invalid_request", detail="Invalid MCP request", details=None):
    request_id = message.get("id") if isinstance(message, dict) else None
    emit({"jsonrpc": "2.0", "id": request_id, "error": {
        "code": code, "message": detail, "data": {
            "kind": kind, **(details or {}), "request_replayed": False,
            "remote_job_state": "unknown", "recovery": "Transport failure does not cancel durable jobs. Reconnect to the same device and OS identity; query the SAME job_id before retrying."
        }
    }})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=os.environ.get("RUSTDESK_MCP_URL", "http://127.0.0.1:59940/mcp"))
    parser.add_argument("--max-parallel", type=int, choices=range(1, 9), default=8,
                        help="Maximum concurrent HTTP calls (default 8); responses may arrive out of order")
    args = parser.parse_args()
    try:
        proxy = Proxy(args.url, os.environ.get("RUSTDESK_MCP_TOKEN", ""))
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    pending = deque()
    with ThreadPoolExecutor(max_workers=args.max_parallel, thread_name_prefix="mcp-http") as pool:
        while True:
            line = sys.stdin.buffer.readline(MAX_REQUEST + 1)
            if not line:
                return 0  # The executor drains accepted requests before closing stdout.
            try:
                if len(line) > MAX_REQUEST:
                    while line and not line.endswith(b"\n"):
                        line = sys.stdin.buffer.readline(MAX_REQUEST + 1)
                    raise ForwardError("payload_too_large", "MCP request exceeds 1 MiB; use write_workspace_file chunks", max_bytes=MAX_REQUEST)
                message = json.loads(line)
            except ValueError as exc:
                if isinstance(exc, ForwardError):
                    emit_forward_error(None, kind=exc.kind, detail=str(exc), details=exc.details)
                else:
                    emit_forward_error(None, -32700, kind="invalid_json", detail="Invalid or truncated JSON request")
                continue
            if isinstance(message, dict) and message.get("method") == "initialize":
                while pending:
                    pending.popleft().result()
                forward_and_emit(proxy, message)
            elif isinstance(message, dict) and "id" not in message:
                forward_and_emit(proxy, message)
            else:
                if len(pending) >= 32:
                    pending.popleft().result()
                pending.append(pool.submit(forward_and_emit, proxy, message))


if __name__ == "__main__":
    sys.exit(main())
