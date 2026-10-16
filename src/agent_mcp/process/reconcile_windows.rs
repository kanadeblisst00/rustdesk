use serde_json::{json, Value};
use windows::{core::PCWSTR, Win32::{
    Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_OBJECT_0},
    System::{
        JobObjects::{OpenJobObjectW, QueryInformationJobObject, JobObjectBasicAccountingInformation,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION},
        Threading::{GetProcessTimes, OpenProcess, WaitForSingleObject,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE},
    },
}};

// windows 0.61 otherwise requires the unrelated SystemServices feature for this SDK constant.
const JOB_OBJECT_QUERY: u32 = 0x0004;

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            hbb_common::log::warn!("Close reconciliation handle: {error}");
        }
    }
}

fn created(handle: HANDLE) -> Result<u64, String> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
        .map_err(|e| format!("GetProcessTimes: {e}"))?;
    Ok(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
}

pub(super) fn identity(pid: u32) -> Result<Value, String> {
    let handle = Handle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| format!("OpenProcess(identity): {e}"))?);
    // Use a string: FILETIME ticks exceed JSON's interoperable integer range.
    Ok(json!({"pid":pid,"created_filetime":created(handle.0)?.to_string()}))
}

fn ended(identity: &Value) -> Result<Value, String> {
    let pid = identity["pid"].as_u64().and_then(|p| u32::try_from(p).ok()).filter(|p| *p > 0)
        .ok_or("Missing process identity PID")?;
    let expected = identity["created_filetime"].as_str().ok_or("Missing process creation identity")?
        .parse::<u64>().map_err(|_| "Invalid process creation identity")?;
    if expected == 0 { return Err("Invalid zero process creation identity".into()); }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, pid) } {
        Ok(handle) => Handle(handle),
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            return Ok(json!({"ended":true,"pid":pid,"reason":"pid_absent"}));
        }
        Err(error) => return Err(format!("OpenProcess(reconcile): {error}")),
    };
    let actual = created(handle.0)?;
    if actual != expected {
        return Ok(json!({"ended":true,"pid":pid,"reason":"pid_reused","expected_creation":expected.to_string(),"observed_creation":actual.to_string()}));
    }
    let wait = unsafe { WaitForSingleObject(handle.0, 0) };
    if wait == windows::Win32::Foundation::WAIT_FAILED {
        return Err(format!("WaitForSingleObject: {}", std::io::Error::last_os_error()));
    }
    Ok(json!({"ended":wait == WAIT_OBJECT_0,"pid":pid,"reason":"exact_process_identity"}))
}

fn tree(name: &str) -> Result<Value, String> {
    let suffix = name.strip_prefix("Global\\RustDeskMCP-").ok_or("Invalid recorded job object")?;
    uuid::Uuid::parse_str(suffix).map_err(|_| "Invalid recorded job object identity")?;
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = match unsafe { OpenJobObjectW(JOB_OBJECT_QUERY, false, PCWSTR(wide.as_ptr())) } {
        Ok(handle) => Handle(handle),
        Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
            return Ok(json!({"ended":true,"job_object":name,"reason":"object_absent"}));
        }
        Err(error) => return Err(format!("OpenJobObjectW: {error}")),
    };
    let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    unsafe { QueryInformationJobObject(Some(handle.0), JobObjectBasicAccountingInformation,
        &mut accounting as *mut _ as _, std::mem::size_of_val(&accounting) as u32, None) }
        .map_err(|e| format!("QueryInformationJobObject: {e}"))?;
    Ok(json!({"ended":accounting.ActiveProcesses == 0,"job_object":name,"active_processes":accounting.ActiveProcesses}))
}

pub(super) fn inspect(state: &Value) -> Result<Value, String> {
    if state["recovery_launch_pending"] != false || !state["worker_identity"].is_object()
        || !state["command_identity"].is_object() {
        return Ok(json!({"confirmed_ended":false,"reason":"Missing containment/identity evidence or interrupted launch; preserve legacy jobs for manual review"}));
    }
    let worker = ended(&state["worker_identity"])?;
    if worker["ended"] != true {
        return Ok(json!({"confirmed_ended":false,"worker":worker}));
    }
    let mut trees = Vec::new();
    for role in ["setup", "command"] {
        if let Some(name) = state[format!("{role}_job_object")].as_str() {
            let process = ended(&state[format!("{role}_identity")])?;
            let tree = tree(name)?;
            trees.push(json!({"role":role,"process":process,"tree":tree}));
        } else if role == "command" {
            return Err("Missing command job object; preserve for review".into());
        }
    }
    let confirmed = trees.iter().all(|t| t["process"]["ended"] == true && t["tree"]["ended"] == true);
    Ok(json!({"confirmed_ended":confirmed,"worker":worker,"trees":trees}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_live_and_reused_pid_does_not_identify_the_old_worker() {
        let mut current = identity(std::process::id()).unwrap();
        assert_eq!(ended(&current).unwrap()["ended"], false);
        current["created_filetime"] = json!("1");
        let observation = ended(&current).unwrap();
        assert_eq!(observation["ended"], true);
        assert_eq!(observation["reason"], "pid_reused");
        assert!(ended(&json!({"pid":std::process::id()})).is_err());
        assert_eq!(inspect(&json!({"worker_pid":std::process::id(),"updated_at_ms":1})).unwrap()["confirmed_ended"], false);
    }
}
