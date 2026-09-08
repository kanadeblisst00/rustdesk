use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

static STOPPING: AtomicBool = AtomicBool::new(false);
static THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

pub(super) fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

pub(super) fn remember(thread: JoinHandle<()>) {
    *THREAD.lock().unwrap() = Some(thread);
}

pub(super) async fn cancelled() {
    while !stopping() {
        hbb_common::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub(super) fn shutdown() {
    {
        // Serialize with start() until its thread handle has been recorded.
        let _started = super::STARTED.lock().unwrap();
        STOPPING.store(true, Ordering::SeqCst);
    }
    let thread = THREAD.lock().unwrap().take();
    if let Some(thread) = thread {
        if thread.join().is_err() {
            hbb_common::log::error!("MCP listener thread panicked during shutdown");
        }
    }
}

#[cfg(windows)]
#[no_mangle]
pub extern "C" fn rustdesk_mcp_shutdown() {
    shutdown();
}
