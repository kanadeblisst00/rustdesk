//! Process-level harness for the production startup lock and macOS URL listener.
#![allow(dead_code, unused_imports)]
extern crate self as hbb_common;
pub use {libc, log, tokio};

use std::io::{Read, Write};
use std::path::PathBuf;

mod config {
    pub struct Config;
    impl Config {
        pub fn path(name: &str) -> std::path::PathBuf {
            super::directory().join(name)
        }
    }
}

#[path = "../../src/main_window.rs"]
mod main_window;

#[path = "../../libs/hbb_common/src/bytes_codec.rs"]
mod bytes_codec;

fn directory() -> PathBuf {
    std::env::var_os("RUSTDESK_INSTANCE_TEST_DIR")
        .unwrap()
        .into()
}

mod ipc {
    use super::bytes_codec::BytesCodec;
    use bytes::Bytes;
    use futures::{SinkExt, StreamExt};
    use serde::{Deserialize, Serialize};
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio_util::codec::Framed;

    #[derive(Serialize, Deserialize)]
    #[serde(tag = "t", content = "c")]
    pub enum Data {
        UrlLink(String),
        Other,
    }

    pub struct Connection<T>(Framed<T, BytesCodec>);
    impl<T: AsyncRead + AsyncWrite + Unpin> Connection<T> {
        pub fn new(stream: T) -> Self {
            Self(Framed::new(stream, BytesCodec::new()))
        }
        pub async fn next_timeout(&mut self, ms: u64) -> anyhow::Result<Option<Data>> {
            match tokio::time::timeout(std::time::Duration::from_millis(ms), self.0.next()).await? {
                Some(data) => Ok(Some(serde_json::from_slice(&data?)?)),
                None => anyhow::bail!("Peer closed"),
            }
        }
    }

    pub async fn new_listener(_: &str) -> anyhow::Result<parity_tokio_ipc::Incoming> {
        if std::env::var_os("RUSTDESK_INSTANCE_TEST_BIND_FAIL").is_some() {
            anyhow::bail!("Injected listener bind failure");
        }
        let path = super::directory().join("socket");
        // Tests use a private directory, never a live application's IPC directory.
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(parity_tokio_ipc::Endpoint::new(path.to_str().unwrap().to_owned()).incoming()?)
    }

    #[tokio::main(flavor = "current_thread")]
    pub async fn send_url_scheme(url: String) -> anyhow::Result<()> {
        let path = super::directory().join("socket");
        let conn = parity_tokio_ipc::Endpoint::connect(path.to_str().unwrap()).await?;
        let mut conn = Framed::new(conn, BytesCodec::new());
        conn.send(Bytes::from(serde_json::to_vec(&Data::UrlLink(url))?))
            .await?;
        Ok(())
    }
}

mod server {
    use super::ipc::Data;
    use futures::StreamExt;
    use hbb_common::{log, tokio};
    use std::{collections::HashMap, time::Duration};
    include!(concat!(env!("OUT_DIR"), "/url_server.rs"));
}

mod flutter {
    pub const APP_TYPE_MAIN: &str = "main";
    pub fn push_global_event(_: &str, event: String) -> Option<bool> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(super::directory().join("events.jsonl"))
            .unwrap();
        writeln!(file, "{event}").unwrap();
        Some(true)
    }
}

#[cfg(target_os = "linux")]
mod dbus {
    pub fn invoke_new_connection(url: String) -> anyhow::Result<()> {
        super::ipc::send_url_scheme(url)
    }
}

#[cfg(windows)]
fn get_app_name() -> String {
    // Unique per test, with a Windows-session-scoped mutex just like production.
    format!(
        "RustDeskTest{}",
        directory().file_name().unwrap().to_string_lossy()
    )
}

#[cfg(windows)]
mod platform {
    pub const FLUTTER_RUNNER_WIN32_WINDOW_CLASS: &str = "unused-in-lock-test";
    pub fn send_message_to_hnwd(_: &str, _: &str, _: usize, _: &str, _: bool) -> bool {
        false
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !main_window::prepare_args(&args) {
        println!("SECONDARY");
        return;
    }
    println!("PRIMARY");
    std::io::stdout().flush().unwrap();
    #[cfg(target_os = "macos")]
    {
        let delay = std::env::var("RUSTDESK_INSTANCE_TEST_READY_DELAY_MS")
            .unwrap_or_default()
            .parse()
            .unwrap_or(0);
        std::thread::sleep(std::time::Duration::from_millis(delay));
        main_window::URL_READY.store(true, std::sync::atomic::Ordering::Release);
    }
    let mut byte = [0u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}
