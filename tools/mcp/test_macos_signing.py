import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import package_desktop as package
from test_desktop_identity import native_info


def app_fixture(root):
    app = root / "Release/RustDesk.app"
    (app / "Contents/MacOS").mkdir(parents=True)
    with (app / "Contents/Info.plist").open("wb") as stream:
        plistlib.dump(dict(CFBundleIdentifier=package.BUNDLE_ID,
                           CFBundleExecutable="RustDesk", CFBundlePackageType="APPL"), stream)
    return app


class MacOSSigningTest(unittest.TestCase):
    def test_missing_or_adhoc_identity_fails_before_changing_bundle(self):
        for identity in ["", "-", "certificate name", "A" * 39, "Z" * 40]:
            with self.subTest(identity=identity), tempfile.TemporaryDirectory() as temp:
                app = app_fixture(Path(temp))
                binary = app / "Contents/MacOS/RustDesk"
                binary.write_text("fixture")
                before = (app / "Contents/Info.plist").read_bytes()
                with patch.dict(os.environ, {"MCP_MACOS_SIGN_IDENTITY": identity}), \
                        patch.object(package, "verify_binary"):
                    with self.assertRaisesRegex(ValueError, "persistent"):
                        package.package_macos(app)
                self.assertEqual(binary.read_text(), "fixture")
                self.assertEqual((app / "Contents/Info.plist").read_bytes(), before)
                self.assertFalse(app.with_name("RustDeskMCP.app").exists())

    @unittest.skipUnless(sys.platform == "darwin" and shutil.which("clang")
                         and os.environ.get("MCP_MACOS_SIGN_IDENTITY"),
                         "requires macOS, clang and an unlocked persistent signing identity")
    def test_different_builds_satisfy_the_same_certificate_requirement(self):
        def run(*args):
            return subprocess.run(args, check=True, capture_output=True, text=True)

        def requirement(target):
            result = run("codesign", "-d", "-r-", str(target))
            output = result.stdout + result.stderr
            return next(line.split("designated => ", 1)[1] for line in output.splitlines()
                        if line.startswith("designated => "))

        with tempfile.TemporaryDirectory() as temp:
            apps = []
            for revision in [1, 2]:
                root = Path(temp) / str(revision)
                app = app_fixture(root)
                info = dict(native_info("macos"), revision=revision)
                source = root / "main.c"
                source.write_text('#include <stdio.h>\nint main(void) { puts(' +
                                  json.dumps(json.dumps(info)) + '); return 0; }\n')
                run("clang", str(source), "-o", str(app / "Contents/MacOS/RustDesk"))
                shutil.copy2(app / "Contents/MacOS/RustDesk", app / "Contents/MacOS/service")
                package.package_macos(app)
                apps.append(app.with_name("RustDeskMCP.app"))
            for relative, identifier in [("", package.BUNDLE_ID),
                                         ("Contents/MacOS/service", package.BUNDLE_ID + ".service")]:
                with self.subTest(target=relative):
                    first, second = [app / relative for app in apps]
                    old_requirement = requirement(first)
                    self.assertIn('identifier "' + identifier + '"', old_requirement)
                    self.assertNotIn("cdhash", old_requirement)
                    self.assertEqual(old_requirement, requirement(second))
                    for target in [first, second]:
                        run("codesign", "--verify", "--strict", "-R", "=" + old_requirement, str(target))
                    details = [run("codesign", "-d", "--verbose=4", str(target)).stderr
                               for target in [first, second]]
                    hashes = [next(line for line in text.splitlines() if line.startswith("CDHash="))
                              for text in details]
                    self.assertNotEqual(*hashes)
                    # Matching the bundle ID alone must not allow a replacement signer.
                    run("codesign", "--force", "--sign", "-", "--timestamp=none",
                        "--identifier", identifier, str(first))
                    rejected = subprocess.run(["codesign", "--verify", "-R", "=" + old_requirement,
                                               str(first)], capture_output=True)
                    self.assertNotEqual(rejected.returncode, 0)


if __name__ == "__main__":
    unittest.main()
