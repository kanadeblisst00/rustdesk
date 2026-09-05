import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("rustdesk_build", ROOT / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class BuildFlagTest(unittest.TestCase):
    def test_mcp_requires_flutter(self):
        args = build.make_parser().parse_args(["--mcp"])
        with self.assertRaisesRegex(Exception, "--mcp requires --flutter"):
            build.get_features(args)

    def test_opt_in_does_not_change_existing_feature_paths(self):
        for flags, expected in [([], ["inline"]), (["--flutter"], ["flutter"]),
                                (["--flutter", "--mcp"], ["flutter", "mcp"])]:
            features = build.get_features(build.make_parser().parse_args(flags))
            self.assertEqual(features, expected)


if __name__ == "__main__":
    unittest.main()
