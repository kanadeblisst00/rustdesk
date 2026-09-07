"""Package the regular MCP desktop build with an identity separate from RustDesk."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile

APP_NAME = "RustDeskMCP"
BINARY_NAME = "rustdeskmcp"
BUNDLE_ID = "com.carriez.RustDeskMCP"
PACKAGE_NAME = "rustdesk-mcp"
ROOT = Path(__file__).resolve().parents[2]


def validate_native_info(info):
    expected = {
        "app_name": APP_NAME,
        "bundle_id": BUNDLE_ID,
        "mcp_endpoint": "http://127.0.0.1:59940/mcp",
        "default_direct_access_port": "59942",
        "lan_port": 59943,
    }
    if any(info.get(key) != value for key, value in expected.items()):
        raise ValueError("Native binary does not have the RustDeskMCP identity")
    for key in ("config_dir", "log_dir", "ipc", "service_ipc"):
        parts = str(info.get(key, "")).replace("\\", "/").lower().split("/")
        if not any(part in (BINARY_NAME, BUNDLE_ID.lower()) or
                   part.startswith(BINARY_NAME + "-") for part in parts):
            raise ValueError("Native path is not isolated: " + key)


def verify_binary(binary):
    result = subprocess.run([str(Path(binary).resolve(strict=True)), "--mcp-identity-info"],
                            check=True, capture_output=True, text=True, timeout=30)
    validate_native_info(json.loads(result.stdout))


def package_macos(app):
    app = Path(app).resolve(strict=True)
    if app.name != "RustDesk.app" or app.parent.name != "Release":
        raise ValueError("Expected generated Release/RustDesk.app")
    target_app = app.with_name(APP_NAME + ".app")
    if target_app.exists():
        raise ValueError("MCP output already exists; use a clean build directory")
    info_path = app / "Contents/Info.plist"
    with info_path.open("rb") as stream:
        info = plistlib.load(stream)
    if info.get("CFBundleIdentifier") != BUNDLE_ID or info.get("CFBundleExecutable") != "RustDesk":
        raise ValueError("Rebuild Flutter with the RustDeskMCP bundle ID and executable")
    verify_binary(app / "Contents/MacOS/RustDesk")
    (app / "Contents/MacOS/RustDesk").rename(app / "Contents/MacOS" / APP_NAME)
    info["CFBundleExecutable"] = APP_NAME
    info["CFBundleName"] = APP_NAME
    info["CFBundleDisplayName"] = APP_NAME
    info["CFBundleURLTypes"] = [{
        "CFBundleTypeRole": "Editor",
        "CFBundleURLName": BUNDLE_ID,
        "CFBundleURLSchemes": [BINARY_NAME],
    }]
    with info_path.open("wb") as stream:
        plistlib.dump(info, stream)
    for target in (app / "Contents/MacOS/service", app):
        subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(target)],
                       check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    app.rename(target_app)


def write_file(path, content, executable=False):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755 if executable else 0o644)


def stage_linux(bundle, stage, version, architecture, extra_depends=""):
    bundle, stage = Path(bundle), Path(stage)
    if (bundle / "rustdesk").exists():
        raise ValueError("Stale stock executable in MCP bundle; clean the Flutter build first")
    verify_binary(bundle / BINARY_NAME)
    share = stage / "usr/share" / BINARY_NAME
    shutil.copytree(bundle, share, symlinks=True)
    (stage / "usr/bin").mkdir(parents=True)
    (stage / "usr/bin" / BINARY_NAME).symlink_to("../share/rustdeskmcp/rustdeskmcp")
    for source, target in [
        ("128x128@2x.png", "usr/share/icons/hicolor/256x256/apps/rustdeskmcp.png"),
        ("scalable.svg", "usr/share/icons/hicolor/scalable/apps/rustdeskmcp.svg"),
    ]:
        destination = stage / target
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(ROOT / "res" / source, destination)
    for name in ("rustdesk.desktop", "rustdesk-link.desktop", "rustdesk.service"):
        body = (ROOT / "res" / name).read_text().replace("RustDesk", APP_NAME).replace("rustdesk", BINARY_NAME)
        if name.endswith(".service"):
            # Match argv[0] exactly, including an optional absolute path.
            body = body.replace('pkill -f "rustdeskmcp --"', 'pkill -f "(^|/)rustdeskmcp --"')
            destination = share / "files/systemd/rustdeskmcp.service"
        else:
            destination = stage / "usr/share/applications" / name.replace("rustdesk", BINARY_NAME)
        write_file(destination, body)
    write_file(share / "files/polkit", "#!/bin/sh\n", executable=True)
    control = f"""Package: {PACKAGE_NAME}
Section: net
Priority: optional
Version: {version}
Architecture: {architecture}
Maintainer: rustdesk <info@rustdesk.com>
Depends: libgtk-3-0t64 | libgtk-3-0, libxcb-randr0, libxdo3 | libxdo4, libxfixes3, libxcb-shape0, libxcb-xfixes0, libasound2t64 | libasound2, libsystemd0, curl, libva2, libva-drm2, libva-x11-2, libgstreamer-plugins-base1.0-0, gstreamer1.0-pipewire{extra_depends}
Recommends: libayatana-appindicator3-1
Description: RustDesk with MCP support, installed separately as RustDeskMCP.
"""
    write_file(stage / "DEBIAN/control", control)
    for name in ("postinst", "prerm", "postrm"):
        write_file(stage / "DEBIAN" / name,
                   (ROOT / "tools/mcp/debian" / name).read_text(), executable=True)


def package_linux(bundle, output, version, architecture, extra_depends=""):
    with tempfile.TemporaryDirectory(prefix="rustdeskmcp-deb-") as temp:
        stage = Path(temp) / "stage"
        stage_linux(bundle, stage, version, architecture, extra_depends)
        subprocess.run(["dpkg-deb", "--root-owner-group", "--build", str(stage), str(output)], check=True)


def flutter_build(command, cwd=None):
    env = dict(os.environ, RUSTDESK_MCP_BUILD="1")
    # Flutter is a .bat launcher on Windows.
    subprocess.run(command, cwd=cwd, env=env, check=True, shell=os.name == "nt")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=("macos", "verify"))
    parser.add_argument("path", type=Path)
    args = parser.parse_args()
    if args.platform == "macos":
        package_macos(args.path)
    else:
        verify_binary(args.path)
