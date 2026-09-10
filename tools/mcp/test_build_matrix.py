import base64
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

import build_matrix as matrix

REVISION = "a" * 40


def target(name="linux"):
    return {"name": name, "device_id": "device-" + name, "source_revision": REVISION,
            "prepare": [{"executable": "git", "args": ["clone", "https://example.invalid/project", "{source}"]}],
            "build": [{"executable": "cmake", "args": ["--build", "{build}"]}],
            "test": [{"executable": "ctest", "args": ["--test-dir", "{build}"]}],
            "artifacts": ["reports/tests.xml"]}


class FakeMCP:
    def __init__(self):
        self.lock = threading.RLock()
        self.calls, self.jobs, self.workspaces, self.sessions = [], {}, {}, {}
        self.fail_build = self.uncertain = self.unknown = self.bad_download = False
        self.auth = self.screenshot_error = False
        self.barrier = None
        self.revision = REVISION
        self.file_job = 0
        self.payload = b"<testsuite/>"

    def call(self, name, **args):
        if name == "screenshot":
            if self.screenshot_error:
                raise matrix.MatrixError("Screenshot unavailable")
            return {"structuredContent": {"width": 1, "height": 1},
                    "content": [{"type": "image", "mimeType": "image/png",
                                 "data": base64.b64encode(b"PNG evidence").decode()}]}
        raise AssertionError(name)

    def data(self, name, **args):
        if name == "connect_device" and args["kind"] == "terminal" and self.barrier:
            self.barrier.wait(timeout=3)
        with self.lock:
            self.calls.append((name, copy.deepcopy(args)))
            if name == "get_capabilities":
                return {"processes": {"supported": True}, "workspaces": {"supported": True}}
            if name == "list_connections":
                return {"connections": [{"session": s} for s in self.sessions]}
            if name == "connect_device":
                session = args["device_id"] + "-" + args["kind"]
                self.sessions[session] = not self.auth
                return {"session": session, "connected": not self.auth, "needs_password": self.auth}
            if name == "get_connection_info":
                return {"session": args["session"], "connected": self.sessions[args["session"]]}
            if name == "input_password":
                self.sessions[args["session"]] = True
                return {"queued": True}
            if name == "disconnect_device":
                self.sessions.pop(args["session"], None)
                return {"disconnected": True}
            if name == "get_environment":
                return {"os": "fake", "version_verified": False}
            if name == "create_workspace":
                key = args["workspace_id"]
                return copy.deepcopy(self.workspaces.setdefault(key, {"paths": {
                    p: "/remote/" + key + ("/" + p if p != "root" else "")
                    for p in ("root", "source", "build", "artifacts", "reports")}}))
            if name == "seal_workspace":
                info = self.workspaces[args["workspace_id"]]
                info["source"] = {"sha256": "b" * 64}
                return copy.deepcopy(info)
            if name in ("run_process", "run_workspace_process"):
                job = args["job_id"]
                request = {k: v for k, v in args.items() if k != "session"}
                if job not in self.jobs:
                    revision = "rev-parse" in args["args"]
                    failed = self.fail_build and args["executable"] == "cmake"
                    output = (self.revision + "\n").encode() if revision else "中文".encode() + b"\xffx" * 40000
                    self.jobs[job] = {"request": request, "stdout": output, "stderr": b"diagnostic\n",
                                      "state": {"job_id": job, "state": "unknown" if self.unknown else "exited",
                                                "exit_code": 7 if failed else 0, "success": not failed,
                                                "source_unchanged": True}}
                    if self.uncertain and args["executable"] == "cmake":
                        self.uncertain = False
                        raise matrix.MatrixError("Transport outcome unknown")
                elif self.jobs[job]["request"] != request:
                    raise matrix.MatrixError("Conflicting job ID")
                return copy.deepcopy(self.jobs[job]["state"])
            if name == "get_process_status":
                return copy.deepcopy(self.jobs[args["job_id"]]["state"])
            if name == "read_process_output":
                data = self.jobs[args["job_id"]][args["stream"]]
                offset = args["offset"]
                chunk = data[offset:offset + args["max_bytes"]]
                return {"offset": offset, "next_offset": offset + len(chunk),
                        "data_base64": base64.b64encode(chunk).decode(), "eof": offset + len(chunk) >= len(data)}
            if name == "get_artifact_manifest":
                return {"files": [{"path": args["paths"][0], "remote_path": "/remote/test-report",
                                    "bytes": len(self.payload), "sha256": hashlib.sha256(self.payload).hexdigest()}]}
            if name == "file_transfer":
                self.file_job += 1
                Path(args["destination"]).write_bytes(b"bad" if self.bad_download else self.payload)
                return {"job_id": self.file_job, "after_cursor": 3}
            if name == "wait_for_event":
                return {"next_cursor": 4, "events": [{"type": "job_done", "data": {"id": str(self.file_job)}}]}
            if name == "file_cancel_job":
                return {"queued": True}
            raise AssertionError(name)


class MatrixTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.directory = Path(self.tmp.name) / "run"
        self.addCleanup(self.tmp.cleanup)
        self.fake = FakeMCP()

    def run_matrix(self, targets=None, **kwargs):
        return matrix.run_matrix(self.fake, {"version": 1, "targets": targets or [target()]},
                                 "run-1", self.directory, poll=0, **kwargs)

    def test_parallel_targets_and_complete_binary_logs(self):
        self.fake.barrier = threading.Barrier(2)
        result = self.run_matrix([target("linux"), target("windows")], parallel=2)
        self.assertTrue(result["success"])
        self.assertEqual(len(result["targets"]), 2)
        self.assertEqual(len(self.fake.jobs), 8)
        for job, data in self.fake.jobs.items():
            copies = list(self.directory.glob("*/" + job + ".stdout.log"))
            self.assertEqual(copies[0].read_bytes(), data["stdout"])
        suite = ET.parse(self.directory / "matrix.junit.xml").getroot()
        self.assertEqual(suite.get("failures"), "0")
        self.assertEqual(len(self.fake.sessions), 0)

    def test_explicit_log_policy_reaches_worker_and_rejects_unknown_policy(self):
        spec = target()
        spec["build"][0]["log_limit_policy"] = "terminate"
        self.assertTrue(self.run_matrix([spec])["success"])
        commands = [args for name, args in self.fake.calls if name == "run_workspace_process"]
        self.assertEqual(commands[0]["log_limit_policy"], "terminate")
        spec["build"][0]["log_limit_policy"] = "ignore"
        with self.assertRaisesRegex(matrix.MatrixError, "log_limit_policy"):
            matrix.validate({"version": 1, "targets": [spec]})

    def test_resume_uncertain_submission_reuses_ids_and_log_offsets(self):
        self.fake.uncertain = True
        first = self.run_matrix()
        self.assertFalse(first["success"])
        jobs = set(self.fake.jobs)
        second = self.run_matrix()
        self.assertTrue(second["success"])
        self.assertTrue(jobs < set(self.fake.jobs))
        self.assertEqual(len(self.fake.jobs), 4)
        self.assertEqual(sum(name == "seal_workspace" for name, _ in self.fake.calls), 1)
        for job, data in self.fake.jobs.items():
            self.assertEqual((self.directory / "linux" / (job + ".stdout.log")).read_bytes(), data["stdout"])

    def test_failed_build_stops_test_but_collects_artifacts_and_screenshot(self):
        self.fake.fail_build = True
        item = target()
        item["screenshot_on_failure"] = True
        result = self.run_matrix([item])
        self.assertFalse(result["success"])
        self.assertFalse(any(d["request"]["executable"] == "ctest" for d in self.fake.jobs.values()))
        self.assertEqual(len(result["targets"][0]["artifacts"]), 1)
        self.assertTrue((self.directory / "linux" / "failure.png").is_file())
        self.assertIn("build-0", result["targets"][0]["error"])

    def test_one_target_failure_does_not_stop_another_target(self):
        self.fake.fail_build = True
        good = target("windows")
        good["build"][0]["executable"] = "cmake.exe"
        result = self.run_matrix([target("linux"), good], parallel=2)
        self.assertEqual([r["success"] for r in result["targets"]], [False, True])
        self.assertEqual(ET.parse(self.directory / "matrix.junit.xml").getroot().get("failures"), "1")

    def test_authentication_credentials_are_not_persisted_and_reused_sessions_not_closed(self):
        self.fake.auth = True
        self.fake.sessions["device-linux-terminal"] = False
        item = target()
        item["auth"] = {"password_env": "MATRIX_TEST_PASSWORD", "os_password_env": "MATRIX_TEST_OS_PASSWORD"}
        with patch.dict(os.environ, {"MATRIX_TEST_PASSWORD": "secret-remote", "MATRIX_TEST_OS_PASSWORD": "secret-os"}):
            result = self.run_matrix([item])
        self.assertTrue(result["success"])
        for path in self.directory.rglob("*.json"):
            text = path.read_text()
            self.assertNotIn("secret-remote", text)
            self.assertNotIn("secret-os", text)
        auth = [args for name, args in self.fake.calls if name == "input_password"][0]
        self.assertEqual(auth["os_password"], "secret-os")
        self.assertIn("device-linux-terminal", self.fake.sessions)

    def test_revision_mismatch_and_unknown_state_do_not_advance(self):
        self.fake.revision = "c" * 40
        result = self.run_matrix()
        self.assertFalse(result["success"])
        self.assertFalse(any(n == "seal_workspace" for n, _ in self.fake.calls))
        self.assertIn("differs", result["targets"][0]["error"])
        self.fake.unknown = True
        self.fake.jobs.clear()
        self.directory = Path(self.tmp.name) / "unknown-run"
        result = self.run_matrix()
        self.assertFalse(result["success"])
        self.assertEqual(len(self.fake.jobs), 1)
        self.assertIn("unknown", result["targets"][0]["error"])

    def test_removed_recorded_jobs_are_not_recreated(self):
        self.run_matrix()
        self.fake.jobs.clear()
        result = self.run_matrix()
        self.assertFalse(result["success"])
        self.assertEqual(len(self.fake.jobs), 0)

    def test_stopped_controller_does_not_submit_or_cancel_commands(self):
        stop = threading.Event()
        stop.set()
        run = matrix.TargetRun(self.fake, target(), "run", "digest", self.directory, stop=stop)
        result = run.run()
        self.assertFalse(result["success"])
        self.assertIn("Controller stopped", result["error"])
        self.assertEqual(self.fake.calls, [])

    def test_screenshot_failure_preserves_original_command_error(self):
        self.fake.fail_build = self.fake.screenshot_error = True
        item = target()
        item["screenshot_on_failure"] = True
        result = self.run_matrix([item])
        self.assertIn("build-0", result["targets"][0]["error"])
        self.assertIn("Screenshot unavailable", result["targets"][0]["evidence_error"])

    def test_download_checks_hash_and_resume_skips_verified_file(self):
        result = self.run_matrix(download=True)
        self.assertTrue(result["success"])
        self.assertEqual((self.directory / "linux/downloads/reports/tests.xml").read_bytes(), self.fake.payload)
        self.run_matrix(download=True)
        self.assertEqual(sum(n == "file_transfer" for n, _ in self.fake.calls), 1)

    def test_bad_download_fails_and_does_not_publish_artifact(self):
        self.fake.bad_download = True
        result = self.run_matrix(download=True)
        self.assertFalse(result["success"])
        self.assertFalse((self.directory / "linux/downloads/reports/tests.xml").exists())
        self.assertTrue(any(n == "file_cancel_job" for n, _ in self.fake.calls))

    def test_manifest_change_is_rejected_before_remote_calls(self):
        self.run_matrix()
        calls = len(self.fake.calls)
        changed = target()
        changed["test"][0]["args"].append("different")
        with self.assertRaises(matrix.MatrixError):
            self.run_matrix([changed])
        self.assertEqual(len(self.fake.calls), calls)

    def test_exclusions_are_sent_at_seal_and_validation_failure_keeps_success(self):
        item = target()
        item["source_excludes"] = ["dist", ".cache"]
        self.assertTrue(self.run_matrix([item])["success"])
        seal = [args for name, args in self.fake.calls if name == "seal_workspace"][0]
        self.assertEqual(seal["source_excludes"], ["dist", ".cache"])
        build = next(job for job in self.fake.jobs.values() if job["request"]["executable"] == "cmake")
        build["state"].update(source_unchanged=None, workspace_error="Source manifest exceeds 4096 entries",
                              source_verification={"state": "error"})
        result = self.run_matrix([item])
        report = result["targets"][0]
        self.assertFalse(result["success"])
        self.assertTrue(report["steps"]["build-0"]["success"])
        self.assertEqual(report["steps"]["build-0"]["source_verification"]["state"], "error")
        self.assertIn("command succeeded; workspace verification could not complete", report["error"])
        for path in ["../dist", "dist/", "dist/*", "", "C:dist"]:
            item["source_excludes"] = [path]
            with self.assertRaises(matrix.MatrixError):
                matrix.validate({"version": 1, "targets": [item]})

    def test_paths_unknown_keys_and_implicit_empty_test_stage_are_rejected(self):
        for path in ("../secret", "/tmp/file", "reports/../../secret", "reports/x\\y", "reports/C:file", "reports//file"):
            item = target()
            item["artifacts"] = [path]
            with self.assertRaises(matrix.MatrixError):
                matrix.validate({"version": 1, "targets": [item]})
        for changes in ({"test": []}, {"source_revision": "main"}, {"install": []}, {"name": "../bad"}):
            with self.assertRaises(matrix.MatrixError):
                matrix.validate({"version": 1, "targets": [{**target(), **changes}]})
        for targets in ([target("CON")], [target("Linux"), target("linux")]):
            with self.assertRaises(matrix.MatrixError):
                matrix.validate({"version": 1, "targets": targets})

    def test_literal_arguments_and_windows_paths_are_preserved(self):
        item = target()
        literal = "$(whoami) " + chr(96) + "uname" + chr(96) + " 中文; & |"
        item["build"] = [{"executable": "C:\\Program Files\\CMake\\bin\\cmake.exe",
                          "args": ["--build", "{build}", literal], "env": {"VALUE": literal}}]
        self.run_matrix([item])
        request = [d["request"] for d in self.fake.jobs.values()
                   if d["request"]["executable"].startswith("C:")][0]
        self.assertEqual(request["args"][-1], literal)
        self.assertEqual(request["env"][0]["value"], literal)
        self.assertEqual(request["cwd"], "build")

    def test_environment_script_is_validated_and_forwarded(self):
        item = target()
        setup = {"path": "C:\\Program Files\\VS\\vcvars64.bat", "args": ["x64"], "timeout_ms": 30000}
        item["build"][0]["environment_script"] = setup
        self.assertTrue(self.run_matrix([item])["success"])
        build = next(job for job in self.fake.jobs.values() if job["request"]["executable"] == "cmake")
        self.assertEqual(build["request"]["environment_script"], setup)
        for invalid in [{"path": "x.exe"}, {"path": "x.cmd", "args": ["%PATH%"]},
                        {"path": "x.cmd", "timeout_ms": 0}, {"path": "x.cmd", "extra": True}]:
            item["build"][0]["environment_script"] = invalid
            with self.assertRaises(matrix.MatrixError):
                matrix.validate({"version": 1, "targets": [item]})

    def test_remote_error_content_is_not_echoed(self):
        class Proxy:
            def forward(self, _):
                return {"result": {"isError": True, "content": [{"type": "text", "text": "secret-value"}]}}
        with self.assertRaises(matrix.MatrixError) as error:
            matrix.Client(Proxy()).data("input_password", password="secret-value")
        self.assertNotIn("secret-value", str(error.exception))

    def test_validate_cli_needs_no_token_and_bootstrap_requires_explicit_flag(self):
        item = target()
        item["bootstrap"] = [{"executable": "python", "args": ["-m", "pip", "install", "pytest"]}]
        path = Path(self.tmp.name) / "manifest.json"
        path.write_text(json.dumps({"version": 1, "targets": [item]}))
        args = [sys.executable, str(Path(matrix.__file__)), str(path), "--run-id", "example"]
        env = {k: v for k, v in os.environ.items() if k != "RUSTDESK_MCP_TOKEN"}
        result = subprocess.run(args + ["--validate"], env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run(args, env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--allow-bootstrap", result.stderr)


if __name__ == "__main__":
    unittest.main()
