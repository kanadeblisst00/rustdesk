"""Trusted Cursor preset entry point for the production MCP protocol and worker modules."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]


def report(path, cases):
    counts = {name: sum(c['status'] == name for c in cases) for name in ('passed', 'failed', 'skipped')}
    path.write_text(json.dumps({'total': len(cases), **counts, 'cases': cases}, ensure_ascii=False, indent=2), encoding='utf-8')


def cargo(arguments, target, log):
    env = os.environ.copy()
    env['CARGO_TARGET_DIR'] = str(target)
    executable = shutil.which('cargo')
    if executable is None and env.get('CARGO_HOME'):
        candidate = Path(env['CARGO_HOME']) / 'bin' / ('cargo.exe' if sys.platform == 'win32' else 'cargo')
        if candidate.is_file():
            executable = str(candidate)
    if executable is None:
        raise RuntimeError('Configured cargo is unavailable; no toolchain was installed')
    process = subprocess.run([executable, *arguments], cwd=ROOT, env=env,
                             stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding='utf-8', errors='replace')
    log.write_text(process.stdout, encoding='utf-8')
    print(process.stdout, flush=True)
    return process


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('mode', choices=['build', 'rust-tests', 'worker-tests'])
    parser.add_argument('build', type=Path)
    parser.add_argument('artifacts', type=Path)
    args = parser.parse_args()
    args.artifacts.mkdir(parents=True, exist_ok=True)
    executable = 'process-worker.exe' if sys.platform == 'win32' else 'process-worker'
    if args.mode == 'build':
        result = cargo(['build', '--locked', '--manifest-path', 'tools/mcp/process-check/Cargo.toml'],
                       args.build / 'worker', args.artifacts / 'worker-build.log')
        if result.returncode:
            return result.returncode
        binary = args.artifacts / executable
        shutil.copy2(args.build / 'worker' / 'debug' / executable, binary)
        manifest = {'sha256': hashlib.sha256(binary.read_bytes()).hexdigest(), 'bytes': binary.stat().st_size,
                    'platform': sys.platform, 'binary': executable, 'scope': 'production MCP modules; not a deployable desktop application'}
        (args.artifacts / 'worker-manifest.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
        return 0
    if args.mode == 'rust-tests':
        cases = []
        for name, manifest in [('protocol', 'libs/agent_mcp/Cargo.toml'), ('worker', 'tools/mcp/process-check/Cargo.toml')]:
            result = cargo(['test', '--locked', '--manifest-path', manifest, '--', '--test-threads=4'],
                           args.build / name, args.artifacts / (name + '-tests.log'))
            found = re.findall(r'^test (.+?) \.\.\. (ok|FAILED|ignored[^\n]*)$', result.stdout, re.MULTILINE)
            cases.extend({'name': name + ':' + test, 'status': 'passed' if status == 'ok' else 'failed' if status == 'FAILED' else 'skipped'} for test, status in found)
            if result.returncode or not found:
                cases.append({'name': name + ':cargo-exit', 'status': 'failed', 'message': 'cargo exit ' + str(result.returncode) + '; see retained log'})
                report(args.artifacts / 'rust-tests.json', cases)
                return result.returncode or 1
        report(args.artifacts / 'rust-tests.json', cases)
        return int(any(c['status'] == 'failed' for c in cases))
    os.environ['RUSTDESK_MCP_TEST_EXECUTABLE'] = str(args.artifacts / executable)
    os.environ['RUSTDESK_MCP_TEST_RECONCILE_CLI'] = '1'
    sys.path.insert(0, str(Path(__file__).parent))
    suite = unittest.defaultTestLoader.loadTestsFromNames(['test_process_worker', 'test_process_worker_harness'])
    cases = []

    class Result(unittest.TextTestResult):
        def addSuccess(self, test):
            super().addSuccess(test)
            cases.append({'name': test.id(), 'status': 'passed'})

        def addSkip(self, test, reason):
            super().addSkip(test, reason)
            cases.append({'name': test.id(), 'status': 'skipped', 'message': reason})

        def addFailure(self, test, error):
            super().addFailure(test, error)
            cases.append({'name': test.id(), 'status': 'failed', 'message': self._exc_info_to_string(error, test)})

        def addError(self, test, error):
            super().addError(test, error)
            cases.append({'name': test.id(), 'status': 'failed', 'message': self._exc_info_to_string(error, test)})

    with (args.artifacts / 'worker-native-tests.log').open('w', encoding='utf-8') as log:
        result = unittest.TextTestRunner(stream=log, verbosity=2, resultclass=Result).run(suite)
    report(args.artifacts / 'worker-native-tests.json', cases)
    print((args.artifacts / 'worker-native-tests.log').read_text(encoding='utf-8'))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    sys.exit(main())
