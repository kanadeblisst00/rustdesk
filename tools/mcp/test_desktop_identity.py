import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import package_desktop as package


def native_info(platform="linux"):
    result = dict(app_name=package.APP_NAME, bundle_id=package.BUNDLE_ID,
                  mcp_endpoint="http://127.0.0.1:59940/mcp", default_direct_access_port="59942", lan_port=59943,
                  config_dir="/test/.config/rustdeskmcp", log_dir="/test/.local/share/logs/RustDeskMCP",
                  ipc="/tmp/RustDeskMCP-501/ipc", service_ipc="/tmp/RustDeskMCP-service/ipc_service")
    if platform == "macos":
        result.update(config_dir="/test/Library/Preferences/com.carriez.RustDeskMCP",
                      log_dir="/test/Library/Logs/RustDeskMCP")
    elif platform == "windows":
        result.update(config_dir=r"C:\test\AppData\Roaming\RustDeskMCP\config",
                      log_dir=r"C:\test\AppData\Roaming\RustDeskMCP\log",
                      ipc=r"\\.\pipe\RustDeskMCP\query", service_ipc=r"\\.\pipe\RustDeskMCP\query_service")
    return result


class DesktopIdentityTest(unittest.TestCase):
    @unittest.skipUnless(shutil.which("cmake"), "requires CMake for desktop shell identity checks")
    def test_cmake_selects_identity_and_restores_stock_on_next_build(self):
        for platform in ["linux", "windows"]:
            source = (package.ROOT / "flutter" / platform / "CMakeLists.txt").read_text()
            start = source.index('set(BINARY_NAME "rustdesk")')
            settings = source[start:source.index("endif()", start) + len("endif()")]
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                (root / "CMakeLists.txt").write_text(
                    'cmake_minimum_required(VERSION 3.14)\nproject(identity NONE)\n' + settings +
                    '\nget_directory_property(definitions COMPILE_DEFINITIONS)\n'
                    'file(WRITE "${CMAKE_BINARY_DIR}/identity.txt" "${BINARY_NAME}|${APPLICATION_ID}|${definitions}")\n')
                for flag in ["1", "0"]:
                    subprocess.run([shutil.which("cmake"), "-S", str(root), "-B", str(root / "build")],
                                   env=dict(os.environ, RUSTDESK_MCP_BUILD=flag),
                                   check=True, capture_output=True, text=True)
                    result = (root / "build/identity.txt").read_text().split("|")
                    self.assertEqual(result[0], "rustdeskmcp" if flag == "1" else "rustdesk")
                    self.assertEqual("RUSTDESK_MCP_BUILD" in result[2], flag == "1")
                    if platform == "linux":
                        self.assertEqual(result[1], package.BUNDLE_ID if flag == "1" else "com.carriez.flutter_hbb")

    def test_native_identity_rejects_stock_and_partial_renames(self):
        for platform in ["linux", "macos", "windows"]:
            info = native_info(platform)
            package.validate_native_info(info)
            for key in info:
                with self.subTest(platform=platform, key=key):
                    bad = dict(info, **{key: "RustDesk"})
                    with self.assertRaises(ValueError):
                        package.validate_native_info(bad)

    def test_flutter_environment_is_scoped_to_child_process(self):
        before = dict(os.environ)
        with patch.object(package.subprocess, "run") as run:
            package.flutter_build(["flutter", "build", "linux", "--release"], cwd="flutter")
            self.assertEqual(run.call_args.kwargs["env"]["RUSTDESK_MCP_BUILD"], "1")
            self.assertTrue(run.call_args.kwargs["check"])
        self.assertEqual(dict(os.environ), before)

    @unittest.skipIf(os.name == "nt", "Debian staging requires POSIX permissions and symlinks")
    def test_debian_payload_and_lifecycle_do_not_own_stock_files(self):
        for arch in ["amd64", "arm64"]:
            with self.subTest(arch=arch), tempfile.TemporaryDirectory() as temp:
                bundle = Path(temp) / "bundle"
                bundle.mkdir()
                (bundle / "rustdeskmcp").write_text("fixture executable")
                (bundle / "lib").mkdir()
                (bundle / "lib/librustdesk.so").write_text("fixture library")
                (bundle / "lib/alias.so").symlink_to("librustdesk.so")
                stage = Path(temp) / "stage"
                result = subprocess.CompletedProcess([], 0, json.dumps(native_info()), "")
                with patch.object(package.subprocess, "run", return_value=result) as run:
                    package.stage_linux(bundle, stage, "1.5.0", arch)
                    self.assertEqual(run.call_args.args[0][1], "--mcp-identity-info")
                self.assertTrue((stage / "usr/bin/rustdeskmcp").resolve().is_file())
                self.assertTrue((stage / "usr/share/rustdeskmcp/lib/alias.so").is_symlink())
                for official in ["usr/bin/rustdesk", "usr/share/rustdesk", "usr/share/applications/rustdesk.desktop"]:
                    self.assertFalse((stage / official).exists())
                control = (stage / "DEBIAN/control").read_text()
                self.assertIn("Package: rustdesk-mcp\n", control)
                self.assertIn("Architecture: " + arch + "\n", control)
                for field in ["Conflicts:", "Replaces:", "Provides:"]:
                    self.assertNotIn(field, control)
                link = (stage / "usr/share/applications/rustdeskmcp-link.desktop").read_text()
                self.assertIn("x-scheme-handler/rustdeskmcp;", link)
                for name in ["postinst", "prerm", "postrm"]:
                    script = stage / "DEBIAN" / name
                    subprocess.run(["sh", "-n", str(script)], check=True)
                    self.assertTrue(script.stat().st_mode & 0o111)
                    body = script.read_text()
                    self.assertNotIn("libsciter", body)
                    self.assertNotIn("rustdesk.service", body)
                    self.assertNotIn("/root/.config/rustdesk\n", body)
                (bundle / "rustdesk").write_text("stale stock executable")
                with self.assertRaisesRegex(ValueError, "Stale stock"):
                    package.stage_linux(bundle, Path(temp) / "bad", "1.5.0", arch)

    def test_macos_renames_executable_and_signs_only_after_validation(self):
        for valid in [True, False]:
            with self.subTest(valid=valid), tempfile.TemporaryDirectory() as temp:
                app = Path(temp) / "Release/RustDesk.app"
                (app / "Contents/MacOS").mkdir(parents=True)
                (app / "Contents/MacOS/RustDesk").write_text("fixture executable")
                (app / "Contents/MacOS/service").write_text("fixture service")
                original = dict(CFBundleIdentifier=package.BUNDLE_ID if valid else "com.carriez.rustdesk",
                                CFBundleExecutable="RustDesk", CFBundleName="RustDesk",
                                CFBundleURLTypes=[dict(CFBundleURLSchemes=["rustdesk"])])
                with (app / "Contents/Info.plist").open("wb") as stream:
                    plistlib.dump(original, stream)
                result = subprocess.CompletedProcess([], 0, json.dumps(native_info("macos")), "")
                with patch.object(package.subprocess, "run", return_value=result) as run:
                    if not valid:
                        with self.assertRaisesRegex(ValueError, "Rebuild Flutter"):
                            package.package_macos(app)
                        run.assert_not_called()
                        continue
                    package.package_macos(app)
                    calls = [c.args[0] for c in run.call_args_list]
                    self.assertEqual(calls[0][1], "--mcp-identity-info")
                    self.assertEqual(Path(calls[1][-1]).name, "service")
                    self.assertEqual(calls[-1][1:5], ["--verify", "--deep", "--strict", str(app.resolve())])
                target = app.with_name("RustDeskMCP.app")
                with (target / "Contents/Info.plist").open("rb") as stream:
                    info = plistlib.load(stream)
                self.assertEqual(info["CFBundleExecutable"], "RustDeskMCP")
                self.assertEqual(info["CFBundleURLTypes"][0]["CFBundleURLSchemes"], ["rustdeskmcp"])
                self.assertTrue((target / "Contents/MacOS/RustDeskMCP").exists())
                self.assertFalse((target / "Contents/MacOS/RustDesk").exists())


if __name__ == "__main__":
    unittest.main()
