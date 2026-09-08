#!/usr/bin/env python3
"""Run a pinned build/test matrix through RustDesk MCP (controller Python 3.9+)."""

import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path, PurePosixPath
import re
import sys
import tempfile
import threading
import time
import uuid
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import xml.etree.ElementTree as ET

from stdio import Proxy

STAGES = ("preflight", "prepare", "bootstrap", "build", "test")
FINAL = {"exited", "failed", "cancelled", "timed_out", "log_limit"}
SAFE_ID = re.compile(r"[A-Za-z0-9_-]{1,64}\Z")
RESERVED = {"CON", "PRN", "AUX", "NUL", *(f"COM{n}" for n in range(1, 10)),
            *(f"LPT{n}" for n in range(1, 10))}


class MatrixError(Exception):
    pass


def valid_id(value):
    return isinstance(value, str) and SAFE_ID.fullmatch(value) and value.upper() not in RESERVED


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=True, separators=(",", ":"))


def save(path, value):
    with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                     delete=False) as stream:
        temporary = Path(stream.name)
        try:
            json.dump(value, stream, ensure_ascii=True, indent=2)
            stream.flush()
            os.fsync(stream.fileno())
        except BaseException:
            temporary.unlink(missing_ok=True)
            raise
    os.replace(temporary, path)


@contextmanager
def run_lock(directory):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (directory / ".lock").open("a+b") as stream:
        if stream.seek(0, os.SEEK_END) == 0:
            stream.write(b"0")
            stream.flush()
        stream.seek(0)
        try:
            if os.name == "nt":
                import msvcrt
                msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            raise MatrixError("This run directory is already in use") from None
        yield


def validate(manifest):
    def fields(obj, allowed, required=()):
        if not isinstance(obj, dict) or set(obj) - set(allowed) or set(required) - set(obj):
            raise MatrixError("Invalid or unknown manifest fields")

    fields(manifest, ("version", "targets"), ("version", "targets"))
    targets = manifest["targets"]
    if manifest["version"] != 1 or not isinstance(targets, list) or not 1 <= len(targets) <= 16:
        raise MatrixError("Expected manifest version 1 with 1–16 targets")
    names, devices = set(), set()
    for target in targets:
        fields(target, ("name", "device_id", "source_revision", "git", "auth", "executables",
                        "artifacts", "screenshot_on_failure", *STAGES),
               ("name", "device_id", "source_revision", "prepare", "build", "test"))
        if not valid_id(target["name"]):
            raise MatrixError("Target names must be safe IDs of at most 64 characters")
        if (target["name"].lower() in names or not isinstance(target["device_id"], str)
                or not target["device_id"] or target["device_id"] in devices):
            raise MatrixError("Target names and device IDs must be unique within a matrix")
        names.add(target["name"].lower())
        devices.add(target["device_id"])
        if not isinstance(target["source_revision"], str) or not re.fullmatch(
                r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", target["source_revision"]):
            raise MatrixError("source_revision must be a full Git commit hash")
        if not isinstance(target.get("git", "git"), str) or not target.get("git", "git"):
            raise MatrixError("git must be an executable path or name")
        auth = target.get("auth", {})
        fields(auth, ("password_env", "os_username_env", "os_password_env"))
        if any(not isinstance(v, str) or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", v)
               for v in auth.values()):
            raise MatrixError("Authentication fields must name controller environment variables")
        executables = target.get("executables", [])
        if (not isinstance(executables, list) or len(executables) > 32
                or any(not isinstance(v, str) or v in (".", "..") or not re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", v)
                       for v in executables)):
            raise MatrixError("Invalid executable discovery list")
        if not isinstance(target.get("screenshot_on_failure", False), bool):
            raise MatrixError("screenshot_on_failure must be boolean")
        paths = target.get("artifacts", [])
        if not isinstance(paths, list) or len(paths) > 64 or len(set(map(str, paths))) != len(paths):
            raise MatrixError("Expected at most 64 unique artifact paths")
        for path in paths:
            if (not isinstance(path, str) or not path or "\\" in path or ":" in path
                    or "\0" in path or any(p in ("", ".", "..") for p in path.split("/"))
                    or path.split("/")[0] not in ("artifacts", "reports", "build")
                    or len(path.split("/")) < 2):
                raise MatrixError("Artifacts must be relative files under artifacts/, reports/ or build/")
        for stage in STAGES:
            commands = target.get(stage, [])
            if not isinstance(commands, list) or len(commands) > 32:
                raise MatrixError("Each stage accepts at most 32 commands")
            if stage in ("prepare", "build", "test") and not commands:
                raise MatrixError("prepare, build and test must each declare at least one command")
            for command in commands:
                fields(command, ("executable", "args", "cwd", "env", "timeout_ms", "max_log_bytes"),
                       ("executable",))
                if (not isinstance(command["executable"], str) or not command["executable"]
                        or "\0" in command["executable"]):
                    raise MatrixError("Command executable is required")
                args = command.get("args", [])
                env = command.get("env", {})
                if (not isinstance(args, list) or len(args) > 256
                        or any(not isinstance(v, str) or "\0" in v for v in args)
                        or not isinstance(env, dict) or len(env) > 128
                        or any(not isinstance(v, str) or "\0" in v or not k or "=" in k or "\0" in k
                               for k, v in env.items())
                        or len({k.upper() for k in env}) != len(env)):
                    raise MatrixError("Invalid command arguments or environment overrides")
                cwd = command.get("cwd", "build" if stage in ("build", "test") else "{root}")
                if not isinstance(cwd, str) or not cwd or "\0" in cwd:
                    raise MatrixError("Command cwd must be a path")
                if stage in ("build", "test") and (
                        "{" in cwd or "\\" in cwd or ":" in cwd
                        or any(p in ("", ".", "..") for p in cwd.split("/"))):
                    raise MatrixError("Build/test cwd must be workspace-relative, such as source or build")
                for key, default, low, high in (("timeout_ms", 3600000, 100, 86400000),
                                               ("max_log_bytes", 16777216, 1024, 268435456)):
                    value = command.get(key, default)
                    if type(value) is not int or not low <= value <= high:
                        raise MatrixError("Invalid command timeout or log limit")
    return manifest


class Client:
    def __init__(self, proxy):
        self.proxy = proxy

    def initialize(self):
        try:
            result = self.proxy.forward({"jsonrpc": "2.0", "id": "matrix-init", "method": "initialize",
                                         "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                                                    "clientInfo": {"name": "rustdesk-build-matrix", "version": "1"}}})
            if not isinstance(result, dict) or "error" in result:
                raise MatrixError("MCP initialization rejected")
            self.proxy.forward({"jsonrpc": "2.0", "method": "notifications/initialized"})
        except (OSError, ValueError, http.client.HTTPException):
            raise MatrixError("MCP initialization failed; verify endpoint and token") from None

    def call(self, name, **arguments):
        try:
            response = self.proxy.forward({"jsonrpc": "2.0", "id": str(uuid.uuid4()),
                                           "method": "tools/call",
                                           "params": {"name": name, "arguments": arguments}})
        except (OSError, ValueError, http.client.HTTPException):
            raise MatrixError(name + ": transport outcome unknown; resume with the same run ID and directory") from None
        result = response.get("result", {}) if isinstance(response, dict) else {}
        if not result or result.get("isError") or "error" in response:
            # Remote errors can echo command arguments or authentication values.
            raise MatrixError(name + ": MCP rejected the request; inspect that tool with the recorded session/job ID")
        return result

    def data(self, name, **arguments):
        result = self.call(name, **arguments).get("structuredContent")
        if not isinstance(result, dict):
            raise MatrixError(name + ": missing structured result")
        return result


class TargetRun:
    def __init__(self, client, target, run_id, digest, directory, download=False, poll=1, stop=None):
        self.client, self.target, self.directory = client, target, directory
        self.seed = run_id + ":" + digest + ":" + target["name"]
        self.workspace = "matrix-" + hashlib.sha256(self.seed.encode()).hexdigest()[:40]
        self.download, self.poll = download, poll
        self.stop = stop or threading.Event()
        self.sessions, self.opened = {}, []
        self.directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.journal_path = directory / "target.json"
        self.report = (json.loads(self.journal_path.read_text(encoding="utf-8"))
                       if self.journal_path.exists() else
                       {"name": target["name"], "workspace_id": self.workspace, "steps": {}})

    def persist(self):
        save(self.journal_path, self.report)

    def check_stop(self):
        if self.stop.is_set():
            raise MatrixError("Controller stopped; submitted remote jobs remain available for resume/cancel")

    def data(self, name, **args):
        return self.client.data(name, session=self.sessions["terminal"], **args)

    def connect(self, kind):
        self.check_stop()
        if kind in self.sessions:
            return self.sessions[kind]
        previous = self.client.data("list_connections")
        existing = {c["session"] for c in previous.get("connections", [])}
        info = self.client.data("connect_device", device_id=self.target["device_id"], kind=kind,
                                headless=True, timeout_ms=30000)
        session = info["session"]
        self.sessions[kind] = session
        if session not in existing:
            self.opened.append(session)
        self.report["sessions"] = dict(self.sessions)
        self.persist()
        deadline, submitted = time.monotonic() + 60, False
        while not info.get("connected"):
            self.check_stop()
            if info.get("needs_password") and not submitted:
                auth = self.target.get("auth", {})
                if not auth.get("password_env"):
                    raise MatrixError("Authentication required; configure auth.password_env")
                values = {key.removesuffix("_env"): os.environ.get(value, "")
                          for key, value in auth.items()}
                if any(not value for value in values.values()):
                    raise MatrixError("A configured authentication environment variable is empty")
                self.client.data("input_password", session=session, **values)
                submitted = True
            if time.monotonic() >= deadline:
                raise MatrixError("Connection/authentication incomplete after 60 seconds; check permissions or 2FA")
            time.sleep(self.poll)
            info = self.client.data("get_connection_info", session=session)
        return session

    def logs(self, job, final=False):
        for stream in ("stdout", "stderr"):
            path = self.directory / (job + "." + stream + ".log")
            offset = path.stat().st_size if path.exists() else 0
            with path.open("ab") as output:
                while True:
                    page = self.data("read_process_output", job_id=job, stream=stream,
                                     offset=offset, max_bytes=65536)
                    data = base64.b64decode(page["data_base64"], validate=True)
                    if page.get("offset") != offset or page["next_offset"] != offset + len(data):
                        raise MatrixError("Remote log cursor mismatch")
                    output.write(data)
                    output.flush()
                    offset += len(data)
                    if not final or page.get("eof"):
                        break
                    if not data:
                        raise MatrixError("Final log is incomplete")

    def command(self, stage, index, command, paths):
        self.check_stop()
        label = stage + "-" + str(index)
        job = "matrix-" + hashlib.sha256((self.seed + ":" + label).encode()).hexdigest()[:48]
        workspace = stage in ("build", "test")
        def expand(value):
            return re.sub(r"\{(root|source|build|artifacts|reports|revision)\}",
                          lambda match: paths[match[1]], value)
        args = {"job_id": job, "executable": expand(command["executable"]),
                "args": [expand(v) for v in command.get("args", [])],
                "cwd": command.get("cwd", "build") if workspace else expand(command.get("cwd", "{root}")),
                "env": [{"name": k, "value": expand(v)} for k, v in sorted(command.get("env", {}).items())],
                "timeout_ms": command.get("timeout_ms", 3600000),
                "max_log_bytes": command.get("max_log_bytes", 16777216)}
        if workspace:
            args["workspace_id"] = self.workspace
        entry = self.report["steps"].setdefault(label, {"job_id": job})
        if entry.get("state") not in (None, "submission_pending"):
            # A recorded job must still exist. Never recreate a removed job on resume.
            state = self.data("get_process_status", job_id=job)
        else:
            entry["state"] = "submission_pending"
            self.persist()  # Record the durable ID before sending a potentially mutating call.
            state = self.data("run_workspace_process" if workspace else "run_process", **args)
        deadline = time.monotonic() + args["timeout_ms"] / 1000 + 60
        while True:
            entry.update({key: state.get(key) for key in
                          ("state", "exit_code", "success", "source_unchanged", "logs_truncated")})
            self.persist()
            self.check_stop()
            terminal = state["state"] in FINAL
            self.logs(job, final=terminal)
            if terminal:
                if state["state"] != "exited" or state.get("success") is not True:
                    raise MatrixError(label + ": command did not succeed; see retained stdout/stderr")
                if workspace and state.get("source_unchanged") is not True:
                    raise MatrixError(label + ": source integrity was not preserved")
                return job
            if state["state"] == "unknown" or time.monotonic() >= deadline:
                raise MatrixError(label + ": outcome unknown; inspect/resume the same job ID")
            time.sleep(self.poll)
            state = self.data("get_process_status", job_id=job)

    def evidence(self):
        session = self.connect("desktop")
        result = self.client.call("screenshot", session=session, timeout_ms=10000)
        images = [c for c in result.get("content", [])
                  if c.get("type") == "image" and c.get("mimeType") == "image/png"]
        if not images:
            raise MatrixError("No screenshot image returned")
        (self.directory / "failure.png").write_bytes(base64.b64decode(images[0]["data"], validate=True))
        self.report["screenshot"] = {"path": "failure.png", "metadata": result.get("structuredContent")}

    def artifacts(self, job):
        failures, files = [], []
        for path in self.target.get("artifacts", []):
            try:
                manifest = self.data("get_artifact_manifest", workspace_id=self.workspace, job_id=job, paths=[path])
                item = manifest["files"][0]
                if (item["path"] != path or not re.fullmatch(r"[0-9a-f]{64}", item["sha256"])
                        or type(item["bytes"]) is not int or not 0 <= item["bytes"] <= 2 * 1024 ** 3):
                    raise MatrixError("Invalid artifact metadata")
                files.append(manifest)
                if self.download:
                    self.download_file(item)
            except (MatrixError, OSError, ValueError, KeyError, IndexError) as exc:
                failures.append({"path": path, "error": safe_error(exc)})
        self.report["artifacts"] = files
        self.report["artifact_errors"] = failures
        if failures:
            raise MatrixError("Some declared artifacts could not be collected or verified")

    def download_file(self, item):
        destination = self.directory / "downloads" / PurePosixPath(item["path"])
        if destination.is_file() and file_hash(destination) == item["sha256"]:
            return
        destination.parent.mkdir(parents=True, exist_ok=True)
        if destination.exists():
            raise MatrixError("Existing artifact has another checksum; choose a fresh output directory")
        session = self.connect("files")
        # A fresh staging path never overwrites an unrelated file or an incomplete previous download.
        staging = Path(tempfile.mkdtemp(prefix="download-", dir=self.directory)) / "artifact"
        reply = self.client.data("file_transfer", session=session, source=item["remote_path"],
                                 destination=str(staging), direction="download")
        cursor, job = reply["after_cursor"], reply["job_id"]
        deadline = time.monotonic() + 600
        try:
            while time.monotonic() < deadline:
                self.check_stop()
                page = self.client.data("wait_for_event", session=session, cursor=cursor, timeout_ms=10000)
                if page.get("truncated"):
                    raise MatrixError("File completion events were truncated")
                cursor = page["next_cursor"]
                for event in page["events"]:
                    if str(event.get("data", {}).get("id")) != str(job):
                        continue
                    if event["type"] in ("job_error", "override_file_confirm"):
                        raise MatrixError("Artifact transfer failed or unexpectedly requires overwrite")
                    if event["type"] == "job_done":
                        if (not staging.is_file() or staging.stat().st_size != item["bytes"]
                                or file_hash(staging) != item["sha256"]):
                            raise MatrixError("Downloaded artifact checksum/size mismatch")
                        os.replace(staging, destination)
                        staging.parent.rmdir()
                        return
            raise MatrixError("Artifact transfer timed out")
        except (MatrixError, OSError, ValueError, KeyError):
            try:
                self.client.data("file_cancel_job", session=session, job_id=job)
            except MatrixError:
                pass  # Preserve the original failure; fresh staging paths isolate incomplete transfers.
            raise

    def run(self):
        for key in ("error", "evidence_error", "screenshot", "artifacts", "artifact_errors"):
            self.report.pop(key, None)
        self.report["success"] = False
        latest = None
        try:
            self.connect("terminal")
            self.report["environment"] = self.data("get_environment", executables=self.target.get("executables", []))
            info = self.data("create_workspace", workspace_id=self.workspace,
                             source_revision=self.target["source_revision"].lower())
            paths = {**info["paths"], "revision": self.target["source_revision"].lower()}
            for stage in STAGES[:3]:
                for index, command in enumerate(self.target.get(stage, [])):
                    self.command(stage, index, command, paths)
            revision_job = self.command("revision", 0, {"executable": self.target.get("git", "git"),
                                        "args": ["-C", "{source}", "rev-parse", "HEAD"],
                                        "timeout_ms": 30000}, paths)
            revision = (self.directory / (revision_job + ".stdout.log")).read_bytes().strip().lower()
            if revision != paths["revision"].encode("ascii"):
                raise MatrixError("Checked-out Git commit differs from source_revision")
            self.report["git_head_matched_at_prepare"] = True
            if not info.get("source"):
                info = self.data("seal_workspace", workspace_id=self.workspace)
            self.report["source"] = info["source"]
            self.persist()
            for stage in STAGES[3:]:
                for index, command in enumerate(self.target[stage]):
                    latest = "matrix-" + hashlib.sha256((self.seed + ":" + stage + "-" + str(index)).encode()).hexdigest()[:48]
                    self.command(stage, index, command, paths)
            self.report["success"] = True
        except (MatrixError, OSError, ValueError, KeyError) as exc:
            self.report.update(success=False, error=safe_error(exc))
            if self.target.get("screenshot_on_failure") and not self.stop.is_set():
                try:
                    self.evidence()
                except (MatrixError, OSError, ValueError, KeyError) as evidence_error:
                    self.report["evidence_error"] = safe_error(evidence_error)
        finally:
            if latest and not self.stop.is_set():
                try:
                    self.artifacts(latest)
                except MatrixError as exc:
                    self.report.update(success=False)
                    self.report.setdefault("error", str(exc))
            self.report["disconnect_errors"] = []
            for session in reversed(self.opened):
                try:
                    self.client.data("disconnect_device", session=session)
                except MatrixError:
                    self.report["disconnect_errors"].append(session)
            self.persist()
        return self.report


def safe_error(exc):
    return str(exc) if isinstance(exc, MatrixError) else "Invalid response or local I/O error (" + type(exc).__name__ + ")"


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for data in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(data)
    return digest.hexdigest()


def run_matrix(client, manifest, run_id, directory, parallel=3, download=False, poll=1):
    validate(manifest)
    if not valid_id(run_id) or type(parallel) is not int or not 1 <= parallel <= 4:
        raise MatrixError("Invalid run ID or parallelism")
    digest = hashlib.sha256(canonical(manifest).encode()).hexdigest()
    with run_lock(directory):
        identity = {"run_id": run_id, "manifest_sha256": digest}
        identity_path = directory / "run.json"
        if identity_path.exists() and json.loads(identity_path.read_text(encoding="utf-8")) != identity:
            raise MatrixError("Run ID or manifest changed; use a new run ID and output directory")
        save(identity_path, identity)
        capabilities = client.data("get_capabilities")
        if (capabilities.get("read_only") or not capabilities.get("processes", {}).get("supported")
                or not capabilities.get("workspaces", {}).get("supported")):
            raise MatrixError("MCP must support writable process jobs and workspaces")
        stop = threading.Event()
        targets = [TargetRun(client, target, run_id, digest, directory / target["name"], download, poll, stop)
                   for target in manifest["targets"]]
        with ThreadPoolExecutor(max_workers=parallel, thread_name_prefix="mcp-build") as pool:
            futures = [pool.submit(target.run) for target in targets]
            try:
                results = [future.result() for future in futures]
            except KeyboardInterrupt:
                stop.set()
                results = [future.result() for future in futures]
        summary = {**identity, "success": all(r.get("success") for r in results), "targets": results}
        save(directory / "summary.json", summary)
        suite = ET.Element("testsuite", name="RustDesk build matrix", tests=str(len(results)),
                           failures=str(sum(not r.get("success") for r in results)))
        for result in results:
            case = ET.SubElement(suite, "testcase", classname="remote_build", name=result["name"])
            if not result.get("success"):
                ET.SubElement(case, "failure", message=result.get("error", "Target failed"))
            ET.SubElement(case, "system-out").text = result["name"] + "/target.json"
        ET.ElementTree(suite).write(directory / "matrix.junit.xml", encoding="utf-8", xml_declaration=True)
        return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--output", type=Path, default=Path("mcp-runs"))
    parser.add_argument("--parallel", type=int, choices=range(1, 5), default=3)
    parser.add_argument("--allow-bootstrap", action="store_true", help="Execute declared dependency installation commands")
    parser.add_argument("--download-artifacts", action="store_true", help="Requires runner and HTTP MCP on the same host/filesystem")
    parser.add_argument("--validate", action="store_true", help="Validate only; no network or filesystem mutations")
    parser.add_argument("--url", default=os.environ.get("RUSTDESK_MCP_URL", "http://127.0.0.1:59940/mcp"))
    args = parser.parse_args()
    try:
        if not valid_id(args.run_id):
            raise MatrixError("run-id must be a safe ID of at most 64 characters")
        with args.manifest.open("rb") as stream:
            raw = stream.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise MatrixError("Manifest exceeds 1 MiB")
        manifest = validate(json.loads(raw))
        if args.validate:
            print("Manifest valid; no commands executed")
            return 0
        if not args.allow_bootstrap and any(t.get("bootstrap") for t in manifest["targets"]):
            raise MatrixError("Manifest includes bootstrap commands; review them and pass --allow-bootstrap")
        proxy = Proxy(args.url, os.environ.get("RUSTDESK_MCP_TOKEN", ""))
        if args.download_artifacts and proxy.host != "127.0.0.1":
            raise MatrixError("Artifact downloads require a local MCP endpoint and shared filesystem")
        client = Client(proxy)
        client.initialize()
        result = run_matrix(client, manifest, args.run_id, (args.output / args.run_id).resolve(),
                            args.parallel, args.download_artifacts)
        print(canonical({"success": result["success"], "report": str((args.output / args.run_id / "summary.json").resolve())}))
        return 0 if result["success"] else 1
    except (MatrixError, OSError, ValueError, KeyError) as exc:
        print(safe_error(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
