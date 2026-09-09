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
    request["env"] = json!([{"name":"HTTP_PROXY","value":""},{"name":"HTTPS_PROXY","value":""}]);
    assert!(model::validate("run_process", &request).is_ok());
    request["unset_env"] = json!(["http_proxy"]);
    assert!(model::validate("run_process", &request).is_err());
    request["unset_env"] = json!(["ALL_PROXY"]);
    assert!(model::validate("run_process", &request).is_ok());
    request["shell"] = json!("cmd");
    assert!(model::validate("run_process", &request).is_err());
    request["executable"] = json!("cmd.exe");
    request["args"] = json!(["call \"C:\\Program Files\\build.cmd\""]);
    assert!(model::validate("run_process", &request).is_ok());
    request["args"] = json!(["/c", "echo wrong wrapper"]);
    assert!(model::validate("run_process", &request).is_err());
    assert!(model::validate(
        "read_process_output",
        &json!({"session":"test","job_id":"valid","stream":"stdout","offset":0,"tail_lines":2})
    )
    .is_err());
}

#[test]
#[cfg(unix)]
fn empty_environment_values_and_unset_are_distinct_in_real_commands() {
    let temp = Temp::new();
    let store = temp.store();
    let mut request = shell_spec(
        "env",
        "printf '%s:%s' \"${MCP_EMPTY_VALUE+x}\" \"$MCP_EMPTY_VALUE\"; /usr/bin/env",
    );
    request["env"] = json!([{"name":"MCP_EMPTY_VALUE","value":""}]);
    request["unset_env"] = json!(["HOME"]);
    model::validate("run_process", &request).unwrap();
    store.create(&request, |_| Ok(())).unwrap();
    worker::run(&store.directory("env").unwrap()).unwrap();
    let state = store.status("env").unwrap();
    assert_eq!(state["success"], true);
    assert_eq!(state["timeout_ms"], 3_600_000);
    assert_eq!(state["remaining_timeout_ms"], 0);
    assert!(state["log_paths"]["stdout"].is_string());
    let result = store
        .call(
            "read_process_output",
            &json!({"job_id":"env","stream":"stdout"}),
        )
        .unwrap();
    let text = result["text"].as_str().unwrap();
    assert!(text.starts_with("x:"));
    assert!(!text.lines().any(|line| line.starts_with("HOME=")));
}

#[test]
fn raw_log_bytes_survive_failed_decoding_and_offset_reads() {
    let temp = Temp::new();
    let store = temp.store();
    store.create(&spec("binary"), |_| Ok(())).unwrap();
    std::fs::write(
        store.directory("binary").unwrap().join("stdout.log"),
        [0xcf, 0xb5, 0xcd, 0xb3],
    )
    .unwrap();
    let first = store
        .call(
            "read_process_output",
            &json!({"job_id":"binary","stream":"stdout","max_bytes":1,"encoding":"utf-8"}),
        )
        .unwrap();
    assert!(first["text"].is_null());
    assert_eq!(first["data_base64"], "zw==");
    assert_eq!(first["next_offset"], 1);
    let rest = store
        .call(
            "read_process_output",
            &json!({"job_id":"binary","stream":"stdout","offset":1,"encoding":"base64"}),
        )
        .unwrap();
    assert_eq!(rest["data_base64"], "tc2z");
    assert_eq!(rest["next_offset"], 4);
    std::fs::write(
        store.directory("binary").unwrap().join("stdout.log"),
        b"first\nsecond\nthird\n",
    )
    .unwrap();
    let tail = store
        .call(
            "read_process_output",
            &json!({"job_id":"binary","stream":"stdout","tail_lines":2}),
        )
        .unwrap();
    assert_eq!(tail["text"], "second\nthird\n");
    assert_eq!(tail["offset"], 6);
    assert_eq!(tail["next_offset"], 19);
    let bounded = store
        .call(
            "read_process_output",
            &json!({"job_id":"binary","stream":"stdout","tail_lines":2,"max_bytes":3}),
        )
        .unwrap();
    assert_eq!(bounded["text"], "rd\n");
    assert_eq!(bounded["truncated_start"], true);
}

#[test]
#[cfg(unix)]
fn timeout_extension_is_durable_and_does_not_resume_completed_jobs() {
    let temp = Temp::new();
    let store = temp.store();
    let mut request = shell_spec("extend", "sleep 0.3; printf completed");
    request["timeout_ms"] = json!(100);
    store.create(&request, |_| Ok(())).unwrap();
    let restored = temp.store();
    let update = json!({"job_id":"extend","timeout_ms":2000});
    assert_eq!(
        restored.call("extend_process_timeout", &update).unwrap()["requested_timeout_ms"],
        2000
    );
    assert!(restored
        .call(
            "extend_process_timeout",
            &json!({"job_id":"extend","timeout_ms":500})
        )
        .is_err());
    worker::run(&restored.directory("extend").unwrap()).unwrap();
    let state = restored.status("extend").unwrap();
    assert_eq!(state["success"], true);
    assert_eq!(state["timeout_ms"], 2000);
    assert!(restored.call("extend_process_timeout", &update).is_err());
}

#[test]
fn workspace_bytes_upload_resume_checksum_download_and_reseal() {
    use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
    use sha2::{Digest, Sha256};
    let temp = Temp::new();
    let store = temp.store();
    let request = json!({"session":"test","workspace_id":"bytes","source_revision":"test"});
    let workspace = workspace::call(&store, "create_workspace", &request, |_| panic!()).unwrap();
    let root = PathBuf::from(workspace["paths"]["root"].as_str().unwrap());
    let data: Vec<_> = (0..40000).map(|i| (i % 256) as u8).collect();
    let hash = format!("{:x}", Sha256::digest(&data));
    let mut upload = json!({"session":"test","workspace_id":"bytes","path":"source/nested/input.bin","offset":0,"total_bytes":data.len(),"sha256":hash,"data_base64":STANDARD.encode(&data[..16384])});
    model::validate("write_workspace_file", &upload).unwrap();
    let first = workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).unwrap();
    assert_eq!(first["next_offset"], 16384);
    assert!(!root.join("source/nested/input.bin").exists());
    assert_eq!(
        workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).unwrap(),
        first
    );
    upload["data_base64"] = json!(STANDARD.encode(b"conflict"));
    assert!(workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).is_err());
    for offset in [16384, 32768] {
        upload["offset"] = json!(offset);
        upload["data_base64"] =
            json!(STANDARD.encode(&data[offset..data.len().min(offset + 16384)]));
        workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).unwrap();
    }
    assert_eq!(
        workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).unwrap()["complete"],
        true
    );
    assert_eq!(
        std::fs::read(root.join("source/nested/input.bin")).unwrap(),
        data
    );
    let mut received = Vec::new();
    loop {
        let part = workspace::call(&store,"read_workspace_file",&json!({"workspace_id":"bytes","path":"source/nested/input.bin","offset":received.len()}), |_| panic!()).unwrap();
        let bytes = STANDARD
            .decode(part["data_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            part["chunk_sha256"],
            format!("{:x}", Sha256::digest(&bytes))
        );
        received.extend(bytes);
        if part["eof"] == true {
            break;
        }
    }
    assert_eq!(received, data);
    let seal = workspace::call(&store, "seal_workspace", &request, |_| panic!()).unwrap();
    std::fs::write(root.join("source/resources_rc.py"), "generated").unwrap();
    let reseal = workspace::call(&store, "seal_workspace", &request, |_| panic!()).unwrap();
    assert_ne!(seal["source"]["sha256"], reseal["source"]["sha256"]);
    for path in [
        "source/../escape",
        "source/nested/../../escape",
        "source/file:stream",
        "../escape",
        "workspace.json",
    ] {
        upload["path"] = json!(path);
        assert!(workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).is_err());
    }
    upload["path"] = json!("artifacts/bad.bin");
    upload["offset"] = json!(0);
    upload["total_bytes"] = json!(3);
    upload["data_base64"] = json!(STANDARD.encode(b"bad"));
    assert!(
        workspace::call(&store, "write_workspace_file", &upload, |_| panic!())
            .unwrap_err()
            .contains("SHA-256 mismatch")
    );
    assert!(!root.join("artifacts/bad.bin").exists());
    upload["sha256"] = json!(format!("{:x}", Sha256::digest(b"bad")));
    workspace::call(&store, "write_workspace_file", &upload, |_| panic!()).unwrap();
    std::fs::write(root.join("busy.json"), "{}").unwrap();
    assert!(
        workspace::call(&store, "write_workspace_file", &upload, |_| panic!())
            .unwrap_err()
            .contains("leased")
    );
    assert!(
        workspace::call(&store, "read_workspace_file", &upload, |_| panic!())
            .unwrap_err()
            .contains("leased")
    );
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
