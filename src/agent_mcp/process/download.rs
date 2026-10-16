use super::{store, workspace};
use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::{self, File}, io::{Read, Seek, SeekFrom}, path::Path, time::Instant};

fn plain(path: &Path) -> Result<fs::Metadata,String> {
    let meta = fs::symlink_metadata(path).map_err(|e|e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 { return Err("Download refuses reparse points".into()); }
    }
    if meta.file_type().is_symlink() { return Err("Download refuses symbolic links".into()); }
    Ok(meta)
}

fn source(root: &Path, path: &str) -> Result<std::path::PathBuf,String> {
    if path.split('/').count() > 32 || (cfg!(windows) && path.split('/').any(|p|p.ends_with(['.',' ']))) {
        return Err("Invalid bounded download path components".into());
    }
    let absolute = workspace::relative(root,path)?;
    plain(root)?;
    let mut current = root.to_owned();
    for component in path.split('/') { current.push(component); plain(&current)?; }
    Ok(absolute)
}

fn id(args: &Value) -> Result<&str,String> {
    let id = args["transfer_id"].as_str().ok_or("Missing transfer_id")?;
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b|b.is_ascii_alphanumeric() || b"_-".contains(&b)) {
        return Err("Invalid transfer_id".into());
    }
    Ok(id)
}

pub(super) fn call(store: &store::Store, root: &Path, metadata: &Value, operation: &str, args: &Value) -> Result<Value,String> {
    workspace::idle(root)?;
    let id = id(args)?;
    let directory = root.join(".downloads");
    let record = directory.join(format!("{id}.json"));
    if directory.exists() {
        let meta = plain(&directory)?;
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err("Download record directory cannot be a link".into()); }
    }
    if operation == "begin_workspace_download" && !record.try_exists().map_err(|e|e.to_string())? {
        let job = args["job_id"].as_str().ok_or("Missing job_id")?;
        let path = args["path"].as_str().ok_or("Missing path")?;
        if !matches!(path.split('/').next(),Some("build"|"artifacts"|"reports")) {
            return Err("Downloads require an artifact/build/report path".into());
        }
        if metadata["latest_job_id"] != job || !store::terminal(&store.status(job)?) {
            return Err("Download requires the latest completed workspace job".into());
        }
        let context = store::read_json(&store.directory(job)?.join("workspace.json"))?;
        if context["workspace_id"] != args["workspace_id"] { return Err("Job belongs to another workspace".into()); }
        let absolute = source(root,path)?;
        let (bytes, hash) = workspace::hash_file(&absolute,&mut workspace::Budget::new(2*1024*1024*1024))?;
        store::private_dir(&directory)?;
        if fs::read_dir(&directory).map_err(|e|e.to_string())?.count() >= 64 {
            return Err("Maximum 64 retained download records per workspace".into());
        }
        store::write_json(&record,&json!({"version":1,"transfer_id":id,"workspace_id":args["workspace_id"],
            "job_id":job,"path":path,"total_bytes":bytes,"sha256":hash,"created_at_ms":store::now(),
            "last_read":null,"client_receipt":"unknown","completed":false}))?;
    }
    if operation == "remove_workspace_download" && !record.try_exists().map_err(|e|e.to_string())? {
        return Ok(json!({"transfer_id":id,"removed":false,"record_missing":true}));
    }
    let meta = plain(&record)?;
    if !meta.is_file() || meta.file_type().is_symlink() { return Err("Download record cannot be a link".into()); }
    let mut retained = store::read_json(&record)?;
    if retained["transfer_id"] != id || retained["workspace_id"] != args["workspace_id"] {
        return Err("Download record identity mismatch".into());
    }
    if operation == "remove_workspace_download" {
        if retained["completed"] != true { return Err("Incomplete download record is preserved for recovery".into()); }
        fs::remove_file(&record).map_err(|e|e.to_string())?;
        return Ok(json!({"transfer_id":id,"removed":true,"artifact_deleted":false}));
    }
    if operation == "begin_workspace_download" && (retained["job_id"] != args["job_id"] || retained["path"] != args["path"]) {
        return Err("transfer_id already exists with different source; preserve the original record".into());
    }
    if operation == "get_workspace_download" || operation == "begin_workspace_download" { return Ok(retained); }
    if metadata["latest_job_id"] != retained["job_id"] { return Err("Workspace changed since download began; no bytes returned".into()); }
    let absolute = source(root,retained["path"].as_str().ok_or("Missing retained path")?)?;
    let expected = retained["total_bytes"].as_u64().ok_or("Missing retained size")?;
    let mut file = File::open(&absolute).map_err(|e|e.to_string())?;
    let before = file.metadata().map_err(|e|e.to_string())?;
    if before.len() != expected { return Err("Download source size changed; preserve partial output for review".into()); }
    if operation == "finish_workspace_download" {
        let (bytes, hash) = workspace::hash_file(&absolute,&mut workspace::Budget::new(2*1024*1024*1024))?;
        if args["received_bytes"] != bytes || !args["received_sha256"].as_str().is_some_and(|value|value.eq_ignore_ascii_case(&hash)) || retained["sha256"] != hash {
            return Err("Client receipt or current source checksum differs; download is not confirmed".into());
        }
        retained["completed"] = json!(true);
        retained["client_receipt"] = json!("caller_reported_size_and_sha256_match");
        retained["finished_at_ms"] = json!(store::now());
        store::write_json(&record,&retained)?;
        return Ok(retained);
    }
    let offset = args["offset"].as_u64().unwrap_or(0);
    if offset > expected { return Err("Download offset is beyond source size".into()); }
    if let Some(prefix) = args["prefix_sha256"].as_str() {
        let started = Instant::now();
        let mut left = offset;
        let mut hash = Sha256::new();
        let mut buffer = [0;65536];
        while left > 0 {
            if started.elapsed().as_secs() >= 10 { return Err("Resume prefix verification exceeded its time budget".into()); }
            let take = left.min(buffer.len() as u64) as usize;
            let size = file.read(&mut buffer[..take]).map_err(|e|e.to_string())?;
            if size == 0 { return Err("Source shrank during resume verification".into()); }
            hash.update(&buffer[..size]);
            left -= size as u64;
        }
        if !format!("{:x}",hash.finalize()).eq_ignore_ascii_case(prefix) { return Err("Resume prefix SHA-256 differs; no chunk returned".into()); }
    }
    file.seek(SeekFrom::Start(offset)).map_err(|e|e.to_string())?;
    let mut data = Vec::new();
    file.by_ref().take(args["max_bytes"].as_u64().unwrap_or(16384).clamp(1,262144))
        .read_to_end(&mut data).map_err(|e|e.to_string())?;
    let after = file.metadata().map_err(|e|e.to_string())?;
    if after.len() != expected || before.modified().map_err(|e|e.to_string())? != after.modified().map_err(|e|e.to_string())? {
        return Err("Source changed while reading download chunk".into());
    }
    let next = offset + data.len() as u64;
    let chunk_hash = format!("{:x}",Sha256::digest(&data));
    retained["last_read"] = json!({"offset":offset,"next_offset":next,"chunk_sha256":chunk_hash,"observed_at_ms":store::now()});
    store::write_json(&record,&retained)?;
    Ok(json!({"transfer_id":id,"workspace_id":args["workspace_id"],"offset":offset,"next_offset":next,
        "total_bytes":expected,"sha256":retained["sha256"],"chunk_sha256":chunk_hash,"data_base64":STANDARD.encode(&data),
        "eof":next == expected,"client_receipt":"unknown; confirm with finish_workspace_download",
        "consistency":"Verify the assembled file against retained whole-file SHA-256; external writers are not locked out"}))
}
