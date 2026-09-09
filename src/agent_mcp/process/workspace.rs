use super::store::{self, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const DIRECTORY: &str = ".workspaces";

fn workspace(root: &Path, id: &str) -> Result<PathBuf, String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("Invalid workspace_id".into());
    }
    Ok(root.join(DIRECTORY).join(id))
}

fn relative(base: &Path, path: &str) -> Result<PathBuf, String> {
    let mut result = base.to_path_buf();
    for component in path.split('/') {
        if component.is_empty()
            || matches!(component, "." | "..")
            || component.contains(['\\', ':', '\0'])
        {
            return Err("Path must be relative with normal slash-separated components".into());
        }
        result.push(component);
        let meta = fs::symlink_metadata(&result)
            .map_err(|e| format!("Inspect {}: {e}", result.display()))?;
        if meta.file_type().is_symlink() || !(meta.is_dir() || meta.is_file()) {
            return Err("Workspace paths cannot contain links or special files".into());
        }
    }
    Ok(result)
}

pub(super) struct Budget {
    bytes: u64,
    limit: u64,
    started: Instant,
}
impl Budget {
    pub(super) fn new(limit: u64) -> Self {
        Self {
            bytes: 0,
            limit,
            started: Instant::now(),
        }
    }
    fn check(&self) -> Result<(), String> {
        if self.bytes > self.limit || self.started.elapsed() > Duration::from_secs(10) {
            return Err(
                "Manifest exceeds byte/time limit; split the workspace or artifact selection"
                    .into(),
            );
        }
        Ok(())
    }
}

pub(super) fn hash_file(path: &Path, budget: &mut Budget) -> Result<(u64, String), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("Manifest accepts regular files only".into());
    }
    budget.bytes = budget.bytes.saturating_add(meta.len());
    budget.check()?;
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 65536];
    let mut read = 0u64;
    loop {
        budget.check()?;
        let n = file.read(&mut bytes).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > meta.len() {
            return Err("File grew while hashing; retry after writes finish".into());
        }
        hash.update(&bytes[..n]);
    }
    let after = file.metadata().map_err(|e| e.to_string())?;
    if read != meta.len()
        || after.modified().map_err(|e| e.to_string())?
            != meta.modified().map_err(|e| e.to_string())?
    {
        return Err("File changed while hashing; retry after writes finish".into());
    }
    Ok((read, format!("{:x}", hash.finalize())))
}

fn source_snapshot(dir: &Path) -> Result<Value, String> {
    fn walk(
        root: &Path,
        relative: &str,
        records: &mut Vec<Value>,
        budget: &mut Budget,
        depth: usize,
    ) -> Result<(), String> {
        budget.check()?;
        if depth > 32 {
            return Err("Source directory depth exceeds 32".into());
        }
        for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Source filenames must be UTF-8")?;
            if records.len() >= 4096 {
                return Err("Source manifest exceeds 4096 entries".into());
            }
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() && matches!(name.as_str(), ".git" | "__pycache__" | ".pytest_cache") {
                continue;
            }
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if kind.is_dir() {
                records.push(json!({"path":path,"directory":true}));
                walk(&entry.path(), &path, records, budget, depth + 1)?;
            } else {
                let (bytes, hash) = hash_file(&entry.path(), budget)?;
                records.push(json!({"path":path,"bytes":bytes,"sha256":hash}));
            }
        }
        Ok(())
    }
    relative(dir, "source")?;
    let mut budget = Budget {
        bytes: 0,
        limit: 512 * 1024 * 1024,
        started: Instant::now(),
    };
    let mut records = Vec::new();
    walk(&dir.join("source"), "", &mut records, &mut budget, 0)?;
    records.sort_by_key(|v| v["path"].as_str().unwrap_or("").to_owned());
    let digest = Sha256::digest(serde_json::to_vec(&records).map_err(|e| e.to_string())?);
    Ok(
        json!({"sha256":format!("{digest:x}"),"entries":records.len(),"bytes":budget.bytes,"captured_at_ms":store::now(),"excluded_directories":[".git","__pycache__",".pytest_cache"]}),
    )
}

pub(super) fn idle(dir: &Path) -> Result<(), String> {
    if dir.join("busy.json").exists() {
        return Err(
            "Workspace is leased by a command; query get_workspace and its job status".into(),
        );
    }
    Ok(())
}

pub(super) struct Lease {
    path: PathBuf,
    job: String,
    transferred: bool,
}
impl Lease {
    fn acquire(dir: &Path, job: &str) -> Result<Self, String> {
        let path = dir.join("busy.json");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("Acquire workspace: {e}"))?;
        let lease = Self {
            path,
            job: job.into(),
            transferred: false,
        };
        file.write_all(&serde_json::to_vec(&json!({"job_id":job})).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(lease)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if !self.transferred {
            if let Err(e) = fs::remove_file(&self.path) {
                hbb_common::log::error!("Release workspace for {}: {e}", self.job);
            }
        }
    }
}

pub(super) fn call(
    store: &Store,
    operation: &str,
    args: &Value,
    launch: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Value, String> {
    let id = args["workspace_id"]
        .as_str()
        .ok_or("Missing workspace_id")?;
    let dir = workspace(&store.root, id)?;
    if operation == "create_workspace" {
        if dir.exists() {
            let existing = store::read_json(&dir.join("workspace.json"))?;
            if existing["source_revision"] != args["source_revision"] {
                return Err(format!("Workspace ID already has another source_revision: existing={}, source_sha256={}. Use get_workspace to inspect it or choose a new ID; existing sources are never overwritten implicitly", existing["source_revision"], existing["source"]["sha256"]));
            }
        } else {
            store::private_dir(&store.root.join(DIRECTORY))?;
            let mut count = 0;
            for entry in fs::read_dir(store.root.join(DIRECTORY)).map_err(|e| e.to_string())? {
                entry.map_err(|e| e.to_string())?;
                count += 1;
                if count >= 64 {
                    return Err("Workspace limit reached (64); remove unused workspaces".into());
                }
            }
            store::private_dir(&dir)?;
            for child in ["source", "build", "artifacts", "reports"] {
                store::private_dir(&dir.join(child))?;
            }
            store::write_json(
                &dir.join("workspace.json"),
                &json!({"workspace_id":id,"source_revision":args["source_revision"],"revision_verified":false,"created_at_ms":store::now()}),
            )?;
        }
    }
    if !fs::symlink_metadata(&dir)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("Workspace must be a real directory".into());
    }
    let mut metadata = store::read_json(&dir.join("workspace.json"))?;
    if matches!(operation, "read_workspace_file" | "write_workspace_file") {
        return super::workspace_files::call(&dir, operation, args);
    }
    match operation {
        "create_workspace" | "get_workspace" => {
            metadata["paths"] = json!({"root":dir,"source":dir.join("source"),"build":dir.join("build"),"artifacts":dir.join("artifacts"),"reports":dir.join("reports")});
            metadata["lease"] = if dir.join("busy.json").exists() {
                store::read_json(&dir.join("busy.json"))?
            } else {
                Value::Null
            };
            Ok(metadata)
        }
        "seal_workspace" => {
            idle(&dir)?;
            metadata["source"] = source_snapshot(&dir)?;
            store::write_json(&dir.join("workspace.json"), &metadata)?;
            Ok(metadata)
        }
        "remove_workspace" => {
            idle(&dir)?;
            fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
            Ok(json!({"workspace_id":id,"removed":true}))
        }
        "run_workspace_process" => {
            let job = args["job_id"].as_str().ok_or("Missing job_id")?;
            let job_dir = store.directory(job)?;
            let cwd = relative(&dir, args["cwd"].as_str().unwrap_or("build"))?;
            let mut command = args.clone();
            command
                .as_object_mut()
                .ok_or("Invalid command")?
                .remove("workspace_id");
            command["cwd"] = json!(cwd);
            rustdesk_agent_mcp::process::validate("run_process", &command)?;
            if job_dir.exists() {
                if store::read_json(&job_dir.join("workspace.json"))?["workspace_id"] != id {
                    return Err("Job belongs to another workspace".into());
                }
                return store.create(&command, launch);
            }
            let mut lease = Lease::acquire(&dir, job)?;
            let current = source_snapshot(&dir)?;
            if metadata["source"]["sha256"].is_null() {
                return Err("Source is unsealed; run seal_workspace after upload/checkout, then retry with the same job_id. Use run_process for preparation commands that do not require sealed source".into());
            }
            if metadata["source"]["sha256"] != current["sha256"] {
                return Err(format!("Source changed: sealed_sha256={}, current_sha256={}. Review source/generated-file changes, call seal_workspace again to accept the new snapshot, then retry. Keep generated files in build/ when possible; run_process does not require a seal", metadata["source"]["sha256"], current["sha256"]));
            }
            let result = store.create(&command, |job_dir| {
                metadata["latest_job_id"] = json!(job);
                store::write_json(&dir.join("workspace.json"), &metadata)?;
                store::write_json(&job_dir.join("workspace.json"), &metadata)?;
                launch(job_dir)
            })?;
            if !store::terminal(&result) {
                lease.transferred = true;
            }
            Ok(result)
        }
        "get_artifact_manifest" => {
            idle(&dir)?;
            let job = args["job_id"].as_str().ok_or("Missing job_id")?;
            let state = store.status(job)?;
            if metadata["latest_job_id"] != job {
                return Err("Workspace has been used by another job; request a manifest for its latest job or use separate workspaces".into());
            }
            if !store::terminal(&state) {
                return Err("Artifact manifests require a completed job".into());
            }
            let context = store::read_json(&store.directory(job)?.join("workspace.json"))?;
            if context["workspace_id"] != id {
                return Err("Job belongs to another workspace".into());
            }
            let mut budget = Budget {
                bytes: 0,
                limit: 2 * 1024 * 1024 * 1024,
                started: Instant::now(),
            };
            let mut files = Vec::new();
            let paths = args["paths"].as_array().ok_or("Missing artifact paths")?;
            for path in paths {
                let path = path.as_str().ok_or("Invalid artifact path")?;
                if !matches!(
                    path.split('/').next(),
                    Some("build" | "artifacts" | "reports")
                ) {
                    return Err(
                        "Artifact paths must start with build/, artifacts/ or reports/".into(),
                    );
                }
                let absolute = relative(&dir, path)?;
                let (bytes, sha256) = hash_file(&absolute, &mut budget)?;
                files.push(
                    json!({"path":path,"remote_path":absolute,"bytes":bytes,"sha256":sha256}),
                );
            }
            Ok(
                json!({"workspace_id":id,"job_id":job,"source":context["source"],"source_revision":context["source_revision"],"revision_verified":false,"source_unchanged_after_job":state["source_unchanged"],"command_success":state["success"],"provenance":"workspace snapshot after latest job; not proof that this command created every file","captured_at_ms":store::now(),"files":files}),
            )
        }
        _ => Err("Unknown workspace operation".into()),
    }
}

pub(super) fn finish(job_dir: &Path, state: &mut Value) -> Result<(), String> {
    let context_path = job_dir.join("workspace.json");
    if !context_path.exists() {
        return Ok(());
    }
    let context = store::read_json(&context_path)?;
    let root = job_dir.parent().ok_or("Invalid job root")?;
    let dir = workspace(
        root,
        context["workspace_id"]
            .as_str()
            .ok_or("Invalid workspace context")?,
    )?;
    let current = source_snapshot(&dir)?;
    state["workspace_id"] = context["workspace_id"].clone();
    state["source_sha256"] = context["source"]["sha256"].clone();
    state["source_unchanged"] = json!(current["sha256"] == context["source"]["sha256"]);
    Ok(())
}

pub(super) fn release(job_dir: &Path) -> Result<(), String> {
    if !job_dir.join("workspace.json").exists() {
        return Ok(());
    }
    let context = store::read_json(&job_dir.join("workspace.json"))?;
    let root = job_dir.parent().ok_or("Invalid job root")?;
    let dir = workspace(
        root,
        context["workspace_id"]
            .as_str()
            .ok_or("Invalid workspace context")?,
    )?;
    let path = dir.join("busy.json");
    let lease = store::read_json(&path)?;
    if lease["job_id"].as_str() != job_dir.file_name().and_then(|n| n.to_str()) {
        return Err("Workspace lease owner changed".into());
    }
    fs::remove_file(path).map_err(|e| e.to_string())
}
