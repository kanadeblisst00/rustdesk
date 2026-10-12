#[path = "main_window/lock.rs"]
mod lock;

#[cfg(unix)]
use hbb_common::config::Config;
use hbb_common::log;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

static MAIN_WINDOW: Mutex<Option<lock::InstanceLock>> = Mutex::new(None);

#[cfg(target_os = "macos")]
pub(crate) static URL_READY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(crate) fn prepare_args(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("--cm" | "multi_window")
    ) || args.iter().any(|arg| arg == "--install")
        || prepare(args.first().map(String::as_str).unwrap_or_default())
}

/// Called only for a Flutter main window, before the native runner creates it.
pub(crate) fn prepare(url: &str) -> bool {
    let mut instance = MAIN_WINDOW.lock().unwrap();
    if instance.is_some() {
        return true;
    }
    #[cfg(unix)]
    let name = {
        // Keep the lock outside the IPC directory, whose stale entries can be scrubbed.
        let directory = Config::path("");
        if directory.as_os_str().is_empty() {
            log::error!("Cannot locate the main window lock directory");
            return false;
        }
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!("Cannot create the main window lock directory: {error}");
            return false;
        }
        #[cfg(target_os = "macos")]
        let filename = "main-window.lock".to_owned();
        #[cfg(target_os = "linux")]
        let filename = {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            // D-Bus activation is per desktop session, not per Unix account.
            std::env::var_os("DBUS_SESSION_BUS_ADDRESS").hash(&mut hash);
            format!("main-window-{:x}.lock", hash.finish())
        };
        directory.join(filename)
    };
    #[cfg(windows)]
    let name = crate::get_app_name();

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match lock::InstanceLock::acquire(&name) {
            Ok(Some(guard)) => {
                // An older version may be serving activation without the lifetime lock.
                if forward(url) {
                    return false;
                }
                #[cfg(target_os = "macos")]
                if !start_url_listener() {
                    return false;
                }
                *instance = Some(guard);
                return true;
            }
            Ok(None) => {
                if forward(url) {
                    return false;
                }
            }
            Err(error) => {
                log::error!("Cannot acquire the main window lock: {error}");
                return false;
            }
        }
        if Instant::now() >= deadline {
            log::error!("The existing main window is not responding; refusing to open a duplicate");
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn forward(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    return crate::ipc::send_url_scheme(url.to_owned()).is_ok();
    #[cfg(target_os = "linux")]
    return crate::dbus::invoke_new_connection(url.to_owned()).is_ok();
    #[cfg(windows)]
    return crate::platform::send_message_to_hnwd(
        crate::platform::FLUTTER_RUNNER_WIN32_WINDOW_CLASS,
        &crate::get_app_name(),
        (winapi::um::winuser::WM_USER + 2) as _,
        url,
        url.is_empty(),
    );
}

#[cfg(target_os = "macos")]
pub(crate) fn start_url_listener() -> bool {
    static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        if let Err(error) = std::thread::Builder::new()
            .name("main-window-url".into())
            .spawn(move || crate::server::start_ipc_url_server(sender))
        {
            log::error!("Cannot start the main window URL listener: {error}");
            return false;
        }
        match receiver.recv() {
            Ok(started) => started,
            Err(error) => {
                log::error!("The main window URL listener exited during startup: {error}");
                false
            }
        }
    })
}
