use serde_json::{json, Value};

#[derive(Debug)]
pub(super) struct Failure(Value);

impl Failure {
    pub fn message(operation: &str, message: impl ToString) -> Self {
        Self(json!({"operation":operation,"message":message.to_string()}))
    }

    pub fn io(operation: &str, error: std::io::Error) -> Self {
        let mut failure = Self::message(operation, &error);
        failure.0["os_error"] = json!(error.raw_os_error());
        #[cfg(windows)]
        {
            failure.0["win32_error"] = json!(error.raw_os_error());
        }
        failure
    }

    #[cfg(windows)]
    pub fn windows(operation: &str, error: windows::core::Error) -> Self {
        let mut failure = Self::message(operation, &error);
        let code = error.code().0 as u32;
        failure.0["hresult"] = json!(format!("0x{code:08X}"));
        if code & 0xffff0000 == 0x80070000 {
            failure.0["win32_error"] = json!(code & 0xffff);
        }
        failure
    }

    #[cfg(windows)]
    pub fn child(mut self, process: &mut std::process::Child) -> Self {
        self.0["pid"] = json!(process.id());
        self.0["termination_requested"] = json!(true);
        match process.kill() {
            Ok(()) => match process.wait() {
                Ok(status) => self.0["exit_code"] = json!(status.code()),
                Err(error) => self.0["cleanup_error"] = json!(error.to_string()),
            },
            Err(error) => self.0["cleanup_error"] = json!(error.to_string()),
        }
        self
    }

    pub fn record(self, state: &mut Value) -> String {
        let message = self.to_string();
        for key in ["pid", "exit_code", "cleanup_error"] {
            if let Some(value) = self.0.get(key) {
                let field = match (state["phase"].as_str(), key) {
                    (Some("environment_setup"), "pid") => "setup_pid",
                    (Some("environment_setup"), "exit_code") => "setup_exit_code",
                    _ => key,
                };
                state[field] = value.clone();
            }
        }
        if self.0["termination_requested"] == true {
            state["termination_requested"] = json!(true);
            state["termination_reason"] = json!("startup_failure");
        }
        state["failure"] = self.0;
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_start_failure_never_becomes_a_main_command_exit() {
        let mut state = json!({"phase":"environment_setup","exit_code":null});
        Failure(
            json!({"operation":"AssignProcessToJobObject","message":"denied",
            "pid":42,"exit_code":1,"termination_requested":true}),
        )
        .record(&mut state);
        assert_eq!(state["setup_pid"], 42);
        assert_eq!(state["setup_exit_code"], 1);
        assert!(state["exit_code"].is_null());
        assert!(state.get("pid").is_none());
        assert_eq!(state["termination_requested"], true);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn access_denied_preserves_the_exact_api_and_hresult() {
        let mut state = json!({"exit_code":null});
        Failure::windows(
            "AssignProcessToJobObject",
            windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(5)),
        )
        .record(&mut state);
        assert_eq!(state["failure"]["operation"], "AssignProcessToJobObject");
        assert_eq!(state["failure"]["hresult"], "0x80070005");
        assert_eq!(state["failure"]["win32_error"], 5);
        assert!(state["exit_code"].is_null());
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::message("worker_launch", message)
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::message("worker_launch", message)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.0["operation"].as_str().unwrap_or("process"),
            self.0["message"].as_str().unwrap_or("Unknown failure")
        )
    }
}

pub(super) fn exit_status(state: &mut Value, status: std::process::ExitStatus) {
    state["exit_code"] = json!(status.code());
    #[cfg(windows)]
    if let Some(code) = status.code() {
        state["exit_code_hex"] = json!(format!("0x{:08X}", code as u32));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        state["signal"] = json!(status.signal());
    }
}

pub(super) fn finish(state: &mut Value) {
    if state["state"] == "failed" {
        state["failure_stage"] = state["phase"].clone();
    }
    if state["termination_reason"].is_null() {
        state["termination_reason"] = json!(match state["state"].as_str() {
            Some("exited") => "process_exit",
            Some("cancelled") => "cancelled_before_start",
            _ => "worker_failure",
        });
    }
}
