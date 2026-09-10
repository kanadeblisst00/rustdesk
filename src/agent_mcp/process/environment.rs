use super::platform::Identity;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn resolve(name: &str, vars: &BTreeMap<String, String>) -> Option<PathBuf> {
    let path = vars.get("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) && !name.contains('.') {
        vars.get("PATHEXT")
            .map(String::as_str)
            .unwrap_or(".EXE;.COM;.BAT;.CMD")
            .split(';')
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    for directory in std::env::split_paths(path) {
        if !directory.is_absolute() {
            continue;
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if let Ok(meta) = std::fs::metadata(&candidate) {
                if !meta.is_file() {
                    continue;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn space(path: &Path) -> Result<Value, String> {
    use std::os::unix::ffi::OsStrExt;
    let path =
        std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| "Invalid disk path")?;
    let mut stats = unsafe { std::mem::zeroed::<hbb_common::libc::statvfs>() };
    if unsafe { hbb_common::libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(
        json!({"available_bytes":(stats.f_bavail as u64).saturating_mul(stats.f_frsize as u64),"total_bytes":(stats.f_blocks as u64).saturating_mul(stats.f_frsize as u64)}),
    )
}

#[cfg(windows)]
fn space(path: &Path) -> Result<Value, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{core::PCWSTR, Win32::Storage::FileSystem::GetDiskFreeSpaceExW};
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    let mut total = 0;
    unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(path.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            None,
        )
    }
    .map_err(|e| e.to_string())?;
    Ok(json!({"available_bytes":available,"total_bytes":total}))
}

fn disk_path(root: &Path, requested: Option<&str>) -> Result<PathBuf, String> {
    let path = match requested {
        Some(path) => PathBuf::from(path),
        None => root
            .ancestors()
            .find(|path| path.is_dir())
            .ok_or("No existing parent directory for job storage")?
            .to_owned(),
    };
    if !path.is_absolute() || !path.is_dir() {
        return Err("Environment disk path must be an existing absolute directory".into());
    }
    Ok(path)
}

pub(super) fn observe(identity: &Identity, args: &Value) -> Result<Value, String> {
    let (vars, user) = identity.environment()?;
    let root = identity.root()?;
    let path = disk_path(&root, args["path"].as_str())?;
    let names: Vec<&str> = match args["executables"].as_array() {
        Some(names) => names
            .iter()
            .map(|n| n.as_str().ok_or("Invalid executable name"))
            .collect::<Result<_, _>>()?,
        None => vec![
            "git", "python3", "python", "cargo", "rustc", "cmake", "ninja", "node", "npm",
            "flutter", "dart", "dotnet", "clang", "gcc", "go", "java", "pwsh", "conda",
        ],
    };
    #[cfg(windows)]
    let names = if args["executables"].is_null() {
        let mut names = names;
        names.extend(["powershell", "cl", "link"]);
        names
    } else {
        names
    };
    let mut tools = Vec::new();
    for name in names {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || matches!(name, "." | "..")
        {
            return Err(
                "Executable checks require simple filenames, not paths or shell expressions".into(),
            );
        }
        let resolved = resolve(name, &vars);
        tools.push(json!({"name":name,"found":resolved.is_some(),"path":resolved,"version_verified":false}));
    }
    #[cfg(windows)]
    let console = json!({"oem_code_page":unsafe { windows::Win32::Globalization::GetOEMCP() },"ansi_code_page":unsafe { windows::Win32::Globalization::GetACP() },"output_encoding":"producer-dependent; read_process_output supports explicit encoding"});
    #[cfg(not(windows))]
    let console = Value::Null;
    let visible: BTreeMap<_, _> = vars
        .iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "PATH"
                    | "PATHEXT"
                    | "SHELL"
                    | "COMSPEC"
                    | "HOME"
                    | "USERPROFILE"
                    | "TEMP"
                    | "TMP"
                    | "DISPLAY"
                    | "WAYLAND_DISPLAY"
                    | "XDG_SESSION_TYPE"
                    | "SESSIONNAME"
            )
        })
        .collect();
    Ok(
        json!({"os":std::env::consts::OS,"process_arch":std::env::consts::ARCH,"logical_cpus":std::thread::available_parallelism().map(|n| n.get()).ok(),"user":user,"environment":visible,"console":console,"job_storage_path":root,"disk":{"path":path,"space":space(&path)?},"executables":tools,"native_uia":cfg!(windows),"interactive_desktop_verified":false,"dependencies_modified":false,"package_checks_performed":false,"dependency_policy":rustdesk_agent_mcp::process::dependency_policy(),"note":"Executable discovery checks PATH files, not versions or package imports. Use explicit run_process probes and follow dependency_policy for any provisioning commands. No dependencies were installed."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_only_requested_tools_and_does_not_run_them() {
        let root =
            std::env::temp_dir().join(format!("mcp-environment-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let name = if cfg!(windows) { "probe.exe" } else { "probe" };
        let executable = root.join(name);
        std::fs::write(&executable, "this is not an executable program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut vars = BTreeMap::new();
        vars.insert("PATH".into(), root.to_string_lossy().into_owned());
        assert_eq!(resolve(name, &vars), Some(executable));
        assert_eq!(resolve("missing-tool", &vars), None);
        assert!(space(&root).unwrap()["available_bytes"].as_u64().is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_probe_does_not_create_job_storage() {
        let root = std::env::temp_dir().join(format!("mcp-probe-{}", uuid::Uuid::new_v4()));
        let storage = root.join("jobs");
        let parent = disk_path(&storage, None).unwrap();
        assert!(parent.is_dir());
        assert!(space(&parent).unwrap()["available_bytes"]
            .as_u64()
            .is_some());
        assert!(!root.exists());
        assert!(disk_path(&storage, Some(storage.to_str().unwrap())).is_err());
        assert!(!root.exists());
        assert!(disk_path(&storage, Some("relative")).is_err());
    }
}
