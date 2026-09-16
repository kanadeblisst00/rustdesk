use super::{store, workspace};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 512 * 1024 * 1024;

fn checksum(value: &Value, field: &str) -> Result<String, String> {
    let hash = value.as_str().ok_or_else(|| format!("Missing {field}"))?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("{field} must contain 64 hexadecimal characters"));
    }
    Ok(hash.to_ascii_lowercase())
}

fn existing(path: &Path, budget: &mut workspace::Budget) -> Result<Option<(u64, String)>, String> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.len() > MAX_BYTES {
                return Err("Existing destination exceeds 512 MiB".into());
            }
            workspace::hash_file(path, budget).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn publish(
    store: &store::Store,
    dir: &Path,
    metadata: &Value,
    args: &Value,
) -> Result<Value, String> {
    workspace::idle(dir)?;
    let job = args["job_id"].as_str().ok_or("Missing job_id")?;
    let state = store.status(job)?;
    if metadata["latest_job_id"] != job || !store::terminal(&state) {
        return Err("Publish requires the workspace's latest completed job".into());
    }
    let context = store::read_json(&store.directory(job)?.join("workspace.json"))?;
    if context["workspace_id"] != args["workspace_id"] {
        return Err("Job belongs to another workspace".into());
    }
    let path = args["path"].as_str().ok_or("Missing artifact path")?;
    if !matches!(
        path.split('/').next(),
        Some("build" | "artifacts" | "reports")
    ) {
        return Err("Artifact paths must start with build/, artifacts/ or reports/".into());
    }
    let source = workspace::relative(dir, path)?;
    let destination = PathBuf::from(args["destination"].as_str().ok_or("Missing destination")?);
    if !destination.is_absolute()
        || destination
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || destination.file_name().is_none()
    {
        return Err("Destination must be an absolute remote file path without ..".into());
    }
    #[cfg(windows)]
    if destination
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(':') || n.ends_with(['.', ' ']))
    {
        return Err("Destination cannot use alternate streams or trailing dots/spaces".into());
    }
    let parent = destination.parent().ok_or("Missing destination parent")?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|e| format!("Inspect destination parent: {e}"))?;
    let canonical_workspace = fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let canonical_store = fs::canonicalize(&store.root).map_err(|e| e.to_string())?;
    if canonical_parent.starts_with(canonical_store)
        || canonical_parent.starts_with(&canonical_workspace)
    {
        return Err(
            "Publish destination must be outside the job store and source workspace".into(),
        );
    }
    if !fs::canonicalize(&source)
        .map_err(|e| e.to_string())?
        .starts_with(&canonical_workspace)
    {
        return Err("Artifact path resolves outside the workspace".into());
    }
    let expected_bytes = args["expected_bytes"]
        .as_u64()
        .ok_or("Missing expected_bytes")?;
    if expected_bytes > MAX_BYTES {
        return Err("Artifact publication exceeds 512 MiB; use file transfer".into());
    }
    let expected_hash = checksum(&args["expected_sha256"], "expected_sha256")?;
    let replacement = args
        .get("replace")
        .map(|options| checksum(&options["expected_sha256"], "replace.expected_sha256"))
        .transpose()?;
    let mut budget = workspace::Budget::new(4 * 1024 * 1024 * 1024);
    if fs::symlink_metadata(&source)
        .map_err(|e| e.to_string())?
        .len()
        != expected_bytes
        || workspace::hash_file(&source, &mut budget)? != (expected_bytes, expected_hash.clone())
    {
        return Err(
            "Artifact size/SHA-256 differs from the expected manifest; nothing published".into(),
        );
    }
    let complete = |already_present, replaced| {
        json!({
            "workspace_id":args["workspace_id"],"job_id":job,"path":path,"destination":destination,
            "bytes":expected_bytes,"sha256":expected_hash,"verified":true,
            "already_present":already_present,"replaced":replaced,"command_success":state["success"],
            "source_revision":context["source_revision"],"source":context["source"],
            "provenance":"verified copy of the selected workspace file; not proof of build/test success"
        })
    };
    let current = existing(&destination, &mut budget)?;
    if current.as_ref() == Some(&(expected_bytes, expected_hash.clone())) {
        return Ok(complete(true, false));
    }
    match (&current, &replacement) {
        (Some((_, hash)), Some(expected)) if hash == expected => {}
        (None, None) => {}
        (Some(_), _) => return Err("Destination exists with different content; supply replace.expected_sha256 or choose a new filename".into()),
        (None, Some(_)) => return Err("Replacement requires an existing destination".into()),
    }
    let temporary = parent.join(format!(".mcp-publish-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut input = File::open(&source)
            .map_err(|e| e.to_string())?
            .take(expected_bytes + 1);
        let mut buffer = [0u8; 65536];
        loop {
            budget.check()?;
            let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        if workspace::hash_file(&temporary, &mut budget)? != (expected_bytes, expected_hash.clone())
        {
            return Err("Artifact changed during copy; nothing published".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&source)
                .map_err(|e| e.to_string())?
                .permissions()
                .mode()
                & 0o700;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
                .map_err(|e| e.to_string())?;
        }
        if let Some(expected) = &replacement {
            if existing(&destination, &mut budget)?
                .as_ref()
                .map(|(_, hash)| hash)
                != Some(expected)
            {
                return Err("Destination changed before publication; nothing overwritten".into());
            }
            super::platform::replace(&temporary, &destination)?;
        } else {
            // Atomic no-clobber publication on the destination filesystem, including cross-volume copies.
            fs::hard_link(&temporary, &destination)
                .map_err(|e| format!("Publish without overwrite: {e}"))?;
        }
        let verified = workspace::hash_file(&destination, &mut budget).map_err(|e| {
            format!(
                "Destination was published but verification failed: {e}; inspect before retrying"
            )
        })?;
        if verified != (expected_bytes, expected_hash.clone()) {
            return Err(
                "Published destination changed during verification; inspect it before retrying"
                    .into(),
            );
        }
        Ok(complete(false, replacement.is_some()))
    })();
    if let Err(error) = fs::remove_file(&temporary) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(format!(
                "Clean publication staging file: {error}; publication result: {result:?}"
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    struct Fixture {
        root: PathBuf,
        workspace: PathBuf,
        store: store::Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("mcp-artifact-{}", uuid::Uuid::new_v4()));
            let store = store::Store::new(root.join("jobs")).unwrap();
            let args = json!({"workspace_id":"w","source_revision":"r1"});
            let created = workspace::call(&store, "create_workspace", &args, |_| panic!()).unwrap();
            let workspace = PathBuf::from(created["paths"]["root"].as_str().unwrap());
            fs::write(workspace.join("artifacts/app.exe"), b"artifact").unwrap();
            let request = json!({"session":"test","job_id":"job","executable":"unused","cwd":workspace.join("build")});
            store.create(&request, |_| Ok(())).unwrap();
            let mut metadata = store::read_json(&workspace.join("workspace.json")).unwrap();
            metadata["latest_job_id"] = json!("job");
            store::write_json(&workspace.join("workspace.json"), &metadata).unwrap();
            let job = store.directory("job").unwrap();
            store::write_json(&job.join("workspace.json"), &metadata).unwrap();
            store::write_json(
                &job.join("state.json"),
                &json!({"state":"exited","success":true,"updated_at_ms":store::now()}),
            )
            .unwrap();
            Self {
                root,
                workspace,
                store,
            }
        }
        fn args(&self) -> Value {
            json!({"session":"test","workspace_id":"w","job_id":"job","path":"artifacts/app.exe",
                "destination":self.root.join("交付 app.exe"),"expected_bytes":8,
                "expected_sha256":format!("{:x}",Sha256::digest(b"artifact"))})
        }
        fn publish(&self, args: &Value) -> Result<Value, String> {
            workspace::call(&self.store, "publish_artifact", args, |_| panic!())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn publication_checks_bytes_hashes_and_identical_retry() {
        let fixture = Fixture::new();
        let args = fixture.args();
        let destination = Path::new(args["destination"].as_str().unwrap());
        let mut wrong = args.clone();
        wrong["expected_bytes"] = json!(7);
        assert!(fixture.publish(&wrong).is_err());
        wrong = args.clone();
        wrong["expected_sha256"] = json!("0".repeat(64));
        assert!(fixture.publish(&wrong).is_err());
        assert!(!destination.exists());
        let result = fixture.publish(&args).unwrap();
        assert_eq!(result["verified"], true);
        assert_eq!(result["already_present"], false);
        assert_eq!(result["sha256"], args["expected_sha256"]);
        assert_eq!(fs::read(destination).unwrap(), b"artifact");
        assert_eq!(fixture.publish(&args).unwrap()["already_present"], true);
        assert!(!fs::read_dir(&fixture.root).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".mcp-publish-")));
    }

    #[test]
    fn replacement_requires_old_checksum_and_never_changes_conflicting_content() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        let destination = PathBuf::from(args["destination"].as_str().unwrap());
        fs::write(&destination, b"old").unwrap();
        assert!(fixture.publish(&args).is_err());
        args["replace"] = json!({"expected_sha256":"0".repeat(64)});
        assert!(fixture.publish(&args).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        args["replace"]["expected_sha256"] = json!(format!("{:x}", Sha256::digest(b"old")));
        assert_eq!(fixture.publish(&args).unwrap()["replaced"], true);
        assert_eq!(fs::read(&destination).unwrap(), b"artifact");
        assert_eq!(fixture.publish(&args).unwrap()["already_present"], true);
        fs::remove_file(destination).unwrap();
        assert!(fixture.publish(&args).is_err());
    }

    #[test]
    fn rejects_active_or_wrong_jobs_source_escapes_and_managed_destinations() {
        let fixture = Fixture::new();
        let args = fixture.args();
        let mut wrong = args.clone();
        for path in [
            "source/input",
            "artifacts/../../outside",
            "artifacts/../workspace.json",
        ] {
            wrong["path"] = json!(path);
            assert!(fixture.publish(&wrong).is_err());
        }
        wrong = args.clone();
        for path in [
            fixture.store.root.join("overwrite"),
            fixture.workspace.join("source/overwrite"),
            PathBuf::from("relative.exe"),
        ] {
            wrong["destination"] = json!(path);
            assert!(fixture.publish(&wrong).is_err());
        }
        wrong = args.clone();
        wrong["job_id"] = json!("another");
        assert!(fixture.publish(&wrong).is_err());
        fs::write(fixture.workspace.join("busy.json"), b"{}").unwrap();
        assert!(fixture.publish(&args).is_err());
        fs::remove_file(fixture.workspace.join("busy.json")).unwrap();
        let job = fixture.store.directory("job").unwrap();
        store::write_json(
            &job.join("state.json"),
            &json!({"state":"running","updated_at_ms":store::now()}),
        )
        .unwrap();
        assert!(fixture.publish(&args).is_err());
        assert!(!Path::new(args["destination"].as_str().unwrap()).exists());
    }

    #[test]
    #[cfg(unix)]
    fn symlink_destinations_never_overwrite_the_link_target() {
        let fixture = Fixture::new();
        let mut args = fixture.args();
        let outside = fixture.root.join("keep");
        fs::write(&outside, b"old").unwrap();
        std::os::unix::fs::symlink(&outside, args["destination"].as_str().unwrap()).unwrap();
        args["replace"] = json!({"expected_sha256":format!("{:x}",Sha256::digest(b"old"))});
        assert!(fixture.publish(&args).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"old");
    }
}
