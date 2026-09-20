use super::*;
use std::cell::{Cell, RefCell};
struct Fake {
    events: RefCell<Vec<Event>>,
    locked: Cell<bool>,
    input: Cell<bool>,
    window: RefCell<String>,
    scale: Cell<f64>,
}
impl Fake {
    fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            locked: Cell::new(false),
            input: Cell::new(true),
            window: RefCell::new("window-1".into()),
            scale: Cell::new(2.0),
        }
    }
}
fn display() -> Display {
    Display {
        id: 1,
        name: "retina".into(),
        x: -800.0,
        y: 0.0,
        width: 1600,
        height: 1200,
        logical_width: 800.0,
        logical_height: 600.0,
        rotation: 0.0,
        primary: true,
    }
}
impl Desktop for Fake {
    fn snapshot(&self) -> Result<Snapshot, String> {
        if self.locked.get() {
            return Err("locked".into());
        }
        let mut d = display();
        d.logical_width = f64::from(d.width) / self.scale.get();
        Ok(Snapshot {
            desktop_id: "desktop-1".into(),
            active_window: self.window.borrow().clone(),
            displays: vec![d],
            capture_allowed: true,
            input_allowed: self.input.get(),
            provider: "test".into(),
        })
    }
    fn capture(&self, d: &Display) -> Result<RgbaImage, String> {
        Ok(RgbaImage::new(d.width, d.height))
    }
    fn idle(&self) -> Result<(), String> {
        Ok(())
    }
    fn keycode(&self, key: &str) -> Result<u16, String> {
        match key {
            "ctrl" => Ok(1),
            "a" => Ok(2),
            _ => Err("unknown".into()),
        }
    }
    fn event(&self, event: Event) -> Result<(), String> {
        self.events.borrow_mut().push(event);
        Ok(())
    }
}
fn arguments() -> Map<String, Value> {
    json!({"desktop_id":"desktop-1","agent_generation":"agent-1","display":0,"expected_window":"window-1"}).as_object().unwrap().clone()
}
fn agent() -> Agent<Fake> {
    Agent {
        desktop: Fake::new(),
        identity: "desktop-1".into(),
        generation: "agent-1".into(),
        frame: 0,
    }
}

#[test]
fn retina_negative_origin_and_crop_coordinates() {
    let d = display();
    assert_eq!(d.point(1200, 300).unwrap(), (-200.0, 150.0));
    assert!(d.point(1600, 0).is_err());
    let mut args = arguments();
    args.insert(
        "region".into(),
        json!({"x":100,"y":50,"width":800,"height":400}),
    );
    args.insert("max_width".into(), json!(400));
    let result = agent().call("screenshot", &args, &|| Ok(())).unwrap();
    assert_eq!(
        result["structuredContent"]["image_to_display"],
        json!({"offset_x":100,"offset_y":50,"scale_x":2.0,"scale_y":2.0})
    );
    assert_eq!(result["structuredContent"]["active_window"], "window-1");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    assert!(result["content"][1]["data"]
        .as_str()
        .unwrap()
        .starts_with("iVBOR"));
}
#[test]
fn cancellation_releases_only_our_pressed_keys() {
    let desktop = Fake::new();
    let mut args = arguments();
    args.insert("keys".into(), json!(["ctrl", "a"]));
    let checks = Cell::new(0);
    let result = perform(&desktop, "keyboard_hotkey", &display(), &args, &|| {
        checks.set(checks.get() + 1);
        if checks.get() >= 3 {
            Err("cancel".into())
        } else {
            Ok(())
        }
    });
    assert!(result.is_err());
    let events = desktop.events.borrow();
    assert!(matches!(
        events.as_slice(),
        [Event::Key(1, true), Event::Key(1, false)]
    ));
}
#[test]
fn cancel_drag_releases_mouse_and_invalid_target_never_moves() {
    let desktop = Fake::new();
    let mut args = arguments();
    args.extend(
        json!({"x":5,"y":6,"to_x":50,"to_y":60,"duration_ms":50})
            .as_object()
            .unwrap()
            .clone(),
    );
    let checks = Cell::new(0);
    assert!(perform(&desktop, "mouse_drag", &display(), &args, &|| {
        checks.set(checks.get() + 1);
        if checks.get() == 3 {
            Err("cancel".into())
        } else {
            Ok(())
        }
    })
    .is_err());
    assert!(matches!(
        desktop.events.borrow().last(),
        Some(Event::Button(1, false, 1))
    ));
    desktop.events.borrow_mut().clear();
    args.insert("to_x".into(), json!(1600));
    assert!(perform(&desktop, "mouse_drag", &display(), &args, &|| Ok(())).is_err());
    assert!(desktop.events.borrow().is_empty());
}
#[test]
fn permission_identity_layout_focus_and_lock_fail_before_input() {
    let mut agent = agent();
    let mut args = arguments();
    args.insert("text".into(), json!("hello"));
    args.insert(
        "display_generation".into(),
        json!(agent.desktop.snapshot().unwrap().layout().unwrap()),
    );
    agent.desktop.input.set(false);
    assert!(agent.call("keyboard_input", &args, &|| Ok(())).is_err());
    agent.desktop.input.set(true);
    agent.desktop.scale.set(1.0);
    assert!(agent.call("keyboard_input", &args, &|| Ok(())).is_err());
    agent.desktop.scale.set(2.0);
    *agent.desktop.window.borrow_mut() = "window-2".into();
    assert!(agent.call("keyboard_input", &args, &|| Ok(())).is_err());
    *agent.desktop.window.borrow_mut() = "window-1".into();
    args.insert("agent_generation".into(), json!("old-agent"));
    assert!(agent.call("keyboard_input", &args, &|| Ok(())).is_err());
    agent.desktop.locked.set(true);
    assert!(agent.call("status", &Map::new(), &|| Ok(())).is_err());
    assert!(agent.desktop.events.borrow().is_empty());
}
#[test]
fn invalid_hotkey_and_missing_keyboard_guard_never_inject() {
    let desktop = Fake::new();
    let mut args = arguments();
    args.insert("keys".into(), json!(["ctrl", "unknown"]));
    assert!(perform(&desktop, "keyboard_hotkey", &display(), &args, &|| Ok(())).is_err());
    args.remove("expected_window");
    args.insert("text".into(), json!("hello"));
    assert!(perform(&desktop, "keyboard_input", &display(), &args, &|| Ok(())).is_err());
    assert!(desktop.events.borrow().is_empty());
}
