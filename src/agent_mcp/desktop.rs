use super::*;
use crate::{client::Data, flutter::FlutterSession, input::*};
use hbb_common::{
    base64::{engine::general_purpose::STANDARD, Engine as _},
    message_proto::*,
};
use image::{imageops, ImageBuffer, ImageOutputFormat, Rgba};
use std::io::Cursor;

mod hotkey;

pub(super) struct PendingScreenshot {
    pub id: String,
    pub result: Option<Result<Vec<u8>, String>>,
}

struct ScreenshotGuard<'a>(&'a SessionState);
impl Drop for ScreenshotGuard<'_> {
    fn drop(&mut self) {
        self.0.pending_screenshot.lock().unwrap().take();
    }
}

#[derive(Clone)]
pub(super) struct Frame {
    pub display: usize,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
    pub sequence: u64,
    pub captured: Instant,
}

impl Frame {
    pub fn from_rgb(display: usize, sequence: u64, rgb: &scrap::ImageRgb) -> Result<Self, String> {
        let bgra = match rgb.fmt {
            scrap::ImageFormat::ABGR => false,
            scrap::ImageFormat::ARGB => true,
            _ => return Err("Unsupported raw pixel format".into()),
        };
        let rgba = rustdesk_agent_mcp::pixels::pack_rgba(&rgb.raw, rgb.w, rgb.h, rgb.align, bgra)?;
        Ok(Self {
            display,
            width: rgb.w as u32,
            height: rgb.h as u32,
            rgba: Arc::new(rgba),
            sequence,
            captured: Instant::now(),
        })
    }
}

pub(super) fn display(s: &FlutterSession, state: &SessionState, args: &Map<String, Value>) -> usize {
    args.get("display")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .or(*state.requested_display.lock().unwrap())
        .unwrap_or_else(|| {
            s.lc.read()
                .unwrap()
                .peer_info
                .as_ref()
                .map(|p| p.current_display as usize)
                .unwrap_or(0)
        })
}

pub(super) fn select(
    s: &FlutterSession,
    state: &SessionState,
    display: usize,
) -> Result<(), String> {
    if !s.is_default() {
        return Err("A desktop session is required".into());
    }
    let lc = s.lc.read().unwrap();
    if lc
        .peer_info
        .as_ref()
        .map_or(true, |p| display >= p.displays.len())
    {
        return Err("Unknown display".into());
    }
    drop(lc);
    *state.requested_display.lock().unwrap() = Some(display);
    // Reading a screen must not apply a saved custom resolution or unsubscribe another window.
    let mut misc = Misc::new();
    misc.set_switch_display(SwitchDisplay {
        display: display as i32,
        ..Default::default()
    });
    let mut msg = Message::new();
    msg.set_misc(misc);
    session::send(s, Data::Message(msg))?;
    s.capture_displays(vec![display as i32], vec![], vec![]);
    s.refresh_video(display as i32);
    Ok(())
}

pub(super) fn screenshot(
    id: SessionID,
    s: &FlutterSession,
    state: &SessionState,
    args: &Map<String, Value>,
) -> ToolResult {
    let _lock = state
        .screenshot
        .try_lock()
        .map_err(|_| "Another screenshot is in progress on this session")?;
    let _pending = ScreenshotGuard(state);
    session::ready(s)?;
    if !s.is_default() {
        return Err("A desktop session is required".into());
    }
    let disp = display(s, state, args);
    if *state.requested_display.lock().unwrap() != Some(disp) {
        select(s, state, disp)?;
    }
    let deadline = Instant::now() + Duration::from_millis(number(args, "timeout_ms", 3000) as u64);
    let after = number(args, "after_frame", 0) as u64;
    if after
        > state
            .frame
            .lock()
            .unwrap()
            .as_ref()
            .map(|f| f.sequence)
            .unwrap_or(0)
    {
        return Err("after_frame is ahead of this session's frame cursor".into());
    }
    let started = Instant::now();
    let mut requested = false;
    let frame = loop {
        session::get(id)?;
        session::ready(s)?;
        if let Some(frame) = state
            .frame
            .lock()
            .unwrap()
            .as_ref()
            .filter(|f| f.display == disp && f.sequence > after)
            .cloned()
        {
            break frame;
        }
        if !requested
            && started.elapsed() >= Duration::from_millis(150)
            && s.is_screenshot_supported()
        {
            let request_id = format!("agent-mcp:{}", uuid::Uuid::new_v4());
            *state.pending_screenshot.lock().unwrap() = Some(PendingScreenshot {
                id: request_id.clone(),
                result: None,
            });
            session::send(s, Data::TakeScreenshot((disp as i32, request_id)))?;
            requested = true;
        }
        let response = state
            .pending_screenshot
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|p| p.result.take());
        if let Some(response) = response {
            let data = response?;
            let dimensions = image::io::Reader::new(Cursor::new(&data))
                .with_guessed_format()
                .map_err(|e| e.to_string())?
                .into_dimensions()
                .map_err(|e| e.to_string())?;
            let bytes = (dimensions.0 as u64)
                .checked_mul(dimensions.1 as u64)
                .and_then(|pixels| pixels.checked_mul(4))
                .ok_or("Remote screenshot dimensions overflow")?;
            if bytes == 0 || bytes > rustdesk_agent_mcp::pixels::MAX_FRAME_BYTES as u64 {
                return Err("Remote screenshot dimensions exceed size limit".into());
            }
            let decoded = image::load_from_memory(&data)
                .map_err(|e| e.to_string())?
                .into_rgba8();
            let mut cache = state.frame.lock().unwrap();
            let frame = Frame {
                display: disp,
                width: decoded.width(),
                height: decoded.height(),
                rgba: Arc::new(decoded.into_raw()),
                sequence: cache.as_ref().map(|f| f.sequence + 1).unwrap_or(1),
                captured: Instant::now(),
            };
            *cache = Some(frame.clone());
            break frame;
        }
        if Instant::now() >= deadline {
            return Err("No remote frame before timeout. Verify the connection and display; old GPU-only peers need a CPU-readable renderer or remote screenshot protocol support.".into());
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let mut img = ImageBuffer::<Rgba<u8>, _>::from_raw(
        frame.width,
        frame.height,
        frame.rgba.as_ref().clone(),
    )
    .ok_or("Invalid RGBA frame")?;
    let (mut x, mut y) = (0, 0);
    if let Some(region) = args.get("region").and_then(Value::as_object) {
        x = number(region, "x", 0) as u32;
        y = number(region, "y", 0) as u32;
        let w = number(region, "width", 0) as u32;
        let h = number(region, "height", 0) as u32;
        if x.checked_add(w).map_or(true, |v| v > frame.width)
            || y.checked_add(h).map_or(true, |v| v > frame.height)
        {
            return Err("Crop lies outside the remote frame".into());
        }
        img = imageops::crop_imm(&img, x, y, w, h).to_image();
    }
    let original_width = img.width();
    let original_height = img.height();
    let max_width = number(args, "max_width", img.width() as i64) as u32;
    if img.width() > max_width {
        let height = ((img.height() as u64 * max_width as u64) / img.width() as u64).max(1) as u32;
        img = imageops::resize(&img, max_width, height, imageops::FilterType::Triangle);
    }
    let mut png = Cursor::new(Vec::new());
    img.write_to(&mut png, ImageOutputFormat::Png)
        .map_err(|e| e.to_string())?;
    let scale = original_width as f64 / img.width() as f64;
    let data = json!({"session":id.to_string(),"display":disp,"frame_id":frame.sequence,
        "coordinate_space":"display_relative_original_pixels",
        "image_to_display":{"offset_x":x,"offset_y":y,"scale_x":scale,"scale_y":original_height as f64 / img.height() as f64},
        "frame_age_ms":frame.captured.elapsed().as_millis(),"width":img.width(),"height":img.height(),
        "remote_width":frame.width,"remote_height":frame.height,"origin_x":x,"origin_y":y,
        "remote_pixels_per_image_pixel":scale,
        "scale_x":scale,"scale_y":original_height as f64 / img.height() as f64});
    let mut result = success(data);
    if let Some(content) = result["content"].as_array_mut() {
        content.push(
            json!({"type":"image","mimeType":"image/png","data":STANDARD.encode(png.into_inner())}),
        );
    }
    Ok(result)
}

pub(super) fn input_allowed(s: &FlutterSession) -> Result<(), String> {
    writable()?;
    if !session::allowed(s.lc.read().unwrap().get_id()) {
        return Err("Device access revoked".into());
    }
    session::ready(s)?;
    if !s.is_default()
        || !*s.server_keyboard_enabled.read().unwrap()
        || s.lc.read().unwrap().view_only.v
    {
        return Err("Remote keyboard/mouse permission is unavailable".into());
    }
    Ok(())
}

fn mouse(s: &FlutterSession, mask: i32, x: i32, y: i32) -> Result<(), String> {
    input_allowed(s)?;
    let mut msg = Message::new();
    msg.set_mouse_event(MouseEvent {
        mask,
        x,
        y,
        ..Default::default()
    });
    session::send(s, Data::Message(msg))
}

fn point(
    s: &FlutterSession,
    state: &SessionState,
    args: &Map<String, Value>,
    x: &str,
    y: &str,
) -> Result<(i32, i32), String> {
    let disp = display(s, state, args);
    let lc = s.lc.read().unwrap();
    let d = lc
        .peer_info
        .as_ref()
        .and_then(|p| p.displays.get(disp))
        .ok_or("Unknown display")?;
    rustdesk_agent_mcp::pixels::remote_point(
        (d.x, d.y),
        (d.width, d.height),
        (number(args, x, 0) as i32, number(args, y, 0) as i32),
    )
}

pub(super) fn input(
    s: &FlutterSession,
    state: &SessionState,
    name: &str,
    args: &Map<String, Value>,
) -> ToolResult {
    input_allowed(s)?;
    match name {
        "mouse_move" | "mouse_click" | "mouse_scroll" | "mouse_drag" => {
            let (x, y) = point(s, state, args, "x", "y")?;
            let end = if name == "mouse_drag" {
                Some(point(s, state, args, "to_x", "to_y")?)
            } else {
                None
            };
            mouse(s, MOUSE_TYPE_MOVE, x, y)?;
            if name == "mouse_click" {
                let button = match args.get("button").and_then(Value::as_str).unwrap_or("left") {
                    "left" => MOUSE_BUTTON_LEFT,
                    "right" => MOUSE_BUTTON_RIGHT,
                    "middle" => MOUSE_BUTTON_WHEEL,
                    _ => return Err("Unknown mouse button".into()),
                };
                for _ in 0..number(args, "clicks", 1) {
                    mouse(s, (button << 3) | MOUSE_TYPE_DOWN, x, y)?;
                    release_mouse(s, button, x, y)?;
                }
            } else if name == "mouse_scroll" {
                mouse(
                    s,
                    MOUSE_TYPE_WHEEL,
                    number(args, "dx", 0) as i32,
                    number(args, "dy", 0) as i32,
                )?;
            } else if let Some((end_x, end_y)) = end {
                mouse(s, (MOUSE_BUTTON_LEFT << 3) | MOUSE_TYPE_DOWN, x, y)?;
                let mut result = Ok(());
                let mut last = (x, y);
                for step in 1..=20 {
                    let pos = (
                        (x as i64 + (end_x as i64 - x as i64) * step / 20) as i32,
                        (y as i64 + (end_y as i64 - y as i64) * step / 20) as i32,
                    );
                    if let Err(e) = mouse(s, MOUSE_TYPE_MOVE, pos.0, pos.1) {
                        result = Err(e);
                        break;
                    }
                    last = pos;
                    std::thread::sleep(Duration::from_millis(
                        number(args, "duration_ms", 400) as u64 / 20,
                    ));
                }
                let release = release_mouse(s, MOUSE_BUTTON_LEFT, last.0, last.1);
                result?;
                release?;
            }
        }
        "keyboard_input" => {
            let mut key = KeyEvent {
                mode: KeyboardMode::Legacy.into(),
                press: true,
                ..Default::default()
            };
            key.set_seq(string(args, "text")?.into());
            let mut msg = Message::new();
            msg.set_key_event(key);
            session::send(s, Data::Message(msg))?;
        }
        "keyboard_hotkey" => {
            let keys = args
                .get("keys")
                .and_then(Value::as_array)
                .ok_or("Missing keys")?;
            let mut event = KeyEvent {
                mode: KeyboardMode::Legacy.into(),
                press: true,
                ..Default::default()
            };
            for modifier in &keys[..keys.len() - 1] {
                let key = match modifier
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "ctrl" | "control" => ControlKey::Control,
                    "alt" | "option" => ControlKey::Alt,
                    "shift" => ControlKey::Shift,
                    "meta" | "cmd" | "command" | "win" | "lwin" | "super" => ControlKey::Meta,
                    _ => {
                        return Err(
                            "Only Ctrl, Alt, Shift and Meta may precede the final key".into()
                        )
                    }
                };
                if event.modifiers.contains(&key.into()) {
                    return Err("Duplicate modifier".into());
                }
                event.modifiers.push(key.into());
            }
            let key = keys
                .last()
                .and_then(Value::as_str)
                .ok_or("Missing key")?
                .to_ascii_lowercase();
            if key.chars().count() == 1 {
                event.set_chr(key.chars().next().ok_or("Empty key")? as u32);
            } else {
                event.set_control_key(control(&key).ok_or("Unknown key")?);
            }
            if !event.modifiers.is_empty() && s.peer_platform() == "Windows" {
                hotkey::send(s, event)?;
            } else {
                let mut msg = Message::new();
                msg.set_key_event(event);
                session::send(s, Data::Message(msg))?;
            }
        }
        _ => return Err("Unsupported desktop action".into()),
    }
    state.events.push("tool_queued", json!({"tool":name}));
    Ok(success(json!({"queued":true})))
}

fn release_mouse(s: &FlutterSession, button: i32, x: i32, y: i32) -> Result<(), String> {
    // Cleanup is allowed even after MCP is disabled, to avoid leaving a remote button held.
    let mut msg = Message::new();
    msg.set_mouse_event(MouseEvent {
        mask: (button << 3) | MOUSE_TYPE_UP,
        x,
        y,
        ..Default::default()
    });
    s.sender
        .read()
        .unwrap()
        .as_ref()
        .ok_or("Session closed before button release")?
        .send(Data::Message(msg))
        .map_err(|_| "Unable to release remote mouse button".into())
}

fn control(key: &str) -> Option<ControlKey> {
    Some(match key {
        "meta" | "cmd" | "command" | "win" | "lwin" | "super" => ControlKey::Meta,
        "ctrl" | "control" => ControlKey::Control,
        "alt" | "option" => ControlKey::Alt,
        "shift" => ControlKey::Shift,
        "enter" | "return" => ControlKey::Return,
        "escape" | "esc" => ControlKey::Escape,
        "tab" => ControlKey::Tab,
        "space" => ControlKey::Space,
        "backspace" => ControlKey::Backspace,
        "delete" => ControlKey::Delete,
        "insert" => ControlKey::Insert,
        "home" => ControlKey::Home,
        "end" => ControlKey::End,
        "pageup" => ControlKey::PageUp,
        "pagedown" => ControlKey::PageDown,
        "up" => ControlKey::UpArrow,
        "down" => ControlKey::DownArrow,
        "left" => ControlKey::LeftArrow,
        "right" => ControlKey::RightArrow,
        "f1" => ControlKey::F1,
        "f2" => ControlKey::F2,
        "f3" => ControlKey::F3,
        "f4" => ControlKey::F4,
        "f5" => ControlKey::F5,
        "f6" => ControlKey::F6,
        "f7" => ControlKey::F7,
        "f8" => ControlKey::F8,
        "f9" => ControlKey::F9,
        "f10" => ControlKey::F10,
        "f11" => ControlKey::F11,
        "f12" => ControlKey::F12,
        _ => return None,
    })
}
