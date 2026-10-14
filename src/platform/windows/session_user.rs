use super::log;

pub(super) fn get_session_username(session_id: u32) -> String {
    extern "C" {
        fn get_session_user_info(path: *mut u16, n: u32, session_id: u32) -> u32;
    }
    let mut buffer = [0u16; 257];
    // The native bridge accepts and returns byte counts, including the terminating NUL.
    let bytes = unsafe {
        get_session_user_info(
            buffer.as_mut_ptr(),
            std::mem::size_of_val(&buffer) as u32,
            session_id,
        )
    };
    if bytes == 0 {
        log::warn!(
            "Failed to query Windows session {session_id} user: {}",
            std::io::Error::last_os_error()
        );
        // WTS queries fail when Remote Desktop Services is stopped. Only an ordinary
        // process in an interactive session can safely use its own account as fallback.
        if session_id != 0 && session_id != u32::MAX && !super::is_root() {
            return crate::username();
        }
        return String::new();
    }
    let len = bytes as usize / std::mem::size_of::<u16>();
    let Some(units) = buffer.get(..len).filter(|_| bytes % 2 == 0) else {
        log::warn!("Invalid Windows session username length: {bytes}");
        return String::new();
    };
    match String::from_utf16(units) {
        Ok(name) => name.trim_end_matches('\0').to_owned(),
        Err(err) => {
            log::warn!("Invalid Windows session username: {err}");
            String::new()
        }
    }
}
