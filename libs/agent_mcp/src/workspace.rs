use serde_json::{json, Value};

pub fn tools() -> Vec<Value> {
    let id = json!({"type":"string","minLength":1,"maxLength":64});
    let mut result = Vec::new();
    for (name, description, read, properties, required) in [
        ("create_workspace", "Create private source/build/artifacts/reports directories on the remote terminal user account. source_revision is a caller-declared label, not proof of a Git checkout. Upload/clone sources then seal_workspace. Reusing an ID with another revision fails.", false, json!({"workspace_id":id,"source_revision":{"type":"string","minLength":1,"maxLength":256}}), vec!["workspace_id","source_revision"]),
        ("get_workspace", "Inspect workspace paths, source fingerprint and current exclusive command lease.", true, json!({"workspace_id":id}), vec!["workspace_id"]),
        ("write_workspace_file", "Upload bytes from the agent filesystem over this terminal session, without a files session or shared controller filesystem. Max 16 KiB decoded per chunk. Specify workspace-relative path, offset, total_bytes and whole-file SHA-256. Sequential chunks and identical retries are supported; conflicting retries/overwrites fail. Publishes only after full checksum verification. Workspace must be idle. Seal source after upload.", false, json!({"workspace_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":536870912},"total_bytes":{"type":"integer","minimum":0,"maximum":536870912},"sha256":{"type":"string","minLength":64,"maxLength":64},"data_base64":{"type":"string","maxLength":21848}}), vec!["workspace_id","path","offset","total_bytes","sha256","data_base64"]),
        ("read_workspace_file", "Read an idle workspace file over this terminal session in chunks of up to 16 KiB, without a files session. Paths start with source/, build/, artifacts/ or reports/. Returns raw base64, byte offsets and chunk SHA-256. For artifacts verify the final assembled file against get_artifact_manifest; external writers can still change files.", true, json!({"workspace_id":id,"path":{"type":"string","minLength":1,"maxLength":4096},"offset":{"type":"integer","minimum":0,"maximum":9007199254740991i64},"max_bytes":{"type":"integer","minimum":1,"maximum":16384}}), vec!["workspace_id","path"]),
        ("seal_workspace", "Fingerprint source files with SHA-256 after upload/checkout. Rejects symlinks, special files and concurrent workspace jobs. Skips .git, __pycache__ and .pytest_cache directories. Max 4096 entries/512 MiB; no command is run.", false, json!({"workspace_id":id}), vec!["workspace_id"]),
        ("get_artifact_manifest", "Hash selected relative files from build/artifacts/reports for a completed workspace job. Returns paths, byte sizes, SHA-256 and recorded source identity. Max 64 files/2 GiB. Download separately with file_transfer and verify hashes. Does not claim test coverage or source immutability.", true, json!({"workspace_id":id,"job_id":id,"paths":{"type":"array","minItems":1,"maxItems":64,"items":{"type":"string","minLength":1,"maxLength":4096}}}), vec!["workspace_id","job_id","paths"]),
        ("remove_workspace", "Delete a workspace and its source/build/artifact/report files. Refuses active/unknown workspace leases. Download artifacts first; retained job logs remain.", false, json!({"workspace_id":id}), vec!["workspace_id"]),
    ] {
        let mut props = properties.as_object().cloned().unwrap_or_default();
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
            | "write_workspace_file"
            | "read_workspace_file"
            | "seal_workspace"
            | "get_artifact_manifest"
            | "remove_workspace"
            | "run_workspace_process"
    )
}
