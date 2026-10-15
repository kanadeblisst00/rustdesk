"""Exercise production terminal attachment and event delivery without Dart/audio/video SDKs."""
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


def function(source, signature):
    start = source.index(signature)
    body = source.index('{', start)
    depth = 1
    end = body + 1
    while depth:
        depth += (source[end] == '{') - (source[end] == '}')
        end += 1
    return source[start:end]


class TerminalUiTest(unittest.TestCase):
    def test_production_attachment_and_event_delivery(self):
        cargo = shutil.which('cargo')
        if not cargo:
            self.skipTest('cargo is required')
        flutter = (ROOT / 'src/flutter.rs').read_text()
        terminal = (ROOT / 'src/agent_mcp/terminal.rs').read_text()
        state = ROOT / 'src/agent_mcp/terminal/initialization.rs'
        lockfile = (ROOT / 'libs/agent_mcp/Cargo.lock').read_text()
        versions = {
            name: re.search(r'\[\[package\]\]\nname = "' + name + r'"\nversion = "([^"]+)"',
                            lockfile).group(1)
            for name in ('serde', 'serde_json')
        }
        with tempfile.TemporaryDirectory(prefix='rustdesk-terminal-ui-') as directory:
            path = Path(directory)
            (path / 'Cargo.toml').write_text('''[package]
name = "rustdesk-terminal-ui-check"
version = "0.1.0"
edition = "2021"
[workspace]
[features]
mcp = []
[dependencies]
serde = "=SERDE_VERSION"
serde_json = "=SERDE_JSON_VERSION"
[lib]
path = "lib.rs"
'''.replace('SERDE_VERSION', versions['serde'])
               .replace('SERDE_JSON_VERSION', versions['serde_json']))
            source = FIXTURE.replace('INITIALIZATION_PATH', state.as_posix())
            source = source.replace('INITIALIZE_UI', function(terminal, 'pub(crate) fn initialize_ui('))
            source = source.replace('PUSH_EVENT', function(flutter, '    pub fn push_event_<V>('))
            source = source.replace('SESSION_START', function(flutter, 'pub fn session_start_('))
            source = source.replace('CLOSE_EVENT', function(flutter, 'fn try_send_close_event('))
            source = source.replace('INSERT_SESSION', function(flutter, '    pub fn insert_session(')
                                    .replace('SESSIONS', 'registry()'))
            (path / 'lib.rs').write_text(source)
            for features in (['--features', 'mcp'], []):
                result = subprocess.run(
                    [cargo, 'test', '--offline', '--manifest-path', str(path / 'Cargo.toml'),
                     *features, '--', '--test-threads=1'],
                    cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    timeout=120)
                self.assertEqual(result.returncode, 0, result.stdout)
                print(result.stdout)


FIXTURE = r'''
#![allow(dead_code, unused_imports, non_camel_case_types)]
extern crate self as flutter_rust_bridge;
extern crate self as log;
#[macro_export] macro_rules! info { ($($arg:tt)*) => {}; }
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::{Arc, Mutex, RwLock, OnceLock, atomic::{AtomicBool, Ordering}}};
type ResultType<T> = Result<T, String>;
macro_rules! bail { ($($arg:tt)*) => { return Err(format!($($arg)*)) }; }
pub struct StreamSink<T>(Arc<Mutex<Vec<T>>>);
impl<T> Clone for StreamSink<T> { fn clone(&self) -> Self { Self(self.0.clone()) } }
impl<T> Default for StreamSink<T> { fn default() -> Self { Self(Default::default()) } }
impl<T> StreamSink<T> { fn add(&self, event: T) { self.0.lock().unwrap().push(event); } }
mod flutter_ffi {
    pub type SessionID = u64;
    pub enum EventToUI { Event(String) }
}
use flutter_ffi::{SessionID, EventToUI};
#[derive(Clone, Copy, Default, Eq, Hash, PartialEq)]
enum ConnType { #[default] DEFAULT_CONN, TERMINAL, FILE_TRANSFER }
#[derive(Default)] struct Login { id: String, kind: ConnType, peer_info: Option<()> }
#[derive(Default)] struct Round { connected: bool }
impl Round {
    fn is_connected(&self) -> bool { self.connected }
    fn new_round(&mut self) -> u32 { self.connected = false; 1 }
}
struct Sender { closed: bool }
impl Sender { fn is_closed(&self) -> bool { self.closed } }
#[derive(Default)] struct Session {
    ui_handler: flutter::FlutterHandler,
    lc: RwLock<Login>,
    connection_round_state: Mutex<Round>,
    sender: RwLock<Option<Sender>>,
    use_texture_render: AtomicBool,
}
impl Session {
    fn is_terminal(&self) -> bool { self.lc.read().unwrap().kind == ConnType::TERMINAL }
    fn get_id(&self) -> String { self.lc.read().unwrap().id.clone() }
}
impl std::ops::Deref for Session {
    type Target = flutter::FlutterHandler;
    fn deref(&self) -> &Self::Target { &self.ui_handler }
}
fn io_loop(_: Session, _: u32) { panic!("an existing transport must not start a second loop"); }
impl Clone for Session { fn clone(&self) -> Self { panic!("an existing transport must be reused"); } }
mod agent_mcp {
    use super::*;
    pub fn event(_: SessionID, _: &str, _: &impl Serialize) {}
    pub(crate) mod terminal {
        use super::*;
        #[path = "INITIALIZATION_PATH"] pub(crate) mod initialization;
        INITIALIZE_UI
    }
}
mod flutter {
    use super::*;
    pub type FlutterSession = Arc<Session>;
    #[derive(Default)] struct SessionHandler { event_stream: Option<StreamSink<EventToUI>> }
    #[derive(Default)] pub struct FlutterHandler {
        session_handlers: RwLock<HashMap<SessionID, SessionHandler>>,
        #[cfg(feature = "mcp")]
        terminal_ui: Arc<crate::agent_mcp::terminal::initialization::State>,
    }
    impl FlutterHandler { PUSH_EVENT }
    SESSION_START
    CLOSE_EVENT
    mod sessions {
        use super::*;
        static SESSIONS: OnceLock<RwLock<HashMap<(String, ConnType), FlutterSession>>> = OnceLock::new();
        fn registry() -> &'static RwLock<HashMap<(String, ConnType), FlutterSession>> { SESSIONS.get_or_init(Default::default) }
        INSERT_SESSION
        fn update_session_count_to_server() {}
        pub fn clear() { registry().write().unwrap().clear(); }
        pub fn get_sessions() -> Vec<FlutterSession> { registry().read().unwrap().values().cloned().collect() }
        pub fn get_session_by_session_id(id: &SessionID) -> Option<FlutterSession> {
            get_sessions().into_iter().find(|s| s.session_handlers.read().unwrap().contains_key(id))
        }
    }
    #[cfg(test)] mod tests {
        use super::*;
        fn peer(kind: ConnType, id: &str) -> FlutterSession {
            sessions::clear();
            let s = Arc::new(Session::default());
            *s.lc.write().unwrap() = Login { id: id.into(), kind, peer_info: Some(()) };
            s.connection_round_state.lock().unwrap().connected = true;
            *s.sender.write().unwrap() = Some(Sender { closed: false });
            sessions::insert_session(1, kind, s.clone());
            s
        }
        fn emit(s: &FlutterSession, name: &str, event: &[(&str, &str)]) {
            s.push_event_(name, event, &[], &[]);
        }
        fn authenticated(s: &FlutterSession) {
            emit(s, "connection_ready", &[("secure", "true"), ("direct", "false"), ("stream_type", "tcp")]);
            emit(s, "permission", &[("keyboard", "false")]);
            emit(s, "peer_info", &[("platform", "Windows"), ("hostname", "fixture"), ("version", "1.5.0")]);
        }
        fn attach(s: &FlutterSession, id: SessionID) -> StreamSink<EventToUI> {
            let candidate = Arc::new(Session::default());
            let kind = s.lc.read().unwrap().kind;
            *candidate.lc.write().unwrap() = Login { id: s.get_id(), kind, peer_info: None };
            sessions::insert_session(id, kind, candidate);
            let stream = StreamSink::default();
            session_start_(&id, &s.get_id(), stream.clone()).unwrap();
            stream
        }
        fn events(stream: &StreamSink<EventToUI>) -> Vec<Value> {
            stream.0.lock().unwrap().iter().map(|EventToUI::Event(event)| serde_json::from_str(event).unwrap()).collect()
        }
        #[cfg(feature = "mcp")]
        #[test] fn attaches_gui_after_headless_authentication_without_restarting_transport() {
            let s = peer(ConnType::TERMINAL, "late-ui");
            let existing = StreamSink::default();
            s.session_handlers.write().unwrap().get_mut(&1).unwrap().event_stream = Some(existing.clone());
            authenticated(&s);
            let before = events(&existing);
            let stream = attach(&s, 2);
            let received = events(&stream);
            assert_eq!(received.iter().map(|e| e["name"].as_str().unwrap()).collect::<Vec<_>>(),
                       ["connection_ready", "permission", "peer_info"]);
            assert_eq!(received, before);
            assert_eq!(events(&existing), before, "existing UI/MCP sessions must not be reinitialized");
        }
        #[cfg(feature = "mcp")]
        #[test] fn headless_session_without_dart_sink_still_caches_authentication() {
            let s = peer(ConnType::TERMINAL, "no-dart");
            authenticated(&s);
            assert_eq!(events(&attach(&s, 2)).last().unwrap()["name"], "peer_info");
        }
        #[cfg(feature = "mcp")]
        #[test] fn attachment_before_authentication_receives_live_peer_info_once() {
            let s = peer(ConnType::TERMINAL, "early-ui");
            s.lc.write().unwrap().peer_info = None;
            let stream = attach(&s, 2);
            assert!(events(&stream).is_empty());
            s.lc.write().unwrap().peer_info = Some(());
            authenticated(&s);
            assert_eq!(events(&stream).iter().filter(|e| e["name"] == "peer_info").count(), 1);
        }
        #[cfg(feature = "mcp")]
        #[test] fn authentication_racing_attachment_initializes_the_new_ui_once() {
            for iteration in 0..32 {
                let s = peer(ConnType::TERMINAL, &format!("race-{iteration}"));
                emit(&s, "connection_ready", &[("secure", "true")]);
                let barrier = std::sync::Barrier::new(2);
                let stream = std::thread::scope(|scope| {
                    scope.spawn(|| {
                        barrier.wait();
                        emit(&s, "peer_info", &[("hostname", "fixture")]);
                    });
                    barrier.wait();
                    attach(&s, 2)
                });
                assert_eq!(events(&stream).iter().filter(|e| e["name"] == "peer_info").count(), 1);
            }
        }
        #[cfg(feature = "mcp")]
        #[test] fn attachment_does_not_replay_terminal_output_or_commands() {
            let s = peer(ConnType::TERMINAL, "no-replay");
            authenticated(&s);
            emit(&s, "terminal_response", &[("type", "data"), ("data", "old-shell-output")]);
            emit(&s, "msgbox", &[("type", "error"), ("text", "old-error")]);
            let received = events(&attach(&s, 2));
            assert_eq!(received.len(), 3);
            assert!(received.iter().all(|e| e["name"] != "terminal_response" && e["name"] != "msgbox"));
        }
        #[cfg(feature = "mcp")]
        #[test] fn reconnect_never_replays_the_previous_round() {
            let s = peer(ConnType::TERMINAL, "reconnect");
            authenticated(&s);
            s.connection_round_state.lock().unwrap().connected = false;
            assert!(events(&attach(&s, 2)).is_empty());
            s.connection_round_state.lock().unwrap().connected = true;
            emit(&s, "connection_ready", &[("secure", "true")]);
            let stream = attach(&s, 3);
            assert!(events(&stream).is_empty());
            emit(&s, "peer_info", &[("hostname", "new-round")]);
            assert_eq!(events(&stream).len(), 1);
            assert_eq!(events(&stream)[0]["hostname"], "new-round");
        }
        #[cfg(feature = "mcp")]
        #[test] fn attachment_preserves_the_latest_permission_revocation() {
            let s = peer(ConnType::TERMINAL, "permissions");
            authenticated(&s);
            emit(&s, "permission", &[("clipboard", "true")]);
            emit(&s, "permission", &[("clipboard", "false")]);
            let received = events(&attach(&s, 2));
            assert_eq!(received.iter().filter(|e| e.get("clipboard").is_some()).count(), 1);
            assert_eq!(received.iter().find(|e| e.get("clipboard").is_some()).unwrap()["clipboard"], "false");
        }
        #[cfg(feature = "mcp")]
        #[test] fn closed_transport_does_not_initialize_a_new_terminal_ui() {
            let s = peer(ConnType::TERMINAL, "closed");
            authenticated(&s);
            *s.sender.write().unwrap() = Some(Sender { closed: true });
            assert!(events(&attach(&s, 2)).is_empty());
            *s.sender.write().unwrap() = None;
            assert!(events(&attach(&s, 3)).is_empty());
        }
        #[test] fn desktop_and_file_attachment_keep_the_existing_path() {
            for (kind, id) in [(ConnType::DEFAULT_CONN, "desktop"), (ConnType::FILE_TRANSFER, "files")] {
                let s = peer(kind, id);
                authenticated(&s);
                assert!(events(&attach(&s, 2)).is_empty());
            }
        }
        #[cfg(not(feature = "mcp"))]
        #[test] fn terminal_attachment_without_mcp_keeps_the_existing_path() {
            let s = peer(ConnType::TERMINAL, "feature-off");
            authenticated(&s);
            assert!(events(&attach(&s, 2)).is_empty());
        }
    }
}
'''


if __name__ == '__main__':
    unittest.main()
