from contextlib import ExitStack
import json
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

import test_process_worker as worker


class WorkerHarnessTest(unittest.TestCase):
    def setUp(self):
        self.process = Mock(returncode=None)
        self.polls = 0
        self.cancel = False
        self.never_exit = False
        self.exit_code = 0
        self.process.poll.side_effect = self.poll
        self.process.wait.side_effect = lambda **kwargs: self.process.returncode
        self.process.kill.side_effect = self.kill
        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(patch.object(worker.subprocess, 'Popen', self.spawn))
        stack.enter_context(patch.object(worker.time, 'sleep'))
        read_text = Path.read_text

        def read_when_closed(path, *args, **kwargs):
            if path.name == 'state.json' and self.process.returncode is None:
                raise PermissionError(13, 'State replacement is still in progress', str(path))
            return read_text(path, *args, **kwargs)

        stack.enter_context(patch.object(Path, 'read_text', read_when_closed))
        self.case = worker.ProcessWorkerTest()

    def spawn(self, args, **kwargs):
        self.job = Path(args[-1])
        (self.job / 'stdout.log').write_bytes(b'output')
        (self.job / 'stderr.log').write_bytes(b'error')
        return self.process

    def poll(self):
        if self.process.returncode is not None or self.never_exit:
            return self.process.returncode
        self.polls += 1
        if self.polls == 1:
            self.assertFalse((self.job / 'cancel.json').exists())
            return None
        if self.cancel and self.polls == 2:
            self.assertFalse((self.job / 'cancel.json').exists())
            (self.job.parent / 'started').write_text('ready', encoding='utf-8')
            return None
        if self.cancel:
            self.assertTrue((self.job / 'cancel.json').exists())
        (self.job / 'state.json').write_text(json.dumps({
            'state': 'cancelled' if self.cancel else 'exited',
            'exit_code': 7, 'success': False}), encoding='utf-8')
        self.process.returncode = self.exit_code
        return self.process.returncode

    def kill(self):
        self.process.returncode = -9

    def test_reads_final_state_only_after_worker_exits(self):
        result = self.case.run_worker('pass')
        self.assertEqual(result['state']['exit_code'], 7)
        self.assertEqual(result['stdout'], b'output')
        self.assertEqual(result['stderr'], b'error')
        self.process.wait.assert_called_once_with(timeout=5)
        self.process.kill.assert_not_called()

    def test_cancel_waits_for_command_start_without_reading_state(self):
        self.cancel = True
        result = self.case.run_worker('pass', cancel=True)
        self.assertEqual(result['state']['state'], 'cancelled')
        self.process.kill.assert_not_called()

    def test_worker_crash_is_not_a_successful_command(self):
        self.exit_code = 2
        with self.assertRaisesRegex(AssertionError, 'Worker exited unsuccessfully'):
            self.case.run_worker('pass')

    def test_worker_deadline_still_kills_and_reaps_process(self):
        self.never_exit = True
        with patch.object(worker.time, 'monotonic', side_effect=[0, 1, 31]):
            with self.assertRaisesRegex(AssertionError, 'did not complete'):
                self.case.run_worker('pass')
        self.process.kill.assert_called_once_with()
        self.process.wait.assert_called_once_with(timeout=5)


if __name__ == '__main__':
    unittest.main()
