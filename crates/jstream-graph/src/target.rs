//! Validation of an output's target: the RTMP(S) server URL an output publishes to, the
//! YouTube broadcast it is bound to, and the file a recording output writes.
//!
//! The collection is saved in plain text, so a URL must never carry a credential
//! (R-SEC-001, docs/RULES.md#R-SEC-001). The stream key goes to the OS credential store
//! through `output.set_key` instead. So a URL with `user:password@`, a `?query` or a
//! `#fragment` is rejected, as is anything `jstream-outputs`' `RtmpUrl::parse` would
//! refuse. A key pasted into the path (`rtmp://host/app/<key>`) looks like a nested
//! application name and can't be told apart; the path is stored as given.
//!
//! The rejection reasons are fixed text. They never repeat the URL, which might hold a
//! key.
//!
//! A recording path is checked for its shape only, the same on every platform:
//! absolute in either Unix or Windows form, not a UNC or device path, with no `..`
//! segment, and ending in a file name that is not a DOS device name. Whether it is
//! absolute on the engine's own machine, whether its directory exists, and whether a
//! file is already there, is checked by `output.start`, when the recording is opened.

/// The longest URL accepted, in bytes.
pub(crate) const MAX_URL_LEN: usize = 2048;
/// The longest recording path accepted, in bytes.
pub(crate) const MAX_PATH_LEN: usize = 4096;

/// Why `path` is not an acceptable recording path, or `None` when it is.
///
/// The check is the same on every platform: `/…`, `\…`, `C:/…` or `C:\…`, with `/` or
/// `\` separators, no `..` segment, and a file name last. Two leading separators are
/// refused, since on Windows they name a UNC share (`\\host\share\…`) or a device
/// (`\\.\…`, `\\?\…`), and so are any `:` but a drive letter's and any `?` or `*`,
/// which name an NT object path (`\??\UNC\host\…`). The file name must not be a DOS
/// device name (`NUL`, `CON`, `nul.mp4`, `COM1.mkv`, …; see `is_dos_device_name`),
/// which Win32 opens in place of a file in any folder. The engine checks again, when
/// it opens the file, that the path is absolute on its own machine.
pub(crate) fn record_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        return Some("must not be empty");
    }
    if path.len() > MAX_PATH_LEN {
        return Some("must be at most 4096 bytes");
    }
    if path.chars().any(char::is_control) {
        return Some("must not contain control characters");
    }
    let bytes = path.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\');
    if !(path.starts_with(['/', '\\']) || drive) {
        return Some("must be absolute: `/…`, `\\…`, `C:\\…` or `C:/…`");
    }
    // The rules of `jstream_protocol::check_asset_path` for an `absolute` path (#345).
    let rest = if drive { &path[2..] } else { path };
    if rest[1..].starts_with(['/', '\\']) {
        // On Windows `\\host\share\…` opens an SMB share and sends the user's NTLM
        // hash to that host; `\\.\…` and `\\?\…` are device paths.
        return Some("must not start with two separators");
    }
    if rest.contains(':') {
        // On Windows `C:\a.mp4:s` names an NTFS alternate data stream of `a.mp4`.
        return Some("must not contain `:` but a drive letter's");
    }
    if rest.contains(['?', '*']) {
        // Win32 never allows `?` or `*` in a file name; `\??\…` is an NT object path
        // (`\??\UNC\host\…`, `\??\GLOBALROOT\Device\…`).
        return Some("must not contain `?` or `*`");
    }
    let mut segments = path.split(['/', '\\']);
    if segments.clone().any(|segment| segment == "..") {
        return Some("must not contain a `..` segment");
    }
    match segments.next_back() {
        Some("" | ".") | None => Some("must end in a file name"),
        // On Windows `C:\x\NUL.mp4` writes to the null device and `COM1.mp4` to a
        // serial port, in any folder (#503).
        Some(name) if is_dos_device_name(name) => Some("must not end in a DOS device name"),
        Some(_) => None,
    }
}

/// Whether Win32 opens the path component `name` as a DOS device rather than a file:
/// the rule of `jstream_protocol::check_asset_path` (#384), mirrored here because the
/// recording reasons are this crate's own.
///
/// The stem (the name before its first `.`, without trailing spaces, compared without
/// regard to ASCII case) is `CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `COM¹`–`COM³`,
/// `LPT0`–`LPT9`, `LPT¹`–`LPT³`, `CONIN$` or `CONOUT$`. So `nul`, `NUL `, `nul.`,
/// `nul.mp4` and `CON .mkv` are devices, and `CONSOLE.mp4`, `nul1.mp4` and `COM10.mp4`
/// are not.
fn is_dos_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => ["COM", "LPT"].iter().any(|port| {
            upper.strip_prefix(port).is_some_and(|n| {
                matches!(n.as_bytes(), [b'0'..=b'9']) || matches!(n, "\u{b9}" | "\u{b2}" | "\u{b3}")
            })
        }),
    }
}

/// The longest YouTube broadcast ID accepted, in bytes. YouTube's are 11.
pub(crate) const MAX_BROADCAST_ID_LEN: usize = 128;

/// Why `id` is not an acceptable YouTube broadcast ID, or `None` when it is: YouTube's
/// IDs are ASCII letters, digits, `-` and `_`. The check keeps anything else out of the
/// Live API query the engine builds from it.
pub(crate) fn broadcast_id_problem(id: &str) -> Option<&'static str> {
    if id.is_empty() {
        return Some("must not be empty");
    }
    if id.len() > MAX_BROADCAST_ID_LEN {
        return Some("must be at most 128 bytes");
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some("must be ASCII letters, digits, `-` and `_`");
    }
    None
}

/// Why `url` is not an acceptable RTMP(S) server URL, or `None` when it is.
pub(crate) fn rtmp_url_problem(url: &str) -> Option<&'static str> {
    if url.is_empty() {
        return Some("must not be empty");
    }
    if url.len() > MAX_URL_LEN {
        return Some("must be at most 2048 bytes");
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Some("must not contain whitespace or control characters");
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Some("must start with `rtmp://` or `rtmps://`");
    };
    if !(scheme.eq_ignore_ascii_case("rtmp") || scheme.eq_ignore_ascii_case("rtmps")) {
        return Some("the scheme must be `rtmp` or `rtmps`");
    }
    if rest.contains(['?', '#']) {
        return Some("must not have a query or fragment: set the stream key with `output.set_key`");
    }
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if authority.contains('@') {
        return Some(
            "must not carry credentials (`user:password@`): set the stream key with \
             `output.set_key`",
        );
    }
    if let Some(problem) = host_port_problem(authority) {
        return Some(problem);
    }
    if path.trim_matches('/').is_empty() {
        return Some("is missing the application name (the path after the host)");
    }
    None
}

/// Checks `host[:port]` or `[v6]:port`, as `RtmpUrl::parse` splits it.
fn host_port_problem(authority: &str) -> Option<&'static str> {
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let Some((host, after)) = v6.split_once(']') else {
            return Some("has an unclosed `[` in its IPv6 host");
        };
        match after {
            "" => (host, None),
            _ => match after.strip_prefix(':') {
                Some(port) => (host, Some(port)),
                None => return Some("has unexpected text after its IPv6 host"),
            },
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Some("is missing the host");
    }
    match port.map(str::parse::<u16>) {
        None | Some(Ok(1..)) => None,
        Some(_) => Some("has a port that is not a number in 1..=65535"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATH_LEN, MAX_URL_LEN, record_path_problem, rtmp_url_problem};

    #[test]
    fn recording_paths_are_accepted() {
        for path in [
            "/show.mp4",
            "/home/me/Videos/show.mp4",
            "/home/me/Videos/show",
            "/home/me/My Videos/show 2.mp4",
            "C:\\Users\\me\\Videos\\show.mp4",
            "C:/Users/me/Videos/show.mp4",
            "c:\\show.mp4",
            "\\Videos\\show.mp4",
            "/home/me/.hidden.mp4",
            "/home/me/show.2024.mp4",
        ] {
            assert_eq!(record_path_problem(path), None, "{path}");
        }
    }

    #[test]
    fn bad_recording_paths_are_rejected() {
        let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
        for path in [
            "",
            long.as_str(),
            "show.mp4",
            "Videos/show.mp4",
            "C:show.mp4",
            "/Videos/show\n.mp4",
            "/Videos/show\0.mp4",
            "/",
            "/Videos/",
            "C:\\Videos\\",
            "/Videos/.",
            "/Videos/..",
            "/home/me/../other/show.mp4",
        ] {
            assert!(record_path_problem(path).is_some(), "{path:?} was accepted");
        }
    }

    /// #374: on Windows these open an SMB share (sending the user's NTLM hash to the
    /// host), a device, an NT object path, or an NTFS alternate data stream.
    #[test]
    fn unc_device_and_stream_paths_are_rejected() {
        let two = Some("must not start with two separators");
        let colon = Some("must not contain `:` but a drive letter's");
        let ask = Some("must not contain `?` or `*`");
        for (path, want) in [
            ("\\\\host\\share\\x.mp4", two),
            ("//host/share/x.mp4", two),
            ("\\/host/share/x.mp4", two),
            ("/\\host\\share\\x.mp4", two),
            ("\\\\.\\C:\\x.mp4", two),
            ("\\\\?\\C:\\x.mp4", two),
            ("//./pipe/x.mp4", two),
            ("C:\\\\host\\share\\x.mp4", two),
            ("C://host/share/x.mp4", two),
            ("C:\\x.mp4:stream", colon),
            ("/home/me/x.mp4:stream", colon),
            ("\\C:\\x.mp4", colon),
            ("C:\\a:b\\x.mp4", colon),
            ("\\??\\UNC\\host\\share\\x.mp4", ask),
            ("\\??\\GLOBALROOT\\Device\\NamedPipe\\x", ask),
            ("/home/me/sh?w.mp4", ask),
            ("C:\\Videos\\*.mp4", ask),
        ] {
            assert_eq!(record_path_problem(path), want, "{path:?}");
        }
    }

    /// #503: on Windows a file name that is a DOS device name opens that device, in any
    /// folder, whatever its extension.
    #[test]
    fn dos_device_file_names_are_rejected() {
        for path in [
            "C:\\Videos\\NUL",
            "C:\\Videos\\nul.mp4",
            "/home/me/Con.mkv",
            "\\CON",
            "C:/Videos/prn.mp4",
            "C:\\Videos\\aux.tar.gz",
            "C:\\Videos\\nul.",
            "C:\\Videos\\NUL ",
            "C:\\Videos\\CON .mp4",
            "C:\\Videos\\com1.mp4",
            "C:\\Videos\\COM9",
            "C:\\Videos\\com0.mp4",
            "C:\\Videos\\LPT1.mkv",
            "/home/me/lpt9",
            "C:\\Videos\\COM\u{b9}.mp4",
            "C:\\Videos\\LPT\u{b3}",
            "C:\\Videos\\CONIN$",
            "C:\\Videos\\conout$.mp4",
        ] {
            assert_eq!(
                record_path_problem(path),
                Some("must not end in a DOS device name"),
                "{path:?}"
            );
        }
        // Near misses, and device names before the last component.
        for path in [
            "C:\\Videos\\CONSOLE.mp4",
            "C:\\Videos\\nul1.mp4",
            "C:\\Videos\\COM10.mp4",
            "C:\\Videos\\LPT.mp4",
            "C:\\Videos\\COM\u{b9}0.mp4",
            "C:\\Videos\\show.con",
            "C:\\CON\\show.mp4",
            "/mnt/aux/show.mp4",
            "/home/me/null.mp4",
        ] {
            assert_eq!(record_path_problem(path), None, "{path:?}");
        }
    }

    #[test]
    fn server_urls_are_accepted() {
        for url in [
            "rtmp://a.rtmp.youtube.com/live2",
            "rtmps://a.rtmps.youtube.com:443/live2",
            "RTMP://live.twitch.tv/app/",
            "rtmps://live-api-s.facebook.com:443/rtmp/",
            "rtmp://127.0.0.1:1935/live",
            "rtmp://[::1]:1935/live",
            "rtmp://[::1]/live",
            "rtmp://host/app/instance",
        ] {
            assert_eq!(rtmp_url_problem(url), None, "{url}");
        }
    }

    #[test]
    fn bad_urls_are_rejected_without_being_repeated() {
        let long = format!("rtmp://host/{}", "a".repeat(MAX_URL_LEN));
        for url in [
            "",
            long.as_str(),
            "rtmp://host/live 2",
            "rtmp://host/live\n",
            "a.rtmp.youtube.com/live2",
            "http://host/live",
            "srt://host/live",
            "rtmp://user:secret-key@host/live",
            "rtmp://secret-key@host/live",
            "rtmp://host/live?key=secret-key",
            "rtmp://host/live?secret-key",
            "rtmp://host/live#secret-key",
            "rtmp:///live",
            "rtmp://:1935/live",
            "rtmp://host:0/live",
            "rtmp://host:99999/live",
            "rtmp://host:port/live",
            "rtmp://[::1/live",
            "rtmp://[::1]x/live",
            "rtmp://host",
            "rtmp://host/",
            "rtmp://host//",
        ] {
            let problem = rtmp_url_problem(url);
            assert!(problem.is_some(), "{url:?} was accepted");
            let problem = problem.unwrap_or_default();
            assert!(!problem.contains("secret"), "{url:?}: {problem}");
        }
    }
}
