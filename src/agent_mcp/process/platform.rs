#[cfg(windows)]
#[path = "windows.rs"]
mod windows;
#[cfg(windows)]
pub(super) use windows::*;

#[cfg(unix)]
pub(super) fn replace(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

#[cfg(unix)]
pub(super) struct Identity;
#[cfg(unix)]
impl Identity {
    pub fn new(_: Option<crate::terminal_service::UserToken>) -> Result<Self, String> {
        Ok(Self)
    }
    pub fn enter(&self) -> Result<(), String> {
        Ok(())
    }
    pub fn root(&self) -> Result<std::path::PathBuf, String> {
        Ok(hbb_common::config::Config::get_home().join(".rustdesk-mcp-jobs"))
    }
    pub fn launch(&self, dir: &std::path::Path) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let mut command =
            std::process::Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        command
            .arg("--mcp-process-worker")
            .arg(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // A dedicated worker must survive the RustDesk connection/UI process exiting.
        unsafe {
            command.pre_exec(|| {
                if hbb_common::libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("mcp-process-reaper".into())
            .spawn(move || {
                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        if let Err(e) = tx.send(Err(e.to_string())) {
                            hbb_common::log::debug!("Worker launch reply: {e}");
                        }
                        return;
                    }
                };
                if let Err(e) = tx.send(Ok(())) {
                    hbb_common::log::debug!("Worker launch reply: {e}");
                }
                if let Err(e) = child.wait() {
                    hbb_common::log::warn!("Wait for job worker: {e}");
                }
            })
            .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
}

#[cfg(unix)]
pub(super) struct Child {
    pub process: std::process::Child,
    stopped: bool,
}
#[cfg(unix)]
impl Child {
    pub fn spawn(command: &mut std::process::Command) -> Result<Self, String> {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        Ok(Self {
            process: command.spawn().map_err(|e| e.to_string())?,
            stopped: false,
        })
    }
    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        let result = unsafe {
            hbb_common::libc::kill(-(self.process.id() as i32), hbb_common::libc::SIGKILL)
        };
        if result != 0
            && std::io::Error::last_os_error().raw_os_error() != Some(hbb_common::libc::ESRCH)
        {
            return Err(format!(
                "Kill command process group: {}",
                std::io::Error::last_os_error()
            ));
        }
        self.process.wait().map_err(|e| e.to_string())?;
        self.stopped = true;
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for Child {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            hbb_common::log::error!("Job cleanup: {e}");
        }
    }
}

#[cfg(unix)]
pub(super) struct Reader<T>(T);
#[cfg(unix)]
impl<T: std::io::Read + std::os::fd::AsRawFd> Reader<T> {
    pub fn new(pipe: T) -> Result<Self, String> {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { hbb_common::libc::fcntl(fd, hbb_common::libc::F_GETFL) };
        if flags < 0
            || unsafe {
                hbb_common::libc::fcntl(
                    fd,
                    hbb_common::libc::F_SETFL,
                    flags | hbb_common::libc::O_NONBLOCK,
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self(pipe))
    }
}
#[cfg(unix)]
impl<T: std::io::Read> std::io::Read for Reader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
