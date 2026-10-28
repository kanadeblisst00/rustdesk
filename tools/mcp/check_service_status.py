"""Cursor preset runner: exercise production IPC loop and password update method."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def extract(source, start, end):
    offset = source.index(start)
    return source[offset:source.index(end, offset)]


def run(command, log, env=None):
    result = subprocess.run(command, cwd=ROOT, env=env, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    log.write_text(result.stdout, encoding='utf-8')
    print(result.stdout, flush=True)
    if result.returncode:
        raise RuntimeError(f'{command[0]} exited {result.returncode}; see {log.name}')
    return result.stdout


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['native', 'dart'])
    parser.add_argument('build', type=Path)
    parser.add_argument('artifacts', type=Path)
    parser.add_argument('--dart')
    args = parser.parse_args()
    args.build.mkdir(parents=True, exist_ok=True)
    args.artifacts.mkdir(parents=True, exist_ok=True)
    if args.mode == 'native':
        source = (ROOT / 'src/ui_interface.rs').read_text()
        status = '#[derive(Clone, Debug, Serialize)]\n' + extract(
            source, 'pub struct UiStatus {', '\n#[derive(Debug, Clone, Serialize)]')
        loop = '#[tokio::main(flavor = "current_thread")]\n' + extract(
            source, 'async fn check_connect_status_(', '\n#[allow(dead_code)]')
        fixture = (ROOT / 'tools/mcp/service_status_fixture.rs').read_text()
        fixture = fixture.replace('// @PRODUCTION_STATUS@', status).replace('// @PRODUCTION_LOOP@', loop)
        stage = args.build / 'native'
        stage.mkdir()
        for name in ['Cargo.toml', 'Cargo.lock']:
            shutil.copy2(ROOT / 'tools/main-window-check' / name, stage / name)
        (stage / 'main.rs').write_text(fixture)
        env = dict(os.environ, CARGO_TARGET_DIR=str(args.build / 'target'))
        run(['cargo', 'build', '--locked', '--manifest-path', str(stage / 'Cargo.toml')],
            args.artifacts / 'native-build.log', env)
        name = 'main-window-check' + ('.exe' if os.name == 'nt' else '')
        executable = args.artifacts / ('service-status-check' + ('.exe' if os.name == 'nt' else ''))
        shutil.copy2(args.build / 'target/debug' / name, executable)
        output = run([str(executable)], args.artifacts / 'native-tests.log')
        result = json.loads(output.splitlines()[-1])
        print('binary_sha256=' + hashlib.sha256(executable.read_bytes()).hexdigest())
    else:
        source = (ROOT / 'flutter/lib/models/server_model.dart').read_text()
        method = extract(source, '  updatePasswordModel() async {', '\n  toggleAudio() async {')
        fixture = (ROOT / 'tools/mcp/service_status_fixture.dart').read_text()
        status_uri = (ROOT / 'flutter/lib/models/local_service_status.dart').as_uri()
        fixture = fixture.replace('// @STATUS_IMPORT@', f"import '{status_uri}';")
        fixture = fixture.replace('// @PASSWORD_METHOD@', method)
        script = args.build / 'password_status.dart'
        script.write_text(fixture)
        output = run([args.dart, str(script)], args.artifacts / 'dart-tests.log')
        result = json.loads(output.splitlines()[-1])
    (args.artifacts / f'{args.mode}-tests.json').write_text(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
