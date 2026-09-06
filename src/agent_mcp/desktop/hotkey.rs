use super::*;

pub(super) fn send(s: &FlutterSession, event: KeyEvent) -> Result<(), String> {
    let sender = s
        .sender
        .read()
        .unwrap()
        .clone()
        .ok_or("Session transport is not ready")?;
    queue(event, |key| {
        if key.down {
            input_allowed(s)?;
        }
        // Releases must still be queued if permission is revoked after a key-down.
        let mut msg = Message::new();
        msg.set_key_event(key);
        sender
            .send(Data::Message(msg))
            .map_err(|_| "Session transport is closed".into())
    })
}

fn queue(
    mut event: KeyEvent,
    mut send: impl FnMut(KeyEvent) -> Result<(), String>,
) -> Result<(), String> {
    let mut downs = Vec::new();
    for modifier in &event.modifiers {
        let mut key = KeyEvent {
            mode: KeyboardMode::Legacy.into(),
            down: true,
            modifiers: event.modifiers[..downs.len()].to_vec(),
            ..Default::default()
        };
        key.set_control_key(modifier.enum_value().map_err(|_| "Unknown modifier")?);
        downs.push(key);
    }
    if let Some(key_event::Union::Chr(chr)) = event.union {
        if let Some(chr) = char::from_u32(chr).filter(char::is_ascii_alphanumeric) {
            // Windows Translate mode encodes a virtual key in the high word. This
            // avoids layout/IME character injection turning Win+R into a lone Win.
            event.mode = KeyboardMode::Translate.into();
            event.set_chr((chr.to_ascii_uppercase() as u32) << 16);
        }
    }
    event.down = true;
    event.press = false;
    downs.push(event);

    let mut pressed = Vec::new();
    let mut result = Ok(());
    for key in downs {
        if let Err(err) = send(key.clone()) {
            result = Err(err);
            break;
        }
        pressed.push(key);
    }
    // The main key comes up before its modifiers, including on partial failure.
    for mut key in pressed.into_iter().rev() {
        key.down = false;
        if let Err(err) = send(key) {
            result = Err(match result {
                Ok(()) => format!("Unable to release hotkey: {err}"),
                Err(previous) => format!("{previous}; unable to release hotkey: {err}"),
            });
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_dispatch_preserves_single_keys_and_non_windows_peers() {
        if std::env::var_os("RUSTDESK_MCP_HOTKEY_TEST_CHILD").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agent_mcp::desktop::hotkey::tests::desktop_dispatch_preserves_single_keys_and_non_windows_peers",
                    "--test-threads=1",
                ])
                .env("RUSTDESK_MCP_HOTKEY_TEST_CHILD", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use hbb_common::config;
        *config::APP_NAME.write().unwrap() = format!("RustDeskHotkeyTest-{}", SessionID::new_v4());
        config::OVERWRITE_LOCAL_SETTINGS.write().unwrap().extend([
            ("enable-agent-mcp".into(), "Y".into()),
            ("agent-mcp-read-only".into(), "N".into()),
            ("agent-mcp-devices".into(), String::new()),
        ]);
        let s = FlutterSession::default();
        *s.server_keyboard_enabled.write().unwrap() = true;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        *s.sender.write().unwrap() = Some(tx);
        let state = SessionState::default();
        for platform in ["Windows", "Linux", "Mac OS"] {
            {
                let mut lc = s.lc.write().unwrap();
                lc.get_config().info.platform = platform.into();
                lc.peer_info = Some(PeerInfo {
                    platform: platform.into(),
                    ..Default::default()
                });
            }
            for keys in [
                json!(["Meta", "r"]),
                json!(["Ctrl", "c"]),
                json!(["Alt", "Tab"]),
                json!(["Escape"]),
            ] {
                let args = json!({"keys":keys});
                super::super::input(&s, &state, "keyboard_hotkey", args.as_object().unwrap())
                    .unwrap();
                let mut sent = Vec::new();
                while let Ok(data) = rx.try_recv() {
                    let Data::Message(msg) = data else {
                        panic!("Expected key message")
                    };
                    sent.push(msg.key_event().clone());
                }
                if platform == "Windows" && keys.as_array().unwrap().len() > 1 {
                    assert_eq!(sent.len(), 4);
                    assert!(sent[0].down && sent[1].down);
                    assert!(!sent[2].down && !sent[3].down);
                } else {
                    assert_eq!(sent.len(), 1);
                    assert!(sent[0].press);
                    assert_eq!(sent[0].mode.enum_value().unwrap(), KeyboardMode::Legacy);
                }
            }
            let invalid = json!({"keys":["Ctrl", "control", "c"]});
            assert!(super::super::input(
                &s,
                &state,
                "keyboard_hotkey",
                invalid.as_object().unwrap()
            )
            .is_err());
            assert!(rx.try_recv().is_err());
        }
    }

    fn chord(modifiers: &[ControlKey], key: key_event::Union) -> KeyEvent {
        KeyEvent {
            mode: KeyboardMode::Legacy.into(),
            press: true,
            modifiers: modifiers.iter().copied().map(Into::into).collect(),
            union: Some(key),
            ..Default::default()
        }
    }

    fn events(event: KeyEvent) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        queue(event, |key| {
            events.push(key);
            Ok(())
        })
        .unwrap();
        events
    }

    #[test]
    fn windows_meta_r_holds_win_around_a_virtual_r_press() {
        let keys = events(chord(
            &[ControlKey::Meta],
            key_event::Union::Chr('r' as u32),
        ));
        assert_eq!(keys.len(), 4);
        assert_eq!(keys[0].control_key(), ControlKey::Meta);
        assert!(keys[0].modifiers.is_empty());
        assert_eq!(keys[1].mode.enum_value().unwrap(), KeyboardMode::Translate);
        assert_eq!(keys[1].chr(), 0x52 << 16);
        assert_eq!(keys[2].chr(), 0x52 << 16);
        assert_eq!(keys[3].control_key(), ControlKey::Meta);
        assert_eq!(
            keys.iter().map(|key| key.down).collect::<Vec<_>>(),
            [true, true, false, false]
        );
        assert!(keys.iter().all(|key| !key.press));
    }

    #[test]
    fn control_shift_c_preserves_modifiers_and_releases_in_reverse() {
        let keys = events(chord(
            &[ControlKey::Control, ControlKey::Shift],
            key_event::Union::Chr('c' as u32),
        ));
        assert_eq!(keys.len(), 6);
        assert_eq!(keys[0].control_key(), ControlKey::Control);
        assert_eq!(keys[1].control_key(), ControlKey::Shift);
        assert_eq!(keys[1].modifiers, [ControlKey::Control.into()]);
        assert_eq!(
            keys[2].modifiers,
            [ControlKey::Control.into(), ControlKey::Shift.into()]
        );
        assert_eq!(keys[2].chr(), 0x43 << 16);
        assert_eq!(keys[3].union, keys[2].union);
        assert_eq!(keys[4].control_key(), ControlKey::Shift);
        assert_eq!(keys[5].control_key(), ControlKey::Control);
        assert!(keys[3..].iter().all(|key| !key.down));
    }

    #[test]
    fn alt_tab_and_layout_specific_characters_keep_legacy_encoding() {
        let keys = events(chord(
            &[ControlKey::Alt],
            key_event::Union::ControlKey(ControlKey::Tab.into()),
        ));
        assert_eq!(keys[1].control_key(), ControlKey::Tab);
        assert_eq!(keys[1].mode.enum_value().unwrap(), KeyboardMode::Legacy);
        for chr in ['/', 'é'] {
            let keys = events(chord(
                &[ControlKey::Meta],
                key_event::Union::Chr(chr as u32),
            ));
            assert_eq!(keys[1].chr(), chr as u32);
            assert_eq!(keys[1].mode.enum_value().unwrap(), KeyboardMode::Legacy);
            assert_eq!(keys[1].modifiers, [ControlKey::Meta.into()]);
        }
    }

    #[test]
    fn failure_at_each_down_releases_only_successfully_queued_keys() {
        for fail_at in 0..3 {
            let mut delivered = Vec::new();
            let mut attempted = 0;
            let result = queue(
                chord(
                    &[ControlKey::Control, ControlKey::Shift],
                    key_event::Union::Chr('c' as u32),
                ),
                |key| {
                    if key.down {
                        let index = attempted;
                        attempted += 1;
                        if index == fail_at {
                            return Err("Device access revoked".into());
                        }
                    }
                    delivered.push(key);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), "Device access revoked");
            assert_eq!(delivered.len(), fail_at * 2);
            for index in 0..fail_at {
                let up = &delivered[delivered.len() - 1 - index];
                assert!(!up.down);
                assert_eq!(up.union, delivered[index].union);
            }
        }
    }

    #[test]
    fn release_failure_does_not_skip_remaining_modifiers() {
        let mut releases = Vec::new();
        let result = queue(
            chord(
                &[ControlKey::Control, ControlKey::Shift],
                key_event::Union::Chr('c' as u32),
            ),
            |key| {
                if !key.down {
                    releases.push(key);
                    return Err("transport closed".into());
                }
                Ok(())
            },
        );
        assert!(result.unwrap_err().contains("Unable to release hotkey"));
        assert_eq!(releases.len(), 3);
    }
}
