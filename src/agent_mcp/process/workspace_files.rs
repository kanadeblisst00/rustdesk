use super::store;
use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const CHUNK: usize = 16 * 1024;

#[path = "workspace_replace.rs"]
mod replacement;

fn target(root: &Path, path: &str, create_parents: bool) -> Result<PathBuf, String> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() < 2
        || parts.len() > 32
        || !matches!(parts[0], "source" | "build" | "artifacts" | "reports")
    {
        return Err("Workspace file path must start with source/, build/, artifacts/ or reports/ (max 32 components)".into());
    }
    if parts.iter().any(|p| {
        p.is_empty()
            || matches!(*p, "." | "..")
            || p.contains(['\\', ':', '\0'])
            || (cfg!(windows) && p.ends_with(['.', ' ']))
    }) {
        return Err("Invalid workspace-relative file path".into());
    }
    let mut result = root.to_owned();
    for (index, part) in parts.iter().enumerate() {
        let meta = fs::symlink_metadata(&result).map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Workspace file parents must be real directories without links".into());
        }
        result.push(part);
        if index + 1 < parts.len() && create_parents && !result.exists() {
            fs::create_dir(&result).map_err(|e| e.to_string())?;
        }
    }
    if let Ok(meta) = fs::symlink_metadata(&result) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            return Err("Workspace file must be a regular file without links".into());
        }
    }
    Ok(result)
}

fn digest(path: &Path) -> Result<(u64, String), String> {
    let mut budget = super::workspace::Budget::new(512 * 1024 * 1024);
    super::workspace::hash_file(path, &mut budget)
}

pub(super) fn call(root: &Path, operation: &str, args: &Value) -> Result<Value, String> {
    super::workspace::idle(root)?;
    let path = args["path"].as_str().ok_or("Missing path")?;
    let offset = args["offset"].as_u64().unwrap_or(0);
    if operation == "read_workspace_file" {
        let absolute = target(root, path, false)?;
        let mut file = File::open(&absolute)
            .map_err(|e| format!("Read remote workspace file {}: {e}", absolute.display()))?;
        let total = file.metadata().map_err(|e| e.to_string())?.len();
        if offset > total {
            return Err("Offset is beyond workspace file length".into());
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut data = Vec::new();
        file.take(
            args["max_bytes"]
                .as_u64()
                .unwrap_or(CHUNK as u64)
                .min(CHUNK as u64),
        )
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
        let next = offset + data.len() as u64;
        return Ok(
            json!({"workspace_id":args["workspace_id"],"path":path,"remote_path":absolute,"side":"remote","offset":offset,"next_offset":next,"total_bytes":total,"eof":next == total,"data_base64":STANDARD.encode(&data),"chunk_sha256":format!("{:x}",Sha256::digest(&data)),"consistency":"Verify the assembled file against get_artifact_manifest; external writers are not locked out"}),
        );
    }
    let data = STANDARD
        .decode(args["data_base64"].as_str().ok_or("Missing data_base64")?)
        .map_err(|_| "Invalid base64 chunk")?;
    let total = args["total_bytes"].as_u64().ok_or("Missing total_bytes")?;
    let sha256 = args["sha256"].as_str().ok_or("Missing sha256")?;
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("sha256 must contain 64 hexadecimal characters".into());
    }
    if data.len() > CHUNK
        || offset > total
        || data.len() as u64 > total - offset
        || (data.is_empty() && total != 0)
    {
        return Err(
            "payload_too_large or invalid chunk range: max 16384 decoded bytes within total_bytes"
                .into(),
        );
    }
    let absolute = target(root, path, true)?;
    let replacement = args.get("replace").map(|options| replacement::Replacement::new(root, path, options)).transpose()?;
    let complete = || {
        let mut result = json!({"workspace_id":args["workspace_id"],"path":path,"remote_path":absolute,"next_offset":total,"total_bytes":total,"sha256":sha256.to_ascii_lowercase(),"complete":true});
        if let Some(replacement) = &replacement {
            result["replacement"] = replacement.info();
        }
        result
    };
    if absolute.exists() {
        let (size, hash) = digest(&absolute)?;
        if size == total && hash.eq_ignore_ascii_case(sha256) {
            if let Some(replacement) = &replacement {
                replacement.completed(&absolute, &hash)?;
            }
            return Ok(complete());
        }
        if let Some(replacement) = &replacement {
            replacement.check_hash(&hash)?;
        } else {
            return Err("Workspace file already exists with different content; provide replace.expected_sha256 for verified replacement with backup, or choose another path".into());
        }
    } else if replacement.is_some() {
        return Err("Replacement requires an existing file; omit replace to create a new file".into());
    }
    let uploads = root.join(".uploads");
    store::private_dir(&uploads)?;
    let staging = uploads.join(format!("{:x}", Sha256::digest(path.as_bytes())));
    if !staging.exists() {
        if offset != 0 {
            return Err("Upload is not initialized; start at offset 0".into());
        }
        let count = fs::read_dir(&uploads)
            .map_err(|e| e.to_string())?
            .try_fold(0usize, |count, entry| entry.map(|_| count + 1))
            .map_err(|e| e.to_string())?;
        if count >= 64 {
            return Err("Maximum 64 unfinished workspace uploads".into());
        }
        store::private_dir(&staging)?;
        let mut metadata = json!({"path":path,"total_bytes":total,"sha256":sha256.to_ascii_lowercase()});
        if let Some(options) = args.get("replace") {
            metadata["replace"] = options.clone();
        }
        store::write_json(
            &staging.join("upload.json"),
            &metadata,
        )?;
        File::create(staging.join("data")).map_err(|e| e.to_string())?;
    }
    store::private_dir(&staging)?;
    let metadata = store::read_json(&staging.join("upload.json"))?;
    if metadata["path"] != path
        || metadata["total_bytes"] != total
        || metadata["sha256"] != sha256.to_ascii_lowercase()
        || metadata["replace"] != args["replace"]
    {
        return Err("Upload parameters conflict with retained upload; reuse the original size/hash or choose another path".into());
    }
    let staged = staging.join("data");
    if fs::symlink_metadata(&staged)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Upload data cannot be a link".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&staged)
        .map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > total {
        return Err("Staged upload exceeds declared total_bytes".into());
    }
    let end = offset + data.len() as u64;
    if offset > size || (offset < size && end > size) {
        return Err(format!("Upload offset mismatch; next_offset={size}"));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| e.to_string())?;
    if offset < size {
        let mut previous = vec![0; data.len()];
        file.read_exact(&mut previous).map_err(|e| e.to_string())?;
        if previous != data {
            return Err("Retried upload chunk differs from stored bytes".into());
        }
    } else {
        file.write_all(&data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    let next = size.max(end);
    drop(file);
    if next == total {
        let (_, actual) = digest(&staged)?;
        if !actual.eq_ignore_ascii_case(sha256) {
            fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
            return Err(
                "Uploaded file SHA-256 mismatch; discarded staging data, restart at offset 0"
                    .into(),
            );
        }
        if let Some(replacement) = &replacement {
            replacement.publish(&staged, &target(root, path, false)?)?;
        } else {
            // Hard-link publication refuses an existing destination, including a concurrent writer.
            fs::hard_link(&staged, &absolute)
                .map_err(|e| format!("Publish verified workspace file without overwrite: {e}"))?;
        }
        fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
        return Ok(complete());
    }
    Ok(
        json!({"workspace_id":args["workspace_id"],"path":path,"next_offset":next,"total_bytes":total,"complete":false}),
    )
}
