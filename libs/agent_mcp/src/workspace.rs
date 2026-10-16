use serde_json::{json, Value};

pub fn tools() -> Vec<Value> {
    let id = json!({"type":"string","minLength":1,"maxLength":64});
    let mut result = Vec::new();
    for (name, description, read, properties, required) in [
        ("create_workspace", "Create private source/build/artifacts/reports directories on the remote terminal user account. source_revision is a caller-declared label, not proof of a Git checkout. Upload/clone sources then seal_workspace. Reusing an ID with another revision fails.", false, json!({"workspace_id":id,"source_revision":{"type":"string","minLength":1,"maxLength":256}}), vec!["workspace_id","source_revision"]),
        ("get_workspace", "Inspect workspace paths, source fingerprint and current exclusive command lease.", true, json!({"workspace_id":id}), vec!["workspace_id"]),
        ("begin_workspace_download", "Create or recover a durable artifact download record by transfer_id for the workspace's latest completed job. Returns total bytes and whole-file SHA-256; same ID with different source fails. Uses existing workspace byte chunks. Maximum 2 GiB and 64 records per workspace; bounded hashing can require smaller artifacts on slow storage.", false, json!({"workspace_id":id,"transfer_id":id,"job_id":id,"path":{"type":"string","minLength":1,"maxLength":4096}}), vec!["workspace_id","transfer_id","job_id","path"]),
        ("get_workspace_download", "Query the durable download record after reconnect. last_read means bytes served, not received by the caller; completed requires an explicit matching client receipt. Does not resend chunks.", true, json!({"workspace_id":id,"transfer_id":id}), vec!["workspace_id","transfer_id"]),
        ("remove_workspace_download", "Remove one explicitly selected completed download receipt to release retained-record capacity. Incomplete or unknown transfers are preserved. Does not delete the artifact, workspace source or caller's downloaded file. A missing record returns removed:false for safe recovery of a lost removal reply.", false, json!({"workspace_id":id,"transfer_id":id}), vec!["workspace_id","transfer_id"]),
        ("read_workspace_download", "Read up to 256 KiB (default 16 KiB) using the original transfer_id and a byte offset. Returns chunk SHA-256 and retained whole-file SHA-256. On resuming, optionally supply SHA-256 of the local prefix [0,offset) to verify it before receiving more bytes. Persisted progress never proves client delivery; check final assembled checksum and submit finish_workspace_download.", true, json!({"workspace_id":id,"transfer_id":id,"offset":{"type":"integer","minimum":0,"maximum":2147483648u64},"max_bytes":{"type":"integer","minimum":1,"maximum":262144},"prefix_sha256":{"type":"string","minLength":64,"maxLength":64}}), vec!["workspace_id","transfer_id"]),
        ("finish_workspace_download", "Record the caller's received byte count and SHA-256 only after comparing them with the retained identity and freshly verified source. Idempotent receipt recovery; never marks completion merely because the last chunk was served. The caller remains responsible for computing its actual local checksum.", false, json!({"workspace_id":id,"transfer_id":id,"received_bytes":{"type":"integer","minimum":0,"maximum":2147483648u64},"received_sha256":{"type":"string","minLength":64,"maxLength":64}}), vec!["workspace_id","transfer_id","received_bytes","received_sha256"]),
        ("get_workspace_upload", "Recover a workspace upload after reconnect or a missing write reply. Supply the original path, total_bytes and whole-file sha256. Returns durable staged bytes/next_offset, or confirms the published file hash. No file contents or configuration values are returned. Does not restart a transfer or overwrite anything; conflicting staged identities fail.", true, json!({"workspace_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"total_bytes":{"type":"integer","minimum":0,"maximum":536870912},"sha256":{"type":"string","minLength":64,"maxLength":64}}), vec!["workspace_id","path","total_bytes","sha256"]),
        ("write_workspace_file", "Upload bytes from the agent filesystem over this terminal session, without a files session or shared controller filesystem. Max 16 KiB decoded per chunk. Specify workspace-relative path, offset, total_bytes and whole-file SHA-256. Sequential chunks and identical retries are supported; conflicting retries/implicit overwrites fail; use replace.expected_sha256 for explicit replacement with backup. Publishes only after full checksum verification. Workspace must be idle. Seal source after upload.", false, json!({"workspace_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":536870912},"total_bytes":{"type":"integer","minimum":0,"maximum":536870912},"sha256":{"type":"string","minLength":64,"maxLength":64},"data_base64":{"type":"string","maxLength":21848}}), vec!["workspace_id","path","offset","total_bytes","sha256","data_base64"]),
        ("read_workspace_file", "Read an idle workspace file over this terminal session in chunks of up to 16 KiB, without a files session. Paths start with source/, build/, artifacts/ or reports/. Returns raw base64, byte offsets and chunk SHA-256. For artifacts verify the final assembled file against get_artifact_manifest; external writers can still change files.", true, json!({"workspace_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"max_bytes":{"type":"integer","minimum":1,"maximum":16384}}), vec!["workspace_id","path"]),
        ("seal_workspace", "Fingerprint source files with SHA-256 after upload/checkout. Rejects symlinks, special files and concurrent workspace jobs. Skips .git, __pycache__ and .pytest_cache directories. Max 4096 entries/512 MiB; no command is run.", false, json!({"workspace_id":id}), vec!["workspace_id"]),
        ("get_artifact_manifest", "Hash selected relative files from build/artifacts/reports for a completed workspace job. Returns paths, byte sizes, SHA-256 and recorded source identity. Max 64 files/2 GiB. Download separately with file_transfer and verify hashes. Does not claim test coverage or source immutability.", true, json!({"workspace_id":id,"job_id":id,"paths":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","minLength":1,"maxLength":4096}}}), vec!["workspace_id","job_id","paths"]),
        ("publish_artifact", "Copy one artifact from an idle workspace's latest completed job to an absolute remote file path outside the workspace/job store, using the same authenticated OS identity. Require expected size and SHA-256 from get_artifact_manifest; verify the staged copy and published destination. Max 512 MiB and bounded I/O time. Existing identical content is an idempotent success; different content fails unless replace.expected_sha256 matches the old destination. Parent directory must exist. External writers must be stopped; replacement is atomic but not a filesystem compare-and-swap. An uncertain response requires inspecting/retrying the same destination and checksums. Publication does not imply command/test success.", false, json!({"workspace_id":id,"job_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"destination":{"type":"string","minLength":1,"maxLength":4096},"expected_bytes":{"type":"integer","minimum":0,"maximum":536870912},"expected_sha256":{"type":"string","minLength":64,"maxLength":64},"replace":{"type":"object","properties":{"expected_sha256":{"type":"string","minLength":64,"maxLength":64}},"required":["expected_sha256"],"additionalProperties":false}}), vec!["workspace_id","job_id","path","destination","expected_bytes","expected_sha256"]),
        ("remove_workspace", "Delete a workspace and its source/build/artifact/report files. Refuses active/unknown workspace leases. Download artifacts first; retained job logs remain.", false, json!({"workspace_id":id}), vec!["workspace_id"]),
    ] {
        let mut props = properties.as_object().cloned().unwrap_or_default();
        if name == "create_workspace" {
            props.insert("workspace_root".into(), json!({"type":"string","minLength":1,"maxLength":4096,"description":"Optional existing absolute directory on the remote machine, e.g. C:\\w, under the same authorized OS identity. Creates a fresh child named workspace_id; existing directories are never adopted. Choose a private writable parent with suitable inherited Windows ACLs. The location is persisted by workspace ID; subsequent calls need only the ID. Cannot relocate an existing ID. Omit for the existing job-store location. No junction or automatic elevation."}));
        }
        if name == "write_workspace_file" {
            props.insert("chunk_sha256".into(),json!({"type":"string","minLength":64,"maxLength":64,"description":"Optional SHA-256 of this decoded chunk; checked before any staged bytes or directories are written. Whole-file SHA-256 is still mandatory."}));
            props.insert("replace".into(), json!({"type":"object","properties":{"expected_sha256":{"type":"string","minLength":64,"maxLength":64}},"required":["expected_sha256"],"additionalProperties":false,"description":"Explicit replacement of an existing file. Supply its current whole-file SHA-256 with every chunk. Verify the new upload, retain a verified copy under reports/.mcp-backups/, recheck the old checksum, then atomically replace. Conflicting content/retries fail. Response includes backup paths/hash. Cooperating MCP workspace operations are serialized; external writers must be stopped. Omit to keep no-overwrite behavior."}));
        }
        if name == "seal_workspace" {
            props.insert("source_excludes".into(), json!({"type":"array","maxItems":64,"items":{"type":"string","minLength":1,"maxLength":4096},"description":"Explicit source-relative files or directory subtrees excluded from fingerprinting, e.g. [\"dist\",\".cache\"]. No wildcards/trailing slash. Saved in the seal and reused before/after jobs. Omit to retain the previous rules; [] resets them. Excluded content has no source integrity guarantee. Prefer build/ for generated outputs."}));
        }
        props.insert("session".into(), json!({"type":"string","minLength":1,"maxLength":36}));
        let mut required = required;
        required.push("session");
        result.push(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":props,"required":required,"additionalProperties":false},"annotations":{"readOnlyHint":read,"destructiveHint":!read,"openWorldHint":true}}));
    }
    let mut run = crate::process::tools().remove(0);
    run["name"] = json!("run_workspace_process");
    run["description"] = json!("Run a durable command exclusively in a sealed workspace. cwd is workspace-relative (default build). Verify the source fingerprint before launching. Prevents two cooperating jobs from sharing one build directory; use separate workspaces for parallel platforms. Other terminal/file tools and humans are not locked out. Query with the standard process status/output/cancel tools.");
    run["inputSchema"]["properties"]["workspace_id"] = id;
    run["inputSchema"]["properties"]["cwd"] =
        json!({"type":"string","minLength":1,"maxLength":4096});
    run["inputSchema"]["required"] = json!(["session", "workspace_id", "job_id", "executable"]);
    result.push(run);
    result
}

pub fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "create_workspace"
            | "get_workspace"
            | "begin_workspace_download"
            | "get_workspace_download"
            | "remove_workspace_download"
            | "read_workspace_download"
            | "finish_workspace_download"
            | "get_workspace_upload"
            | "write_workspace_file"
            | "read_workspace_file"
            | "seal_workspace"
            | "get_artifact_manifest"
            | "publish_artifact"
            | "remove_workspace"
            | "run_workspace_process"
    )
}
