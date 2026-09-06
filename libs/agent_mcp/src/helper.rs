use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Run an embedded helper with bounded output and a hard process deadline.
/// Input is piped as data, never interpolated into a command or script.
pub fn run(command: &mut Command, input: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    run_checked(command, input, timeout, || true)
}

pub fn run_checked(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    active: impl Fn() -> bool,
) -> Result<Vec<u8>, String> {
    const MAX_OUTPUT: u64 = 2 * 1024 * 1024;
    if !active() {
        return Err("Automation request was cancelled before execution".into());
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Unable to start automation helper: {e}"))?;
    let result = thread::scope(|scope| {
        let mut stdin = child.stdin.take().ok_or("Helper stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("Helper stdout unavailable")?;
        let writer = scope.spawn(move || stdin.write_all(input));
        let reader = scope.spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAX_OUTPUT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            if !active() {
                break Err("Automation request cancelled; action outcome may be unknown. Do not retry automatically".to_owned());
            }
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Ok(None) => break Err("Automation helper timed out; action outcome may be unknown. Do not retry actions automatically".to_owned()),
                Err(e) => break Err(format!("Unable to wait for automation helper: {e}")),
            }
        };
        if status.is_err() {
            // Reap before joining pipe threads, including on timeouts.
            let killed = child.kill();
            let waited = child.wait();
            if let Err(e) = waited {
                return Err(format!(
                    "Unable to reap helper: {e}; kill result: {killed:?}"
                ));
            }
        }
        let written = writer.join().map_err(|_| "Helper input thread failed")?;
        let output = reader
            .join()
            .map_err(|_| "Helper output thread failed")?
            .map_err(|e| format!("Unable to read helper output: {e}"))?;
        let status = status?;
        written.map_err(|e| format!("Unable to send helper input: {e}"))?;
        if output.len() as u64 > MAX_OUTPUT {
            return Err("Automation helper output exceeds 2 MiB".into());
        }
        if !status.success() {
            return Err(format!(
                "Automation helper exited with {status}; verify its runtime and dependencies"
            ));
        }
        Ok(output)
    });
    result
}
