#!/usr/bin/env python3
"""Build/run isolated Windows regressions against the production privacy-mode module."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def run(argv, log, **kwargs):
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **kwargs)
    log.write_bytes(result.stdout)
    if result.returncode:
        raise RuntimeError(f'{argv[0]} exited {result.returncode}; see {log}')
    return result.stdout.decode('utf-8', errors='replace')


def stage(build, mock):
    target = build / ('mock' if mock else 'native')
    target.mkdir(parents=True, exist_ok=False)
    source = ROOT / 'src/privacy_mode/win_virtual_display.rs'
    snapshot = ROOT / 'src/privacy_mode/win_virtual_display/snapshot.rs'
    privacy = source.read_text()
    topology = snapshot.read_text()
    if mock:
        privacy = privacy.replace('use winapi::', 'use crate::fake_winapi::')
        privacy = privacy.replace('Duration::from_secs(1)', 'Duration::from_millis(1)')
        privacy = privacy.replace('Duration::from_millis(5000)', 'Duration::from_millis(50)')
        privacy = privacy.replace('Duration::from_millis(500)', 'Duration::from_millis(1)')
        topology = topology.replace('use windows::Win32::', 'use crate::fake_windows::Win32::')
    path = target / 'privacy_mode'
    (path / 'win_virtual_display').mkdir(parents=True)
    (path / 'win_virtual_display.rs').write_text(privacy)
    (path / 'win_virtual_display/snapshot.rs').write_text(topology)
    fixture = (ROOT / 'tools/mcp/privacy_mode2_fixture.rs').read_text()
    if not mock:
        fixture = fixture[:fixture.index('fn main() {')] + 'fn main() {}\n'
    (target / 'main.rs').write_text(fixture)
    # These versions match the project's locked dependencies. Generated manifests
    # and lockfiles live exclusively in the task build directory.
    (target / 'Cargo.toml').write_text('''[package]
name = "privacy-mode2-check"
version = "0.1.0"
edition = "2021"
publish = false
[dependencies]
anyhow = "=1.0.98"
log = "=0.4.22"
serde_json = "=1.0.118"
winapi = {version = "=0.3.9", features = ["winuser", "wingdi"]}
windows = {version = "=0.61.1", features = ["Win32_Devices_Display", "Win32_Foundation"]}
[[bin]]
name = "privacy-mode2-check"
path = "main.rs"
[workspace]
''')
    return target


def main():
    p = argparse.ArgumentParser()
    p.add_argument('mode', choices=['build', 'test'])
    p.add_argument('build', type=Path)
    p.add_argument('artifacts', type=Path)
    args = p.parse_args()
    if os.name != 'nt':
        raise RuntimeError('Privacy mode 2 checks must run on Windows')
    args.build.mkdir(parents=True, exist_ok=True)
    args.artifacts.mkdir(parents=True, exist_ok=True)
    if args.mode == 'build':
        inputs = {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in [ROOT / 'src/privacy_mode/win_virtual_display.rs',
                               ROOT / 'src/privacy_mode/win_virtual_display/snapshot.rs',
                               Path(__file__), ROOT / 'tools/mcp/privacy_mode2_fixture.rs']}
        for mock in [True, False]:
            name = 'mock' if mock else 'native'
            staged = stage(args.build, mock)
            env = dict(os.environ, CARGO_TARGET_DIR=str(args.build / 'target'))
            manifest = str(staged / 'Cargo.toml')
            run(['cargo', 'generate-lockfile', '--manifest-path', manifest],
                args.artifacts / f'{name}-lock.log', env=env)
            run(['cargo', 'build', '--locked', '--manifest-path', manifest],
                args.artifacts / f'{name}-build.log', env=env)
            destination = args.artifacts / f'privacy-mode2-{name}-check.exe'
            shutil.copy2(args.build / 'target/debug/privacy-mode2-check.exe', destination)
            shutil.copy2(staged / 'Cargo.lock', args.artifacts / f'{name}-Cargo.lock')
        (args.artifacts / 'privacy-inputs.json').write_text(json.dumps(inputs, indent=2))
    else:
        executable = args.artifacts / 'privacy-mode2-mock-check.exe'
        output = run([str(executable)], args.artifacts / 'privacy-tests.log')
        report = json.loads(output.splitlines()[-1])
        if report['failed'] or report['total'] < 9:
            raise RuntimeError('Privacy mode 2 regression contract failed')
        report['scope'] = 'production Windows privacy module; simulated display and driver APIs; polling time scaled'
        report['binary_sha256'] = hashlib.sha256(executable.read_bytes()).hexdigest()
        (args.artifacts / 'privacy-tests.json').write_text(json.dumps(report, indent=2))
        print(json.dumps(report))


if __name__ == '__main__':
    main()
