use serde_json::{json,Value};
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows::{core::PCWSTR, Win32::{
    Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
    Storage::FileSystem::{CreateFileW, FILE_ADD_FILE, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_SHARE_DELETE, OPEN_EXISTING},
    System::Registry::{RegGetValueW,RegOpenCurrentUser,RegCloseKey,HKEY,HKEY_LOCAL_MACHINE,
        KEY_READ,RRF_RT_REG_SZ,RRF_RT_REG_EXPAND_SZ,RRF_NOEXPAND},
}};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        let status = unsafe { RegCloseKey(self.0) };
        if status.is_err() { hbb_common::log::warn!("Close environment registry key: {status:?}"); }
    }
}

pub(super) fn variable(name: &str, machine: bool) -> Result<Option<String>,String> {
    let user = if machine {None} else {
        let mut key = HKEY::default();
        unsafe { RegOpenCurrentUser(KEY_READ.0,&mut key) }.ok().map_err(|e|format!("RegOpenCurrentUser: {e}"))?;
        Some(Key(key))
    };
    let key = user.as_ref().map(|k|k.0).unwrap_or(HKEY_LOCAL_MACHINE);
    let subkey:Vec<u16> = if machine {r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"} else {"Environment"}
        .encode_utf16().chain(Some(0)).collect();
    let name:Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut size = 0u32;
    let status = unsafe { RegGetValueW(key,PCWSTR(subkey.as_ptr()),PCWSTR(name.as_ptr()),flags,None,None,Some(&mut size)) };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND { return Ok(None); }
    status.ok().map_err(|e|format!("RegGetValueW(size): {e}"))?;
    if size > 65536 || size % 2 != 0 { return Err("Invalid bounded root environment value".into()); }
    let mut data = vec![0u16;size as usize/2];
    unsafe { RegGetValueW(key,PCWSTR(subkey.as_ptr()),PCWSTR(name.as_ptr()),flags,None,
        Some(data.as_mut_ptr() as _),Some(&mut size)) }.ok().map_err(|e|format!("RegGetValueW(value): {e}"))?;
    data.truncate(size as usize/2);
    while data.last() == Some(&0) { data.pop(); }
    let value = String::from_utf16(&data).map_err(|_|"Root environment value is not valid Unicode")?;
    if value.contains('\0') { return Err("Root environment value contains embedded NUL".into()); }
    Ok(Some(value))
}

pub(super) fn writable(path: &Path) -> Value {
    let wide:Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    match unsafe { CreateFileW(PCWSTR(wide.as_ptr()),FILE_ADD_FILE.0,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,None,OPEN_EXISTING,FILE_FLAG_BACKUP_SEMANTICS,None) } {
        Ok(handle) => {
            let close = unsafe { CloseHandle(handle) }.err().map(|e|e.to_string());
            json!({"writable":true,"method":"Open existing directory with FILE_ADD_FILE under authorized identity; no file created","cleanup_error":close})
        }
        Err(error) => json!({"writable":if error.code() == windows::core::HRESULT::from_win32(5) {json!(false)} else {Value::Null},"operation":"CreateFileW(directory access)",
            "hresult":format!("0x{:08X}",error.code().0 as u32),"error":error.to_string()}),
    }
}
