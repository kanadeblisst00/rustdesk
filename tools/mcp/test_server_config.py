import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

from verify_server_config import source_config, verify


ROOT = Path(__file__).resolve().parents[2]


class ServerConfigTest(unittest.TestCase):
    def test_crlf_checkout_is_normalized_before_applying_patch(self):
        original = subprocess.check_output(
            ["git", "-C", str(ROOT / "libs/hbb_common"), "show", "HEAD:src/config.rs"])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            repository = root / "repository"
            source = repository / "src/config.rs"
            source.parent.mkdir(parents=True)
            source.write_bytes(original)
            patch = repository / "server.diff"
            patch.write_bytes((ROOT / ".github/patches/agent-mcp-server.diff").read_bytes())
            subprocess.run(["git", "init", "-q", str(repository)], check=True)
            files = ["src/config.rs", "server.diff"]
            subprocess.run(["git", "-c", "core.autocrlf=false", "add", "--", *files],
                           cwd=repository, check=True)
            for mode in ["true", "false"]:
                output = root / mode
                subprocess.run(["git", "checkout-index", "--prefix=" + str(output) + "/",
                                "--", *files], cwd=repository, check=True, env={
                                    **os.environ,
                                    "GIT_CONFIG_COUNT": "1",
                                    "GIT_CONFIG_KEY_0": "core.autocrlf",
                                    "GIT_CONFIG_VALUE_0": mode,
                                })
                patch = output / "server.diff"
                source = output / "src/config.rs"
                result = subprocess.run(["git", "apply", "--check", str(patch)],
                                        cwd=output, capture_output=True)
                if mode == "true":
                    self.assertIn(b"\r\n", patch.read_bytes())
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(b"corrupt patch", result.stderr)
                    self.assertIn(b"18", result.stderr)
                else:
                    self.assertNotIn(b"\r\n", patch.read_bytes())
                    self.assertNotIn(b"\r\n", source.read_bytes())
                    self.assertEqual(result.returncode, 0, result.stderr)
                    subprocess.run(["git", "apply", str(patch)], cwd=output, check=True)
                    expected = json.loads((ROOT / "tools/mcp/server-config.json").read_text())
                    verify(source_config(source.read_text()), expected)

    def test_patch_applies_to_pinned_submodule_and_matches_manifest(self):
        original = subprocess.check_output(
            ["git", "-C", str(ROOT / "libs/hbb_common"), "show", "HEAD:src/config.rs"])
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "src/config.rs"
            source.parent.mkdir()
            source.write_bytes(original)
            subprocess.run(["git", "apply", str(ROOT / ".github/patches/agent-mcp-server.diff")],
                           cwd=directory, check=True)
            expected = json.loads((ROOT / "tools/mcp/server-config.json").read_text())
            verify(source_config(source.read_text()), expected)

    def test_unpatched_source_is_rejected(self):
        with self.assertRaises(ValueError):
            source_config('pub const RS_PUB_KEY: &str = "upstream";')

    def test_mismatched_native_config_is_rejected(self):
        expected = json.loads((ROOT / "tools/mcp/server-config.json").read_text())
        for field in expected:
            with self.subTest(field=field), self.assertRaises(ValueError):
                verify({**expected, field: "wrong"}, expected)

    def test_invalid_public_key_is_rejected(self):
        config = {"public_key": "YWJj"}
        with self.assertRaises(ValueError):
            verify(config, config)


if __name__ == "__main__":
    unittest.main()
