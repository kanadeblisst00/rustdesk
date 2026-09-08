use super::*;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("mcp-job-test-{}", uuid::Uuid::new_v4())))
    }
    fn store(&self) -> store::Store {
        store::Store::new(self.0.clone()).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn spec(id: &str) -> Value {
    json!({"session":"test","job_id":id,"executable":"test-command","cwd":std::env::temp_dir()})
}

#[test]
fn durable_idempotency_and_conflicting_retries() {
    let temp = Temp::new();
    let store = temp.store();
    let first = store.create(&spec("build-a"), |_| Ok(())).unwrap();
    assert_eq!(first["state"], "starting");
    let restored = temp.store();
    restored
        .create(&spec("build-a"), |_| panic!("must not launch twice"))
        .unwrap();
    let mut conflicting = spec("build-a");
    conflicting["args"] = json!(["changed"]);
    assert!(restored.create(&conflicting, |_| panic!()).is_err());
    assert_eq!(
        restored.list().unwrap()["jobs"].as_array().unwrap().len(),
        1
    );
    assert!(restored.directory("../escape").is_err());
    assert!(restored
        .call("remove_process", &json!({"job_id":"build-a"}))
        .is_err());
    assert!(restored
        .call("cancel_process", &json!({"job_id":"build-a"}))
        .unwrap()["cancel_requested"]
        .as_bool()
        .unwrap());
    let mut stale = first;
    stale["updated_at_ms"] = json!(0);
    store::write_json(
        &store.directory("build-a").unwrap().join("state.json"),
        &stale,
    )
    .unwrap();
    assert_eq!(restored.status("build-a").unwrap()["state"], "unknown");
}

#[test]
fn validates_paths_nul_and_environment() {
    for id in ["../x", "x/y", "x\\y", "点", ".", ""] {
        assert!(model::validate("run_process", &spec(id)).is_err());
    }
    let mut request = spec("valid");
    request["args"] = json!(["a\0b"]);
    assert!(model::validate("run_process", &request).is_err());
    request["args"] = json!(["$(not-a-shell)"]);
    request["env"] = json!([{"name":"Path","value":"one"},{"name":"PATH","value":"two"}]);
    assert!(model::validate("run_process", &request).is_err());
}

#[tokio::test]
async fn rejects_non_terminal_authorization_before_execution() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let request = encode(&json!({"protocol":model::WIRE_VERSION,"id":"test","operation":"run_process","arguments":spec("unauthorized")})).unwrap();
    assert!(dispatch(&request, false, None, &Some(tx)));
    let (_, reply) = rx.recv().await.unwrap();
    assert!(decode(&reply).unwrap().unwrap()["error"]
        .as_str()
        .unwrap()
        .contains("authorized terminal"));
    let mut mixed = request;
    mixed.set_key_event(Default::default());
    assert!(!is_message(&mixed));
    assert!(decode(&mixed).unwrap().is_err());
}

#[cfg(unix)]
fn shell_spec(id: &str, script: &str) -> Value {
    let mut request = spec(id);
    request["executable"] = json!("/bin/sh");
    request["args"] = json!(["-c", script]);
    request
}

#[test]
#[cfg(unix)]
fn separates_streams_preserves_exit_code_and_removes_only_completed_jobs() {
    let temp = Temp::new();
    let store = temp.store();
    let mut request = shell_spec(
        "build",
        "printf '%s' \"$MCP_TEST_VALUE\"; printf 'failure' >&2; exit 7",
    );
    request["env"] = json!([{"name":"MCP_TEST_VALUE","value":"中文 $(literal)"}]);
    store.create(&request, |_| Ok(())).unwrap();
    worker::run(&store.directory("build").unwrap()).unwrap();
    let restarted = temp.store();
    let state = restarted.status("build").unwrap();
    assert_eq!(state["state"], "exited");
    assert_eq!(state["exit_code"], 7);
    assert_eq!(state["success"], false);
    assert_eq!(
        restarted
            .call(
                "read_process_output",
                &json!({"job_id":"build","stream":"stdout"})
            )
            .unwrap()["text"],
        "中文 $(literal)"
    );
    let err = restarted
        .call(
            "read_process_output",
            &json!({"job_id":"build","stream":"stderr","offset":2,"max_bytes":3}),
        )
        .unwrap();
    assert_eq!(err["text"], "ilu");
    assert_eq!(err["next_offset"], 5);
    assert_eq!(err["eof"], false);
    assert!(worker::run(&store.directory("build").unwrap()).is_err());
    restarted
        .call("remove_process", &json!({"job_id":"build"}))
        .unwrap();
    assert!(restarted.status("build").is_err());
}

#[test]
#[cfg(unix)]
fn timeout_cancel_and_log_limit_are_distinct() {
    for (id, script, limit, timeout, cancel, expected) in [
        ("timeout", "sleep 30", 1024, 100, false, "timed_out"),
        ("cancel", "sleep 30", 1024, 30000, true, "cancelled"),
        (
            "limit",
            "while true; do printf '0123456789'; done",
            1024,
            30000,
            false,
            "log_limit",
        ),
    ] {
        let temp = Temp::new();
        let store = temp.store();
        let mut request = shell_spec(id, script);
        request["timeout_ms"] = json!(timeout);
        request["max_log_bytes"] = json!(limit);
        store.create(&request, |_| Ok(())).unwrap();
        let dir = store.directory(id).unwrap();
        let worker = std::thread::spawn(move || worker::run(&dir));
        let started = Instant::now();
        if cancel {
            while store.status(id).unwrap()["state"] == "starting" {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(10));
            }
            store.call("cancel_process", &json!({"job_id":id})).unwrap();
        }
        worker.join().unwrap().unwrap();
        let state = store.status(id).unwrap();
        assert_eq!(state["state"], expected, "{state}");
        assert!(state["stdout_bytes"].as_u64().unwrap() <= limit);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[test]
#[cfg(unix)]
fn cancelling_kills_grandchildren_before_they_write() {
    let temp = Temp::new();
    let store = temp.store();
    let request = shell_spec("tree", "(sleep 1; echo leaked > marker) & wait");
    let mut request = request;
    request["cwd"] = json!(temp.0);
    request["timeout_ms"] = json!(100);
    store.create(&request, |_| Ok(())).unwrap();
    worker::run(&store.directory("tree").unwrap()).unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!temp.0.join("marker").exists());
}

#[test]
fn workspace_seals_sources_and_rejects_conflicts_and_path_escapes() {
    let temp = Temp::new();
    let store = temp.store();
    let request = json!({"workspace_id":"project","source_revision":"commit-123"});
    let created = workspace::call(&store, "create_workspace", &request, |_| panic!()).unwrap();
    let source = PathBuf::from(created["paths"]["source"].as_str().unwrap());
    std::fs::write(source.join("main.txt"), "source version one").unwrap();
    let sealed = workspace::call(&store, "seal_workspace", &request, |_| panic!()).unwrap();
    assert_eq!(sealed["source"]["entries"], 1);
    assert_eq!(store.list().unwrap()["jobs"].as_array().unwrap().len(), 0);
    assert!(workspace::call(
        &store,
        "create_workspace",
        &json!({"workspace_id":"project","source_revision":"other"}),
        |_| panic!()
    )
    .is_err());
    let run = json!({"workspace_id":"project","job_id":"build","session":"test","executable":"test","cwd":"../escape"});
    assert!(workspace::call(&store, "run_workspace_process", &run, |_| panic!()).is_err());
    let mut run = run;
    run["cwd"] = json!("build");
    std::fs::write(source.join("main.txt"), "changed").unwrap();
    assert!(workspace::call(&store, "run_workspace_process", &run, |_| panic!()).is_err());
    assert!(!store.directory("build").unwrap().exists());
    workspace::call(&store, "seal_workspace", &request, |_| panic!()).unwrap();
    workspace::call(&store, "run_workspace_process", &run, |_| Ok(())).unwrap();
    assert!(workspace::call(&store, "seal_workspace", &request, |_| panic!()).is_err());
    assert!(workspace::call(&store, "remove_workspace", &request, |_| panic!()).is_err());
    let mut second = run.clone();
    second["job_id"] = json!("second");
    assert!(workspace::call(&store, "run_workspace_process", &second, |_| panic!()).is_err());
    assert!(!store.directory("second").unwrap().exists());
    workspace::call(&store, "run_workspace_process", &run, |_| {
        panic!("idempotent retry launched")
    })
    .unwrap();
}

#[test]
#[cfg(unix)]
fn workspace_commands_release_lease_and_artifacts_are_bound_to_latest_job() {
    use sha2::{Digest, Sha256};
    let temp = Temp::new();
    let store = temp.store();
    let workspace_args = json!({"workspace_id":"project","source_revision":"commit-123"});
    let created =
        workspace::call(&store, "create_workspace", &workspace_args, |_| panic!()).unwrap();
    let source = PathBuf::from(created["paths"]["source"].as_str().unwrap());
    std::fs::write(source.join("main.txt"), "source").unwrap();
    workspace::call(&store, "seal_workspace", &workspace_args, |_| panic!()).unwrap();
    let mut run = shell_spec("build", "printf artifact > ../artifacts/app.bin");
    run["workspace_id"] = json!("project");
    run["cwd"] = json!("build");
    workspace::call(&store, "run_workspace_process", &run, |_| Ok(())).unwrap();
    worker::run(&store.directory("build").unwrap()).unwrap();
    assert_eq!(store.status("build").unwrap()["source_unchanged"], true);
    let args = json!({"workspace_id":"project","job_id":"build","paths":["artifacts/app.bin"]});
    let manifest = workspace::call(&store, "get_artifact_manifest", &args, |_| panic!()).unwrap();
    assert_eq!(
        manifest["files"][0]["sha256"],
        format!("{:x}", Sha256::digest(b"artifact"))
    );
    let mut escaped = args.clone();
    escaped["paths"] = json!(["artifacts/../../outside"]);
    assert!(workspace::call(&store, "get_artifact_manifest", &escaped, |_| panic!()).is_err());
    std::os::unix::fs::symlink("/etc/passwd", source.join("link")).unwrap();
    assert!(workspace::call(&store, "seal_workspace", &workspace_args, |_| panic!()).is_err());
    std::fs::remove_file(source.join("link")).unwrap();
    run["job_id"] = json!("second");
    run["args"] = json!(["-c", "printf changed > ../source/main.txt"]);
    workspace::call(&store, "run_workspace_process", &run, |_| Ok(())).unwrap();
    worker::run(&store.directory("second").unwrap()).unwrap();
    assert_eq!(store.status("second").unwrap()["source_unchanged"], false);
    assert!(workspace::call(&store, "get_artifact_manifest", &args, |_| panic!()).is_err());
    workspace::call(&store, "remove_workspace", &workspace_args, |_| panic!()).unwrap();
    assert_eq!(store.status("build").unwrap()["state"], "exited");
}
