use super::store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};

fn location(dir: &Path) -> Result<(PathBuf, String), String> {
    let id = dir.file_name().and_then(|v| v.to_str()).ok_or("Missing recovery job ID")?;
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("Invalid recovery job ID".into());
    }
    let root = dir.parent().ok_or("Missing recovery store")?.join(".recovery");
    Ok((root, id.to_owned()))
}

fn request_hash(dir: &Path) -> Result<String, String> {
    let path = dir.join("request.json");
    let meta = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
        return Err("Invalid original recovery request".into());
    }
    Ok(format!("{:x}", Sha256::digest(fs::read(path).map_err(|e| e.to_string())?)))
}

pub(super) fn exists(dir: &Path) -> Result<bool, String> {
    let (root, id) = location(dir)?;
    root.join(format!("{id}.json")).try_exists().map_err(|e| e.to_string())
}

pub(super) fn checkpoint(dir: &Path, state: &Value) -> Result<(), String> {
    let (root, id) = location(dir)?;
    if state["job_id"] != id { return Err("Recovery job identity mismatch".into()); }
    let hash = request_hash(dir)?;
    store::private_dir(&root)?;
    let path = root.join(format!("{id}.json"));
    if path.try_exists().map_err(|e| e.to_string())? {
        let prior = store::read_json(&path)?;
        if prior["job_id"] != id || prior["request_sha256"] != hash {
            return Err("Recovery request binding changed; preserve original evidence".into());
        }
    }
    // A separate directory survives deletion of the per-job directory. Never
    // copy executable arguments, environment values or output into this record.
    let mut evidence = json!({});
    for field in ["job_id", "state", "created_at_ms", "updated_at_ms", "started_at_ms",
        "finished_at_ms", "worker_pid", "worker_identity", "command_identity", "setup_identity",
        "command_job_object", "setup_job_object", "recovery_launch_pending", "exit_code", "success",
        "termination_requested", "termination_reason", "process_tree_cleanup", "failure_stage", "execution_outcome"] {
        if let Some(value) = state.get(field) { evidence[field] = value.clone(); }
    }
    store::write_json(&path, &json!({"version":1,"job_id":id,"request_sha256":hash,"state":evidence}))
}

fn snapshot(dir: &Path) -> Result<Value, String> {
    let (root, id) = location(dir)?;
    if !root.try_exists().map_err(|e| e.to_string())? { return Err("No independent recovery evidence; preserve unknown job".into()); }
    store::private_dir(&root)?;
    let value = store::read_json(&root.join(format!("{id}.json")))?;
    let hash = value["request_sha256"].as_str().ok_or("Missing recovery request binding")?;
    if value["version"] != 1 || value["job_id"] != id || value["state"]["job_id"] != id
        || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid recovery evidence binding".into());
    }
    if dir.join("request.json").try_exists().map_err(|e| e.to_string())? && request_hash(dir)? != hash {
        return Err("Original request differs from recovery evidence".into());
    }
    Ok(value)
}

pub(super) fn read_state(dir: &Path) -> Result<Value, String> {
    let path = dir.join("state.json");
    if path.try_exists().map_err(|e| e.to_string())? { return store::read_json(&path); }
    let record = snapshot(dir)?;
    let (root, id) = location(dir)?;
    let result = root.join(format!("{id}.result.json"));
    let mut state = if result.try_exists().map_err(|e| e.to_string())? {
        let value = store::read_json(&result)?;
        if !store::terminal(&value) || value["job_id"] != id || value["recovery_request_sha256"] != record["request_sha256"] {
            return Err("Invalid recovered final record".into());
        }
        value
    } else { record["state"].clone() };
    state["recovery_record_used"] = json!(true);
    state["recovery_request_sha256"] = record["request_sha256"].clone();
    if !store::terminal(&state) {
        state["state"] = json!("unknown");
        state["success"] = Value::Null;
        state["exit_code"] = Value::Null;
    }
    Ok(state)
}

pub(super) fn lock_path(dir: &Path) -> Result<PathBuf, String> {
    if dir.is_dir() { return Ok(dir.join("reconcile.lock")); }
    snapshot(dir)?;
    let (root, id) = location(dir)?;
    Ok(root.join(format!("{id}.lock")))
}

pub(super) fn write_result(dir: &Path, value: &Value) -> Result<(), String> {
    if dir.is_dir() {
        store::write_json(&dir.join("result.json"), value)?;
        if exists(dir)? { checkpoint(dir, value)?; }
        return Ok(());
    }
    let record = snapshot(dir)?;
    if value["recovery_request_sha256"] != record["request_sha256"] {
        return Err("Recovered result request binding mismatch".into());
    }
    let (root, id) = location(dir)?;
    store::write_json(&root.join(format!("{id}.result.json")), value)
}

pub(super) fn missing_jobs(root: &Path) -> Result<Vec<String>, String> {
    let backup = root.join(".recovery");
    if !backup.try_exists().map_err(|e| e.to_string())? { return Ok(Vec::new()); }
    store::private_dir(&backup)?;
    let mut ids = Vec::new();
    for entry in fs::read_dir(backup).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".json").filter(|id| !id.contains('.')) {
            let dir = root.join(id);
            if !dir.try_exists().map_err(|e| e.to_string())? {
                let state = read_state(&dir)?;
                if !store::terminal(&state) { ids.push(id.to_owned()); }
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deleted_job_keeps_identity_capacity_and_cannot_be_replayed() {
        let root = std::env::temp_dir().join(format!("recovery-record-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(root.clone()).unwrap();
        let args = json!({"job_id":"job","args":["secret-argument"],"env":[{"name":"TOKEN","value":"private-value"}]});
        let mut state = store.create(&args, |_| Ok(())).unwrap();
        let dir = root.join("job");
        state["worker_identity"] = json!({"pid":123,"created_filetime":"456"});
        state["recovery_launch_pending"] = json!(true);
        checkpoint(&dir, &state).unwrap();
        let bytes = fs::read_to_string(root.join(".recovery/job.json")).unwrap();
        assert!(!bytes.contains("secret-argument") && !bytes.contains("private-value"));
        fs::remove_dir_all(&dir).unwrap();
        let recovered = store.status("job").unwrap();
        assert_eq!(recovered["state"], "unknown");
        assert_eq!(recovered["worker_identity"], state["worker_identity"]);
        assert_eq!(recovered["recovery_launch_pending"], true);
        assert!(recovered["exit_code"].is_null() && recovered["success"].is_null());
        assert_eq!(store.list().unwrap()["capacity"]["unknown"], 1);
        assert!(store.create(&args, |_| panic!("must not replay missing original job")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reconciled_unknown_outcome_survives_later_job_directory_removal() {
        let root = std::env::temp_dir().join(format!("recovery-final-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(root.clone()).unwrap();
        let mut state = store.create(&json!({"job_id":"job"}), |_| Ok(())).unwrap();
        let dir = root.join("job");
        checkpoint(&dir, &state).unwrap();
        state["state"] = json!("failed");
        state["execution_outcome"] = json!("unknown");
        write_result(&dir, &state).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        let recovered = store.status("job").unwrap();
        assert_eq!(recovered["state"], "failed");
        assert_eq!(recovered["execution_outcome"], "unknown");
        assert!(recovered["exit_code"].is_null() && recovered["success"].is_null());
        assert_eq!(store.list().unwrap()["capacity"]["unknown"], 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_request_or_mismatched_job_is_never_accepted() {
        let root = std::env::temp_dir().join(format!("recovery-binding-{}", uuid::Uuid::new_v4()));
        let store = store::Store::new(root.clone()).unwrap();
        let state = store.create(&json!({"job_id":"job"}), |_| Ok(())).unwrap();
        let dir = root.join("job");
        checkpoint(&dir, &state).unwrap();
        fs::remove_file(dir.join("state.json")).unwrap();
        store::write_json(&dir.join("request.json"), &json!({"job_id":"other"})).unwrap();
        assert!(store.status("job").is_err());
        assert!(checkpoint(&dir, &json!({"job_id":"other"})).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
