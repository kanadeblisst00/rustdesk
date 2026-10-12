"""macOS integration checks against the real Rust lock/startup/listener code."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

BINARY = Path(sys.argv.pop(1)).resolve()


class MainWindowTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rd-instance-', dir='/tmp')
        self.path = Path(self.temp.name)
        self.children = []

    def tearDown(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
            child.communicate(timeout=5)
        self.temp.cleanup()

    def launch(self, *args, **extra_env):
        child = subprocess.Popen([str(BINARY), *args], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env={**os.environ, 'RUSTDESK_INSTANCE_TEST_DIR': str(self.path),
                                      **extra_env}, text=True)
        self.children.append(child)
        return child

    def result(self, child):
        return child.stdout.readline().strip()

    def events(self, count):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            path = self.path / 'events.jsonl'
            events = [json.loads(s) for s in path.read_text().splitlines()] if path.exists() else []
            if len(events) >= count:
                return events
            time.sleep(.02)
        self.fail(f'Expected {count} activations, received {events}')

    def test_simultaneous_launches_have_one_owner(self):
        children = [self.launch(f'request-{i}') for i in range(16)]
        results = [self.result(child) for child in children]
        self.assertEqual(results.count('PRIMARY'), 1, results)
        self.assertEqual(results.count('SECONDARY'), 15, results)
        winner = results.index('PRIMARY')
        self.assertEqual({e['url'] for e in self.events(15)},
                         {f'request-{i}' for i in range(16) if i != winner})

    def test_early_activation_waits_for_flutter_ready(self):
        first = self.launch(RUSTDESK_INSTANCE_TEST_READY_DELAY_MS='1500')
        self.assertEqual(self.result(first), 'PRIMARY')
        second = self.launch('rustdeskmcp://connect/123456')
        self.assertEqual(self.result(second), 'SECONDARY')
        self.assertFalse((self.path / 'events.jsonl').exists())
        self.assertEqual(self.events(1)[0]['url'], 'rustdeskmcp://connect/123456')

    def test_crashed_owner_and_stale_socket_can_restart(self):
        first = self.launch()
        self.assertEqual(self.result(first), 'PRIMARY')
        first.kill()
        first.wait(timeout=5)
        self.assertTrue((self.path / 'main-window.lock').exists())
        self.assertTrue((self.path / 'socket').exists())
        self.assertEqual(self.result(self.launch()), 'PRIMARY')

    def test_bind_failure_never_admits_a_main_window(self):
        child = self.launch(RUSTDESK_INSTANCE_TEST_BIND_FAIL='1')
        self.assertEqual(self.result(child), 'SECONDARY')
        child.wait(timeout=5)
        self.assertEqual(self.result(self.launch()), 'PRIMARY')

    def test_broken_activation_transport_never_admits_a_duplicate(self):
        first = self.launch()
        self.assertEqual(self.result(first), 'PRIMARY')
        (self.path / 'socket').unlink()
        second = self.launch()
        output, errors = second.communicate(timeout=15)
        self.assertEqual(output.strip(), 'SECONDARY', errors)
        self.assertIsNone(first.poll())

    def test_existing_legacy_listener_is_used_without_creating_a_window(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.bind(str(self.path / 'socket'))
            listener.listen()
            listener.settimeout(5)
            child = self.launch('legacy-url')
            with listener.accept()[0] as connection:
                with connection.makefile('rb') as stream:
                    first = stream.read(1)
                    header = first + stream.read(first[0] & 3)
                    size = int.from_bytes(header, 'little') >> 2
                    self.assertEqual(json.loads(stream.read(size))['c'], 'legacy-url')
            self.assertEqual(self.result(child), 'SECONDARY')

    def test_install_and_connection_manager_do_not_take_main_lock(self):
        for arg in ('--cm', '--install', 'multi_window'):
            child = self.launch(arg)
            self.assertEqual(self.result(child), 'PRIMARY')
            self.assertFalse((self.path / 'main-window.lock').exists())
            self.assertFalse((self.path / 'socket').exists())
            child.communicate('q', timeout=5)

    def test_symlink_lock_is_rejected_without_touching_target(self):
        target = self.path / 'target'
        target.write_text('unchanged')
        (self.path / 'main-window.lock').symlink_to(target)
        child = self.launch()
        self.assertEqual(self.result(child), 'SECONDARY')
        self.assertEqual(target.read_text(), 'unchanged')


if __name__ == '__main__':
    unittest.main(verbosity=2)
