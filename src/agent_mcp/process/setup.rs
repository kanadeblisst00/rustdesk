use super::{platform, store, worker};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const LIMIT: usize = 1024 * 1024;

fn environment(bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    if bytes.len() % 2 != 0 || bytes.len() > LIMIT {
        return Err("Setup environment is not bounded UTF-16LE".into());
    }
    let wide: Vec<_> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    let text = String::from_utf16(&wide).map_err(|_| "Setup environment is not valid UTF-16LE")?;
    let mut vars = BTreeMap::new();
    for line in text.trim_start_matches('\u{feff}').split("\r\n") {
        if line.is_empty() || line.starts_with('=') {
            continue;
        }
        let (name, value) = line
            .split_once('=')
            .ok_or("Malformed setup environment record")?;
        if name.is_empty() || line.contains(['\0', '\r', '\n']) {
            return Err("Malformed setup environment record".into());
        }
        if vars
            .insert(name.to_ascii_uppercase(), value.to_owned())
            .is_some()
        {
            return Err("Duplicate setup environment variable".into());
        }
    }
    if vars.is_empty() {
        return Err("Setup did not return an environment".into());
    }
    Ok(vars)
}

pub(super) fn initialize(
    dir: &Path,
    spec: &Value,
    state: &mut Value,
    target: &mut Command,
) -> Result<bool, String> {
    let setup = &spec["environment_script"];
    let path = setup["path"]
        .as_str()
        .ok_or("Missing environment_script.path")?;
    if !Path::new(path).is_absolute() || !Path::new(path).is_file() {
        return Err(
            "environment_script.path must be an existing absolute remote batch path".into(),
        );
    }
    let cmd = platform::system_directory()?.join("cmd.exe");
    let mut script = format!("call \"{path}\"");
    if let Some(args) = setup["args"].as_array() {
        for arg in args {
            script.push_str(&format!(
                " \"{}\"",
                arg.as_str().ok_or("Invalid setup argument")?
            ));
        }
    }
    // Only the environment dump is Unicode; ordinary script output remains in setup.log.
    script.push_str(&format!(" 1>&2 && \"{}\" /D /U /C set", cmd.display()));
    let mut command = Command::new(&cmd);
    command
        .args(["/D", "/E:ON", "/V:OFF", "/S", "/C"])
        .raw_arg(format!("\"{script}\""))
        .current_dir(spec["cwd"].as_str().ok_or("Missing cwd")?)
        .env("PATH", platform::process_path()?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let log = File::create(dir.join("setup.log")).map_err(|e| e.to_string())?;
    let mut child = platform::Child::spawn(&mut command).map_err(|error| error.record(state))?;
    state["setup_pid"] = json!(child.process.id());
    state["termination_requested"] = json!(true);
    state["termination_reason"] = json!("worker_failure");
    let mut output =
        platform::Reader::new(child.process.stdout.take().ok_or("Missing setup stdout")?)?;
    let errors = platform::Reader::new(child.process.stderr.take().ok_or("Missing setup stderr")?)?;
    let stop = Arc::new(AtomicBool::new(false));
    let exceeded = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let logger = worker::capture(
        errors,
        log,
        LIMIT as u64,
        stop.clone(),
        exceeded.clone(),
        failed.clone(),
    )
    .map_err(|e| e.to_string())?;
    state["termination_requested"] = json!(false);
    state["termination_reason"] = Value::Null;
    let started = Instant::now();
    let timeout = Duration::from_millis(setup["timeout_ms"].as_u64().unwrap_or(120_000));
    state["phase"] = json!("environment_setup");
    state["setup_started_at_ms"] = json!(store::now());
    state["setup_timeout_ms"] = json!(timeout.as_millis() as u64);
    state["setup_pid"] = json!(child.process.id());
    let mut bytes = Vec::new();
    let mut read = || -> Result<bool, String> {
        let mut buffer = [0; 16384];
        match output.read(&mut buffer) {
            Ok(0) => Ok(false),
            Ok(n) => {
                if bytes.len() + n > LIMIT {
                    return Err("Setup environment exceeds 1 MiB".into());
                }
                bytes.extend_from_slice(&buffer[..n]);
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(format!("Read setup environment: {e}")),
        }
    };
    let result = (|| -> Result<bool, String> {
        let mut heartbeat = Instant::now() - Duration::from_secs(2);
        loop {
            read()?;
            if heartbeat.elapsed() >= Duration::from_secs(1) {
                state["updated_at_ms"] = json!(store::now());
                store::write_json(&dir.join("state.json"), state)?;
                heartbeat = Instant::now();
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
                state["termination_reason"] = json!(reason);
                state["termination_requested"] = json!(true);
                state["state"] = json!(reason);
                state["setup_success"] = json!(false);
                return Ok(false);
            }
            if failed.load(Ordering::SeqCst) {
                return Err("Setup log capture failed".into());
            }
            if let Some(status) = child.process.try_wait().map_err(|e| e.to_string())? {
                state["setup_exit_code"] = json!(status.code());
                state["setup_success"] = json!(status.success());
                if !status.success() {
                    return Err("Environment script failed; read stream:setup. Main command was not started".into());
                }
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    })();
    if result.is_err() && state["setup_exit_code"].is_null() {
        state["termination_requested"] = json!(true);
        state["termination_reason"] = json!("worker_failure");
    }
    let cleanup = child.stop();
    state["setup_tree_cleanup"] = json!(if cleanup.is_ok() { "succeeded" } else { "failed" });
    if let Err(error) = &cleanup { state["cleanup_error"] = json!(error); }
    match child.process.try_wait() {
        Ok(Some(status)) => state["setup_exit_code"] = json!(status.code()),
        Ok(None) => {}
        Err(error) => state["setup_exit_status_error"] = json!(error.to_string()),
    }
    state["setup_finished_at_ms"] = json!(store::now());
    stop.store(true, Ordering::SeqCst);
    let logged = logger.join().map_err(|_| "Setup log collector panicked");
    cleanup?;
    logged??;
    if !result? {
        return Ok(false);
    }
    while read()? {}
    if exceeded.load(Ordering::SeqCst) || failed.load(Ordering::SeqCst) {
        return Err("Setup log capture failed or exceeded 1 MiB".into());
    }
    target.env_clear().envs(environment(&bytes)?);
    state["setup_finished_at_ms"] = json!(store::now());
    state["phase"] = json!("command");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn unicode_environment_preserves_equals_and_rejects_ambiguous_records() {
        let vars = environment(&utf16("=C:=C:\\build\r\nPath=C:\\工具\r\nVALUE=a=b\r\n")).unwrap();
        assert_eq!(vars["PATH"], "C:\\工具");
        assert_eq!(vars["VALUE"], "a=b");
        for text in [
            "",
            "not an environment\r\n",
            "Path=a\r\nPATH=b\r\n",
            "X=a\nb\r\n",
        ] {
            assert!(environment(&utf16(text)).is_err());
        }
        assert!(environment(&[0]).is_err());
        assert!(environment(&[0, 0xd8]).is_err());
    }
}
