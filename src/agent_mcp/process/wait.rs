use super::store::{self, Store};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub(super) fn call(store: &Store, args: &Value) -> Result<Value, String> {
    let id = args["job_id"].as_str().ok_or("Missing job_id")?;
    let started = Instant::now();
    let timeout = Duration::from_millis(args["timeout_ms"].as_u64().unwrap_or(1000).min(10000));
    let mut job = store.status(id)?;
    let after_state = args.get("after_state").unwrap_or(&job["state"]).clone();
    let after_phase = args.get("after_phase").unwrap_or(&job["phase"]).clone();
    let event = loop {
        if store::terminal(&job) {
            break "completed";
        }
        if job["state"] == "unknown" {
            break "unknown";
        }
        if job["state"] != after_state {
            break "state_changed";
        }
        if job["phase"] != after_phase {
            break "phase_changed";
        }
        if ["stdout", "stderr", "setup"].iter().any(|stream| {
            job[format!("{stream}_bytes")].as_u64().unwrap_or(0)
                > args[format!("{stream}_offset")].as_u64().unwrap_or(0)
        }) {
            break "output";
        }
        if started.elapsed() >= timeout {
            break "timeout";
        }
        std::thread::sleep(
            Duration::from_millis(50).min(timeout.saturating_sub(started.elapsed())),
        );
        job = store.status(id)?;
    };
    let mut output = json!({});
    for stream in ["stdout", "stderr", "setup"] {
        output[stream] = store.call(
            "read_process_output",
            &json!({
                "job_id":id,"stream":stream,
                "offset":args[format!("{stream}_offset")].as_u64().unwrap_or(0),
                "max_bytes":args["max_bytes"].as_u64().unwrap_or(16384).clamp(1,32768),
                "encoding":args["encoding"].as_str().unwrap_or("auto")
            }),
        )?;
    }
    Ok(
        json!({"job":job,"event":event,"waited_ms":started.elapsed().as_millis() as u64,"output":output}),
    )
}
