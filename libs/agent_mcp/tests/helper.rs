use rustdesk_agent_mcp::helper;
use std::{
    io::{Read, Write},
    process::Command,
    time::{Duration, Instant},
};

// The child is this test executable, so the watchdog tests need no external runtime.
#[test]
fn helper_child() {
    let Ok(mode) = std::env::var("RUSTDESK_HELPER_TEST") else {
        return;
    };
    match mode.as_str() {
        "echo" => {
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            std::io::stdout().write_all(&input).unwrap();
        }
        "sleep" => std::thread::sleep(Duration::from_secs(10)),
        "large" => std::io::stdout()
            .write_all(&vec![b'x'; 3 * 1024 * 1024])
            .unwrap(),
        _ => std::process::exit(5),
    }
}

fn child(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "helper_child", "--nocapture"])
        .env("RUSTDESK_HELPER_TEST", mode);
    command
}

#[test]
fn pipes_input_as_data_and_enforces_deadlines_and_output_limits() {
    let output = helper::run(
        &mut child("echo"),
        "保存 $(echo unsafe) `not code`".as_bytes(),
        Duration::from_secs(3),
    )
    .unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("保存 $(echo unsafe) `not code`"));
    let started = Instant::now();
    let error = helper::run(
        &mut child("sleep"),
        &vec![0; 1024 * 1024],
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(helper::run(&mut child("large"), &[], Duration::from_secs(1)).is_err());
    assert!(helper::run(&mut child("failure"), &[], Duration::from_secs(1)).is_err());
}

#[test]
fn cancellation_reaps_a_running_child_and_prevents_start_after_revocation() {
    let started = Instant::now();
    let error = helper::run_checked(&mut child("sleep"), &[], Duration::from_secs(10), || {
        started.elapsed() < Duration::from_millis(100)
    })
    .unwrap_err();
    assert!(error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(3));
    let mut missing = Command::new("this-program-does-not-exist");
    assert!(
        helper::run_checked(&mut missing, &[], Duration::from_secs(1), || false)
            .unwrap_err()
            .contains("before execution")
    );
}
