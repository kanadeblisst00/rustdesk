//! Private inherited pipes: no local listener, discovery file, or shared UIA output channel.
use crate::ToolResult;
use serde_json::{json, Map, Value};
use std::{
    io::{Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

pub const VERSION: u64 = 1;
const MAX_REQUEST: usize = 300 * 1024;
const MAX_RESPONSE: usize = 24 * 1024 * 1024;

fn read_frame(input: &mut impl Read, limit: usize) -> Result<Value, String> {
    let mut size = [0; 4];
    input
        .read_exact(&mut size)
        .map_err(|e| format!("Local agent pipe closed: {e}"))?;
    let size = u32::from_le_bytes(size) as usize;
    if size == 0 || size > limit {
        return Err("Local agent frame exceeds its size limit".into());
    }
    let mut data = vec![0; size];
    input
        .read_exact(&mut data)
        .map_err(|e| format!("Local agent frame incomplete: {e}"))?;
    serde_json::from_slice(&data).map_err(|_| "Invalid local agent frame".into())
}

fn write_frame(output: &mut impl Write, value: &Value, limit: usize) -> Result<(), String> {
    let data = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if data.len() > limit {
        return Err("Local agent frame exceeds its size limit".into());
    }
    output
        .write_all(&(data.len() as u32).to_le_bytes())
        .and_then(|_| output.write_all(&data))
        .and_then(|_| output.flush())
        .map_err(|e| format!("Local agent pipe write failed: {e}"))
}

pub struct Client {
    child: Mutex<Option<Child>>,
    input: Mutex<ChildStdin>,
    responses: Mutex<mpsc::Receiver<ToolResult>>,
    stopped: AtomicBool,
}

impl Client {
    pub fn spawn(
        command: &mut Command,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start local agent: {e}"))?;
        let pipes = child.stdin.take().zip(child.stdout.take());
        let Some((input, mut output)) = pipes else {
            let killed = child.kill();
            child
                .wait()
                .map_err(|e| format!("Cannot reap local agent: {e}; {killed:?}"))?;
            return Err("Local agent pipes unavailable".into());
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::Builder::new()
            .name("mcp-local-reader".into())
            .spawn(move || loop {
                let response = read_frame(&mut output, MAX_RESPONSE);
                let failed = response.is_err();
                if tx.send(response).is_err() || failed {
                    break;
                }
            });
        if let Err(e) = reader {
            let killed = child.kill();
            child
                .wait()
                .map_err(|error| format!("Cannot reap local agent: {error}; {killed:?}"))?;
            return Err(format!("Cannot start local agent reader: {e}"));
        }
        Ok(Self {
            child: Mutex::new(Some(child)),
            input: Mutex::new(input),
            responses: Mutex::new(rx),
            stopped: AtomicBool::new(false),
        })
    }

    pub fn call(
        &self,
        name: &str,
        args: &Map<String, Value>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> ToolResult {
        check()?;
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Local agent stopped; reconnect and observe".into());
        }
        let responses = self.responses.lock().unwrap();
        check()?;
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Local agent stopped; reconnect and observe".into());
        }
        write_frame(
            &mut *self.input.lock().unwrap(),
            &json!({"version":VERSION,"name":name,"arguments":args,"timeout_ms":10000}),
            MAX_REQUEST,
        )?;
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if let Err(e) = check() {
                let stopped = self.stop();
                return Err(format!(
                    "{e}; local action outcome may be unknown; stop: {stopped:?}"
                ));
            }
            match responses.recv_timeout(Duration::from_millis(20)) {
                Ok(response) => {
                    let response = response?;
                    if response["version"].as_u64() != Some(VERSION) {
                        self.stop()?;
                        return Err("Local agent protocol mismatch".into());
                    }
                    if let Some(error) = response["error"].as_str() {
                        return Err(error.into());
                    }
                    return response
                        .get("result")
                        .cloned()
                        .ok_or_else(|| "Local agent response missing result".into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.stop()?;
                    return Err("Local agent exited; action outcome unknown; do not replay".into());
                }
                Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => {
                    self.stop()?;
                    return Err(
                        "Local agent timed out; action outcome unknown; do not replay".into(),
                    );
                }
            }
        }
    }

    pub fn stop(&self) -> Result<(), String> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        write_frame(
            &mut *self.input.lock().unwrap(),
            &json!({"version":VERSION,"name":"stop"}),
            MAX_REQUEST,
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Local agent stop: {e}");
        }
        let child = Arc::new(Mutex::new(self.child.get_mut().unwrap().take()));
        let pending = child.clone();
        let reaper = std::thread::Builder::new()
            .name("mcp-local-reaper".into())
            .spawn(move || {
                if let Some(mut child) = pending.lock().unwrap().take() {
                    let deadline = Instant::now() + Duration::from_millis(500);
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => return,
                            Ok(None) if Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(10))
                            }
                            _ => break,
                        }
                    }
                    reap(&mut child);
                }
            });
        if let Err(e) = reaper {
            eprintln!("Local agent reaper thread: {e}");
            if let Some(mut child) = child.lock().unwrap().take() {
                reap(&mut child);
            }
        }
    }
}

fn reap(child: &mut Child) {
    if let Err(e) = child.kill() {
        eprintln!("Local agent kill: {e}");
    }
    if let Err(e) = child.wait() {
        eprintln!("Local agent reap: {e}");
    }
}

pub fn run(
    handler: impl Fn(&str, &Map<String, Value>, &dyn Fn() -> Result<(), String>) -> ToolResult
        + Send
        + 'static,
) -> Result<(), String> {
    let stopped = Arc::new(AtomicBool::new(false));
    let cancelled = stopped.clone();
    let (tx, rx) = mpsc::sync_channel::<Value>(1);
    let worker = std::thread::Builder::new().name("mcp-local-desktop".into()).spawn(move || -> Result<(), String> {
        let mut output = std::io::stdout().lock();
        for message in rx {
            let deadline = Instant::now() + Duration::from_millis(message["timeout_ms"].as_u64().unwrap_or(0).min(10000));
            let check = || {
                if cancelled.load(Ordering::SeqCst) || Instant::now() >= deadline { Err("Local operation cancelled or expired; observe before retrying".into()) } else { Ok(()) }
            };
            let result = (|| {
                check()?;
                let name = message["name"].as_str().ok_or("Missing local operation")?;
                if !matches!(name, "status" | "screenshot") && !super::input_tool(name) { return Err("Unsupported local operation".into()); }
                let args = message["arguments"].as_object().ok_or("Invalid local arguments")?;
                handler(name, args, &check)
            })();
            let response = match result { Ok(value) => json!({"version":VERSION,"result":value}), Err(error) => json!({"version":VERSION,"error":error}) };
            if serde_json::to_vec(&response).map_err(|e| e.to_string())?.len() > MAX_RESPONSE {
                write_frame(&mut output, &json!({"version":VERSION,"error":"Local screenshot exceeds 24 MiB response limit"}), MAX_RESPONSE)?;
            } else { write_frame(&mut output, &response, MAX_RESPONSE)?; }
        }
        Ok(())
    }).map_err(|e| e.to_string())?;
    let mut input = std::io::stdin().lock();
    let result = loop {
        let message = match read_frame(&mut input, MAX_REQUEST) {
            Ok(message) => message,
            Err(e) => break Err(e),
        };
        if message["version"].as_u64() != Some(VERSION) {
            break Err("Local agent protocol mismatch".into());
        }
        if message["name"] == "stop" {
            break Ok(());
        }
        if tx.try_send(message).is_err() {
            break Err("Local agent queue is full".into());
        }
    };
    stopped.store(true, Ordering::SeqCst);
    drop(tx);
    worker.join().map_err(|_| "Local agent worker failed")??;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_frames_reject_sizes_before_allocation_and_truncated_payloads() {
        assert!(read_frame(
            &mut (MAX_RESPONSE as u32 + 1).to_le_bytes().as_slice(),
            MAX_RESPONSE
        )
        .is_err());
        assert!(read_frame(&mut [0, 0, 0, 0].as_slice(), MAX_RESPONSE).is_err());
        assert!(read_frame(&mut [3, 0, 0, 0, b'{'].as_slice(), MAX_RESPONSE).is_err());
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &json!({"name":"status"}), MAX_REQUEST).unwrap();
        assert_eq!(
            read_frame(&mut bytes.as_slice(), MAX_REQUEST).unwrap()["name"],
            "status"
        );
    }
}
