"""Opt-in interactive Windows UIA regression, using only a temporary test window."""
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
import unittest


FIXTURE = r"""
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
$form = New-Object System.Windows.Forms.Form
$form.Text = 'RustDesk UIA regression fixture'
$form.Size = New-Object System.Drawing.Size(600,350)
$form.TopMost = $true
$button = New-Object System.Windows.Forms.Button
$button.Name = 'uia_button'; $button.Text = 'Click me'; $button.SetBounds(20,20,180,40)
$edit = New-Object System.Windows.Forms.TextBox
$edit.Name = 'uia_edit'; $edit.SetBounds(20,80,300,30)
$checkbox = New-Object System.Windows.Forms.CheckBox
$checkbox.Name = 'uia_toggle'; $checkbox.Text = 'Enable test'; $checkbox.SetBounds(20,130,180,30)
$label = New-Object System.Windows.Forms.Label
$label.Name = 'uia_status'; $label.Text = 'Waiting'; $label.SetBounds(20,180,300,30)
$button.Add_Click({$label.Text = 'Clicked'})
$form.Controls.AddRange(@($button,$edit,$checkbox,$label))
$form.Add_Shown({$form.Activate(); [Console]::WriteLine('READY'); [Console]::Out.Flush()})
[void]$form.ShowDialog()
"""


@unittest.skipUnless(sys.platform == "win32" and os.environ.get("RUSTDESK_TEST_UIA") == "1",
                     "set RUSTDESK_TEST_UIA=1 on an interactive Windows desktop")
class WindowsUiaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.powershell = str(Path(os.environ["SystemRoot"]) / "System32/WindowsPowerShell/v1.0/powershell.exe")
        cls.script = (Path(__file__).resolve().parents[2] / "src/platform/agent_uia.ps1").read_text()
        capability = cls.request("capabilities")
        if "error" in capability:
            reason = capability["error"]
            if any(text in reason for text in ("session 0", "secure desktop", "Input desktop is unavailable")):
                raise unittest.SkipTest(reason)
            raise AssertionError(reason)
        cls.fixture = subprocess.Popen([cls.powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Sta", "-Command", FIXTURE],
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, creationflags=subprocess.CREATE_NO_WINDOW)
        cls.addClassCleanup(cls.stop_fixture)
        output = queue.Queue()
        threading.Thread(target=lambda: output.put(cls.fixture.stdout.readline()), daemon=True).start()
        if output.get(timeout=20).strip() != b"READY":
            raise AssertionError("Windows fixture did not open")

    @classmethod
    def stop_fixture(cls):
        cls.fixture.terminate()
        cls.fixture.communicate(timeout=10)

    @classmethod
    def request(cls, operation, **kwargs):
        result = subprocess.run([cls.powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Mta", "-Command", cls.script],
                                input=json.dumps({"operation": operation, **kwargs}).encode(), capture_output=True,
                                creationflags=subprocess.CREATE_NO_WINDOW, timeout=12, check=True)
        return json.loads(result.stdout.decode("utf-8-sig"))

    def tree(self):
        result = self.request("tree")
        self.assertNotIn("error", result)
        self.assertEqual(result["active_window"]["title"], "RustDesk UIA regression fixture")
        return result["elements"]

    def element(self, automation_id):
        deadline = time.monotonic() + 5
        while True:
            element = next((e for e in self.tree() if e["automation_id"] == automation_id), None)
            if element is not None:
                return element
            if time.monotonic() >= deadline:
                self.fail(f"UIA element did not appear: {automation_id}")
            time.sleep(0.1)

    def test_invoke_value_toggle_and_stale_identity(self):
        button = self.element("uia_button")
        self.assertIn("Invoke", button["patterns"])
        self.assertTrue(self.request("invoke", element=button)["acknowledged"])
        self.assertEqual(self.element("uia_status")["name"], "Clicked")
        edit = self.element("uia_edit")
        self.assertIn("Value", edit["patterns"])
        self.assertTrue(self.request("set_value", element=edit, value="hello 保存")["acknowledged"])
        self.assertEqual(self.element("uia_edit")["value"], "hello 保存")
        toggle = self.element("uia_toggle")
        self.assertIn("Toggle", toggle["patterns"])
        self.assertTrue(self.request("toggle", element=toggle)["acknowledged"])
        self.assertNotEqual(self.element("uia_toggle")["toggle_state"], toggle["toggle_state"])
        button["name"] = "wrong identity"
        self.assertIn("STALE_TARGET", self.request("invoke", element=button)["error"])

    def test_window_enumeration_focus_and_stale_process_identity(self):
        windows = self.request("windows")
        self.assertNotIn("error", windows)
        window = next(w for w in windows["windows"] if w["title"] == "RustDesk UIA regression fixture")
        self.assertTrue(window["process_started"])
        self.assertEqual(window["process_id"], str(self.fixture.pid))
        focused = self.request("focus_window", element=window)
        self.assertNotIn("error", focused)
        self.assertTrue(focused["focused"], focused)
        foreground = self.request("foreground")["active_window"]
        self.assertEqual(foreground["handle"], window["handle"])
        self.assertNotIn("elements", foreground)
        stale = dict(window, process_started="1")
        self.assertIn("STALE_TARGET", self.request("focus_window", element=stale)["error"])

    def test_taskbar_scope_is_separate_from_the_foreground_app(self):
        result = self.request("taskbar_tree")
        self.assertNotIn("error", result)
        self.assertEqual(result["scope"], "taskbar")
        self.assertTrue(all(e["scope"] == "taskbar" for e in result["elements"]))
        self.assertFalse(any(e["automation_id"] == "uia_edit" for e in result["elements"]))


@unittest.skipUnless(os.environ.get("RUSTDESK_TEST_PWSH"), "set RUSTDESK_TEST_PWSH to validate the PowerShell helper")
class HelperSyntaxTests(unittest.TestCase):
    def test_script_parses_and_embedded_csharp_compiles(self):
        script = Path(__file__).resolve().parents[2] / "src/platform/agent_uia.ps1"
        command = r'''
$ErrorActionPreference = 'Stop'
$path = [Console]::In.ReadToEnd()
$tokens = $null; $parseErrors = $null
[void][System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$parseErrors)
if ($parseErrors.Count) { throw ($parseErrors | Out-String) }
$source = [System.IO.File]::ReadAllText($path)
$match = [regex]::Match($source, "(?s)Add-Type -TypeDefinition @'\r?\n(.*?)\r?\n'@")
if (-not $match.Success) { throw 'Missing embedded C# helper' }
Add-Type -TypeDefinition $match.Groups[1].Value
[Console]::Write('OK')
'''
        result = subprocess.run([os.environ["RUSTDESK_TEST_PWSH"], "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command],
                                input=str(script).encode(), capture_output=True, timeout=30, check=True)
        self.assertEqual(result.stdout.decode("utf-8-sig"), "OK")


if __name__ == "__main__":
    unittest.main()
