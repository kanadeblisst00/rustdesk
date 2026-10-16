use super::store::{self, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(test)]
#[path = "retention_tests.rs"]
mod tests;

pub(super) const HIGH_WATER: usize = 224;
pub(super) const LOW_WATER: usize = 192;
const MIN_AGE_MS: u64 = 86_400_000;

pub(super) fn capacity(jobs: &[Value]) -> Value {
    let unknown = jobs.iter().filter(|j| j["state"] == "unknown").count();
    let completed = jobs.iter().filter(|j| store::terminal(j)).count();
    let active = jobs.len() - completed - unknown;
    json!({"retained":jobs.len(),"completed":completed,"active":active,"unknown":unknown,
        "max_retained":256,"max_active":16,"available_retained":256usize.saturating_sub(jobs.len()),
        "available_active":16usize.saturating_sub(active+unknown),
        "blocked_by":if active+unknown>=16 {"active_or_unknown"} else if jobs.len()>=256 {"retained"} else {"none"},
        "auto_archive":{"high_water":HIGH_WATER,"low_water":LOW_WATER,"min_age_ms":MIN_AGE_MS,"preserves_unknown":true}})
}

pub(super) fn archive_root(root: &Path) -> Result<PathBuf, String> {
    Ok(root
        .parent()
        .ok_or("Job root has no parent")?
        .join("job-archives")
        .join("retained"))
}

fn plain(path: &Path) -> Result<fs::Metadata, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("Archive refuses reparse points".into());
        }
    }
    if meta.file_type().is_symlink() {
        return Err("Archive refuses symbolic links".into());
    }
    Ok(meta)
}

fn snapshot(base: &Path, dir: &Path, files: &mut Vec<Value>) -> Result<(), String> {
    if !plain(dir)?.is_dir() {
        return Err("Archive requires a real directory".into());
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let meta = plain(&path)?;
        if meta.is_dir() {
            snapshot(base, &path, files)?;
        } else if meta.is_file() {
            let mut file = fs::File::open(&path).map_err(|e| e.to_string())?;
            let mut hasher = Sha256::new();
            let mut bytes = 0u64;
            let mut buffer = [0u8; 65536];
            loop {
                let size = file.read(&mut buffer).map_err(|e| e.to_string())?;
                if size == 0 {
                    break;
                }
                bytes += size as u64;
                hasher.update(&buffer[..size]);
            }
            files.push(json!({"path":path.strip_prefix(base).map_err(|e| e.to_string())?,"size":bytes,"sha256":format!("{:x}",hasher.finalize())}));
        } else {
            return Err("Archive refuses special files".into());
        }
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(())
}

fn old_terminal(state: &Value, now: u64) -> bool {
    store::terminal(state)
        && state["finished_at_ms"]
            .as_u64()
            .or_else(|| state["updated_at_ms"].as_u64())
            .is_some_and(|time| time > 0 && now.saturating_sub(time) >= MIN_AGE_MS)
}

fn release_lock(path: &Path, owner: &str) -> Result<(), String> {
    plain(path)?;
    if fs::read_to_string(path).map_err(|e| e.to_string())? != owner {
        return Err("Archive lock ownership changed; replacement lock preserved".into());
    }
    fs::remove_file(path).map_err(|e| format!("Archive lock cleanup failed: {e}"))
}

pub(super) fn reclaim(store: &Store, jobs: &[Value]) -> Result<usize, String> {
    if jobs.len() < HIGH_WATER {
        return Ok(0);
    }
    let archive = archive_root(&store.root)?;
    store::private_dir(archive.parent().ok_or("Missing archive parent")?)?;
    store::private_dir(&archive)?;
    // The same create-new lock is used by the bridge's compatibility archiver.
    let lock = archive
        .parent()
        .ok_or("Missing archive parent")?
        .join("retention.lock");
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(&lock) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err("Archive lock is held or needs review; no lock was removed".into())
        }
        Err(e) => return Err(e.to_string()),
    };
    let owner = uuid::Uuid::new_v4().to_string();
    file.write_all(owner.as_bytes())
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    let result = (|| {
        let mut candidates: Vec<_> = jobs
            .iter()
            .filter(|state| old_terminal(state, store::now()))
            .collect();
        candidates.sort_by_key(|state| {
            state["finished_at_ms"]
                .as_u64()
                .or_else(|| state["updated_at_ms"].as_u64())
                .unwrap_or(u64::MAX)
        });
        let mut count = 0;
        for state in candidates
            .into_iter()
            .take(jobs.len().saturating_sub(LOW_WATER))
        {
            let id = state["job_id"].as_str().ok_or("Archive job ID missing")?;
            let source = store.directory(id)?;
            if source.parent() != Some(store.root.as_path()) {
                return Err("Archive source already moved".into());
            }
            let current = store.status(id)?;
            if !old_terminal(&current, store::now()) || current["job_id"] != id {
                return Err("Archive job state changed; preserved for review".into());
            }
            let destination = archive.join(id);
            if destination.try_exists().map_err(|e| e.to_string())? {
                return Err("Archive already exists; never overwrite".into());
            }
            let mut before = Vec::new();
            snapshot(&source, &source, &mut before)?;
            let receipt = archive.join(format!("{id}.manifest.json"));
            if receipt.try_exists().map_err(|e| e.to_string())? {
                return Err("Archive receipt already exists; inspect interrupted archival".into());
            }
            store::write_json(
                &receipt,
                &json!({"version":1,"job_id":id,"files":before,"archived_at_ms":store::now(),"source":source,"destination":destination}),
            )?;
            let mut after = Vec::new();
            snapshot(&source, &source, &mut after)?;
            if before != after || !old_terminal(&store.status(id)?, store::now()) {
                return Err("Archive input changed; no source removed".into());
            }
            // One same-filesystem rename retains every original byte, including private request data.
            fs::rename(&source, &destination)
                .map_err(|e| format!("Archive rename failed; inspect receipt: {e}"))?;
            count += 1;
        }
        Ok(count)
    })();
    drop(file);
    match (result, release_lock(&lock, &owner)) {
        (Err(error), Err(cleanup)) => Err(format!("{error}; {cleanup}")),
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}
