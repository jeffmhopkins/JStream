//! Global hotkeys: the desktop app hears the collection's bound chords while another
//! application has the focus ([ADR-0112](../../../docs/adr/0112-assignable-camera-zoom-bindings.md)
//! §5 and §6), as `jstream-platform-windows`' and `jstream-platform-linux`' listeners do.
//!
//! **Keys** are registered with Carbon's `RegisterEventHotKey`, the system's single-chord
//! API, which needs no permission and lets the process see no other key. Carbon delivers
//! a hotkey to the application's event target on the main thread, inside the run loop
//! the window's `NSApplication` runs, so registering, unregistering and the handler all
//! run there: [`GlobalHotkeys::set`] sends the work to the main dispatch queue and waits
//! for its answer. A chord registered exclusively by another application can't be: `set`
//! returns it, and it is heard only while JStream has the focus.
//!
//! The key names are the protocol's chord keys (W3C `KeyboardEvent.code` values). macOS
//! virtual key codes (`kVK_*`) are physical keys, as those names are, so one table maps
//! them on every keyboard layout. The function keys past F20, Print Screen, Scroll Lock,
//! Pause and the media and volume keys have no virtual key a hotkey can take: `set`
//! returns them. The table and the chord parsing are pure and tested on every platform.
//!
//! **A wheel chord** (`Ctrl+WheelUp`, ADR-0112 §3) has no hotkey API: a Quartz event tap
//! (`CGEventTapCreate`, scroll-wheel events only) hears it, on its own thread's run loop,
//! installed only while a wheel chord is set and removed when none is. The desktop host
//! sets a wheel chord only while a follow camera is live (§5), so the tap is there only
//! then. A turn whose modifiers are exactly a set chord's, in its direction, is the
//! chord's: the tap swallows it, so no window scrolls, and it runs once per notch. Any
//! other turn passes on. [`WheelChords`] does that matching and counting; it is pure and
//! tested on every platform.
//!
//! A tap that can swallow events needs the Accessibility permission. It is asked for the
//! first time a wheel chord is set (macOS shows its prompt once); until it is granted,
//! `set` returns each wheel chord with the reason, and wheel chords are heard only while
//! JStream has the focus. The next `set` (a binding changed, or a follow camera went
//! live) tries again.
//!
//! Docs: crates/jstream-platform-macos/README.md#purpose

/// The modifiers and key of one chord, as `RegisterEventHotKey` takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyChord {
    /// The modifiers held.
    pub held: Held,
    /// The key's virtual key code (`kVK_*`).
    pub key: u32,
}

/// The modifiers held with a key or a wheel turn. The protocol's `Meta` is Command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    /// Control is held (`Ctrl`).
    pub ctrl: bool,
    /// Option is held (`Alt`).
    pub alt: bool,
    /// Shift is held.
    pub shift: bool,
    /// Command is held (`Meta`).
    pub cmd: bool,
}

impl Held {
    /// The modifiers before the key in `parts` (`Ctrl`, `Alt`, `Shift`, `Meta`, in any
    /// order, each at most once), or `None` when one isn't a modifier or repeats.
    fn parse(parts: &[&str]) -> Option<Held> {
        let mut held = Held::default();
        for part in parts {
            let on = match *part {
                "Ctrl" => &mut held.ctrl,
                "Alt" => &mut held.alt,
                "Shift" => &mut held.shift,
                "Meta" => &mut held.cmd,
                _ => return None,
            };
            if *on {
                return None;
            }
            *on = true;
        }
        Some(held)
    }

    /// The Carbon modifier mask: `cmdKey`, `shiftKey`, `optionKey`, `controlKey`.
    pub fn carbon(self) -> u32 {
        [
            (self.cmd, 1 << 8),
            (self.shift, 1 << 9),
            (self.alt, 1 << 11),
            (self.ctrl, 1 << 12),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |mask, (_, bit)| mask | bit)
    }

    /// The modifiers in a Quartz event's flags (`CGEventFlags`' raw bits): Shift,
    /// Control, Option and Command, ignoring Caps Lock, Fn and the rest.
    pub fn from_event_flags(flags: u64) -> Held {
        Held {
            shift: flags & 0x0002_0000 != 0,
            ctrl: flags & 0x0004_0000 != 0,
            alt: flags & 0x0008_0000 != 0,
            cmd: flags & 0x0010_0000 != 0,
        }
    }
}

/// The chord `Ctrl+Alt+Shift+Meta+<code>` (any of the modifiers, in that order), or
/// `None` when it isn't one or its key has no virtual key a hotkey can take.
pub fn parse_chord(chord: &str) -> Option<HotkeyChord> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let key = key_code(parts.pop()?)?;
    Some(HotkeyChord {
        held: Held::parse(&parts)?,
        key,
    })
}

/// A chord key's macOS virtual key code (`kVK_*`, from HIToolbox's `Events.h`), the
/// same on every layout.
pub fn key_code(code: &str) -> Option<u32> {
    if let Some(letter) = code.strip_prefix("Key") {
        const LETTERS: [(char, u32); 26] = [
            ('A', 0x00),
            ('B', 0x0B),
            ('C', 0x08),
            ('D', 0x02),
            ('E', 0x0E),
            ('F', 0x03),
            ('G', 0x05),
            ('H', 0x04),
            ('I', 0x22),
            ('J', 0x26),
            ('K', 0x28),
            ('L', 0x25),
            ('M', 0x2E),
            ('N', 0x2D),
            ('O', 0x1F),
            ('P', 0x23),
            ('Q', 0x0C),
            ('R', 0x0F),
            ('S', 0x01),
            ('T', 0x11),
            ('U', 0x20),
            ('V', 0x09),
            ('W', 0x0D),
            ('X', 0x07),
            ('Y', 0x10),
            ('Z', 0x06),
        ];
        let mut chars = letter.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        return LETTERS.iter().find(|(l, _)| *l == c).map(|(_, key)| *key);
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        const DIGITS: [u32; 10] = [0x1D, 0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19];
        let d: usize = digit.parse().ok().filter(|_| digit.len() == 1)?;
        return DIGITS.get(d).copied();
    }
    if let Some(n) = code.strip_prefix("Numpad")
        && let Ok(n) = n.parse::<usize>()
    {
        const KEYPAD: [u32; 10] = [0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C];
        return KEYPAD.get(n).copied();
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<usize>()
    {
        // F1 to F20; there is no virtual key past F20.
        const FUNCTION: [u32; 20] = [
            0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B,
            0x71, 0x6A, 0x40, 0x4F, 0x50, 0x5A,
        ];
        return n.checked_sub(1).and_then(|i| FUNCTION.get(i)).copied();
    }
    Some(match code {
        "Minus" => 0x1B,
        "Equal" => 0x18,
        "BracketLeft" => 0x21,
        "BracketRight" => 0x1E,
        "Backslash" => 0x2A,
        "Semicolon" => 0x29,
        "Quote" => 0x27,
        "Backquote" => 0x32,
        "Comma" => 0x2B,
        "Period" => 0x2F,
        "Slash" => 0x2C,
        "Space" => 0x31,
        "Enter" => 0x24,
        "Tab" => 0x30,
        "Backspace" => 0x33,
        "Escape" => 0x35,
        // `kVK_Help`, where an Apple extended keyboard has Insert; Chromium names it so.
        "Insert" => 0x72,
        "Delete" => 0x75,
        "Home" => 0x73,
        "End" => 0x77,
        "PageUp" => 0x74,
        "PageDown" => 0x79,
        "ArrowLeft" => 0x7B,
        "ArrowRight" => 0x7C,
        "ArrowDown" => 0x7D,
        "ArrowUp" => 0x7E,
        "NumpadDecimal" => 0x41,
        "NumpadMultiply" => 0x43,
        "NumpadAdd" => 0x45,
        "NumpadDivide" => 0x4B,
        // Its own key on a Mac, unlike Windows, where it shares Enter's virtual key.
        "NumpadEnter" => 0x4C,
        "NumpadSubtract" => 0x4E,
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

/// The wheel chord `Ctrl+Alt+Shift+Meta+WheelUp` or `…+WheelDown` (any of the modifiers,
/// in that order), or `None` when it isn't one.
pub fn parse_wheel_chord(chord: &str) -> Option<WheelChord> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let up = match parts.pop()? {
        "WheelUp" => true,
        "WheelDown" => false,
        _ => return None,
    };
    Some(WheelChord {
        held: Held::parse(&parts)?,
        up,
    })
}

/// One wheel notch, in the units [`WheelChords::turn`] counts: Windows' `WHEEL_DELTA`, so
/// the protocol's "120 units of high-resolution wheel delta" reads the same here.
pub const WHEEL_DELTA: u32 = 120;

/// How a Quartz scroll-wheel event's turn reads in [`WHEEL_DELTA`] units: a mouse wheel
/// reports whole lines (`kCGScrollWheelEventDeltaAxis1`), one a notch; a trackpad or a
/// continuous wheel reports points (`kCGScrollWheelEventPointDeltaAxis1`), counted at
/// the 120 a notch the page counts its pixels at. Positive is up, away from the user.
pub fn scroll_delta(continuous: bool, lines: i64, points: i64) -> i32 {
    let delta = if continuous {
        points
    } else {
        lines.saturating_mul(i64::from(WHEEL_DELTA))
    };
    let clamped = delta.clamp(i64::from(i32::MIN) + 1, i64::from(i32::MAX));
    i32::try_from(clamped).unwrap_or_default()
}

/// The wheel chords set, and how far the wheel has turned toward the next notch of the
/// last one turned. A trackpad or a continuous wheel reports parts of a notch, which add
/// up while the same chord is turned. The same counting as `jstream-platform-windows`'.
#[derive(Debug, Default)]
pub struct WheelChords {
    chords: Vec<(WheelChord, String)>,
    /// The index in `chords` of the chord turned last, and how far it has turned past
    /// its last whole notch.
    carry: Option<(usize, u32)>,
}

impl WheelChords {
    /// The chords among `chords` that are wheel chords, each as named; the rest are
    /// returned for `RegisterEventHotKey`.
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

    /// A turn of `delta` [`WHEEL_DELTA`] units (positive is up) with `held` down: `None`
    /// when no chord set is those modifiers in that direction, so the turn passes on;
    /// else the chord's name and how many whole notches it has turned now, which may be
    /// none. The turn is the chord's either way, and isn't passed on.
    pub fn turn(&mut self, held: Held, delta: i32) -> Option<(&str, u32)> {
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
        let turned = before.saturating_add(delta.unsigned_abs());
        self.carry = Some((index, turned % WHEEL_DELTA));
        Some((self.chords[index].1.as_str(), turned / WHEEL_DELTA))
    }
}

/// Whether wheel chords would be heard only while JStream has the focus, because the
/// app doesn't have the Accessibility permission the wheel tap needs. Checks without
/// asking (`CGPreflightPostEventAccess`); always `false` off macOS.
pub fn wheel_needs_accessibility() -> bool {
    #[cfg(target_os = "macos")]
    {
        !objc2_core_graphics::CGPreflightPostEventAccess()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Why a wheel chord isn't heard while the Accessibility permission is missing.
pub const NEEDS_ACCESSIBILITY: &str = "wheel chords need the Accessibility permission \
    (System Settings > Privacy & Security > Accessibility); until JStream has it, they are \
    heard only while JStream has the focus";

#[cfg(target_os = "macos")]
pub use listener::GlobalHotkeys;

#[cfg(target_os = "macos")]
mod listener {
    use super::{Held, NEEDS_ACCESSIBILITY, WheelChords, parse_chord, scroll_delta};
    use dispatch2::DispatchQueue;
    use jstream_platform::PlatformError;
    use objc2_core_foundation::{
        CFMachPort, CFRetained, CFRunLoop, kCFRunLoopCommonModes, kCFRunLoopDefaultMode,
    };
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventTapProxy, CGEventType, CGPreflightPostEventAccess, CGRequestPostEventAccess,
    };
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError, mpsc};
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// What the listener calls with each chord pressed. Called from the main thread
    /// (keys) and the tap's thread (the wheel), never at once.
    type OnPress = Mutex<Box<dyn Fn(&str) + Send>>;

    /// How long [`GlobalHotkeys::set`] waits for the main thread to register keys. It
    /// answers at once while the window's run loop runs; it doesn't before the window
    /// opens or once the app is quitting.
    const MAIN_WAIT: Duration = Duration::from_secs(10);

    /// The hotkey listener. Dropping it unregisters every key and removes the wheel tap.
    pub struct GlobalHotkeys {
        on_press: Arc<OnPress>,
        /// The wheel tap, while a wheel chord is set.
        tap: Mutex<Option<Tap>>,
    }

    impl GlobalHotkeys {
        /// Starts the listener, with nothing registered. `on_press` runs with the chord,
        /// as [`set`](Self::set) named it, each time it is pressed: on the main thread
        /// for a key, on the tap's thread for a wheel notch. It must return quickly.
        ///
        /// # Errors
        /// None on macOS today; the `Result` matches the other platforms' listeners.
        pub fn start(
            on_press: impl Fn(&str) + Send + 'static,
        ) -> Result<GlobalHotkeys, PlatformError> {
            Ok(GlobalHotkeys {
                on_press: Arc::new(Mutex::new(Box::new(on_press))),
                tap: Mutex::new(None),
            })
        }

        /// Listens for `chords` in place of the ones before, and returns each chord it
        /// couldn't with the reason: a key another application holds, one with no
        /// virtual key, a wheel chord without the Accessibility permission, or one that
        /// isn't a chord. Waits for the main thread, so it must not be called there.
        pub fn set(&self, chords: Vec<String>) -> Vec<(String, String)> {
            let (wheel, keys) = WheelChords::split(chords);
            let mut failed = self.set_keys(keys);
            failed.extend(self.set_wheel(wheel));
            failed
        }

        /// Registers `keys` on the main thread in place of the ones before.
        fn set_keys(&self, keys: Vec<String>) -> Vec<(String, String)> {
            let (reply, answer) = mpsc::channel();
            let all = keys.clone();
            let on_press = Arc::clone(&self.on_press);
            DispatchQueue::main().exec_async(move || {
                let _ = reply.send(carbon::set(keys, on_press));
            });
            answer.recv_timeout(MAIN_WAIT).unwrap_or_else(|_| {
                all.into_iter()
                    .map(|chord| (chord, "the main thread didn't answer".into()))
                    .collect()
            })
        }

        /// Hears `wheel` with the tap, installing it when there are chords and it isn't,
        /// and removing it when there are none.
        fn set_wheel(&self, wheel: WheelChords) -> Vec<(String, String)> {
            let mut tap = self.tap.lock().unwrap_or_else(PoisonError::into_inner);
            if wheel.is_empty() {
                if let Some(installed) = tap.take() {
                    installed.remove();
                }
                return Vec::new();
            }
            if let Some(installed) = tap.as_ref() {
                *installed.shared.lock() = wheel;
                return Vec::new();
            }
            // Asks once: macOS prompts the first time and answers at once either way;
            // the person grants it in System Settings, and the next `set` sees it.
            if !CGPreflightPostEventAccess() && !CGRequestPostEventAccess() {
                return refuse(&wheel, NEEDS_ACCESSIBILITY);
            }
            let names: Vec<String> = wheel.names().map(str::to_owned).collect();
            match Tap::install(wheel, Arc::clone(&self.on_press)) {
                Ok(installed) => {
                    *tap = Some(installed);
                    Vec::new()
                }
                Err(reason) => names
                    .into_iter()
                    .map(|name| (name, reason.clone()))
                    .collect(),
            }
        }
    }

    impl Drop for GlobalHotkeys {
        fn drop(&mut self) {
            let tap = self.tap.get_mut().unwrap_or_else(PoisonError::into_inner);
            if let Some(installed) = tap.take() {
                installed.remove();
            }
            // Not waited for: the main thread may be the one dropping it, or be busy
            // quitting. Unregistering runs as soon as it turns.
            DispatchQueue::main().exec_async(carbon::clear);
        }
    }

    /// Each of `wheel`'s chords, with `reason`.
    fn refuse(wheel: &WheelChords, reason: &str) -> Vec<(String, String)> {
        wheel
            .names()
            .map(|name| (name.to_owned(), reason.to_owned()))
            .collect()
    }

    /// What the tap's callback reads: the chords and their carry, and where presses go.
    struct Shared {
        chords: Mutex<WheelChords>,
        on_press: Arc<OnPress>,
        /// Set by [`Tap::remove`]; the tap's thread checks it between runs of its loop.
        stopping: AtomicBool,
    }

    impl Shared {
        fn lock(&self) -> std::sync::MutexGuard<'_, WheelChords> {
            self.chords.lock().unwrap_or_else(PoisonError::into_inner)
        }
    }

    /// The longest one run of the tap's loop lasts before it checks whether it should
    /// stop (seconds): the bound on how long a lost `CFRunLoopStop` delays removing it.
    const RUN_SLICE: f64 = 0.5;

    /// A run loop handle another thread may stop.
    struct RunLoopHandle(CFRetained<CFRunLoop>);

    // SAFETY: CFRunLoop is thread-safe (Apple's "Thread Safety Summary": "CFRunLoop
    // … can be safely used from multiple threads"), and the handle is only used to call
    // `CFRunLoopStop` from another thread.
    unsafe impl Send for RunLoopHandle {}

    /// The wheel tap: a Quartz event tap on scroll-wheel events, on its own thread,
    /// running that thread's run loop until it is removed.
    struct Tap {
        shared: Arc<Shared>,
        run_loop: RunLoopHandle,
        thread: Option<JoinHandle<()>>,
    }

    thread_local! {
        /// The tap's port, on the tap's thread, so its callback can turn it back on
        /// when the system turns it off.
        static PORT: RefCell<Option<CFRetained<CFMachPort>>> = const { RefCell::new(None) };
    }

    impl Tap {
        /// Starts the tap's thread, which creates the tap, adds it to its run loop and
        /// runs that until [`remove`](Self::remove).
        fn install(chords: WheelChords, on_press: Arc<OnPress>) -> Result<Tap, String> {
            let shared = Arc::new(Shared {
                chords: Mutex::new(chords),
                on_press,
                stopping: AtomicBool::new(false),
            });
            let (ready, started) = mpsc::channel::<Result<RunLoopHandle, String>>();
            let theirs = Arc::clone(&shared);
            let thread = std::thread::Builder::new()
                .name("jstream-wheel-tap".into())
                .spawn(move || run(&theirs, &ready))
                .map_err(|error| format!("the wheel tap's thread didn't start ({error})"))?;
            match started.recv() {
                Ok(Ok(run_loop)) => Ok(Tap {
                    shared,
                    run_loop,
                    thread: Some(thread),
                }),
                Ok(Err(reason)) => {
                    let _ = thread.join();
                    Err(reason)
                }
                Err(_) => {
                    let _ = thread.join();
                    Err("the wheel tap's thread ended before it started".into())
                }
            }
        }

        /// Stops the tap's run loop and waits for its thread, which removes the tap.
        ///
        /// `CFRunLoopStop` does nothing to a loop that isn't running yet, so a stop sent
        /// between the thread saying it is ready and entering its loop would be lost.
        /// The flag closes that gap: the thread checks it before each run, and each run
        /// is bounded by [`RUN_SLICE`], so the thread ends within one slice whatever the
        /// order.
        fn remove(mut self) {
            self.shared.stopping.store(true, Ordering::SeqCst);
            self.run_loop.0.stop();
            self.run_loop.0.wake_up();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The tap's thread: creates the tap with `shared` as its context, says it is
    /// ready, and runs until stopped; then removes the tap.
    fn run(shared: &Arc<Shared>, ready: &mpsc::Sender<Result<RunLoopHandle, String>>) {
        let context = Arc::as_ptr(shared).cast_mut().cast::<c_void>();
        // SAFETY: `wheel_tap` is a tap callback with the signature Quartz calls, and
        // `context` points at the `Shared` this thread keeps alive (through `shared`)
        // until the tap is invalidated below; the callback runs only on this thread,
        // inside its run loop.
        let port = unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                1 << CGEventType::ScrollWheel.0,
                Some(wheel_tap),
                context,
            )
        };
        let Some(port) = port else {
            let _ = ready.send(Err(NEEDS_ACCESSIBILITY.into()));
            return;
        };
        let (Some(source), Some(run_loop)) = (
            CFMachPort::new_run_loop_source(None, Some(&port), 0),
            CFRunLoop::current(),
        ) else {
            port.invalidate();
            let _ = ready.send(Err("the wheel tap can't join a run loop".into()));
            return;
        };
        // SAFETY: a static the CoreFoundation framework defines, valid for the process.
        let common = unsafe { kCFRunLoopCommonModes };
        run_loop.add_source(Some(&source), common);
        CGEvent::tap_enable(&port, true);
        PORT.with(|cell| *cell.borrow_mut() = Some(port.clone()));
        if ready.send(Ok(RunLoopHandle(run_loop))).is_ok() {
            // SAFETY: a static the CoreFoundation framework defines, valid for the process.
            let default = unsafe { kCFRunLoopDefaultMode };
            while !shared.stopping.load(Ordering::SeqCst) {
                CFRunLoop::run_in_mode(default, RUN_SLICE, false);
            }
        }
        CGEvent::tap_enable(&port, false);
        port.invalidate();
        PORT.with(|cell| cell.borrow_mut().take());
    }

    /// The tap's callback: swallows a scroll that is a set wheel chord's (returning no
    /// event) and presses it once per whole notch; passes on everything else.
    unsafe extern "C-unwind" fn wheel_tap(
        _proxy: CGEventTapProxy,
        kind: CGEventType,
        event: NonNull<CGEvent>,
        context: *mut c_void,
    ) -> *mut CGEvent {
        let passed = event.as_ptr();
        if kind == CGEventType::TapDisabledByTimeout || kind == CGEventType::TapDisabledByUserInput
        {
            // The system turns a slow tap off; turn it back on.
            PORT.with(|cell| {
                if let Ok(port) = cell.try_borrow()
                    && let Some(port) = port.as_ref()
                {
                    CGEvent::tap_enable(port, true);
                }
            });
            return passed;
        }
        if kind != CGEventType::ScrollWheel || context.is_null() {
            return passed;
        }
        // SAFETY: `context` is the `Shared` the tap's thread keeps alive while the tap
        // exists, and this callback runs only while it does.
        let shared = unsafe { &*context.cast::<Shared>() };
        // SAFETY: Quartz passes a valid event for the length of the call.
        let event = unsafe { event.as_ref() };
        let field = |field| CGEvent::integer_value_field(Some(event), field);
        let delta = scroll_delta(
            field(CGEventField::ScrollWheelEventIsContinuous) != 0,
            field(CGEventField::ScrollWheelEventDeltaAxis1),
            field(CGEventField::ScrollWheelEventPointDeltaAxis1),
        );
        let held = Held::from_event_flags(CGEvent::flags(Some(event)).bits());
        let heard = {
            let mut chords = shared.lock();
            chords
                .turn(held, delta)
                .map(|(name, notches)| (name.to_owned(), notches))
        };
        let Some((name, notches)) = heard else {
            return passed;
        };
        let on_press = shared
            .on_press
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for _ in 0..notches {
            on_press(&name);
        }
        // Swallowed: no window sees the turn.
        ptr::null_mut()
    }

    /// Carbon's hotkeys, on the main thread: the registered keys and the one handler
    /// that hears them.
    mod carbon {
        use super::OnPress;
        use std::cell::RefCell;
        use std::ffi::c_void;
        use std::sync::{Arc, PoisonError};

        type OsStatus = i32;
        type EventTargetRef = *mut c_void;
        type EventHotKeyRef = *mut c_void;
        type EventHandlerRef = *mut c_void;
        type EventHandlerCallRef = *mut c_void;
        type EventRef = *mut c_void;
        type EventHandlerUpp =
            unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OsStatus;

        /// `EventHotKeyID`.
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct EventHotKeyId {
            signature: u32,
            id: u32,
        }

        /// `EventTypeSpec`.
        #[repr(C)]
        struct EventTypeSpec {
            event_class: u32,
            event_kind: u32,
        }

        const NO_ERR: OsStatus = 0;
        /// `eventNotHandledErr`.
        const EVENT_NOT_HANDLED: OsStatus = -9874;
        /// `eventHotKeyExistsErr`.
        const HOT_KEY_EXISTS: OsStatus = -9878;
        /// `kEventClassKeyboard`, `'keyb'`.
        const CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
        /// `kEventHotKeyPressed`.
        const HOT_KEY_PRESSED: u32 = 5;
        /// `kEventParamDirectObject`, `'----'`.
        const PARAM_DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");
        /// `typeEventHotKeyID`, `'hkid'`.
        const TYPE_HOT_KEY_ID: u32 = u32::from_be_bytes(*b"hkid");
        /// `kEventHotKeyExclusive`: fails when another application has the chord.
        const HOT_KEY_EXCLUSIVE: u32 = 1;
        /// The signature JStream's hotkey IDs carry.
        const SIGNATURE: u32 = u32::from_be_bytes(*b"JStr");

        // Carbon's Event Manager has no maintained Rust binding (the objc2 crates cover
        // the Objective-C and Core frameworks, not HIToolbox's C API), so these few
        // declarations are written here from `CarbonEvents.h` (R-UNSAFE-004).
        #[link(name = "Carbon", kind = "framework")]
        unsafe extern "C" {
            fn GetApplicationEventTarget() -> EventTargetRef;
            fn InstallEventHandler(
                target: EventTargetRef,
                handler: EventHandlerUpp,
                num_types: usize,
                list: *const EventTypeSpec,
                user_data: *mut c_void,
                out_ref: *mut EventHandlerRef,
            ) -> OsStatus;
            fn RemoveEventHandler(handler: EventHandlerRef) -> OsStatus;
            fn RegisterEventHotKey(
                key_code: u32,
                modifiers: u32,
                id: EventHotKeyId,
                target: EventTargetRef,
                options: u32,
                out_ref: *mut EventHotKeyRef,
            ) -> OsStatus;
            fn UnregisterEventHotKey(hot_key: EventHotKeyRef) -> OsStatus;
            fn GetEventParameter(
                event: EventRef,
                name: u32,
                desired_type: u32,
                actual_type: *mut u32,
                buffer_size: usize,
                actual_size: *mut usize,
                data: *mut c_void,
            ) -> OsStatus;
        }

        /// The main thread's hotkeys.
        #[derive(Default)]
        struct State {
            handler: Option<EventHandlerRef>,
            /// Each registered key with its chord; its index is its hotkey ID.
            keys: Vec<(EventHotKeyRef, String)>,
            on_press: Option<Arc<OnPress>>,
        }

        thread_local! {
            static STATE: RefCell<State> = RefCell::default();
        }

        /// Registers `keys` in place of the ones before, sending presses to `on_press`;
        /// returns those that couldn't be, with the reason. On the main thread.
        pub(super) fn set(keys: Vec<String>, on_press: Arc<OnPress>) -> Vec<(String, String)> {
            STATE.with(|state| {
                let Ok(mut state) = state.try_borrow_mut() else {
                    return refuse(keys, "the hotkeys are busy");
                };
                unregister(&mut state);
                state.on_press = Some(on_press);
                if keys.is_empty() {
                    return Vec::new();
                }
                // SAFETY: no arguments; on the main thread, the application's target.
                let target = unsafe { GetApplicationEventTarget() };
                if state.handler.is_none() {
                    let spec = EventTypeSpec {
                        event_class: CLASS_KEYBOARD,
                        event_kind: HOT_KEY_PRESSED,
                    };
                    let mut handler: EventHandlerRef = std::ptr::null_mut();
                    // SAFETY: `pressed` has the handler signature Carbon calls; `spec` is
                    // one valid EventTypeSpec for the call; no user data; `handler` is
                    // written on success. The handler stays until `clear`.
                    let status = unsafe {
                        InstallEventHandler(
                            target,
                            pressed,
                            1,
                            &raw const spec,
                            std::ptr::null_mut(),
                            &raw mut handler,
                        )
                    };
                    if status != NO_ERR {
                        return refuse(keys, &format!("Carbon refused its handler ({status})"));
                    }
                    state.handler = Some(handler);
                }
                let mut failed = Vec::new();
                for chord in keys {
                    let Some(parsed) = super::parse_chord(&chord) else {
                        failed.push((chord, "macOS has no hotkey for its key".into()));
                        continue;
                    };
                    let Ok(id) = u32::try_from(state.keys.len()) else {
                        failed.push((chord, "too many hotkeys".into()));
                        continue;
                    };
                    let mut hot_key: EventHotKeyRef = std::ptr::null_mut();
                    // SAFETY: plain values, the application target, and `hot_key` to
                    // write on success.
                    let status = unsafe {
                        RegisterEventHotKey(
                            parsed.key,
                            parsed.held.carbon(),
                            EventHotKeyId {
                                signature: SIGNATURE,
                                id,
                            },
                            target,
                            HOT_KEY_EXCLUSIVE,
                            &raw mut hot_key,
                        )
                    };
                    match status {
                        NO_ERR => state.keys.push((hot_key, chord)),
                        HOT_KEY_EXISTS => {
                            failed.push((chord, "another application holds it".into()));
                        }
                        status => {
                            failed.push((chord, format!("Carbon refused it ({status})")));
                        }
                    }
                }
                failed
            })
        }

        /// Unregisters every key and removes the handler. On the main thread.
        pub(super) fn clear() {
            STATE.with(|state| {
                if let Ok(mut state) = state.try_borrow_mut() {
                    unregister(&mut state);
                    if let Some(handler) = state.handler.take() {
                        // SAFETY: the handler `set` installed and nothing removed.
                        let _ = unsafe { RemoveEventHandler(handler) };
                    }
                    state.on_press = None;
                }
            });
        }

        fn unregister(state: &mut State) {
            for (hot_key, _) in state.keys.drain(..) {
                // SAFETY: a hotkey `set` registered and nothing unregistered.
                let _ = unsafe { UnregisterEventHotKey(hot_key) };
            }
        }

        fn refuse(keys: Vec<String>, reason: &str) -> Vec<(String, String)> {
            keys.into_iter()
                .map(|chord| (chord, reason.to_owned()))
                .collect()
        }

        /// The hotkey handler, on the main thread: presses the chord whose ID the event
        /// carries.
        unsafe extern "C" fn pressed(
            _call: EventHandlerCallRef,
            event: EventRef,
            _user_data: *mut c_void,
        ) -> OsStatus {
            let mut id = EventHotKeyId::default();
            // SAFETY: `event` is the event Carbon is handling; `id` has room for the
            // `EventHotKeyID` asked for, and the actual type and size aren't wanted.
            let status = unsafe {
                GetEventParameter(
                    event,
                    PARAM_DIRECT_OBJECT,
                    TYPE_HOT_KEY_ID,
                    std::ptr::null_mut(),
                    size_of::<EventHotKeyId>(),
                    std::ptr::null_mut(),
                    (&raw mut id).cast(),
                )
            };
            if status != NO_ERR || id.signature != SIGNATURE {
                return EVENT_NOT_HANDLED;
            }
            let heard = STATE.with(|state| {
                let state = state.try_borrow().ok()?;
                let (_, chord) = state.keys.get(usize::try_from(id.id).ok()?)?;
                Some((chord.clone(), Arc::clone(state.on_press.as_ref()?)))
            });
            let Some((chord, on_press)) = heard else {
                return EVENT_NOT_HANDLED;
            };
            let on_press = on_press.lock().unwrap_or_else(PoisonError::into_inner);
            on_press(&chord);
            NO_ERR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Held, HotkeyChord, WHEEL_DELTA, WheelChord, WheelChords, key_code, parse_chord,
        parse_wheel_chord, scroll_delta,
    };
    use jstream_protocol::Chord;

    const CTRL: Held = Held {
        ctrl: true,
        alt: false,
        shift: false,
        cmd: false,
    };

    /// The chord keys macOS has no hotkey for.
    const NO_HOTKEY: [&str; 11] = [
        "F21",
        "F22",
        "F23",
        "F24",
        "PrintScreen",
        "ScrollLock",
        "Pause",
        "MediaPlayPause",
        "MediaStop",
        "MediaTrackNext",
        "MediaTrackPrevious",
    ];

    #[test]
    fn every_chord_key_has_a_virtual_key_but_the_ones_macos_lacks() {
        for code in Chord::KEYS {
            let mapped = key_code(code);
            let lacking = NO_HOTKEY.contains(code) || code.starts_with("AudioVolume");
            assert_eq!(mapped.is_none(), lacking, "{code}: {mapped:?}");
        }
    }

    #[test]
    fn every_virtual_key_is_used_once() {
        let mut keys: Vec<u32> = Chord::KEYS.iter().filter_map(|c| key_code(c)).collect();
        let all = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all, "two chord keys share a virtual key");
    }

    #[test]
    fn keys_map_to_their_virtual_key_codes() {
        assert_eq!(key_code("KeyA"), Some(0x00));
        assert_eq!(key_code("KeyQ"), Some(0x0C));
        assert_eq!(key_code("KeyZ"), Some(0x06));
        assert_eq!(key_code("Digit1"), Some(0x12));
        assert_eq!(key_code("Digit0"), Some(0x1D));
        assert_eq!(key_code("Space"), Some(0x31));
        assert_eq!(key_code("F1"), Some(0x7A));
        assert_eq!(key_code("F20"), Some(0x5A));
        assert_eq!(key_code("Numpad7"), Some(0x59));
        assert_eq!(key_code("NumpadEnter"), Some(0x4C));
        assert_eq!(key_code("Enter"), Some(0x24));
        for bad in ["F0", "F21", "KeyAB", "Digit10", "Numpad10", "Hyper"] {
            assert_eq!(key_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_chord_parses_into_its_modifiers_and_key() {
        assert_eq!(
            parse_chord("Ctrl+Shift+F9"),
            Some(HotkeyChord {
                held: Held {
                    shift: true,
                    ..CTRL
                },
                key: 0x65,
            })
        );
        assert_eq!(parse_chord("Meta+KeyA").map(|c| c.held.cmd), Some(true));
        assert_eq!(parse_chord("Ctrl+Ctrl+KeyA"), None);
        assert_eq!(parse_chord("Hyper+KeyA"), None);
        assert_eq!(parse_chord("Ctrl"), None);
        assert_eq!(parse_chord("Ctrl+WheelUp"), None);
    }

    #[test]
    fn modifiers_become_carbons_mask_and_come_from_quartzs_flags() {
        assert_eq!(Held::default().carbon(), 0);
        assert_eq!(CTRL.carbon(), 0x1000);
        let all = Held {
            ctrl: true,
            alt: true,
            shift: true,
            cmd: true,
        };
        // cmdKey | shiftKey | optionKey | controlKey.
        assert_eq!(all.carbon(), 0x0100 | 0x0200 | 0x0800 | 0x1000);
        // Control, with Caps Lock (0x10000) and the numeric pad bit (0x200000) ignored.
        assert_eq!(
            Held::from_event_flags(0x0004_0000 | 0x0001_0000 | 0x0020_0000),
            CTRL
        );
        assert_eq!(
            Held::from_event_flags(0x0002_0000 | 0x0004_0000 | 0x0008_0000 | 0x0010_0000),
            all
        );
    }

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
            parsed.map(|c| (c.held.alt, c.held.shift, c.held.cmd, c.up)),
            Some((true, true, true, false))
        );
        assert_eq!(parse_wheel_chord("Ctrl+Ctrl+WheelUp"), None);
        assert_eq!(parse_wheel_chord("Ctrl+KeyA"), None);
        for key in Chord::WHEEL {
            assert!(parse_wheel_chord(&format!("Ctrl+{key}")).is_some(), "{key}");
        }
    }

    #[test]
    fn a_mouse_wheel_counts_lines_and_a_trackpad_points() {
        assert_eq!(scroll_delta(false, 1, 10), 120);
        assert_eq!(scroll_delta(false, -3, -30), -360);
        assert_eq!(scroll_delta(true, 1, 40), 40);
        assert_eq!(scroll_delta(true, 0, -7), -7);
        assert_eq!(scroll_delta(false, i64::MAX, 0), i32::MAX);
        assert_eq!(scroll_delta(true, 0, i64::MIN), i32::MIN + 1);
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
    fn a_wheel_chord_runs_once_per_notch_and_only_with_exactly_its_modifiers() {
        let (mut wheel, _) =
            WheelChords::split(vec!["Ctrl+WheelUp".into(), "Ctrl+WheelDown".into()]);
        let notch = i32::try_from(WHEEL_DELTA).unwrap_or_default();
        assert_eq!(wheel.turn(CTRL, notch), Some(("Ctrl+WheelUp", 1)));
        assert_eq!(wheel.turn(CTRL, -notch), Some(("Ctrl+WheelDown", 1)));
        assert_eq!(wheel.turn(CTRL, 3 * notch), Some(("Ctrl+WheelUp", 3)));
        assert_eq!(wheel.turn(Held::default(), notch), None);
        assert_eq!(wheel.turn(Held { cmd: true, ..CTRL }, notch), None);
        assert_eq!(wheel.turn(CTRL, 0), None);
    }

    #[test]
    fn a_trackpads_points_add_up_while_the_same_chord_turns() {
        let (mut wheel, _) =
            WheelChords::split(vec!["Ctrl+WheelUp".into(), "Ctrl+WheelDown".into()]);
        assert_eq!(wheel.turn(CTRL, 50), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 50), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 30), Some(("Ctrl+WheelUp", 1)));
        // 10 carried; turning back starts over.
        assert_eq!(wheel.turn(CTRL, -110), Some(("Ctrl+WheelDown", 0)));
        assert_eq!(wheel.turn(CTRL, 110), Some(("Ctrl+WheelUp", 0)));
        // A turn no chord takes starts over too.
        assert_eq!(wheel.turn(Held::default(), 100), None);
        assert_eq!(wheel.turn(CTRL, 110), Some(("Ctrl+WheelUp", 0)));
        assert_eq!(wheel.turn(CTRL, 10), Some(("Ctrl+WheelUp", 1)));
    }
}
