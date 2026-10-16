use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use std::{fs::File, io::{Read, Seek, SeekFrom}, path::Path};

pub(super) fn call(args: &Value) -> Result<Value, String> {
    let path = Path::new(args["path"].as_str().ok_or("Missing path")?);
    if !path.is_absolute() || !std::fs::metadata(path).map_err(|e| e.to_string())?.is_file() {
        return Err("path must name an existing absolute regular file".into());
    }
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(hbb_common::libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() { return Err("Only regular files can be read".into()); }
    let size = meta.len();
    let limit = args["max_bytes"].as_u64().unwrap_or(16384).clamp(1,65536);
    let mut offset = args["offset"].as_u64().unwrap_or(0);
    if args.get("tail_lines").is_some() { offset = size.saturating_sub(limit); }
    if offset > size { return Err("Offset is beyond current file size".into()); }
    file.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let mut truncated_start = false;
    if let Some(lines) = args["tail_lines"].as_u64() {
        let end = bytes.len().saturating_sub(usize::from(bytes.ends_with(b"\n")));
        let start = bytes[..end].iter().enumerate().rev().filter(|(_, b)| **b == b'\n')
            .nth(lines.saturating_sub(1) as usize).map(|(i, _)| i + 1).unwrap_or(0);
        truncated_start = offset > 0 && start == 0;
        bytes.drain(..start);
        offset += start as u64;
    }
    let next = offset + bytes.len() as u64;
    let mut result = super::output::decode(&bytes, args["encoding"].as_str().unwrap_or("auto"));
    super::output::provenance(&mut result,args,&Value::Null);
    super::presentation::annotate(&mut result);
    if args["include_base64"] != false || !result["text"].is_string() {
        result["data_base64"] = json!(STANDARD.encode(&bytes));
    }
    result["path"] = json!(path);
    result["offset"] = json!(offset);
    result["next_offset"] = json!(next);
    result["size_bytes"] = json!(size);
    result["eof"] = json!(next >= size);
    result["truncated_start"] = json!(truncated_start);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_offsets_and_bounded_tail_without_modifying_file() {
        let dir = std::env::temp_dir().join(format!("mcp-read-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("log.txt");
        std::fs::write(&path, b"first\nlast\n").unwrap();
        let tail = call(&json!({"path":path,"tail_lines":1,"include_base64":false})).unwrap();
        assert_eq!(tail["text"], "last\n");
        assert_eq!(tail["offset"], 6);
        assert_eq!(tail["next_offset"], 11);
        assert!(tail.get("data_base64").is_none());
        let page = call(&json!({"path":path,"offset":1,"max_bytes":2})).unwrap();
        assert_eq!(page["text"], "ir");
        assert_eq!(page["eof"], false);
        assert!(call(&json!({"path":dir})).is_err());
        assert!(call(&json!({"path":path,"offset":12})).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"first\nlast\n");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
