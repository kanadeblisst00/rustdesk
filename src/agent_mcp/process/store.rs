use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(super) fn read_json(path: &Path) -> Result<Value, String> {
    let file = File::open(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 1024 * 1024 {
        return Err("Job record exceeds 1 MiB".into());
    }
    serde_json::from_reader(file).map_err(|e| format!("Invalid job record: {e}"))
}

pub(super) fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        file.write_all(&data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        super::platform::replace(&temporary, path)
    })();
    if result.is_err() {
        if let Err(e) = fs::remove_file(&temporary) {
            hbb_common::log::debug!("Remove incomplete job record: {e}");
        }
    }
    result
}

pub(super) fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Job directory must be a real directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { hbb_common::libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("Job directory must be owned by the executing user with mode 0700".into());
        }
    }
    Ok(())
}

pub(super) fn terminal(state: &Value) -> bool {
    matches!(
        state["state"].as_str(),
        Some("exited" | "failed" | "cancelled" | "timed_out" | "log_limit")
    )
}

pub(super) struct Store {
    pub root: PathBuf,
}

static TIMEOUT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Store {
    pub fn new(root: PathBuf) -> Result<Self, String> {
        private_dir(&root)?;
        Ok(Self { root })
    }

    pub fn directory(&self, id: &str) -> Result<PathBuf, String> {
        if id.is_empty()
            || id.len() > 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("Invalid job_id".into());
        }
        let path = self.root.join(id);
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.is_dir() || meta.file_type().is_symlink() {
                return Err("Invalid job directory".into());
            }
        }
        Ok(path)
    }

    pub fn status(&self, id: &str) -> Result<Value, String> {
        let dir = self.directory(id)?;
        let mut state = read_json(&dir.join("state.json"))?;
        state["last_known_state"] = state["state"].clone();
        state["observed_at_ms"] = json!(now());
        state["heartbeat_age_ms"] = json!(now().saturating_sub(state["updated_at_ms"].as_u64().unwrap_or(0)));
        if !terminal(&state)
            && now().saturating_sub(state["updated_at_ms"].as_u64().unwrap_or(0)) > 30_000
        {
            state["state"] = json!("unknown");
            state["error"] = json!("Worker heartbeat is stale; outcome unknown. Inspect the remote machine; do not rerun automatically.");
        }
        for stream in ["stdout", "stderr", "setup"] {
            let size = match fs::metadata(dir.join(format!("{stream}.log"))) {
                Ok(meta) => meta.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.to_string()),
            };
            state[format!("{stream}_bytes")] = json!(size);
        }
        state["log_paths"] =
            json!({"stdout":dir.join("stdout.log"),"stderr":dir.join("stderr.log")});
        if dir.join("setup.log").exists() {
            state["log_paths"]["setup"] = json!(dir.join("setup.log"));
        }
        state["poll_after_ms"] = json!(if terminal(&state) { 0 } else { 1000 });
        if let Some(created) = state["created_at_ms"].as_u64() {
            state["total_elapsed_ms"] = json!(state["finished_at_ms"].as_u64().unwrap_or_else(now).saturating_sub(created));
        }
        if let Some(started) = state["started_at_ms"].as_u64() {
            let end = state["finished_at_ms"].as_u64().unwrap_or_else(now);
            let elapsed = end.saturating_sub(started);
            state["elapsed_ms"] = json!(elapsed);
            if let Some(timeout) = state["timeout_ms"].as_u64() {
                state["remaining_timeout_ms"] = json!(if terminal(&state) {
                    0
                } else {
                    timeout.saturating_sub(elapsed)
                });
            }
        }
        Ok(state)
    }

    pub fn list(&self) -> Result<Value, String> {
        let mut jobs = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() == ".workspaces" {
                continue;
            }
            if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                let id = entry.file_name().to_string_lossy().into_owned();
                jobs.push(match self.status(&id) {
                    Ok(state) => state,
                    Err(error) => json!({"job_id":id,"state":"unknown","error":error}),
                });
            }
            if jobs.len() > 256 {
                return Err("Job store exceeds 256 records; remove completed records".into());
            }
        }
        jobs.sort_by_key(|j| j["job_id"].as_str().unwrap_or("").to_owned());
        Ok(json!({"jobs":jobs,"storage_path":self.root}))
    }

    pub fn create(
        &self,
        arguments: &Value,
        launch: impl FnOnce(&Path) -> Result<(), super::diagnostics::Failure>,
    ) -> Result<Value, String> {
        let id = arguments["job_id"].as_str().ok_or("Missing job_id")?;
        let dir = self.directory(id)?;
        let mut spec = arguments.clone();
        spec.as_object_mut()
            .ok_or("Invalid arguments")?
            .remove("session");
        if dir.exists() {
            if read_json(&dir.join("request.json"))? != spec {
                return Err(
                    "job_id already exists with different parameters; choose a new ID".into(),
                );
            }
            return self.status(id);
        }
        let existing = self.list()?;
        let jobs = existing["jobs"].as_array().ok_or("Invalid job store")?;
        if jobs.len() >= 256 || jobs.iter().filter(|j| !terminal(j)).count() >= 16 {
            return Err("Job limit reached (16 active/unknown, 256 retained); cancel or remove completed jobs".into());
        }
        // This is also the cross-connection idempotency claim. Never relaunch an existing directory.
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|e| format!("Claim job_id: {e}; query the ID before retrying"))?;
        write_json(&dir.join("request.json"), &spec)?;
        let mut state = json!({"job_id":id,"state":"starting","created_at_ms":now(),"updated_at_ms":now(),"exit_code":null,"success":null});
        state["timeout_ms"] = json!(arguments["timeout_ms"].as_u64().unwrap_or(3_600_000));
        state["timeout_policy"] = json!("terminate_process_tree");
        state["log_limit_policy"] = json!(arguments["log_limit_policy"].as_str().unwrap_or("truncate"));
        state["logs_truncated"] = json!(false);
        write_json(&dir.join("state.json"), &state)?;
        if let Err(error) = launch(&dir) {
            let error = error.record(&mut state);
            state["failure_stage"] = json!("worker_launch");
            state["termination_reason"] = json!("launch_failure");
            state["termination_requested"] = json!(false);
            state["state"] = json!("failed");
            state["error"] = json!(error);
            state["success"] = json!(false);
            state["finished_at_ms"] = json!(now());
            write_json(&dir.join("state.json"), &state)?;
        }
        self.status(id)
    }

    pub fn call(&self, operation: &str, args: &Value) -> Result<Value, String> {
        if operation == "list_processes" {
            return self.list();
        }
        if operation == "wait_for_process" {
            return super::wait::call(self, args);
        }
        let id = args["job_id"].as_str().ok_or("Missing job_id")?;
        let dir = self.directory(id)?;
        let state = self.status(id)?;
        match operation {
            "get_process_status" => Ok(state),
            "extend_process_timeout" => {
                let _lock = TIMEOUT_LOCK.lock().unwrap();
                let state = self.status(id)?;
                if !matches!(state["state"].as_str(), Some("starting" | "running")) {
                    return Err("Only starting/running jobs can be extended; a timed_out job has already been terminated".into());
                }
                let requested = args["timeout_ms"].as_u64().ok_or("Missing timeout_ms")?;
                let previous = if dir.join("timeout.json").exists() {
                    read_json(&dir.join("timeout.json"))?["timeout_ms"]
                        .as_u64()
                        .unwrap_or(0)
                } else {
                    0
                };
                if !(100..=86_400_000).contains(&requested)
                    || requested < previous.max(state["timeout_ms"].as_u64().unwrap_or(3_600_000))
                {
                    return Err("Timeout extension cannot shorten the current/requested timeout and must be at most 24 hours".into());
                }
                write_json(&dir.join("timeout.json"), &json!({"timeout_ms":requested}))?;
                let current = self.status(id)?;
                let applied = current["timeout_ms"]
                    .as_u64()
                    .is_some_and(|effective| effective >= requested);
                Ok(
                    json!({"job":current,"requested_timeout_ms":requested,"applied":applied,"note":"Query timeout_ms to confirm the worker applied the extension; a job may finish before the request is observed"}),
                )
            }
            "cancel_process" => {
                if !terminal(&state) {
                    write_json(&dir.join("cancel.json"), &json!({"requested_at_ms":now()}))?;
                }
                Ok(json!({"job":state,"cancel_requested":!terminal(&state)}))
            }
            "remove_process" => {
                if !terminal(&state) {
                    return Err("Only completed jobs can be removed".into());
                }
                fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
                Ok(json!({"job_id":id,"removed":true}))
            }
            "read_process_output" => {
                let stream = args["stream"].as_str().ok_or("Missing stream")?;
                if !matches!(stream, "stdout" | "stderr" | "setup") {
                    return Err("Invalid stream".into());
                }
                let mut offset = args["offset"].as_u64().unwrap_or(0);
                let size = args["max_bytes"].as_u64().unwrap_or(65536).clamp(1, 65536);
                let mut file = match File::open(dir.join(format!("{stream}.log"))) {
                    Ok(file) => Some(file),
                    Err(e)
                        if e.kind() == std::io::ErrorKind::NotFound
                            && state[format!("{stream}_bytes")] == 0 =>
                    {
                        None
                    }
                    Err(e) => return Err(e.to_string()),
                };
                let mut data = Vec::new();
                if let Some(file) = file.as_mut() {
                    if args["tail_lines"].is_u64() {
                        offset = file
                            .metadata()
                            .map_err(|e| e.to_string())?
                            .len()
                            .saturating_sub(size);
                    }
                    if offset > file.metadata().map_err(|e| e.to_string())?.len() {
                        return Err("Offset is beyond current output".into());
                    }
                    file.seek(SeekFrom::Start(offset))
                        .map_err(|e| e.to_string())?;
                    file.take(size)
                        .read_to_end(&mut data)
                        .map_err(|e| e.to_string())?;
                } else if offset != 0 {
                    return Err("Offset is beyond current output".into());
                }
                let mut truncated_start = false;
                if let Some(lines) = args["tail_lines"].as_u64() {
                    let end = data
                        .len()
                        .saturating_sub(usize::from(data.ends_with(b"\n")));
                    let start = data[..end]
                        .iter()
                        .enumerate()
                        .rev()
                        .filter(|(_, byte)| **byte == b'\n')
                        .nth(lines.saturating_sub(1) as usize)
                        .map(|(index, _)| index + 1)
                        .unwrap_or(0);
                    truncated_start = offset > 0 && start == 0;
                    data.drain(..start);
                    offset += start as u64;
                }
                let next = offset + data.len() as u64;
                let mut result =
                    super::output::decode(&data, args["encoding"].as_str().unwrap_or("auto"));
                super::presentation::annotate(&mut result);
                result["truncated_start"] = json!(truncated_start);
                let fields = result.as_object_mut().ok_or("Invalid output metadata")?;
                fields.extend(json!({"job_id":id,"stream":stream,"offset":offset,"next_offset":next,"data_base64":STANDARD.encode(&data),"complete":terminal(&state),"eof":terminal(&state) && next >= state[format!("{stream}_bytes")].as_u64().unwrap_or(0)}).as_object().ok_or("Invalid log metadata")?.clone());
                Ok(result)
            }
            _ => Err("Unknown process operation".into()),
        }
    }
}
