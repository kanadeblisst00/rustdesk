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
    def run_worker(self, code, *, timeout=30000, arguments=(), cancel=False, descendant=False, request_overrides=None):
        with tempfile.TemporaryDirectory(prefix="mcp-worker-") as directory:
            root = Path(directory)
            job = root / "job"
            job.mkdir(mode=0o700)
            request = {"job_id": "entry-test", "executable": sys.executable,
                       "args": ["-c", code, *arguments], "cwd": str(root), "timeout_ms": timeout}
            if request_overrides:
                request.update(request_overrides)
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
                    if process.poll() is not None:
                        break
                    if cancel and (root / "started").is_file() and not cancellation_sent:
                        (job / "cancel.json").write_text("{}", encoding="utf-8")
                        cancellation_sent = True
                    time.sleep(0.02)
                else:
                    self.fail("Built executable did not complete its worker entry point")
                process.wait(timeout=5)
                self.assertEqual(process.returncode, 0, "Worker exited unsuccessfully")
                # Python's Windows file handles conflict with the worker's state replacement.
                state = json.loads((job / "state.json").read_text(encoding="utf-8"))
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

    def test_empty_environment_and_removal_reach_the_real_worker(self):
        result = self.run_worker(
            "import os; print(repr(os.environ.get('MCP_EMPTY_VALUE'))); print('PATH' in os.environ)",
            request_overrides={"env": [{"name": "MCP_EMPTY_VALUE", "value": ""}], "unset_env": ["PATH"]})
        self.assertTrue(result["state"]["success"], result)
        self.assertEqual(result["stdout"].splitlines(), [b"''", b"False"])

    @unittest.skipUnless(sys.platform == "win32", "Windows explicit shell grammar")
    def test_windows_shells_preserve_quoted_paths_and_scripts(self):
        with tempfile.TemporaryDirectory(prefix="mcp shell space ") as directory:
            script = Path(directory) / "build environment.cmd"
            script.write_text("@echo off\necho quoted-path-ok\nexit /b 0\n", encoding="ascii")
            result = self.run_worker("", request_overrides={
                "executable": os.environ.get("COMSPEC", "cmd.exe"), "shell": "cmd",
                "args": ['call "%s"' % script]})
            self.assertTrue(result["state"]["success"], result)
            self.assertIn(b"quoted-path-ok", result["stdout"])
        result = self.run_worker("", request_overrides={
            "executable": "powershell.exe", "shell": "powershell",
            "args": ["Write-Output 'space and \"literal quote\"'; exit 7"]})
        self.assertEqual(result["state"]["exit_code"], 7, result)
        self.assertIn(b'space and "literal quote"', result["stdout"])

    def test_cancel_running_command(self):
        result = self.run_worker(
            "import time,pathlib; pathlib.Path('started').write_text('ready'); time.sleep(60)",
            cancel=True)
        self.assertEqual(result["state"]["state"], "cancelled")

    @unittest.skipUnless(sys.platform == "win32", "Windows environment script")
    def test_environment_script_and_direct_argv(self):
        with tempfile.TemporaryDirectory(prefix="mcp setup space ") as directory:
            script = Path(directory) / "setup environment.cmd"
            script.write_text('@echo off\nset "MCP_SETUP_VALUE=from-script"\nset "MCP_SETUP_REMOVE=present"\n'
                              'set "MCP_SETUP_ARGUMENT=%~1"\necho setup-output\nexit /b 0\n', encoding="ascii")
            literal = 'spaces & percent% quote" caret^ 中文'
            result = self.run_worker(
                "import os,sys,json; print(json.dumps([os.environ.get('MCP_SETUP_VALUE'), "
                "os.environ.get('MCP_SETUP_REMOVE'),os.environ.get('MCP_SETUP_ARGUMENT'),sys.argv[1]]))",
                arguments=[literal], request_overrides={
                    "environment_script": {"path": str(script), "args": ["x64 argument"]},
                    "env": [{"name": "MCP_SETUP_VALUE", "value": "override"}],
                    "unset_env": ["MCP_SETUP_REMOVE"]})
            self.assertTrue(result["state"]["success"], result)
            self.assertEqual(result["state"]["setup_exit_code"], 0)
            self.assertEqual(json.loads(result["stdout"]), ["override", None, "x64 argument", literal])

    @unittest.skipUnless(sys.platform == "win32", "Windows setup failure/timeout")
    def test_environment_failure_and_timeout_never_launch_command(self):
        with tempfile.TemporaryDirectory(prefix="mcp setup ") as directory:
            script = Path(directory) / "fail.cmd"
            for body, options, expected in [
                ("@exit /b 7\n", {}, "failed"),
                ("@ping -n 10 127.0.0.1 >nul\n", {"timeout_ms": 100}, "timed_out"),
            ]:
                script.write_text(body, encoding="ascii")
                result = self.run_worker("print('must-not-start')", request_overrides={
                    "environment_script": {"path": str(script), **options}})
                self.assertEqual(result["state"]["state"], expected, result)
                self.assertEqual(result["stdout"], b"")
                self.assertNotIn("pid", result["state"])
            script.write_text("@echo ready>started\n@ping -n 10 127.0.0.1 >nul\n", encoding="ascii")
            result = self.run_worker("print('must-not-start')", cancel=True, request_overrides={
                "environment_script": {"path": str(script)}})
            self.assertEqual(result["state"]["state"], "cancelled", result)
            self.assertEqual(result["stdout"], b"")

    def test_timeout_cleans_up_grandchild(self):
        child = ("import time,pathlib; pathlib.Path('started').write_text('ready'); "
                 "time.sleep(2); pathlib.Path('leaked').write_text('bad')")
        result = self.run_worker(
            "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',%r]); time.sleep(60)" % child,
            timeout=1500, descendant=True)
        self.assertEqual(result["state"]["state"], "timed_out")

    def test_cancel_cleans_up_grandchild(self):
        child = ("import time,pathlib; pathlib.Path('started').write_text('ready'); "
                 "time.sleep(2); pathlib.Path('leaked').write_text('bad')")
        result = self.run_worker(
            "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',%r]); time.sleep(60)" % child,
            cancel=True, descendant=True)
        self.assertEqual(result["state"]["state"], "cancelled")

    @unittest.skipUnless(sys.platform == "win32", "Windows Job Object cleanup")
    def test_worker_termination_cleans_up_process_tree(self):
        with tempfile.TemporaryDirectory(prefix="mcp-worker-exit-") as directory:
            root = Path(directory)
            job = root / "job"
            job.mkdir(mode=0o700)
            child = ("import time,pathlib; pathlib.Path('started').write_text('ready'); "
                     "time.sleep(2); pathlib.Path('leaked').write_text('bad')")
            code = "import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',%r]); time.sleep(60)" % child
            (job / "request.json").write_text(json.dumps({
                "job_id": "exit-test", "executable": sys.executable,
                "args": ["-c", code], "cwd": str(root), "timeout_ms": 30000,
            }), encoding="utf-8")
            (job / "state.json").write_text(json.dumps({
                "job_id": "exit-test", "state": "starting", "exit_code": None,
                "updated_at_ms": int(time.time() * 1000),
            }), encoding="utf-8")
            process = subprocess.Popen([EXECUTABLE, "--mcp-process-worker", str(job)],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 10
                while not (root / "started").is_file():
                    self.assertIsNone(process.poll(), "Worker exited before its grandchild started")
                    self.assertLess(time.monotonic(), deadline, "Grandchild did not start")
                    time.sleep(0.02)
                process.kill()
                process.wait(timeout=5)
                time.sleep(2.5)
                self.assertFalse((root / "leaked").exists(), "Worker exit left its process tree alive")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
