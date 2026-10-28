//! Runs the production status loop with a controllable IPC transport.
#![allow(dead_code)]
use serde::Serialize;
use std::{collections::HashMap, sync::{LazyLock, Mutex}, time::{Duration, Instant}};
use tokio::{sync::mpsc, time};

macro_rules! allow_err {
    ($value:expr) => { if let Err(error) = $value { eprintln!("{error}"); } };
}

// @PRODUCTION_STATUS@

static UI_STATUS: LazyLock<Mutex<UiStatus>> = LazyLock::new(|| Mutex::new(UiStatus {
    status_num: 0, ipc_status: "connecting", mouse_time: 0, video_conn_count: 0,
}));
static OPTIONS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
static OPTION_SYNCED: Mutex<bool> = Mutex::new(false);
static TEMPORARY_PASSWD: Mutex<String> = Mutex::new(String::new());
static IS_REMOTE_MODIFY_ENABLED_BY_CONTROL_PERMISSIONS: Mutex<Option<bool>> = Mutex::new(None);
#[cfg(windows)]
static IS_FILE_TRANSFER_ENABLED: Mutex<Option<bool>> = Mutex::new(None);

mod common { pub fn is_cm() -> bool { false } }
mod ui_cm_interface { pub fn quit_cm() { panic!("not a CM fixture"); } }
#[cfg(windows)]
mod clipboard {
    pub struct ContextSend;
    impl ContextSend { pub fn enable(_: bool) {} }
}
fn rustdesk_interval(interval: time::Interval) -> time::Interval { interval }
async fn sleep(seconds: f32) { time::sleep(Duration::from_secs_f32(seconds)).await; }

mod ipc {
    use super::*;
    #[derive(Debug)]
    pub enum Data {
        MouseMoveTime(i64), Options(Option<HashMap<String, String>>),
        Config((String, Option<String>)), VideoConnCount(Option<usize>),
        OnlineStatus(Option<(i64, bool)>), ControlPermissionsRemoteModify(Option<bool>),
        #[cfg(windows)]
        FileTransferEnabledState(Option<bool>),
    }
    #[derive(Debug)]
    pub struct Connection(pub mpsc::UnboundedReceiver<Data>);
    pub static CONNECTIONS: LazyLock<(mpsc::UnboundedSender<Result<Connection, String>>,
        tokio::sync::Mutex<mpsc::UnboundedReceiver<Result<Connection, String>>>)> = LazyLock::new(|| {
        let (tx, rx) = mpsc::unbounded_channel();
        (tx, tokio::sync::Mutex::new(rx))
    });
    pub async fn connect(_: u64, _: &str) -> Result<Connection, String> {
        CONNECTIONS.1.lock().await.recv().await.unwrap()
    }
    impl Connection {
        pub async fn next(&mut self) -> Result<Option<Data>, String> {
            self.0.recv().await.map(Some).ok_or("fixture disconnect".into())
        }
        pub async fn send(&mut self, _: &Data) -> Result<(), String> { Ok(()) }
    }
}

// @PRODUCTION_LOOP@

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "status transition timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn connect(password: &str, online: i64) -> mpsc::UnboundedSender<ipc::Data> {
    let (tx, rx) = mpsc::unbounded_channel();
    ipc::CONNECTIONS.0.send(Ok(ipc::Connection(rx))).unwrap();
    tx.send(ipc::Data::Options(Some(HashMap::new()))).unwrap();
    tx.send(ipc::Data::Config(("temporary-password".into(), Some(password.into())))).unwrap();
    tx.send(ipc::Data::OnlineStatus(Some((online, false)))).unwrap();
    wait_for(|| UI_STATUS.lock().unwrap().ipc_status == "connected");
    tx
}

fn main() {
    assert_eq!(UI_STATUS.lock().unwrap().ipc_status, "connecting");
    let (_tx, rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || check_connect_status_(true, rx));
    ipc::CONNECTIONS.0.send(Err("fixture service absent".into())).unwrap();
    wait_for(|| UI_STATUS.lock().unwrap().ipc_status == "disconnected");
    assert!(!*OPTION_SYNCED.lock().unwrap());
    let peer = connect("test-password-one", -1);
    assert_eq!(UI_STATUS.lock().unwrap().status_num, -1);
    assert_eq!(&*TEMPORARY_PASSWD.lock().unwrap(), "test-password-one");
    assert!(*OPTION_SYNCED.lock().unwrap());
    let json = serde_json::to_value(&*UI_STATUS.lock().unwrap()).unwrap();
    assert_eq!(json["ipc_status"], "connected");
    drop(peer);
    wait_for(|| TEMPORARY_PASSWD.lock().unwrap().is_empty());
    assert!(!*OPTION_SYNCED.lock().unwrap());
    assert_eq!(UI_STATUS.lock().unwrap().ipc_status, "disconnected");
    let _peer = connect("test-password-two", 42);
    assert_eq!(UI_STATUS.lock().unwrap().status_num, 1);
    assert_eq!(&*TEMPORARY_PASSWD.lock().unwrap(), "test-password-two");
    assert!(*OPTION_SYNCED.lock().unwrap());
    let cases: Vec<_> = [
        "initial connect failure",
        "network down with live IPC",
        "disconnect clears cached password and options state",
        "reconnect refreshes status and password",
    ].into_iter().map(|name| serde_json::json!({"name": name, "status": "passed"})).collect();
    println!("{}", serde_json::json!({
        "total": cases.len(), "passed": cases.len(), "failed": 0, "skipped": 0, "cases": cases,
    }));
}
