import copy
import unittest

from package_macos_isolated import BUNDLE_ID, isolated_plist, validate_native_info


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


if __name__ == '__main__':
    unittest.main()
