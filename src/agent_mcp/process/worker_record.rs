use super::store;
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

fn event(dir: &Path, pid: u32, kind: &str) -> Value {
    json!({"version":1,"job_id":dir.file_name().and_then(|s|s.to_str()),"worker_pid":pid,
        "event":kind,"observed_at_ms":store::now(),"command_result_inferred":false})
}

fn create(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|e| e.to_string())
}

fn append(file: &mut File, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}

pub(super) struct Observer {
    file: File,
    identity: Value,
}
impl Observer {
    pub(super) fn start(dir: &Path, pid: u32) -> Option<Self> {
        let result = (|| {
            let mut file = create(&dir.join("worker-observer.jsonl"))?;
            let mut identity = event(dir, pid, "watch_started");
            identity["observer_pid"] = json!(std::process::id());
            identity["identity_source"] =
                json!("original spawned child/owned handle; no PID reopen");
            append(&mut file, &identity)?;
            Ok::<_, String>(Self { file, identity })
        })();
        match result {
            Ok(observer) => Some(observer),
            Err(error) => {
                hbb_common::log::warn!("Start worker exit evidence: {error}");
                None
            }
        }
    }

    pub(super) fn finish(mut self, status: Result<&std::process::ExitStatus, String>) {
        self.identity["event"] = json!("watch_finished");
        self.identity["observed_at_ms"] = json!(store::now());
        match status {
            Ok(status) => {
                self.identity["worker_exit_observed"] = json!(true);
                self.identity["worker_exit_code"] = json!(status.code());
                self.identity["worker_exit_success"] = json!(status.success());
                #[cfg(windows)]
                if let Some(code) = status.code() {
                    self.identity["worker_exit_code_hex"] = json!(format!("0x{:08X}", code as u32));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    self.identity["worker_signal"] = json!(status.signal());
                }
            }
            Err(error) => {
                self.identity["worker_exit_observed"] = json!(false);
                self.identity["observer_error"] = json!(error);
            }
        }
        if let Err(error) = append(&mut self.file, &self.identity) {
            hbb_common::log::warn!("Persist worker exit evidence: {error}");
        }
    }
}

// The file is opened before handing work to a thread, preserving the authorized identity's access.
#[cfg(windows)]
pub(super) fn watch_windows(
    dir: &Path,
    pid: u32,
    handle: std::os::windows::io::OwnedHandle,
) -> Option<std::thread::JoinHandle<()>> {
    use std::os::windows::{io::AsRawHandle, process::ExitStatusExt};
    use windows::Win32::{
        Foundation::{HANDLE, WAIT_OBJECT_0},
        System::Threading::{GetExitCodeProcess, WaitForSingleObject},
    };
    let observer = Observer::start(dir, pid)?;
    match std::thread::Builder::new()
        .name("mcp-worker-exit".into())
        .spawn(move || {
            let result = (|| {
                let raw = HANDLE(handle.as_raw_handle());
                if unsafe { WaitForSingleObject(raw, u32::MAX) } != WAIT_OBJECT_0 {
                    return Err(format!(
                        "WaitForSingleObject(worker): {}",
                        std::io::Error::last_os_error()
                    ));
                }
                let mut code = 0;
                unsafe { GetExitCodeProcess(raw, &mut code) }
                    .map_err(|e| format!("GetExitCodeProcess(worker): {e}"))?;
                Ok(std::process::ExitStatus::from_raw(code))
            })();
            observer.finish(result.as_ref().map_err(Clone::clone));
        }) {
        Ok(thread) => Some(thread),
        Err(error) => {
            hbb_common::log::warn!("Start worker exit observer thread: {error}");
            None
        }
    }
}

#[cfg(not(test))]
pub(super) fn install_panic_hook(dir: &Path) {
    let mut file = match create(&dir.join("worker-panic.jsonl")) {
        Ok(file) => file,
        Err(error) => {
            hbb_common::log::warn!("Create worker panic evidence: {error}");
            return;
        }
    };
    let identity = event(dir, std::process::id(), "panic_hook_armed");
    if let Err(error) = append(&mut file, &identity) {
        hbb_common::log::warn!("Arm worker panic evidence: {error}");
        return;
    }
    let file = std::sync::Mutex::new(file);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(
            &file,
            &identity,
            info.location()
                .map(|p| json!({"file":p.file(),"line":p.line()})),
        );
        previous(info);
    }));
}

fn record_panic(file: &std::sync::Mutex<File>, identity: &Value, location: Option<Value>) {
    let mut value = identity.clone();
    value["event"] = json!("panic");
    value["observed_at_ms"] = json!(store::now());
    value["source_location"] = json!(location);
    value["payload_recorded"] = json!(false);
    // Release builds abort on panic; catch_unwind cannot supply this evidence.
    match file.try_lock() {
        Ok(mut file) => {
            if append(&mut file, &value).is_err() {
                eprintln!("MCP worker panic evidence could not be persisted");
            }
        }
        Err(_) => eprintln!("MCP worker panic evidence file is busy"),
    }
}

pub(super) fn checkpoint(dir: &Path, state: &Value, stage: &str) {
    let mut record = event(dir, std::process::id(), "progress");
    record["stage"] = json!(stage);
    if state["exit_code"].is_number() || state["signal"].is_number() {
        record["command_exit_observation"] = json!({"exit_code":state["exit_code"],"exit_code_hex":state["exit_code_hex"],
            "signal":state["signal"],"success":state["success"],"final_job_result":false});
    }
    if let Err(error) = store::write_json(&dir.join("worker-progress.json"), &record) {
        hbb_common::log::warn!("Persist worker progress at {stage}: {error}");
    }
}

fn identity_matches(value: &Value, id: &str, pid: Option<u64>) -> bool {
    value["version"] == 1
        && value["job_id"] == id
        && value["worker_pid"].as_u64().is_some_and(|observed| {
            observed > 0
                && observed <= u32::MAX as u64
                && pid.map_or(true, |expected| expected == observed)
        })
}

fn journal(path: &Path, id: &str, pid: Option<u64>) -> Result<Value, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 8192 {
        return Err("Worker evidence exceeds its bounded size".into());
    }
    let mut events = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        let value: Value = serde_json::from_slice(line).map_err(|e| e.to_string())?;
        if !identity_matches(&value, id, pid) {
            return Err("Worker evidence identity mismatch".into());
        }
        events.push(value);
    }
    Ok(json!({"events":events,"trailing_record_incomplete":!bytes.ends_with(b"\n")}))
}

pub(super) fn annotate(dir: &Path, id: &str, state: &mut Value) {
    let mut evidence = json!({});
    let pid = state["worker_pid"].as_u64();
    for (key, name) in [
        ("observer", "worker-observer.jsonl"),
        ("panic", "worker-panic.jsonl"),
        ("progress", "worker-progress.json"),
    ] {
        let path = dir.join(name);
        let result = match path.try_exists() {
            Ok(false) => continue,
            Ok(true) if key == "progress" => store::read_json(&path).and_then(|v| {
                if identity_matches(&v, id, pid) {
                    Ok(v)
                } else {
                    Err("Worker progress identity mismatch".into())
                }
            }),
            Ok(true) => journal(&path, id, pid),
            Err(error) => Err(error.to_string()),
        };
        evidence[key] = match result {
            Ok(value) => value,
            Err(error) => json!({"error":error}),
        };
    }
    if evidence.as_object().is_some_and(|v| !v.is_empty()) {
        state["worker_evidence"] = evidence;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_exit_and_panic_evidence_never_become_the_command_exit_code() {
        let root = std::env::temp_dir().join(format!("worker-record-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(root.clone()).unwrap();
        let state = store.create(&json!({"job_id":"job"}), |_| Ok(())).unwrap();
        let dir = store.directory("job").unwrap();
        #[cfg(unix)]
        let status = {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(7 << 8)
        };
        #[cfg(windows)]
        let status = {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(0xC0000005)
        };
        Observer::start(&dir, 42).unwrap().finish(Ok(&status));
        let panic_file = std::sync::Mutex::new(create(&dir.join("worker-panic.jsonl")).unwrap());
        record_panic(
            &panic_file,
            &event(&dir, 42, "armed"),
            Some(json!({"line":100})),
        );
        let mut state = state;
        state["updated_at_ms"] = json!(1);
        store::write_json(&dir.join("state.json"), &state).unwrap();
        let result = store.status("job").unwrap();
        assert_eq!(result["state"], "unknown");
        assert!(result["exit_code"].is_null());
        assert!(result["success"].is_null());
        assert_eq!(
            result["worker_evidence"]["observer"]["events"][1]["worker_exit_observed"],
            true
        );
        assert_eq!(
            result["worker_evidence"]["panic"]["events"][0]["payload_recorded"],
            false
        );
        let mut file = OpenOptions::new()
            .append(true)
            .open(dir.join("worker-observer.jsonl"))
            .unwrap();
        file.write_all(b"{\"unfinished\":").unwrap();
        assert_eq!(
            store.status("job").unwrap()["worker_evidence"]["observer"]
                ["trailing_record_incomplete"],
            true
        );
        drop(file);
        drop(panic_file);
        std::fs::write(dir.join("worker-progress.json"), b"corrupt").unwrap();
        let result = store.status("job").unwrap();
        assert_eq!(result["state"], "unknown");
        assert!(result["worker_evidence"]["progress"]["error"].is_string());
        assert_eq!(store.list().unwrap()["capacity"]["unknown"], 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn authorized_handle_observer_records_windows_worker_exit_without_pid_reopen() {
        use std::{
            os::windows::io::AsHandle,
            time::{Duration, Instant},
        };
        let root = std::env::temp_dir().join(format!("worker-handle-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(root.clone()).unwrap();
        store.create(&json!({"job_id":"job"}), |_| Ok(())).unwrap();
        let dir = store.directory("job").unwrap();
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "exit 7"])
            .spawn()
            .unwrap();
        let observer = watch_windows(
            &dir,
            child.id(),
            child.as_handle().try_clone_to_owned().unwrap(),
        )
        .unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(7));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let state = store.status("job").unwrap();
            if state["worker_evidence"]["observer"]["events"][1]["worker_exit_observed"] == true {
                assert_eq!(
                    state["worker_evidence"]["observer"]["events"][1]["worker_exit_code"],
                    7
                );
                assert!(state["exit_code"].is_null());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "No exit receipt from the retained worker handle"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        observer.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
