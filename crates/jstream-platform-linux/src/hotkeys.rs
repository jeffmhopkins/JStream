//! Global hotkeys on X11: the desktop app hears the collection's bound chords while
//! another application has the focus
//! ([ADR-0112](../../../docs/adr/0112-assignable-camera-zoom-bindings.md) §5 and §6).
//!
//! Each chord is a passive grab on the root window (`xcb_grab_key`), X11's single-chord
//! API, so the process never sees any other key. One listener thread owns its own X
//! connection, grabs what it is asked to, and hands each press to a callback. A chord
//! another client has grabbed already can't be grabbed: [`set`] returns it, and it is
//! heard only while JStream has the focus. libxcb is loaded at run time, as the crate's
//! other system libraries are (ADR-0060 §8).
//!
//! A grab names exact modifiers, so each chord is grabbed four times: alone, with Caps
//! Lock, with Num Lock and with both, which X counts as modifiers too. Holding a chord
//! runs it once: X's auto-repeat sends a release and a press with the same timestamp,
//! and that press is dropped, as `MOD_NOREPEAT` does on Windows.
//!
//! The key names are the protocol's chord keys (W3C `KeyboardEvent.code` values), which
//! name physical keys. On X11 a key's keycode is its Linux evdev code plus 8 under the
//! `evdev` keycodes every current X server uses, so a binding is the physical key the
//! page names on any layout. The table and the chord parsing are pure and tested on every
//! platform.
//!
//! Wayland has no grab: on a Wayland session the host doesn't start this listener.
//!
//! [`set`]: GlobalHotkeys::set
//!
//! Docs: crates/jstream-platform-linux/README.md#purpose

/// The modifiers and key of one chord, as an X11 grab takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrabChord {
    /// Ctrl is held.
    pub ctrl: bool,
    /// Alt is held.
    pub alt: bool,
    /// Shift is held.
    pub shift: bool,
    /// The Super (Windows) key is held (`Meta`).
    pub meta: bool,
    /// The key's X11 keycode.
    pub keycode: u8,
}

/// The chord `Ctrl+Alt+Shift+Meta+<code>` (any of the modifiers, in that order), or
/// `None` when it isn't one or its key has no keycode.
pub fn parse_chord(chord: &str) -> Option<GrabChord> {
    let mut parts: Vec<&str> = chord.split('+').collect();
    let keycode = key_code(parts.pop()?)?;
    let mut parsed = GrabChord {
        ctrl: false,
        alt: false,
        shift: false,
        meta: false,
        keycode,
    };
    for part in parts {
        let held = match part {
            "Ctrl" => &mut parsed.ctrl,
            "Alt" => &mut parsed.alt,
            "Shift" => &mut parsed.shift,
            "Meta" => &mut parsed.meta,
            _ => return None,
        };
        if *held {
            return None;
        }
        *held = true;
    }
    Some(parsed)
}

/// A chord key's X11 keycode: its evdev code plus 8.
pub fn key_code(code: &str) -> Option<u8> {
    evdev_code(code).and_then(|evdev| u8::try_from(evdev + 8).ok())
}

/// A chord key's Linux evdev code (`linux/input-event-codes.h`).
fn evdev_code(code: &str) -> Option<u16> {
    const LETTERS: [(char, u16); 26] = [
        ('A', 30),
        ('B', 48),
        ('C', 46),
        ('D', 32),
        ('E', 18),
        ('F', 33),
        ('G', 34),
        ('H', 35),
        ('I', 23),
        ('J', 36),
        ('K', 37),
        ('L', 38),
        ('M', 50),
        ('N', 49),
        ('O', 24),
        ('P', 25),
        ('Q', 16),
        ('R', 19),
        ('S', 31),
        ('T', 20),
        ('U', 22),
        ('V', 47),
        ('W', 17),
        ('X', 45),
        ('Y', 21),
        ('Z', 44),
    ];
    // KEY_KP0 to KEY_KP9.
    const NUMPAD: [u16; 10] = [82, 79, 80, 81, 75, 76, 77, 71, 72, 73];
    if let Some(letter) = code.strip_prefix("Key") {
        let mut chars = letter.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return None;
        };
        return LETTERS.iter().find(|(l, _)| *l == c).map(|(_, key)| *key);
    }
    if let Some(digit) = code.strip_prefix("Digit") {
        let d: u16 = digit.parse().ok().filter(|d| *d <= 9 && digit.len() == 1)?;
        // KEY_1 to KEY_9 are 2 to 10, and KEY_0 follows them.
        return Some(if d == 0 { 11 } else { d + 1 });
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<u16>()
    {
        return match n {
            // KEY_F1 to KEY_F10 are 59 to 68.
            1..=10 => Some(58 + n),
            11 => Some(87),
            12 => Some(88),
            // KEY_F13 to KEY_F24 are 183 to 194.
            13..=24 => Some(170 + n),
            _ => None,
        };
    }
    if let Some(n) = code.strip_prefix("Numpad")
        && let Ok(n) = n.parse::<usize>()
    {
        return NUMPAD.get(n).copied();
    }
    Some(match code {
        "Escape" => 1,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "Backslash" => 43,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "NumpadMultiply" => 55,
        "Space" => 57,
        "ScrollLock" => 70,
        "NumpadSubtract" => 74,
        "NumpadAdd" => 78,
        "NumpadDecimal" => 83,
        "NumpadEnter" => 96,
        "NumpadDivide" => 98,
        "PrintScreen" => 99,
        "Home" => 102,
        "ArrowUp" => 103,
        "PageUp" => 104,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "End" => 107,
        "ArrowDown" => 108,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        "AudioVolumeMute" => 113,
        "AudioVolumeDown" => 114,
        "AudioVolumeUp" => 115,
        "Pause" => 119,
        "MediaTrackNext" => 163,
        "MediaPlayPause" => 164,
        "MediaTrackPrevious" => 165,
        "MediaStop" => 166,
        _ => return None,
    })
}

/// The X modifier masks the chords' modifiers and the locks are on: X lets the server
/// put Alt, Super and Num Lock on any of Mod1 to Mod5, so the listener reads where they
/// are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierMasks {
    /// Alt's mask (usually Mod1).
    pub alt: u16,
    /// Super's mask, for `Meta` (usually Mod4).
    pub meta: u16,
    /// Num Lock's mask (usually Mod2).
    pub num_lock: u16,
}

/// `ShiftMask`.
const SHIFT_MASK: u16 = 1 << 0;
/// `LockMask`: Caps Lock.
const LOCK_MASK: u16 = 1 << 1;
/// `ControlMask`.
const CONTROL_MASK: u16 = 1 << 2;

impl Default for ModifierMasks {
    /// The masks of a default X server: Alt on Mod1, Num Lock on Mod2, Super on Mod4.
    fn default() -> Self {
        Self {
            alt: 1 << 3,
            num_lock: 1 << 4,
            meta: 1 << 6,
        }
    }
}

impl ModifierMasks {
    /// Reads the masks from the server's modifier map: `keycodes` is
    /// `GetModifierMapping`'s list, `per_modifier` keycodes for each of the eight
    /// modifiers in order (Shift, Lock, Control, Mod1 to Mod5). A modifier key the map
    /// doesn't hold keeps its default mask.
    pub fn from_map(keycodes: &[u8], per_modifier: usize) -> Self {
        // Alt_L and Alt_R, Super_L and Super_R, Num_Lock: their evdev codes plus 8.
        const ALT: [u8; 2] = [56 + 8, 100 + 8];
        const META: [u8; 2] = [125 + 8, 126 + 8];
        const NUM_LOCK: [u8; 1] = [69 + 8];
        let mut masks = Self::default();
        let find = |keys: &[u8]| -> Option<u16> {
            if per_modifier == 0 {
                return None;
            }
            // Mod1 to Mod5 are the fourth to eighth rows.
            (3..8).find_map(|row| {
                let held = keycodes.get(row * per_modifier..(row + 1) * per_modifier)?;
                held.iter().any(|k| keys.contains(k)).then_some(1 << row)
            })
        };
        if let Some(mask) = find(&ALT) {
            masks.alt = mask;
        }
        if let Some(mask) = find(&META) {
            masks.meta = mask;
        }
        if let Some(mask) = find(&NUM_LOCK) {
            masks.num_lock = mask;
        }
        masks
    }

    /// The modifier mask a chord grabs, without the locks.
    pub fn of(&self, chord: GrabChord) -> u16 {
        let mut mask = 0;
        for (held, flag) in [
            (chord.ctrl, CONTROL_MASK),
            (chord.alt, self.alt),
            (chord.shift, SHIFT_MASK),
            (chord.meta, self.meta),
        ] {
            if held {
                mask |= flag;
            }
        }
        mask
    }

    /// The four lock states a chord is grabbed in: none, Caps Lock, Num Lock, both.
    pub fn lock_variants(&self) -> [u16; 4] {
        [0, LOCK_MASK, self.num_lock, LOCK_MASK | self.num_lock]
    }

    /// The modifiers of an event's `state` that a chord names, without the locks and the
    /// mouse buttons.
    pub fn relevant(&self, state: u16) -> u16 {
        state & (SHIFT_MASK | CONTROL_MASK | self.alt | self.meta)
    }
}

#[cfg(target_os = "linux")]
pub use listener::GlobalHotkeys;

#[cfg(target_os = "linux")]
mod listener {
    use super::{ModifierMasks, parse_chord};
    use crate::dl::SystemLibrary;
    use jstream_platform::PlatformError;
    use std::ffi::{c_char, c_int, c_uint, c_void};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    /// An X connection (`xcb_connection_t`), opaque.
    #[repr(C)]
    struct Connection {
        _private: [u8; 0],
    }

    /// `xcb_void_cookie_t` and the other cookies: a request's sequence number.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Cookie {
        sequence: c_uint,
    }

    /// `xcb_screen_iterator_t`.
    #[repr(C)]
    struct ScreenIterator {
        data: *const u32,
        rem: c_int,
        index: c_int,
    }

    /// The start of `xcb_get_modifier_mapping_reply_t`.
    #[repr(C)]
    struct ModifierMappingReply {
        response_type: u8,
        keycodes_per_modifier: u8,
    }

    /// The start of `xcb_generic_error_t`.
    #[repr(C)]
    struct GenericError {
        response_type: u8,
        error_code: u8,
    }

    /// `xcb_key_press_event_t` and `xcb_key_release_event_t`.
    #[repr(C)]
    struct KeyEvent {
        response_type: u8,
        detail: u8,
        sequence: u16,
        time: u32,
        root: u32,
        event: u32,
        child: u32,
        root_x: i16,
        root_y: i16,
        event_x: i16,
        event_y: i16,
        state: u16,
        same_screen: u8,
        pad: u8,
    }

    /// `XCB_KEY_PRESS` and `XCB_KEY_RELEASE`.
    const KEY_PRESS: u8 = 2;
    const KEY_RELEASE: u8 = 3;
    /// `XCB_GRAB_MODE_ASYNC`.
    const GRAB_MODE_ASYNC: u8 = 1;
    /// `BadAccess`: another client has grabbed the chord.
    const BAD_ACCESS: u8 = 10;

    type ConnectFn = unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut Connection;
    type ConnectionFn = unsafe extern "C" fn(*mut Connection) -> c_int;
    type DisconnectFn = unsafe extern "C" fn(*mut Connection);
    type GetSetupFn = unsafe extern "C" fn(*mut Connection) -> *const c_void;
    type RootsFn = unsafe extern "C" fn(*const c_void) -> ScreenIterator;
    type ScreenNextFn = unsafe extern "C" fn(*mut ScreenIterator);
    type GrabKeyFn = unsafe extern "C" fn(*mut Connection, u8, u32, u16, u8, u8, u8) -> Cookie;
    type UngrabKeyFn = unsafe extern "C" fn(*mut Connection, u8, u32, u16) -> Cookie;
    type RequestCheckFn = unsafe extern "C" fn(*mut Connection, Cookie) -> *mut GenericError;
    type ModMapFn = unsafe extern "C" fn(*mut Connection) -> Cookie;
    type ModMapReplyFn = unsafe extern "C" fn(
        *mut Connection,
        Cookie,
        *mut *mut GenericError,
    ) -> *mut ModifierMappingReply;
    type ModMapKeycodesFn = unsafe extern "C" fn(*const ModifierMappingReply) -> *const u8;
    type PollEventFn = unsafe extern "C" fn(*mut Connection) -> *mut KeyEvent;

    /// The libxcb functions the listener calls.
    struct Xcb {
        connect: ConnectFn,
        has_error: ConnectionFn,
        file_descriptor: ConnectionFn,
        flush: ConnectionFn,
        disconnect: DisconnectFn,
        get_setup: GetSetupFn,
        roots: RootsFn,
        screen_next: ScreenNextFn,
        grab_key: GrabKeyFn,
        ungrab_key: UngrabKeyFn,
        request_check: RequestCheckFn,
        modifier_mapping: ModMapFn,
        modifier_mapping_reply: ModMapReplyFn,
        modifier_mapping_keycodes: ModMapKeycodesFn,
        poll_for_event: PollEventFn,
        _lib: SystemLibrary,
    }

    impl Xcb {
        fn load() -> Result<Self, PlatformError> {
            let lib = SystemLibrary::open(
                "libxcb",
                &["libxcb.so.1", "libxcb.so"],
                "install libxcb (Debian and Ubuntu: libxcb1; Fedora: libxcb)",
            )?;
            // SAFETY: each type is the function's C declaration in xcb/xcb.h and
            // xcb/xproto.h: connections and replies as pointers, cookies and iterators
            // as the `repr(C)` structs above, `xcb_window_t` as u32, `xcb_keycode_t`
            // and the modes as u8, the modifier mask as u16. `_lib` keeps the library
            // loaded while the table lives.
            unsafe {
                Ok(Self {
                    connect: lib.function("xcb_connect")?,
                    has_error: lib.function("xcb_connection_has_error")?,
                    file_descriptor: lib.function("xcb_get_file_descriptor")?,
                    flush: lib.function("xcb_flush")?,
                    disconnect: lib.function("xcb_disconnect")?,
                    get_setup: lib.function("xcb_get_setup")?,
                    roots: lib.function("xcb_setup_roots_iterator")?,
                    screen_next: lib.function("xcb_screen_next")?,
                    grab_key: lib.function("xcb_grab_key_checked")?,
                    ungrab_key: lib.function("xcb_ungrab_key_checked")?,
                    request_check: lib.function("xcb_request_check")?,
                    modifier_mapping: lib.function("xcb_get_modifier_mapping")?,
                    modifier_mapping_reply: lib.function("xcb_get_modifier_mapping_reply")?,
                    modifier_mapping_keycodes: lib.function("xcb_get_modifier_mapping_keycodes")?,
                    poll_for_event: lib.function("xcb_poll_for_event")?,
                    _lib: lib,
                })
            }
        }
    }

    /// An open X connection, its root window and its modifier masks. Dropping it
    /// disconnects, which lets every grab it holds go.
    struct Display {
        xcb: Xcb,
        connection: *mut Connection,
        root: u32,
        masks: ModifierMasks,
    }

    impl Display {
        /// Connects to `$DISPLAY`.
        fn open() -> Result<Self, PlatformError> {
            let xcb = Xcb::load()?;
            let mut screen: c_int = 0;
            // SAFETY: a null name reads `$DISPLAY`; `screen` is a valid int to write.
            // xcb_connect never returns null: a failed connection reports through
            // xcb_connection_has_error and must still be disconnected.
            let connection = unsafe { (xcb.connect)(std::ptr::null(), &raw mut screen) };
            let mut display = Self {
                xcb,
                connection,
                root: 0,
                masks: ModifierMasks::default(),
            };
            // SAFETY: `connection` came from xcb_connect.
            let error = unsafe { (display.xcb.has_error)(display.connection) };
            if error != 0 {
                return Err(PlatformError::Os {
                    call: "xcb_connect",
                    code: u32::try_from(error).ok(),
                    message: "could not connect to the X server named by $DISPLAY".into(),
                });
            }
            display.root = display.root_of(screen).ok_or_else(|| PlatformError::Os {
                call: "xcb_setup_roots_iterator",
                code: None,
                message: format!("the X server has no screen {screen}"),
            })?;
            display.masks = display.read_masks();
            Ok(display)
        }

        /// Screen `screen`'s root window.
        fn root_of(&self, screen: c_int) -> Option<u32> {
            // SAFETY: the setup belongs to the open connection and lives as long as it.
            let setup = unsafe { (self.xcb.get_setup)(self.connection) };
            if setup.is_null() {
                return None;
            }
            // SAFETY: `setup` is the connection's setup.
            let mut screens = unsafe { (self.xcb.roots)(setup) };
            for _ in 0..screen {
                if screens.rem <= 0 {
                    return None;
                }
                // SAFETY: `screens` is a live iterator over the setup's screens.
                unsafe { (self.xcb.screen_next)(&raw mut screens) };
            }
            if screens.rem <= 0 || screens.data.is_null() {
                return None;
            }
            // SAFETY: `data` points at an `xcb_screen_t`, whose first field is the root
            // window, a u32.
            Some(unsafe { screens.data.read_unaligned() })
        }

        /// The modifier masks, from the server's modifier map, or the defaults when it
        /// can't be read.
        fn read_masks(&self) -> ModifierMasks {
            // SAFETY: the connection is open.
            let cookie = unsafe { (self.xcb.modifier_mapping)(self.connection) };
            let mut error: *mut GenericError = std::ptr::null_mut();
            // SAFETY: `cookie` is this connection's; `error` is a valid out pointer.
            let reply = unsafe {
                (self.xcb.modifier_mapping_reply)(self.connection, cookie, &raw mut error)
            };
            free(error.cast());
            if reply.is_null() {
                return ModifierMasks::default();
            }
            // SAFETY: `reply` is a non-null reply xcb allocated.
            let per_modifier = usize::from(unsafe { (*reply).keycodes_per_modifier });
            // SAFETY: the reply holds `8 * keycodes_per_modifier` keycodes, which this
            // accessor points at.
            let keycodes = unsafe { (self.xcb.modifier_mapping_keycodes)(reply) };
            let masks = if keycodes.is_null() {
                ModifierMasks::default()
            } else {
                // SAFETY: as above; the slice is read before the reply is freed.
                let keycodes = unsafe { std::slice::from_raw_parts(keycodes, 8 * per_modifier) };
                ModifierMasks::from_map(keycodes, per_modifier)
            };
            free(reply.cast());
            masks
        }

        /// Grabs `keycode` with `modifiers` on the root window, waiting for the server's
        /// answer. `Err` holds the X error code.
        fn grab(&self, keycode: u8, modifiers: u16) -> Result<(), u8> {
            // SAFETY: the connection is open; the arguments are plain values.
            let cookie = unsafe {
                (self.xcb.grab_key)(
                    self.connection,
                    1,
                    self.root,
                    modifiers,
                    keycode,
                    GRAB_MODE_ASYNC,
                    GRAB_MODE_ASYNC,
                )
            };
            self.check(cookie)
        }

        /// Lets go of a grab.
        fn ungrab(&self, keycode: u8, modifiers: u16) {
            // SAFETY: the connection is open; the arguments are plain values.
            let cookie =
                unsafe { (self.xcb.ungrab_key)(self.connection, keycode, self.root, modifiers) };
            let _ = self.check(cookie);
        }

        /// Waits for a checked request's answer: `Err` with the X error code if it
        /// failed.
        fn check(&self, cookie: Cookie) -> Result<(), u8> {
            // SAFETY: `cookie` is a checked request on this connection.
            let error = unsafe { (self.xcb.request_check)(self.connection, cookie) };
            if error.is_null() {
                return Ok(());
            }
            // SAFETY: a non-null error xcb allocated.
            let code = unsafe { (*error).error_code };
            free(error.cast());
            Err(code)
        }

        /// The connection's socket, to wait on.
        fn fd(&self) -> c_int {
            // SAFETY: the connection is open.
            unsafe { (self.xcb.file_descriptor)(self.connection) }
        }

        /// Whether the connection has failed (the X server went away).
        fn failed(&self) -> bool {
            // SAFETY: the connection is open.
            unsafe { (self.xcb.has_error)(self.connection) != 0 }
        }

        fn flush(&self) {
            // SAFETY: the connection is open.
            let _ = unsafe { (self.xcb.flush)(self.connection) };
        }

        /// The next queued key press or release, as (press, keycode, state, time), and
        /// `Some(None)` for any other event; `None` when nothing is queued.
        fn next_event(&self) -> Option<Option<(bool, u8, u16, u32)>> {
            // SAFETY: the connection is open.
            let event = unsafe { (self.xcb.poll_for_event)(self.connection) };
            if event.is_null() {
                return None;
            }
            // SAFETY: every event is 32 bytes, the size of `KeyEvent`, and starts with
            // its response type; the key fields are read only for key events.
            let (kind, keycode, state, time) = unsafe {
                let e = &*event;
                (e.response_type & 0x7F, e.detail, e.state, e.time)
            };
            free(event.cast());
            Some(match kind {
                KEY_PRESS => Some((true, keycode, state, time)),
                KEY_RELEASE => Some((false, keycode, state, time)),
                _ => None,
            })
        }
    }

    impl Drop for Display {
        fn drop(&mut self) {
            // SAFETY: the connection came from xcb_connect and is disconnected once.
            unsafe { (self.xcb.disconnect)(self.connection) };
        }
    }

    // SAFETY: the connection is used only by the one thread that owns the `Display`;
    // it is moved to that thread once, before any use.
    unsafe impl Send for Display {}

    /// Frees memory xcb allocated with `malloc`.
    fn free(pointer: *mut c_void) {
        if !pointer.is_null() {
            // SAFETY: xcb's replies, events and errors are `malloc`ed and the caller's
            // to free, once.
            unsafe { libc::free(pointer) };
        }
    }

    /// An `eventfd` that wakes the listener thread.
    struct Wake(c_int);

    impl Wake {
        fn new() -> Result<Self, PlatformError> {
            // SAFETY: plain flags; the descriptor is owned by the returned value.
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(PlatformError::Os {
                    call: "eventfd",
                    code: None,
                    message: std::io::Error::last_os_error().to_string(),
                });
            }
            Ok(Self(fd))
        }

        fn signal(&self) {
            let one: u64 = 1;
            // SAFETY: writes 8 bytes from a live u64 to the owned eventfd.
            let _ = unsafe { libc::write(self.0, (&raw const one).cast(), 8) };
        }

        fn clear(&self) {
            let mut count: u64 = 0;
            // SAFETY: reads 8 bytes into a live u64 from the owned, non-blocking eventfd.
            let _ = unsafe { libc::read(self.0, (&raw mut count).cast(), 8) };
        }
    }

    impl Drop for Wake {
        fn drop(&mut self) {
            // SAFETY: the descriptor is owned and closed once.
            unsafe { libc::close(self.0) };
        }
    }

    /// A request to the listener thread: grab these chords in place of the last ones,
    /// and answer with the ones that couldn't be.
    type Request = (Vec<String>, mpsc::Sender<Vec<(String, String)>>);

    /// One grabbed chord: its keycode, its modifiers without the locks, and its name.
    struct Grab {
        keycode: u8,
        modifiers: u16,
        chord: String,
    }

    /// The listener thread. Dropping it lets every chord go and ends the thread.
    pub struct GlobalHotkeys {
        requests: mpsc::Sender<Request>,
        wake: Arc<Wake>,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl GlobalHotkeys {
        /// Connects to the X server `$DISPLAY` names and starts the listener thread, with
        /// nothing grabbed. `on_press` runs on that thread with the chord, as
        /// [`set`](Self::set) named it, each time it is pressed; it must return quickly.
        ///
        /// # Errors
        /// [`PlatformError::MissingComponent`] without libxcb, and [`PlatformError::Os`]
        /// when there is no X server to connect to or the thread can't start.
        pub fn start(
            on_press: impl Fn(&str) + Send + 'static,
        ) -> Result<GlobalHotkeys, PlatformError> {
            let display = Display::open()?;
            let wake = Arc::new(Wake::new()?);
            let stop = Arc::new(AtomicBool::new(false));
            let (requests, incoming) = mpsc::channel::<Request>();
            let thread = std::thread::Builder::new()
                .name("jstream-hotkeys".into())
                .spawn({
                    let wake = Arc::clone(&wake);
                    let stop = Arc::clone(&stop);
                    move || run(&display, &wake, &stop, &incoming, &on_press)
                })
                .map_err(|error| PlatformError::Os {
                    call: "std::thread::spawn",
                    code: None,
                    message: error.to_string(),
                })?;
            Ok(GlobalHotkeys {
                requests,
                wake,
                stop,
                thread: Some(thread),
            })
        }

        /// Grabs `chords` in place of the ones grabbed before, and returns each chord
        /// that couldn't be grabbed with the reason: one another client holds, one whose
        /// key has no keycode, or one that isn't a chord.
        pub fn set(&self, chords: Vec<String>) -> Vec<(String, String)> {
            let (reply, answer) = mpsc::channel();
            let all = chords.clone();
            let stopped = || {
                all.iter()
                    .map(|chord| (chord.clone(), "the hotkey thread has stopped".into()))
                    .collect()
            };
            if self.requests.send((chords, reply)).is_err() {
                return stopped();
            }
            self.wake.signal();
            answer.recv().unwrap_or_else(|_| stopped())
        }
    }

    impl Drop for GlobalHotkeys {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.wake.signal();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// The listener thread: grabs what it is asked to and hands each press to
    /// `on_press` until it is stopped or the X server goes away. Lets every grab go
    /// before it returns (and the connection's close would too).
    fn run(
        display: &Display,
        wake: &Wake,
        stop: &AtomicBool,
        incoming: &mpsc::Receiver<Request>,
        on_press: &dyn Fn(&str),
    ) {
        let mut grabs: Vec<Grab> = Vec::new();
        // The last release, as auto-repeat sends it just before its press.
        let mut last_release: Option<(u8, u32)> = None;
        let mut fds = [
            libc::pollfd {
                fd: display.fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake.0,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            while let Some(event) = display.next_event() {
                let Some((press, keycode, state, time)) = event else {
                    continue;
                };
                if !press {
                    last_release = Some((keycode, time));
                    continue;
                }
                if last_release.take() == Some((keycode, time)) {
                    continue;
                }
                let held = display.masks.relevant(state);
                if let Some(grab) = grabs
                    .iter()
                    .find(|g| g.keycode == keycode && g.modifiers == held)
                {
                    on_press(&grab.chord);
                }
            }
            if stop.load(Ordering::SeqCst) || display.failed() {
                break;
            }
            while let Ok((chords, reply)) = incoming.try_recv() {
                ungrab(display, &grabs);
                let (kept, failed) = grab(display, chords);
                grabs = kept;
                let _ = reply.send(failed);
            }
            display.flush();
            for fd in &mut fds {
                fd.revents = 0;
            }
            // SAFETY: `fds` is a live array of two pollfds; -1 waits until one is ready.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                break;
            }
            if fds[1].revents != 0 {
                wake.clear();
            }
        }
        ungrab(display, &grabs);
        display.flush();
    }

    /// Grabs each chord in its four lock states, returning those grabbed and those that
    /// failed with the reason. A chord is grabbed in all four or none.
    fn grab(display: &Display, chords: Vec<String>) -> (Vec<Grab>, Vec<(String, String)>) {
        let mut kept: Vec<Grab> = Vec::new();
        let mut failed = Vec::new();
        for chord in chords {
            let Some(parsed) = parse_chord(&chord) else {
                failed.push((chord, "its key can't be grabbed on X11".into()));
                continue;
            };
            let modifiers = display.masks.of(parsed);
            if kept
                .iter()
                .any(|g| g.keycode == parsed.keycode && g.modifiers == modifiers)
            {
                continue;
            }
            let mut done = Vec::new();
            let mut refused = None;
            for lock in display.masks.lock_variants() {
                match display.grab(parsed.keycode, modifiers | lock) {
                    Ok(()) => done.push(modifiers | lock),
                    Err(code) => {
                        refused = Some(code);
                        break;
                    }
                }
            }
            match refused {
                None => kept.push(Grab {
                    keycode: parsed.keycode,
                    modifiers,
                    chord,
                }),
                Some(code) => {
                    for mask in done {
                        display.ungrab(parsed.keycode, mask);
                    }
                    let reason = if code == BAD_ACCESS {
                        "another application holds it".to_owned()
                    } else {
                        format!("the X server refused the grab (error {code})")
                    };
                    failed.push((chord, reason));
                }
            }
        }
        (kept, failed)
    }

    /// Lets go of every grab in `grabs`, in all four lock states.
    fn ungrab(display: &Display, grabs: &[Grab]) {
        for grab in grabs {
            for lock in display.masks.lock_variants() {
                display.ungrab(grab.keycode, grab.modifiers | lock);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GrabChord, ModifierMasks, key_code, parse_chord};
    use jstream_protocol::Chord;

    #[test]
    fn every_chord_key_has_a_keycode() {
        for code in Chord::KEYS {
            assert!(key_code(code).is_some(), "{code} has no keycode");
        }
    }

    #[test]
    fn no_two_chord_keys_share_a_keycode() {
        let mut codes: Vec<u8> = Chord::KEYS.iter().filter_map(|c| key_code(c)).collect();
        codes.sort_unstable();
        let count = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), count);
    }

    #[test]
    fn keys_map_to_their_evdev_codes_plus_8() {
        assert_eq!(key_code("KeyA"), Some(38));
        assert_eq!(key_code("KeyQ"), Some(24));
        assert_eq!(key_code("Digit1"), Some(10));
        assert_eq!(key_code("Digit0"), Some(19));
        assert_eq!(key_code("Space"), Some(65));
        assert_eq!(key_code("Escape"), Some(9));
        assert_eq!(key_code("F1"), Some(67));
        assert_eq!(key_code("F10"), Some(76));
        assert_eq!(key_code("F11"), Some(95));
        assert_eq!(key_code("F12"), Some(96));
        assert_eq!(key_code("F13"), Some(191));
        assert_eq!(key_code("F24"), Some(202));
        assert_eq!(key_code("Numpad0"), Some(90));
        assert_eq!(key_code("Numpad7"), Some(79));
        assert_eq!(key_code("NumpadEnter"), Some(104));
        assert_eq!(key_code("ArrowUp"), Some(111));
        assert_eq!(key_code("MediaPlayPause"), Some(172));
        assert_eq!(key_code("F25"), None);
        assert_eq!(key_code("F0"), None);
        assert_eq!(key_code("KeyAB"), None);
        assert_eq!(key_code("Digit10"), None);
        assert_eq!(key_code("Numpad10"), None);
    }

    #[test]
    fn a_chord_parses_into_its_modifiers_and_key() {
        assert_eq!(
            parse_chord("Ctrl+Shift+F9"),
            Some(GrabChord {
                ctrl: true,
                alt: false,
                shift: true,
                meta: false,
                keycode: 75,
            })
        );
        assert_eq!(parse_chord("Meta+KeyA").map(|c| c.meta), Some(true));
        assert_eq!(parse_chord("Ctrl+Ctrl+KeyA"), None);
        assert_eq!(parse_chord("Hyper+KeyA"), None);
        assert_eq!(parse_chord("Ctrl"), None);
    }

    #[test]
    fn masks_come_from_the_modifier_map() {
        // Two keycodes per modifier: Shift, Lock, Control, then Mod1 to Mod5. This
        // server puts Super on Mod3 and Num Lock on Mod5, and Alt where it usually is.
        let map = [
            50, 62, // Shift
            66, 0, // Lock
            37, 105, // Control
            64, 108, // Mod1: Alt
            0, 0, // Mod2
            133, 134, // Mod3: Super
            0, 0, // Mod4
            77, 0, // Mod5: Num Lock
        ];
        let masks = ModifierMasks::from_map(&map, 2);
        assert_eq!(
            masks,
            ModifierMasks {
                alt: 1 << 3,
                meta: 1 << 5,
                num_lock: 1 << 7,
            }
        );
        // An empty map keeps the defaults.
        assert_eq!(ModifierMasks::from_map(&[], 0), ModifierMasks::default());
    }

    #[test]
    fn a_chord_matches_with_any_lock_held() {
        let masks = ModifierMasks::default();
        let chord = parse_chord("Ctrl+Alt+KeyZ").unwrap_or_else(|| panic!("a chord"));
        let mask = masks.of(chord);
        assert_eq!(mask, (1 << 2) | (1 << 3));
        for lock in masks.lock_variants() {
            // A mouse button held (Button1Mask) doesn't change it either.
            assert_eq!(masks.relevant(mask | lock | (1 << 8)), mask);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod x11_tests {
    //! Against a real X server, so `#[ignore]`d: run them with one on `$DISPLAY` (Xvfb
    //! will do) that has the XTEST extension, with
    //! `DISPLAY=:1 cargo test -p jstream-platform-linux hotkeys -- --ignored`.

    use super::GlobalHotkeys;
    use std::sync::mpsc;
    use std::time::Duration;

    /// libXtst's `XTestFakeKeyEvent` on an Xlib display, to press keys as a person
    /// would.
    struct Keyboard {
        lib_x11: libloading::Library,
        lib_xtst: libloading::Library,
        display: *mut std::ffi::c_void,
    }

    impl Keyboard {
        fn open() -> Option<Self> {
            std::env::var_os("DISPLAY")?;
            // SAFETY: well-known system libraries without initialiser preconditions.
            let lib_x11 = unsafe { libloading::Library::new("libX11.so.6") }.ok()?;
            // SAFETY: as above.
            let lib_xtst = unsafe { libloading::Library::new("libXtst.so.6") }.ok()?;
            // SAFETY: `Display *XOpenDisplay(const char *)`; null reads `$DISPLAY`.
            let display = unsafe {
                let open = lib_x11
                    .get::<unsafe extern "C" fn(*const i8) -> *mut std::ffi::c_void>(
                        b"XOpenDisplay",
                    )
                    .ok()?;
                open(std::ptr::null())
            };
            (!display.is_null()).then_some(Self {
                lib_x11,
                lib_xtst,
                display,
            })
        }

        /// Presses or releases the key with X keycode `keycode`, and flushes.
        fn key(&self, keycode: u8, down: bool) {
            // SAFETY: `int XTestFakeKeyEvent(Display *, unsigned int, Bool, unsigned
            // long)` and `int XFlush(Display *)` on the open display.
            unsafe {
                let fake = self
                    .lib_xtst
                    .get::<unsafe extern "C" fn(*mut std::ffi::c_void, u32, i32, u64) -> i32>(
                        b"XTestFakeKeyEvent",
                    )
                    .unwrap_or_else(|e| panic!("XTestFakeKeyEvent: {e}"));
                let flush = self
                    .lib_x11
                    .get::<unsafe extern "C" fn(*mut std::ffi::c_void) -> i32>(b"XFlush")
                    .unwrap_or_else(|e| panic!("XFlush: {e}"));
                fake(self.display, u32::from(keycode), i32::from(down), 0);
                flush(self.display);
            }
        }

        fn tap(&self, keys: &[&str]) {
            let codes: Vec<u8> = keys
                .iter()
                .map(|k| match *k {
                    "Ctrl" => 37,
                    "Shift" => 50,
                    "Alt" => 64,
                    "CapsLock" => 66,
                    code => super::key_code(code).unwrap_or_else(|| panic!("{code}")),
                })
                .collect();
            for code in &codes {
                self.key(*code, true);
            }
            for code in codes.iter().rev() {
                self.key(*code, false);
            }
        }
    }

    impl Drop for Keyboard {
        fn drop(&mut self) {
            // SAFETY: `int XCloseDisplay(Display *)` on the display opened once.
            unsafe {
                if let Ok(close) = self
                    .lib_x11
                    .get::<unsafe extern "C" fn(*mut std::ffi::c_void) -> i32>(b"XCloseDisplay")
                {
                    close(self.display);
                }
            }
        }
    }

    fn pressed(presses: &mpsc::Receiver<String>) -> Vec<String> {
        let mut got = Vec::new();
        while let Ok(chord) = presses.recv_timeout(Duration::from_millis(300)) {
            got.push(chord);
        }
        got
    }

    /// A grabbed chord is heard with and without Caps Lock, a chord that isn't grabbed
    /// isn't, a chord another client holds is refused, a held chord runs once, and
    /// dropping lets go. One test, as a held key's grab takes the whole keyboard and
    /// would steal another test's presses.
    #[test]
    #[ignore = "needs an X server on $DISPLAY, such as Xvfb"]
    fn it_hears_grabbed_chords_on_x11() {
        let keyboard =
            Keyboard::open().unwrap_or_else(|| panic!("needs an X server with XTEST on $DISPLAY"));
        let (sender, presses) = mpsc::channel();
        let hotkeys = GlobalHotkeys::start(move |chord| {
            let _ = sender.send(chord.to_owned());
        })
        .unwrap_or_else(|e| panic!("start: {e}"));
        let failed = hotkeys.set(vec![
            "Ctrl+Alt+KeyJ".into(),
            "F9".into(),
            "Ctrl+Bogus".into(),
        ]);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0].0, "Ctrl+Bogus");

        keyboard.tap(&["Ctrl", "Alt", "KeyJ"]);
        keyboard.tap(&["F9"]);
        keyboard.tap(&["F8"]);
        keyboard.tap(&["Ctrl", "KeyJ"]);
        assert_eq!(pressed(&presses), ["Ctrl+Alt+KeyJ", "F9"]);

        // Caps Lock on still hears it.
        keyboard.tap(&["CapsLock"]);
        keyboard.tap(&["F9"]);
        keyboard.tap(&["CapsLock"]);
        assert_eq!(pressed(&presses), ["F9"]);

        // Another listener can't take a chord this one holds.
        let (other, _unused) = mpsc::channel::<String>();
        let second = GlobalHotkeys::start(move |chord| {
            let _ = other.send(chord.to_owned());
        })
        .unwrap_or_else(|e| panic!("start: {e}"));
        let refused = second.set(vec!["F9".into(), "F7".into()]);
        assert_eq!(
            refused,
            [("F9".to_owned(), "another application holds it".to_owned())]
        );
        drop(second);

        // Set again: only F7 now.
        assert!(hotkeys.set(vec!["F7".into()]).is_empty());
        keyboard.tap(&["F9"]);
        keyboard.tap(&["F7"]);
        assert_eq!(pressed(&presses), ["F7"]);

        // Holding a chord down runs it once, however long X auto-repeats it.
        let f7 = super::key_code("F7").unwrap_or_else(|| panic!("F7"));
        keyboard.key(f7, true);
        // Longer than X's auto-repeat delay (660 ms by default), so it repeats.
        std::thread::sleep(Duration::from_millis(1200));
        keyboard.key(f7, false);
        assert_eq!(pressed(&presses), ["F7"]);

        // Dropped, it lets go: another client can grab F7.
        drop(hotkeys);
        let (third, _unused) = mpsc::channel::<String>();
        let after = GlobalHotkeys::start(move |chord| {
            let _ = third.send(chord.to_owned());
        })
        .unwrap_or_else(|e| panic!("start: {e}"));
        assert!(after.set(vec!["F7".into()]).is_empty());
    }
}
