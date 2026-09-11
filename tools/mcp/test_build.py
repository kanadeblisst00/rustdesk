import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("rustdesk_build", ROOT / "build.py")
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class BuildFlagTest(unittest.TestCase):
    def test_macos_packaging_selects_stock_mcp_and_test_identities(self):
        for features, isolated in [('flutter', False), ('flutter,mcp', False), ('flutter,mcp,mcp-isolated', True)]:
            with self.subTest(features=features), patch.object(build, 'skip_cargo', True), \
                    patch.object(build.os, 'chdir'), patch.object(build, 'system2') as run, \
                    patch.object(build.subprocess, 'run') as package:
                build.build_flutter_dmg('test', features)
                commands = [call.args[0] for call in run.call_args_list]
                flutter_command = next(command for command in commands if 'flutter build macos' in command)
                self.assertEqual('cn.ikanade.RustDeskMCPTest' in flutter_command, isolated)
                self.assertEqual(package.called, 'mcp' in features.split(','))
                if isolated:
                    self.assertTrue(package.call_args.args[0][1].endswith('package_macos_isolated.py'))
                elif features == 'flutter,mcp':
                    self.assertIn('com.carriez.RustDeskMCP', flutter_command)
                    self.assertNotIn('FLUTTER_XCODE_PRODUCT_NAME', flutter_command)
                    self.assertTrue(package.call_args.args[0][1].endswith('package_desktop.py'))

    def test_mcp_linux_uses_separate_packaging_for_both_architectures(self):
        for arch, directory in [('amd64', 'x64'), ('arm64', 'arm64')]:
            with self.subTest(arch=arch), patch.object(build, 'skip_cargo', True), \
                    patch.object(build, 'flutter_build_dir', f'build/linux/{directory}/release/bundle/'), \
                    patch.object(build, 'get_deb_arch', return_value=arch), \
                    patch.object(build.mcp_package, 'flutter_build') as flutter, \
                    patch.object(build.mcp_package, 'package_linux') as package:
                build.build_flutter_deb('1.5.0', 'flutter,mcp')
                flutter.assert_called_once_with(['flutter', 'build', 'linux', '--release'], cwd='flutter')
                self.assertEqual(Path(package.call_args.args[0]),
                                 Path('flutter') / f'build/linux/{directory}/release/bundle')
                self.assertEqual(package.call_args.args[1:4],
                                 ('rustdesk-mcp-1.5.0.deb', '1.5.0', arch))

    def test_windows_mcp_renames_payload_and_enables_portable_identity(self):
        for features, mcp in [('flutter', False), ('flutter,mcp', True)]:
            with self.subTest(features=features), patch.object(build, 'skip_cargo', True), \
                    patch.object(build.os, 'chdir'), patch.object(build.os, 'rename'), \
                    patch.object(build.os.path, 'exists', return_value=False), \
                    patch.object(build.shutil, 'copy2'), patch.object(build, 'system2') as run, \
                    patch.object(build.mcp_package, 'flutter_build') as flutter, \
                    patch.object(build.mcp_package, 'verify_binary') as verify:
                build.build_flutter_windows('test', features, False)
                command = next(c.args[0] for c in run.call_args_list if 'generate.py' in c.args[0])
                self.assertEqual('/rustdeskmcp.exe' in command, mcp)
                self.assertEqual(command.endswith(' --mcp'), mcp)
                self.assertEqual(flutter.called, mcp)
                self.assertEqual(verify.called, mcp)

    def test_windows_mcp_artifact_preserves_click_install_suffix(self):
        workflow = (ROOT / '.github/workflows/agent-mcp-build.yml').read_text()
        artifact = 'dist/rustdesk-mcp-windows-x64-install.exe'
        self.assertEqual(workflow.count(artifact), 2)
        self.assertNotIn('dist/rustdesk-mcp-windows-x64.exe', workflow)

        portable = (ROOT / 'libs/portable/src/main.rs').read_text()
        self.assertIn('arg_exe.to_lowercase().ends_with("install.exe")', portable)

    def test_mcp_rejects_packaging_paths_that_would_overwrite_stock(self):
        for flags in [['--drm'], ['--package', 'bundle']]:
            args = build.make_parser().parse_args(['--flutter', '--mcp'] + flags)
            with self.assertRaisesRegex(Exception, '--mcp'):
                build.get_features(args)
        with patch.object(build, 'windows', False), patch.object(build, 'osx', False), \
                patch.object(build, 'linux_packaging_branch', return_value='pacman'):
            args = build.make_parser().parse_args(['--flutter', '--mcp'])
            with self.assertRaisesRegex(Exception, 'Debian'):
                build.get_features(args)

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
