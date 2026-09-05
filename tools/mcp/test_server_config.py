import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from verify_server_config import source_config, verify


ROOT = Path(__file__).resolve().parents[2]


class ServerConfigTest(unittest.TestCase):
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
