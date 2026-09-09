use std::{
    ffi::OsStr,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::{Path, PathBuf},
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_BROKEN_PIPE, HANDLE,
        },
        Security::{ImpersonateLoggedOnUser, RevertToSelf},
        Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
        System::{
            Com::CoTaskMemFree,
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Pipes::PeekNamedPipe,
            Threading::{
                CreateProcessAsUserW, GetCurrentProcess, OpenThread, ResumeThread,
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
                PROCESS_INFORMATION, STARTUPINFOW, THREAD_SUSPEND_RESUME,
            },
        },
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT},
    },
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if let Err(e) = unsafe { CloseHandle(self.0) } {
            hbb_common::log::warn!("Close process handle: {e}");
        }
    }
}
fn wide(path: &OsStr) -> Vec<u16> {
    path.encode_wide().chain(Some(0)).collect()
}

fn system_directory() -> Result<PathBuf, String> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buffer = vec![0u16; 32768];
    let size = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if size == 0 || size >= buffer.len() {
        return Err("Unable to locate the Windows system directory".into());
    }
    String::from_utf16(&buffer[..size])
        .map(PathBuf::from)
        .map_err(|e| e.to_string())
}

fn complete_path(path: &str, system: &Path) -> Result<String, String> {
    let mut directories: Vec<_> = std::env::split_paths(path).collect();
    for directory in [
        system.to_owned(),
        system.join("WindowsPowerShell").join("v1.0"),
        system.join("Wbem"),
    ] {
        if !directories.iter().any(|p| {
            p.to_string_lossy()
                .eq_ignore_ascii_case(&directory.to_string_lossy())
        }) {
            directories.push(directory);
        }
    }
    std::env::join_paths(directories)
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|_| "Windows PATH is not valid Unicode".into())
}

pub(in super::super) fn process_path() -> Result<String, String> {
    let path = match std::env::var("PATH") {
        Ok(path) => path,
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(error) => return Err(format!("Read authorized user's PATH: {error}")),
    };
    complete_path(&path, &system_directory()?)
}

pub(in super::super) fn shell_arguments(
    command: &mut std::process::Command,
    spec: &serde_json::Value,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let script = spec["args"][0].as_str().ok_or("Missing shell script")?;
    if spec["shell"] == "cmd" {
        // cmd's /S /C grammar is not the C-runtime argv grammar used by Command::arg.
        command
            .args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{script}\""));
    } else {
        use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
        let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-EncodedCommand",
            ])
            .arg(STANDARD.encode(bytes));
    }
    Ok(())
}

pub(in super::super) fn replace(from: &Path, to: &Path) -> Result<(), String> {
    unsafe {
        MoveFileExW(
            PCWSTR(wide(from.as_os_str()).as_ptr()),
            PCWSTR(wide(to.as_os_str()).as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| e.to_string())
}

pub(in super::super) struct Identity {
    token: Option<usize>,
}
impl Identity {
    pub fn environment(
        &self,
    ) -> Result<
        (
            std::collections::BTreeMap<String, String>,
            serde_json::Value,
        ),
        String,
    > {
        if self.token.is_none() {
            let mut vars: std::collections::BTreeMap<String, String> = std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((
                        key.into_string().ok()?.to_ascii_uppercase(),
                        value.into_string().ok()?,
                    ))
                })
                .collect();
            vars.insert("PATH".into(), process_path()?);
            let user = serde_json::json!({"name":vars.get("USERNAME"),"domain":vars.get("USERDOMAIN"),"identity_source":"authorized terminal process"});
            return Ok((vars, user));
        }
        let mut environment = std::ptr::null_mut();
        unsafe {
            CreateEnvironmentBlock(&mut environment, self.token.map(|t| HANDLE(t as _)), false)
        }
        .map_err(|e| e.to_string())?;
        let result = (|| {
            let mut vars = std::collections::BTreeMap::new();
            let mut offset = 0;
            let pointer = environment as *const u16;
            while unsafe { *pointer.add(offset) } != 0 {
                let start = offset;
                while unsafe { *pointer.add(offset) } != 0 {
                    offset += 1;
                    if offset > 262144 {
                        return Err("User environment exceeds size limit".into());
                    }
                }
                let line = String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(pointer.add(start), offset - start)
                });
                if let Some((key, value)) = line.split_once('=') {
                    if !key.is_empty() {
                        vars.insert(key.to_ascii_uppercase(), value.to_owned());
                    }
                }
                offset += 1;
            }
            let path = complete_path(
                vars.get("PATH").map(String::as_str).unwrap_or(""),
                &system_directory()?,
            )?;
            vars.insert("PATH".into(), path);
            let user = serde_json::json!({"name":vars.get("USERNAME"),"domain":vars.get("USERDOMAIN"),"identity_source":if self.token.is_some(){"authorized terminal logon token"}else{"authorized terminal process"}});
            Ok((vars, user))
        })();
        if let Err(e) = unsafe { DestroyEnvironmentBlock(environment) } {
            hbb_common::log::warn!("Destroy environment probe: {e}");
        }
        result
    }
    pub fn new(token: Option<crate::terminal_service::UserToken>) -> Result<Self, String> {
        let token = if let Some(token) = token {
            let mut duplicate = HANDLE::default();
            unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    HANDLE(token.as_raw() as _),
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    false,
                    DUPLICATE_SAME_ACCESS,
                )
            }
            .map_err(|e| e.to_string())?;
            Some(duplicate.0 as usize)
        } else {
            None
        };
        Ok(Self { token })
    }
    pub fn enter(&self) -> Result<Impersonation, String> {
        if let Some(token) = self.token {
            unsafe { ImpersonateLoggedOnUser(HANDLE(token as _)) }.map_err(|e| e.to_string())?;
        }
        Ok(Impersonation(self.token.is_some()))
    }
    pub fn root(&self) -> Result<PathBuf, String> {
        let ptr = unsafe {
            SHGetKnownFolderPath(
                &FOLDERID_LocalAppData,
                KF_FLAG_DEFAULT,
                self.token.map(|t| HANDLE(t as _)),
            )
        }
        .map_err(|e| e.to_string())?;
        let path = unsafe { ptr.to_string() }.map_err(|e| e.to_string());
        unsafe {
            CoTaskMemFree(Some(ptr.0 as _));
        }
        Ok(PathBuf::from(path?).join("RustDeskMCP").join("jobs"))
    }
    pub fn launch(&self, dir: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let Some(token) = self.token else {
            let mut command = std::process::Command::new(exe);
            command
                .arg("--mcp-process-worker")
                .arg(dir)
                .creation_flags(CREATE_NO_WINDOW.0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
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
            return rx.recv().map_err(|e| e.to_string())?;
        };
        // Both quoted paths end in fixed filenames/IDs, so neither ends in a backslash.
        let command_line = format!(
            "\"{}\" --mcp-process-worker \"{}\"",
            exe.display(),
            dir.display()
        );
        if command_line.contains('\0')
            || exe.to_string_lossy().contains('"')
            || dir.to_string_lossy().contains('"')
        {
            return Err("Invalid worker path".into());
        }
        let mut line = wide(OsStr::new(&command_line));
        let executable = wide(exe.as_os_str());
        let mut environment = std::ptr::null_mut();
        unsafe { CreateEnvironmentBlock(&mut environment, Some(HANDLE(token as _)), false) }
            .map_err(|e| format!("Load authorized user's environment: {e}"))?;
        let mut startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut desktop = wide(OsStr::new("winsta0\\default"));
        startup.lpDesktop = PWSTR(desktop.as_mut_ptr());
        let mut process = PROCESS_INFORMATION::default();
        let result = unsafe {
            CreateProcessAsUserW(
                Some(HANDLE(token as _)),
                PCWSTR(executable.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                Some(environment),
                PCWSTR::null(),
                &startup,
                &mut process,
            )
        };
        if let Err(e) = unsafe { DestroyEnvironmentBlock(environment) } {
            hbb_common::log::warn!("Destroy user environment: {e}");
        }
        result.map_err(|e| format!("Launch worker as authorized user: {e}"))?;
        let _process = Handle(process.hProcess);
        let _thread = Handle(process.hThread);
        Ok(())
    }
}
impl Drop for Identity {
    fn drop(&mut self) {
        if let Some(token) = self.token {
            drop(Handle(HANDLE(token as _)));
        }
    }
}
pub(in super::super) struct Impersonation(bool);
impl Drop for Impersonation {
    fn drop(&mut self) {
        if self.0 && unsafe { RevertToSelf() }.is_err() {
            // Continuing a pooled thread under another user's identity is unsafe.
            std::process::abort();
        }
    }
}

pub(in super::super) struct Child {
    pub process: std::process::Child,
    job: Handle,
    stopped: bool,
}
impl Child {
    pub fn spawn(command: &mut std::process::Command) -> Result<Self, String> {
        use std::os::windows::process::CommandExt;
        let job =
            Handle(unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| e.to_string())?);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as _,
                std::mem::size_of_val(&limits) as u32,
            )
        }
        .map_err(|e| e.to_string())?;
        let process = command
            .creation_flags(CREATE_SUSPENDED.0 | CREATE_NO_WINDOW.0)
            .spawn()
            .map_err(|e| e.to_string())?;
        let mut child = Self {
            process,
            job,
            stopped: false,
        };
        let setup = (|| {
            unsafe { AssignProcessToJobObject(child.job.0, HANDLE(child.process.as_raw_handle())) }
                .map_err(|e| e.to_string())?;
            // The initial thread has not run: attach the process tree before resuming it.
            let snapshot = Handle(
                unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
                    .map_err(|e| e.to_string())?,
            );
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            unsafe { Thread32First(snapshot.0, &mut entry) }.map_err(|e| e.to_string())?;
            loop {
                if entry.th32OwnerProcessID == child.process.id() {
                    let thread = Handle(
                        unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                            .map_err(|e| e.to_string())?,
                    );
                    if unsafe { ResumeThread(thread.0) } == u32::MAX {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    return Ok(());
                }
                if unsafe { Thread32Next(snapshot.0, &mut entry) }.is_err() {
                    return Err("Suspended command thread not found".into());
                }
            }
        })();
        if let Err(e) = setup {
            child
                .process
                .kill()
                .map_err(|kill| format!("{e}; terminate suspended command: {kill}"))?;
            child.stop()?;
            return Err(e);
        }
        Ok(child)
    }
    pub fn stop(&mut self) -> Result<(), String> {
        if !self.stopped {
            unsafe { TerminateJobObject(self.job.0, 1) }.map_err(|e| e.to_string())?;
            self.process.wait().map_err(|e| e.to_string())?;
            self.stopped = true;
        }
        Ok(())
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            hbb_common::log::error!("Job cleanup: {e}");
        }
    }
}

pub(in super::super) struct Reader<T>(T);
impl<T> Reader<T> {
    pub fn new(pipe: T) -> Result<Self, String> {
        Ok(Self(pipe))
    }
}
impl<T: std::io::Read + AsRawHandle> std::io::Read for Reader<T> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut available = 0;
        let result = unsafe {
            PeekNamedPipe(
                HANDLE(self.0.as_raw_handle()),
                None,
                0,
                None,
                Some(&mut available),
                None,
            )
        };
        if let Err(e) = result {
            if e.code() == windows::core::HRESULT::from_win32(ERROR_BROKEN_PIPE.0) {
                return Ok(0);
            }
            return Err(std::io::Error::other(e.to_string()));
        }
        if available == 0 {
            return Err(std::io::ErrorKind::WouldBlock.into());
        }
        let size = buf.len().min(available as usize);
        self.0.read(&mut buf[..size])
    }
}
