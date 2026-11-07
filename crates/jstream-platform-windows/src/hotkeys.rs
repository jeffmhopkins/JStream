//! Global hotkeys: the desktop app hears the collection's bound chords while another
//! application has the focus ([ADR-0112](../../../docs/adr/0112-assignable-camera-zoom-bindings.md)
//! §5 and §6).
//!
//! Each chord is registered with `RegisterHotKey`, the OS's single-chord API, so the
//! process never sees any other key. Registration belongs to the thread that made it, so
//! one listener thread registers, waits in its message loop and hands each press to a
//! callback. A chord another application holds already can't be registered: [`set`]
//! returns it, and it is heard only while JStream has the focus.
//!
//! The key names are the protocol's chord keys (W3C `KeyboardEvent.code` values). The
//! writing-system keys are physical keys, so they go through their scan code and the
//! keyboard layout's virtual key at registration; the rest have fixed virtual keys. Both
//! tables and the chord parsing are pure and tested on every platform.
//!
//! [`set`]: GlobalHotkeys::set
//!
//! Docs: crates/jstream-platform-windows/README.md#purpose

/// How a chord's key reaches a virtual key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    /// A physical key, by its set 1 scan code: the layout decides its virtual key.
    Scan(u32),
    /// A key with the same virtual key on every layout.
    Virtual(u32),
}

/// The modifiers and key of one chord, as `RegisterHotKey` takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyChord {
    /// Ctrl is held.
    pub ctrl: bool,
    /// Alt is held.
    pub alt: bool,
    /// Shift is held.
    pub shift: bool,
    /// The Windows key is held (`Meta`).
    pub win: bool,
    /// The key.
    pub key: KeyCode,
}

/// The chord `Ctrl+Alt+Shift+Meta+<code>` (any of the modifiers, in that order), or
/// `None` when it isn't one or its key can't be registered. `NumpadEnter` can't: Windows
/// gives it Enter's virtual key, so registering it would take Enter.
pub fn parse_chord(chord: &str) -> Option<HotkeyChord> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = key_code(parts.pop()?)?;
    let mut parsed = HotkeyChord {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        key,
    };
    for part in parts {
        let held = match part {
            "Ctrl" => &mut parsed.ctrl,
            "Alt" => &mut parsed.alt,
            "Shift" => &mut parsed.shift,
            "Meta" => &mut parsed.win,
            _ => return None,
        };
        if *held {
            return None;
        }
        *held = true;
    }
    Some(parsed)
}

/// A chord key's scan code or virtual key.
pub fn key_code(code: &str) -> Option<KeyCode> {
    if let Some(scan) = scan_code(code) {
        return Some(KeyCode::Scan(scan));
    }
    virtual_key(code).map(KeyCode::Virtual)
}

/// The set 1 scan code of a writing-system key, a digit or the space bar.
fn scan_code(code: &str) -> Option<u32> {
    const LETTERS: [(char, u32); 26] = [
        ('A', 0x1E),
        ('B', 0x30),
        ('C', 0x2E),
        ('D', 0x20),
        ('E', 0x12),
        ('F', 0x21),
        ('G', 0x22),
        ('H', 0x23),
        ('I', 0x17),
        ('J', 0x24),
        ('K', 0x25),
        ('L', 0x26),
        ('M', 0x32),
        ('N', 0x31),
        ('O', 0x18),
        ('P', 0x19),
        ('Q', 0x10),
        ('R', 0x13),
        ('S', 0x1F),
        ('T', 0x14),
        ('U', 0x16),
        ('V', 0x2F),
        ('W', 0x11),
        ('X', 0x2D),
        ('Y', 0x15),
        ('Z', 0x2C),
    ];
    if let Some(letter) = code.strip_prefix("Key") {
        let mut chars = letter.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        return LETTERS.iter().find(|(l, _)| *l == c).map(|(_, scan)| *scan);
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let d: u32 = digit.parse().ok().filter(|d| *d <= 9 && digit.len() == 1)?;
        // Digit1 to Digit9 are 0x02 to 0x0A, and Digit0 follows them.
        return Some(if d == 0 { 0x0B } else { d + 1 });
    }
    Some(match code {
        "Minus" => 0x0C,
        "Equal" => 0x0D,
        "BracketLeft" => 0x1A,
        "BracketRight" => 0x1B,
        "Semicolon" => 0x27,
        "Quote" => 0x28,
        "Backquote" => 0x29,
        "Backslash" => 0x2B,
        "Comma" => 0x33,
        "Period" => 0x34,
        "Slash" => 0x35,
        "Space" => 0x39,
        _ => return None,
    })
}

/// The virtual key of a key that has the same one on every layout.
fn virtual_key(code: &str) -> Option<u32> {
    if let Some(n) = code.strip_prefix('F') {
        let n: u32 = n.parse().ok().filter(|n| (1..=24).contains(n))?;
        // VK_F1 is 0x70; VK_F24 is 0x87.
        return Some(0x6F + n);
    }
    if let Some(n) = code.strip_prefix("Numpad")
        && let Ok(n) = n.parse::<u32>()
    {
        // VK_NUMPAD0 is 0x60.
        return (n <= 9).then_some(0x60 + n);
    }
    Some(match code {
        "Enter" => 0x0D,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Escape" => 0x1B,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "PrintScreen" => 0x2C,
        "ScrollLock" => 0x91,
        "Pause" => 0x13,
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        "MediaTrackNext" => 0xB0,
        "MediaTrackPrevious" => 0xB1,
        "MediaStop" => 0xB2,
        "MediaPlayPause" => 0xB3,
        "AudioVolumeMute" => 0xAD,
        "AudioVolumeDown" => 0xAE,
        "AudioVolumeUp" => 0xAF,
        _ => return None,
    })
}

#[cfg(windows)]
pub use listener::GlobalHotkeys;

#[cfg(windows)]
mod listener {
    use super::{HotkeyChord, KeyCode, parse_chord};
    use jstream_platform::PlatformError;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, MAPVK_VSC_TO_VK_EX, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
        MOD_WIN, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY, WM_QUIT,
    };

    /// The message that tells the listener thread a new set of chords is waiting.
    const WM_SET: u32 = WM_APP + 1;

    /// A request to the listener thread: register these chords in place of the last
    /// ones, and answer with the ones that couldn't be.
    type Request = (Vec<String>, mpsc::Sender<Vec<(String, String)>>);

    /// The listener thread. Dropping it unregisters every chord and ends the thread.
    pub struct GlobalHotkeys {
        thread_id: u32,
        requests: mpsc::Sender<Request>,
        thread: Option<JoinHandle<()>>,
    }

    impl GlobalHotkeys {
        /// Starts the listener thread, with nothing registered. `on_press` runs on that
        /// thread with the chord, as [`set`](Self::set) named it, each time it is
        /// pressed; it must return quickly.
        ///
        /// # Errors
        /// [`PlatformError::Os`] when the thread can't be started.
        pub fn start(
            on_press: impl Fn(&str) + Send + 'static,
        ) -> Result<GlobalHotkeys, PlatformError> {
            let (requests, incoming) = mpsc::channel::<Request>();
            let (ready, started) = mpsc::channel::<u32>();
            let thread = std::thread::Builder::new()
                .name("jstream-hotkeys".into())
                .spawn(move || run(&incoming, &ready, &on_press))
                .map_err(|error| PlatformError::Os {
                    call: "std::thread::spawn",
                    code: None,
                    message: error.to_string(),
                })?;
            let thread_id = started.recv().map_err(|_| PlatformError::Os {
                call: "GetCurrentThreadId",
                code: None,
                message: "the hotkey thread ended before it started".into(),
            })?;
            Ok(GlobalHotkeys {
                thread_id,
                requests,
                thread: Some(thread),
            })
        }

        /// Registers `chords` in place of the ones registered before, and returns each
        /// chord that couldn't be registered with the reason: one another application
        /// holds, one whose key can't be registered, or one that isn't a chord.
        pub fn set(&self, chords: Vec<String>) -> Vec<(String, String)> {
            let (reply, answer) = mpsc::channel();
            let all = chords.clone();
            if self.requests.send((chords, reply)).is_err() || !self.wake(WM_SET) {
                return all
                    .into_iter()
                    .map(|chord| (chord, "the hotkey thread has stopped".into()))
                    .collect();
            }
            answer.recv().unwrap_or_else(|_| {
                all.into_iter()
                    .map(|chord| (chord, "the hotkey thread has stopped".into()))
                    .collect()
            })
        }

        /// Posts `message` to the listener thread; `false` if it has no queue any more.
        fn wake(&self, message: u32) -> bool {
            // SAFETY: PostThreadMessageW takes plain values; a thread ID that no longer
            // has a queue only makes it fail.
            unsafe { PostThreadMessageW(self.thread_id, message, WPARAM(0), LPARAM(0)) }.is_ok()
        }
    }

    impl Drop for GlobalHotkeys {
        fn drop(&mut self) {
            if self.wake(WM_QUIT)
                && let Some(thread) = self.thread.take()
            {
                let _ = thread.join();
            }
        }
    }

    /// The listener thread: makes its message queue, says its ID, then registers what it
    /// is asked to and hands each press to `on_press` until `WM_QUIT`.
    fn run(incoming: &mpsc::Receiver<Request>, ready: &mpsc::Sender<u32>, on_press: &dyn Fn(&str)) {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid MSG to write into; peeking creates this thread's
        // message queue, so a message posted after `ready` is never lost.
        let _ = unsafe { PeekMessageW(&raw mut msg, None, 0, 0, PM_NOREMOVE) };
        // SAFETY: no arguments; it returns this thread's ID.
        let thread_id = unsafe { GetCurrentThreadId() };
        if ready.send(thread_id).is_err() {
            return;
        }
        let mut registered: Vec<String> = Vec::new();
        loop {
            // SAFETY: `msg` is a valid MSG to write into. A null window reads this
            // thread's messages, where `RegisterHotKey(None, …)` posts WM_HOTKEY.
            let got = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
            if got.0 <= 0 {
                break;
            }
            match msg.message {
                WM_HOTKEY => {
                    // WM_HOTKEY's wParam is the ID `register` gave: the chord's index.
                    if let Some(chord) = registered.get(msg.wParam.0) {
                        on_press(chord);
                    }
                }
                WM_SET => {
                    while let Ok((chords, reply)) = incoming.try_recv() {
                        unregister(registered.len());
                        let (kept, failed) = register(chords);
                        registered = kept;
                        let _ = reply.send(failed);
                    }
                }
                _ => {}
            }
        }
        unregister(registered.len());
    }

    /// Registers each chord under its index, returning those registered (each at its
    /// ID) and those that failed with the reason.
    fn register(chords: Vec<String>) -> (Vec<String>, Vec<(String, String)>) {
        let mut kept = Vec::new();
        let mut failed = Vec::new();
        for chord in chords {
            let Some(parsed) = parse_chord(&chord) else {
                failed.push((chord, "its key can't be registered with Windows".into()));
                continue;
            };
            let Some(vk) = virtual_key(parsed.key) else {
                failed.push((chord, "this keyboard layout has no key there".into()));
                continue;
            };
            let Ok(id) = i32::try_from(kept.len()) else {
                failed.push((chord, "too many hotkeys".into()));
                continue;
            };
            // SAFETY: plain values; a null window posts WM_HOTKEY to this thread.
            match unsafe { RegisterHotKey(None, id, modifiers(parsed), vk) } {
                Ok(()) => kept.push(chord),
                Err(error) => failed.push((
                    chord,
                    format!("another application holds it ({})", error.message()),
                )),
            }
        }
        (kept, failed)
    }

    /// Unregisters IDs `0..count`, as [`register`] gave them.
    fn unregister(count: usize) {
        for id in 0..count {
            if let Ok(id) = i32::try_from(id) {
                // SAFETY: plain values; an ID this thread didn't register only fails.
                let _ = unsafe { UnregisterHotKey(None, id) };
            }
        }
    }

    fn modifiers(chord: HotkeyChord) -> HOT_KEY_MODIFIERS {
        let mut held = MOD_NOREPEAT;
        for (on, flag) in [
            (chord.ctrl, MOD_CONTROL),
            (chord.alt, MOD_ALT),
            (chord.shift, MOD_SHIFT),
            (chord.win, MOD_WIN),
        ] {
            if on {
                held |= flag;
            }
        }
        held
    }

    /// The virtual key for a key on the current keyboard layout.
    fn virtual_key(key: KeyCode) -> Option<u32> {
        match key {
            KeyCode::Virtual(vk) => Some(vk),
            KeyCode::Scan(scan) => {
                // SAFETY: plain values; an unmapped scan code returns 0.
                let vk = unsafe { MapVirtualKeyW(scan, MAPVK_VSC_TO_VK_EX) };
                (vk != 0).then_some(vk)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HotkeyChord, KeyCode, key_code, parse_chord};
    use jstream_protocol::Chord;

    #[test]
    fn every_chord_key_but_numpad_enter_can_be_registered() {
        for code in Chord::KEYS {
            let mapped = key_code(code);
            if *code == "NumpadEnter" {
                assert_eq!(mapped, None, "NumpadEnter would take Enter");
            } else {
                assert!(mapped.is_some(), "{code} has no key");
            }
        }
    }

    #[test]
    fn keys_map_to_their_scan_codes_or_virtual_keys() {
        assert_eq!(key_code("KeyA"), Some(KeyCode::Scan(0x1E)));
        assert_eq!(key_code("KeyQ"), Some(KeyCode::Scan(0x10)));
        assert_eq!(key_code("Digit1"), Some(KeyCode::Scan(0x02)));
        assert_eq!(key_code("Digit0"), Some(KeyCode::Scan(0x0B)));
        assert_eq!(key_code("Space"), Some(KeyCode::Scan(0x39)));
        assert_eq!(key_code("F1"), Some(KeyCode::Virtual(0x70)));
        assert_eq!(key_code("F24"), Some(KeyCode::Virtual(0x87)));
        assert_eq!(key_code("Numpad7"), Some(KeyCode::Virtual(0x67)));
        assert_eq!(key_code("MediaPlayPause"), Some(KeyCode::Virtual(0xB3)));
        assert_eq!(key_code("F25"), None);
        assert_eq!(key_code("KeyAB"), None);
        assert_eq!(key_code("Digit10"), None);
    }

    #[test]
    fn a_chord_parses_into_its_modifiers_and_key() {
        assert_eq!(
            parse_chord("Ctrl+Shift+F9"),
            Some(HotkeyChord {
                ctrl: true,
                alt: false,
                shift: true,
                win: false,
                key: KeyCode::Virtual(0x78),
            })
        );
        assert_eq!(parse_chord("Meta+KeyA").map(|c| c.win), Some(true));
        assert_eq!(parse_chord("Ctrl+Ctrl+KeyA"), None);
        assert_eq!(parse_chord("Hyper+KeyA"), None);
        assert_eq!(parse_chord("Ctrl"), None);
        assert_eq!(parse_chord("NumpadEnter"), None);
    }
}
