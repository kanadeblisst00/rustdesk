use super::super::store::{read_json, write_json};
use super::*;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("mcp-retention-test-{}", uuid::Uuid::new_v4())))
    }
    fn store(&self) -> Store {
        Store::new(self.0.join("jobs")).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if self.0.exists() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}
fn add(store: &Store, id: &str, state: &str, time: u64) {
    let dir = store.directory(id).unwrap();
    store::private_dir(&dir).unwrap();
    write_json(&dir.join("state.json"),&json!({"job_id":id,"state":state,"updated_at_ms":time,"finished_at_ms":time,"exit_code":0})).unwrap();
    write_json(
        &dir.join("request.json"),
        &json!({"job_id":id,"executable":"original"}),
    )
    .unwrap();
    fs::write(dir.join("stdout.log"), b"diagnostics\x00\xff").unwrap();
}

#[test]
fn capacity_distinguishes_record_pressure_from_active_and_unknown() {
    let jobs: Vec<_> = (0..256)
        .map(|i| json!({"state":if i<6 {"unknown"} else {"exited"}}))
        .collect();
    let c = capacity(&jobs);
    assert_eq!(c["blocked_by"], "retained");
    assert_eq!(c["active"], 0);
    assert_eq!(c["unknown"], 6);
    assert_eq!(c["completed"], 250);
    assert_eq!(c["available_active"], 10);
    assert_eq!(
        capacity(&vec![json!({"state":"unknown"}); 16])["blocked_by"],
        "active_or_unknown"
    );
}

#[test]
fn archival_preserves_bytes_unknown_jobs_and_archived_request_idempotency() {
    let temp = Temp::new();
    let store = temp.store();
    let old = store::now() - MIN_AGE_MS - 1000;
    for i in 0..224 {
        add(
            &store,
            &format!("job-{i:03}"),
            if i < 6 { "running" } else { "exited" },
            old,
        );
    }
    let unknown = fs::read(store.root.join("job-000/state.json")).unwrap();
    let jobs = store.list().unwrap();
    assert_eq!(
        reclaim(&store, jobs["jobs"].as_array().unwrap()).unwrap(),
        32
    );
    assert_eq!(store.list().unwrap()["jobs"].as_array().unwrap().len(), 192);
    assert_eq!(
        fs::read(store.root.join("job-000/state.json")).unwrap(),
        unknown
    );
    let archived = store.status("job-006").unwrap();
    assert_eq!(archived["archived"], true);
    let path = store.directory("job-006").unwrap();
    assert_eq!(
        fs::read(path.join("stdout.log")).unwrap(),
        b"diagnostics\x00\xff"
    );
    let manifest = read_json(
        &archive_root(&store.root)
            .unwrap()
            .join("job-006.manifest.json"),
    )
    .unwrap();
    assert_eq!(manifest["files"].as_array().unwrap().len(), 3);
    let mut files = Vec::new();
    snapshot(&path, &path, &mut files).unwrap();
    assert_eq!(manifest["files"], json!(files));
    let request = json!({"session":"new-session","job_id":"job-006","executable":"original"});
    assert_eq!(
        store
            .create(&request, |_| panic!("archived job must not be relaunched"))
            .unwrap()["archived"],
        true
    );
    assert!(store
        .create(
            &json!({"job_id":"job-006","executable":"changed"}),
            |_| panic!("must not relaunch")
        )
        .is_err());
    use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
    let output = store
        .call(
            "read_process_output",
            &json!({"job_id":"job-006","stream":"stdout"}),
        )
        .unwrap();
    assert_eq!(
        STANDARD
            .decode(output["data_base64"].as_str().unwrap())
            .unwrap(),
        b"diagnostics\x00\xff"
    );
}

#[test]
fn recent_completed_jobs_are_not_archived_and_capacity_error_is_precise() {
    let temp = Temp::new();
    let store = temp.store();
    for i in 0..256 {
        add(
            &store,
            &format!("job-{i:03}"),
            if i < 6 { "running" } else { "exited" },
            store::now(),
        );
    }
    let error = store
        .create(&json!({"job_id":"new"}), |_| {
            panic!("full store must not launch")
        })
        .unwrap_err();
    assert!(error.contains("\"retained\":256"));
    assert!(error.contains("\"completed\":250"));
    assert!(error.contains("\"active\":6"));
    assert!(error.contains("\"blocked_by\":\"retained\""));
    assert_eq!(store.list().unwrap()["jobs"].as_array().unwrap().len(), 256);
}

#[test]
fn recovery_lock_and_existing_archive_are_never_overwritten() {
    let temp = Temp::new();
    let store = temp.store();
    for i in 0..224 {
        add(
            &store,
            &format!("job-{i:03}"),
            "failed",
            store::now() - MIN_AGE_MS - 1,
        );
    }
    let archive = archive_root(&store.root).unwrap();
    store::private_dir(&archive).unwrap();
    let lock = archive.parent().unwrap().join("retention.lock");
    fs::write(&lock, "other-owner").unwrap();
    let jobs = store.list().unwrap();
    assert!(reclaim(&store, jobs["jobs"].as_array().unwrap())
        .unwrap_err()
        .contains("lock"));
    assert_eq!(fs::read_to_string(&lock).unwrap(), "other-owner");
    fs::remove_file(lock).unwrap();
    fs::create_dir(archive.join("job-000")).unwrap();
    assert!(reclaim(&store, jobs["jobs"].as_array().unwrap())
        .unwrap_err()
        .contains("already exists"));
    assert!(store.root.join("job-000").exists());
    assert!(archive.join("job-000").exists());
}

#[test]
fn a_replaced_lock_is_not_released_by_the_previous_owner() {
    let temp = Temp::new();
    let store = temp.store();
    let path = store.root.join("lock");
    fs::write(&path, "new-owner").unwrap();
    assert!(release_lock(&path, "old-owner")
        .unwrap_err()
        .contains("ownership changed"));
    assert_eq!(fs::read_to_string(path).unwrap(), "new-owner");
}

#[test]
fn full_history_is_archived_before_admitting_one_new_job() {
    let temp = Temp::new();
    let store = temp.store();
    for i in 0..256 {
        add(
            &store,
            &format!("job-{i:03}"),
            if i < 6 { "running" } else { "exited" },
            store::now() - MIN_AGE_MS - 1,
        );
    }
    let result = store
        .create(&json!({"job_id":"new-job","executable":"original"}), |_| {
            Ok(())
        })
        .unwrap();
    assert_eq!(result["state"], "starting");
    let list = store.list().unwrap();
    assert_eq!(list["capacity"]["retained"], 193);
    assert_eq!(list["capacity"]["unknown"], 6);
    assert_eq!(list["capacity"]["active"], 1);
    assert_eq!(list["capacity"]["completed"], 186);
    assert_eq!(
        fs::read_dir(archive_root(&store.root).unwrap())
            .unwrap()
            .count(),
        128
    );
}

#[cfg(unix)]
#[test]
fn linked_diagnostics_are_preserved_for_review() {
    let temp = Temp::new();
    let store = temp.store();
    for i in 0..224 {
        add(
            &store,
            &format!("job-{i:03}"),
            "failed",
            store::now() - MIN_AGE_MS - 1,
        );
    }
    std::os::unix::fs::symlink("/etc/hosts", store.root.join("job-000/linked")).unwrap();
    let jobs = store.list().unwrap();
    assert!(reclaim(&store, jobs["jobs"].as_array().unwrap())
        .unwrap_err()
        .contains("links"));
    assert!(store.root.join("job-000/linked").symlink_metadata().is_ok());
}
