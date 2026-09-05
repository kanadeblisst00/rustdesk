import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("rustdesk_build", ROOT / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class BuildFlagTest(unittest.TestCase):
    def test_macos_packaging_is_only_changed_for_isolated_feature(self):
        for features, isolated in [('flutter,mcp', False), ('flutter,mcp,mcp-isolated', True)]:
            with self.subTest(features=features), patch.object(build, 'skip_cargo', True), \
                    patch.object(build.os, 'chdir'), patch.object(build, 'system2') as run, \
                    patch.object(build.subprocess, 'run') as package:
                build.build_flutter_dmg('test', features)
                commands = [call.args[0] for call in run.call_args_list]
                flutter_command = next(command for command in commands if 'flutter build macos' in command)
                self.assertEqual('cn.ikanade.RustDeskMCPTest' in flutter_command, isolated)
                self.assertEqual(package.called, isolated)
                if isolated:
                    self.assertTrue(package.call_args.args[0][1].endswith('package_macos_isolated.py'))

    def test_isolated_mcp_requires_macos_and_mcp(self):
        with patch.object(build, 'osx', False):
            args = build.make_parser().parse_args(['--flutter', '--mcp', '--mcp-isolated'])
            with self.assertRaisesRegex(Exception, 'macOS only'):
                build.get_features(args)
        with patch.object(build, 'osx', True):
            args = build.make_parser().parse_args(['--flutter', '--mcp-isolated'])
            with self.assertRaisesRegex(Exception, 'requires --flutter --mcp'):
                build.get_features(args)
            args = build.make_parser().parse_args(['--flutter', '--mcp', '--mcp-isolated'])
            self.assertEqual(build.get_features(args), ['flutter', 'mcp', 'mcp-isolated'])

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
