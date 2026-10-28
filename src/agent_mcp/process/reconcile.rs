use super::store::{self, Store};
use serde_json::{json, Value};
use std::{fs::OpenOptions, path::Path};

#[cfg(windows)]
#[path = "reconcile_windows.rs"]
mod windows;

pub(super) fn worker_identity(state: &mut Value) -> Result<(), String> {
    #[cfg(windows)]
    { state["worker_identity"] = windows::identity(std::process::id())?; }
    let _ = state;
    Ok(())
}

pub(super) fn spawn(command: &mut std::process::Command, dir: &Path, state: &mut Value,
    role: &str) -> Result<super::platform::Child, super::diagnostics::Failure> {
    #[cfg(not(windows))]
    {
        let _ = (dir, state, role);
        super::platform::Child::spawn(command)
    }
    #[cfg(windows)]
    {
        let name = format!("Global\\RustDeskMCP-{}", uuid::Uuid::new_v4());
        state["recovery_launch_pending"] = json!(true);
        store::write_json_detailed(&dir.join("state.json"), state)?;
        super::recovery_record::checkpoint(dir, state)
            .map_err(|e| super::diagnostics::Failure::message("persist_recovery_pending", e))?;
        let mut child = super::platform::Child::spawn_named(command, &name)?;
        state[format!("{role}_job_object")] = json!(name);
        match windows::identity(child.process.id()) {
            Ok(identity) => {
                state[format!("{role}_identity")] = identity;
                state["recovery_launch_pending"] = json!(false);
            }
            Err(error) => {
                child.stop().map_err(|e| super::diagnostics::Failure::message("stop_unidentified_process_tree", e))?;
                return Err(super::diagnostics::Failure::message("read_command_identity", error));
            }
        }
        if let Err(error) = store::write_json_detailed(&dir.join("state.json"), state) {
            error.warn(state, "recovery_write");
        }
        if let Err(error) = super::recovery_record::checkpoint(dir, state) {
            child.stop().map_err(|e| super::diagnostics::Failure::message("stop_unrecorded_process_tree", e))?;
            return Err(super::diagnostics::Failure::message("persist_recovery_identity", error));
        }
        Ok(child)
    }
}

pub(super) fn call(store: &Store, args: &Value) -> Result<Value, String> {
    let id = args["job_id"].as_str().ok_or("Missing job_id")?;
    let dir = store.directory(id)?;
    let lock = super::recovery_record::lock_path(&dir)?;
    let owner = uuid::Uuid::new_v4().to_string();
    let mut file = OpenOptions::new().write(true).create_new(true).open(&lock)
        .map_err(|e| format!("Reconciliation lock unavailable; preserve for review: {e}"))?;
    use std::io::Write;
    file.write_all(owner.as_bytes()).map_err(|e| e.to_string())?;
    let result = reconcile(store, id, inspect);
    drop(file);
    if std::fs::read_to_string(&lock).map_err(|e| e.to_string())? != owner {
        return Err("Reconciliation lock ownership changed; preserved".into());
    }
    std::fs::remove_file(lock).map_err(|e| e.to_string())?;
    result
}

fn inspect(state: &Value) -> Result<Value, String> {
    #[cfg(windows)]
    { windows::inspect(state) }
    #[cfg(not(windows))]
    { let _ = state; Ok(json!({"confirmed_ended":false,"reason":"Process-tree reconciliation is only available for Windows jobs with recorded identities and named job objects"})) }
}

fn reconcile(store: &Store, id: &str, inspect: impl Fn(&Value) -> Result<Value, String>) -> Result<Value, String> {
    let state = store.status(id)?;
    if store::terminal(&state) || state["state"] != "unknown" {
        return Ok(json!({"job":state,"reconciled":false,"reason":"Job is terminal or its worker heartbeat is current"}));
    }
    let dir = store.directory(id)?;
    let before = super::recovery_record::read_state(&dir)?;
    let evidence = inspect(&before)?;
    if evidence["confirmed_ended"] != true {
        return Ok(json!({"job":state,"reconciled":false,"evidence":evidence}));
    }
    if super::recovery_record::read_state(&dir)? != before || dir.join("result.json").exists() {
        return Err("Job changed during reconciliation; query the SAME job_id again".into());
    }
    let mut result = before.clone();
    result["state"] = json!("failed");
    result["failure_stage"] = json!("worker_lost");
    result["execution_outcome"] = json!("unknown");
    result["success"] = Value::Null;
    result["finished_at_ms"] = json!(store::now());
    result["updated_at_ms"] = json!(store::now());
    result["error"] = json!("Worker and contained process trees ended without a final result; command success is unknown. No process was replayed or killed by reconciliation.");
    result["reconciliation"] = json!({"state":"interrupted","evidence":evidence,"previous_state":before["state"],
        "original_record":"state.json","request_replayed":false,"processes_terminated":false});
    // Keep the original anomalous state and logs. A missing exit code remains missing.
    super::recovery_record::write_result(&dir, &result)?;
    if let Err(error) = super::workspace::release(&dir) {
        result["workspace_release_error"] = json!(error);
        super::recovery_record::write_result(&dir, &result)?;
    }
    Ok(json!({"job":store.status(id)?,"reconciled":true,"active_slot_released":true}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_time_alone_never_releases_capacity_and_recovery_preserves_evidence() {
        let root = std::env::temp_dir().join(format!("reconcile-{}", uuid::Uuid::new_v4()));
        let store = Store::new(root.clone()).unwrap();
        let dir = root.join("job");
        store::private_dir(&dir).unwrap();
        let original = json!({"job_id":"job","state":"running","updated_at_ms":1,"exit_code":null});
        store::write_json(&dir.join("state.json"), &original).unwrap();
        store::write_json(&dir.join("request.json"), &json!({"job_id":"job"})).unwrap();
        std::fs::write(dir.join("stderr.log"), "last error").unwrap();
        let uncertain = reconcile(&store, "job", |_| Ok(json!({"confirmed_ended":false}))).unwrap();
        assert_eq!(uncertain["reconciled"], false);
        assert_eq!(store.list().unwrap()["capacity"]["unknown"], 1);
        let recovered = reconcile(&store, "job", |_| Ok(json!({"confirmed_ended":true}))).unwrap();
        assert_eq!(recovered["job"]["state"], "failed");
        assert_eq!(recovered["job"]["execution_outcome"], "unknown");
        assert!(recovered["job"]["exit_code"].is_null());
        assert!(recovered["job"]["success"].is_null());
        assert_eq!(store::read_json(&dir.join("state.json")).unwrap(), original);
        assert_eq!(std::fs::read_to_string(dir.join("stderr.log")).unwrap(), "last error");
        assert_eq!(store.list().unwrap()["capacity"]["available_active"], 16);
        assert_eq!(store.create(&json!({"job_id":"job"}), |_| panic!("must not replay reconciled job")).unwrap()["state"], "failed");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_worker_update_prevents_reconciliation() {
        let root = std::env::temp_dir().join(format!("reconcile-race-{}", uuid::Uuid::new_v4()));
        let store = Store::new(root.clone()).unwrap();
        let dir = root.join("job");
        store::private_dir(&dir).unwrap();
        store::write_json(&dir.join("state.json"), &json!({"job_id":"job","state":"running","updated_at_ms":1})).unwrap();
        assert!(reconcile(&store, "job", |_| {
            store::write_json(&dir.join("state.json"), &json!({"job_id":"job","state":"running","updated_at_ms":store::now()}))?;
            Ok(json!({"confirmed_ended":true}))
        }).is_err());
        assert!(!dir.join("result.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
