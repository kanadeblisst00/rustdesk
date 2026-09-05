#!/usr/bin/env python3
"""Forward MCP stdio to the enabled RustDesk desktop's authenticated local endpoint."""

import argparse
import http.client
import json
import os
import sys
from urllib.parse import urlsplit

MAX_REQUEST = 1024 * 1024
MAX_RESPONSE = 96 * 1024 * 1024


class Proxy:
    def __init__(self, url, token):
        target = urlsplit(url)
        if (
            target.scheme != "http"
            or target.hostname not in ("127.0.0.1", "localhost")
            or target.path != "/mcp"
            or target.query
            or target.fragment
            or target.username
            or target.password
        ):
            raise ValueError("MCP URL must be http://127.0.0.1:PORT/mcp")
        if len(token) < 32 or any(ord(c) < 32 or ord(c) > 126 for c in token):
            raise ValueError("Set RUSTDESK_MCP_TOKEN to the token from RustDesk MCP settings")
        self.port = target.port or 80
        self.token = token
        self.version = "2025-03-26"

    def forward(self, message):
        payload = json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if len(payload) > MAX_REQUEST:
            raise ValueError("MCP request exceeds 1 MiB")
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=70)
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
    sys.stdout.write(json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=os.environ.get("RUSTDESK_MCP_URL", "http://127.0.0.1:59940/mcp"))
    args = parser.parse_args()
    try:
        proxy = Proxy(args.url, os.environ.get("RUSTDESK_MCP_TOKEN", ""))
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2
    while True:
        line = sys.stdin.buffer.readline(MAX_REQUEST + 1)
        if not line:
            return 0
        message = None
        try:
            if len(line) > MAX_REQUEST:
                while line and not line.endswith(b"\n"):
                    line = sys.stdin.buffer.readline(MAX_REQUEST + 1)
                raise ValueError("MCP request exceeds 1 MiB")
            message = json.loads(line)
            response = proxy.forward(message)
            if response is not None:
                emit(response)
        except (ValueError, OSError, http.client.HTTPException) as exc:
            # Never retry an action: the remote side may have executed it before a connection failed.
            if isinstance(message, dict) and "id" not in message:
                print("MCP notification could not be forwarded", file=sys.stderr)
                continue
            request_id = message.get("id") if isinstance(message, dict) else None
            code = -32700 if isinstance(exc, json.JSONDecodeError) else -32000
            emit({"jsonrpc": "2.0", "id": request_id, "error": {
                "code": code, "message": "Unable to forward MCP request; verify RustDesk service, token and request size"
            }})


if __name__ == "__main__":
    sys.exit(main())
