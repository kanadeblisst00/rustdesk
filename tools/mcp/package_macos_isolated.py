"""Validate and package the opt-in macOS MCP test app without touching installed apps."""

import argparse
import json
from pathlib import Path
import plistlib
import subprocess

APP_NAME = "RustDeskMCPTest"
BUNDLE_ID = "cn.ikanade." + APP_NAME


def isolated_plist(original):
    if original.get("CFBundleIdentifier") != BUNDLE_ID:
        raise ValueError("Rebuild Flutter with the isolated PRODUCT_BUNDLE_IDENTIFIER")
    result = dict(original)
    result["CFBundleName"] = APP_NAME
    result["CFBundleDisplayName"] = APP_NAME
    result["CFBundleURLTypes"] = [{
        "CFBundleTypeRole": "Editor",
        "CFBundleURLName": BUNDLE_ID,
        "CFBundleURLSchemes": [APP_NAME.lower()],
    }]
    return result


def validate_native_info(info):
    expected = {
        "app_name": APP_NAME,
        "bundle_id": BUNDLE_ID,
        "mcp_endpoint": "http://127.0.0.1:59941/mcp",
        "outgoing_only": True,
        "installation_disabled": True,
    }
    if any(info.get(key) != value for key, value in expected.items()):
        raise ValueError("Native binary does not have the isolated MCP identity")
    if not str(info.get("config_dir", "")).rstrip("/").endswith("/" + BUNDLE_ID):
        raise ValueError("Native config directory is not isolated")
    if not str(info.get("log_dir", "")).endswith("/Logs/" + APP_NAME):
        raise ValueError("Native log directory is not isolated")
    ipc = str(info.get("ipc", ""))
    if not ipc.startswith("/tmp/" + APP_NAME + "-") or not ipc.endswith("/ipc"):
        raise ValueError("Native IPC is not isolated")
    if info.get("service_ipc") != "/tmp/" + APP_NAME + "-service/ipc_service":
        raise ValueError("Native service IPC is not isolated")


def package(app):
    app = app.resolve(strict=True)
    if app.name != "RustDesk.app" or app.parent.name != "Release":
        raise ValueError("Expected the generated Release/RustDesk.app build directory")
    target = app.with_name(APP_NAME + ".app")
    if target.exists():
        raise ValueError("Isolated output already exists; use a clean build output directory")
    info_path = app / "Contents/Info.plist"
    with info_path.open("rb") as stream:
        metadata = isolated_plist(plistlib.load(stream))
    binary = app / "Contents/MacOS/RustDesk"
    native = subprocess.run([str(binary), "--mcp-isolation-info"], check=True,
                            capture_output=True, text=True, timeout=30)
    validate_native_info(json.loads(native.stdout))
    with info_path.open("wb") as stream:
        plistlib.dump(metadata, stream)
    # Xcode signed the original bundle before its metadata and service were staged.
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none",
                    str(app / "Contents/MacOS/service")], check=True)
    subprocess.run(["codesign", "--force", "--sign", "-", "--timestamp=none", str(app)], check=True)
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    app.rename(target)
    print("Validated isolated MCP app:", target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    package(parser.parse_args().app)
