use serde_json::{json, Value};
use std::{collections::BTreeMap, path::{Path, PathBuf}};

#[cfg(windows)]
#[path = "preflight_windows.rs"]
mod windows;

fn expand(value: &str, vars: &BTreeMap<String,String>) -> Result<PathBuf,String> {
    let mut result = String::new();
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after = &rest[start+1..];
        let end = after.find('%').ok_or("Unterminated root environment reference")?;
        let name = after[..end].to_ascii_uppercase();
        result.push_str(vars.get(&name).ok_or("Unresolved root environment reference")?);
        rest = &after[end+1..];
    }
    result.push_str(rest);
    let path = PathBuf::from(result);
    if !path.is_absolute() || path.components().any(|p| p == std::path::Component::ParentDir) {
        return Err("Root must be absolute without parent traversal".into());
    }
    Ok(path)
}

#[cfg(unix)]
fn writable(path: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    if unsafe { hbb_common::libc::getuid() != hbb_common::libc::geteuid()
        || hbb_common::libc::getgid() != hbb_common::libc::getegid() } {
        return json!({"writable":null,"method":"identity mismatch; no write performed"});
    }
    let path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(path) => path,
        Err(_) => return json!({"writable":false,"error":"Path contains NUL"}),
    };
    let result = unsafe { hbb_common::libc::access(path.as_ptr(), hbb_common::libc::W_OK | hbb_common::libc::X_OK) };
    json!({"writable":result == 0,"method":"access(W_OK|X_OK); no file created",
        "os_error":if result == 0 {None} else {std::io::Error::last_os_error().raw_os_error()}})
}

fn directory(path: &Path) -> Value {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_dir() => return json!({"exists":true,"directory":false,"writable":false,"error":"Root is not a directory"}),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return json!({"exists":false,"writable":false}),
        Err(error) => return json!({"exists":null,"writable":null,"os_error":error.raw_os_error(),"error":"Root metadata unavailable"}),
        _ => {}
    }
    #[cfg(windows)]
    let access = windows::writable(path);
    #[cfg(not(windows))]
    let access = writable(path);
    let mut result = access;
    result["exists"] = json!(true);
    result["directory"] = json!(true);
    result["write_performed"] = json!(false);
    result
}

fn root(name: &str, vars: &BTreeMap<String,String>, home: &Path) -> Value {
    let mut scopes = json!({});
    #[cfg(windows)]
    for scope in ["machine","user"] {
        scopes[scope] = match windows::variable(name, scope == "machine") {
            Ok(value) => json!({"value":value}),
            Err(error) => json!({"error":error}),
        };
    }
    scopes["process"] = match std::env::var_os(name).map(|v|v.into_string()) {
        Some(Err(_)) => json!({"error":"Explicit process root is not valid Unicode"}),
        value => json!({"value":value.and_then(Result::ok)}),
    };
    scopes["authorized_environment"] = json!({"value":vars.get(name)});
    let order = if cfg!(windows) {vec!["machine","user","authorized_environment","process"]} else {vec!["authorized_environment","process"]};
    let mut selected = None;
    for scope in order {
        if scopes[scope].get("error").is_some() {
            return json!({"scopes":scopes,"valid":false,"requires_review":true,"error":"Root scope could not be observed; no fallback selected"});
        }
        if let Some(value) = scopes[scope]["value"].as_str().filter(|v| !v.is_empty()) {
            selected = Some((scope, value.to_owned()));
            break;
        }
    }
    let (scope, path, explicit) = match selected {
        Some((scope,value)) => match expand(&value,vars) {
            Ok(path) => (scope, path, true),
            Err(error) => return json!({"scopes":scopes,"valid":false,"requires_review":true,"error":error}),
        },
        None => {
            let path = if name == "CODEX_DEPLOYMENT_ROOT" { home.join("CodexDeployments") }
                else if cfg!(windows) {
                    let primary = PathBuf::from("D:/Temp");
                    let observed = directory(&primary);
                    let access = if observed["exists"] == false { directory(Path::new("D:/")) } else { observed };
                    if access["writable"] == true {primary}
                    else if access["exists"] == false || access["writable"] == false {PathBuf::from("C:/Temp")}
                    else { return json!({"scopes":scopes,"valid":false,"requires_review":true,"error":"Default D: root access is unknown; no fallback selected"}); }
                } else {home.join("codex-builds")};
            ("default", path, false)
        }
    };
    let observation = directory(&path);
    let valid = observation["exists"] == true && observation["writable"] == true;
    let parent = path.ancestors().skip(1).find(|p| p.is_dir()).map(|p| json!({"path":p,"access":directory(p)}));
    json!({"scopes":scopes,"selected_scope":scope,"path":path,"explicit":explicit,
        "valid":valid,"requires_review":explicit && !valid,"directory":observation,
        "existing_parent":parent,"needs_creation":!explicit && !path.exists(),
        "note":"Read-only access observation; no directory or test file was created. Runtime writes can still fail."})
}

pub(super) fn observe(vars: &BTreeMap<String,String>, args: &Value) -> Value {
    let home = vars.get(if cfg!(windows) {"USERPROFILE"} else {"HOME"}).map(PathBuf::from);
    let roots = match home.filter(|home| home.is_absolute()) {
        Some(home) => json!({"build":root("CODEX_BUILD_ROOT",vars,&home),"deployment":root("CODEX_DEPLOYMENT_ROOT",vars,&home)}),
        None => json!({"error":"Authorized user home unavailable; no default root guessed"}),
    };
    let mut required = Vec::new();
    if let Some(paths) = args["required_paths"].as_array() {
        for item in paths {
            let path = item["path"].as_str().map(PathBuf::from);
            let mut observation = json!({"id":item["id"],"contents_read":false});
            match path.filter(|p|p.is_absolute()) {
                None => { observation["exists"] = Value::Null; observation["error"] = json!("An absolute path is required"); }
                Some(path) => match std::fs::metadata(path) {
                    Ok(meta) => {
                        observation["exists"] = json!(true);
                        observation["kind_matches"] = json!(if item["kind"] == "directory" {meta.is_dir()} else {meta.is_file()});
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => observation["exists"] = json!(false),
                    Err(error) => { observation["exists"] = Value::Null; observation["os_error"] = json!(error.raw_os_error()); }
                }
            }
            required.push(observation);
        }
    }
    json!({"roots":roots,"required_paths":required,"toolchain_usable":null,
        "version_checks":"Use explicit probe_environment jobs, then inspect their exit codes and bounded output"})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_expansion_rejects_missing_variables_and_does_not_create_directories() {
        let vars = BTreeMap::from([("USERPROFILE".into(),std::env::temp_dir().to_string_lossy().into_owned())]);
        assert!(expand("%MISSING%/build",&vars).is_err());
        assert!(expand("relative",&vars).is_err());
        let path = expand(&format!("%USERPROFILE%/mcp-absent-{}",uuid::Uuid::new_v4()),&vars).unwrap();
        assert!(!directory(&path)["exists"].as_bool().unwrap());
        assert!(!path.exists());
    }
}
