//! Tests the production login detection modules without desktop SDK dependencies.
#![allow(dead_code)]

pub use log;
use std::cell::RefCell;

#[path = "../../src/platform/linux_session_user.rs"]
mod linux_session_user;
#[path = "../../src/platform/windows/session_user.rs"]
mod windows_session_user;

const CMD_LOGINCTL: &str = "loginctl";

fn get_values_of_seat0(_: &[usize]) -> Vec<String> {
    vec!["test-session".into()]
}

#[derive(Default)]
struct Session {
    wts_username: Option<Vec<u16>>,
    system: bool,
    process_username: String,
}

thread_local! {
    static SESSION: RefCell<Session> = RefCell::new(Session::default());
}

fn is_root() -> bool {
    SESSION.with(|s| s.borrow().system)
}

fn username() -> String {
    SESSION.with(|s| s.borrow().process_username.clone())
}

// Emulate the existing C++ bridge's byte-count contract, including an empty
// successful response (one UTF-16 NUL) and a failed query (zero bytes).
#[no_mangle]
unsafe extern "C" fn get_session_user_info(buffer: *mut u16, capacity: u32, _: u32) -> u32 {
    SESSION.with(|s| {
        let s = s.borrow();
        let Some(units) = s.wts_username.as_ref() else {
            return 0;
        };
        let bytes = (units.len() * 2).min(capacity as usize);
        std::ptr::copy_nonoverlapping(units.as_ptr().cast::<u8>(), buffer.cast::<u8>(), bytes);
        bytes as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configure(wts: Option<&str>, system: bool, process: &str) {
        SESSION.with(|s| {
            *s.borrow_mut() = Session {
                wts_username: wts.map(|name| name.encode_utf16().chain(Some(0)).collect()),
                system,
                process_username: process.into(),
            }
        });
    }

    #[test]
    fn windows_wts_failure_keeps_logged_in_process_user() {
        configure(None, false, "alice");
        assert_eq!(windows_session_user::get_session_username(1), "alice");
    }

    #[test]
    fn windows_successful_empty_query_does_not_fall_back() {
        configure(Some(""), false, "alice");
        assert_eq!(windows_session_user::get_session_username(1), "");
    }

    #[test]
    fn windows_system_process_does_not_become_a_logged_in_user() {
        configure(None, true, "SYSTEM");
        assert_eq!(windows_session_user::get_session_username(1), "");
    }

    #[test]
    fn windows_service_and_invalid_sessions_do_not_fall_back() {
        configure(None, false, "service-account");
        assert_eq!(windows_session_user::get_session_username(0), "");
        assert_eq!(windows_session_user::get_session_username(u32::MAX), "");
    }

    #[test]
    fn windows_session_owner_takes_precedence_over_process_identity() {
        configure(Some("rdp-user"), true, "SYSTEM");
        assert_eq!(windows_session_user::get_session_username(7), "rdp-user");
        configure(Some("console-user"), false, "run-as-admin");
        assert_eq!(
            windows_session_user::get_session_username(1),
            "console-user"
        );
    }

    #[test]
    fn windows_long_unicode_name_is_not_truncated_at_a_surrogate() {
        let name = format!("{}😀用户", "a".repeat(127));
        configure(Some(&name), false, "wrong-user");
        assert_eq!(windows_session_user::get_session_username(1), name);
    }

    #[test]
    fn windows_invalid_utf16_does_not_fall_back_to_another_account() {
        configure(None, false, "wrong-user");
        SESSION.with(|s| s.borrow_mut().wts_username = Some(vec![0xD800, 0]));
        assert_eq!(windows_session_user::get_session_username(1), "");
    }

    #[test]
    fn linux_no_active_user_is_prelogin_without_enumerating_passwd() {
        assert!(linux_session_user::is_prelogin(""));
    }
}
