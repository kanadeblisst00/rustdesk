#![cfg(feature = "local")]
use rustdesk_agent_mcp::{
    context::CallContext,
    local::{Provider, Registry},
    success, ToolResult,
};
use serde_json::{json, Map, Value};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct Fake {
    writes: AtomicUsize,
    stopped: AtomicBool,
    generation: AtomicUsize,
    layout: AtomicUsize,
    fail: AtomicBool,
    block: AtomicBool,
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}
struct FakeProvider(Arc<Fake>);
impl std::ops::Deref for FakeProvider {
    type Target = Fake;
    fn deref(&self) -> &Fake {
        &self.0
    }
}
impl Provider for FakeProvider {
    fn call(
        &self,
        name: &str,
        _: &Map<String, Value>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> ToolResult {
        check()?;
        if name == "status" {
            return Ok(
                json!({"desktop_id":"desktop", "agent_generation":self.generation.load(Ordering::SeqCst).to_string(), "display_generation":self.layout.load(Ordering::SeqCst).to_string()}),
            );
        }
        if name == "screenshot" {
            return Ok(success(
                json!({"display_generation":self.layout.load(Ordering::SeqCst).to_string()}),
            ));
        }
        self.writes.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = self.entered.lock().unwrap().take() {
            tx.send(()).unwrap();
        }
        while self.block.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst) {
            check()?;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        check()?;
        if self.fail.load(Ordering::SeqCst) {
            return Err("Input outcome unknown".into());
        }
        Ok(success(json!({"accepted":true})))
    }
    fn stop(&self) -> Result<(), String> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

fn context(owner: &str) -> CallContext {
    CallContext::new(owner.into(), "credential:epoch1".into(), true)
}
fn call(reg: &Registry<FakeProvider>, ctx: &CallContext, name: &str, args: Value) -> ToolResult {
    reg.call(
        ctx,
        name,
        args.as_object().unwrap(),
        &["desktop.capture", "desktop.input"],
        || "session1".into(),
        || Ok(()),
    )
}
fn connect(reg: &Registry<FakeProvider>, ctx: &CallContext) {
    call(
        reg,
        ctx,
        "connect_local",
        json!({"requested_scopes":["desktop.capture","desktop.input"]}),
    )
    .unwrap();
}
fn observe(reg: &Registry<FakeProvider>, ctx: &CallContext) {
    call(reg, ctx, "screenshot", json!({"session":"session1"})).unwrap();
}
fn click() -> Value {
    json!({"session":"session1","x":10,"y":20,"operation_id":"op1","display_generation":"0"})
}

#[test]
fn local_scopes_are_server_granted_and_credentials_cannot_cross_sessions() {
    let fake = Arc::new(Fake::default());
    let reg = Registry::new(FakeProvider(fake.clone()));
    let alice = context("alice");
    let remote = CallContext::new("legacy".into(), String::new(), false);
    assert!(call(
        &reg,
        &remote,
        "connect_local",
        json!({"requested_scopes":["desktop.capture"]})
    )
    .is_err());
    assert!(reg
        .call(
            &alice,
            "connect_local",
            json!({"requested_scopes":["desktop.capture","desktop.input"]})
                .as_object()
                .unwrap(),
            &["desktop.capture"],
            || "s".into(),
            || Ok(())
        )
        .is_err());
    connect(&reg, &alice);
    for name in [
        "screenshot",
        "get_connection_info",
        "disconnect_session",
        "mouse_click",
    ] {
        assert!(call(&reg, &context("bob"), name, click()).is_err());
    }
    let mut rotated = alice.clone();
    rotated.credential = "credential:epoch2".into();
    assert!(call(&reg, &rotated, "screenshot", json!({"session":"session1"})).is_err());
    assert_eq!(
        call(
            &reg,
            &context("bob"),
            "list_connections",
            json!({"target_type":"all"})
        )
        .unwrap()["structuredContent"]["connections"],
        json!([])
    );
    assert_eq!(
        call(&reg, &alice, "list_connections", json!({})).unwrap()["structuredContent"]
            ["connections"],
        json!([])
    );
    assert_eq!(
        call(
            &reg,
            &alice,
            "list_connections",
            json!({"target_type":"local"})
        )
        .unwrap()["structuredContent"]["connections"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn input_requires_observation_and_deduplicates_success_unknown_and_conflicts() {
    let fake = Arc::new(Fake::default());
    let reg = Registry::new(FakeProvider(fake.clone()));
    let ctx = context("alice");
    connect(&reg, &ctx);
    assert!(call(&reg, &ctx, "mouse_click", click()).is_err());
    observe(&reg, &ctx);
    let first = call(&reg, &ctx, "mouse_click", click()).unwrap();
    assert_eq!(call(&reg, &ctx, "mouse_click", click()).unwrap(), first);
    let mut conflict = click();
    conflict["x"] = json!(99);
    assert!(call(&reg, &ctx, "mouse_click", conflict)
        .unwrap_err()
        .contains("different arguments"));
    fake.fail.store(true, Ordering::SeqCst);
    let mut next = click();
    next["operation_id"] = json!("unknown");
    assert!(call(&reg, &ctx, "mouse_click", next.clone()).is_err());
    fake.fail.store(false, Ordering::SeqCst);
    // The gateway must return the journal even if the failed helper has restarted.
    fake.generation.store(1, Ordering::SeqCst);
    assert!(call(&reg, &ctx, "mouse_click", next)
        .unwrap_err()
        .contains("unknown"));
    assert_eq!(fake.writes.load(Ordering::SeqCst), 2);
}

#[test]
fn layout_changes_agent_restart_and_cancelled_contexts_fail_closed() {
    let fake = Arc::new(Fake::default());
    let reg = Registry::new(FakeProvider(fake.clone()));
    let ctx = context("alice");
    connect(&reg, &ctx);
    observe(&reg, &ctx);
    fake.layout.store(1, Ordering::SeqCst);
    assert!(call(&reg, &ctx, "mouse_click", click())
        .unwrap_err()
        .contains("layout changed"));
    fake.generation.store(1, Ordering::SeqCst);
    assert!(
        call(&reg, &ctx, "screenshot", json!({"session":"session1"}))
            .unwrap_err()
            .contains("agent or desktop changed")
    );
    assert!(call(&reg, &ctx, "screenshot", json!({"session":"session1"})).is_err());
    connect(&reg, &ctx);
    ctx.cancel();
    assert!(call(&reg, &ctx, "screenshot", json!({"session":"session1"})).is_err());
    assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn stop_bypasses_active_desktop_operation_and_revokes_queued_actions() {
    let fake = Arc::new(Fake::default());
    let reg = Arc::new(Registry::new(FakeProvider(fake.clone())));
    let ctx = context("alice");
    connect(&reg, &ctx);
    observe(&reg, &ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    *fake.entered.lock().unwrap() = Some(tx);
    fake.block.store(true, Ordering::SeqCst);
    let active = {
        let reg = reg.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || call(&reg, &ctx, "mouse_click", click()))
    };
    rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
    let queued = {
        let reg = reg.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut args = click();
            args["operation_id"] = json!("queued");
            call(&reg, &ctx, "mouse_click", args)
        })
    };
    call(&reg, &ctx, "stop_local", json!({})).unwrap();
    assert!(active.join().unwrap().is_err());
    assert!(queued.join().unwrap().is_err());
    assert_eq!(fake.writes.load(Ordering::SeqCst), 1);
}

#[test]
fn read_only_sessions_never_reach_input_or_remote_file_and_process_providers() {
    let fake = Arc::new(Fake::default());
    let reg = Registry::new(FakeProvider(fake.clone()));
    let ctx = context("alice");
    call(
        &reg,
        &ctx,
        "connect_local",
        json!({"requested_scopes":["desktop.capture"]}),
    )
    .unwrap();
    observe(&reg, &ctx);
    for name in [
        "mouse_click",
        "keyboard_input",
        "file_transfer",
        "run_process",
        "clipboard_set",
        "get_ui_tree",
    ] {
        assert!(call(&reg, &ctx, name, click()).is_err());
    }
    assert_eq!(fake.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn mouse_input_requires_observation_of_the_same_display() {
    let fake = Arc::new(Fake::default());
    let reg = Registry::new(FakeProvider(fake.clone()));
    let ctx = context("alice");
    connect(&reg, &ctx);
    call(
        &reg,
        &ctx,
        "screenshot",
        json!({"session":"session1","display":1}),
    )
    .unwrap();
    assert!(call(&reg, &ctx, "mouse_click", click()).is_err());
    let mut args = click();
    args["display"] = json!(1);
    call(&reg, &ctx, "mouse_click", args).unwrap();
    assert_eq!(fake.writes.load(Ordering::SeqCst), 1);
}
