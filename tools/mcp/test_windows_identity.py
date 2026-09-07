"""Exercise Windows installation selection without accessing the host registry."""

from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(shutil.which("rustc"), "requires rustc for the Windows registry fixture")
class WindowsIdentityTest(unittest.TestCase):
    def test_official_inno_installation_is_never_selected_by_mcp(self):
        source = (ROOT / "src/platform/windows.rs").read_text()
        functions = []
        for name in ["get_subkey", "get_mcp_install_subkey", "get_valid_subkey", "get_before_uninstall"]:
            start = source.index("fn " + name + "(")
            end = source.index("\n}", start) + 2
            functions.append(source[start:end])
        constants = [re.search(r"(?m)^const " + name + r":.*;$", source).group()
                     for name in ["IS1", "HKLM_PREFIX"]]
        identity = (ROOT / "src/agent_mcp/identity.rs").read_text()
        constants.append(re.search(r"(?m)^pub const APP_NAME:.*;$", identity).group())
        privacy = (ROOT / "src/privacy_mode/win_topmost_window.rs").read_text()
        constants.append(re.findall(r"(?m)^pub const WIN_TOPMOST_INJECTED_PROCESS_EXE:.*;$", privacy)[-1])
        fixture = r'''
use std::cell::RefCell;
thread_local! { static INSTALLED: RefCell<Vec<String>> = RefCell::new(Vec::new()); }
fn get_app_name() -> String { APP_NAME.into() }
fn get_current_pid() -> u32 { 123 }
fn get_reg_of(key: &str, _: &str) -> String {
    INSTALLED.with(|keys| if keys.borrow().iter().any(|k| k == key) { "installed".into() } else { String::new() })
}
#[test]
fn legacy_official_and_mcp_installations_remain_separate() {
    let official = vec![get_subkey(IS1, false), get_subkey(IS1, true), get_subkey("RustDesk", false)];
    INSTALLED.with(|keys| *keys.borrow_mut() = official.clone());
    assert_eq!(get_valid_subkey(), get_subkey(APP_NAME, false));
    for mcp_key in [get_subkey(APP_NAME, true), format!("{HKLM_PREFIX}Software\\{APP_NAME}\\InstallState\\{APP_NAME}")] {
        INSTALLED.with(|keys| {
            *keys.borrow_mut() = official.clone();
            keys.borrow_mut().push(mcp_key.clone());
        });
        assert_eq!(get_valid_subkey(), mcp_key);
    }
    let uninstall = get_before_uninstall(false);
    assert!(uninstall.contains("sc stop RustDeskMCP"));
    assert!(uninstall.contains("taskkill /F /IM RustDeskMCP.exe"));
    assert!(uninstall.contains("RuntimeBroker_rustdeskmcp.exe"));
    assert!(!uninstall.contains("RuntimeBroker_rustdesk.exe"));
    assert!(!uninstall.contains("HKEY_CLASSES_ROOT\\rustdesk /f"));
}
'''
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "fixture.rs"
            path.write_text("\n".join(constants + functions) + fixture)
            executable = Path(temp) / "fixture.exe"
            subprocess.run([shutil.which("rustc"), "--edition=2021", "--test", "--cfg", 'feature="mcp"',
                            str(path), "-o", str(executable)], check=True, capture_output=True, text=True)
            subprocess.run([str(executable)], check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
