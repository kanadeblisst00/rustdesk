use super::{platform, store};
use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs::{self, File}, io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

fn snapshot(path: &Path, total: u64, prefix: u64, tail: &VecDeque<u8>) -> Result<(), String> {
    let data: Vec<u8> = tail.iter().copied().collect();
    let header = json!({"version":1,"total_bytes":total,"prefix_bytes":prefix,
        "tail_offset":total.saturating_sub(data.len() as u64),"tail_bytes":data.len(),
        "sha256":format!("{:x}", Sha256::digest(&data))});
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e| e.to_string())?;
        writeln!(file, "{header}").map_err(|e| e.to_string())?;
        file.write_all(&data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        drop(file);
        platform::replace(&temporary, path)
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) { hbb_common::log::warn!("Remove tail snapshot temporary: {error}"); }
    }
    result
}

fn open(path: &Path) -> Result<(Value, BufReader<File>), String> {
    let mut reader = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut header = Vec::new();
    reader.by_ref().take(2048).read_until(b'\n', &mut header).map_err(|e| e.to_string())?;
    if !header.ends_with(b"\n") { return Err("Invalid bounded log snapshot header".into()); }
    let value: Value = serde_json::from_slice(&header).map_err(|e| e.to_string())?;
    let total = value["total_bytes"].as_u64().ok_or("Missing total log size")?;
    let prefix = value["prefix_bytes"].as_u64().ok_or("Missing log prefix")?;
    let offset = value["tail_offset"].as_u64().ok_or("Missing log tail offset")?;
    let size = value["tail_bytes"].as_u64().ok_or("Missing log tail size")?;
    if value["version"] != 1 || prefix > total || size > 1024*1024 || offset.checked_add(size) != Some(total) {
        return Err("Invalid log snapshot ranges".into());
    }
    Ok((value, reader))
}

pub(super) fn bounds(path: &Path) -> Result<Value, String> { open(path).map(|(header, _)| header) }

pub(super) fn capture<R: Read + Send + 'static>(mut reader: R, mut prefix_file: File, path: PathBuf,
    limit: u64, stop: Arc<AtomicBool>, exceeded: Arc<AtomicBool>, failed: Arc<AtomicBool>)
    -> std::io::Result<thread::JoinHandle<Result<(), String>>> {
    thread::Builder::new().name("mcp-process-head-tail".into()).spawn(move || {
        let result = (|| {
            let tail_limit = (limit / 2).min(1024*1024) as usize;
            let prefix_limit = limit - tail_limit as u64;
            let mut tail = VecDeque::with_capacity(tail_limit);
            let mut total = 0u64;
            let mut prefix = 0u64;
            let mut drained = 0;
            let mut checkpoint = Instant::now();
            snapshot(&path, total, prefix, &tail)?;
            let mut bytes = [0; 16384];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(size) => {
                        if stop.load(Ordering::SeqCst) {
                            drained += size;
                            if drained > 1024*1024 { return Err("Output did not settle after process cleanup".into()); }
                        }
                        let keep = prefix_limit.saturating_sub(prefix).min(size as u64) as usize;
                        prefix_file.write_all(&bytes[..keep]).map_err(|e| e.to_string())?;
                        prefix += keep as u64;
                        for byte in &bytes[..size] {
                            if tail.len() == tail_limit { tail.pop_front(); }
                            tail.push_back(*byte);
                        }
                        total = total.checked_add(size as u64).ok_or("Log byte counter overflow")?;
                        if total > limit { exceeded.store(true, Ordering::SeqCst); }
                        if checkpoint.elapsed() >= Duration::from_secs(1) {
                            snapshot(&path, total, prefix, &tail)?;
                            checkpoint = Instant::now();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop.load(Ordering::SeqCst) { break; }
                        if checkpoint.elapsed() >= Duration::from_secs(1) {
                            snapshot(&path, total, prefix, &tail)?;
                            checkpoint = Instant::now();
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error.to_string()),
                }
            }
            prefix_file.sync_all().map_err(|e| e.to_string())?;
            snapshot(&path, total, prefix, &tail)
        })();
        if result.is_err() { failed.store(true, Ordering::SeqCst); }
        result
    })
}

pub(super) fn read(dir: &Path, state: &Value, args: &Value) -> Result<Value, String> {
    let stream = args["stream"].as_str().ok_or("Missing stream")?;
    let path = dir.join(format!("{stream}.tail"));
    let (header, mut reader) = open(&path)?;
    let mut tail = Vec::new();
    reader.by_ref().take(1024*1024+1).read_to_end(&mut tail).map_err(|e| e.to_string())?;
    if tail.len() as u64 != header["tail_bytes"].as_u64().unwrap_or(u64::MAX)
        || header["sha256"] != format!("{:x}", Sha256::digest(&tail)) {
        return Err("Log tail snapshot checksum mismatch".into());
    }
    let total = header["total_bytes"].as_u64().ok_or("Missing total")?;
    let prefix = header["prefix_bytes"].as_u64().ok_or("Missing prefix")?;
    let tail_offset = header["tail_offset"].as_u64().ok_or("Missing tail offset")?;
    let size = args["max_bytes"].as_u64().unwrap_or(65536).clamp(1,65536);
    let requested = args["offset"].as_u64().unwrap_or(0);
    let tail_lines = args["tail_lines"].is_u64();
    let mut offset = if tail_lines {
        if tail_offset > prefix { total.saturating_sub(size).max(tail_offset) }
        else { total.saturating_sub(size) }
    } else { requested };
    if offset > total { return Err("Offset is beyond current output".into()); }
    let gap = if offset >= prefix && offset < tail_offset {
        let gap = json!({"from":offset,"to":tail_offset});
        offset = tail_offset;
        gap
    } else { Value::Null };
    let mut data = Vec::new();
    if offset < prefix {
        let mut file = File::open(dir.join(format!("{stream}.log"))).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        file.take(size.min(prefix-offset)).read_to_end(&mut data).map_err(|e| e.to_string())?;
        if tail_lines && data.len() as u64 != size.min(prefix-offset) {
            return Err("Log prefix changed while reading the tail window".into());
        }
        if tail_lines && data.len() < size as usize && offset + data.len() as u64 >= tail_offset {
            let start = (offset + data.len() as u64 - tail_offset) as usize;
            let end = (start + size as usize - data.len()).min(tail.len());
            data.extend_from_slice(&tail[start..end]);
        }
    } else {
        let start = offset.saturating_sub(tail_offset) as usize;
        data.extend_from_slice(&tail[start..(start + size as usize).min(tail.len())]);
    }
    let mut truncated_start = false;
    if let Some(lines) = args["tail_lines"].as_u64() {
        let end = data.len().saturating_sub(usize::from(data.ends_with(b"\n")));
        let start = data[..end].iter().enumerate().rev().filter(|(_,byte)| **byte == b'\n')
            .nth(lines.saturating_sub(1) as usize).map(|(index,_)| index+1).unwrap_or(0);
        truncated_start = offset > 0 && start == 0;
        data.drain(..start);
        offset += start as u64;
    }
    let next = offset + data.len() as u64;
    let mut result = super::output::decode(&data, args["encoding"].as_str().unwrap_or("auto"));
    super::output::provenance(&mut result,args,&state["producer_encoding_hint"]);
    super::presentation::annotate(&mut result);
    for (key,value) in json!({"job_id":args["job_id"],"stream":stream,"offset":offset,"next_offset":next,
        "total_bytes":total,"total_bytes_scope":"original_stream","retained_ranges":[[0,prefix],[tail_offset,total]],"gap":gap,
        "logs_truncated":tail_offset > prefix,"truncated_start":truncated_start,
        "complete":store::terminal(state),"eof":store::terminal(state) && next == total,
        "data_base64":STANDARD.encode(&data)}).as_object().ok_or("Invalid log metadata")? {
        result[key] = value.clone();
    }
    if args["include_base64"] == false && result["text"].is_string() {
        result.as_object_mut().ok_or("Invalid log output")?.remove("data_base64");
    }
    Ok(result)
}
