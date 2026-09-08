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
            raise ValueError("MCP request exceeds 1 MiB")
        connection = http.client.HTTPConnection(self.host, self.port, timeout=70)
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
                raise ValueError("RustDesk MCP HTTP status %d" % response.status)
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
    except (ValueError, OSError, http.client.HTTPException):
        # A failed reply does not prove that the action failed. Never replay it here.
        if isinstance(message, dict) and "id" not in message:
            print("MCP notification could not be forwarded", file=sys.stderr)
            return
        emit_forward_error(message)


def emit_forward_error(message, code=-32000):
    request_id = message.get("id") if isinstance(message, dict) else None
    emit({"jsonrpc": "2.0", "id": request_id, "error": {
        "code": code, "message": "Unable to forward MCP request; verify RustDesk service, token and request size"
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
                    raise ValueError("MCP request exceeds 1 MiB")
                message = json.loads(line)
            except ValueError as exc:
                emit_forward_error(None, -32700 if isinstance(exc, json.JSONDecodeError) else -32000)
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
