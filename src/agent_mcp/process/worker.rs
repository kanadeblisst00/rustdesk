use super::{platform, store};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn capture<R: Read + Send + 'static>(
    mut reader: R,
    mut file: File,
    limit: u64,
    stop: Arc<AtomicBool>,
    exceeded: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<Result<(), String>>> {
    thread::Builder::new()
        .name("mcp-process-log".into())
        .spawn(move || {
            let result = (|| {
                let mut written = 0;
                let mut drained = 0;
                let mut bytes = [0u8; 16384];
                loop {
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            if stop.load(Ordering::SeqCst) {
                                drained += n;
                                if drained > 1024 * 1024 {
                                    return Err(
                                        "Output did not settle after process cleanup".into()
                                    );
                                }
                            }
                            let keep = (limit.saturating_sub(written) as usize).min(n);
                            file.write_all(&bytes[..keep]).map_err(|e| e.to_string())?;
                            written += keep as u64;
                            if keep < n {
                                exceeded.store(true, Ordering::SeqCst);
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if stop.load(Ordering::SeqCst) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e.to_string()),
                    }
                }
                file.sync_all().map_err(|e| e.to_string())
            })();
            if result.is_err() {
                failed.store(true, Ordering::SeqCst);
            }
            result
        })
}

pub(super) fn run(dir: &Path) -> Result<(), String> {
    store::private_dir(dir)?;
    let _claim = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("worker.lock"))
        .map_err(|e| format!("Worker already claimed or cannot start: {e}"))?;
    let mut state = store::read_json(&dir.join("state.json"))?;
    if store::terminal(&state) {
        return Err("Job is already terminal".into());
    }
    let result = execute(dir, &mut state);
    if let Err(error) = result {
        state["state"] = json!("failed");
        state["error"] = json!(error);
    }
    if state["state"] != "exited" {
        state["success"] = json!(false);
    }
    if let Err(e) = super::workspace::finish(dir, &mut state) {
        state["source_unchanged"] = serde_json::Value::Null;
        state["source_verification"] = json!({"state":"error","error":e});
        state["workspace_error"] = json!(e);
    }
    state["updated_at_ms"] = json!(store::now());
    state["finished_at_ms"] = json!(store::now());
    store::write_json(&dir.join("state.json"), &state)?;
    super::workspace::release(dir)
}

fn execute(dir: &Path, state: &mut serde_json::Value) -> Result<(), String> {
    let mut spec = store::read_json(&dir.join("request.json"))?;
    spec["session"] = json!("worker");
    rustdesk_agent_mcp::process::validate("run_process", &spec)?;
    let stdout = File::create(dir.join("stdout.log")).map_err(|e| e.to_string())?;
    let stderr = File::create(dir.join("stderr.log")).map_err(|e| e.to_string())?;
    if dir.join("cancel.json").exists() {
        state["state"] = json!("cancelled");
        return Ok(());
    }
    let mut command =
        std::process::Command::new(spec["executable"].as_str().ok_or("Missing executable")?);
    command
        .current_dir(spec["cwd"].as_str().ok_or("Missing cwd")?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if matches!(spec["shell"].as_str(), Some("cmd" | "powershell")) {
        #[cfg(windows)]
        platform::shell_arguments(&mut command, &spec)?;
        #[cfg(not(windows))]
        return Err("shell cmd/powershell requires a Windows peer".into());
    } else if let Some(args) = spec["args"].as_array() {
        for arg in args {
            command.arg(arg.as_str().ok_or("Invalid argument")?);
        }
    }
    #[cfg(windows)]
    command.env("PATH", platform::process_path()?);
    if let Some(env) = spec["env"].as_array() {
        for entry in env {
            command.env(
                entry["name"].as_str().ok_or("Invalid environment name")?,
                entry["value"].as_str().ok_or("Invalid environment value")?,
            );
        }
    }
    if let Some(names) = spec["unset_env"].as_array() {
        for name in names {
            command.env_remove(name.as_str().ok_or("Invalid environment name")?);
        }
    }
    let mut child = platform::Child::spawn(&mut command).map_err(|error| format!("Start remote executable {} in cwd {}: {error}; verify the executable path, working directory and PATH with get_environment", spec["executable"], spec["cwd"]))?;
    let output = platform::Reader::new(child.process.stdout.take().ok_or("Missing stdout pipe")?)?;
    let errors = platform::Reader::new(child.process.stderr.take().ok_or("Missing stderr pipe")?)?;
    let stop = Arc::new(AtomicBool::new(false));
    let exceeded = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let limit = spec["max_log_bytes"].as_u64().unwrap_or(16 * 1024 * 1024);
    let out = capture(
        output,
        stdout,
        limit,
        stop.clone(),
        exceeded.clone(),
        failed.clone(),
    )
    .map_err(|e| e.to_string())?;
    let err = match capture(
        errors,
        stderr,
        limit,
        stop.clone(),
        exceeded.clone(),
        failed.clone(),
    ) {
        Ok(thread) => thread,
        Err(e) => {
            child.stop()?;
            stop.store(true, Ordering::SeqCst);
            out.join().map_err(|_| "Log collector panicked")??;
            return Err(e.to_string());
        }
    };
    let started = Instant::now();
    let mut timeout = Duration::from_millis(spec["timeout_ms"].as_u64().unwrap_or(3_600_000));
    state["timeout_ms"] = json!(timeout.as_millis() as u64);
    state["timeout_policy"] = json!("terminate_process_tree");
    state["state"] = json!("running");
    state["started_at_ms"] = json!(store::now());
    state["pid"] = json!(child.process.id());
    let monitor = (|| {
        let mut heartbeat = Instant::now() - Duration::from_secs(2);
        loop {
            if (heartbeat.elapsed() >= Duration::from_secs(1) || started.elapsed() >= timeout)
                && dir.join("timeout.json").exists()
            {
                let update = store::read_json(&dir.join("timeout.json"))?;
                let requested = update["timeout_ms"]
                    .as_u64()
                    .ok_or("Invalid timeout update")?;
                if !(100..=86_400_000).contains(&requested) {
                    return Err("Invalid timeout update".into());
                }
                timeout = timeout.max(Duration::from_millis(requested));
                state["timeout_ms"] = json!(timeout.as_millis() as u64);
            }
            if heartbeat.elapsed() >= Duration::from_secs(1) {
                state["updated_at_ms"] = json!(store::now());
                store::write_json(&dir.join("state.json"), state)?;
                heartbeat = Instant::now();
            }
            if let Some(status) = child.process.try_wait().map_err(|e| e.to_string())? {
                state["state"] = json!("exited");
                state["exit_code"] = json!(status.code());
                state["success"] = json!(status.success());
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    state["signal"] = json!(status.signal());
                }
                break;
            }
            let reason = if dir.join("cancel.json").exists() {
                Some("cancelled")
            } else if started.elapsed() >= timeout {
                Some("timed_out")
            } else if exceeded.load(Ordering::SeqCst) {
                Some("log_limit")
            } else {
                None
            };
            if let Some(reason) = reason {
                state["state"] = json!(reason);
                break;
            }
            if failed.load(Ordering::SeqCst) {
                return Err("Log capture failed".into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok::<_, String>(())
    })();
    let cleanup = child.stop();
    stop.store(true, Ordering::SeqCst);
    let out_result = out.join().map_err(|_| "stdout collector panicked");
    let err_result = err.join().map_err(|_| "stderr collector panicked");
    out_result??;
    err_result??;
    monitor?;
    cleanup?;
    if exceeded.load(Ordering::SeqCst) {
        state["logs_truncated"] = json!(true);
        state["success"] = json!(false);
        if state["state"] == "exited" {
            state["state"] = json!("log_limit");
        }
    }
    Ok(())
}
