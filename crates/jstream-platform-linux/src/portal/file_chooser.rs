//! The desktop app's Browse dialogs through the xdg-desktop-portal
//! [FileChooser](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.FileChooser.html)
//! interface: the desktop's own open, save and folder dialog, in a process of its own, on
//! the crate's D-Bus client. It works the same on X11 and Wayland, in GNOME, KDE Plasma
//! and every desktop with a portal backend, and links no toolkit.
//!
//! The request and its answer are built and read here without a bus, so they are tested
//! everywhere; [`pick_path`] runs them on the session bus, on Linux.

use std::path::PathBuf;

use jstream_platform::PlatformError;

use super::handshake::{DESTINATION, OBJECT};
use crate::dbus::{Message, Value, lookup};

/// The FileChooser interface.
pub(crate) const FILE_CHOOSER: &str = "org.freedesktop.portal.FileChooser";

/// What a Browse button asked the dialog to pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChoiceKind {
    /// An existing file.
    Open,
    /// A file to write, which need not exist yet.
    Save,
    /// An existing folder.
    Folder,
}

/// A dialog to show: the portal's `OpenFile` or `SaveFile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChoice {
    /// Open, save or folder.
    pub kind: FileChoiceKind,
    /// The dialog's title. Empty gives the kind's own ("Open", "Save", "Choose a folder").
    pub title: String,
    /// Accepted extensions, without a leading dot. Only ASCII letters and digits, at
    /// most 16 bytes each, are kept ([`filter_extensions`]); none offers every file.
    pub extensions: Vec<String>,
    /// An absolute folder to open in, or empty.
    pub folder: String,
    /// A file name to offer, for a save.
    pub name: String,
}

/// Extensions a dialog filter may name: ASCII letters and digits, at most 16, no dot,
/// lowercased, each once. Anything else is dropped, so no pattern reaches the portal
/// but `*.<ext>`.
pub fn filter_extensions(accept: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for ext in accept {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        let allowed =
            !ext.is_empty() && ext.len() <= 16 && ext.bytes().all(|b| b.is_ascii_alphanumeric());
        if allowed && !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}

/// The call that shows `choice`, its request answering on `handle_token`, for the
/// window `parent` (the portal's window identifier, such as `x11:1e00004`; empty when
/// unknown, which shows the dialog unowned).
pub(crate) fn request(choice: &FileChoice, handle_token: &str, parent: &str) -> Message {
    let (member, fallback) = match choice.kind {
        FileChoiceKind::Open => ("OpenFile", "Open"),
        FileChoiceKind::Save => ("SaveFile", "Save"),
        FileChoiceKind::Folder => ("OpenFile", "Choose a folder"),
    };
    let title = if choice.title.trim().is_empty() {
        fallback.to_owned()
    } else {
        choice.title.clone()
    };
    let mut options = vec![
        ("handle_token", Value::Str(handle_token.into())),
        ("modal", Value::Bool(true)),
    ];
    if choice.kind == FileChoiceKind::Folder {
        options.push(("directory", Value::Bool(true)));
    } else {
        let extensions = filter_extensions(&choice.extensions);
        if !extensions.is_empty() {
            // a(sa(us)): one filter, its globs (type 0) one per extension, both cases,
            // as a case-sensitive glob would miss `IMAGE.PNG`.
            let globs = extensions
                .iter()
                .flat_map(|ext| [ext.clone(), ext.to_ascii_uppercase()])
                .map(|glob| Value::Struct(vec![Value::U32(0), Value::Str(format!("*.{glob}"))]))
                .collect();
            let label = extensions
                .iter()
                .map(|ext| format!("*.{ext}"))
                .collect::<Vec<_>>()
                .join(", ");
            let filter = Value::Struct(vec![Value::Str(label), Value::Array("(us)".into(), globs)]);
            options.push(("filters", Value::Array("(sa(us))".into(), vec![filter])));
        }
    }
    if choice.folder.starts_with('/') && !choice.folder.contains('\0') {
        options.push(("current_folder", nul_terminated(&choice.folder)));
    }
    if choice.kind == FileChoiceKind::Save && !choice.name.is_empty() {
        options.push(("current_name", Value::Str(choice.name.clone())));
    }
    Message::call(
        DESTINATION,
        OBJECT,
        FILE_CHOOSER,
        member,
        vec![
            Value::Str(parent.into()),
            Value::Str(title),
            Value::dict(options),
        ],
    )
}

/// A path as the portal's `ay` takes one: its bytes and a NUL.
fn nul_terminated(path: &str) -> Value {
    let bytes = path.bytes().chain([0]).map(Value::Byte).collect();
    Value::Array("y".into(), bytes)
}

/// Reads the `Response` signal's body, `(u, a{sv})`: the path picked, or `None` when the
/// person cancelled (code 1).
///
/// # Errors
/// [`PlatformError::Os`] when the portal ended the request (code 2), or the answer is
/// malformed or names no local file.
pub(crate) fn chosen(body: &[Value]) -> Result<Option<PathBuf>, PlatformError> {
    let bad = |message: &str| PlatformError::Os {
        call: "org.freedesktop.portal.FileChooser",
        code: None,
        message: message.into(),
    };
    let [code, results] = body else {
        return Err(bad("a malformed Response"));
    };
    match code.as_u32() {
        Some(0) => {}
        Some(1) => return Ok(None),
        Some(other) => {
            return Err(PlatformError::Os {
                call: "org.freedesktop.portal.FileChooser",
                code: Some(other),
                message: "the desktop portal ended the request".into(),
            });
        }
        None => return Err(bad("no response code")),
    }
    let results = results.as_dict().ok_or_else(|| bad("no results"))?;
    let Some(Value::Array(_, uris)) = lookup(&results, "uris") else {
        return Err(bad("no uris"));
    };
    let uri = uris
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| bad("no file was picked"))?;
    file_uri_path(uri)
        .map(Some)
        .ok_or_else(|| bad("the pick is not a local file"))
}

/// The path of a `file://` URI with an empty or `localhost` host, percent-decoded.
/// `None` for any other URI, a relative path or one with a NUL.
pub(crate) fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len());
    let mut bytes = rest.bytes();
    while let Some(b) = bytes.next() {
        let b = if b == b'%' {
            let hi = char::from(bytes.next()?).to_digit(16)?;
            let lo = char::from(bytes.next()?).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()?
        } else {
            b
        };
        if b == 0 {
            return None;
        }
        out.push(b);
    }
    #[cfg(unix)]
    let path = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(out))
    };
    // Only the tests build this off Unix.
    #[cfg(not(unix))]
    let path = PathBuf::from(String::from_utf8(out).ok()?);
    Some(path)
}

/// Shows `choice` through the portal for the window `parent` (see [`request`]) and
/// returns the path the person picked, or `None` if they cancelled. Blocks until they
/// answer, for up to an hour, so it must not run on a UI thread.
///
/// # Errors
/// [`PlatformError::MissingComponent`] without a session bus, and [`PlatformError::Os`]
/// when the portal isn't running, refuses the request or doesn't answer.
#[cfg(target_os = "linux")]
pub fn pick_path(choice: &FileChoice, parent: &str) -> Result<Option<PathBuf>, PlatformError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use super::handshake::{QUICK, REQUEST, portal_owner_query};
    use super::{handle_token, request_path};
    use crate::dbus::bus::{BUS, Bus};
    use crate::dbus::{add_match, match_rule};

    /// How long the person may keep a dialog open.
    const DIALOG: Duration = Duration::from_secs(60 * 60);
    static NEXT: AtomicU64 = AtomicU64::new(1);

    let no_answer = |call: &'static str, message: &str| PlatformError::Os {
        call,
        code: None,
        message: message.into(),
    };
    let mut bus = Bus::session()?;
    // Only the portal's own connection may answer, so another process on the session
    // bus can't hand the app a path.
    let owner = bus.call(
        portal_owner_query(),
        Some(BUS),
        "org.freedesktop.DBus.GetNameOwner",
        QUICK,
    )?;
    let portal = owner
        .msg
        .body
        .first()
        .and_then(Value::as_str)
        .filter(|name| name.starts_with(':'))
        .map(str::to_owned)
        .ok_or_else(|| {
            no_answer(
                "org.freedesktop.DBus.GetNameOwner",
                "the desktop portal isn't running",
            )
        })?;
    bus.call(
        add_match(&match_rule(DESTINATION, REQUEST, "Response")),
        Some(BUS),
        "org.freedesktop.DBus.AddMatch",
        QUICK,
    )?;
    let token = format!(
        "{}_file",
        handle_token(NEXT.fetch_add(1, Ordering::Relaxed))
    );
    let mut expected: Vec<String> = request_path(bus.unique_name(), &token)
        .into_iter()
        .collect();
    let what = "org.freedesktop.portal.FileChooser";
    let reply = bus.call(request(choice, &token, parent), Some(&portal), what, QUICK)?;
    // An older portal answers on a handle of its own choosing.
    if let Some(handle) = reply.msg.body.first().and_then(Value::as_str) {
        expected.push(handle.to_owned());
    }
    let deadline = Instant::now() + DIALOG;
    let response = bus
        .wait_signal(deadline, |m| {
            m.is_signal(REQUEST, "Response")
                && m.sender.as_deref() == Some(portal.as_str())
                && m.path.as_ref().is_some_and(|p| expected.contains(p))
        })?
        .ok_or_else(|| no_answer(what, "no answer from the file dialog in time"))?;
    chosen(&response.body)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{FileChoice, FileChoiceKind, chosen, file_uri_path, filter_extensions, request};
    use crate::dbus::{Message, Value, lookup};

    fn choice(kind: FileChoiceKind) -> FileChoice {
        FileChoice {
            kind,
            title: String::new(),
            extensions: vec!["PNG".into(), ".jpg".into(), "a/b".into(), "png".into()],
            folder: "/home/me/Pictures".into(),
            name: "scene.json".into(),
        }
    }

    fn options(msg: &Message) -> Vec<(String, Value)> {
        let dict = msg.body[2].as_dict().expect("options");
        dict.into_iter()
            .map(|(k, v)| (k.to_owned(), v.clone()))
            .collect()
    }

    fn option(msg: &Message, key: &str) -> Option<Value> {
        options(msg)
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    #[test]
    fn extensions_are_plain_and_unique() {
        assert_eq!(
            filter_extensions(&["PNG".into(), ".jpg".into(), "*".into(), "png".into()]),
            ["png", "jpg"]
        );
        assert!(filter_extensions(&["x".repeat(17)]).is_empty());
    }

    #[test]
    fn an_open_asks_for_the_accepted_files_where_the_field_points() {
        let msg = request(
            &choice(FileChoiceKind::Open),
            "jstream_1_file",
            "x11:1e00004",
        );
        assert_eq!(msg.member.as_deref(), Some("OpenFile"));
        assert_eq!(msg.interface.as_deref(), Some(super::FILE_CHOOSER));
        assert_eq!(msg.signature(), "ssa{sv}");
        assert_eq!(msg.body[0], Value::Str("x11:1e00004".into()));
        assert_eq!(msg.body[1], Value::Str("Open".into()));
        assert_eq!(
            option(&msg, "handle_token"),
            Some(Value::Str("jstream_1_file".into()))
        );
        assert_eq!(option(&msg, "directory"), None);
        let Some(Value::Array(sig, filters)) = option(&msg, "filters") else {
            panic!("a filter");
        };
        assert_eq!(sig, "(sa(us))");
        let Value::Struct(filter) = &filters[0] else {
            panic!("a struct");
        };
        assert_eq!(filter[0], Value::Str("*.png, *.jpg".into()));
        let Value::Array(_, globs) = &filter[1] else {
            panic!("globs");
        };
        let globs: Vec<_> = globs
            .iter()
            .map(|g| match g {
                Value::Struct(f) => (f[0].as_u32(), f[1].as_str().map(str::to_owned)),
                _ => panic!("a glob"),
            })
            .collect();
        assert_eq!(
            globs,
            [
                (Some(0), Some("*.png".into())),
                (Some(0), Some("*.PNG".into())),
                (Some(0), Some("*.jpg".into())),
                (Some(0), Some("*.JPG".into())),
            ]
        );
        let Some(Value::Array(sig, bytes)) = option(&msg, "current_folder") else {
            panic!("a folder");
        };
        assert_eq!(sig, "y");
        assert_eq!(bytes.last(), Some(&Value::Byte(0)));
        assert_eq!(bytes.len(), "/home/me/Pictures".len() + 1);
        // Only a save offers a name.
        assert_eq!(option(&msg, "current_name"), None);
        msg.encode(1).expect("encodes");
    }

    #[test]
    fn a_save_offers_the_name_and_a_folder_pick_asks_for_a_directory() {
        let save = request(&choice(FileChoiceKind::Save), "t", "");
        assert_eq!(save.member.as_deref(), Some("SaveFile"));
        assert_eq!(
            option(&save, "current_name"),
            Some(Value::Str("scene.json".into()))
        );
        let folder = request(&choice(FileChoiceKind::Folder), "t", "");
        assert_eq!(folder.member.as_deref(), Some("OpenFile"));
        assert_eq!(option(&folder, "directory"), Some(Value::Bool(true)));
        assert_eq!(option(&folder, "filters"), None);
        assert_eq!(folder.body[1], Value::Str("Choose a folder".into()));
        folder.encode(1).expect("encodes");
    }

    #[test]
    fn a_relative_folder_or_no_extensions_add_nothing() {
        let mut c = choice(FileChoiceKind::Open);
        c.folder = "relative/path".into();
        c.extensions.clear();
        c.title = "Pick a picture".into();
        let msg = request(&c, "t", "");
        assert_eq!(option(&msg, "current_folder"), None);
        assert_eq!(option(&msg, "filters"), None);
        assert_eq!(msg.body[1], Value::Str("Pick a picture".into()));
    }

    fn response(code: u32, uris: &[&str]) -> Vec<Value> {
        let uris = uris.iter().map(|u| Value::Str((*u).into())).collect();
        vec![
            Value::U32(code),
            Value::dict(vec![("uris", Value::Array("s".into(), uris))]),
        ]
    }

    #[test]
    fn the_answer_is_the_first_local_file_or_none_on_cancel() {
        assert_eq!(
            chosen(&response(
                0,
                &["file:///home/me/My%20Clip.mp4", "file:///b"]
            ))
            .expect("ok"),
            Some(PathBuf::from("/home/me/My Clip.mp4"))
        );
        assert_eq!(chosen(&response(1, &[])).expect("cancelled"), None);
        assert!(chosen(&response(2, &[])).is_err());
        assert!(chosen(&response(0, &[])).is_err());
        assert!(chosen(&response(0, &["https://example.com/a"])).is_err());
        assert!(chosen(&[Value::U32(0)]).is_err());
        let no_uris = vec![Value::U32(0), Value::dict(vec![])];
        assert!(chosen(&no_uris).is_err());
        assert!(lookup(&[], "uris").is_none());
    }

    #[test]
    fn only_local_absolute_file_uris_are_paths() {
        assert_eq!(
            file_uri_path("file://localhost/tmp/a%2Fb"),
            Some(PathBuf::from("/tmp/a/b"))
        );
        assert_eq!(file_uri_path("file://host/tmp/a"), None);
        assert_eq!(file_uri_path("file:///tmp/%00"), None);
        assert_eq!(file_uri_path("file:///tmp/%zz"), None);
        assert_eq!(file_uri_path("/tmp/a"), None);
    }
}
