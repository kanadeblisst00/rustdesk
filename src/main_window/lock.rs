use std::io;

#[cfg(unix)]
pub(super) struct InstanceLock {
    _file: std::fs::File,
}

#[cfg(unix)]
impl InstanceLock {
    pub(super) fn acquire(path: &std::path::Path) -> io::Result<Option<Self>> {
        use hbb_common::libc;
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid main window lock file",
            ));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // Never unlink this file: contenders must always lock the same inode.
        Ok(Some(Self { _file: file }))
    }
}

#[cfg(windows)]
pub(super) struct InstanceLock {
    handle: usize,
}

#[cfg(windows)]
impl InstanceLock {
    pub(super) fn acquire(name: &str) -> io::Result<Option<Self>> {
        use winapi::shared::winerror::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS};
        use winapi::um::{
            errhandlingapi::{GetLastError, SetLastError},
            handleapi::CloseHandle,
            synchapi::CreateMutexW,
        };

        let name: Vec<u16> = format!("Local\\{name}_main_window")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            SetLastError(0);
            let handle = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
            let error = GetLastError();
            if !handle.is_null() {
                if error != ERROR_ALREADY_EXISTS {
                    return Ok(Some(Self {
                        handle: handle as usize,
                    }));
                }
                CloseHandle(handle);
                return Ok(None);
            }
            if error == ERROR_ACCESS_DENIED {
                return Ok(None);
            }
            Err(io::Error::from_raw_os_error(error as _))
        }
    }
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            winapi::um::handleapi::CloseHandle(self.handle as _);
        }
    }
}
