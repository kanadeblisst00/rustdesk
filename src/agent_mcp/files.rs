use super::*;
use crate::flutter::FlutterSession;
use std::path::{Path, PathBuf};

fn local_path_error(path: &Path, operation: &str, error: std::io::Error) -> Value {
    json!({"code":"local_path_error","side":"local","filesystem":"RustDesk MCP controller process","path":path,"operation":operation,"os_error":error.raw_os_error(),"message":error.to_string()})
}

fn preflight(args: &Map<String, Value>) -> Result<Value, Value> {
    let upload = args.get("direction").and_then(Value::as_str) == Some("upload");
    let key = if upload { "source" } else { "destination" };
    let path = PathBuf::from(args.get(key).and_then(Value::as_str).unwrap_or(""));
    if !path.is_absolute() {
        return Err(
            json!({"code":"local_path_error","side":"local","filesystem":"RustDesk MCP controller process","path":path,"message":format!("{key} must be an absolute path on the machine running RustDesk MCP, not an agent/container path on another machine")}),
        );
    }
    let normalized = if upload {
        let metadata =
            std::fs::metadata(&path).map_err(|e| local_path_error(&path, "metadata", e))?;
        if metadata.is_file() {
            std::fs::File::open(&path).map_err(|e| local_path_error(&path, "open", e))?;
        } else if metadata.is_dir() {
            std::fs::read_dir(&path).map_err(|e| local_path_error(&path, "read_dir", e))?;
        } else {
            return Err(
                json!({"code":"local_path_error","side":"local","path":path,"message":"Transfer source must be a file or directory"}),
            );
        }
        std::fs::canonicalize(&path).map_err(|e| local_path_error(&path, "canonicalize", e))?
    } else {
        // Downloads may create nested directories; do not create them during preflight.
        path
    };
    Ok(
        json!({"direction":if upload {"upload"}else{"download"},"local_path":normalized,"remote_path":args[if upload {"destination"}else{"source"}],"local_filesystem":"RustDesk MCP controller process"}),
    )
}

pub(super) fn page(directory: &Value, offset: usize, limit: usize) -> Result<Value, String> {
    if let Some(error) = directory["error"].as_str() {
        return Err(error.into());
    }
    let entries = directory["entries"]
        .as_array()
        .ok_or("Invalid directory listing")?;
    if offset > entries.len() {
        return Err("Directory offset is out of range".into());
    }
    let mut end = offset;
    let mut bytes = 0;
    for entry in entries.iter().skip(offset).take(limit) {
        let size = entry.to_string().len();
        if bytes + size > 48 * 1024 {
            if end == offset {
                return Err("One directory entry exceeds the 48 KiB page budget".into());
            }
            break;
        }
        bytes += size;
        end += 1;
    }
    Ok(
        json!({"path":directory["path"],"id":directory["id"],"side":"remote","entries":entries[offset..end],"total":entries.len(),"entry_count":entries.len(),"next_offset":if end < entries.len(){Some(end)}else{None}}),
    )
}

pub(super) fn directory_event(state: &SessionState, data: &Value) -> Value {
    let raw = data["value"].as_str().unwrap_or("");
    let directory = if raw.len() > 4 * 1024 * 1024 {
        json!({"error":"Directory listing exceeds 4 MiB cache limit; list a narrower directory"})
    } else {
        serde_json::from_str(raw).unwrap_or_else(|_| json!({"error":"Invalid directory listing"}))
    };
    let event = if data.to_string().len() <= 65536 {
        data.clone()
    } else {
        let mut event = page(&directory, 0, 100).unwrap_or_else(
            |error| json!({"error":error,"path":directory["path"],"side":"remote"}),
        );
        event["paginated"] = json!(true);
        event["is_local"] = json!("false");
        event
    };
    *state.directory.lock().unwrap() = Some(directory);
    event
}

fn same_path(left: &str, right: &str, windows: bool) -> bool {
    if windows {
        left.replace('\\', "/")
            .trim_end_matches('/')
            .eq_ignore_ascii_case(right.replace('\\', "/").trim_end_matches('/'))
    } else {
        left.trim_end_matches('/') == right.trim_end_matches('/')
    }
}

pub(super) fn call(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    session::ready(s)?;
    if !s.is_file_transfer() || !*s.server_file_transfer_enabled.read().unwrap() {
        return Err("An authenticated file-transfer session with file permission is required; file_list always lists the REMOTE machine".into());
    }
    if name == "file_transfer" {
        let metadata = match preflight(args) {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(
                    json!({"content":[{"type":"text","text":error.to_string()}],"structuredContent":{"error":error},"isError":true}),
                )
            }
        };
        let mut result = session::call(id, s, state, name, args)?;
        result["structuredContent"]["paths"] = metadata;
        result["content"][0]["text"] = json!(result["structuredContent"].to_string());
        return Ok(result);
    }
    if name == "file_directory" {
        let directory = state.directory.lock().unwrap();
        return page(
            directory
                .as_ref()
                .ok_or("No remote directory listing received; call file_list first")?,
            number(args, "offset", 0) as usize,
            number(args, "limit", 100) as usize,
        )
        .map(success);
    }
    let queued = session::call(id, s, state, "file_list", args)?;
    let deadline = Instant::now() + Duration::from_millis(number(args, "timeout_ms", 3000) as u64);
    loop {
        session::get(id)?;
        session::ready(s)?;
        if let Some(directory) = state.directory.lock().unwrap().as_ref() {
            if let Some(error) = directory["error"].as_str() {
                return Err(error.into());
            }
            let path = string(args, "path")?;
            let windows =
                s.lc.read()
                    .unwrap()
                    .peer_info
                    .as_ref()
                    .is_some_and(|p| p.platform == "Windows");
            if !path.is_empty()
                && !same_path(directory["path"].as_str().unwrap_or(""), path, windows)
            {
                return Err(format!("Remote directory changed while listing: requested {path}, received {}; retry file_list", directory["path"]));
            }
            let mut result = page(directory, 0, 100)?;
            result["after_cursor"] = queued["structuredContent"]["after_cursor"].clone();
            result["event_type"] = json!("file_dir");
            result["queued"] = json!(false);
            return Ok(success(result));
        }
        if Instant::now() >= deadline {
            let mut result = queued;
            result["structuredContent"]["side"] = json!("remote");
            result["structuredContent"]["path"] = args["path"].clone();
            result["structuredContent"]["pending"] = json!(true);
            result["content"][0]["text"] = json!(result["structuredContent"].to_string());
            return Ok(result);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_upload_identifies_controller_path_and_errno() {
        let path = std::env::temp_dir().join(format!("mcp-missing-{}", SessionID::new_v4()));
        let args = json!({"direction":"upload","source":path,"destination":"C:\\temp\\upload"});
        let error = preflight(args.as_object().unwrap()).unwrap_err();
        assert_eq!(error["side"], "local");
        assert_eq!(error["path"], json!(path));
        assert!(error["os_error"].is_number());
    }

    #[test]
    fn real_local_file_is_accepted_and_large_remote_directory_is_paged() {
        let args = json!({"direction":"upload","source":std::env::current_exe().unwrap(),"destination":"C:\\temp\\upload"});
        assert!(preflight(args.as_object().unwrap()).is_ok());
        let entries: Vec<_> = (0..2000)
            .map(|i| json!({"name":format!("file-{i}-{}", "x".repeat(200))}))
            .collect();
        let directory = json!({"path":"C:\\temp","entries":entries});
        let state = SessionState::default();
        let event = directory_event(&state, &json!({"value":directory.to_string()}));
        assert_eq!(event["entry_count"], 2000);
        assert!(event.to_string().len() < 65536);
        let mut offset = 0;
        while offset < 2000 {
            let result = page(&directory, offset, 500).unwrap();
            let count = result["entries"].as_array().unwrap().len();
            assert!(count > 0);
            assert_eq!(result["entries"][0], entries[offset]);
            offset += count;
            assert!(result.to_string().len() < 65536);
        }
        assert!(same_path("C:\\temp\\", "c:/temp", true));
        assert!(!same_path("/tmp/A", "/tmp/a", false));
    }
}
