import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import unittest
from unittest.mock import patch, MagicMock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SCRIPT = Path(__file__).with_name("stdio.py")
spec = importlib.util.spec_from_file_location("mcp_stdio", SCRIPT)
proxy_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proxy_module)
TOKEN = "a" * 64


class Handler(BaseHTTPRequestHandler):
    seen = []
    release_wait = threading.Event()

    def do_POST(self):
        message = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.seen.append((message, self.headers["Authorization"], self.headers["MCP-Protocol-Version"]))
        if message.get("method") == "test/slow":
            self.release_wait.wait(3)
        if message.get("method") == "test/fast":
            self.release_wait.set()
        if "id" not in message:
            self.send_response(202)
            self.end_headers()
            return
        result = {"protocolVersion": "2025-11-25"} if message["method"] == "initialize" else {}
        body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class ProxyTest(unittest.TestCase):
    def test_address_and_token_validation(self):
        for url in ("http://evil.test/mcp", "https://127.0.0.1/mcp", "http://localhost/mcp?x=1",
                    "http://user:secret@localhost/mcp", "http://localhost/other",
                    "http://0.0.0.0:59940/mcp", "http://[::1]/mcp", "http://224.0.0.1/mcp",
                    "http://255.255.255.255/mcp", "http://localhost:0/mcp", "http://127.1/mcp",
                    "http://192.168.1.20:65536/mcp", "http://0.1.2.3/mcp"):
            with self.assertRaises(ValueError):
                proxy_module.Proxy(url, TOKEN)
        for token in ("short", "x" * 32 + "\nInjected: header", " " * 32, "x" * 257):
            with self.assertRaises(ValueError):
                proxy_module.Proxy("http://localhost:59940/mcp", token)

    def test_lan_forwarding_uses_selected_host_and_bearer_token(self):
        for host in ("192.168.1.20", "10.0.0.12", "172.16.1.2", "127.0.0.1", "localhost"):
            with self.subTest(host=host), patch.object(proxy_module.http.client, "HTTPConnection") as connection:
                response = MagicMock(status=200)
                response.read.return_value = b'{"jsonrpc":"2.0","id":1,"result":{}}'
                connection.return_value.getresponse.return_value = response
                proxy = proxy_module.Proxy("http://%s:60000/mcp" % host, TOKEN)
                proxy.forward({"jsonrpc": "2.0", "id": 1, "method": "ping"})
                connection.assert_called_once_with("127.0.0.1" if host == "localhost" else host, 60000, timeout=70)
                self.assertEqual(connection.return_value.request.call_args.kwargs["headers"]["Authorization"], "Bearer " + TOKEN)
                connection.return_value.close.assert_called_once()

    def test_stdio_forwarding_initialization_notifications_and_clean_stdout(self):
        Handler.seen = []
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            messages = [
                {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-11-25"}},
                {"jsonrpc": "2.0", "method": "notifications/initialized"},
                {"jsonrpc": "2.0", "id": "中文", "method": "ping"},
            ]
            for encoding in ("utf-8", "cp1252", "ascii"):
                with self.subTest(encoding=encoding):
                    Handler.seen = []
                    output = subprocess.run(
                        [sys.executable, str(SCRIPT), "--url", "http://127.0.0.1:%d/mcp" % server.server_port],
                        input="\n".join(json.dumps(m, ensure_ascii=False) for m in messages) + "\n",
                        encoding="utf-8", capture_output=True,
                        env={**os.environ, "RUSTDESK_MCP_TOKEN": TOKEN, "PYTHONIOENCODING": encoding}, timeout=10)
                    self.assertEqual(output.returncode, 0, output.stderr)
                    replies = [json.loads(line) for line in output.stdout.splitlines()]
                    self.assertEqual([r["id"] for r in replies], [0, "中文"])
                    self.assertEqual(len(Handler.seen), 3)
                    self.assertTrue(all(r[1] == "Bearer " + TOKEN for r in Handler.seen))
                    self.assertEqual(Handler.seen[-1][2], "2025-11-25")
                    self.assertNotIn(TOKEN, output.stdout + output.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_missing_token_has_no_stdout(self):
        output = subprocess.run([sys.executable, str(SCRIPT)], input="", text=True, capture_output=True,
                                env={k: v for k, v in os.environ.items() if k != "RUSTDESK_MCP_TOKEN"})
        self.assertEqual(output.returncode, 2)
        self.assertEqual(output.stdout, "")

    def test_slow_call_does_not_block_control_and_eof_drains_replies(self):
        Handler.release_wait.clear()
        Handler.seen = []
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            messages = [
                {"jsonrpc": "2.0", "id": "init", "method": "initialize"},
                {"jsonrpc": "2.0", "id": "slow", "method": "test/slow"},
                {"jsonrpc": "2.0", "id": "fast", "method": "test/fast"},
            ]
            output = subprocess.run(
                [sys.executable, str(SCRIPT), "--url", "http://127.0.0.1:%d/mcp" % server.server_port],
                input="\n".join(json.dumps(m) for m in messages) + "\n", text=True, capture_output=True,
                env={**os.environ, "RUSTDESK_MCP_TOKEN": TOKEN}, timeout=2)
            self.assertEqual(output.returncode, 0, output.stderr)
            replies = [json.loads(line) for line in output.stdout.splitlines()]
            self.assertEqual(replies[0]["id"], "init")
            self.assertEqual({r["id"] for r in replies}, {"init", "slow", "fast"})
            self.assertEqual(len(Handler.seen), 3)
            self.assertTrue(all(item[2] == "2025-11-25" for item in Handler.seen[1:]))
            self.assertNotIn(TOKEN, output.stdout + output.stderr)
        finally:
            Handler.release_wait.set()
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
