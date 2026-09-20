//! Local Unix providers share only the new path; Windows and remote input stay independent.
use hbb_common::base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{imageops, ImageOutputFormat, RgbaImage};
use rustdesk_agent_mcp::{local::ipc, success, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Display {
    id: u32,
    name: String,
    x: f64,
    y: f64,
    width: u32,
    height: u32,
    logical_width: f64,
    logical_height: f64,
    rotation: f64,
    primary: bool,
}
impl Display {
    fn point(&self, x: u32, y: u32) -> Result<(f64, f64), String> {
        if x >= self.width || y >= self.height {
            return Err("Coordinates outside local display".into());
        }
        Ok((
            self.x + f64::from(x) * self.logical_width / f64::from(self.width),
            self.y + f64::from(y) * self.logical_height / f64::from(self.height),
        ))
    }
}

#[derive(Deserialize)]
pub(super) struct Snapshot {
    desktop_id: String,
    active_window: String,
    displays: Vec<Display>,
    capture_allowed: bool,
    input_allowed: bool,
    provider: String,
}
impl Snapshot {
    fn layout(&self) -> Result<String, String> {
        if self.displays.is_empty() || self.displays.len() > 64 {
            return Err("Local displays unavailable".into());
        }
        for d in &self.displays {
            if d.width == 0
                || d.height == 0
                || !d.x.is_finite()
                || !d.y.is_finite()
                || !d.logical_width.is_finite()
                || !d.logical_height.is_finite()
                || d.logical_width <= 0.0
                || d.logical_height <= 0.0
                || !d.rotation.is_finite()
            {
                return Err("Invalid local display geometry".into());
            }
        }
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&self.displays).map_err(|e| e.to_string())?)
        ))
    }
    fn values(&self) -> Vec<Value> {
        self.displays
            .iter()
            .enumerate()
            .map(|(index, display)| {
                let mut value = json!(display);
                value["display"] = json!(index);
                value["scale_x"] = json!(f64::from(display.width) / display.logical_width);
                value["scale_y"] = json!(f64::from(display.height) / display.logical_height);
                value
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Event {
    Move(f64, f64),
    Button(u8, bool, u32),
    Key(u16, bool),
    Unicode(char),
    Scroll(i32, i32),
}
pub(super) trait Desktop: Send {
    fn snapshot(&self) -> Result<Snapshot, String>;
    fn capture(&self, display: &Display) -> Result<RgbaImage, String>;
    fn idle(&self) -> Result<(), String>;
    fn keycode(&self, key: &str) -> Result<u16, String>;
    fn validate_text(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }
    fn event(&self, event: Event) -> Result<(), String>;
}

pub(crate) fn run() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let create = linux::Linux::new;
    #[cfg(target_os = "macos")]
    let create = macos::Mac::new;
    let state = Mutex::new(None);
    ipc::run(move |name, args, active| {
        let mut state = state.lock().unwrap();
        if state.is_none() {
            let desktop = create()?;
            let identity = desktop.snapshot()?.desktop_id;
            *state = Some(Agent {
                desktop,
                identity,
                generation: uuid::Uuid::new_v4().to_string(),
                frame: 0,
            });
        }
        state
            .as_mut()
            .ok_or("Local desktop unavailable")?
            .call(name, args, active)
    })
}

struct Agent<D> {
    desktop: D,
    identity: String,
    generation: String,
    frame: u64,
}
impl<D: Desktop> Agent<D> {
    fn call(
        &mut self,
        name: &str,
        args: &Map<String, Value>,
        active: &dyn Fn() -> Result<(), String>,
    ) -> ToolResult {
        active()?;
        let snapshot = self.desktop.snapshot()?;
        if snapshot.desktop_id != self.identity {
            return Err("Local login changed; reconnect".into());
        }
        let layout = snapshot.layout()?;
        if name == "status" {
            return Ok(
                json!({"state":"ready","desktop_id":self.identity,"agent_generation":self.generation,
                "display_generation":layout,"active_window":snapshot.active_window,"displays":snapshot.values(),
                "provider":snapshot.provider,"requires_interactive_user":true,
                "permissions":{"capture":snapshot.capture_allowed,"input":snapshot.input_allowed}}),
            );
        }
        if args.get("agent_generation").and_then(Value::as_str) != Some(&self.generation)
            || args.get("desktop_id").and_then(Value::as_str) != Some(&self.identity)
        {
            return Err("Local agent identity changed; reconnect".into());
        }
        let input = rustdesk_agent_mcp::local::input_tool(name);
        if !snapshot.capture_allowed || (input && !snapshot.input_allowed) {
            return Err("Local desktop permission missing: grant Screen Recording and Accessibility on macOS, or authorize an active X11 session on Linux".into());
        }
        let index = args.get("display").and_then(Value::as_u64).unwrap_or(0);
        let display = snapshot
            .displays
            .get(usize::try_from(index).map_err(|_| "Invalid display")?)
            .ok_or("Unknown local display")?;
        let guard = || {
            active()?;
            let now = self.desktop.snapshot()?;
            if now.desktop_id != self.identity || now.layout()? != layout {
                return Err("Local login or display layout changed; reconnect and observe".into());
            }
            if !now.capture_allowed || (input && !now.input_allowed) {
                return Err("Local OS permission revoked".into());
            }
            if let Some(expected) = args.get("expected_window").and_then(Value::as_str) {
                if now.active_window != expected {
                    return Err("Foreground window changed; observe before retrying".into());
                }
            }
            Ok(())
        };
        if name == "screenshot" {
            self.frame += 1;
            if args
                .get("after_frame")
                .and_then(Value::as_u64)
                .is_some_and(|after| after >= self.frame)
            {
                return Err(
                    "after_frame must refer to an earlier frame from this local agent".into(),
                );
            }
            if u64::from(display.width) * u64::from(display.height) > 16 * 1024 * 1024 {
                return Err("Local frame exceeds 64 MiB pixel limit".into());
            }
            let started = Instant::now();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis();
            let image = self.desktop.capture(display)?;
            if image.dimensions() != (display.width, display.height) {
                return Err("Captured size changed; observe again".into());
            }
            let mut result = encode(image, args)?;
            guard()?;
            if self.desktop.snapshot()?.active_window != snapshot.active_window {
                return Err("Foreground window changed during capture; observe again".into());
            }
            let metadata = &mut result["structuredContent"];
            metadata["frame_id"] = json!(self.frame);
            metadata["captured_at_ms"] = json!(timestamp);
            metadata["frame_age_ms"] = json!(started.elapsed().as_millis());
            metadata["agent_generation"] = json!(self.generation);
            metadata["display_generation"] = json!(layout);
            metadata["active_window"] = json!(snapshot.active_window);
            // success() serializes metadata into the first text block as well.
            result["content"][0]["text"] = json!(result["structuredContent"].to_string());
            return Ok(result);
        }
        if !input {
            return Err("Unsupported local operation".into());
        }
        if args.get("display_generation").and_then(Value::as_str) != Some(&layout) {
            return Err("Display layout changed; take a new screenshot".into());
        }
        perform(&self.desktop, name, display, args, &guard)?;
        Ok(success(
            json!({"session":args.get("session"),"operation_id":args.get("operation_id"),
            "delivery":"os_input_api","task_success":Value::Null,"request_replayed":false,
            "verify":"Take a new screenshot; OS acceptance does not prove application success"}),
        ))
    }
}

fn number(args: &Map<String, Value>, key: &str) -> Result<u32, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("Invalid {key}"))
}
fn encode(mut image: RgbaImage, args: &Map<String, Value>) -> ToolResult {
    let (width, height) = image.dimensions();
    let (mut x, mut y) = (0, 0);
    if let Some(region) = args.get("region").and_then(Value::as_object) {
        x = number(region, "x")?;
        y = number(region, "y")?;
        let w = number(region, "width")?;
        let h = number(region, "height")?;
        if w == 0
            || h == 0
            || x.checked_add(w).map_or(true, |v| v > width)
            || y.checked_add(h).map_or(true, |v| v > height)
        {
            return Err("Crop lies outside the local display".into());
        }
        image = imageops::crop_imm(&image, x, y, w, h).to_image();
    }
    let (original_width, original_height) = image.dimensions();
    let max_width = args
        .get("max_width")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(original_width))
        .clamp(64, 32768) as u32;
    if image.width() > max_width {
        let h = (u64::from(image.height()) * u64::from(max_width) / u64::from(image.width())).max(1)
            as u32;
        image = imageops::resize(&image, max_width, h, imageops::FilterType::Triangle);
    }
    let sx = f64::from(original_width) / f64::from(image.width());
    let sy = f64::from(original_height) / f64::from(image.height());
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageOutputFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut result = success(
        json!({"session":args.get("session"),"display":args.get("display").and_then(Value::as_u64).unwrap_or(0),
        "width":image.width(),"height":image.height(),"remote_width":width,"remote_height":height,
        "origin_x":x,"origin_y":y,"scale_x":sx,"scale_y":sy,"coordinate_space":"display_relative_original_pixels",
        "image_to_display":{"offset_x":x,"offset_y":y,"scale_x":sx,"scale_y":sy}}),
    );
    result["content"]
        .as_array_mut()
        .ok_or("Invalid image response")?
        .push(
            json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(png.into_inner())}),
        );
    Ok(result)
}

struct Held<'a, D: Desktop> {
    desktop: &'a D,
    release: Vec<Event>,
}
impl<D: Desktop> Held<'_, D> {
    fn press(&mut self, down: Event, up: Event) -> Result<(), String> {
        // Queue release first: a failed OS acknowledgement may still have delivered the press.
        self.release.push(up);
        self.desktop.event(down)
    }
    fn release(&mut self) -> Result<(), String> {
        let mut error = None;
        while let Some(up) = self.release.pop() {
            if let Err(e) = self.desktop.event(up) {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
impl<D: Desktop> Drop for Held<'_, D> {
    fn drop(&mut self) {
        if let Err(e) = self.release() {
            eprintln!("Local input release: {e}");
        }
    }
}
fn perform<D: Desktop>(
    desktop: &D,
    name: &str,
    display: &Display,
    args: &Map<String, Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    desktop.idle()?;
    let mut held = Held {
        desktop,
        release: Vec::new(),
    };
    if name.starts_with("mouse_") {
        let (x, y) = display.point(number(args, "x")?, number(args, "y")?)?;
        let target = if name == "mouse_drag" {
            Some(display.point(number(args, "to_x")?, number(args, "to_y")?)?)
        } else {
            None
        };
        let button = match args.get("button").and_then(Value::as_str).unwrap_or("left") {
            "left" => 1,
            "middle" => 2,
            "right" => 3,
            _ => return Err("Invalid mouse button".into()),
        };
        desktop.event(Event::Move(x, y))?;
        match name {
            "mouse_move" => {}
            "mouse_click" => {
                for count in 1..=args
                    .get("clicks")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 3)
                {
                    check()?;
                    held.press(
                        Event::Button(button, true, count as u32),
                        Event::Button(button, false, count as u32),
                    )?;
                    held.release()?;
                }
            }
            "mouse_drag" => {
                let (tx, ty) = target.ok_or("Missing drag target")?;
                check()?;
                held.press(Event::Button(1, true, 1), Event::Button(1, false, 1))?;
                let duration = args
                    .get("duration_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(300)
                    .clamp(50, 5000);
                let steps = (duration / 20).max(1);
                for step in 1..=steps {
                    check()?;
                    desktop.event(Event::Move(
                        x + (tx - x) * step as f64 / steps as f64,
                        y + (ty - y) * step as f64 / steps as f64,
                    ))?;
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
            "mouse_scroll" => {
                let dx = args
                    .get("dx")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .clamp(-100, 100) as i32;
                let dy = args
                    .get("dy")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .clamp(-100, 100) as i32;
                // One tick per guard also makes long wheel sequences cancellable on X11.
                for i in 0..dx.abs().max(dy.abs()) {
                    check()?;
                    desktop.event(Event::Scroll(
                        if i < dx.abs() { dx.signum() } else { 0 },
                        if i < dy.abs() { dy.signum() } else { 0 },
                    ))?;
                }
            }
            _ => return Err("Unknown local mouse operation".into()),
        }
    } else {
        if args
            .get("expected_window")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .is_none()
        {
            return Err("Local keyboard input requires expected_window from a screenshot".into());
        }
        match name {
            "keyboard_input" => {
                let text = args
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or("Missing text")?;
                if text.len() > 65536 {
                    return Err("Local Unicode input is limited to 64 KiB".into());
                }
                desktop.validate_text(text)?;
                for ch in text.chars() {
                    check()?;
                    desktop.event(Event::Unicode(ch))?;
                }
            }
            "keyboard_hotkey" => {
                let keys = args
                    .get("keys")
                    .and_then(Value::as_array)
                    .ok_or("Missing keys")?;
                if keys.is_empty() || keys.len() > 16 {
                    return Err("Invalid local hotkey length".into());
                }
                let codes = keys
                    .iter()
                    .map(|key| desktop.keycode(key.as_str().ok_or("Invalid key")?))
                    .collect::<Result<Vec<_>, String>>()?;
                for code in codes {
                    check()?;
                    held.press(Event::Key(code, true), Event::Key(code, false))?;
                }
            }
            _ => return Err("Unsupported local input operation".into()),
        }
    }
    held.release()?;
    check()
}

#[cfg(test)]
mod tests;
