use super::*;
use dbus::{
    arg::{prop_cast, PropMap},
    blocking::{stdintf::org_freedesktop_dbus::Properties, Connection as Bus},
    Path,
};
use x11rb::{
    connection::Connection,
    protocol::{
        randr::ConnectionExt as _,
        xkb::ConnectionExt as _,
        xproto::{self, ConnectionExt as _},
        xtest::ConnectionExt as _,
    },
    rust_connection::RustConnection,
    CURRENT_TIME, NONE,
};

pub(super) struct Linux {
    x: RustConnection,
    screen: usize,
    system: Bus,
    session: Bus,
    display: String,
}
fn error(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn local_display(value: &str) -> Result<String, String> {
    let value = value.strip_prefix("unix/").unwrap_or(value);
    let parts: Vec<_> = value
        .strip_prefix(':')
        .ok_or(
            "Local X11 requires a Unix socket DISPLAY; SSH and network displays are not supported",
        )?
        .split('.')
        .collect();
    if parts.len() > 2
        || parts
            .iter()
            .any(|s| s.is_empty() || !s.bytes().all(|v| v.is_ascii_digit()))
    {
        return Err("Invalid local X11 DISPLAY".into());
    }
    Ok(format!(":{}", parts[0].parse::<u32>().map_err(error)?))
}
impl Linux {
    pub(super) fn new() -> Result<Self, String> {
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v != "x11")
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
        {
            return Err("Local Wayland control is not supported; use a logged-in X11 desktop (XWayland is not a full desktop provider)".into());
        }
        let display = local_display(
            &std::env::var("DISPLAY")
                .map_err(|_| "No local X11 DISPLAY; start the GUI in the logged-in desktop")?,
        )?;
        if unsafe {
            hbb_common::libc::getuid() == 0
                || hbb_common::libc::getuid() != hbb_common::libc::geteuid()
        } {
            return Err("Local X11 control must run as the logged-in unprivileged user".into());
        }
        let (x, screen) = x11rb::connect(None).map_err(error)?;
        let version = x
            .randr_query_version(1, 3)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if (version.major_version, version.minor_version) < (1, 3) {
            return Err("Local X11 requires RandR 1.3".into());
        }
        x.xtest_get_version(2, 2)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let xkb = x
            .xkb_use_extension(1, 0)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if !xkb.supported {
            return Err("Local X11 requires XKB".into());
        }
        let desktop = Self {
            x,
            screen,
            display,
            system: Bus::new_system().map_err(error)?,
            session: Bus::new_session().map_err(error)?,
        };
        desktop.snapshot()?;
        Ok(desktop)
    }
    fn root(&self) -> u32 {
        self.x.setup().roots[self.screen].root
    }
    fn identity(&self) -> Result<String, String> {
        let manager = self.system.with_proxy(
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            Duration::from_millis(500),
        );
        let by_pid: Result<(Path<'static>,), _> = manager.method_call(
            "org.freedesktop.login1.Manager",
            "GetSessionByPID",
            (std::process::id(),),
        );
        let (path,) = match by_pid {
            Ok(path) => path,
            Err(_) => {
                // Desktop launchers can be in the user's service manager cgroup. Verify the
                // inherited session against logind; never search for or select another login.
                let id = std::env::var("XDG_SESSION_ID")
                    .map_err(|_| "Cannot identify this X11 login via logind")?;
                manager
                    .method_call("org.freedesktop.login1.Manager", "GetSession", (id,))
                    .map_err(error)?
            }
        };
        let proxy =
            self.system
                .with_proxy("org.freedesktop.login1", path, Duration::from_millis(500));
        let props: PropMap = proxy
            .get_all("org.freedesktop.login1.Session")
            .map_err(error)?;
        let user: (u32, Path<'static>) = proxy
            .get("org.freedesktop.login1.Session", "User")
            .map_err(error)?;
        let uid = unsafe { hbb_common::libc::getuid() };
        validate_session(&props, user.0, uid, &self.display)?;
        let mut known_unlocked = false;
        for (service, path) in [
            (
                "org.freedesktop.ScreenSaver",
                "/org/freedesktop/ScreenSaver",
            ),
            ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
            ("org.xfce.ScreenSaver", "/org/xfce/ScreenSaver"),
        ] {
            let proxy = self
                .session
                .with_proxy(service, path, Duration::from_millis(300));
            let result: Result<(bool,), _> = proxy.method_call(service, "GetActive", ());
            match result {
                Ok((true,)) => {
                    return Err("Local X11 desktop is locked or its screen saver is active".into())
                }
                Ok((false,)) => known_unlocked = true,
                Err(e)
                    if matches!(
                        e.name(),
                        Some(
                            "org.freedesktop.DBus.Error.ServiceUnknown"
                                | "org.freedesktop.DBus.Error.NameHasNoOwner"
                                | "org.freedesktop.DBus.Error.UnknownMethod"
                                | "org.freedesktop.DBus.Error.UnknownObject"
                        )
                    ) => {}
                Err(e) => return Err(format!("Cannot determine X11 screen lock state: {e}")),
            }
        }
        if !known_unlocked {
            return Err("Cannot verify X11 lock state; a GNOME, KDE, or XFCE ScreenSaver D-Bus service is required".into());
        }
        let id = prop_cast::<String>(&props, "Id").ok_or("logind omitted session ID")?;
        Ok(format!("linux:{uid}:{id}:{}:{}", self.display, self.root()))
    }
    fn atom(&self, name: &[u8]) -> Result<u32, String> {
        Ok(self
            .x
            .intern_atom(false, name)
            .map_err(error)?
            .reply()
            .map_err(error)?
            .atom)
    }
    fn active_window(&self) -> Result<String, String> {
        let value = self
            .x
            .get_property(
                false,
                self.root(),
                self.atom(b"_NET_ACTIVE_WINDOW")?,
                xproto::AtomEnum::WINDOW,
                0,
                1,
            )
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let window = value.value32().and_then(|mut v| v.next()).unwrap_or(0);
        if window == 0 {
            return Ok(String::new());
        }
        let focus = self
            .x
            .get_input_focus()
            .map_err(error)?
            .reply()
            .map_err(error)?
            .focus;
        Ok(format!("x11:{window:x}:{focus:x}"))
    }
    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), String> {
        self.x
            .xtest_fake_input(kind, detail, CURRENT_TIME, self.root(), x, y, 0)
            .map_err(error)?
            .check()
            .map_err(error)
    }
    fn mapping(&self) -> Result<xproto::GetKeyboardMappingReply, String> {
        let setup = self.x.setup();
        self.x
            .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
            .map_err(error)?
            .reply()
            .map_err(error)
    }
    fn unicode(&self, ch: char) -> Result<(), String> {
        let sym = character_symbol(ch)?;
        let mapping = self.mapping()?;
        let width = usize::from(mapping.keysyms_per_keycode);
        if width == 0 {
            return Err("Invalid X11 keyboard map".into());
        }
        let (code, shifted) = mapped_character(&mapping.keysyms, width, self.x.setup().min_keycode, sym)
            .ok_or("Character is not present in the active X11 keyboard layout; local input does not remap the user's keyboard or clipboard")?;
        let shift = if shifted {
            Some(self.keycode("shift")? as u8)
        } else {
            None
        };
        let result = (|| {
            if let Some(shift) = shift {
                self.fake(xproto::KEY_PRESS_EVENT, shift, 0, 0)?;
            }
            self.fake(xproto::KEY_PRESS_EVENT, code, 0, 0)
        })();
        let released = self.fake(xproto::KEY_RELEASE_EVENT, code, 0, 0);
        let shift_released = if let Some(shift) = shift {
            self.fake(xproto::KEY_RELEASE_EVENT, shift, 0, 0)
        } else {
            Ok(())
        };
        shift_released?;
        released?;
        result
    }
}
fn character_symbol(ch: char) -> Result<u32, String> {
    Ok(match ch {
        '\n' | '\r' => 0xff0d,
        '\t' => 0xff09,
        '\u{8}' => 0xff08,
        _ if ch.is_control() => return Err("Unsupported X11 control character".into()),
        _ if ch as u32 <= 0xff => ch as u32,
        _ => 0x0100_0000 | ch as u32,
    })
}

fn mapped_character(symbols: &[u32], width: usize, minimum: u8, sym: u32) -> Option<(u8, bool)> {
    if width == 0 {
        return None;
    }
    for (i, row) in symbols.chunks_exact(width).enumerate() {
        if row[0] == sym {
            return Some((minimum.checked_add(u8::try_from(i).ok()?)?, false));
        }
        if row.get(1) == Some(&sym) {
            return Some((minimum.checked_add(u8::try_from(i).ok()?)?, true));
        }
    }
    None
}

fn validate_session(props: &PropMap, user: u32, uid: u32, display: &str) -> Result<(), String> {
    if user != uid
        || prop_cast::<String>(props, "Type").map(String::as_str) != Some("x11")
        || prop_cast::<String>(props, "Class").map(String::as_str) != Some("user")
        || prop_cast::<bool>(props, "Active") != Some(&true)
        || prop_cast::<bool>(props, "Remote") != Some(&false)
        || prop_cast::<bool>(props, "LockedHint") != Some(&false)
    {
        return Err(
            "Local X11 requires this user's active, unlocked, non-remote graphical login".into(),
        );
    }
    let current = prop_cast::<String>(props, "Display").ok_or("logind omitted X11 display")?;
    if local_display(current)? != display {
        return Err("X11 DISPLAY does not match the logged-in session".into());
    }
    Ok(())
}
impl Desktop for Linux {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let desktop_id = self.identity()?;
        let resources = self
            .x
            .randr_get_screen_resources_current(self.root())
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let primary = self
            .x
            .randr_get_output_primary(self.root())
            .map_err(error)?
            .reply()
            .map_err(error)?
            .output;
        let mut displays = Vec::new();
        for output in resources.outputs {
            let info = self
                .x
                .randr_get_output_info(output, resources.config_timestamp)
                .map_err(error)?
                .reply()
                .map_err(error)?;
            if info.crtc == NONE {
                continue;
            }
            let crtc = self
                .x
                .randr_get_crtc_info(info.crtc, resources.config_timestamp)
                .map_err(error)?
                .reply()
                .map_err(error)?;
            if crtc.width == 0 || crtc.height == 0 {
                continue;
            }
            displays.push(Display {
                id: output,
                name: String::from_utf8_lossy(&info.name).into_owned(),
                x: f64::from(crtc.x),
                y: f64::from(crtc.y),
                width: u32::from(crtc.width),
                height: u32::from(crtc.height),
                logical_width: f64::from(crtc.width),
                logical_height: f64::from(crtc.height),
                rotation: f64::from(u16::from(crtc.rotation)),
                primary: output == primary,
            });
        }
        displays.sort_by(|a, b| b.primary.cmp(&a.primary).then(a.id.cmp(&b.id)));
        let keyboard = self
            .x
            .xkb_get_state(256u16)
            .map_err(error)?
            .reply()
            .map_err(error)?;
        Ok(Snapshot {
            desktop_id,
            active_window: self.active_window()?,
            displays,
            capture_allowed: true,
            input_allowed: u16::from(keyboard.locked_mods) & 2 == 0
                && u8::from(keyboard.group) == 0,
            provider: "Linux X11 / RandR / XTEST (root framebuffer pixels)".into(),
        })
    }
    fn capture(&self, display: &Display) -> Result<RgbaImage, String> {
        let width = u16::try_from(display.width).map_err(|_| "X11 display too wide")?;
        let height = u16::try_from(display.height).map_err(|_| "X11 display too tall")?;
        if display.x < 0.0
            || display.y < 0.0
            || display.x > f64::from(i16::MAX)
            || display.y > f64::from(i16::MAX)
        {
            return Err("X11 display outside root coordinate range".into());
        }
        let reply = self
            .x
            .get_image(
                xproto::ImageFormat::Z_PIXMAP,
                self.root(),
                display.x as i16,
                display.y as i16,
                width,
                height,
                u32::MAX,
            )
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let setup = self.x.setup();
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == reply.depth)
            .ok_or("Unknown X11 pixel format")?;
        let visual = setup.roots[self.screen]
            .allowed_depths
            .iter()
            .flat_map(|depth| &depth.visuals)
            .find(|visual| visual.visual_id == reply.visual)
            .ok_or("Unknown X11 visual")?;
        if visual.class != xproto::VisualClass::TRUE_COLOR {
            return Err("Local X11 requires a TrueColor visual".into());
        }
        decode(
            &reply.data,
            display.width,
            display.height,
            format.bits_per_pixel,
            format.scanline_pad,
            setup.image_byte_order == xproto::ImageOrder::LSB_FIRST,
            [visual.red_mask, visual.green_mask, visual.blue_mask],
        )
    }
    fn idle(&self) -> Result<(), String> {
        let keys = self
            .x
            .query_keymap()
            .map_err(error)?
            .reply()
            .map_err(error)?;
        let pointer = self
            .x
            .query_pointer(self.root())
            .map_err(error)?
            .reply()
            .map_err(error)?;
        if keys.keys.iter().any(|b| *b != 0) || u16::from(pointer.mask) & 0x1f00 != 0 {
            return Err(
                "User input is already held; wait and observe before local automation".into(),
            );
        }
        Ok(())
    }
    fn keycode(&self, name: &str) -> Result<u16, String> {
        let sym = keysym(name)?;
        let map = self.mapping()?;
        if map.keysyms_per_keycode == 0 {
            return Err("Invalid X11 keyboard map".into());
        }
        // Hotkeys select an unshifted key; modifiers are explicit separate keys.
        map.keysyms
            .chunks_exact(usize::from(map.keysyms_per_keycode))
            .position(|row| row[0] == sym)
            .map(|i| u16::from(self.x.setup().min_keycode) + i as u16)
            .ok_or_else(|| format!("Key is absent from the active X11 layout: {name}"))
    }
    fn validate_text(&self, text: &str) -> Result<(), String> {
        let map = self.mapping()?;
        for ch in text.chars() {
            if mapped_character(
                &map.keysyms,
                usize::from(map.keysyms_per_keycode),
                self.x.setup().min_keycode,
                character_symbol(ch)?,
            )
            .is_none()
            {
                return Err("Text contains characters not directly mapped by the active X11 keyboard layout; nothing was typed".into());
            }
        }
        Ok(())
    }
    fn event(&self, event: Event) -> Result<(), String> {
        match event {
            Event::Move(x, y) => {
                if x < 0.0 || y < 0.0 || x > f64::from(i16::MAX) || y > f64::from(i16::MAX) {
                    return Err("Coordinates outside X11 input range".into());
                }
                self.fake(
                    xproto::MOTION_NOTIFY_EVENT,
                    0,
                    x.round() as i16,
                    y.round() as i16,
                )
            }
            Event::Button(button, down, _) => self.fake(
                if down {
                    xproto::BUTTON_PRESS_EVENT
                } else {
                    xproto::BUTTON_RELEASE_EVENT
                },
                button,
                0,
                0,
            ),
            Event::Key(code, down) => self.fake(
                if down {
                    xproto::KEY_PRESS_EVENT
                } else {
                    xproto::KEY_RELEASE_EVENT
                },
                u8::try_from(code).map_err(|_| "Invalid X11 keycode")?,
                0,
                0,
            ),
            Event::Unicode(ch) => self.unicode(ch),
            Event::Scroll(dx, dy) => {
                for (value, positive, negative) in [(dy, 4, 5), (dx, 6, 7)] {
                    if value != 0 {
                        let button = if value > 0 { positive } else { negative };
                        let press = self.fake(xproto::BUTTON_PRESS_EVENT, button, 0, 0);
                        let release = self.fake(xproto::BUTTON_RELEASE_EVENT, button, 0, 0);
                        release?;
                        press?;
                    }
                }
                Ok(())
            }
        }
    }
}
fn keysym(name: &str) -> Result<u32, String> {
    let name = name.to_ascii_lowercase();
    Ok(match name.as_str() {
        "ctrl" | "control" => 0xffe3,
        "shift" => 0xffe1,
        "alt" => 0xffe9,
        "meta" | "win" | "lwin" | "super" | "cmd" => 0xffeb,
        "enter" | "return" => 0xff0d,
        "esc" | "escape" => 0xff1b,
        "tab" => 0xff09,
        "space" => 0x20,
        "backspace" => 0xff08,
        "delete" => 0xffff,
        "insert" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" => 0xff55,
        "pagedown" => 0xff56,
        "left" => 0xff51,
        "up" => 0xff52,
        "right" => 0xff53,
        "down" => 0xff54,
        _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() => {
            u32::from(name.as_bytes()[0])
        }
        _ if name.starts_with('f') => {
            let n = name[1..].parse::<u32>().map_err(|_| "Unknown X11 key")?;
            if !(1..=24).contains(&n) {
                return Err("Unknown X11 function key".into());
            }
            0xffbd + n
        }
        _ => return Err(format!("Unknown X11 key: {name}")),
    })
}
fn decode(
    data: &[u8],
    width: u32,
    height: u32,
    bits: u8,
    pad: u8,
    little: bool,
    masks: [u32; 3],
) -> Result<RgbaImage, String> {
    if width == 0
        || height == 0
        || !matches!(bits, 16 | 24 | 32)
        || !matches!(pad, 8 | 16 | 32)
        || masks.contains(&0)
    {
        return Err("Unsupported X11 pixel layout".into());
    }
    let row = (u64::from(width) * u64::from(bits)).div_ceil(u64::from(pad)) * u64::from(pad) / 8;
    if row * u64::from(height) != data.len() as u64
        || u64::from(width) * u64::from(height) > 16 * 1024 * 1024
    {
        return Err("Invalid X11 frame size".into());
    }
    let mut image = RgbaImage::new(width, height);
    let bytes = usize::from(bits / 8);
    for (y, line) in data.chunks_exact(row as usize).enumerate() {
        for x in 0..width as usize {
            let mut pixel = 0u32;
            for (i, byte) in line[x * bytes..(x + 1) * bytes].iter().enumerate() {
                pixel |= u32::from(*byte) << (8 * if little { i } else { bytes - i - 1 });
            }
            let channel = |mask: u32| {
                (((u64::from(pixel & mask) >> mask.trailing_zeros()) * 255)
                    / u64::from(mask >> mask.trailing_zeros())) as u8
            };
            image.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([channel(masks[0]), channel(masks[1]), channel(masks[2]), 255]),
            );
        }
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires an explicitly opted-in disposable Xvfb display"]
    fn isolated_xvfb_capture_and_input_roundtrip() {
        assert_eq!(
            std::env::var("RUSTDESK_LOCAL_XVFB_TEST").as_deref(),
            Ok("1")
        );
        let (x, screen) = x11rb::connect(None).unwrap();
        let desktop = Linux {
            x,
            screen,
            system: Bus::new_session().unwrap(),
            session: Bus::new_session().unwrap(),
            display: ":test".into(),
        };
        let root = desktop.root();
        let geometry = desktop.x.get_geometry(root).unwrap().reply().unwrap();
        assert_eq!((geometry.width, geometry.height), (640, 480));
        desktop.x.xtest_get_version(2, 2).unwrap().reply().unwrap();
        desktop.x.xkb_use_extension(1, 0).unwrap().reply().unwrap();
        let window = desktop.x.generate_id().unwrap();
        desktop
            .x
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                10,
                10,
                80,
                60,
                0,
                xproto::WindowClass::INPUT_OUTPUT,
                0,
                &xproto::CreateWindowAux::new()
                    .background_pixel(0x112233)
                    .event_mask(xproto::EventMask::KEY_PRESS | xproto::EventMask::KEY_RELEASE),
            )
            .unwrap()
            .check()
            .unwrap();
        desktop.x.map_window(window).unwrap().check().unwrap();
        desktop
            .x
            .set_input_focus(xproto::InputFocus::PARENT, window, CURRENT_TIME)
            .unwrap()
            .check()
            .unwrap();
        let d = Display {
            id: 0,
            name: "Xvfb".into(),
            x: 0.0,
            y: 0.0,
            width: 640,
            height: 480,
            logical_width: 640.0,
            logical_height: 480.0,
            rotation: 0.0,
            primary: true,
        };
        let pixels = desktop.capture(&d).unwrap();
        assert_eq!(pixels.get_pixel(15, 15).0, [0x11, 0x22, 0x33, 255]);
        desktop.idle().unwrap();
        desktop.event(Event::Move(20.0, 30.0)).unwrap();
        let pointer = desktop.x.query_pointer(root).unwrap().reply().unwrap();
        assert_eq!((pointer.root_x, pointer.root_y), (20, 30));
        desktop.event(Event::Button(1, true, 1)).unwrap();
        assert!(desktop.idle().is_err());
        desktop.event(Event::Button(1, false, 1)).unwrap();
        let before = desktop.mapping().unwrap().keysyms;
        desktop.event(Event::Unicode('A')).unwrap();
        assert!(desktop.event(Event::Unicode('中')).is_err());
        assert_eq!(desktop.mapping().unwrap().keysyms, before);
        desktop.idle().unwrap();
        let mut presses = 0;
        let mut releases = 0;
        while let Some(event) = desktop.x.poll_for_event().unwrap() {
            match event {
                x11rb::protocol::Event::KeyPress(_) => presses += 1,
                x11rb::protocol::Event::KeyRelease(_) => releases += 1,
                _ => {}
            }
        }
        assert_eq!((presses, releases), (2, 2));
        desktop.x.destroy_window(window).unwrap().check().unwrap();
    }
    #[test]
    fn unicode_mapping_handles_shift_without_rewriting_the_layout() {
        assert_eq!(
            mapped_character(&[97, 65, 98, 66], 2, 8, 65),
            Some((8, true))
        );
        assert_eq!(
            mapped_character(&[97, 65, 98, 66], 2, 8, 98),
            Some((9, false))
        );
        assert_eq!(mapped_character(&[97, 65], 2, 8, 0x01004e2d), None);
    }
    #[test]
    fn local_socket_only_and_matching_display() {
        assert_eq!(local_display(":0.0").unwrap(), ":0");
        assert_eq!(local_display("unix/:12").unwrap(), ":12");
        for value in ["localhost:10.0", "192.168.1.2:0", ":", ":0.bad", ":0.0.1"] {
            assert!(local_display(value).is_err());
        }
    }
    #[test]
    fn x11_pixels_handle_padding_byte_order_and_color_depth() {
        let little = decode(
            &[0, 128, 255, 0],
            1,
            1,
            32,
            32,
            true,
            [0xff0000, 0xff00, 0xff],
        )
        .unwrap();
        assert_eq!(little.get_pixel(0, 0).0, [255, 128, 0, 255]);
        let big = decode(
            &[255, 128, 0, 0],
            1,
            1,
            24,
            32,
            false,
            [0xff0000, 0xff00, 0xff],
        )
        .unwrap();
        assert_eq!(big, little);
        assert_eq!(
            decode(&[0, 248], 1, 1, 16, 16, true, [0xf800, 0x7e0, 0x1f])
                .unwrap()
                .get_pixel(0, 0)
                .0,
            [255, 0, 0, 255]
        );
        assert!(decode(&[0], 2, 1, 32, 32, true, [1, 2, 4]).is_err());
    }
    #[test]
    fn lock_identity_and_wayland_fail_closed() {
        use dbus::arg::Variant;
        let mut props = PropMap::new();
        props.insert("Type".into(), Variant(Box::new("x11".to_owned())));
        props.insert("Class".into(), Variant(Box::new("user".to_owned())));
        props.insert("Display".into(), Variant(Box::new(":0".to_owned())));
        props.insert("Active".into(), Variant(Box::new(true)));
        props.insert("Remote".into(), Variant(Box::new(false)));
        props.insert("LockedHint".into(), Variant(Box::new(false)));
        assert!(validate_session(&props, 1000, 1000, ":0").is_ok());
        assert!(validate_session(&props, 1001, 1000, ":0").is_err());
        assert!(validate_session(&props, 1000, 1000, ":1").is_err());
        props.insert("LockedHint".into(), Variant(Box::new(true)));
        assert!(validate_session(&props, 1000, 1000, ":0").is_err());
        props.insert("LockedHint".into(), Variant(Box::new(false)));
        props.insert("Type".into(), Variant(Box::new("wayland".to_owned())));
        assert!(validate_session(&props, 1000, 1000, ":0").is_err());
    }
}
