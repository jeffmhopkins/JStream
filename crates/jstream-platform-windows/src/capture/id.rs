//! How capture targets are named as [`DeviceId`]s, and how a window's ID finds its window
//! again after the window was closed and opened again.
//!
//! - A display is `monitor:` and its GDI device name, such as `monitor:\\.\DISPLAY1`.
//!   The name is stable while the display stays connected to the same output; Windows
//!   may renumber displays when the topology changes.
//! - A window is `hwnd:` and its handle in hexadecimal, then `;` and its key, such as
//!   `hwnd:0x1a2b;w1.<exe>.<class>.<title>`. A handle is valid until the window is
//!   destroyed, and Windows may reuse it afterwards. The key ([`WindowKey`]) is what
//!   outlives the window: hashes of its program's file name, its window class and its
//!   title, each 16 hex digits, or `-` when Windows wouldn't say. The UWP host's program
//!   is `-` too, since it hosts every UWP app and so doesn't tell them apart: a field
//!   that is `-` means "find this window by its title". Hashes rather than the
//!   text itself, so the ID carries no text from outside JStream (ADR-0085). An ID
//!   without a key (`hwnd:0x1a2b`, as IDs were before keys) names only its handle.
//!
//! [`find_window`] picks the window an ID names among the windows open now.
//!
//! Platform-independent, so it is tested on every platform.

use jstream_platform::DeviceId;

const MONITOR: &str = "monitor:";
const WINDOW: &str = "hwnd:0x";
/// Between a window ID's handle and its key.
const KEY_SEPARATOR: char = ';';
/// The key format's version, its first field.
const KEY_VERSION: &str = "w1";
/// Where Windows doesn't say a key's field.
const UNKNOWN: &str = "-";
/// The program that hosts every UWP app's window, all with the same window class, so its
/// file name and class don't tell one app from another.
const SHARED_HOST: &str = "applicationframehost.exe";

/// The ID of the display with GDI device name `device`.
pub(crate) fn monitor_id(device: &str) -> DeviceId {
    DeviceId::new(format!("{MONITOR}{device}"))
}

/// The GDI device name in a display's ID.
pub(crate) fn parse_monitor(id: &DeviceId) -> Option<&str> {
    id.as_str()
        .strip_prefix(MONITOR)
        .filter(|name| !name.is_empty())
}

/// What finds a window again once its handle has gone: hashes of its program's file name
/// (lower case), its window class and its title. A field is `None` where Windows wouldn't
/// say, such as the program of an elevated window. The program is `None` for the UWP host
/// too, which doesn't tell one app from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowKey {
    exe: Option<u64>,
    class: Option<u64>,
    title: Option<u64>,
}

impl WindowKey {
    /// The key of a window whose program's file name (just the name, such as
    /// `notepad.exe`), class and title are these. An empty one counts as unknown.
    pub(crate) fn new(exe: Option<&str>, class: Option<&str>, title: Option<&str>) -> Self {
        let hash = |text: Option<&str>| text.filter(|t| !t.is_empty()).map(fnv1a);
        let exe = exe.map(str::to_lowercase).filter(|e| e != SHARED_HOST);
        Self {
            exe: hash(exe.as_deref()),
            class: hash(class),
            title: hash(title),
        }
    }

    /// Whether `self`, a window open now, is of the same program and class as `wanted`:
    /// the same program, or both unknown, and the same class, known.
    fn same_kind(&self, wanted: &Self) -> bool {
        self.exe == wanted.exe && self.class.is_some() && self.class == wanted.class
    }

    /// Whether a window of the same kind but another title may stand in for `self`: not
    /// when the program is unknown (or the UWP host, whose windows are all one kind).
    fn kind_is_enough(&self) -> bool {
        self.exe.is_some()
    }

    fn encode(&self) -> String {
        let field = |f: Option<u64>| f.map_or_else(|| UNKNOWN.to_owned(), |h| format!("{h:016x}"));
        format!(
            "{KEY_VERSION}.{}.{}.{}",
            field(self.exe),
            field(self.class),
            field(self.title)
        )
    }

    fn decode(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        if parts.next()? != KEY_VERSION {
            return None;
        }
        let mut field = || -> Option<Option<u64>> {
            match parts.next()? {
                UNKNOWN => Some(None),
                hex if hex.len() == 16 => u64::from_str_radix(hex, 16).ok().map(Some),
                _ => None,
            }
        };
        let key = Self {
            // An ID saved while the UWP host's program was hashed reads as the host's
            // windows are keyed now.
            exe: field()?.filter(|&exe| exe != fnv1a(SHARED_HOST)),
            class: field()?,
            title: field()?,
        };
        parts.next().is_none().then_some(key)
    }
}

/// FNV-1a, 64 bits: a hash that stays the same across builds and runs, which a saved ID
/// needs. It only tells windows apart; nothing relies on it resisting collisions.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The ID of the window with handle `hwnd` and key `key`.
pub(crate) fn window_id(hwnd: usize, key: &WindowKey) -> DeviceId {
    DeviceId::new(format!("{WINDOW}{hwnd:x}{KEY_SEPARATOR}{}", key.encode()))
}

/// A window's ID, read: its handle, never zero, and its key, if it has one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowRef {
    pub(crate) hwnd: usize,
    pub(crate) key: Option<WindowKey>,
}

/// The handle and key in a window's ID. `None` for an ID that isn't a window's, or whose
/// key is malformed.
pub(crate) fn parse_window(id: &DeviceId) -> Option<WindowRef> {
    let rest = id.as_str().strip_prefix(WINDOW)?;
    let (hex, key) = match rest.split_once(KEY_SEPARATOR) {
        Some((hex, key)) => (hex, Some(WindowKey::decode(key)?)),
        None => (rest, None),
    };
    let hwnd = usize::from_str_radix(hex, 16).ok().filter(|&h| h != 0)?;
    Some(WindowRef { hwnd, key })
}

/// The window `wanted` names among `open`, the windows open now, each with its handle and
/// key, in Z order (the topmost first):
///
/// 1. Its own handle, while that window is of the same program and class (the title can
///    change while it is open, as a browser's does with its tab). An ID without a key
///    takes its handle as it is.
/// 2. Else the first window with the same program, class and title.
/// 3. Else the first of the same program and class, unless the program is unknown or is
///    the UWP host, whose windows only the title tells apart.
///
/// "The same program" includes both unknown (an elevated window's, say), so such a
/// window is found again by its class and title.
///
/// `at_handle` is the key of the window `wanted`'s handle names now, or `None` when it
/// names no window. It is read apart from `open`, which lists only the windows a user
/// would pick, so the window a source captures stays captured, as before keys, when it
/// drops out of that list (moved to another virtual desktop, say).
pub(crate) fn find_window(
    wanted: WindowRef,
    open: &[(usize, WindowKey)],
    at_handle: Option<WindowKey>,
) -> Option<usize> {
    let Some(key) = wanted.key else {
        return at_handle.map(|_| wanted.hwnd);
    };
    if at_handle.is_some_and(|k| k.same_kind(&key)) {
        return Some(wanted.hwnd);
    }
    let same_title = |k: &WindowKey| k.same_kind(&key) && k.title.is_some() && k.title == key.title;
    open.iter()
        .find(|(_, k)| same_title(k))
        .or_else(|| {
            key.kind_is_enough()
                .then(|| open.iter().find(|(_, k)| k.same_kind(&key)))
                .flatten()
        })
        .map(|&(hwnd, _)| hwnd)
}

#[cfg(test)]
mod tests {
    use jstream_platform::DeviceId;

    use super::{
        WindowKey, WindowRef, find_window, fnv1a, monitor_id, parse_monitor, parse_window,
        window_id,
    };

    fn key(exe: &str, class: &str, title: &str) -> WindowKey {
        WindowKey::new(Some(exe), Some(class), Some(title))
    }

    fn wanted(hwnd: usize, key: WindowKey) -> WindowRef {
        WindowRef {
            hwnd,
            key: Some(key),
        }
    }

    #[test]
    fn ids_round_trip() {
        let m = monitor_id(r"\\.\DISPLAY2");
        assert_eq!(m.as_str(), r"monitor:\\.\DISPLAY2");
        assert_eq!(parse_monitor(&m), Some(r"\\.\DISPLAY2"));
        let k = key("Notepad.exe", "Notepad", "notes.txt - Notepad");
        let w = window_id(0x1A2B, &k);
        assert!(w.as_str().starts_with("hwnd:0x1a2b;w1."), "{w}");
        assert!(
            !w.as_str().contains("otepad"),
            "no window text in the ID: {w}"
        );
        assert_eq!(parse_window(&w), Some(wanted(0x1A2B, k)));
        let unknown = WindowKey::new(None, Some(""), Some("t"));
        let w = window_id(7, &unknown);
        assert!(w.as_str().starts_with("hwnd:0x7;w1.-.-."), "{w}");
        assert_eq!(parse_window(&w), Some(wanted(7, unknown)));
    }

    /// An ID from before keys names its handle and nothing else.
    #[test]
    fn a_keyless_id_is_still_read() {
        assert_eq!(
            parse_window(&DeviceId::new("hwnd:0x1a2b")),
            Some(WindowRef {
                hwnd: 0x1A2B,
                key: None
            })
        );
    }

    /// The program's file name is compared without case, as Windows names files.
    #[test]
    fn the_program_name_ignores_case() {
        assert_eq!(key("NOTEPAD.EXE", "c", "t"), key("notepad.exe", "c", "t"));
        assert_ne!(key("a.exe", "C", "t"), key("a.exe", "c", "t"));
    }

    /// FNV-1a's published test vectors: the hash in saved IDs mustn't change.
    #[test]
    fn the_hash_is_fnv1a() {
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a("foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn other_ids_are_refused() {
        for bad in [
            "",
            "monitor:",
            "DISPLAY1",
            "hwnd:",
            "hwnd:0x",
            "hwnd:0x0",
            "hwnd:1a2b",
            "hwnd:0x1a2b;",
            "hwnd:0x1a2b;w2.-.-.-",
            "hwnd:0x1a2b;w1.-.-",
            "hwnd:0x1a2b;w1.-.-.-.-",
            "hwnd:0x1a2b;w1.12.-.-",
            "hwnd:0x1a2b;w1.zzzzzzzzzzzzzzzz.-.-",
            "hwnd:0x;w1.-.-.-",
        ] {
            let id = DeviceId::new(bad);
            assert_eq!(parse_monitor(&id), None, "{bad:?}");
            assert_eq!(parse_window(&id), None, "{bad:?}");
        }
        assert_eq!(parse_window(&DeviceId::new("hwnd:0xzz")), None);
        let k = key("a.exe", "c", "t");
        assert_eq!(parse_monitor(&window_id(5, &k)), None);
        assert_eq!(parse_window(&monitor_id("x")), None);
    }

    /// While the window is open its handle names it, though its title has changed.
    #[test]
    fn the_open_window_keeps_its_handle() {
        let was = key("browser.exe", "Frame", "Tab one");
        let now = key("browser.exe", "Frame", "Tab two");
        let other = key("browser.exe", "Frame", "Tab one");
        let open = [(0x20, other), (0x10, now)];
        assert_eq!(find_window(wanted(0x10, was), &open, Some(now)), Some(0x10));
    }

    /// A handle Windows reused for another program's window is not the window.
    #[test]
    fn a_reused_handle_is_not_the_window() {
        let was = key("editor.exe", "Edit", "notes");
        let reused = key("game.exe", "Game", "notes");
        let again = key("editor.exe", "Edit", "notes");
        let open = [(0x10, reused), (0x30, again)];
        assert_eq!(
            find_window(wanted(0x10, was), &open, Some(reused)),
            Some(0x30)
        );
        assert_eq!(
            find_window(wanted(0x10, was), &open[..1], Some(reused)),
            None
        );
    }

    /// Opened again, the window with the same title wins over the topmost of its kind.
    #[test]
    fn a_reopened_window_is_found_by_its_title() {
        let was = key("editor.exe", "Edit", "b.txt");
        let a = key("editor.exe", "Edit", "a.txt");
        let b = key("editor.exe", "Edit", "b.txt");
        let open = [(0x40, a), (0x50, b)];
        assert_eq!(find_window(wanted(0x10, was), &open, None), Some(0x50));
    }

    /// With no window of that title, the topmost of the same program and class.
    #[test]
    fn else_the_topmost_of_its_kind() {
        let was = key("daw.exe", "Main", "Song 1");
        let open = [
            (0x40, key("other.exe", "Main", "Song 1")),
            (0x50, key("daw.exe", "Main", "Song 2")),
            (0x60, key("daw.exe", "Main", "Song 3")),
        ];
        assert_eq!(find_window(wanted(0x10, was), &open, None), Some(0x50));
    }

    /// UWP apps share one host program and class, so only the title finds one of them,
    /// and a window whose program is unknown is found only by its title too.
    #[test]
    fn shared_or_unknown_programs_need_the_title() {
        let host = "ApplicationFrameHost.exe";
        let was = key(host, "ApplicationFrameWindow", "Settings");
        let open = [(0x40, key(host, "ApplicationFrameWindow", "Calculator"))];
        assert_eq!(find_window(wanted(0x10, was), &open, None), None);
        let settings = key(host, "ApplicationFrameWindow", "Settings");
        let open = [open[0], (0x50, settings)];
        assert_eq!(find_window(wanted(0x10, was), &open, None), Some(0x50));
        // Its program is left out of the key, as an unknown one is, so a list that
        // compares keys' fields doesn't take one UWP app for another.
        assert_eq!(
            was,
            WindowKey::new(None, Some("ApplicationFrameWindow"), Some("Settings"))
        );
        assert!(
            window_id(0x10, &was)
                .as_str()
                .starts_with("hwnd:0x10;w1.-.")
        );
        // An ID saved with the host's program hashed reads the same way.
        let saved = format!(
            "hwnd:0x10;w1.{:016x}.{:016x}.{:016x}",
            fnv1a("applicationframehost.exe"),
            fnv1a("ApplicationFrameWindow"),
            fnv1a("Settings")
        );
        assert_eq!(parse_window(&DeviceId::new(saved)), Some(wanted(0x10, was)));

        let was = WindowKey::new(None, Some("Admin"), Some("Tool"));
        let other = WindowKey::new(None, Some("Admin"), Some("Else"));
        assert_eq!(find_window(wanted(0x10, was), &[(0x40, other)], None), None);
        let open = [(0x40, other), (0x50, was)];
        assert_eq!(find_window(wanted(0x10, was), &open, None), Some(0x50));
        // A known program is not an unknown one.
        let known = key("tool.exe", "Admin", "Tool");
        assert_eq!(find_window(wanted(0x10, was), &[(0x40, known)], None), None);
    }

    /// An ID without a key takes its handle while it is a window, and nothing else.
    #[test]
    fn a_keyless_id_takes_only_its_handle() {
        let keyless = WindowRef {
            hwnd: 0x10,
            key: None,
        };
        let k = key("a.exe", "c", "t");
        assert_eq!(find_window(keyless, &[(0x20, k)], Some(k)), Some(0x10));
        assert_eq!(find_window(keyless, &[(0x20, k)], None), None);
    }
}
