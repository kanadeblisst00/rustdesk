"""Opt-in checks against a built MCP executable; never opens a remote connection."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

EXECUTABLE = os.environ.get("RUSTDESK_MCP_TEST_EXECUTABLE")


@unittest.skipUnless(EXECUTABLE, "set RUSTDESK_MCP_TEST_EXECUTABLE to a built MCP executable")
class ProcessWorkerTest(unittest.TestCase):
    def run_worker(self, code, *, timeout=30000, arguments=(), cancel=False, descendant=False):
        with tempfile.TemporaryDirectory(prefix="mcp-worker-") as directory:
            root = Path(directory)
            job = root / "job"
            job.mkdir(mode=0o700)
            request = {"job_id": "entry-test", "executable": sys.executable,
                       "args": ["-c", code, *arguments], "cwd": str(root), "timeout_ms": timeout}
            (job / "request.json").write_text(json.dumps(request), encoding="utf-8")
            (job / "state.json").write_text(json.dumps({
                "job_id": "entry-test", "state": "starting", "exit_code": None,
                "updated_at_ms": int(time.time() * 1000)}), encoding="utf-8")
            process = subprocess.Popen([EXECUTABLE, "--mcp-process-worker", str(job)],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 30
                cancellation_sent = False
                while time.monotonic() < deadline:
                    state = json.loads((job / "state.json").read_text(encoding="utf-8"))
                    if state["state"] not in ("starting", "running"):
                        break
                    if cancel and state["state"] == "running" and not cancellation_sent:
                        (job / "cancel.json").write_text("{}", encoding="utf-8")
                        cancellation_sent = True
                    time.sleep(0.02)
                else:
                    self.fail("Built executable did not complete its worker entry point")
                process.wait(timeout=5)
                result = {"state": state, "stdout": (job / "stdout.log").read_bytes(),
                          "stderr": (job / "stderr.log").read_bytes()}
                if descendant:
                    self.assertTrue((root / "started").exists(), "Grandchild did not start; cleanup was not exercised")
                    time.sleep(2)
                    self.assertFalse((root / "leaked").exists(), "Grandchild survived task cleanup")
                return result
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)

    def test_streams_exit_code_and_literal_arguments(self):
        result = self.run_worker(
            "import sys; sys.stdout.buffer.write(sys.argv[1].encode('utf-8')); "
            "sys.stderr.write('error-stream'); sys.exit(7)", arguments=("中文 $(literal)",))
        self.assertEqual(result["state"]["state"], "exited")
        self.assertEqual(result["state"]["exit_code"], 7)
        self.assertFalse(result["state"]["success"])
        self.assertEqual(result["stdout"].decode("utf-8"), "中文 $(literal)")
        self.assertEqual(result["stderr"], b"error-stream")

    def test_cancel_running_command(self):
        result = self.run_worker("import time; time.sleep(60)", cancel=True)
        self.assertEqual(result["state"]["state"], "cancelled")

    def test_timeout_cleans_up_grandchild(self):
        child = ("import time,pathlib; pathlib.Path('started').write_text('ready'); "
                 "time.sleep(2); pathlib.Path('leaked').write_text('bad')")
        result = self.run_worker(
            "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',%r]); time.sleep(60)" % child,
            timeout=1500, descendant=True)
        self.assertEqual(result["state"]["state"], "timed_out")


if __name__ == "__main__":
    unittest.main()
