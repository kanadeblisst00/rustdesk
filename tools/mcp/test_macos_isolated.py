import copy
import json
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest
from unittest.mock import call, patch

from package_macos_isolated import BUNDLE_ID, isolated_plist, package, validate_native_info


class MacIsolationTest(unittest.TestCase):
    def test_bundle_metadata_does_not_register_stock_url_scheme(self):
        original = {'CFBundleIdentifier': BUNDLE_ID, 'CFBundleName': 'RustDesk',
                    'CFBundleURLTypes': [{'CFBundleURLSchemes': ['rustdesk']}]}
        snapshot = copy.deepcopy(original)
        result = isolated_plist(original)
        self.assertEqual(original, snapshot)
        self.assertEqual(result['CFBundleName'], 'RustDeskMCPTest')
        self.assertEqual(result['CFBundleURLTypes'][0]['CFBundleURLSchemes'], ['rustdeskmcptest'])
        with self.assertRaisesRegex(ValueError, 'Rebuild Flutter'):
            isolated_plist({'CFBundleIdentifier': 'com.carriez.rustdesk'})

    def test_native_identity_must_be_fully_isolated(self):
        valid = {
            'app_name': 'RustDeskMCPTest', 'bundle_id': BUNDLE_ID,
            'mcp_endpoint': 'http://127.0.0.1:59941/mcp',
            'outgoing_only': True, 'installation_disabled': True,
            'config_dir': '/test/Library/Preferences/' + BUNDLE_ID,
            'log_dir': '/test/Library/Logs/RustDeskMCPTest',
            'ipc': '/tmp/RustDeskMCPTest-501/ipc',
            'service_ipc': '/tmp/RustDeskMCPTest-service/ipc_service',
        }
        validate_native_info(valid)
        validate_native_info({**valid, 'config_dir': valid['config_dir'] + '/'})
        for key in valid:
            with self.subTest(key=key), self.assertRaises(ValueError):
                validate_native_info({**valid, key: None})

    def test_package_signs_staged_service_before_bundle(self):
        for helper_fails in (False, True):
            with self.subTest(helper_fails=helper_fails), tempfile.TemporaryDirectory() as root:
                app = Path(root).resolve() / 'Release/RustDesk.app'
                (app / 'Contents/MacOS').mkdir(parents=True)
                with (app / 'Contents/Info.plist').open('wb') as stream:
                    plistlib.dump({'CFBundleIdentifier': BUNDLE_ID}, stream)
                sign = ['codesign', '--force', '--sign', '-', '--timestamp=none']
                native = subprocess.CompletedProcess([], 0, stdout=json.dumps({}))
                helper = subprocess.CalledProcessError(1, sign) if helper_fails else None
                with patch('package_macos_isolated.subprocess.run',
                           side_effect=[native, helper, None, None]) as run, \
                        patch('package_macos_isolated.validate_native_info') as validate:
                    if helper_fails:
                        with self.assertRaises(subprocess.CalledProcessError):
                            package(app)
                    else:
                        package(app)
                    validate.assert_called_once_with({})
                expected = [
                    call([str(app / 'Contents/MacOS/RustDesk'), '--mcp-isolation-info'],
                         check=True, capture_output=True, text=True, timeout=30),
                    call(sign + [str(app / 'Contents/MacOS/service')], check=True),
                ]
                if not helper_fails:
                    expected += [
                        call(sign + [str(app)], check=True),
                        call(['codesign', '--verify', '--deep', '--strict', str(app)], check=True),
                    ]
                self.assertEqual(run.call_args_list, expected)
                self.assertEqual(app.exists(), helper_fails)
                self.assertEqual(app.with_name('RustDeskMCPTest.app').exists(), not helper_fails)


if __name__ == '__main__':
    unittest.main()
