use super::{store, workspace::DIRECTORY};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn absolute_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("workspace_root must be an existing absolute directory without ..".into());
    }
    let canonical = fs::canonicalize(path).map_err(|e| format!("Resolve workspace_root: {e}"))?;
    real_directory(&canonical)?;
    // Keep ordinary drive paths usable by build tools that do not accept verbatim paths.
    #[cfg(windows)]
    if let Some(path) = canonical.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        if path.as_bytes().get(1) != Some(&b':') {
            return Err("Custom workspaces require a local drive".into());
        }
        return Ok(PathBuf::from(path));
    }
    Ok(canonical)
}

fn real_directory(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("Workspace location cannot be a reparse point".into());
        }
    }
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("Workspace location must be a real directory".into());
    }
    Ok(())
}

pub(super) fn resolve(registry: &Path) -> Result<PathBuf, String> {
    let location = registry.join("location.json");
    if !location.try_exists().map_err(|e| e.to_string())? {
        return Ok(registry.to_owned());
    }
    real_directory(registry)?;
    let record = store::read_json(&location)?;
    let path = PathBuf::from(
        record["path"]
            .as_str()
            .ok_or("Invalid workspace location")?,
    );
    if !path.is_absolute() {
        return Err("Invalid workspace location path".into());
    }
    let parent = path.parent().ok_or("Missing workspace parent")?;
    if absolute_directory(parent)? != parent {
        return Err("Workspace parent changed; refusing redirected workspace access".into());
    }
    real_directory(&path)?;
    let metadata = store::read_json(&path.join("workspace.json"))?;
    if record["location_id"].as_str().is_none()
        || record["workspace_id"].as_str() != registry.file_name().and_then(|n| n.to_str())
        || metadata["location_id"] != record["location_id"]
        || metadata["workspace_id"] != record["workspace_id"]
    {
        return Err("Workspace location ownership changed; refusing access".into());
    }
    Ok(path)
}

pub(super) fn create(root: &Path, id: &str, args: &Value) -> Result<(), String> {
    let requested = args["workspace_root"]
        .as_str()
        .ok_or("Invalid workspace_root")?;
    let parent = absolute_directory(Path::new(requested))?;
    let path = parent.join(id);
    let registry = root.join(DIRECTORY).join(id);
    if registry.try_exists().map_err(|e| e.to_string())? {
        if resolve(&registry)? != path {
            return Err(
                "Workspace ID already has another location; existing workspaces are never moved"
                    .into(),
            );
        }
        return Ok(());
    }
    let jobs = absolute_directory(root)?;
    if path.starts_with(&jobs) || jobs.starts_with(&path) {
        return Err("Custom workspace cannot overlap the job store".into());
    }
    store::private_dir(&root.join(DIRECTORY))?;
    let count = fs::read_dir(root.join(DIRECTORY))
        .map_err(|e| e.to_string())?
        .try_fold(0usize, |n, entry| entry.map(|_| n + 1))
        .map_err(|e| e.to_string())?;
    if count >= 64 {
        return Err("Workspace limit reached (64); remove unused workspaces".into());
    }
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
        .create(&path)
        .map_err(|e| format!("Create fresh custom workspace {}: {e}", path.display()))?;
    let location_id = uuid::Uuid::new_v4().to_string();
    let result = (|| {
        store::private_dir(&path)?;
        for child in ["source", "build", "artifacts", "reports"] {
            store::private_dir(&path.join(child))?;
        }
        store::write_json(
            &path.join("workspace.json"),
            &json!({
                "workspace_id":id,"source_revision":args["source_revision"],"revision_verified":false,
                "created_at_ms":store::now(),"location_id":location_id,"workspace_root":parent
            }),
        )?;
        store::private_dir(&registry)?;
        store::write_json(
            &registry.join("location.json"),
            &json!({
                "path":path,"workspace_id":id,"location_id":location_id
            }),
        )
    })();
    if let Err(error) = result {
        // Retain partially created data for inspection, never adopt or delete another directory.
        return Err(format!(
            "Custom workspace initialization failed: {error}; inspect {} before retrying",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn remove_registry(root: &Path, id: &str, removed: &Path) -> Result<(), String> {
    let registry = root.join(DIRECTORY).join(id);
    if registry != removed {
        fs::remove_file(registry.join("location.json")).map_err(|e| e.to_string())?;
        fs::remove_dir(&registry).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::workspace;
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("mcp-location-{}", uuid::Uuid::new_v4()));
            store::private_dir(&path).unwrap();
            store::private_dir(&path.join("short")).unwrap();
            Self(path)
        }
        fn store(&self) -> store::Store {
            store::Store::new(self.0.join("jobs")).unwrap()
        }
        fn request(&self) -> Value {
            json!({"workspace_id":"w","source_revision":"r1","workspace_root":self.0.join("short")})
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn custom_location_survives_store_reopen_job_finish_and_removal() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let request = fixture.request();
        let created = workspace::call(&store, "create_workspace", &request, |_| panic!()).unwrap();
        let path = absolute_directory(&fixture.0.join("short"))
            .unwrap()
            .join("w");
        assert_eq!(created["paths"]["root"], json!(path));
        let reopened = fixture.store();
        let id = json!({"workspace_id":"w","source_revision":"r1"});
        assert_eq!(
            workspace::call(&reopened, "create_workspace", &id, |_| panic!()).unwrap()["paths"],
            created["paths"]
        );
        workspace::call(&reopened, "seal_workspace", &id, |_| panic!()).unwrap();
        let run = json!({"session":"test","workspace_id":"w","job_id":"job","executable":"test"});
        workspace::call(&reopened, "run_workspace_process", &run, |_| Ok(())).unwrap();
        assert!(workspace::call(&reopened, "remove_workspace", &id, |_| panic!()).is_err());
        let job = reopened.directory("job").unwrap();
        assert_eq!(
            store::read_json(&job.join("request.json")).unwrap()["cwd"],
            json!(path.join("build"))
        );
        let mut state = json!({"state":"exited","success":true,"updated_at_ms":store::now()});
        workspace::finish(&job, &mut state).unwrap();
        assert_eq!(state["source_unchanged"], true);
        store::write_json(&job.join("state.json"), &state).unwrap();
        workspace::release(&job).unwrap();
        assert!(
            workspace::call(&reopened, "get_workspace", &id, |_| panic!()).unwrap()["lease"]
                .is_null()
        );
        workspace::call(&reopened, "remove_workspace", &id, |_| panic!()).unwrap();
        assert!(!path.exists());
        assert!(!reopened.root.join(DIRECTORY).join("w").exists());
        assert!(job.join("state.json").exists());
    }

    #[test]
    fn rejects_existing_directories_relocation_revision_conflicts_and_escaped_ids() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let mut request = fixture.request();
        let existing = fixture.0.join("short/w");
        fs::create_dir(&existing).unwrap();
        fs::write(existing.join("keep"), b"user data").unwrap();
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        assert_eq!(fs::read(existing.join("keep")).unwrap(), b"user data");
        request["workspace_id"] = json!("new");
        workspace::call(&store, "create_workspace", &request, |_| panic!()).unwrap();
        workspace::call(&store, "create_workspace", &request, |_| panic!()).unwrap();
        request["source_revision"] = json!("r2");
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        request["workspace_root"] = json!(fixture.0);
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        request["workspace_id"] = json!("../escape");
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        request["workspace_id"] = json!("fresh");
        request["workspace_root"] = json!("relative");
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        request["workspace_root"] = json!(store.root);
        assert!(workspace::call(&store, "create_workspace", &request, |_| panic!()).is_err());
        let old = json!({"workspace_id":"legacy","source_revision":"r1"});
        let created = workspace::call(&store, "create_workspace", &old, |_| panic!()).unwrap();
        assert_eq!(
            created["paths"]["root"],
            json!(store.root.join(DIRECTORY).join("legacy"))
        );
    }

    #[test]
    fn replaced_location_marker_fails_closed() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let created =
            workspace::call(&store, "create_workspace", &fixture.request(), |_| panic!()).unwrap();
        let path = PathBuf::from(created["paths"]["root"].as_str().unwrap());
        let metadata_path = path.join("workspace.json");
        let mut metadata = store::read_json(&metadata_path).unwrap();
        metadata["location_id"] = json!("another workspace");
        store::write_json(&metadata_path, &metadata).unwrap();
        assert!(
            workspace::call(&store, "remove_workspace", &fixture.request(), |_| panic!()).is_err()
        );
        assert!(path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn redirected_workspace_or_parent_cannot_delete_another_tree() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let created =
            workspace::call(&store, "create_workspace", &fixture.request(), |_| panic!()).unwrap();
        let path = PathBuf::from(created["paths"]["root"].as_str().unwrap());
        let original = path.with_file_name("original");
        let outside = fixture.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep"), b"user data").unwrap();
        fs::rename(&path, &original).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(
            workspace::call(&store, "remove_workspace", &fixture.request(), |_| panic!()).is_err()
        );
        assert!(outside.join("keep").exists());
        fs::remove_file(&path).unwrap();
        fs::rename(&original, &path).unwrap();
        let parent = path.parent().unwrap();
        let moved = parent.with_file_name("moved");
        fs::rename(parent, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, parent).unwrap();
        assert!(
            workspace::call(&store, "remove_workspace", &fixture.request(), |_| panic!()).is_err()
        );
        assert!(moved.join("w/workspace.json").exists());
    }
}
