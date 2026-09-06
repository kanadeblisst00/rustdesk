"""Check the source-embedded public server configuration, not saved user settings."""

import argparse
import base64
import json
from pathlib import Path
import re


def source_config(source, webrtc_source):
    servers = re.search(r'pub const RENDEZVOUS_SERVERS: &\[&str\] = &\[(.*?)\];', source)
    key = re.search(r'pub const RS_PUB_KEY: &str = "([^"\n]+)";', source)
    defaults = re.search(
        r'pub static ref DEFAULT_SETTINGS:.*?RwLock::new\(HashMap::from\(\[(.*?)\]\)\);',
        source, re.DOTALL)
    relay = re.search(r'\("relay-server"\.to_owned\(\), "([^"\n]+)"\.to_owned\(\)\)',
                      defaults.group(1) if defaults else "")
    if not all((servers, key, relay)):
        raise ValueError("Missing source-embedded ID server, public key or relay default")
    result = {
        "id_servers": re.findall(r'"([^"\n]+)"', servers.group(1)),
        "relay_server": relay.group(1),
        "public_key": key.group(1),
    }
    options = dict(re.findall(r'\("([^"\n]+)"\.to_owned\(\), "([^"\n]+)"\.to_owned\(\)\)',
                              defaults.group(1)))
    result["always_relay"] = options.get("force-always-relay") == "Y"
    ice = re.search(r'static DEFAULT_ICE_SERVERS: \[&str; \d+\] = \[(.*?)\];',
                    webrtc_source, re.DOTALL)
    if not ice:
        raise ValueError("Missing source-embedded ICE server defaults")
    ice_urls = re.findall(r'"([^"\n]+)"', ice.group(1))
    if any(not url.startswith("stun:") for url in ice_urls):
        raise ValueError("Unexpected non-STUN entry in built-in ICE defaults")
    result["default_stun_servers"] = [url[5:] for url in ice_urls]
    verify_default_options(options, result)
    return result


def verify_default_options(options, expected):
    actual = {
        "id_servers": [options.get("custom-rendezvous-server")],
        "relay_server": options.get("relay-server"),
        "public_key": options.get("key"),
        "always_relay": options.get("force-always-relay") == "Y",
    }
    if actual != {key: expected.get(key) for key in actual}:
        raise ValueError("Server setting defaults differ from the embedded server configuration")


def native_config(info):
    result = info["built_in_server"]
    verify_default_options(info.get("default_server_options", {}), result)
    return result


def verify(actual, expected):
    if len(base64.b64decode(expected["public_key"], validate=True)) != 32:
        raise ValueError("Expected a 32-byte Ed25519 public key")
    if actual.get("always_relay") is not True:
        raise ValueError("Private server builds must connect via relay by default")
    if actual.get("default_stun_servers") != []:
        raise ValueError("Private server builds must not include public STUN fallbacks")
    if actual != expected:
        raise ValueError("Embedded server configuration differs from server-config.json")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    inputs = parser.add_mutually_exclusive_group(required=True)
    inputs.add_argument("--source", type=Path)
    inputs.add_argument("--native-info", type=Path)
    args = parser.parse_args()
    expected = json.loads(Path(__file__).with_name("server-config.json").read_text(encoding="utf-8"))
    if args.source:
        actual = source_config(args.source.read_text(encoding="utf-8"),
                               args.source.with_name("webrtc.rs").read_text(encoding="utf-8"))
    else:
        actual = native_config(json.loads(args.native_info.read_text(encoding="utf-8")))
    verify(actual, expected)
    print("Verified embedded server configuration:", json.dumps(actual))
