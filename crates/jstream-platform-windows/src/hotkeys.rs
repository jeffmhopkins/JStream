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
//! A wheel chord (`Ctrl+WheelUp`, ADR-0112 §3) has no `RegisterHotKey`: it is heard by a
//! low-level mouse hook (`WH_MOUSE_LL`), which the listener thread installs only while a
//! wheel chord is set, and removes when none is. The desktop host sets a wheel chord only
//! while a follow camera is live (§5), so the hook is there only then. A turn whose
//! modifiers are exactly a set chord's, in its direction, is the chord's: the hook
//! swallows it, so no window scrolls, and it runs once per notch. Any other turn passes
//! on. [`WheelChords`] does that matching and counting; it is pure and tested on every
//! platform.
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

/// The modifiers and direction of one wheel chord (`Ctrl+WheelUp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelChord {
    /// The modifiers held.
    pub held: Held,
    /// `WheelUp` (away from the user) rather than `WheelDown`.
    pub up: bool,
}

/// The modifiers held during a wheel turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Ctrl is held.
    pub ctrl: bool,
    /// Alt is held.
    pub alt: bool,
    /// Shift is held.
    pub shift: bool,
    /// The Windows key is held (`Meta`).
    pub win: bool,
}

/// The wheel chord `Ctrl+Alt+Shift+Meta+WheelUp` or `…+WheelDown` (any of the modifiers,
/// in that order), or `None` when it isn't one.
pub fn parse_wheel_chord(chord: &str) -> Option<WheelChord> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let up = match parts.pop()? {
        "WheelUp" => true,
        "WheelDown" => false,
        _ => return None,
    };
    let mut held = Held::default();
    for part in parts {
        let on = match part {
            "Ctrl" => &mut held.ctrl,
            "Alt" => &mut held.alt,
            "Shift" => &mut held.shift,
            "Meta" => &mut held.win,
            _ => return None,
        };
        if *on {
            return None;
        }
        *on = true;
    }
    Some(WheelChord { held, up })
}

/// One wheel notch, in `WM_MOUSEWHEEL`'s units (`WHEEL_DELTA`).
pub const WHEEL_DELTA: u32 = 120;

/// The wheel chords set, and how far the wheel has turned toward the next notch of the
/// last one turned. A high-resolution wheel or a touchpad reports parts of a notch, which
/// add up while the same chord is turned.
#[derive(Debug, Default)]
pub struct WheelChords {
    chords: Vec<(WheelChord, String)>,
    /// The index in `chords` of the chord turned last, and how far it has turned past
    /// its last whole notch.
    carry: Option<(usize, u32)>,
}

impl WheelChords {
    /// The chords among `chords` that are wheel chords, each as named; the rest are
    /// returned for `RegisterHotKey`.
    pub fn split(chords: Vec<String>) -> (WheelChords, Vec<String>) {
        let mut wheel = Vec::new();
        let mut keys = Vec::new();
        for chord in chords {
            match parse_wheel_chord(&chord) {
                Some(parsed) => wheel.push((parsed, chord)),
                None => keys.push(chord),
            }
        }
        (
            WheelChords {
                chords: wheel,
                carry: None,
            },
            keys,
        )
    }

    /// No wheel chord is set.
    pub fn is_empty(&self) -> bool {
        self.chords.is_empty()
    }

    /// Every wheel chord set, as named.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.chords.iter().map(|(_, name)| name.as_str())
    }

    /// A turn of `delta` (positive is up) with `held` down: `None` when no chord set is
    /// those modifiers in that direction, so the turn passes on; else the chord's name
    /// and how many whole notches it has turned now, which may be none. The turn is the
    /// chord's either way, and isn't passed on.
    pub fn turn(&mut self, held: Held, delta: i16) -> Option<(&str, u32)> {
        if delta == 0 {
            return None;
        }
        let up = delta > 0;
        let Some(index) = self
            .chords
            .iter()
            .position(|(chord, _)| chord.held == held && chord.up == up)
        else {
            self.carry = None;
            return None;
        };
        let before = match self.carry {
            Some((last, carry)) if last == index => carry,
            _ => 0,
        };
        let turned = before + u32::from(delta.unsigned_abs());
        self.carry = Some((index, turned % WHEEL_DELTA));
        Some((self.chords[index].1.as_str(), turned / WHEEL_DELTA))
    }
}

#[cfg(windows)]
pub use listener::GlobalHotkeys;

#[cfg(windows)]
mod listener {
    use super::{Held, HotkeyChord, KeyCode, WheelChords, parse_chord};
    use jstream_platform::PlatformError;
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, HOT_KEY_MODIFIERS, MAPVK_VSC_TO_VK_EX, MOD_ALT, MOD_CONTROL,
        MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, HC_ACTION, HHOOK, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE,
        PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL,
        WM_APP, WM_HOTKEY, WM_MOUSEWHEEL, WM_QUIT,
    };

    /// The message that tells the listener thread a new set of chords is waiting.
    const WM_SET: u32 = WM_APP + 1;
    /// The message the wheel hook posts when it has heard a wheel chord.
    const WM_WHEEL: u32 = WM_APP + 2;

    /// The wheel hook's state. The hook runs on the thread that installed it, inside
    /// that thread's `GetMessageW`, so it lives in that thread's own storage.
    #[derive(Default)]
    struct Wheel {
        chords: WheelChords,
        /// The wheel chords heard, one per notch, for the message loop to hand on.
        heard: Vec<String>,
    }

    thread_local! {
        static WHEEL: RefCell<Wheel> = RefCell::default();
    }

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
        let mut hook: Option<HHOOK> = None;
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
                WM_WHEEL => {
                    let heard = WHEEL.with(|wheel| std::mem::take(&mut wheel.borrow_mut().heard));
                    for chord in &heard {
                        on_press(chord);
                    }
                }
                WM_SET => {
                    while let Ok((chords, reply)) = incoming.try_recv() {
                        unregister(registered.len());
                        let (wheel, keys) = WheelChords::split(chords);
                        let (kept, mut failed) = register(keys);
                        registered = kept;
                        failed.extend(hook_wheel(&mut hook, wheel));
                        let _ = reply.send(failed);
                    }
                }
                _ => {}
            }
        }
        unregister(registered.len());
        let _ = hook_wheel(&mut hook, WheelChords::default());
    }

    /// Sets the wheel chords the hook hears to `chords`, installing the hook when there
    /// are some and it isn't, and removing it when there are none. Returns each chord
    /// with the reason when the hook can't be installed.
    fn hook_wheel(hook: &mut Option<HHOOK>, chords: WheelChords) -> Vec<(String, String)> {
        if chords.is_empty() {
            if let Some(installed) = hook.take() {
                // SAFETY: `installed` is the hook this thread installed and hasn't removed.
                let _ = unsafe { UnhookWindowsHookEx(installed) };
            }
            WHEEL.with(|wheel| *wheel.borrow_mut() = Wheel::default());
            return Vec::new();
        }
        if hook.is_none() {
            // SAFETY: a null name is this process's own module; `wheel_proc` is a hook
            // procedure with the signature WH_MOUSE_LL calls, and it stays loaded for
            // the life of the process. A low-level hook runs on this thread, which pumps
            // messages until it removes it.
            let installed = unsafe { GetModuleHandleW(None) }.and_then(|module| unsafe {
                SetWindowsHookExW(WH_MOUSE_LL, Some(wheel_proc), Some(module.into()), 0)
            });
            match installed {
                Ok(installed) => *hook = Some(installed),
                Err(error) => {
                    let reason = format!("the wheel can't be hooked ({})", error.message());
                    return chords
                        .names()
                        .map(|name| (name.to_owned(), reason.clone()))
                        .collect();
                }
            }
        }
        WHEEL.with(|wheel| {
            let mut wheel = wheel.borrow_mut();
            wheel.chords = chords;
            wheel.heard.clear();
        });
        Vec::new()
    }

    /// The low-level mouse hook: swallows a wheel turn that is a set wheel chord's, and
    /// queues a press for each whole notch; passes on everything else.
    unsafe extern "system" fn wheel_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && wparam.0 == WM_MOUSEWHEEL as usize && lparam.0 != 0 {
            // SAFETY: for HC_ACTION, WH_MOUSE_LL's lParam points at an MSLLHOOKSTRUCT
            // that is valid for this call.
            let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
            // The turn is the high word of mouseData, signed: positive is away from the
            // user, in WHEEL_DELTA units per notch.
            let delta = (info.mouseData >> 16) as u16 as i16;
            let held = held();
            let heard = WHEEL.with(|wheel| {
                let Ok(mut wheel) = wheel.try_borrow_mut() else {
                    return None;
                };
                let (name, notches) = wheel.chords.turn(held, delta)?;
                let name = name.to_owned();
                for _ in 0..notches {
                    wheel.heard.push(name.clone());
                }
                Some(notches > 0)
            });
            if let Some(pressed) = heard {
                if pressed {
                    // SAFETY: plain values, to this thread's own queue.
                    let _ = unsafe {
                        PostThreadMessageW(GetCurrentThreadId(), WM_WHEEL, WPARAM(0), LPARAM(0))
                    };
                }
                // Swallowed: no window sees the turn.
                return LRESULT(1);
            }
        }
        // SAFETY: the arguments are the ones this hook was called with.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// The modifiers held now. A low-level hook runs before the input reaches any
    /// thread's key state, so it asks for the keys' physical state.
    fn held() -> Held {
        let down = |key: VIRTUAL_KEY| {
            // SAFETY: plain value; the high bit is set while the key is down.
            let state = unsafe { GetAsyncKeyState(i32::from(key.0)) };
            state < 0
        };
        Held {
            ctrl: down(VK_CONTROL),
            alt: down(VK_MENU),
            shift: down(VK_SHIFT),
            win: down(VK_LWIN) || down(VK_RWIN),
        }
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
    use super::{
        Held, HotkeyChord, KeyCode, WheelChord, WheelChords, key_code, parse_chord,
        parse_wheel_chord,
    };
    use jstream_protocol::Chord;

    const CTRL: Held = Held {
        ctrl: true,
        alt: false,
        shift: false,
        win: false,
    };

    #[test]
    fn a_wheel_chord_parses_into_its_modifiers_and_direction() {
        assert_eq!(
            parse_wheel_chord("Ctrl+WheelUp"),
            Some(WheelChord {
                held: CTRL,
                up: true
            })
        );
        let parsed = parse_wheel_chord("Alt+Shift+Meta+WheelDown");
        assert_eq!(
            parsed.map(|c| (c.held.alt, c.held.shift, c.held.win, c.up)),
            Some((true, true, true, false))
        );
        assert_eq!(
            parse_wheel_chord("WheelUp").map(|c| c.held),
            Some(Held::default())
        );
        assert_eq!(parse_wheel_chord("Ctrl+Ctrl+WheelUp"), None);
        assert_eq!(parse_wheel_chord("Ctrl+KeyA"), None);
        for key in Chord::WHEEL {
            assert!(parse_wheel_chord(&format!("Ctrl+{key}")).is_some(), "{key}");
        }
        // A wheel chord isn't a key chord, so it never goes to RegisterHotKey.
        assert_eq!(parse_chord("Ctrl+WheelUp"), None);
    }

    #[test]
    fn wheel_chords_are_split_from_key_chords() {
        let (wheel, keys) = WheelChords::split(vec![
            "F9".into(),
            "Ctrl+WheelUp".into(),
            "Ctrl+WheelDown".into(),
        ]);
        assert_eq!(keys, ["F9"]);
        assert_eq!(
            wheel.names().collect::<Vec<_>>(),
            ["Ctrl+WheelUp", "Ctrl+WheelDown"]
        );
        assert!(WheelChords::split(vec!["F9".into()]).0.is_empty());
    }

    #[test]
    fn a_wheel_chord_runs_once_per_notch_and_only_with_its_modifiers() {
        let (mut wheel, _) =
            WheelChords::split(vec!["Ctrl+WheelUp".into(), "Ctrl+WheelDown".into()]);
        // One notch each way; two coalesced into one turn are two.
        assert_eq!(wheel.turn(CTRL, 120), Some(("Ctrl+WheelUp", 1)));
        assert_eq!(wheel.turn(CTRL, -120), Some(("Ctrl+WheelDown", 1)));
        assert_eq!(wheel.turn(CTRL, 240), Some(("Ctrl+WheelUp", 2)));
        // Without Ctrl, or with another modifier too, the turn passes on.
        assert_eq!(wheel.turn(Held::default(), 120), None);
        assert_eq!(
            wheel.turn(
                Held {
                    shift: true,
                    ..CTRL
                },
                120
            ),
            None
        );
        assert_eq!(wheel.turn(CTRL, 0), None);
    }

    #[test]
    fn parts_of_a_notch_add_up_while_the_same_chord_turns() {
        let (mut wheel, _) =
            WheelChords::split(vec!["Ctrl+WheelUp".into(), "Ctrl+WheelDown".into()]);
        // A high-resolution wheel's parts are the chord's, and swallowed, before a whole
        // notch has turned.
        assert_eq!(wheel.turn(CTRL, 40), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 40), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 60), Some(("Ctrl+WheelUp", 1)));
        // 20 carried; turning back starts over.
        assert_eq!(wheel.turn(CTRL, -100), Some(("Ctrl+WheelDown", 0)));
        assert_eq!(wheel.turn(CTRL, 100), Some(("Ctrl+WheelUp", 0)));
        // A turn no chord takes starts over too.
        assert_eq!(wheel.turn(Held::default(), 100), None);
        assert_eq!(wheel.turn(CTRL, 100), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 20), Some(("Ctrl+WheelUp", 1)));
    }

    #[test]
    fn a_turn_with_no_chord_in_its_direction_passes_on() {
        let (mut wheel, _) = WheelChords::split(vec!["Ctrl+WheelUp".into()]);
        assert_eq!(wheel.turn(CTRL, -120), None);
        assert_eq!(wheel.turn(CTRL, 120), Some(("Ctrl+WheelUp", 1)));
    }

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
