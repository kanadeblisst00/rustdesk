use super::*;
use std::{
    cell::Cell,
    ffi::{c_char, CStr},
};

extern "C" {
    fn mcp_local_mac_snapshot() -> *mut c_char;
    fn mcp_local_mac_capture(id: u32, width: u32, height: u32, rgba: *mut u8) -> i32;
    fn mcp_local_mac_idle() -> bool;
    fn mcp_local_mac_event(
        kind: u32,
        code: u16,
        down: bool,
        x: f64,
        y: f64,
        flags: u64,
        clicks: u32,
        text: *const u16,
        length: usize,
    ) -> bool;
}

pub(super) struct Mac {
    flags: Cell<u64>,
    point: Cell<(f64, f64)>,
    button: Cell<u8>,
}
impl Mac {
    pub(super) fn new() -> Result<Self, String> {
        let desktop = Self {
            flags: Cell::new(0),
            point: Cell::new((0.0, 0.0)),
            button: Cell::new(0),
        };
        desktop.snapshot()?;
        Ok(desktop)
    }
}
impl Desktop for Mac {
    fn snapshot(&self) -> Result<Snapshot, String> {
        unsafe {
            let pointer = mcp_local_mac_snapshot();
            if pointer.is_null() {
                return Err("Cannot inspect macOS desktop".into());
            }
            let parsed = serde_json::from_slice::<Value>(CStr::from_ptr(pointer).to_bytes());
            hbb_common::libc::free(pointer as _);
            let value = parsed.map_err(|e| e.to_string())?;
            if let Some(error) = value.get("error").and_then(Value::as_str) {
                return Err(error.into());
            }
            serde_json::from_value(value).map_err(|e| e.to_string())
        }
    }
    fn capture(&self, display: &Display) -> Result<RgbaImage, String> {
        let length = u64::from(display.width) * u64::from(display.height) * 4;
        if length == 0 || length > 64 * 1024 * 1024 {
            return Err("Local frame exceeds 64 MiB pixel limit".into());
        }
        let mut pixels = vec![0; length as usize];
        let result = unsafe {
            mcp_local_mac_capture(
                display.id,
                display.width,
                display.height,
                pixels.as_mut_ptr(),
            )
        };
        if result != 0 {
            return Err(format!("ScreenCaptureKit capture failed ({result}); check Screen Recording permission and the active display"));
        }
        RgbaImage::from_raw(display.width, display.height, pixels)
            .ok_or_else(|| "Invalid macOS frame".into())
    }
    fn idle(&self) -> Result<(), String> {
        if unsafe { mcp_local_mac_idle() } {
            Ok(())
        } else {
            Err("User input is already held; wait and observe before local automation".into())
        }
    }
    fn keycode(&self, name: &str) -> Result<u16, String> {
        keycode(name)
    }
    fn event(&self, event: Event) -> Result<(), String> {
        let (mut x, mut y) = self.point.get();
        let (kind, code, down, clicks, text) = match event {
            Event::Move(xx, yy) => {
                x = xx;
                y = yy;
                self.point.set((x, y));
                (0, u16::from(self.button.get()), false, 0, Vec::new())
            }
            Event::Button(code, down, clicks) => {
                self.button.set(if down { code } else { 0 });
                (1, u16::from(code), down, clicks, Vec::new())
            }
            Event::Key(code, down) => {
                let mask = match code {
                    56 | 60 => 1 << 17,
                    59 | 62 => 1 << 18,
                    58 | 61 => 1 << 19,
                    54 | 55 => 1 << 20,
                    _ => 0,
                };
                self.flags.set(if down {
                    self.flags.get() | mask
                } else {
                    self.flags.get() & !mask
                });
                (2, code, down, 0, Vec::new())
            }
            Event::Unicode(ch) => (3, 0, false, 0, ch.encode_utf16(&mut [0; 2]).to_vec()),
            Event::Scroll(dx, dy) => {
                x = f64::from(dx);
                y = f64::from(dy);
                (4, 0, false, 0, Vec::new())
            }
        };
        if unsafe {
            mcp_local_mac_event(
                kind,
                code,
                down,
                x,
                y,
                self.flags.get(),
                clicks,
                text.as_ptr(),
                text.len(),
            )
        } {
            Ok(())
        } else {
            Err("macOS rejected event creation or input permission; action may be partial".into())
        }
    }
}

fn keycode(name: &str) -> Result<u16, String> {
    let key = name.to_ascii_lowercase();
    // Virtual key positions match the existing remote hotkey contract; text uses Unicode.
    let code = match key.as_str() {
        "a" => 0,
        "s" => 1,
        "d" => 2,
        "f" => 3,
        "h" => 4,
        "g" => 5,
        "z" => 6,
        "x" => 7,
        "c" => 8,
        "v" => 9,
        "b" => 11,
        "q" => 12,
        "w" => 13,
        "e" => 14,
        "r" => 15,
        "y" => 16,
        "t" => 17,
        "1" => 18,
        "2" => 19,
        "3" => 20,
        "4" => 21,
        "6" => 22,
        "5" => 23,
        "9" => 25,
        "7" => 26,
        "8" => 28,
        "0" => 29,
        "o" => 31,
        "u" => 32,
        "i" => 34,
        "p" => 35,
        "l" => 37,
        "j" => 38,
        "k" => 40,
        "n" => 45,
        "m" => 46,
        "enter" | "return" => 36,
        "tab" => 48,
        "space" => 49,
        "backspace" => 51,
        "escape" | "esc" => 53,
        "meta" | "cmd" | "command" | "super" | "win" | "lwin" => 55,
        "shift" => 56,
        "alt" | "option" => 58,
        "ctrl" | "control" => 59,
        "delete" => 117,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        "f13" => 105,
        "f14" => 107,
        "f15" => 113,
        "f16" => 106,
        "f17" => 64,
        "f18" => 79,
        "f19" => 80,
        "f20" => 90,
        _ => return Err(format!("Unsupported macOS local key: {name}")),
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_key_positions_and_aliases() {
        assert_eq!(super::keycode("command").unwrap(), 55);
        assert_eq!(super::keycode("A").unwrap(), 0);
        assert_eq!(super::keycode("Delete").unwrap(), 117);
        assert!(super::keycode("F24").is_err());
    }
}
