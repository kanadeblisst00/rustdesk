use core_foundation::{
    base::TCFType,
    string::{CFString, CFStringRef},
};
use std::{ffi::c_void, ptr};

#[link(name = "SystemConfiguration", kind = "framework")]
extern "C" {
    fn SCDynamicStoreCopyConsoleUser(
        store: *const c_void,
        uid: *mut u32,
        gid: *mut u32,
    ) -> CFStringRef;
    fn SCError() -> i32;
}

pub(super) fn get_active_user() -> Result<(String, u32), i32> {
    let mut uid = 0;
    // /dev/console can remain owned by root even while a GUI user is logged in.
    let name = unsafe { SCDynamicStoreCopyConsoleUser(ptr::null(), &mut uid, ptr::null_mut()) };
    if name.is_null() {
        let error = unsafe { SCError() };
        return if error == 0 {
            Ok((String::new(), 0))
        } else {
            Err(error)
        };
    }
    let name = unsafe { CFString::wrap_under_create_rule(name) }.to_string();
    Ok(console_user(name, uid))
}

fn console_user(name: String, uid: u32) -> (String, u32) {
    if uid == 0 || name.is_empty() || name == "loginwindow" {
        (String::new(), 0)
    } else {
        (name, uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_in_console_user_keeps_name_and_uid() {
        assert_eq!(console_user("alice".into(), 501), ("alice".into(), 501));
        assert_eq!(
            console_user("测试用户".into(), 502),
            ("测试用户".into(), 502)
        );
    }

    #[test]
    fn login_window_is_not_a_logged_in_user() {
        assert_eq!(console_user("loginwindow".into(), 501), (String::new(), 0));
    }

    #[test]
    fn root_console_remains_prelogin() {
        assert_eq!(console_user("root".into(), 0), (String::new(), 0));
    }

    #[test]
    fn missing_console_name_remains_prelogin() {
        assert_eq!(console_user(String::new(), 501), (String::new(), 0));
    }
}
